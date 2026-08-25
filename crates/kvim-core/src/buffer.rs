//! The transactional text buffer.
//!
//! The buffer owns the rope, the detected line ending, the buffer version, and
//! the bounded undo history. It performs no input and no output. A caller loads
//! the text elsewhere and hands the text to [`TextBuffer::from_text`].

use std::fmt;

use ropey::{Rope, RopeSlice};
use thiserror::Error;

use super::coordinates::{ByteOffset, CharPosition, CoordinateError, LineIndex, SourceColumn};
use super::history::{AppliedChange, AppliedTransaction, UndoHistory};
use super::transaction::EditTransaction;
use kvim_settings::FileSettings;

/// The line-break characters that the rope counts as a line terminator.
///
/// The rope follows the Unicode line-break set. The list mirrors that set
/// without the line feed, which the buffer strips first, so that a carriage
/// return and a line feed strip as one terminator.
const LINE_BREAK_CHARS: [char; 6] = ['\u{0b}', '\u{0c}', '\r', '\u{85}', '\u{2028}', '\u{2029}'];

/// A rejected buffer load.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LoadError {
    /// The text is larger than the configured maximum file size.
    #[error("the text holds {bytes} bytes; the limit is {max_bytes} bytes")]
    TooLarge {
        /// The size of the rejected text, in bytes.
        bytes: u64,
        /// The configured maximum size, in bytes.
        max_bytes: u64,
    },
}

/// A rejected edit transaction.
///
/// The buffer validates the complete transaction before the first change, so a
/// rejected transaction leaves the buffer unchanged.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EditError {
    /// A change range falls outside the current buffer.
    #[error("the change range {start}..{end} is outside the buffer of {len_chars} characters")]
    RangeOutOfBounds {
        /// The start position of the rejected range.
        start: usize,
        /// The end position of the rejected range.
        end: usize,
        /// The buffer length, in characters.
        len_chars: usize,
    },
    /// The recorded cursor position falls outside the current buffer.
    #[error("the cursor position {position} is outside the buffer of {len_chars} characters")]
    CursorOutOfBounds {
        /// The rejected cursor position.
        position: usize,
        /// The buffer length, in characters.
        len_chars: usize,
    },
}

/// The number of state changes that one buffer applied.
///
/// Background analysis, formatting, and language-server results carry the
/// version that produced them, so the editor rejects an obsolete result.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BufferVersion(u64);

impl BufferVersion {
    /// Returns the version number.
    #[must_use]
    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Self {
        Self(
            self.0
                .checked_add(1)
                .expect("a u64 version counts more edits than one session applies"),
        )
    }
}

/// The line terminator that the buffer keeps and writes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LineEnding {
    /// One line feed. This is the default on macOS and Linux.
    #[default]
    Lf,
    /// One carriage return and one line feed.
    Crlf,
}

impl LineEnding {
    /// Detects the line ending of loaded text.
    ///
    /// The first line break decides. Text without a line break uses
    /// [`LineEnding::Lf`]. Text with mixed line breaks keeps its existing lines
    /// unchanged and uses the detected ending for new lines.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::LineEnding;
    ///
    /// assert_eq!(LineEnding::detect("one\r\ntwo\n"), LineEnding::Crlf);
    /// assert_eq!(LineEnding::detect("one\ntwo\r\n"), LineEnding::Lf);
    /// assert_eq!(LineEnding::detect("one line"), LineEnding::Lf);
    /// ```
    #[must_use]
    pub fn detect(text: &str) -> Self {
        match text.find('\n') {
            Some(index) if text[..index].ends_with('\r') => Self::Crlf,
            _ => Self::Lf,
        }
    }

    /// Returns the characters that terminate one line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

/// What the file behind one buffer holds at its end.
///
/// A text file normally ends with a line ending, and that byte terminates its
/// last line. The buffer text always ends with a line ending, so the last line
/// is a line like every other one: a motion reaches it, and a new line opens
/// behind it. This value records what the file held, so the save writes the
/// bytes that the file had instead of the bytes that the buffer keeps.
///
/// # Examples
///
/// ```
/// use kvim_core::FinalLineEnding;
///
/// assert_eq!(FinalLineEnding::of_text("one\n"), FinalLineEnding::Present);
/// assert_eq!(FinalLineEnding::of_text("one"), FinalLineEnding::Absent);
/// assert_eq!(FinalLineEnding::of_text(""), FinalLineEnding::Absent);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalLineEnding {
    /// The file ended with a line ending, and the save writes it.
    Present,
    /// The file ended without a line ending, and the save writes none.
    Absent,
}

