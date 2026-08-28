//! The persistent undo file.
//!
//! kvim writes one undo file for each saved buffer, so undo history survives a
//! restart. The file records the text states below the saved state as a base
//! text and a chain of forward changes. Loading replays that chain, so the
//! restored history uses the same transactions as a live editing session.
//!
//! The format holds no redo history above the saved state, because kvim writes
//! the file at save time and the saved state is then the newest state.
//!
//! `docs/files.md` owns the location, the version field, and the invalidation
//! rule. Keep both in agreement.

use std::fs;
use std::path::{Path, PathBuf};

use kvim_core::{CharRange, EditTransaction, FinalLineEnding, LineEnding, TextBuffer, TextChange};

use super::file::{FileTarget, render_content};

/// The first bytes of every undo file.
const UNDO_FILE_MAGIC: [u8; 8] = *b"KVIMUNDO";

/// The format version in the undo file header.
///
/// A file with another version is not readable, so kvim ignores it.
pub const UNDO_FILE_VERSION: u32 = 1;

/// The largest number of undo steps that one undo file keeps.
///
/// Each step costs one complete text comparison when kvim writes the file, so
/// the bound also bounds the save cost. The remaining steps stay in memory for
/// the running session.
pub const UNDO_FILE_STEPS_MAX: usize = 64;

/// The largest amount of replacement text that one undo file keeps.
pub const UNDO_FILE_CHANGE_BYTES_MAX: usize = 1024 * 1024;

/// The largest undo file that kvim reads or writes, in bytes.
///
/// The value holds one base text of the maximum file size and the bounded
/// change chain above it.
pub const UNDO_FILE_BYTES_MAX: u64 = 8 * 1024 * 1024;

/// The directory name that holds every undo file.
const UNDO_DIRECTORY: &str = "kvim/undo";

/// The file extension of one undo file.
const UNDO_FILE_EXTENSION: &str = "kvu";

/// One recorded transaction of the undo chain.
///
/// The change replaces `removed_chars` characters at `start` with `inserted`.
/// The positions count characters, so a replay can never split a character.
#[derive(Clone, Debug, Eq, PartialEq)]
struct UndoStep {
    cursor_before: usize,
    start: usize,
    removed_chars: usize,
    inserted: String,
}

/// The undo history of one buffer in a form that survives a restart.
///
/// The base text is the oldest state that the record keeps. The steps lead from
/// that base text to the saved file content, in application order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UndoRecord {
    base: String,
    steps: Vec<UndoStep>,
}

impl UndoRecord {
    /// Reads the undo chain below the current state of one buffer.
    ///
    /// The walk uses a copy of the buffer, so the live buffer keeps its history
    /// position. The walk stops at [`UNDO_FILE_STEPS_MAX`] steps or at
    /// [`UNDO_FILE_CHANGE_BYTES_MAX`] of replacement text.
    #[must_use]
    pub fn capture(buffer: &TextBuffer) -> Self {
        let mut history = buffer.clone();
        let mut newer = history.to_string();
        let mut steps = Vec::new();
        let mut bytes = 0;
        while steps.len() < UNDO_FILE_STEPS_MAX {
            let Some(cursor) = history.undo() else {
                break;
            };
            let older = history.to_string();
            let (start, removed_chars, inserted) = derive_change(&older, &newer);
            bytes += inserted.len();
            if bytes > UNDO_FILE_CHANGE_BYTES_MAX {
                break;
            }
            steps.push(UndoStep {
                cursor_before: cursor.get(),
                start,
                removed_chars,
                inserted,
            });
            newer = older;
        }
        steps.reverse();
        Self { base: newer, steps }
    }

    /// Reports whether the record holds no undo step.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Returns the number of undo steps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Writes the record and the identity of the saved content.
    ///
    /// The header identity is the invalidation rule: a later load rejects the
    /// record when the file content no longer matches.
    #[must_use]
    pub fn encode(&self, content: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&UNDO_FILE_MAGIC);
        bytes.extend_from_slice(&UNDO_FILE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&(content.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&content_hash(content).to_le_bytes());
        write_text(&mut bytes, &self.base);
        bytes.extend_from_slice(&(self.steps.len() as u64).to_le_bytes());
        for step in &self.steps {
            bytes.extend_from_slice(&(step.cursor_before as u64).to_le_bytes());
            bytes.extend_from_slice(&(step.start as u64).to_le_bytes());
            bytes.extend_from_slice(&(step.removed_chars as u64).to_le_bytes());
            write_text(&mut bytes, &step.inserted);
        }
        bytes
    }

