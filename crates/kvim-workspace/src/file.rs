//! File loading and the staged atomic save.
//!
//! Every function of this file blocks. The caller runs each one on the bounded
//! worker service, never on the terminal event loop. See
//! `docs/responsiveness.md`.
//!
//! The save writes a temporary file beside the target, flushes it, and renames
//! it over the target, so a reader never observes a partial file. A failure at
//! any step leaves the original file and the dirty buffer unchanged. See
//! `docs/files.md`.

use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use thiserror::Error;

use kvim_core::{FinalLineEnding, LineEnding, TextBuffer};
use kvim_path::{
    ResolvedTargetState, ResolvedWorktreePath, WorktreeConfinementError, WorktreeRelativePath,
    WorktreeRoot,
};
use kvim_settings::FileSettings;

use crate::durable::{
    DurableOutcome, FailurePoint, Indeterminate, RecoveryAction, RecoveryFailure, fail_at,
};

/// The counter that keeps two temporary file names of one process apart.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The observed state of one file.
///
/// kvim records the identity at load time and after every successful save. The
/// save compares the recorded identity with the current identity before it
/// replaces the file, so an external change becomes a typed conflict instead of
/// a silent overwrite.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    /// The file size, in bytes.
    pub len_bytes: u64,
    /// The modification time, or `None` when the platform reports none.
    pub modified: Option<SystemTime>,
}

impl FileIdentity {
    /// Reads the identity from file metadata.
    #[must_use]
    pub fn from_metadata(metadata: &cap_std::fs::Metadata) -> Self {
        Self {
            len_bytes: metadata.len(),
            modified: metadata.modified().ok().map(|time| time.into_std()),
        }
    }

    /// Compares the recorded state of one file with its current state.
    ///
    /// The save asks this question before it overwrites a file, and the reload
    /// asks it before it replaces a buffer, so one rule answers both
    /// directions. kvim reads no file content for the comparison, so the check
    /// stays cheap for a large file. See `docs/files.md`.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_workspace::{FileChange, FileIdentity};
    ///
    /// let recorded = FileIdentity { len_bytes: 12, modified: None };
    /// let grown = FileIdentity { len_bytes: 20, modified: None };
    ///
    /// assert_eq!(FileIdentity::compare(Some(recorded), Some(recorded)), FileChange::Unchanged);
    /// assert_eq!(FileIdentity::compare(Some(recorded), Some(grown)), FileChange::Changed);
    /// assert_eq!(FileIdentity::compare(Some(recorded), None), FileChange::Missing);
    /// // A path that held no file, and holds one now, changed.
    /// assert_eq!(FileIdentity::compare(None, Some(grown)), FileChange::Changed);
    /// ```
    #[must_use]
    pub fn compare(recorded: Option<Self>, current: Option<Self>) -> FileChange {
        match (recorded, current) {
            (Some(recorded), Some(current)) if recorded != current => FileChange::Changed,
            // A file that appeared where kvim observed none is another program's
            // file, not the one that the buffer describes.
            (None, Some(_)) => FileChange::Changed,
            (Some(_), None) => FileChange::Missing,
            (Some(_), Some(_)) | (None, None) => FileChange::Unchanged,
        }
    }
}

/// What happened to one file since kvim recorded its state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileChange {
    /// The file still holds the state that kvim recorded.
    Unchanged,
    /// Another program changed, created, or replaced the file.
    Changed,
    /// The file no longer lies at the path.
    Missing,
}

