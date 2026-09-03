//! Validated workspace mutations.
//!
//! One mutation renames, creates, deletes, copies, or moves workspace entries.
//! [`MutationPlan::stage`] validates the complete operation and computes every
//! affected buffer path before anything on disk changes.
//! [`MutationPlan::apply`] then performs the filesystem work through a staged
//! replacement, so a failure of one path leaves no partial result.
//!
//! One create may name directories that the workspace does not hold yet. The
//! staging then produces one create for each missing level and one create for
//! the entry itself, so a failure names the exact level that failed. A level
//! that holds another kind of entry refuses the whole create.
//!
//! A destination that holds an entry refuses the mutation. [`Overwrite`] names
//! the destinations that one confirmed answer approved, and only those
//! destinations lose their entries. The commit parks each replaced entry under
//! a temporary name, so a later failure puts it back. See `docs/files.md`.
//!
//! Both functions block. Run them on the bounded worker service only. See
//! `docs/files.md` and `docs/responsiveness.md`.
//!
//! Every path is a validated worktree-relative path, and every filesystem step
//! runs through the capability directory of one canonical root. Containment
//! therefore holds by construction, and no step can name an entry beside or
//! above the workspace. The staging records the resolved identity of each path
//! and confirms it again immediately before the commit, so a concurrent
//! replacement cannot make the commit destroy another entry than the staging
//! approved. See `docs/files.md`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_std::fs::Dir;
use thiserror::Error;

use kvim_path::{
    ResolvedWorktreePath, WORKTREE_PATH_COMPONENTS_MAX, WorktreeConfinementError,
    WorktreeDirectoryPath, WorktreeRelativePath, WorktreeRoot,
};

use crate::durable::{
    DurableOutcome, FailurePoint, Indeterminate, RecoveryAction, RecoveryFailure, fail_at,
};

use super::buffer::BufferId;
use super::file::temporary_name;
use super::tree::EntryKind;

/// The largest number of paths that one mutation names.
pub const MUTATION_PATHS_MAX: usize = 128;

/// The largest number of sibling names that one same-directory copy tests.
pub const SAME_DIRECTORY_COPY_CANDIDATES_MAX: usize = 128;

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

/// One destination that holds an entry already.
///
/// The kind belongs to the entry that the staging observed. A confirmed answer
/// carries it back, so the second staging can reject a destination that became
/// another kind while the question waited.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TakenDestination {
    /// The contained destination path.
    pub path: WorktreeRelativePath,
    /// The kind of the entry that holds it.
    pub kind: EntryKind,
}

/// Whether one mutation may destroy an entry that holds a destination.
///
/// The default refuses every taken destination, so a caller must name each
/// entry that it destroys.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Overwrite {
    /// Every taken destination refuses the mutation.
    #[default]
    Refuse,
    /// The named destinations lose their entries.
    ///
    /// Every destination outside the list still refuses the mutation, so the
    /// list is the complete permission of one answer.
    Replace(Vec<TakenDestination>),
}

/// One requested workspace mutation.
///
/// Every path is contained by construction, so no operation can name an entry
/// outside the workspace root that validated it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileOperation {
    /// Create one empty file or one empty directory.
    ///
    /// The path may name directories that the workspace does not hold yet. The
    /// staging then creates every missing directory above the entry, and it
    /// refuses a level that holds another kind of entry. See `docs/files.md`.
    Create {
        /// The path of the new entry.
        path: WorktreeRelativePath,
        /// The kind of the new entry.
        kind: EntryKind,
    },
    /// Remove the named entries.
    Delete {
        /// The entries to remove.
        paths: Vec<WorktreeRelativePath>,
    },
    /// Give one entry another path inside the workspace.
    Rename {
        /// The entry that keeps its content.
        from: WorktreeRelativePath,
        /// The complete new path.
        to: WorktreeRelativePath,
    },
    /// Copy or move the named entries into one directory.
    Transfer {
        /// Whether the sources stay in place.
        mode: TransferMode,
        /// The entries to copy or move.
        sources: Vec<WorktreeRelativePath>,
        /// The directory that receives the entries.
        destination: WorktreeDirectoryPath,
    },
}

/// One loaded buffer that a mutation can affect.
#[derive(Clone, Debug)]
pub struct OpenBuffer {
    /// The stable identity of the buffer.
    pub id: BufferId,
    /// The current contained path of the buffer.
    pub path: WorktreeRelativePath,
    /// Whether the buffer holds unsaved changes.
    pub is_modified: bool,
}

