//! Validated workspace mutations.
//!
//! One mutation renames, creates, deletes, copies, or moves workspace entries.
//! [`MutationPlan::stage`] validates the complete operation and computes every
//! affected buffer path before anything on disk changes.
//! [`MutationPlan::apply`] then performs the filesystem work through a staged
//! replacement, so a failure of one path leaves no partial result.
//!
//! Both functions block. Run them on the bounded worker service only. See
//! `docs/files.md` and `docs/responsiveness.md`.
//!
//! Every path must be absolute and must hold no parent-directory component, so
//! containment stays decidable without a further filesystem read.

use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use super::buffer::BufferId;
use super::file::temporary_name;
use super::tree::EntryKind;

/// The largest number of paths that one mutation names.
pub const MUTATION_PATHS_MAX: usize = 128;

/// The largest number of entries that one recursive copy visits.
pub const COPY_ENTRIES_MAX: usize = 4096;

/// The largest directory depth that one recursive copy visits.
pub const COPY_DEPTH_MAX: usize = 32;

/// Whether a transfer keeps the source or removes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferMode {
    /// The source stays in place.
    Copy,
    /// The source moves to the destination.
    Move,
}

/// One requested workspace mutation.
#[derive(Clone, Debug)]
pub enum FileOperation {
    /// Create one empty file or one empty directory.
    Create {
        /// The path of the new entry.
        path: PathBuf,
        /// The kind of the new entry.
        kind: EntryKind,
    },
    /// Remove the named entries.
    Delete {
        /// The entries to remove.
        paths: Vec<PathBuf>,
    },
    /// Give one entry another path inside the workspace.
    Rename {
        /// The entry that keeps its content.
        from: PathBuf,
        /// The complete new path.
        to: PathBuf,
    },
    /// Copy or move the named entries into one directory.
    Transfer {
        /// Whether the sources stay in place.
        mode: TransferMode,
        /// The entries to copy or move.
        sources: Vec<PathBuf>,
        /// The directory that receives the entries.
        destination: PathBuf,
    },
}

/// One loaded buffer that a mutation can affect.
#[derive(Clone, Debug)]
pub struct OpenBuffer {
    /// The stable identity of the buffer.
    pub id: BufferId,
    /// The current path of the buffer.
    pub path: PathBuf,
    /// Whether the buffer holds unsaved changes.
    pub is_modified: bool,
}

/// The new path of one loaded buffer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BufferPathUpdate {
    /// The buffer that keeps its identity.
    pub buffer: BufferId,
    /// The path that the mutation gave it.
    pub path: PathBuf,
}

/// The complete result of one applied mutation.
#[derive(Clone, Debug)]
pub struct MutationOutcome {
    /// The buffers that the event loop retargets as one transition.
    pub updates: Vec<BufferPathUpdate>,
    /// The directories that need a new read.
    pub changed: Vec<PathBuf>,
    /// The entry that the tree selects after the refresh.
    pub selection: Option<PathBuf>,
}

/// A rejected workspace mutation.
#[derive(Debug, Error)]
pub enum MutationError {
    /// The operation names no path.
    #[error("the operation names no entry")]
    Empty,
    /// The operation names more paths than the bound allows.
    #[error("the operation names {count} entries; the limit is {max} entries")]
    TooManyPaths {
        /// The number of named paths.
        count: usize,
        /// The bound.
        max: usize,
    },
    /// The path lies outside the workspace root.
    #[error("{path} is outside the workspace")]
    Outside {
        /// The rejected path.
        path: PathBuf,
    },
    /// The source holds no entry.
    #[error("{path} holds no entry")]
    Missing {
        /// The rejected path.
        path: PathBuf,
    },
    /// The destination holds an entry already.
    #[error("{path} exists already")]
    Collision {
        /// The rejected destination.
        path: PathBuf,
    },
    /// The destination lies inside the moved or copied directory.
    #[error("{path} cannot receive one of its own parents")]
    IntoDescendant {
        /// The rejected destination.
        path: PathBuf,
    },
    /// The destination names no directory.
    #[error("{path} is not a directory")]
    NotADirectory {
        /// The rejected destination.
        path: PathBuf,
    },
    /// A buffer of the removed entry holds unsaved changes.
    #[error("{path} holds unsaved changes")]
    DirtyBuffer {
        /// The path of the modified buffer.
        path: PathBuf,
    },
    /// The copy visits more entries than the bound allows.
    #[error("the copy holds more than {max} entries")]
    CopyTooLarge {
        /// The bound.
        max: usize,
    },
    /// The copy visits a deeper directory than the bound allows.
    #[error("the copy is deeper than {max} directories")]
    CopyTooDeep {
        /// The bound.
        max: usize,
    },
    /// The platform offers no symbolic link support.
    #[error("{path} is a symbolic link, which this platform cannot copy")]
    UnsupportedLink {
        /// The rejected path.
        path: PathBuf,
    },
    /// The filesystem refused one step of the mutation.
    #[error("{path} could not be changed")]
    Filesystem {
        /// The path that the step named.
        path: PathBuf,
        /// The reported cause.
        #[source]
        source: io::Error,
    },
}

