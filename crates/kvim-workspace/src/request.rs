//! The file operations that the editor hands to the bounded worker service.
//!
//! One [`FileRequest`] holds every value that the operation needs, so the
//! worker never reads editor state. One [`FileResult`] holds the complete
//! candidate, so the event loop applies it as one transition. See
//! `docs/responsiveness.md`.

use std::path::{Path, PathBuf};

use kvim_core::{LoadError, TextBuffer};
use kvim_settings::FileSettings;

use super::buffer::BufferId;
use super::file::{self, FileIdentity, OpenError, SaveError};
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

/// One blocking file operation.
#[derive(Debug)]
pub enum FileRequest {
    /// Load one file into a new buffer.
    Open(OpenRequest),
    /// Write one buffer over its file.
    Save(SaveRequest),
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