/// The new path of one loaded buffer.
///
/// The path is the absolute display path of the retargeted buffer, because the
/// event loop names a loaded file by the path that the reader sees.
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
    /// The path did not remain contained in the workspace root.
    ///
    /// The path itself is contained by construction, so this names a link that
    /// leaves the root, a link chain that loops, and an entry that another
    /// program replaced while the mutation ran.
    #[error("{path} is not contained in the workspace")]
    Confinement {
        /// The rejected path.
        path: PathBuf,
        /// The reported cause.
        #[source]
        source: WorktreeConfinementError,
    },
    /// The source holds no entry.
    #[error("{path} holds no entry")]
    Missing {
        /// The rejected path.
        path: PathBuf,
    },
    /// The destinations hold entries already.
    ///
    /// The editor asks the user before it replaces them. See `docs/files.md`.
    #[error("{}", collision_message(.entries))]
    Collision {
        /// Every rejected destination, with the kind that it holds.
        entries: Vec<TakenDestination>,
    },
    /// Two sources of one mutation claim one destination name.
    #[error("two entries claim {path}")]
    DuplicateDestination {
        /// The rejected destination.
        path: PathBuf,
    },
    /// The source and the destination name one entry.
    #[error("{path} is its own destination")]
    SameEntry {
        /// The rejected destination.
        path: PathBuf,
    },
    /// One approved destination holds another kind of entry now.
    #[error("{path} changed while the question waited")]
    DestinationChanged {
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
    /// A same-directory copy found no free sibling name inside its bound.
    #[error("{path} has no free copy name among {max} candidates")]
    SameDirectoryCopyNamesExhausted {
        /// The source that the user copied.
        path: PathBuf,
        /// The number of candidate names that staging checked.
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

/// Returns the message of one destination collision.
///
/// One destination appears by its path. Several appear as a count, because the
/// message line holds one row.
fn collision_message(entries: &[TakenDestination]) -> String {
    match entries {
        [entry] => format!("{} exists already", entry.path.as_path().display()),
        _ => format!("{} entries exist already", entries.len()),
    }
}

/// One contained path and the identity that the staging resolved for it.
///
/// The filesystem work names the requested path, so a mutation of a symbolic
/// link moves the link and not its target. The resolved identity answers one
/// question only: does the path still name what the staging approved? The
/// commit asks it immediately before it destroys or replaces anything.
#[derive(Clone, Debug)]
struct StagedPath {
    requested: WorktreeRelativePath,
    resolved: ResolvedWorktreePath,
}

impl StagedPath {
    /// Resolves one contained path and records its identity.
    fn stage(root: &WorktreeRoot, requested: &WorktreeRelativePath) -> Result<Self, MutationError> {
        let resolved = root
            .resolve(requested)
            .map_err(|source| MutationError::Confinement {
                path: display_path(root, requested.as_path()),
                source,
            })?;
        Ok(Self {
            requested: requested.clone(),
            resolved,
        })
    }

    /// Confirms that the path still names the entry that the staging approved.
    fn revalidate(&self, root: &WorktreeRoot) -> Result<(), MutationError> {
        root.revalidate(&self.requested, &self.resolved)
            .map_err(|source| MutationError::Confinement {
                path: self.display_path(root),
                source,
            })
    }

    /// Returns the path that the capability directory receives.
    fn as_path(&self) -> &Path {
        self.requested.as_path()
    }

    /// Returns the absolute path that the reader sees.
    fn display_path(&self, root: &WorktreeRoot) -> PathBuf {
        display_path(root, self.as_path())
    }
}

/// One entry that moves from its origin to its destination.
#[derive(Clone, Debug)]
struct Relocation {
    origin: StagedPath,
    destination: StagedPath,
    /// Whether the destination holds an entry that the commit replaces.
    replaces: bool,
}

/// One entry that a create makes.
#[derive(Clone, Debug)]
struct PlannedEntry {
    path: StagedPath,
    kind: EntryKind,
}

/// The validated filesystem work of one mutation.
///
/// A create names every level that the workspace does not hold yet, from the
/// top down, and the entry itself last. Every level before the last one is a
/// directory, because a directory must exist before its child.
#[derive(Clone, Debug)]
enum PlannedWork {
    Create(Vec<PlannedEntry>),
    Copy(Vec<Relocation>),
    Move(Vec<Relocation>),
    Discard(Vec<StagedPath>),
}

/// One complete mutation that passed every validation.
///
/// The plan holds the canonical root, the filesystem work, the affected buffer
/// paths, and the directories that need a new read. It changes nothing until
/// [`MutationPlan::apply`] runs.
#[derive(Clone, Debug)]
pub struct MutationPlan {
    root: Arc<WorktreeRoot>,
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
    /// that leaves the root through a link, a missing source, a destination
    /// collision, a directory that would receive one of its own parents, and a
    /// removed entry whose buffer holds unsaved changes.
    pub fn stage(
        operation: &FileOperation,
        root: &Arc<WorktreeRoot>,
        buffers: &[OpenBuffer],
    ) -> Result<Self, MutationError> {
        Self::stage_with(operation, root, buffers, &Overwrite::Refuse)
    }

    /// Validates one operation that may replace the approved destinations.
    ///
    /// The function reads the filesystem, but it writes nothing. Only
    /// [`Overwrite::Replace`] destroys an entry that holds a destination, and
    /// only the entries that its list names.
    ///
    /// # Errors
    ///
    /// Returns every error of [`MutationPlan::stage`], and
    /// [`MutationError::DestinationChanged`] for an approved destination that
    /// holds another kind of entry now.
    pub(crate) fn stage_with(
        operation: &FileOperation,
        root: &Arc<WorktreeRoot>,
        buffers: &[OpenBuffer],
        overwrite: &Overwrite,
    ) -> Result<Self, MutationError> {
        match operation {
            FileOperation::Create { path, kind } => stage_create(path, *kind, root),
            FileOperation::Delete { paths } => stage_delete(paths, root, buffers),
            FileOperation::Rename { from, to } => {
                stage_relocations(TransferMode::Move, &[(from, to)], root, buffers, overwrite)
            }
            FileOperation::Transfer {
                mode,
                sources,
                destination,
            } => {
                check_count(sources.len())?;
                check_directory(root, destination)?;
                let mut destinations = Vec::with_capacity(sources.len());
                for source in sources {
                    destinations.push(transfer_destination(destination, source)?);
                }
                let pairs: Vec<(&WorktreeRelativePath, &WorktreeRelativePath)> =
                    sources.iter().zip(destinations.iter()).collect();
                stage_relocations(*mode, &pairs, root, buffers, overwrite)
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
    /// Every destructive step confirms the resolved identity of its path
    /// immediately before it runs, so an entry that another program replaced
    /// after the staging stops the mutation instead of losing its content.
    ///
    /// A failure of one path unwinds every staged step, so the workspace keeps
    /// the state that it held before the call. The unwind also puts back every
    /// entry that an approved overwrite parked.
    ///
    /// # Errors
    ///
    /// Returns [`MutationError::Confinement`] for a path that another program
    /// replaced, [`MutationError::Filesystem`] and the copy bounds when the
    /// filesystem refuses one step.
    pub fn apply(self) -> DurableOutcome<MutationOutcome, MutationError> {
        let affected = self.changed.clone();
        let root = &self.root;
        let result = match &self.work {
            PlannedWork::Create(entries) => create(root, entries),
            PlannedWork::Copy(relocations) => transfer(root, TransferMode::Copy, relocations),
            PlannedWork::Move(relocations) => transfer(root, TransferMode::Move, relocations),
            PlannedWork::Discard(paths) => discard(root, paths),
        };
        match result {
            Ok(()) => DurableOutcome::Committed(MutationOutcome {
                updates: self.updates,
                changed: self.changed,
                selection: self.selection,
            }),
            Err(failure) if !failure.indeterminate && failure.recovery.is_empty() => {
                DurableOutcome::Unchanged(failure.primary)
            }
            Err(failure) => DurableOutcome::Indeterminate(Indeterminate::from_operation(
                failure.primary,
                failure.recovery,
                affected,
            )),
        }
    }
}

/// Returns the absolute path that one reader sees for a contained path.
///
/// An empty relative path names the workspace root itself.
fn display_path(root: &WorktreeRoot, relative: &Path) -> PathBuf {
    if relative.as_os_str().is_empty() {
        return root.as_path().to_path_buf();
    }
    root.as_path().join(relative)
}

/// Returns the parent directory of one contained path.
///
/// An entry directly below the root reports the empty path, which the
/// capability directory reads as the root itself.
fn parent_of(path: &Path) -> &Path {
    path.parent().unwrap_or_else(|| Path::new(""))
}

/// Returns the complete destination of one transferred source.
fn transfer_destination(
    directory: &WorktreeDirectoryPath,
    source: &WorktreeRelativePath,
) -> Result<WorktreeRelativePath, MutationError> {
    let name = source
        .as_path()
        .file_name()
        .expect("a contained relative path ends with one ordinary component");
    let directory = directory
        .relative_path()
        .map_or_else(PathBuf::new, |path| path.as_path().to_path_buf());
    WorktreeRelativePath::new(directory.join(name)).map_err(|_| MutationError::NotADirectory {
        path: directory.join(name),
    })
}

/// Validates one create operation and every missing directory above it.
///
/// The path may name directories that the workspace does not hold yet, so the
/// plan holds one create for each missing level and one create for the entry
/// itself. A level that already holds a directory needs no create. A level that
/// holds another kind of entry refuses the operation and names that entry,
/// because no new entry can live inside it. See `docs/files.md`.
fn stage_create(
    path: &WorktreeRelativePath,
    kind: EntryKind,
    root: &Arc<WorktreeRoot>,
) -> Result<MutationPlan, MutationError> {
    // The levels answer before the leaf resolves, because a level that holds a
    // file makes the leaf uncontained. The refusal must name the level that
    // holds the file, not the path that the reader typed.
    let parent = parent_of(path.as_path()).to_path_buf();
    let mut entries = Vec::new();
    let mut level = PathBuf::new();
    for component in parent.components() {
        level.push(component);
        if matches!(create_level(root, &level)?, CreateLevel::Present) {
            continue;
        }
        let missing =
            WorktreeRelativePath::new(&level).expect("a prefix of a contained path is contained");
        entries.push(PlannedEntry {
            path: StagedPath::stage(root, &missing)?,
            kind: EntryKind::Directory,
        });
    }
    let staged = StagedPath::stage(root, path)?;
    check_free(root, &staged)?;
    let mut changed: Vec<PathBuf> = entries
        .iter()
        .map(|entry| display_path(root, parent_of(entry.path.as_path())))
        .collect();
    changed.push(display_path(root, &parent));
    changed.sort();
    changed.dedup();
    let selection = staged.display_path(root);
    entries.push(PlannedEntry { path: staged, kind });
    debug_assert!(
        entries.len() <= WORKTREE_PATH_COMPONENTS_MAX,
        "the plan holds one create for each name of a contained path, and \
         WorktreeRelativePath bounds that count"
    );
    Ok(MutationPlan {
        root: Arc::clone(root),
        selection: Some(selection),
        changed,
        work: PlannedWork::Create(entries),
        updates: Vec::new(),
    })
}

/// Whether one level above a new entry already holds a directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CreateLevel {
    /// The level holds a directory, so the create skips it.
    Present,
    /// The level holds no entry, so the create makes it.
    Missing,
}

/// Returns whether one level above a new entry already holds a directory.
fn create_level(root: &WorktreeRoot, path: &Path) -> Result<CreateLevel, MutationError> {
    match root.directory().metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(CreateLevel::Present),
        Ok(_) => Err(MutationError::NotADirectory {
            path: display_path(root, path),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(CreateLevel::Missing),
        Err(source) => Err(MutationError::Filesystem {
            path: display_path(root, path),
            source,
        }),
    }
}

/// Validates one delete operation.
fn stage_delete(
    paths: &[WorktreeRelativePath],
    root: &Arc<WorktreeRoot>,
    buffers: &[OpenBuffer],
) -> Result<MutationPlan, MutationError> {
    check_count(paths.len())?;
    let mut changed = Vec::new();
    let mut staged = Vec::with_capacity(paths.len());
    for path in paths {
        let path = StagedPath::stage(root, path)?;
        check_exists(root, &path)?;
        // A removed file must never discard unsaved work.
        check_clean_subtree(root, path.as_path(), buffers)?;
        changed.push(display_path(root, parent_of(path.as_path())));
        staged.push(path);
    }
    changed.sort();
    changed.dedup();
    Ok(MutationPlan {
        root: Arc::clone(root),
        work: PlannedWork::Discard(staged),
        updates: Vec::new(),
        changed,
        selection: None,
    })
}

/// Validates one copy or move of complete source and destination pairs.
///
/// The loop collects every taken destination instead of stopping at the first
/// one, so the refusal reports the complete size of the collision. The editor
/// names that size in its question. See `docs/files.md`.
fn stage_relocations(
    mode: TransferMode,
    pairs: &[(&WorktreeRelativePath, &WorktreeRelativePath)],
    root: &Arc<WorktreeRoot>,
    buffers: &[OpenBuffer],
    overwrite: &Overwrite,
) -> Result<MutationPlan, MutationError> {
    check_count(pairs.len())?;
    let mut changed = Vec::new();
    let mut planned = Vec::with_capacity(pairs.len());
    let mut collisions = Vec::new();
    for (origin, destination) in pairs {
        let origin = StagedPath::stage(root, origin)?;
        let mut destination = StagedPath::stage(root, destination)?;
        let kind = check_exists(root, &origin)?;
        if origin.as_path() == destination.as_path() {
            destination = match mode {
                TransferMode::Copy => same_directory_copy_destination(root, &origin, kind)?,
                TransferMode::Move => {
                    return Err(MutationError::SameEntry {
                        path: destination.display_path(root),
                    });
                }
            };
        }
        let parent = parent_of(destination.as_path()).to_path_buf();
        check_existing_directory(root, &parent)?;
        // Two sources with one name would overwrite each other during the
        // commit, so the collision must fail before any staging starts.
        if planned
            .iter()
            .any(|earlier: &Relocation| earlier.destination.as_path() == destination.as_path())
        {
            return Err(MutationError::DuplicateDestination {
                path: destination.display_path(root),
            });
        }
        if kind == EntryKind::Directory && destination.as_path().starts_with(origin.as_path()) {
            return Err(MutationError::IntoDescendant {
                path: destination.display_path(root),
            });
        }
        let replaces = check_destination(root, &destination, overwrite, buffers, &mut collisions)?;
        changed.push(display_path(root, &parent));
        if mode == TransferMode::Move {
            changed.push(display_path(root, parent_of(origin.as_path())));
        }
        planned.push(Relocation {
            origin,
            destination,
            replaces,
        });
    }
    if !collisions.is_empty() {
        return Err(MutationError::Collision {
            entries: collisions,
        });
    }
    changed.sort();
    changed.dedup();

    let updates = match mode {
        TransferMode::Copy => Vec::new(),
        TransferMode::Move => buffer_updates(root, &planned, buffers),
    };
    let selection = planned
        .first()
        .map(|relocation| relocation.destination.display_path(root));
    let work = match mode {
        TransferMode::Copy => PlannedWork::Copy(planned),
        TransferMode::Move => PlannedWork::Move(planned),
    };
    Ok(MutationPlan {
        root: Arc::clone(root),
        work,
        updates,
        changed,
        selection,
    })
}

/// Selects the first free sibling name for a copy beside its source.
///
/// Files insert the suffix before their final extension. Directories,
/// extensionless files, and dotfiles append it to the complete name.
fn same_directory_copy_destination(
    root: &WorktreeRoot,
    origin: &StagedPath,
    kind: EntryKind,
) -> Result<StagedPath, MutationError> {
    let path = origin.as_path();
    let parent = parent_of(path);
    let name = path
        .file_name()
        .expect("a contained relative path ends with one ordinary component");
    for offset in 0..SAME_DIRECTORY_COPY_CANDIDATES_MAX {
        let suffix = offset
            .checked_add(2)
            .expect("the candidate bound fits inside usize");
        let candidate_name = suffixed_copy_name(name, kind, suffix);
        let requested = match WorktreeRelativePath::new(parent.join(candidate_name)) {
            Ok(requested) => requested,
            Err(_) => continue,
        };
        let candidate = StagedPath::stage(root, &requested)?;
        if peek(root, candidate.as_path())?.is_none() {
            return Ok(candidate);
        }
    }
    Err(MutationError::SameDirectoryCopyNamesExhausted {
        path: origin.display_path(root),
        max: SAME_DIRECTORY_COPY_CANDIDATES_MAX,
    })
}

/// Adds one copy suffix while preserving a file's final extension.
fn suffixed_copy_name(name: &OsStr, kind: EntryKind, suffix: usize) -> OsString {
    let path = Path::new(name);
    let mut candidate = OsString::new();
    if kind == EntryKind::File
        && let (Some(stem), Some(extension)) = (path.file_stem(), path.extension())
    {
        candidate.push(stem);
        candidate.push(format!("_{suffix}."));
        candidate.push(extension);
        return candidate;
    }
    candidate.push(name);
    candidate.push(format!("_{suffix}"));
    candidate
}

/// Returns the new path of every buffer that one move retargets.
///
/// A buffer of a moved directory keeps its identity and follows the directory,
/// so the buffer of a renamed file stays the same buffer.
fn buffer_updates(
    root: &WorktreeRoot,
    relocations: &[Relocation],
    buffers: &[OpenBuffer],
) -> Vec<BufferPathUpdate> {
    let mut updates = Vec::new();
    for relocation in relocations {
        for buffer in buffers {
            let Ok(relative) = buffer
                .path
                .as_path()
                .strip_prefix(relocation.origin.as_path())
            else {
                continue;
            };
            updates.push(BufferPathUpdate {
                buffer: buffer.id,
                path: display_path(root, &relocation.destination.as_path().join(relative)),
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

/// Returns the kind of one entry, or `None` while the path holds none.
///
/// The lookup never follows the last component, so a mutation reads the link
/// itself. A symbolic link still takes the kind of its target, so a link to a
/// directory cannot receive one of its own parents either.
fn peek(root: &WorktreeRoot, path: &Path) -> Result<Option<EntryKind>, MutationError> {
    let directory = root.directory();
    match directory.symlink_metadata(path) {
        Ok(_) => Ok(Some(match directory.metadata(path) {
            Ok(metadata) if metadata.is_dir() => EntryKind::Directory,
            _ => EntryKind::File,
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(MutationError::Filesystem {
            path: display_path(root, path),
            source,
        }),
    }
}

/// Returns the kind of an entry that must exist.
fn check_exists(root: &WorktreeRoot, path: &StagedPath) -> Result<EntryKind, MutationError> {
    peek(root, path.as_path())?.ok_or_else(|| MutationError::Missing {
        path: path.display_path(root),
    })
}

/// Rejects a destination that holds an entry already.
fn check_free(root: &WorktreeRoot, path: &StagedPath) -> Result<(), MutationError> {
    match peek(root, path.as_path())? {
        None => Ok(()),
        Some(kind) => Err(MutationError::Collision {
            entries: vec![TakenDestination {
                path: path.requested.clone(),
                kind,
            }],
        }),
    }
}

/// Rejects a mutation that would discard the unsaved changes of one buffer.
fn check_clean_subtree(
    root: &WorktreeRoot,
    path: &Path,
    buffers: &[OpenBuffer],
) -> Result<(), MutationError> {
    let dirty = buffers
        .iter()
        .find(|buffer| buffer.is_modified && buffer.path.as_path().starts_with(path));
    match dirty {
        None => Ok(()),
        Some(buffer) => Err(MutationError::DirtyBuffer {
            path: display_path(root, buffer.path.as_path()),
        }),
    }
}

/// Reports whether one destination loses the entry that it holds.
///
/// A free destination reports `false`. A taken destination that the answer
/// approved reports `true`. Every other taken destination joins `collisions`,
/// which refuses the complete mutation after the loop, so no unapproved entry
/// reaches the commit.
///
/// # Errors
///
/// Returns [`MutationError::DirtyBuffer`] for a destination whose buffer holds
/// unsaved changes, and [`MutationError::DestinationChanged`] for an approved
/// destination that holds another kind of entry now.
fn check_destination(
    root: &WorktreeRoot,
    destination: &StagedPath,
    overwrite: &Overwrite,
    buffers: &[OpenBuffer],
    collisions: &mut Vec<TakenDestination>,
) -> Result<bool, MutationError> {
    let Some(kind) = peek(root, destination.as_path())? else {
        return Ok(false);
    };
    // The destination loses its entry, so it follows the rule of a removal.
    check_clean_subtree(root, destination.as_path(), buffers)?;
    let taken = TakenDestination {
        path: destination.requested.clone(),
        kind,
    };
    let Overwrite::Replace(approved) = overwrite else {
        collisions.push(taken);
        return Ok(false);
    };
    match approved
        .iter()
        .find(|entry| entry.path == destination.requested)
    {
        Some(entry) if entry.kind == kind => Ok(true),
        // The world changed while the question waited, so the answer would
        // destroy another entry than the question named.
        Some(_) => Err(MutationError::DestinationChanged {
            path: destination.display_path(root),
        }),
        None => {
            collisions.push(taken);
            Ok(false)
        }
    }
}

/// Rejects a destination directory that leaves the root or holds no directory.
fn check_directory(
    root: &WorktreeRoot,
    directory: &WorktreeDirectoryPath,
) -> Result<(), MutationError> {
    // The resolution answers the containment question with a typed cause, so a
    // directory that a link moves outside the root refuses the paste itself.
    root.resolve_directory(directory)
        .map_err(|source| MutationError::Confinement {
            path: directory.display_path(root),
            source,
        })?;
    let Some(relative) = directory.relative_path() else {
        // The capability directory is the root, so the root is a directory.
        return Ok(());
    };
    check_existing_directory(root, relative.as_path())
}

/// Rejects a contained path that names no existing directory.
fn check_existing_directory(root: &WorktreeRoot, path: &Path) -> Result<(), MutationError> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    match root.directory().metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(MutationError::NotADirectory {
            path: display_path(root, path),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(MutationError::Missing {
            path: display_path(root, path),
        }),
        Err(source) => Err(MutationError::Filesystem {
            path: display_path(root, path),
            source,
        }),
    }
}

struct MutationFailure {
    primary: MutationError,
    recovery: Vec<RecoveryFailure>,
    indeterminate: bool,
}

impl From<MutationError> for MutationFailure {
    fn from(primary: MutationError) -> Self {
        Self {
            primary,
            recovery: Vec::new(),
            indeterminate: false,
        }
    }
}

/// Creates every planned entry, or leaves the workspace unchanged.
fn create(root: &WorktreeRoot, entries: &[PlannedEntry]) -> Result<(), MutationFailure> {
    let mut staged = StagedCreate::new(root);
    for entry in entries {
        if let Err(primary) = staged.create(entry) {
            let recovery = staged.rollback();
            return Err(MutationFailure {
                primary,
                recovery,
                indeterminate: false,
            });
        }
    }
    staged.commit();
    Ok(())
}

/// One create that either makes every entry or leaves no trace.
///
/// The call makes the levels in order, because a directory must exist before
/// its child. A failure of one level removes every entry that the same call
/// already made, so the reader learns which level failed and the workspace
/// keeps the state that it held before the call.
#[derive(Debug)]
struct StagedCreate<'a> {
    root: &'a WorktreeRoot,
    made: Vec<PathBuf>,
    settled: bool,
}

impl<'a> StagedCreate<'a> {
    /// Creates one empty create.
    fn new(root: &'a WorktreeRoot) -> Self {
        Self {
            root,
            made: Vec::new(),
            settled: false,
        }
    }

    /// Makes one empty file or one empty directory.
    ///
    /// Both calls fail when the path exists, so the collision check of the
    /// staging step cannot be defeated by a concurrent write.
    fn create(&mut self, entry: &PlannedEntry) -> Result<(), MutationError> {
        entry.path.revalidate(self.root)?;
        let directory = self.root.directory();
        let path = entry.path.as_path();
        let created = match entry.kind {
            EntryKind::Directory => directory.create_dir(path),
            EntryKind::File => {
                let mut options = cap_std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                directory.open_with(path, &options).map(drop)
            }
        };
        created.map_err(|source| MutationError::Filesystem {
            path: entry.path.display_path(self.root),
            source,
        })?;
        self.made.push(path.to_path_buf());
        Ok(())
    }

    /// Keeps every entry that the call made.
    fn commit(mut self) {
        self.settled = true;
    }

    fn rollback(&mut self) -> Vec<RecoveryFailure> {
        let directory = self.root.directory();
        let mut failures = Vec::new();
        for path in self.made.iter().rev() {
            if let Err(source) = remove_tree(directory, path) {
                failures.push(RecoveryFailure::new(
                    display_path(self.root, path),
                    RecoveryAction::RemoveTemporary,
                    source,
                ));
            }
        }
        self.settled = true;
        failures
    }
}

impl Drop for StagedCreate<'_> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let _ = self.rollback();
    }
}

/// Copies or moves every entry, or leaves the workspace unchanged.
fn transfer(
    root: &WorktreeRoot,
    mode: TransferMode,
    relocations: &[Relocation],
) -> Result<(), MutationFailure> {
    let mut staged = StagedTransfer::new(root, mode);
    for relocation in relocations {
        if let Err(mut failure) = staged.stage(relocation) {
            let mut recovery = staged.rollback();
            failure.indeterminate |= !recovery.is_empty();
            failure.recovery.append(&mut recovery);
            return Err(failure);
        }
    }
    staged.commit()
}

/// Removes every entry, or leaves the workspace unchanged.
fn discard(root: &WorktreeRoot, paths: &[StagedPath]) -> Result<(), MutationFailure> {
    let mut staged = StagedDiscard::new(root);
    for path in paths {
        if let Err(primary) = staged.stage(path) {
            let recovery = staged.rollback();
            return Err(MutationFailure {
                primary,
                recovery,
                indeterminate: false,
            });
        }
    }
    staged.commit()
}

/// One entry that waits under a temporary name for the commit.
#[derive(Debug)]
struct StagedItem {
    origin: PathBuf,
    temporary: PathBuf,
    destination: PathBuf,
    /// Whether the destination holds an entry that the commit replaces.
    replaces: bool,
    /// The identity that the destination must still hold at the commit.
    approved: StagedPath,
    /// The temporary name of the entry that held the destination.
    parked: Option<PathBuf>,
    /// Whether the destination holds the staged entry now.
    committed: bool,
}

/// One copy or move that either finishes completely or leaves no trace.
///
/// Every fallible step writes a temporary name beside the destination. The
/// commit renames the temporary names, which is one cheap step inside one
/// directory. A drop before the commit undoes every staged step.
///
/// An approved destination keeps its entry until the commit reaches it. The
/// commit confirms the resolved identity of that destination, parks the entry
/// under a temporary name, and only then takes the name, so the unwind puts it
/// back and a failed overwrite leaves the destination unchanged.
#[derive(Debug)]
struct StagedTransfer<'a> {
    root: &'a WorktreeRoot,
    mode: TransferMode,
    items: Vec<StagedItem>,
    settled: bool,
}

impl<'a> StagedTransfer<'a> {
    /// Creates one empty transfer.
    fn new(root: &'a WorktreeRoot, mode: TransferMode) -> Self {
        Self {
            root,
            mode,
            items: Vec::new(),
            settled: false,
        }
    }

    /// Puts one entry beside its destination under a temporary name.
    fn stage(&mut self, relocation: &Relocation) -> Result<(), MutationFailure> {
        relocation
            .origin
            .revalidate(self.root)
            .map_err(MutationFailure::from)?;
        let destination = relocation.destination.as_path();
        let temporary = parent_of(destination).join(temporary_name(destination));
        let directory = self.root.directory();
        match self.mode {
            TransferMode::Copy => {
                if let Err(primary) = copy_tree(self.root, relocation.origin.as_path(), &temporary)
                {
                    let recovery: Vec<RecoveryFailure> = remove_tree(directory, &temporary)
                        .err()
                        .map(|source| {
                            RecoveryFailure::new(
                                display_path(self.root, &temporary),
                                RecoveryAction::RemoveTemporary,
                                source,
                            )
                        })
                        .into_iter()
                        .collect();
                    return Err(MutationFailure {
                        primary,
                        indeterminate: !recovery.is_empty(),
                        recovery,
                    });
                }
            }
            TransferMode::Move => {
                directory
                    .rename(relocation.origin.as_path(), directory, &temporary)
                    .map_err(|source| {
                        MutationFailure::from(MutationError::Filesystem {
                            path: relocation.origin.display_path(self.root),
                            source,
                        })
                    })?;
            }
        }
        self.items.push(StagedItem {
            origin: relocation.origin.as_path().to_path_buf(),
            temporary,
            destination: destination.to_path_buf(),
            replaces: relocation.replaces,
            approved: relocation.destination.clone(),
            parked: None,
            committed: false,
        });
        Ok(())
    }

    /// Gives every staged entry its destination name.
    ///
    /// The commit confirms the identity of an approved destination and parks it
    /// before it takes the name. It removes the parked entries only after every
    /// destination holds its new entry.
    fn commit(mut self) -> Result<(), MutationFailure> {
        let directory = self.root.directory();
        for index in 0..self.items.len() {
            let step = (|| -> Result<(), MutationError> {
                self.items[index].approved.revalidate(self.root)?;
                if self.items[index].replaces {
                    self.items[index].parked = park(self.root, &self.items[index].destination)?;
                }
                let item = &self.items[index];
                directory
                    .rename(&item.temporary, directory, &item.destination)
                    .map_err(|source| MutationError::Filesystem {
                        path: display_path(self.root, &item.destination),
                        source,
                    })?;
                self.items[index].committed = true;
                fail_at(FailurePoint::MutationAfterRename).map_err(|source| {
                    MutationError::Filesystem {
                        path: display_path(self.root, &self.items[index].destination),
                        source,
                    }
                })?;
                Ok(())
            })();
            if let Err(primary) = step {
                let recovery = self.rollback();
                return Err(MutationFailure {
                    primary,
                    recovery,
                    indeterminate: true,
                });
            }
        }
        self.settled = true;
        let mut recovery = Vec::new();
        for item in &self.items {
            if let Some(parked) = &item.parked {
                let removed = fail_at(FailurePoint::MutationCleanup)
                    .and_then(|()| remove_tree(directory, parked));
                if let Err(source) = removed {
                    recovery.push(RecoveryFailure::new(
                        display_path(self.root, parked),
                        RecoveryAction::RemoveParked,
                        source,
                    ));
                }
            }
        }
        if recovery.is_empty() {
            Ok(())
        } else {
            Err(MutationFailure {
                primary: MutationError::Filesystem {
                    path: self.root.as_path().to_path_buf(),
                    source: io::Error::other("cleanup after mutation commit failed"),
                },
                recovery,
                indeterminate: true,
            })
        }
    }

    fn rollback(&mut self) -> Vec<RecoveryFailure> {
        let directory = self.root.directory();
        let mut failures = Vec::new();
        for item in self.items.iter().rev() {
            let restored = match (self.mode, item.committed) {
                (TransferMode::Copy, false) => remove_tree(directory, &item.temporary),
                (TransferMode::Move, false) => {
                    directory.rename(&item.temporary, directory, &item.origin)
                }
                (TransferMode::Copy, true) => remove_tree(directory, &item.destination),
                (TransferMode::Move, true) => {
                    directory.rename(&item.destination, directory, &item.origin)
                }
            };
            let restored = fail_at(FailurePoint::MutationRestore).and(restored);
            if let Err(source) = restored {
                failures.push(RecoveryFailure::new(
                    display_path(self.root, &item.origin),
                    RecoveryAction::RestoreOriginal,
                    source,
                ));
            }
            if let Some(parked) = &item.parked
                && let Err(source) = directory.rename(parked, directory, &item.destination)
            {
                failures.push(RecoveryFailure::new(
                    display_path(self.root, &item.destination),
                    RecoveryAction::RestoreDestination,
                    source,
                ));
            }
        }
        self.settled = true;
        failures
    }
}

impl Drop for StagedTransfer<'_> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let _ = self.rollback();
    }
}

/// Renames the entry of one destination to a temporary name beside itself.
///
/// The parked entry keeps the complete content of the destination, so the
/// unwind can put it back. A destination that holds no entry parks nothing.
fn park(root: &WorktreeRoot, destination: &Path) -> Result<Option<PathBuf>, MutationError> {
    let parked = parent_of(destination).join(temporary_name(destination));
    let directory = root.directory();
    match directory.rename(destination, directory, &parked) {
        Ok(()) => Ok(Some(parked)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(MutationError::Filesystem {
            path: display_path(root, destination),
            source,
        }),
    }
}

/// One removal that either hides every entry or leaves every entry in place.
///
/// The rename to a temporary name is the visible removal. The commit then
/// removes the temporary names. A failed removal leaves one hidden temporary
/// entry, which the default hidden-entry policy keeps out of the tree.
#[derive(Debug)]
struct StagedDiscard<'a> {
    root: &'a WorktreeRoot,
    items: Vec<DiscardedItem>,
    settled: bool,
}

/// One removed entry that waits under a temporary name.
#[derive(Debug)]
struct DiscardedItem {
    origin: PathBuf,
    temporary: PathBuf,
}

impl<'a> StagedDiscard<'a> {
    /// Creates one empty removal.
    fn new(root: &'a WorktreeRoot) -> Self {
        Self {
            root,
            items: Vec::new(),
            settled: false,
        }
    }

    /// Renames one entry to a temporary name beside itself.
    fn stage(&mut self, path: &StagedPath) -> Result<(), MutationError> {
        // The rename below is the removal, so the entry must still be the entry
        // that the staging observed.
        path.revalidate(self.root)?;
        let origin = path.as_path();
        let temporary = parent_of(origin).join(temporary_name(origin));
        let directory = self.root.directory();
        directory
            .rename(origin, directory, &temporary)
            .map_err(|source| MutationError::Filesystem {
                path: path.display_path(self.root),
                source,
            })?;
        self.items.push(DiscardedItem {
            origin: origin.to_path_buf(),
            temporary,
        });
        Ok(())
    }

    /// Removes every renamed entry.
    fn commit(mut self) -> Result<(), MutationFailure> {
        self.settled = true;
        let directory = self.root.directory();
        let mut recovery = Vec::new();
        for item in &self.items {
            if let Err(source) = remove_tree(directory, &item.temporary) {
                recovery.push(RecoveryFailure::new(
                    display_path(self.root, &item.origin),
                    RecoveryAction::RemoveParked,
                    source,
                ));
            }
        }
        if recovery.is_empty() {
            Ok(())
        } else {
            Err(MutationFailure {
                primary: MutationError::Filesystem {
                    path: self.root.as_path().to_path_buf(),
                    source: io::Error::other("cleanup after mutation commit failed"),
                },
                recovery,
                indeterminate: true,
            })
        }
    }

    fn rollback(&mut self) -> Vec<RecoveryFailure> {
        let directory = self.root.directory();
        let mut failures = Vec::new();
        for item in self.items.iter().rev() {
            if let Err(source) = directory.rename(&item.temporary, directory, &item.origin) {
                failures.push(RecoveryFailure::new(
                    display_path(self.root, &item.origin),
                    RecoveryAction::RestoreOriginal,
                    source,
                ));
            }
        }
        self.settled = true;
        failures
    }
}

impl Drop for StagedDiscard<'_> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let _ = self.rollback();
    }
}

/// Copies one file, one symbolic link, or one complete directory.
///
/// The walk uses an explicit stack, so a deep directory never grows the call
/// stack. The entry and depth bounds stop a very large or looping tree. Every
/// step names a contained path, so the copy stays inside the root.
fn copy_tree(root: &WorktreeRoot, source: &Path, target: &Path) -> Result<(), MutationError> {
    let directory = root.directory();
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
        let metadata =
            directory
                .symlink_metadata(&from)
                .map_err(|source| MutationError::Filesystem {
                    path: display_path(root, &from),
                    source,
                })?;
        if metadata.is_symlink() {
            copy_link(root, &from, &to)?;
            continue;
        }
        if !metadata.is_dir() {
            directory
                .copy(&from, directory, &to)
                .map_err(|source| MutationError::Filesystem {
                    path: display_path(root, &from),
                    source,
                })?;
            continue;
        }
        directory
            .create_dir(&to)
            .map_err(|source| MutationError::Filesystem {
                path: display_path(root, &to),
                source,
            })?;
        let reader = directory
            .read_dir(&from)
            .map_err(|source| MutationError::Filesystem {
                path: display_path(root, &from),
                source,
            })?;
        for entry in reader {
            let entry = entry.map_err(|source| MutationError::Filesystem {
                path: display_path(root, &from),
                source,
            })?;
            let name = entry.file_name();
            stack.push((from.join(&name), to.join(&name), depth + 1));
        }
    }
    Ok(())
}

/// Recreates one symbolic link at the target path.
///
/// The copy reproduces the exact contents of the link, including an absolute
/// target, because a contained link may name its target either way. It grants
/// no new reach: every reader resolves a link through the capability, which
/// refuses one that leaves the root.
#[cfg(not(windows))]
fn copy_link(root: &WorktreeRoot, source: &Path, target: &Path) -> Result<(), MutationError> {
    let directory = root.directory();
    let link = directory
        .read_link_contents(source)
        .map_err(|error| MutationError::Filesystem {
            path: display_path(root, source),
            source: error,
        })?;
    directory
        .symlink_contents(link, target)
        .map_err(|error| MutationError::Filesystem {
            path: display_path(root, target),
            source: error,
        })
}

/// Reports that this platform offers no symbolic link support.
#[cfg(windows)]
fn copy_link(root: &WorktreeRoot, source: &Path, _target: &Path) -> Result<(), MutationError> {
    Err(MutationError::UnsupportedLink {
        path: display_path(root, source),
    })
}

/// Removes one file, one symbolic link, or one complete directory.
fn remove_tree(directory: &Dir, path: &Path) -> io::Result<()> {
    let metadata = directory.symlink_metadata(path)?;
    if metadata.is_dir() {
        directory.remove_dir_all(path)
    } else {
        directory.remove_file(path)
    }
}
