//! The file operations that the editor hands to the bounded worker service.
//!
//! One [`FileRequest`] holds every value that the operation needs, so the
//! worker never reads editor state. One [`FileResult`] holds the complete
//! candidate, so the event loop applies it as one transition. See
//! `docs/responsiveness.md`.

use std::path::{Path, PathBuf};

use kvim_core::{LoadError, TextBuffer};
use kvim_settings::FileSettings;

use super::buffer::{BUFFERS_MAX, BufferId};
use super::file::{self, FileChange, FileIdentity, OpenError, SaveError};
use super::undo_file::{self, UndoRecord};

/// One file to load into a new buffer.
#[derive(Clone, Debug)]
pub struct OpenRequest {
    /// The path that the user named.
    pub path: PathBuf,
    /// The load and save policy of this editor.
    pub files: FileSettings,
}

/// One buffer to write over its file.
#[derive(Debug)]
pub struct SaveRequest {
    /// The buffer that the result belongs to.
    pub buffer: BufferId,
    /// The path that the save replaces.
    pub path: PathBuf,
    /// The complete buffer text with the line ending of the buffer.
    pub content: String,
    /// The file state that Kvim observed at load time or after the last save.
    pub expected: Option<FileIdentity>,
    /// The buffer copy that produces the persistent undo file.
    pub snapshot: TextBuffer,
    /// The load and save policy of this editor.
    pub files: FileSettings,
}

/// The largest number of buffers that one reload request checks.
///
/// One request checks every loaded buffer, so the buffer list is the bound.
pub const RELOAD_TARGETS_MAX: usize = BUFFERS_MAX;

/// What one reload target asks the worker to do with its file.
///
/// The event loop owns the unsaved state of a buffer, so it selects the trigger
/// before the request leaves. A buffer with unsaved changes can therefore never
/// reach the reading variants at all. See `docs/files.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReloadTrigger {
    /// Read the file only when it differs from this recorded identity.
    ///
    /// The buffer holds no unsaved change, so the new text may replace it.
    Refresh(Option<FileIdentity>),
    /// Compare the file with this recorded identity and read no content.
    ///
    /// The buffer holds unsaved changes, which no reload may replace, so the
    /// editor reports the external change instead.
    Compare(Option<FileIdentity>),
    /// Read the file whatever its identity holds, because the user asked.
    Read,
}

/// One buffer that a reload request checks against its file.
#[derive(Clone, Debug)]
pub struct ReloadTarget {
    /// The buffer that the outcome belongs to.
    pub buffer: BufferId,
    /// The path that the buffer holds.
    pub path: PathBuf,
    /// What the worker does with the file.
    pub trigger: ReloadTrigger,
}

/// The buffers to check against their files.
#[derive(Debug)]
pub struct ReloadRequest {
    /// The buffers to check, bounded by [`RELOAD_TARGETS_MAX`].
    pub targets: Vec<ReloadTarget>,
    /// The load and save policy of this editor.
    pub files: FileSettings,
}

/// One blocking file operation.
#[derive(Debug)]
pub enum FileRequest {
    /// Load one file into a new buffer.
    Open(OpenRequest),
    /// Write one buffer over its file.
    Save(SaveRequest),
    /// Check loaded buffers against their files and read the changed ones.
    Reload(ReloadRequest),
}

/// One buffer that the worker loaded.
#[derive(Debug)]
pub struct OpenedFile {
    /// The absolute path of the file, with every symlink resolved.
    pub path: PathBuf,
    /// The loaded text with its restored undo history.
    pub text: TextBuffer,
    /// The observed file state, or `None` while the path holds no file yet.
    pub identity: Option<FileIdentity>,
}

/// What one reload target found on disk.
#[derive(Debug)]
pub enum ReloadOutcome {
    /// The file still holds the recorded state, so the buffer is current.
    Unchanged,
    /// The file changed, and the worker read its new text.
    Loaded(OpenedFile),
    /// The file changed while the buffer held unsaved changes.
    ///
    /// The worker read no content, because no reload may replace those changes.
    Conflict,
    /// The file no longer lies at the path of the buffer.
    Missing,
}

/// The completed check of one reload target.
#[derive(Debug)]
pub struct ReloadedBuffer {
    /// The buffer that the outcome belongs to.
    pub buffer: BufferId,
    /// The path that the worker checked.
    pub path: PathBuf,
    /// What the worker found, or the reason that it read no text.
    pub outcome: Result<ReloadOutcome, OpenError>,
}

/// The completed result of one file operation.
#[derive(Debug)]
pub enum FileResult {
    /// One load finished.
    Opened {
        /// The path that the user named.
        requested: PathBuf,
        /// The loaded buffer, or the reason that Kvim rejected the file.
        outcome: Result<OpenedFile, OpenError>,
    },
    /// One reload check finished, with one outcome for each target.
    Reloaded {
        /// The outcome of every checked buffer, in target order.
        buffers: Vec<ReloadedBuffer>,
    },
    /// One save finished.
    Saved {
        /// The buffer that the save belongs to.
        buffer: BufferId,
        /// The path that the user named.
        requested: PathBuf,
        /// The new file state, or the reason that the save changed nothing.
        outcome: Result<SavedBuffer, SaveError>,
    },
}

