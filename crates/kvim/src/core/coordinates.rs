//! The five validated text positions of the buffer.
//!
//! Each type keeps one position kind. A value exists only after the buffer that
//! owns the text validated it, so a conversion is an explicit operation and
//! never an implicit cast. [`TextBuffer`](super::TextBuffer) owns the
//! constructors, because only the buffer knows the text.

use thiserror::Error;

/// A rejected coordinate.
///
/// The buffer returns this error instead of a panic, so an invalid position
/// from a motion, a language server, or a stale background result stays a
/// recoverable state.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CoordinateError {
    /// The byte offset is larger than the buffer.
    #[error("the byte offset {offset} is outside the buffer of {len_bytes} bytes")]
    ByteOutOfBounds {
        /// The rejected byte offset.
        offset: usize,
        /// The buffer length, in bytes.
        len_bytes: usize,
    },
    /// The byte offset falls inside a UTF-8 character.
    #[error("the byte offset {offset} splits a UTF-8 character")]
    ByteSplitsCharacter {
        /// The rejected byte offset.
        offset: usize,
    },
    /// The character position is larger than the buffer.
    #[error("the character position {position} is outside the buffer of {len_chars} characters")]
    CharOutOfBounds {
        /// The rejected character position.
        position: usize,
        /// The buffer length, in characters.
        len_chars: usize,
    },
    /// The line index does not exist in the buffer.
    #[error("the line index {index} is outside the buffer of {line_count} lines")]
    LineOutOfBounds {
        /// The rejected line index.
        index: usize,
        /// The number of lines in the buffer.
        line_count: usize,
    },
    /// The source column does not exist in its line.
    #[error("the source column {column} is outside the line of {line_len_chars} characters")]
    ColumnOutOfBounds {
        /// The rejected source column.
        column: usize,
        /// The line length, in characters, without the line terminator.
        line_len_chars: usize,
    },
}

/// A position in the UTF-8 byte sequence of the buffer.
///
/// The value always falls on a character boundary. The end of the buffer is a
/// valid offset.
///
/// # Examples
///
/// ```
/// use kvim::core::TextBuffer;
/// use kvim::settings::FileSettings;
///
/// let buffer = TextBuffer::from_text("hé", &FileSettings::default())
///     .expect("the text is small");
/// assert_eq!(buffer.byte_offset(3).expect("3 ends the text").get(), 3);
/// assert!(buffer.byte_offset(2).is_err());
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteOffset(usize);

impl ByteOffset {
    /// Returns the byte offset.
    #[must_use]
    #[inline]
    pub const fn get(self) -> usize {
        self.0
    }

    pub(super) const fn from_validated(offset: usize) -> Self {
        Self(offset)
    }
}

/// A count of Unicode scalar values from the start of the buffer.
///
/// Every transaction range uses this position, so an applied transaction cannot
/// split a character. The end of the buffer is a valid position.
///
/// # Examples
///
/// ```
/// use kvim::core::TextBuffer;
/// use kvim::settings::FileSettings;
///
/// let buffer = TextBuffer::from_text("hé", &FileSettings::default())
///     .expect("the text is small");
/// let position = buffer.char_position(2).expect("2 ends the text");
/// assert_eq!(position.get(), 2);
/// assert_eq!(buffer.char_to_byte(position).get(), 3);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CharPosition(usize);

impl CharPosition {
    /// Returns the character position.
    #[must_use]
    #[inline]
    pub const fn get(self) -> usize {
        self.0
    }

    pub(super) const fn from_validated(position: usize) -> Self {
        Self(position)
    }
}

/// A zero-based line number that exists in the buffer.
///
/// An empty buffer holds one line, so the value zero is always valid.
///
/// # Examples
///
/// ```
/// use kvim::core::TextBuffer;
/// use kvim::settings::FileSettings;
///
/// let buffer = TextBuffer::from_text("one\ntwo\n", &FileSettings::default())
///     .expect("the text is small");
/// assert_eq!(buffer.line_count(), 3);
/// assert_eq!(buffer.line_index(2).expect("the last line exists").get(), 2);
/// assert!(buffer.line_index(3).is_err());
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineIndex(usize);

impl LineIndex {
    /// The first line of a buffer.
    ///
    /// Every buffer holds this line, because the rope reports one line for
    /// empty text. A caller therefore uses the value without a buffer.
    pub const FIRST: Self = Self(0);

    /// Returns the line index.
    #[must_use]
    #[inline]
    pub const fn get(self) -> usize {
        self.0
    }

    pub(super) const fn from_validated(index: usize) -> Self {
        Self(index)
    }
}

/// A position inside one line, counted in characters of the source text.
///
/// The line terminator stays outside the line, so the column at the line length
/// is the position after the last character.
///
/// # Examples
///
/// ```
/// use kvim::core::TextBuffer;
/// use kvim::settings::FileSettings;
///
/// let buffer = TextBuffer::from_text("héllo\nworld\n", &FileSettings::default())
///     .expect("the text is small");
/// let line = buffer.line_index(0).expect("the first line exists");
/// assert_eq!(buffer.source_column(line, 5).expect("the line holds 5 characters").get(), 5);
/// assert!(buffer.source_column(line, 6).is_err());
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceColumn(usize);

impl SourceColumn {
    /// The first column of a line.
    ///
    /// Every line holds this column, because a line of zero characters still
    /// holds the position before its first character. A caller therefore uses
    /// the value without a buffer.
    pub const FIRST: Self = Self(0);

    /// Returns the source column.
    #[must_use]
    #[inline]
    pub const fn get(self) -> usize {
        self.0
    }

    pub(super) const fn from_validated(column: usize) -> Self {
        Self(column)
    }
}

/// A position on the rendered terminal row, counted in cells.
///
/// `core` defines this type, but `core` never measures cell width. A wide
/// character and a tab both occupy more cells than source characters. The
/// terminal boundary measures the width with `unicode-width` and constructs the
/// value. See the dependency ledger in `docs/architecture.md`.
///
/// # Examples
///
/// ```
/// use kvim::core::TerminalColumn;
///
/// // The terminal boundary measured two cells for one wide character.
/// let column = TerminalColumn::from_measured_cells(2);
/// assert_eq!(column.get(), 2);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TerminalColumn(usize);

impl TerminalColumn {
    /// Creates a terminal column from a width that the terminal boundary measured.
    #[must_use]
    #[inline]
    pub const fn from_measured_cells(cells: usize) -> Self {
        Self(cells)
    }

    /// Returns the terminal column.
    #[must_use]
    #[inline]
    pub const fn get(self) -> usize {
        self.0
    }
}