/// A rejected file open.
#[derive(Debug, Error)]
pub enum OpenError {
    /// The target did not remain confined to its worktree root.
    #[error("the file target is not confined to the worktree")]
    Confinement(#[from] WorktreeConfinementError),
    /// The path names a directory.
    #[error("the path is a directory")]
    Directory,
    /// The path names a device, a socket, or another special file.
    #[error("the path is not a regular file")]
    UnsupportedKind,
    /// The file is larger than the configured maximum file size.
    #[error("the file holds {bytes} bytes; the limit is {max_bytes} bytes")]
    TooLarge {
        /// The size of the rejected file, in bytes.
        bytes: u64,
        /// The configured maximum size, in bytes.
        max_bytes: u64,
    },
    /// The file holds bytes that are not UTF-8 text.
    #[error("the file is not UTF-8 text at byte {valid_up_to}")]
    NotUtf8 {
        /// The number of valid bytes before the rejected sequence.
        valid_up_to: usize,
    },
    /// The file could not be read.
    #[error("the file could not be read")]
    Read(#[source] io::Error),
}

/// A rejected file save.
#[derive(Debug, Error)]
pub enum SaveError {
    /// The target did not remain confined to its worktree root.
    #[error("the file target is not confined to the worktree")]
    Confinement(#[from] WorktreeConfinementError),
    /// The file changed after kvim loaded or last saved it.
    #[error("the file changed on disk; the buffer keeps every unsaved change")]
    Conflict,
    /// The target path holds no parent directory.
    #[error("the target path holds no directory")]
    NoDirectory,
    /// The temporary file or the target file could not be written.
    #[error("the file could not be written")]
    Write(#[source] io::Error),
    /// The temporary file could not replace the target file.
    #[error("the temporary file could not replace the target")]
    Replace(#[source] io::Error),
}

/// One file that the worker read.
#[derive(Debug)]
pub struct LoadedFile {
    /// The complete file text.
    pub text: String,
    /// The validated root and canonical contained target.
    pub target: FileTarget,
    /// The observed file state, or `None` while the path holds no file yet.
    pub identity: Option<FileIdentity>,
}

/// The canonical identity of one loaded or new worktree file.
///
/// Equality includes both the canonical root and the contained target. Equal
/// relative paths under different roots are different file identities.
#[derive(Clone, Debug)]
pub struct FileTarget {
    root: Arc<WorktreeRoot>,
    relative: WorktreeRelativePath,
    absolute: PathBuf,
}

impl FileTarget {
    pub(crate) fn resolved(root: Arc<WorktreeRoot>, relative: WorktreeRelativePath) -> Self {
        let absolute = root.as_path().join(relative.as_path());
        Self {
            root,
            relative,
            absolute,
        }
    }

    /// Returns the canonical worktree root of this target.
    #[must_use]
    pub fn root(&self) -> &WorktreeRoot {
        &self.root
    }

    pub(crate) fn root_handle(&self) -> Arc<WorktreeRoot> {
        Arc::clone(&self.root)
    }

    /// Returns the canonical contained path of this target.
    #[must_use]
    pub const fn relative_path(&self) -> &WorktreeRelativePath {
        &self.relative
    }

    /// Returns the canonical absolute display path of this target.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.absolute
    }

    pub(crate) fn retarget(&self, path: &Path) -> Option<Self> {
        let relative = path.strip_prefix(self.root.as_path()).ok()?;
        let relative = WorktreeRelativePath::new(relative).ok()?;
        Some(Self::resolved(Arc::clone(&self.root), relative))
    }
}

impl PartialEq for FileTarget {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root && self.relative == other.relative
    }
}

impl Eq for FileTarget {}

impl Hash for FileTarget {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.root.hash(state);
        self.relative.hash(state);
    }
}

/// Reads one file into memory.
///
/// A path that holds no file is not a failure. The result then carries an empty
/// text and no identity, so the editor opens a new file.
///
/// # Errors
///
/// Returns [`OpenError`] for a directory, a special file, an oversized file, a
/// file that is not UTF-8 text, and an unreadable file.
pub fn load(
    root: &Arc<WorktreeRoot>,
    path: &WorktreeRelativePath,
    files: &FileSettings,
) -> Result<LoadedFile, OpenError> {
    let resolved = root.resolve(path)?;
    let target = FileTarget::resolved(Arc::clone(root), resolved.path().clone());
    if resolved.state() == ResolvedTargetState::Missing {
        root.revalidate(path, &resolved)?;
        return Ok(LoadedFile {
            text: String::new(),
            target,
            identity: None,
        });
    }
    let mut file = root
        .directory()
        .open(resolved.path().as_path())
        .map_err(|error| replaced_or_open_error(error, &resolved))?;
    let metadata = file.metadata().map_err(OpenError::Read)?;
    if metadata.is_dir() {
        return Err(OpenError::Directory);
    }
    if !metadata.is_file() {
        return Err(OpenError::UnsupportedKind);
    }
    // Reject the size before the read, so an oversized file never enters memory.
    if metadata.len() > files.max_file_bytes {
        return Err(OpenError::TooLarge {
            bytes: metadata.len(),
            max_bytes: files.max_file_bytes,
        });
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(OpenError::Read)?;
    // The file can grow between the metadata read and the content read.
    let read_bytes = bytes.len() as u64;
    if read_bytes > files.max_file_bytes {
        return Err(OpenError::TooLarge {
            bytes: read_bytes,
            max_bytes: files.max_file_bytes,
        });
    }
    let identity = FileIdentity::from_metadata(&metadata);
    let descriptor_identity =
        FileIdentity::from_metadata(&file.metadata().map_err(OpenError::Read)?);
    if descriptor_identity != identity {
        return Err(OpenError::Confinement(WorktreeConfinementError::Replaced));
    }
    root.revalidate(path, &resolved)?;
    let path_metadata = root
        .directory()
        .metadata(resolved.path().as_path())
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                OpenError::Confinement(WorktreeConfinementError::Replaced)
            } else {
                OpenError::Read(error)
            }
        })?;
    if FileIdentity::from_metadata(&path_metadata) != descriptor_identity {
        return Err(OpenError::Confinement(WorktreeConfinementError::Replaced));
    }
    let text = String::from_utf8(bytes).map_err(|error| OpenError::NotUtf8 {
        valid_up_to: error.utf8_error().valid_up_to(),
    })?;
    Ok(LoadedFile {
        text,
        target,
        identity: Some(identity),
    })
}