    /// Reads one record that belongs to the given file content.
    ///
    /// Returns `None` for another magic value, another version, another
    /// content, a malformed body, or a body above the bounds. An ignored undo
    /// file is not a failure.
    #[must_use]
    pub fn decode(bytes: &[u8], content: &str) -> Option<Self> {
        let mut reader = Reader::new(bytes);
        if reader.take(UNDO_FILE_MAGIC.len())? != UNDO_FILE_MAGIC {
            return None;
        }
        if reader.u32()? != UNDO_FILE_VERSION {
            return None;
        }
        // The recorded content identity decides whether this chain still
        // belongs to the file that the editor just loaded.
        if reader.u64()? != content.len() as u64 || reader.u64()? != content_hash(content) {
            return None;
        }
        let base = reader.text()?;
        let count = usize::try_from(reader.u64()?).ok()?;
        if count > UNDO_FILE_STEPS_MAX {
            return None;
        }
        let mut steps = Vec::with_capacity(count);
        let mut change_bytes = 0;
        for _ in 0..count {
            let cursor_before = usize::try_from(reader.u64()?).ok()?;
            let start = usize::try_from(reader.u64()?).ok()?;
            let removed_chars = usize::try_from(reader.u64()?).ok()?;
            let inserted = reader.text()?;
            change_bytes += inserted.len();
            if change_bytes > UNDO_FILE_CHANGE_BYTES_MAX {
                return None;
            }
            steps.push(UndoStep {
                cursor_before,
                start,
                removed_chars,
                inserted,
            });
        }
        if !reader.is_finished() {
            return None;
        }
        Some(Self { base, steps })
    }

    /// Builds one buffer that holds the file content and this undo history.
    ///
    /// Returns `None` when the replay does not reproduce the file content
    /// exactly. That check protects the buffer against a stale or damaged
    /// record that passed the header check.
    #[must_use]
    pub fn restore(&self, content: &str, content_buffer: &TextBuffer) -> Option<TextBuffer> {
        // The buffer detects its line ending from the base text, so a base text
        // with another line ending would save the file with the wrong
        // terminator.
        if LineEnding::detect(&self.base) != LineEnding::detect(content) {
            return None;
        }
        let mut buffer = TextBuffer::from_text(&self.base, content_buffer.bytes_max()).ok()?;
        // The base text is a buffer text, which always ends with a line ending.
        // The file decides what a later save writes at the end.
        buffer
            .set_final_line_ending(FinalLineEnding::of_text(content))
            .ok()?;
        for step in &self.steps {
            let cursor = buffer.char_position(step.cursor_before).ok()?;
            let start = buffer.char_position(step.start).ok()?;
            let end = buffer
                .char_position(step.start.checked_add(step.removed_chars)?)
                .ok()?;
            let range = CharRange::new(start, end).ok()?;
            let change = TextChange::replace(range, step.inserted.clone());
            buffer.apply(EditTransaction::single(cursor, change)).ok()?;
        }
        if render_content(&buffer) != content {
            return None;
        }
        buffer.mark_saved();
        Some(buffer)
    }
}

/// Returns the undo file of one target path.
///
/// The name is the hash of the target path, so a long path stays inside the
/// file name limit of the filesystem. A hash collision cannot corrupt a buffer,
/// because the content check rejects a record of another file.
///
/// Returns `None` when the platform reports no state directory.
#[must_use]
pub fn undo_file_path(target: &FileTarget) -> Option<PathBuf> {
    Some(undo_file_path_in(&state_directory()?, target))
}