/// One entry that moves from its origin to its destination.
#[derive(Clone, Debug)]
struct Relocation {
    origin: PathBuf,
    destination: PathBuf,
}

/// The validated filesystem work of one mutation.
#[derive(Clone, Debug)]
enum PlannedWork {
    Create { path: PathBuf, kind: EntryKind },
    Copy(Vec<Relocation>),
    Move(Vec<Relocation>),
    Discard(Vec<PathBuf>),
}

/// One complete mutation that passed every validation.
///
/// The plan holds the filesystem work, the affected buffer paths, and the
/// directories that need a new read. It changes nothing until
/// [`MutationPlan::apply`] runs.
#[derive(Clone, Debug)]
pub struct MutationPlan {
    work: PlannedWork,
    updates: Vec<BufferPathUpdate>,
    changed: Vec<PathBuf>,
    selection: Option<PathBuf>,
}

impl MutationPlan {
    /// Validates one operation against the workspace and the loaded buffers.
    ///
    /// The function reads the filesystem, but it writes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`MutationError`] for an empty or oversized operation, a path
    /// outside the workspace, a missing source, a destination collision, a
    /// directory that would receive one of its own parents, and a removed entry
    /// whose buffer holds unsaved changes.
    pub fn stage(
        operation: &FileOperation,
        root: &Path,
        buffers: &[OpenBuffer],
    ) -> Result<Self, MutationError> {
        match operation {
            FileOperation::Create { path, kind } => stage_create(path, *kind, root),
            FileOperation::Delete { paths } => stage_delete(paths, root, buffers),
            FileOperation::Rename { from, to } => {
                stage_relocations(TransferMode::Move, &[relocate(from, to)], root, buffers)
            }
            FileOperation::Transfer {
                mode,
                sources,
                destination,
            } => {
                check_count(sources.len())?;
                check_contained(destination, root)?;
                check_directory(destination)?;
                let mut relocations = Vec::with_capacity(sources.len());
                for source in sources {
                    let name = source.file_name().ok_or_else(|| MutationError::Outside {
                        path: source.clone(),
                    })?;
                    relocations.push(relocate(source, &destination.join(name)));
                }
                stage_relocations(*mode, &relocations, root, buffers)
            }
        }
    }

    /// Returns the buffer paths that the mutation changes.
    #[must_use]
    pub fn updates(&self) -> &[BufferPathUpdate] {
        &self.updates
    }

    /// Performs the validated filesystem work.
    ///
    /// A failure of one path unwinds every staged step, so the workspace keeps
    /// the state that it held before the call.
    ///
    /// # Errors
    ///
    /// Returns [`MutationError::Filesystem`] and the copy bounds when the
    /// filesystem refuses one step.
    pub fn apply(self) -> Result<MutationOutcome, MutationError> {
        match &self.work {
            PlannedWork::Create { path, kind } => create(path, *kind)?,
            PlannedWork::Copy(relocations) => transfer(TransferMode::Copy, relocations)?,
            PlannedWork::Move(relocations) => transfer(TransferMode::Move, relocations)?,
            PlannedWork::Discard(paths) => discard(paths)?,
        }
        Ok(MutationOutcome {
            updates: self.updates,
            changed: self.changed,
            selection: self.selection,
        })
    }
}

/// Returns one relocation from a source and a complete destination path.
fn relocate(origin: &Path, destination: &Path) -> Relocation {
    Relocation {
        origin: origin.to_path_buf(),
        destination: destination.to_path_buf(),
    }
}

/// Validates one create operation.
fn stage_create(path: &Path, kind: EntryKind, root: &Path) -> Result<MutationPlan, MutationError> {
    check_entry(path, root)?;
    let parent = path.parent().ok_or_else(|| MutationError::Outside {
        path: path.to_path_buf(),
    })?;
    check_directory(parent)?;
    check_free(path)?;
    Ok(MutationPlan {
        work: PlannedWork::Create {
            path: path.to_path_buf(),
            kind,
        },
        updates: Vec::new(),
        changed: vec![parent.to_path_buf()],
        selection: Some(path.to_path_buf()),
    })
}