/// Reads the current state of one file without reading its content.
///
/// A path that holds no file is not a failure. The result is then `None`, which
/// [`FileIdentity::compare`] reads as a missing file.
///
/// The call blocks. Run it on the bounded worker service only.
///
/// # Errors
///
/// Returns [`OpenError::Read`] when the metadata read failed for another reason
/// than a missing file.
pub fn identity(target: &FileTarget) -> Result<Option<FileIdentity>, OpenError> {
    let resolved = validate_stored_target(target)?;
    match target
        .root()
        .directory()
        .metadata(resolved.path().as_path())
    {
        Ok(metadata) => Ok(Some(FileIdentity::from_metadata(&metadata))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OpenError::Read(error)),
    }
}

/// The outcome of one successful save.
#[derive(Debug)]
pub struct SavedFile {
    /// The canonical contained target that the save replaced.
    pub target: FileTarget,
    /// The observed state of the new file.
    pub identity: FileIdentity,
}

/// Replaces one file with the buffer content.
///
/// The `expected` identity is the state that kvim observed at load time or
/// after the last save. A different current state is a conflict, so the save
/// changes nothing.
///
/// # Errors
///
/// Returns [`SaveError::Conflict`] for an external change, and the write or
/// replace failure of the staged replacement. Every failure leaves the original
/// file unchanged.
pub fn save(
    target: &FileTarget,
    content: &str,
    expected: Option<FileIdentity>,
    files: &FileSettings,
) -> DurableOutcome<SavedFile, SaveError> {
    match save_inner(target, content, expected, files) {
        Ok(saved) => DurableOutcome::Committed(saved),
        Err(SaveFailure::Unchanged(error)) => DurableOutcome::Unchanged(error),
        Err(SaveFailure::Indeterminate { primary, recovery }) => DurableOutcome::Indeterminate(
            Indeterminate::from_operation(primary, recovery, vec![target.as_path().to_path_buf()]),
        ),
    }
}

enum SaveFailure {
    Unchanged(SaveError),
    Indeterminate {
        primary: SaveError,
        recovery: Vec<RecoveryFailure>,
    },
}

impl From<SaveError> for SaveFailure {
    fn from(error: SaveError) -> Self {
        Self::Unchanged(error)
    }
}

