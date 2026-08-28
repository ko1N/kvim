//! Bounded crash-recovery records for dirty file-backed buffers.
//!
//! Recovery records are independent from atomic-save temporary files and
//! persistent undo records. `docs/files.md` owns their persistence contract.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::durable::{
    DurableOutcome, FailurePoint, Indeterminate, RecoveryAction, RecoveryFailure, fail_at,
};
use super::file::{FileTarget, create_temporary, temporary_name};
use super::hash::content_hash;

/// The format version in every recovery record header.
pub const RECOVERY_RECORD_VERSION: u32 = 1;

/// The largest complete recovery record that the reader accepts.
pub const RECOVERY_RECORD_BYTES_MAX: u64 = 4 * 1024 * 1024 + 128 * 1024;

const RECOVERY_RECORD_MAGIC: [u8; 8] = *b"KVIMRECV";
const RECOVERY_DIRECTORY: &str = "kvim/recovery";
const RECOVERY_RECORD_EXTENSION: &str = "kvr";
const RECOVERY_HEADER_BYTES: u64 = 8 + 4 + 8 + 8 + 8 + 8 + 8 + 8;

/// The disk state that a recovery record expects before restoration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryBaseline {
    /// The target did not exist when the recovered buffer began.
    Missing,
    /// The target held this exact saved content.
    Saved {
        /// The saved file length in bytes.
        len_bytes: u64,
        /// The saved file content hash.
        content_hash: u64,
    },
}

impl RecoveryBaseline {
    /// Captures the baseline of saved text.
    #[must_use]
    pub fn saved(content: &str) -> Self {
        Self::Saved {
            len_bytes: content.len() as u64,
            content_hash: content_hash(content.as_bytes()),
        }
    }

    /// Returns whether this baseline describes the current disk content.
    #[must_use]
    pub fn matches_disk(&self, content: Option<&str>) -> bool {
        match (self, content) {
            (Self::Missing, None) => true,
            (Self::Saved { .. }, None) | (Self::Missing, Some(_)) => false,
            (
                Self::Saved {
                    len_bytes,
                    content_hash: recorded_hash,
                },
                Some(content),
            ) => {
                *len_bytes == content.len() as u64
                    && *recorded_hash == content_hash(content.as_bytes())
            }
        }
    }

    fn encode(&self, bytes: &mut Vec<u8>) {
        match self {
            Self::Missing => bytes.extend_from_slice(&0_u64.to_le_bytes()),
            Self::Saved {
                len_bytes,
                content_hash,
            } => {
                bytes.extend_from_slice(&1_u64.to_le_bytes());
                bytes.extend_from_slice(&len_bytes.to_le_bytes());
                bytes.extend_from_slice(&content_hash.to_le_bytes());
            }
        }
    }

    fn decode(reader: &mut Reader<'_>) -> Option<Self> {
        match reader.u64()? {
            0 => Some(Self::Missing),
            1 => Some(Self::Saved {
                len_bytes: reader.u64()?,
                content_hash: reader.u64()?,
            }),
            _ => None,
        }
    }
}

/// One complete dirty-buffer checkpoint that survives an interrupted process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryRecord {
    target: PathBuf,
    baseline: RecoveryBaseline,
    revision: u64,
    text: String,
}

impl RecoveryRecord {
    /// Creates a bounded record for one absolute target.
    ///
    /// # Errors
    ///
    /// Returns [`RecoveryError::TooLarge`] when recovered text exceeds either
    /// configured bound.
    pub fn new(
        target: &FileTarget,
        baseline: RecoveryBaseline,
        revision: u64,
        text: String,
        recovery_max_bytes: u64,
        file_max_bytes: u64,
    ) -> Result<Self, RecoveryError> {
        if target.as_path().to_str().is_none() {
            return Err(RecoveryError::TargetNotUtf8);
        }
        let text_bytes = text.len() as u64;
        let max_bytes = recovery_max_bytes.min(file_max_bytes);
        if text_bytes > max_bytes {
            return Err(RecoveryError::TooLarge {
                bytes: text_bytes,
                max_bytes,
            });
        }
        Ok(Self {
            target: target.as_path().to_path_buf(),
            baseline,
            revision,
            text,
        })
    }

    /// Returns the complete canonical target path.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Returns the saved disk baseline.
    #[must_use]
    pub const fn baseline(&self) -> &RecoveryBaseline {
        &self.baseline
    }

    /// Returns the buffer revision that produced this checkpoint.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the recovered dirty text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns whether the current disk text still matches the saved baseline.
    #[must_use]
    pub fn matches_disk(&self, content: Option<&str>) -> bool {
        self.baseline.matches_disk(content)
    }