impl FinalLineEnding {
    /// Reports what one loaded text holds at its end.
    ///
    /// Empty text ends with no line ending, so an empty file stays an empty
    /// file through a save that changes nothing.
    #[must_use]
    pub fn of_text(text: &str) -> Self {
        match text.chars().next_back() {
            Some(value) if is_line_break(value) => Self::Present,
            _ => Self::Absent,
        }
    }
}

/// Bounded mutable text with validated coordinates and undoable transactions.
///
/// # Examples
///
/// ```
/// use kvim_core::{EditTransaction, TextBuffer, TextChange};
/// use kvim_settings::FileSettings;
///
/// let mut buffer = TextBuffer::from_text("fn main() {}\n", &FileSettings::default())
///     .expect("the text is small");
/// assert!(!buffer.is_modified());
///
/// let cursor = buffer.char_position(0).expect("the position exists");
/// buffer
///     .apply(EditTransaction::single(cursor, TextChange::insert(cursor, "// note\n")))
///     .expect("the position fits the buffer");
/// assert_eq!(buffer.line_count(), 2);
/// assert!(buffer.is_modified());
///
/// assert_eq!(buffer.undo(), Some(cursor));
/// assert_eq!(buffer.to_string(), "fn main() {}\n");
/// assert!(!buffer.is_modified());
/// ```
#[derive(Clone, Debug)]
pub struct TextBuffer {
    /// The buffer text. It always ends with a line ending, so every line of the
    /// buffer, including the last one, carries its own terminator.
    rope: Rope,
    line_ending: LineEnding,
    final_line_ending: FinalLineEnding,
    version: BufferVersion,
    history: UndoHistory,
}

impl TextBuffer {
    /// Creates a buffer from text that a caller already holds in memory.
    ///
    /// The buffer detects the line ending of the text and keeps it for save. It
    /// also records whether the text ended with that line ending, and it
    /// terminates the last line when the text did not. The buffer text
    /// therefore always ends with a line ending, and the save writes the file
    /// end that [`FinalLineEnding`] records.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::TooLarge`] when the text exceeds
    /// [`FileSettings::max_file_bytes`]. The check runs before the buffer
    /// exists, so an oversized file never reaches parsing or rendering.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::{LoadError, TextBuffer};
    /// use kvim_settings::FileSettings;
    ///
    /// let mut files = FileSettings::default();
    /// files.max_file_bytes = 4;
    /// assert_eq!(
    ///     TextBuffer::from_text("hello", &files).unwrap_err(),
    ///     LoadError::TooLarge { bytes: 5, max_bytes: 4 },
    /// );
    /// ```
    pub fn from_text(text: &str, files: &FileSettings) -> Result<Self, LoadError> {
        let bytes = text.len() as u64;
        if bytes > files.max_file_bytes {
            return Err(LoadError::TooLarge {
                bytes,
                max_bytes: files.max_file_bytes,
            });
        }

        let line_ending = LineEnding::detect(text);
        let final_line_ending = FinalLineEnding::of_text(text);
        let mut rope = Rope::from_str(text);
        if final_line_ending == FinalLineEnding::Absent {
            rope.insert(rope.len_chars(), line_ending.as_str());
        }
        Ok(Self {
            rope,
            line_ending,
            final_line_ending,
            version: BufferVersion(0),
            history: UndoHistory::new(),
        })
    }

    /// Returns the buffer length, in bytes.
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Returns the buffer length, in characters.
    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Returns the number of lines.
    ///
    /// A line ending terminates its line, so it opens no further empty line.
    /// `"one\ntwo\n"` holds two lines, exactly as the reference editor counts
    /// them, and so does `"one\ntwo"`. Empty text holds one empty line, so every
    /// buffer holds line zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::TextBuffer;
    /// use kvim_settings::FileSettings;
    ///
    /// let files = FileSettings::default();
    /// assert_eq!(TextBuffer::from_text("one\ntwo\n", &files).unwrap().line_count(), 2);
    /// assert_eq!(TextBuffer::from_text("one\ntwo", &files).unwrap().line_count(), 2);
    /// assert_eq!(TextBuffer::from_text("", &files).unwrap().line_count(), 1);
    /// ```
    #[must_use]
    pub fn line_count(&self) -> usize {
        let lines = self.rope.len_lines();
        // The terminator of the last line is the end of the text, so the rope
        // counts the empty text behind it as one more line.
        if self.ends_with_line_ending() {
            lines - 1
        } else {
            lines
        }
    }