/// The new file state of one saved buffer.
#[derive(Debug)]
pub struct SavedBuffer {
    /// The absolute path that the save replaced.
    pub path: PathBuf,
    /// The observed state of the new file.
    pub identity: FileIdentity,
    /// The number of bytes that the save wrote.
    pub bytes: u64,
}

impl FileRequest {
    /// Runs the operation and returns its complete typed result.
    ///
    /// The call blocks. Run it on the bounded worker service only.
    #[must_use]
    pub fn run(self) -> FileResult {
        match self {
            Self::Open(request) => FileResult::Opened {
                outcome: open(&request),
                requested: request.path,
            },
            Self::Save(request) => FileResult::Saved {
                buffer: request.buffer,
                outcome: write(&request),
                requested: request.path,
            },
            Self::Reload(request) => FileResult::Reloaded {
                buffers: reload(&request),
            },
        }
    }
}

/// Checks every target of one reload request against its file.
fn reload(request: &ReloadRequest) -> Vec<ReloadedBuffer> {
    debug_assert!(
        request.targets.len() <= RELOAD_TARGETS_MAX,
        "the editor holds at most BUFFERS_MAX buffers, so it names at most that many targets"
    );
    request
        .targets
        .iter()
        .take(RELOAD_TARGETS_MAX)
        .map(|target| ReloadedBuffer {
            buffer: target.buffer,
            outcome: check(target, &request.files),
            path: target.path.clone(),
        })
        .collect()
}

/// Checks one buffer against its file and reads the file when it must.
///
/// The read takes the same path as an ordinary open, so a reloaded buffer holds
/// exactly what an open of that file would hold, including its restored
/// persistent undo history.
fn check(target: &ReloadTarget, files: &FileSettings) -> Result<ReloadOutcome, OpenError> {
    let current = file::identity(&target.path)?;
    if current.is_none() {
        // A file that no longer exists carries no text to read. The buffer holds
        // the only remaining copy, so it keeps its text and stays editable.
        return Ok(ReloadOutcome::Missing);
    }
    let change = match target.trigger {
        // The user asked for the read, so no comparison decides it.
        ReloadTrigger::Read => FileChange::Changed,
        ReloadTrigger::Refresh(recorded) | ReloadTrigger::Compare(recorded) => {
            FileIdentity::compare(recorded, current)
        }
    };
    match (change, target.trigger) {
        (FileChange::Unchanged, _) => Ok(ReloadOutcome::Unchanged),
        (FileChange::Missing, _) => Ok(ReloadOutcome::Missing),
        (FileChange::Changed, ReloadTrigger::Compare(_)) => Ok(ReloadOutcome::Conflict),
        (FileChange::Changed, ReloadTrigger::Refresh(_) | ReloadTrigger::Read) => {
            open(&OpenRequest {
                path: target.path.clone(),
                files: *files,
            })
            .map(ReloadOutcome::Loaded)
        }
    }
}

/// Loads one file and restores its persistent undo history.
fn open(request: &OpenRequest) -> Result<OpenedFile, OpenError> {
    let loaded = file::load(&request.path, &request.files)?;
    let text =
        TextBuffer::from_text(&loaded.text, &request.files).map_err(|error| match error {
            LoadError::TooLarge { bytes, max_bytes } => OpenError::TooLarge { bytes, max_bytes },
        })?;
    let text = restore_undo(text, &loaded.text, &loaded.path, &request.files);
    Ok(OpenedFile {
        path: loaded.path,
        text,
        identity: loaded.identity,
    })
}

/// Returns the buffer with its persistent undo history, where one is usable.
///
/// An unreadable, unsupported, or invalidated undo file keeps the plain buffer,
/// because a rejected undo file must never fail the open.
fn restore_undo(text: TextBuffer, content: &str, path: &Path, files: &FileSettings) -> TextBuffer {
    if !files.undo_file {
        return text;
    }
    let Some(undo_path) = undo_file::undo_file_path(path) else {
        return text;
    };
    undo_file::read_record(&undo_path, content)
        .and_then(|record| record.restore(content, files))
        .unwrap_or(text)
}

/// Saves one buffer and writes its persistent undo file.
fn write(request: &SaveRequest) -> Result<SavedBuffer, SaveError> {
    let saved = file::save(
        &request.path,
        &request.content,
        request.expected,
        &request.files,
    )?;
    if request.files.undo_file {
        // The undo file is an accelerator, not part of the save. A failure here
        // leaves the saved file correct.
        if let Some(undo_path) = undo_file::undo_file_path(&saved.path) {
            undo_file::write_record(
                &undo_path,
                &UndoRecord::capture(&request.snapshot),
                &request.content,
            );
        }
    }
    Ok(SavedBuffer {
        path: saved.path,
        identity: saved.identity,
        bytes: request.content.len() as u64,
    })
}