    /// Encodes this record into its versioned persistence format.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let target = self
            .target
            .to_str()
            .expect("RecoveryRecord::new accepts only UTF-8 target paths")
            .as_bytes();
        let mut bytes = Vec::with_capacity(
            RECOVERY_HEADER_BYTES as usize + target.len() + self.text.len() + 24,
        );
        bytes.extend_from_slice(&RECOVERY_RECORD_MAGIC);
        bytes.extend_from_slice(&RECOVERY_RECORD_VERSION.to_le_bytes());
        write_bytes(&mut bytes, target);
        self.baseline.encode(&mut bytes);
        bytes.extend_from_slice(&self.revision.to_le_bytes());
        write_bytes(&mut bytes, self.text.as_bytes());
        bytes.extend_from_slice(&content_hash(self.text.as_bytes()).to_le_bytes());
        bytes
    }

    /// Decodes a record only when it belongs to `target` and satisfies bounds.
    #[must_use]
    pub fn decode(
        bytes: &[u8],
        target: &FileTarget,
        recovery_max_bytes: u64,
        file_max_bytes: u64,
    ) -> Option<Self> {
        if bytes.len() as u64 > RECOVERY_RECORD_BYTES_MAX {
            return None;
        }
        let mut reader = Reader::new(bytes);
        if reader.take(RECOVERY_RECORD_MAGIC.len())? != RECOVERY_RECORD_MAGIC
            || reader.u32()? != RECOVERY_RECORD_VERSION
        {
            return None;
        }
        let recorded_target = PathBuf::from(String::from_utf8(reader.bytes()?.to_vec()).ok()?);
        if recorded_target != target.as_path() {
            return None;
        }
        let baseline = RecoveryBaseline::decode(&mut reader)?;
        let revision = reader.u64()?;
        let text_bytes = reader.bytes()?;
        let max_bytes = recovery_max_bytes.min(file_max_bytes);
        if text_bytes.len() as u64 > max_bytes {
            return None;
        }
        let recorded_hash = reader.u64()?;
        if recorded_hash != content_hash(text_bytes) || !reader.is_finished() {
            return None;
        }
        let text = String::from_utf8(text_bytes.to_vec()).ok()?;
        Some(Self {
            target: recorded_target,
            baseline,
            revision,
            text,
        })
    }
}