    /// Returns what the file behind the buffer holds at its end.
    #[must_use]
    pub const fn final_line_ending(&self) -> FinalLineEnding {
        self.final_line_ending
    }

    /// Records what the file behind the buffer holds at its end.
    ///
    /// A caller that reads the file bytes elsewhere, such as a restored undo
    /// history, records what that file held, so the next save writes the same
    /// file end.
    pub fn set_final_line_ending(&mut self, ending: FinalLineEnding) {
        self.final_line_ending = ending;
    }

    /// Reports whether the buffer text ends with a line terminator.
    ///
    /// The invariant of [`TextBuffer::from_text`] makes this true, and every
    /// edit keeps it true. The check stays here so that a broken invariant
    /// reports one line too few instead of an impossible line count of zero.
    fn ends_with_line_ending(&self) -> bool {
        let len_chars = self.rope.len_chars();
        len_chars > 0 && is_line_break(self.rope.char(len_chars - 1))
    }

    /// Returns the line ending that the buffer writes on save.
    #[must_use]
    pub const fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Returns the number of state changes that the buffer applied.
    #[must_use]
    pub const fn version(&self) -> BufferVersion {
        self.version
    }

    /// Returns a read-only copy of the text of the current version.
    ///
    /// The copy shares the rope storage, so it costs no text memory, and it
    /// holds no undo history. A caller keeps it to convert positions of the
    /// version that an edit transaction replaced.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::{EditTransaction, TextBuffer, TextChange};
    /// use kvim_settings::FileSettings;
    ///
    /// let mut buffer = TextBuffer::from_text("alpha\n", &FileSettings::default())
    ///     .expect("the text is small");
    /// let before = buffer.snapshot();
    ///
    /// let cursor = buffer.char_position(0).expect("the position exists");
    /// buffer
    ///     .apply(EditTransaction::single(cursor, TextChange::insert(cursor, "// ")))
    ///     .expect("the position fits the buffer");
    ///
    /// assert_eq!(before.to_string(), "alpha\n");
    /// assert_ne!(before.version(), buffer.version());
    /// ```
    #[must_use]
    pub fn snapshot(&self) -> Self {
        Self {
            rope: self.rope.clone(),
            line_ending: self.line_ending,
            final_line_ending: self.final_line_ending,
            version: self.version,
            history: UndoHistory::new(),
        }
    }

    /// Reports whether the buffer differs from the last saved state.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    /// Records the current state as the saved state.
    pub fn mark_saved(&mut self) {
        self.history.mark_saved();
    }

    /// Validates a byte offset against the buffer.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError::ByteOutOfBounds`] beyond the buffer and
    /// [`CoordinateError::ByteSplitsCharacter`] inside a character.
    pub fn byte_offset(&self, offset: usize) -> Result<ByteOffset, CoordinateError> {
        let len_bytes = self.rope.len_bytes();
        if offset > len_bytes {
            return Err(CoordinateError::ByteOutOfBounds { offset, len_bytes });
        }
        // The rope rounds a byte inside a character down to that character, so
        // the round trip proves the boundary.
        if self.rope.char_to_byte(self.rope.byte_to_char(offset)) != offset {
            return Err(CoordinateError::ByteSplitsCharacter { offset });
        }
        Ok(ByteOffset::from_validated(offset))
    }

    /// Validates a character position against the buffer.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError::CharOutOfBounds`] beyond the buffer.
    pub fn char_position(&self, position: usize) -> Result<CharPosition, CoordinateError> {
        let len_chars = self.rope.len_chars();
        if position > len_chars {
            return Err(CoordinateError::CharOutOfBounds {
                position,
                len_chars,
            });
        }
        Ok(CharPosition::from_validated(position))
    }

    /// Validates a line index against the buffer.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError::LineOutOfBounds`] for a line that the buffer
    /// does not hold.
    pub fn line_index(&self, index: usize) -> Result<LineIndex, CoordinateError> {
        let line_count = self.line_count();
        if index >= line_count {
            return Err(CoordinateError::LineOutOfBounds { index, line_count });
        }
        Ok(LineIndex::from_validated(index))
    }