fn save_inner(
    target: &FileTarget,
    content: &str,
    expected: Option<FileIdentity>,
    files: &FileSettings,
) -> Result<SavedFile, SaveFailure> {
    let resolved = validate_stored_target(target).map_err(SaveError::from)?;
    let parent_path = resolved
        .path()
        .as_path()
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = resolved
        .path()
        .as_path()
        .file_name()
        .ok_or(SaveError::NoDirectory)?;
    let parent = target
        .root()
        .directory()
        .open_dir(parent_path)
        .map_err(SaveError::Write)?;
    let existing = match parent.metadata(name) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(SaveError::Write(error).into()),
    };
    require_expected_identity(expected, existing.as_ref().map(FileIdentity::from_metadata))?;

    if files.atomic_save {
        write_staged(
            target,
            &resolved,
            &parent,
            name,
            content,
            existing.as_ref(),
            expected,
        )?;
    } else {
        target
            .root()
            .revalidate(target.relative_path(), &resolved)
            .map_err(SaveError::from)?;
        require_expected_identity(
            expected,
            current_identity(&parent, name).map_err(SaveError::Write)?,
        )?;
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        let mut file = parent.open_with(name, &options).map_err(SaveError::Write)?;
        let bytes = content.as_bytes();
        let split = bytes.len().min(1);
        file.write_all(&bytes[..split])
            .map_err(|source| SaveFailure::Indeterminate {
                primary: SaveError::Write(source),
                recovery: Vec::new(),
            })?;
        if let Err(source) = fail_at(FailurePoint::SaveDirectPartial) {
            return Err(SaveFailure::Indeterminate {
                primary: SaveError::Write(source),
                recovery: Vec::new(),
            });
        }
        file.write_all(&bytes[split..])
            .map_err(|source| SaveFailure::Indeterminate {
                primary: SaveError::Write(source),
                recovery: Vec::new(),
            })?;
        fail_at(FailurePoint::SaveDirectSync)
            .and_then(|()| file.sync_all())
            .map_err(|source| SaveFailure::Indeterminate {
                primary: SaveError::Write(source),
                recovery: Vec::new(),
            })?;
    }
    if let Err(source) = fail_at(FailurePoint::SaveAfterRename) {
        return Err(SaveFailure::Indeterminate {
            primary: SaveError::Write(source),
            recovery: Vec::new(),
        });
    }
    let metadata = parent
        .metadata(name)
        .map_err(|source| SaveFailure::Indeterminate {
            primary: SaveError::Write(source),
            recovery: Vec::new(),
        })?;
    Ok(SavedFile {
        target: target.clone(),
        identity: FileIdentity::from_metadata(&metadata),
    })
}

/// Writes the content beside the target and renames it over the target.
fn write_staged(
    target: &FileTarget,
    resolved: &ResolvedWorktreePath,
    directory: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
    content: &str,
    existing: Option<&cap_std::fs::Metadata>,
    expected: Option<FileIdentity>,
) -> Result<(), SaveFailure> {
    let temporary = temporary_name(Path::new(name));
    let temporary_path = Path::new(&temporary);
    let mut file = create_temporary(directory, temporary_path).map_err(SaveFailure::from)?;
    let prepared = write_temporary(&mut file, content).and_then(|()| apply_mode(&file, existing));
    drop(file);
    if let Err(error) = prepared {
        let recovery = remove_temporary(directory, temporary_path, target);
        return if recovery.is_empty() {
            Err(SaveFailure::Unchanged(error))
        } else {
            Err(SaveFailure::Indeterminate {
                primary: error,
                recovery,
            })
        };
    }
    if let Err(error) = target.root().revalidate(target.relative_path(), resolved) {
        let primary = SaveError::Confinement(error);
        let recovery = remove_temporary(directory, temporary_path, target);
        return if recovery.is_empty() {
            Err(SaveFailure::Unchanged(primary))
        } else {
            Err(SaveFailure::Indeterminate { primary, recovery })
        };
    }
    if let Err(error) = current_identity(directory, name)
        .map_err(SaveError::Write)
        .and_then(|current| require_expected_identity(expected, current))
    {
        let primary = error;
        let recovery = remove_temporary(directory, temporary_path, target);
        return if recovery.is_empty() {
            Err(SaveFailure::Unchanged(primary))
        } else {
            Err(SaveFailure::Indeterminate { primary, recovery })
        };
    }
    if let Err(error) = directory.rename(&temporary, directory, name) {
        let primary = SaveError::Replace(error);
        let recovery = remove_temporary(directory, temporary_path, target);
        return if recovery.is_empty() {
            Err(SaveFailure::Unchanged(primary))
        } else {
            Err(SaveFailure::Indeterminate { primary, recovery })
        };
    }
    Ok(())
}

fn remove_temporary(
    directory: &cap_std::fs::Dir,
    temporary: &Path,
    target: &FileTarget,
) -> Vec<RecoveryFailure> {
    match directory.remove_file(temporary) {
        Ok(()) => Vec::new(),
        Err(source) => vec![RecoveryFailure::new(
            target.as_path().to_path_buf(),
            RecoveryAction::RemoveTemporary,
            source,
        )],
    }
}