/// A recovery record could not be written.
#[derive(Debug, Error)]
pub enum RecoveryError {
    /// The record text exceeds the configured recovery or file bound.
    #[error("the recovery text holds {bytes} bytes; the limit is {max_bytes} bytes")]
    TooLarge {
        /// The rejected text size.
        bytes: u64,
        /// The effective configured maximum.
        max_bytes: u64,
    },
    /// The recovery record target is not UTF-8 text.
    #[error("the recovery record target is not UTF-8 text")]
    TargetNotUtf8,
    /// The record path has no parent directory.
    #[error("the recovery record path holds no directory")]
    NoDirectory,
    /// The temporary recovery file could not be written or synchronized.
    #[error("the recovery record could not be written")]
    Write(#[source] io::Error),
    /// The temporary recovery file could not replace the current record.
    #[error("the recovery record could not replace its prior record")]
    Replace(#[source] io::Error),
    /// The recovery directory could not be synchronized.
    #[error("the recovery directory could not be synchronized")]
    SyncDirectory(#[source] io::Error),
}

/// Returns the recovery path under an injected state directory.
#[must_use]
pub fn recovery_record_path(state: &Path, target: &FileTarget) -> PathBuf {
    let name = format!(
        "{:016x}.{RECOVERY_RECORD_EXTENSION}",
        content_hash(target.as_path().as_os_str().as_encoded_bytes())
    );
    state.join(RECOVERY_DIRECTORY).join(name)
}

/// Reads and validates one recovery record.
///
/// A malformed, interrupted, oversized, or mismatched record is ignored.
#[must_use]
pub fn read_recovery_record(
    path: &Path,
    target: &FileTarget,
    recovery_max_bytes: u64,
    file_max_bytes: u64,
) -> Option<RecoveryRecord> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > RECOVERY_RECORD_BYTES_MAX {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    RecoveryRecord::decode(&bytes, target, recovery_max_bytes, file_max_bytes)
}

/// Durably replaces one recovery record through an injected record path.
pub fn write_recovery_record(
    path: &Path,
    record: &RecoveryRecord,
) -> DurableOutcome<(), RecoveryError> {
    let bytes = record.encode();
    if bytes.len() as u64 > RECOVERY_RECORD_BYTES_MAX {
        return DurableOutcome::Unchanged(RecoveryError::TooLarge {
            bytes: bytes.len() as u64,
            max_bytes: RECOVERY_RECORD_BYTES_MAX,
        });
    }
    match write_recovery_record_inner(path, &bytes) {
        Ok(()) => DurableOutcome::Committed(()),
        Err(RecoveryWriteFailure::Unchanged(error)) => DurableOutcome::Unchanged(error),
        Err(RecoveryWriteFailure::Indeterminate {
            primary,
            recovery,
            affected,
        }) => DurableOutcome::Indeterminate(Indeterminate::from_operation(
            primary, recovery, affected,
        )),
    }
}

enum RecoveryWriteFailure {
    Unchanged(RecoveryError),
    Indeterminate {
        primary: RecoveryError,
        recovery: Vec<RecoveryFailure>,
        affected: Vec<PathBuf>,
    },
}

fn indeterminate(
    primary: RecoveryError,
    recovery: Vec<RecoveryFailure>,
    affected: Vec<PathBuf>,
) -> RecoveryWriteFailure {
    RecoveryWriteFailure::Indeterminate {
        primary,
        recovery,
        affected,
    }
}

fn write_recovery_record_inner(path: &Path, bytes: &[u8]) -> Result<(), RecoveryWriteFailure> {
    let directory_path = path
        .parent()
        .ok_or(RecoveryWriteFailure::Unchanged(RecoveryError::NoDirectory))?;
    let name = path
        .file_name()
        .ok_or(RecoveryWriteFailure::Unchanged(RecoveryError::NoDirectory))?;
    // `create_dir_all` can create a prefix before it reports an error. It can
    // also create this directory before a later pre-commit failure. Neither
    // case can prove that durable state remained unchanged.
    if let Err(source) = fs::create_dir_all(directory_path) {
        return Err(indeterminate(
            RecoveryError::Write(source),
            Vec::new(),
            vec![directory_path.to_path_buf()],
        ));
    }
    let directory =
        cap_std::fs::Dir::open_ambient_dir(directory_path, cap_std::ambient_authority()).map_err(
            |source| {
                indeterminate(
                    RecoveryError::Write(source),
                    Vec::new(),
                    vec![directory_path.to_path_buf()],
                )
            },
        )?;
    let temporary = temporary_name(Path::new(name));
    let temporary_path = Path::new(&temporary);
    let temporary_absolute = directory_path.join(&temporary);
    let mut file = create_temporary(&directory, temporary_path).map_err(|error| {
        indeterminate(
            match error {
                super::file::SaveError::Write(source) => RecoveryError::Write(source),
                super::file::SaveError::Replace(source) => RecoveryError::Replace(source),
                super::file::SaveError::NoDirectory
                | super::file::SaveError::Conflict
                | super::file::SaveError::Confinement(_) => {
                    unreachable!("recovery temporary creation only returns a write error")
                }
            },
            Vec::new(),
            vec![directory_path.to_path_buf()],
        )
    })?;
    if let Err(source) = fail_at(FailurePoint::RecoveryWrite)
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let recovery = fail_at(FailurePoint::RecoveryCleanup)
            .and_then(|()| directory.remove_file(temporary_path))
            .err()
            .map_or_else(Vec::new, |source| {
                vec![RecoveryFailure::new(
                    temporary_absolute.clone(),
                    RecoveryAction::RemoveTemporary,
                    source,
                )]
            });
        let primary = RecoveryError::Write(source);
        return if recovery.is_empty() {
            Err(indeterminate(
                primary,
                recovery,
                vec![directory_path.to_path_buf()],
            ))
        } else {
            Err(indeterminate(
                primary,
                recovery,
                vec![directory_path.to_path_buf(), temporary_absolute],
            ))
        };
    }
    drop(file);
    if let Err(source) = fail_at(FailurePoint::RecoveryRename)
        .and_then(|()| directory.rename(&temporary, &directory, name))
    {
        let recovery = fail_at(FailurePoint::RecoveryCleanup)
            .and_then(|()| directory.remove_file(temporary_path))
            .err()
            .map_or_else(Vec::new, |source| {
                vec![RecoveryFailure::new(
                    temporary_absolute.clone(),
                    RecoveryAction::RemoveTemporary,
                    source,
                )]
            });
        let primary = RecoveryError::Replace(source);
        return if recovery.is_empty() {
            Err(indeterminate(
                primary,
                recovery,
                vec![directory_path.to_path_buf()],
            ))
        } else {
            Err(indeterminate(
                primary,
                recovery,
                vec![directory_path.to_path_buf(), temporary_absolute],
            ))
        };
    }
    sync_directory(directory_path).map_err(|source| {
        indeterminate(
            RecoveryError::SyncDirectory(source),
            Vec::new(),
            vec![path.to_path_buf(), directory_path.to_path_buf()],
        )
    })
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn write_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
    bytes.extend_from_slice(value);
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    const fn is_finished(&self) -> bool {
        self.at == self.bytes.len()
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let value = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(value)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u64()?).ok()?;
        if len as u64 > RECOVERY_RECORD_BYTES_MAX {
            return None;
        }
        self.take(len)
    }
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