    /// Validates a source column against one line.
    ///
    /// The column at the line length is the position after the last character.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError::ColumnOutOfBounds`] beyond the line content.
    pub fn source_column(
        &self,
        line: LineIndex,
        column: usize,
    ) -> Result<SourceColumn, CoordinateError> {
        let line_len_chars = self.line_len_chars(line);
        if column > line_len_chars {
            return Err(CoordinateError::ColumnOutOfBounds {
                column,
                line_len_chars,
            });
        }
        Ok(SourceColumn::from_validated(column))
    }

    /// Converts a byte offset into a character position.
    #[must_use]
    pub fn byte_to_char(&self, offset: ByteOffset) -> CharPosition {
        CharPosition::from_validated(self.rope.byte_to_char(offset.get()))
    }

    /// Converts a character position into a byte offset.
    #[must_use]
    pub fn char_to_byte(&self, position: CharPosition) -> ByteOffset {
        ByteOffset::from_validated(self.rope.char_to_byte(position.get()))
    }

    /// Returns the line that holds a character position.
    ///
    /// The position after the terminator of the last line lies behind every
    /// line, so it reports the index after the last line. That index is the
    /// end-of-text position that the language-server protocol expects, and it
    /// is not a line that [`TextBuffer::line_index`] accepts. Every caller that
    /// places a cursor clamps the line against [`TextBuffer::line_count`].
    #[must_use]
    pub fn char_to_line(&self, position: CharPosition) -> LineIndex {
        LineIndex::from_validated(self.rope.char_to_line(position.get()))
    }

    /// Returns the first character position of a line.
    #[must_use]
    pub fn line_start(&self, line: LineIndex) -> CharPosition {
        CharPosition::from_validated(self.rope.line_to_char(line.get()))
    }

    /// Returns the source column of a character position inside its line.
    ///
    /// A position on a line terminator reports the column after the last
    /// character of the line.
    #[must_use]
    pub fn char_to_column(&self, position: CharPosition) -> SourceColumn {
        let line = self.char_to_line(position);
        let start = self.line_start(line).get();
        let column = position.get() - start;
        SourceColumn::from_validated(column.min(self.line_len_chars(line)))
    }

    /// Converts a line and a source column into a character position.
    #[must_use]
    pub fn column_to_char(&self, line: LineIndex, column: SourceColumn) -> CharPosition {
        CharPosition::from_validated(self.line_start(line).get() + column.get())
    }

    /// Returns the text of one line without its terminator.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::TextBuffer;
    /// use kvim_settings::FileSettings;
    ///
    /// let buffer = TextBuffer::from_text("one\r\ntwo\r\n", &FileSettings::default())
    ///     .expect("the text is small");
    /// let line = buffer.line_index(0).expect("the first line exists");
    /// assert_eq!(buffer.line_text(line), "one");
    /// ```
    #[must_use]
    pub fn line_text(&self, line: LineIndex) -> String {
        self.line_slice(line).into()
    }

    /// Returns the number of characters in one line, without its terminator.
    #[must_use]
    pub fn line_len_chars(&self, line: LineIndex) -> usize {
        self.line_slice(line).len_chars()
    }

    /// Reports whether one line holds ASCII characters only.
    ///
    /// A caller that segments the line into grapheme clusters uses this as a
    /// fast path, because every ASCII character is its own cluster. The check
    /// reads the stored chunks and allocates nothing, unlike
    /// [`TextBuffer::line_text`]. `core` holds no segmentation table, so the
    /// segmentation itself belongs to the caller. See `docs/text-model.md`.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::TextBuffer;
    /// use kvim_settings::FileSettings;
    ///
    /// let buffer = TextBuffer::from_text("one\ntw\u{f6}\n", &FileSettings::default())
    ///     .expect("the text is small");
    /// let first = buffer.line_index(0).expect("the first line exists");
    /// let second = buffer.line_index(1).expect("the second line exists");
    /// assert!(buffer.line_is_ascii(first));
    /// assert!(!buffer.line_is_ascii(second));
    /// ```
    #[must_use]
    pub fn line_is_ascii(&self, line: LineIndex) -> bool {
        self.line_slice(line).chunks().all(str::is_ascii)
    }