/// Returns the undo file of one target path under the given state directory.
///
/// The rule is deterministic, so a caller that holds its own state directory,
/// such as a test, needs no environment variable.
#[must_use]
pub(crate) fn undo_file_path_in(state: &Path, target: &FileTarget) -> PathBuf {
    let name = format!(
        "{:016x}.{UNDO_FILE_EXTENSION}",
        content_hash_bytes(target.as_path().as_os_str().as_encoded_bytes())
    );
    state.join(UNDO_DIRECTORY).join(name)
}

/// Returns the directory that holds the editor state of this user.
///
/// The rule follows the XDG base directory specification, which the reference
/// Neovim setup also follows.
fn state_directory() -> Option<PathBuf> {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(state));
    }
    let home = std::env::var_os("HOME").filter(|value| !value.is_empty())?;
    Some(PathBuf::from(home).join(".local").join("state"))
}

/// Reads the undo record of one file.
///
/// Every failure returns `None`, because an unreadable, unsupported, or
/// invalidated undo file must never fail the open.
#[must_use]
pub fn read_record(undo_path: &Path, content: &str) -> Option<UndoRecord> {
    let metadata = fs::metadata(undo_path).ok()?;
    if !metadata.is_file() || metadata.len() > UNDO_FILE_BYTES_MAX {
        return None;
    }
    let bytes = fs::read(undo_path).ok()?;
    UndoRecord::decode(&bytes, content)
}

/// Writes the undo record of one file.
///
/// The write is best effort. A record without a step removes a stale file
/// instead of writing an empty history.
pub fn write_record(undo_path: &Path, record: &UndoRecord, content: &str) {
    if record.is_empty() {
        let _ = fs::remove_file(undo_path);
        return;
    }
    let bytes = record.encode(content);
    if bytes.len() as u64 > UNDO_FILE_BYTES_MAX {
        return;
    }
    let Some(directory) = undo_path.parent() else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let _ = fs::write(undo_path, bytes);
}

/// Returns the change that leads from the older text to the newer text.
///
/// The change replaces the characters between the common prefix and the common
/// suffix, so one recorded transaction stays small.
fn derive_change(older: &str, newer: &str) -> (usize, usize, String) {
    let prefix = common_prefix_bytes(older, newer);
    let suffix = common_suffix_bytes(&older[prefix..], &newer[prefix..]);
    let removed = &older[prefix..older.len() - suffix];
    let inserted = &newer[prefix..newer.len() - suffix];
    (
        older[..prefix].chars().count(),
        removed.chars().count(),
        inserted.to_owned(),
    )
}

/// Returns the length of the shared start of two texts, in bytes.
fn common_prefix_bytes(left: &str, right: &str) -> usize {
    let mut shared = 0;
    for ((index, value), other) in left.char_indices().zip(right.chars()) {
        if value != other {
            return index;
        }
        shared = index + value.len_utf8();
    }
    shared
}

/// Returns the length of the shared end of two texts, in bytes.
fn common_suffix_bytes(left: &str, right: &str) -> usize {
    let mut shared = 0;
    let mut left_chars = left.chars().rev();
    let mut right_chars = right.chars().rev();
    loop {
        match (left_chars.next(), right_chars.next()) {
            (Some(value), Some(other)) if value == other => shared += value.len_utf8(),
            _ => return shared,
        }
    }
}

/// Returns the FNV-1a 64-bit hash of one text.
fn content_hash(text: &str) -> u64 {
    content_hash_bytes(text.as_bytes())
}

/// Returns the FNV-1a 64-bit hash of one byte sequence.
///
/// The hash identifies content and paths inside kvim only. It is not a
/// cryptographic hash and it protects against no attacker.
fn content_hash_bytes(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Writes one length-prefixed text.
fn write_text(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend_from_slice(&(text.len() as u64).to_le_bytes());
    bytes.extend_from_slice(text.as_bytes());
}

/// A bounded reader over one undo file body.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    /// Reports whether the reader consumed every byte.
    const fn is_finished(&self) -> bool {
        self.at == self.bytes.len()
    }

    /// Takes the next `count` bytes, or `None` past the end.
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// Takes one length-prefixed UTF-8 text.
    fn text(&mut self) -> Option<String> {
        let len = usize::try_from(self.u64()?).ok()?;
        if len as u64 > UNDO_FILE_BYTES_MAX {
            return None;
        }
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}