/// Validates one delete operation.
fn stage_delete(
    paths: &[PathBuf],
    root: &Path,
    buffers: &[OpenBuffer],
) -> Result<MutationPlan, MutationError> {
    check_count(paths.len())?;
    let mut changed = Vec::new();
    for path in paths {
        check_entry(path, root)?;
        check_exists(path)?;
        // A removed file must never discard unsaved work.
        if let Some(buffer) = buffers
            .iter()
            .find(|buffer| buffer.is_modified && buffer.path.starts_with(path))
        {
            return Err(MutationError::DirtyBuffer {
                path: buffer.path.clone(),
            });
        }
        if let Some(parent) = path.parent() {
            changed.push(parent.to_path_buf());
        }
    }
    changed.sort();
    changed.dedup();
    Ok(MutationPlan {
        work: PlannedWork::Discard(paths.to_vec()),
        updates: Vec::new(),
        changed,
        selection: None,
    })
}

/// Validates one copy or move of complete source and destination pairs.
fn stage_relocations(
    mode: TransferMode,
    relocations: &[Relocation],
    root: &Path,
    buffers: &[OpenBuffer],
) -> Result<MutationPlan, MutationError> {
    check_count(relocations.len())?;
    let mut changed = Vec::new();
    for (index, relocation) in relocations.iter().enumerate() {
        check_entry(&relocation.origin, root)?;
        check_entry(&relocation.destination, root)?;
        let kind = check_exists(&relocation.origin)?;
        let parent =
            relocation
                .destination
                .parent()
                .ok_or_else(|| MutationError::NotADirectory {
                    path: relocation.destination.clone(),
                })?;
        check_directory(parent)?;
        check_free(&relocation.destination)?;
        // Two sources with one name would overwrite each other during the
        // commit, so the collision must fail before any staging starts.
        if relocations[..index]
            .iter()
            .any(|earlier| earlier.destination == relocation.destination)
        {
            return Err(MutationError::Collision {
                path: relocation.destination.clone(),
            });
        }
        if kind == EntryKind::Directory && relocation.destination.starts_with(&relocation.origin) {
            return Err(MutationError::IntoDescendant {
                path: relocation.destination.clone(),
            });
        }
        changed.push(parent.to_path_buf());
        if mode == TransferMode::Move
            && let Some(parent) = relocation.origin.parent()
        {
            changed.push(parent.to_path_buf());
        }
    }
    changed.sort();
    changed.dedup();

    let updates = match mode {
        TransferMode::Copy => Vec::new(),
        TransferMode::Move => buffer_updates(relocations, buffers),
    };
    let work = match mode {
        TransferMode::Copy => PlannedWork::Copy(relocations.to_vec()),
        TransferMode::Move => PlannedWork::Move(relocations.to_vec()),
    };
    Ok(MutationPlan {
        work,
        updates,
        changed,
        selection: relocations
            .first()
            .map(|relocation| relocation.destination.clone()),
    })
}

/// Returns the new path of every buffer that one move retargets.
///
/// A buffer of a moved directory keeps its identity and follows the directory,
/// so the buffer of a renamed file stays the same buffer.
fn buffer_updates(relocations: &[Relocation], buffers: &[OpenBuffer]) -> Vec<BufferPathUpdate> {
    let mut updates = Vec::new();
    for relocation in relocations {
        for buffer in buffers {
            let Ok(relative) = buffer.path.strip_prefix(&relocation.origin) else {
                continue;
            };
            updates.push(BufferPathUpdate {
                buffer: buffer.id,
                path: relocation.destination.join(relative),
            });
        }
    }
    updates
}

/// Rejects an empty or oversized path list.
fn check_count(count: usize) -> Result<(), MutationError> {
    if count == 0 {
        return Err(MutationError::Empty);
    }
    if count > MUTATION_PATHS_MAX {
        return Err(MutationError::TooManyPaths {
            count,
            max: MUTATION_PATHS_MAX,
        });
    }
    Ok(())
}