    /// Applies one transaction as one state change and records it for undo.
    ///
    /// # Errors
    ///
    /// Returns [`EditError`] when a range or the recorded cursor falls outside
    /// the current buffer. The buffer stays unchanged in that case.
    pub fn apply(&mut self, transaction: EditTransaction) -> Result<BufferVersion, EditError> {
        let len_chars = self.rope.len_chars();
        let cursor_before = transaction.cursor_before();
        if cursor_before.get() > len_chars {
            return Err(EditError::CursorOutOfBounds {
                position: cursor_before.get(),
                len_chars,
            });
        }
        for change in transaction.changes() {
            let range = change.range();
            if range.end().get() > len_chars {
                return Err(EditError::RangeOutOfBounds {
                    start: range.start().get(),
                    end: range.end().get(),
                    len_chars,
                });
            }
        }

        // Stage the complete history entry from the current text, then change
        // the rope. The entry holds both texts, so undo and redo replay it.
        let mut changes = Vec::with_capacity(transaction.changes().len());
        let mut removed_total = 0;
        let mut inserted_total = 0;
        for change in transaction.changes() {
            let start = change.range().start().get();
            let removed_chars = change.range().len_chars();
            let removed: String = self.rope.slice(start..start + removed_chars).into();
            let inserted = change.replacement().to_owned();
            let inserted_chars = inserted.chars().count();
            debug_assert!(
                start >= removed_total,
                "ascending non-overlapping ranges keep every start behind the removed text"
            );
            changes.push(AppliedChange {
                start,
                new_start: start + inserted_total - removed_total,
                removed,
                removed_chars,
                inserted,
                inserted_chars,
            });
            removed_total += removed_chars;
            inserted_total += inserted_chars;
        }

        let last = changes
            .last()
            .expect("a transaction holds at least one change");
        let cursor_after = CharPosition::from_validated(last.new_start + last.inserted_chars);
        let entry = AppliedTransaction {
            changes,
            cursor_before,
            cursor_after,
        };

        replay(&mut self.rope, &entry);
        self.history.push(entry);
        self.version = self.version.next();
        Ok(self.version)
    }

    /// Reverses the last applied transaction.
    ///
    /// Returns the cursor position that the buffer held before that
    /// transaction, or `None` when the history holds no further step.
    pub fn undo(&mut self) -> Option<CharPosition> {
        let Self {
            rope,
            version,
            history,
            ..
        } = self;
        let entry = history.step_back()?;
        for change in entry.changes.iter().rev() {
            let start = change.new_start;
            rope.remove(start..start + change.inserted_chars);
            rope.insert(start, &change.removed);
        }
        let cursor = entry.cursor_before;
        *version = version.next();
        Some(cursor)
    }

    /// Reapplies the transaction above the current history position.
    ///
    /// Returns the cursor position after that transaction, or `None` when the
    /// history holds no further step.
    pub fn redo(&mut self) -> Option<CharPosition> {
        let Self {
            rope,
            version,
            history,
            ..
        } = self;
        let entry = history.step_forward()?;
        replay(rope, entry);
        let cursor = entry.cursor_after;
        *version = version.next();
        Some(cursor)
    }

    fn line_slice(&self, line: LineIndex) -> RopeSlice<'_> {
        let slice = self.rope.line(line.get());
        let mut end = slice.len_chars();
        if end > 0 && slice.char(end - 1) == '\n' {
            end -= 1;
            if end > 0 && slice.char(end - 1) == '\r' {
                end -= 1;
            }
        } else if end > 0 && LINE_BREAK_CHARS.contains(&slice.char(end - 1)) {
            end -= 1;
        }
        slice.slice(..end)
    }
}

impl fmt::Display for TextBuffer {
    /// Writes the complete buffer text, including every line terminator.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.rope.chunks() {
            formatter.write_str(chunk)?;
        }
        Ok(())
    }
}

/// Reports whether one character terminates a line.
///
/// The list of [`LINE_BREAK_CHARS`] leaves out the line feed, so the check adds
/// it here.
fn is_line_break(value: char) -> bool {
    value == '\n' || LINE_BREAK_CHARS.contains(&value)
}

/// Applies one recorded transaction to the rope.
///
/// The changes run in descending order, so the position of every remaining
/// change stays valid while the rope changes.
fn replay(rope: &mut Rope, entry: &AppliedTransaction) {
    for change in entry.changes.iter().rev() {
        rope.remove(change.start..change.start + change.removed_chars);
        rope.insert(change.start, &change.inserted);
    }
}
