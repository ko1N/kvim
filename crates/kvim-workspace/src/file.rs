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

use std::fs::{self, File, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use thiserror::Error;

use kvim_core::{FinalLineEnding, LineEnding, TextBuffer};
use kvim_settings::FileSettings;

/// The counter that keeps two temporary file names of one process apart.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The observed state of one file.
///
/// Kvim records the identity at load time and after every successful save. The
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
    pub fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            len_bytes: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

/// A rejected file open.
#[derive(Debug, Error)]
pub enum OpenError {
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
    /// The file changed after Kvim loaded or last saved it.
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
    /// The absolute path of the file, with every symlink resolved.
    pub path: PathBuf,
    /// The observed file state, or `None` while the path holds no file yet.
    pub identity: Option<FileIdentity>,
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
pub fn load(path: &Path, files: &FileSettings) -> Result<LoadedFile, OpenError> {
    let resolved = resolve(path);
    let metadata = match fs::metadata(&resolved) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedFile {
                text: String::new(),
                path: resolved,
                identity: None,
            });
        }
        Err(error) => return Err(OpenError::Read(error)),
    };
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

    let bytes = fs::read(&resolved).map_err(OpenError::Read)?;
    // The file can grow between the metadata read and the content read.
    let read_bytes = bytes.len() as u64;
    if read_bytes > files.max_file_bytes {
        return Err(OpenError::TooLarge {
            bytes: read_bytes,
            max_bytes: files.max_file_bytes,
        });
    }
    let text = String::from_utf8(bytes).map_err(|error| OpenError::NotUtf8 {
        valid_up_to: error.utf8_error().valid_up_to(),
    })?;
    Ok(LoadedFile {
        text,
        path: resolved,
        identity: Some(FileIdentity::from_metadata(&metadata)),
    })
}

/// The outcome of one successful save.
#[derive(Debug)]
pub struct SavedFile {
    /// The absolute path that the save replaced.
    pub path: PathBuf,
    /// The observed state of the new file.
    pub identity: FileIdentity,
}

/// Replaces one file with the buffer content.
///
/// The `expected` identity is the state that Kvim observed at load time or
/// after the last save. A different current state is a conflict, so the save
/// changes nothing.
///
/// # Errors
///
/// Returns [`SaveError::Conflict`] for an external change, and the write or
/// replace failure of the staged replacement. Every failure leaves the original
/// file unchanged.
pub fn save(
    path: &Path,
    content: &str,
    expected: Option<FileIdentity>,
    files: &FileSettings,
) -> Result<SavedFile, SaveError> {
    let target = resolve(path);
    let existing = match fs::metadata(&target) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(SaveError::Write(error)),
    };
    let current = existing.as_ref().map(FileIdentity::from_metadata);
    match (expected, current) {
        // A file that another program changed, or that appeared after Kvim
        // opened a path without a file, must not be overwritten.
        (Some(expected), Some(current)) if expected != current => {
            return Err(SaveError::Conflict);
        }
        (None, Some(_)) => return Err(SaveError::Conflict),
        // A file that another program removed carries no content to lose.
        _ => {}
    }

    if files.atomic_save {
        write_staged(&target, content, existing.as_ref())?;
    } else {
        fs::write(&target, content).map_err(SaveError::Write)?;
    }
    let metadata = fs::metadata(&target).map_err(SaveError::Write)?;
    Ok(SavedFile {
        path: target,
        identity: FileIdentity::from_metadata(&metadata),
    })
}

/// Writes the content beside the target and renames it over the target.
fn write_staged(
    target: &Path,
    content: &str,
    existing: Option<&Metadata>,
) -> Result<(), SaveError> {
    let directory = target.parent().ok_or(SaveError::NoDirectory)?;
    let temporary = directory.join(temporary_name(target));
    if let Err(error) = write_temporary(&temporary, content, existing) {
        // A partial temporary file must never stay behind.
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, target) {
        let _ = fs::remove_file(&temporary);
        return Err(SaveError::Replace(error));
    }
    Ok(())
}

/// Writes and flushes the temporary file with the mode of the target file.
fn write_temporary(
    temporary: &Path,
    content: &str,
    existing: Option<&Metadata>,
) -> Result<(), SaveError> {
    let mut file = File::create(temporary).map_err(SaveError::Write)?;
    file.write_all(content.as_bytes())
        .map_err(SaveError::Write)?;
    // The rename must publish complete content, so the bytes reach the device
    // before the rename runs.
    file.sync_all().map_err(SaveError::Write)?;
    drop(file);
    apply_mode(temporary, existing)
}

/// Copies the permissions of the replaced file onto the temporary file.
#[cfg(unix)]
fn apply_mode(temporary: &Path, existing: Option<&Metadata>) -> Result<(), SaveError> {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    let Some(metadata) = existing else {
        return Ok(());
    };
    let mode = metadata.permissions().mode();
    fs::set_permissions(temporary, Permissions::from_mode(mode)).map_err(SaveError::Write)
}

/// Keeps the default permissions on a platform without a file mode.
#[cfg(not(unix))]
fn apply_mode(_temporary: &Path, _existing: Option<&Metadata>) -> Result<(), SaveError> {
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

/// Returns the absolute path of one target with every symlink resolved.
///
/// The save replaces the symlink target, not the symlink, so a link keeps
/// pointing at the file that the user edits.
fn resolve(path: &Path) -> PathBuf {
    fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
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