/// Rejects a path that leaves the workspace root.
fn check_contained(path: &Path, root: &Path) -> Result<(), MutationError> {
    let escapes = path
        .components()
        .any(|component| component == Component::ParentDir);
    if escapes || !path.starts_with(root) {
        return Err(MutationError::Outside {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Rejects a path that leaves the workspace root or names the root itself.
fn check_entry(path: &Path, root: &Path) -> Result<(), MutationError> {
    check_contained(path, root)?;
    if path == root {
        return Err(MutationError::Outside {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Returns the kind of an entry that must exist.
fn check_exists(path: &Path) -> Result<EntryKind, MutationError> {
    match fs::symlink_metadata(path) {
        // A symbolic link takes the kind of its target, so a link to a
        // directory cannot receive one of its own parents either.
        Ok(_) => Ok(match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => EntryKind::Directory,
            _ => EntryKind::File,
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(MutationError::Missing {
            path: path.to_path_buf(),
        }),
        Err(source) => Err(MutationError::Filesystem {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Rejects a destination that holds an entry already.
fn check_free(path: &Path) -> Result<(), MutationError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(MutationError::Collision {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MutationError::Filesystem {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Rejects a destination that names no existing directory.
fn check_directory(path: &Path) -> Result<(), MutationError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(MutationError::NotADirectory {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(MutationError::Missing {
            path: path.to_path_buf(),
        }),
        Err(source) => Err(MutationError::Filesystem {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Creates one empty file or one empty directory.
///
/// Both calls fail when the path exists, so the collision check of the staging
/// step cannot be defeated by a concurrent write.
fn create(path: &Path, kind: EntryKind) -> Result<(), MutationError> {
    let created = match kind {
        EntryKind::Directory => fs::create_dir(path),
        EntryKind::File => File::create_new(path).map(drop),
    };
    created.map_err(|source| MutationError::Filesystem {
        path: path.to_path_buf(),
        source,
    })
}

/// Copies or moves every entry, or leaves the workspace unchanged.
fn transfer(mode: TransferMode, relocations: &[Relocation]) -> Result<(), MutationError> {
    let mut staged = StagedTransfer::new(mode);
    for relocation in relocations {
        staged.stage(relocation)?;
    }
    staged.commit()
}

/// Removes every entry, or leaves the workspace unchanged.
fn discard(paths: &[PathBuf]) -> Result<(), MutationError> {
    let mut staged = StagedDiscard::default();
    for path in paths {
        staged.stage(path)?;
    }
    staged.commit();
    Ok(())
}

/// One entry that waits under a temporary name for the commit.
#[derive(Debug)]
struct StagedItem {
    origin: PathBuf,
    temporary: PathBuf,
    destination: PathBuf,
}

/// How much of one staged transfer reached its destination.
#[derive(Clone, Copy, Debug)]
enum StageState {
    /// The transfer needs an unwind while it drops.
    Staged {
        /// The number of items that already hold their destination.
        committed: usize,
    },
    /// The transfer finished. The drop leaves every path in place.
    Settled,
}

/// One copy or move that either finishes completely or leaves no trace.
///
/// Every fallible step writes a temporary name beside the destination. The
/// commit renames the temporary names, which is one cheap step inside one
/// directory. A drop before the commit undoes every staged step.
#[derive(Debug)]
struct StagedTransfer {
    mode: TransferMode,
    items: Vec<StagedItem>,
    state: StageState,
}

impl StagedTransfer {
    /// Creates one empty transfer.
    fn new(mode: TransferMode) -> Self {
        Self {
            mode,
            items: Vec::new(),
            state: StageState::Staged { committed: 0 },
        }
    }

    /// Puts one entry beside its destination under a temporary name.
    fn stage(&mut self, relocation: &Relocation) -> Result<(), MutationError> {
        let parent =
            relocation
                .destination
                .parent()
                .ok_or_else(|| MutationError::NotADirectory {
                    path: relocation.destination.clone(),
                })?;
        let temporary = parent.join(temporary_name(&relocation.destination));
        match self.mode {
            TransferMode::Copy => {
                if let Err(error) = copy_tree(&relocation.origin, &temporary) {
                    let _ = remove_tree(&temporary);
                    return Err(error);
                }
            }
            TransferMode::Move => {
                fs::rename(&relocation.origin, &temporary).map_err(|source| {
                    MutationError::Filesystem {
                        path: relocation.origin.clone(),
                        source,
                    }
                })?;
            }
        }
        self.items.push(StagedItem {
            origin: relocation.origin.clone(),
            temporary,
            destination: relocation.destination.clone(),
        });
        Ok(())
    }

    /// Gives every staged entry its destination name.
    fn commit(mut self) -> Result<(), MutationError> {
        for index in 0..self.items.len() {
            let item = &self.items[index];
            fs::rename(&item.temporary, &item.destination).map_err(|source| {
                MutationError::Filesystem {
                    path: item.destination.clone(),
                    source,
                }
            })?;
            self.state = StageState::Staged {
                committed: index + 1,
            };
        }
        self.state = StageState::Settled;
        Ok(())
    }
}

impl Drop for StagedTransfer {
    fn drop(&mut self) {
        let StageState::Staged { committed } = self.state else {
            return;
        };
        // The unwind repairs a failed transfer. Every step is best effort,
        // because the mutation already reports the first cause.
        for item in &self.items[committed..] {
            match self.mode {
                TransferMode::Copy => {
                    let _ = remove_tree(&item.temporary);
                }
                TransferMode::Move => {
                    let _ = fs::rename(&item.temporary, &item.origin);
                }
            }
        }
        for item in &self.items[..committed] {
            match self.mode {
                TransferMode::Copy => {
                    let _ = remove_tree(&item.destination);
                }
                TransferMode::Move => {
                    let _ = fs::rename(&item.destination, &item.origin);
                }
            }
        }
    }
}

/// One removal that either hides every entry or leaves every entry in place.
///
/// The rename to a temporary name is the visible removal. The commit then
/// removes the temporary names. A failed removal leaves one hidden temporary
/// entry, which the default hidden-entry policy keeps out of the tree.
#[derive(Debug, Default)]
struct StagedDiscard {
    items: Vec<StagedItem>,
    settled: bool,
}

impl StagedDiscard {
    /// Renames one entry to a temporary name beside itself.
    fn stage(&mut self, path: &Path) -> Result<(), MutationError> {
        let parent = path.parent().ok_or_else(|| MutationError::Outside {
            path: path.to_path_buf(),
        })?;
        let temporary = parent.join(temporary_name(path));
        fs::rename(path, &temporary).map_err(|source| MutationError::Filesystem {
            path: path.to_path_buf(),
            source,
        })?;
        self.items.push(StagedItem {
            origin: path.to_path_buf(),
            destination: temporary.clone(),
            temporary,
        });
        Ok(())
    }

    /// Removes every renamed entry.
    fn commit(mut self) {
        self.settled = true;
        for item in &self.items {
            let _ = remove_tree(&item.temporary);
        }
    }
}

impl Drop for StagedDiscard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        for item in &self.items {
            let _ = fs::rename(&item.temporary, &item.origin);
        }
    }
}

/// Copies one file, one symbolic link, or one complete directory.
///
/// The walk uses an explicit stack, so a deep directory never grows the call
/// stack. The entry and depth bounds stop a very large or looping tree.
fn copy_tree(source: &Path, target: &Path) -> Result<(), MutationError> {
    let mut stack = vec![(source.to_path_buf(), target.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((from, to, depth)) = stack.pop() {
        visited += 1;
        if visited > COPY_ENTRIES_MAX {
            return Err(MutationError::CopyTooLarge {
                max: COPY_ENTRIES_MAX,
            });
        }
        if depth > COPY_DEPTH_MAX {
            return Err(MutationError::CopyTooDeep {
                max: COPY_DEPTH_MAX,
            });
        }
        let metadata = fs::symlink_metadata(&from).map_err(|source| MutationError::Filesystem {
            path: from.clone(),
            source,
        })?;
        if metadata.is_symlink() {
            copy_link(&from, &to)?;
            continue;
        }
        if !metadata.is_dir() {
            fs::copy(&from, &to).map_err(|source| MutationError::Filesystem {
                path: from.clone(),
                source,
            })?;
            continue;
        }
        fs::create_dir(&to).map_err(|source| MutationError::Filesystem {
            path: to.clone(),
            source,
        })?;
        let reader = fs::read_dir(&from).map_err(|source| MutationError::Filesystem {
            path: from.clone(),
            source,
        })?;
        for entry in reader {
            let entry = entry.map_err(|source| MutationError::Filesystem {
                path: from.clone(),
                source,
            })?;
            let name = entry.file_name();
            stack.push((from.join(&name), to.join(&name), depth + 1));
        }
    }
    Ok(())
}

/// Recreates one symbolic link at the target path.
#[cfg(unix)]
fn copy_link(source: &Path, target: &Path) -> Result<(), MutationError> {
    let link = fs::read_link(source).map_err(|error| MutationError::Filesystem {
        path: source.to_path_buf(),
        source: error,
    })?;
    std::os::unix::fs::symlink(link, target).map_err(|error| MutationError::Filesystem {
        path: target.to_path_buf(),
        source: error,
    })
}

/// Reports that this platform offers no symbolic link support.
#[cfg(not(unix))]
fn copy_link(source: &Path, _target: &Path) -> Result<(), MutationError> {
    Err(MutationError::UnsupportedLink {
        path: source.to_path_buf(),
    })
}

/// Removes one file, one symbolic link, or one complete directory.
fn remove_tree(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}