/// Creates one owned temporary file without following or replacing an entry.
pub(super) fn create_temporary(
    directory: &cap_std::fs::Dir,
    temporary: &Path,
) -> Result<cap_std::fs::File, SaveError> {
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    directory
        .open_with(temporary, &options)
        .map_err(SaveError::Write)
}

/// Writes and flushes the temporary file with the mode of the target file.
fn write_temporary(file: &mut cap_std::fs::File, content: &str) -> Result<(), SaveError> {
    file.write_all(content.as_bytes())
        .map_err(SaveError::Write)?;
    // The rename must publish complete content, so the bytes reach the device
    // before the rename runs.
    file.sync_all().map_err(SaveError::Write)
}

/// Copies the permissions of the replaced file onto the temporary file.
#[cfg(unix)]
fn apply_mode(
    file: &cap_std::fs::File,
    existing: Option<&cap_std::fs::Metadata>,
) -> Result<(), SaveError> {
    let Some(metadata) = existing else {
        return Ok(());
    };
    file.set_permissions(metadata.permissions())
        .map_err(SaveError::Write)
}

/// Keeps the default permissions on a platform without a file mode.
#[cfg(not(unix))]
fn apply_mode(
    _file: &cap_std::fs::File,
    _existing: Option<&cap_std::fs::Metadata>,
) -> Result<(), SaveError> {
    Ok(())
}

/// Returns the temporary file name for one target.
///
/// The name stays inside the target directory, so the rename never crosses a
/// filesystem boundary. The process identifier and the counter keep two saves
/// and two staged workspace mutations apart. The name starts with a full stop,
/// so the default hidden-entry policy of the file tree keeps it out of view.
pub(super) fn temporary_name(target: &Path) -> String {
    let name = target.file_name().map_or_else(
        || "buffer".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(".{name}.kvim-{}-{counter}.tmp", std::process::id())
}

fn validate_stored_target(
    target: &FileTarget,
) -> Result<ResolvedWorktreePath, WorktreeConfinementError> {
    let resolved = target.root().resolve(target.relative_path())?;
    if resolved.path() != target.relative_path() || resolved.followed_link() {
        return Err(WorktreeConfinementError::Replaced);
    }
    Ok(resolved)
}

fn current_identity(
    directory: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
) -> Result<Option<FileIdentity>, io::Error> {
    match directory.metadata(name) {
        Ok(metadata) => Ok(Some(FileIdentity::from_metadata(&metadata))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn require_expected_identity(
    expected: Option<FileIdentity>,
    current: Option<FileIdentity>,
) -> Result<(), SaveError> {
    match FileIdentity::compare(expected, current) {
        // A file that another program changed, or that appeared after kvim
        // opened a path without a file, must not be overwritten.
        FileChange::Changed => Err(SaveError::Conflict),
        // A file that another program removed carries no content to lose.
        FileChange::Unchanged | FileChange::Missing => Ok(()),
    }
}

fn replaced_or_open_error(error: io::Error, resolved: &ResolvedWorktreePath) -> OpenError {
    if resolved.state() == ResolvedTargetState::Existing && error.kind() == io::ErrorKind::NotFound
    {
        OpenError::Confinement(WorktreeConfinementError::Replaced)
    } else {
        OpenError::Read(error)
    }
}

/// Returns the file content of one buffer.
///
/// A buffer that loaded carriage return and line feed keeps that terminator for
/// every line that the editor wrote with a line feed alone. A file that ended
/// without a line ending receives none, because the buffer terminates its last
/// line for editing alone. A file that ended with one keeps exactly one. See
/// `docs/text-model.md`.
#[must_use]
pub fn render_content(buffer: &TextBuffer) -> String {
    let mut content = with_line_endings(buffer);
    let ending = buffer.line_ending().as_str();
    if buffer.final_line_ending() == FinalLineEnding::Absent && content.ends_with(ending) {
        content.truncate(content.len() - ending.len());
    }
    content
}

/// Returns the complete buffer text with the line ending of the buffer.
fn with_line_endings(buffer: &TextBuffer) -> String {
    let text = buffer.to_string();
    match buffer.line_ending() {
        LineEnding::Lf => text,
        LineEnding::Crlf => {
            let mut output = String::with_capacity(text.len());
            let mut previous = '\0';
            for value in text.chars() {
                if value == '\n' && previous != '\r' {
                    output.push('\r');
                }
                output.push(value);
                previous = value;
            }
            output
        }
    }
}
