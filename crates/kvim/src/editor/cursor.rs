//! The cursor position and the preferred column.
//!
//! A cursor is valid for the buffer that produced it. Every constructor clamps
//! the line and the column against that buffer, so a motion past a buffer limit
//! stays a valid position instead of an error.

use crate::core::{CharPosition, LineIndex, SourceColumn, TextBuffer};

/// The last column that the cursor may hold on one line.
///
/// Normal mode and the three Visual modes keep the cursor on a character. Insert
/// mode lets the cursor stand after the last character, because the next typed
/// character goes there. [`ModeState`](super::ModeState) selects the limit, so
/// the mode and the limit cannot disagree.
///
/// # Examples
///
/// ```
/// use kvim::editor::ColumnLimit;
///
/// assert_eq!(ColumnLimit::LastCharacter.last_column(5), 4);
/// assert_eq!(ColumnLimit::AfterLastCharacter.last_column(5), 5);
/// // An empty line holds column zero only.
/// assert_eq!(ColumnLimit::LastCharacter.last_column(0), 0);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColumnLimit {
    /// The cursor stays on the last character of the line.
    LastCharacter,
    /// The cursor may stand after the last character of the line.
    AfterLastCharacter,
}

impl ColumnLimit {
    /// Returns the largest column that a line of `line_len_chars` characters allows.
    #[must_use]
    pub const fn last_column(self, line_len_chars: usize) -> usize {
        match self {
            Self::LastCharacter => line_len_chars.saturating_sub(1),
            Self::AfterLastCharacter => line_len_chars,
        }
    }
}

/// The column that vertical movement tries to reach.
///
/// Vim keeps one preferred column while the cursor moves up and down, so a short
/// line does not shorten the cursor column for every later line. The `$` motion
/// selects [`PreferredColumn::LineEnd`] instead of a number, so the cursor stays
/// at the end of every line that it passes.
///
/// The value counts source columns. `core` defines the terminal-cell column, but
/// only the terminal boundary measures cell width. See `docs/text-model.md`.
///
/// # Examples
///
/// ```
/// use kvim::editor::PreferredColumn;
///
/// // A short line shortens the cursor column, but not the preferred column.
/// assert_eq!(PreferredColumn::Column(9).resolve(2), 2);
/// assert_eq!(PreferredColumn::Column(9).resolve(20), 9);
/// assert_eq!(PreferredColumn::LineEnd.resolve(2), 2);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreferredColumn {
    /// Return to this source column while the line is long enough.
    Column(usize),
    /// Stay at the end of every line, like Vim after the `$` motion.
    LineEnd,
}

impl PreferredColumn {
    /// Resolves the preferred column against the last column of one line.
    #[must_use]
    pub const fn resolve(self, last_column: usize) -> usize {
        match self {
            Self::Column(column) if column < last_column => column,
            Self::Column(_) | Self::LineEnd => last_column,
        }
    }
}

/// One text position and the column that vertical movement tries to reach.
///
/// # Examples
///
/// ```
/// use kvim::core::TextBuffer;
/// use kvim::editor::{ColumnLimit, Cursor};
/// use kvim::settings::FileSettings;
///
/// let buffer = TextBuffer::from_text("alpha\nx\n", &FileSettings::default())
///     .expect("the text is small");
/// // The column clamps to the second line, which holds one character.
/// let cursor = Cursor::clamped(&buffer, 1, 4, ColumnLimit::LastCharacter);
/// assert_eq!(cursor.line().get(), 1);
/// assert_eq!(cursor.column().get(), 0);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    line: LineIndex,
    column: SourceColumn,
    preferred: PreferredColumn,
}

impl Cursor {
    /// Places the cursor on the first character of the buffer.
    #[must_use]
    pub fn at_buffer_start(buffer: &TextBuffer, limit: ColumnLimit) -> Self {
        Self::clamped(buffer, 0, 0, limit)
    }

    /// Places the cursor at a line and a column, clamped to the buffer.
    ///
    /// The preferred column follows the clamped column, which is the rule for
    /// every horizontal motion.
    #[must_use]
    pub fn clamped(buffer: &TextBuffer, line: usize, column: usize, limit: ColumnLimit) -> Self {
        let mut cursor =
            Self::clamped_with_preferred(buffer, line, column, PreferredColumn::LineEnd, limit);
        cursor.preferred = PreferredColumn::Column(cursor.column.get());
        cursor
    }

    /// Places the cursor at a line and a column, and keeps an explicit preferred column.
    ///
    /// Vertical motions keep their preferred column, so a run of short lines does
    /// not shorten it.
    #[must_use]
    pub fn clamped_with_preferred(
        buffer: &TextBuffer,
        line: usize,
        column: usize,
        preferred: PreferredColumn,
        limit: ColumnLimit,
    ) -> Self {
        let line_count = buffer.line_count();
        debug_assert!(line_count > 0, "the rope reports one line for empty text");
        let line = buffer
            .line_index(line.min(line_count - 1))
            .expect("the clamp keeps the line index inside the buffer");
        let last_column = limit.last_column(buffer.line_len_chars(line));
        let column = buffer
            .source_column(line, column.min(last_column))
            .expect("the clamp keeps the column inside the line");
        Self {
            line,
            column,
            preferred,
        }
    }

    /// Places the cursor at a character position of the buffer.
    #[must_use]
    pub fn at_position(buffer: &TextBuffer, position: CharPosition, limit: ColumnLimit) -> Self {
        let line = buffer.char_to_line(position);
        let column = buffer.char_to_column(position);
        Self::clamped(buffer, line.get(), column.get(), limit)
    }

    /// Clamps the cursor again, after a mode change or after an edit.
    #[must_use]
    pub fn re_clamped(self, buffer: &TextBuffer, limit: ColumnLimit) -> Self {
        Self::clamped_with_preferred(
            buffer,
            self.line.get(),
            self.column.get(),
            self.preferred,
            limit,
        )
    }

    /// Returns the line that holds the cursor.
    #[must_use]
    pub const fn line(self) -> LineIndex {
        self.line
    }

    /// Returns the source column of the cursor inside its line.
    #[must_use]
    pub const fn column(self) -> SourceColumn {
        self.column
    }

    /// Returns the column that vertical movement tries to reach.
    #[must_use]
    pub const fn preferred_column(self) -> PreferredColumn {
        self.preferred
    }

    /// Returns the character position of the cursor in the buffer.
    #[must_use]
    pub fn position(self, buffer: &TextBuffer) -> CharPosition {
        buffer.column_to_char(self.line, self.column)
    }
}
