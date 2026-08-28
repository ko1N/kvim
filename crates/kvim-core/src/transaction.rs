//! The only way that buffer text changes.
//!
//! A transaction holds the complete set of insertions, deletions, and
//! replacements for one user-visible change. Typing, paste, a comment toggle, an
//! indent change, a block edit, and a formatter result all apply as one
//! transaction, so one undo reverses one user-visible action.

use thiserror::Error;

use super::coordinates::CharPosition;

/// The largest number of changes that one transaction holds.
///
/// A Visual Block edit produces one change for each selected line. The bound
/// keeps one transaction and its history entry bounded.
pub const TRANSACTION_CHANGES_MAX: usize = 4_096;

/// A rejected transaction or range.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TransactionError {
    /// The range end precedes the range start.
    #[error("the range end {end} precedes the range start {start}")]
    ReversedRange {
        /// The requested start position.
        start: usize,
        /// The requested end position.
        end: usize,
    },
    /// The transaction holds no change.
    #[error("the transaction holds no change")]
    Empty,
    /// Two changes overlap, or the changes do not ascend.
    #[error(
        "the change at {start} overlaps or precedes the previous change that ends at {previous_end}"
    )]
    OverlappingChanges {
        /// The start position of the rejected change.
        start: usize,
        /// The end position of the previous change.
        previous_end: usize,
    },
    /// The transaction holds more than [`TRANSACTION_CHANGES_MAX`] changes.
    #[error("the transaction holds {count} changes; the limit is {TRANSACTION_CHANGES_MAX}")]
    TooManyChanges {
        /// The number of requested changes.
        count: usize,
    },
}

/// A character range in one buffer.
///
/// The range is half-open. An empty range marks one insertion point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CharRange {
    start: CharPosition,
    end: CharPosition,
}

impl CharRange {
    /// Creates a range from two positions of the same buffer.
    ///
    /// # Errors
    ///
    /// Returns [`TransactionError::ReversedRange`] when the end precedes the
    /// start.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::{BufferBytesMax, CharRange, TextBuffer};
    ///
    /// let buffer = TextBuffer::from_text("hello", BufferBytesMax::default())
    ///     .expect("the text is small");
    /// let start = buffer.char_position(1).expect("the position exists");
    /// let end = buffer.char_position(4).expect("the position exists");
    /// assert_eq!(CharRange::new(start, end).expect("the range ascends").len_chars(), 3);
    /// assert!(CharRange::new(end, start).is_err());
    /// ```
    pub fn new(start: CharPosition, end: CharPosition) -> Result<Self, TransactionError> {
        if end < start {
            return Err(TransactionError::ReversedRange {
                start: start.get(),
                end: end.get(),
            });
        }
        Ok(Self { start, end })
    }

    /// Creates an empty range at one insertion point.
    #[must_use]
    pub const fn empty(at: CharPosition) -> Self {
        Self { start: at, end: at }
    }

    /// Returns the first position of the range.
    #[must_use]
    pub const fn start(self) -> CharPosition {
        self.start
    }

    /// Returns the position after the range.
    #[must_use]
    pub const fn end(self) -> CharPosition {
        self.end
    }

    /// Returns the number of characters in the range.
    #[must_use]
    pub const fn len_chars(self) -> usize {
        self.end.get() - self.start.get()
    }
}

/// One replacement of a character range by new text.
///
/// An empty range inserts. An empty replacement deletes. Both together are a
/// replacement, so the three edit kinds share one representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextChange {
    range: CharRange,
    replacement: String,
}

impl TextChange {
    /// Inserts text at one position.
    #[must_use]
    pub fn insert(at: CharPosition, text: impl Into<String>) -> Self {
        Self {
            range: CharRange::empty(at),
            replacement: text.into(),
        }
    }

    /// Removes the text of one range.
    #[must_use]
    pub fn delete(range: CharRange) -> Self {
        Self {
            range,
            replacement: String::new(),
        }
    }

    /// Replaces the text of one range by new text.
    #[must_use]
    pub fn replace(range: CharRange, text: impl Into<String>) -> Self {
        Self {
            range,
            replacement: text.into(),
        }
    }

    /// Returns the replaced range.
    #[must_use]
    pub const fn range(&self) -> CharRange {
        self.range
    }

    /// Returns the new text of the change.
    #[must_use]
    pub fn replacement(&self) -> &str {
        &self.replacement
    }
}

/// One user-visible change of the buffer text.
///
/// The changes ascend and never overlap, so the buffer applies them as one state
/// change. The transaction records the cursor position before the change, and
/// undo restores that position.
///
/// # Examples
///
/// ```
/// use kvim_core::{BufferBytesMax, CharRange, EditTransaction, TextBuffer, TextChange};
///
/// let mut buffer = TextBuffer::from_text("hello world\n", BufferBytesMax::default())
///     .expect("the text is small");
/// let cursor = buffer.char_position(0).expect("the position exists");
/// let start = buffer.char_position(0).expect("the position exists");
/// let end = buffer.char_position(5).expect("the position exists");
/// let range = CharRange::new(start, end).expect("the range ascends");
///
/// let transaction = EditTransaction::single(cursor, TextChange::replace(range, "goodbye"));
/// buffer.apply(transaction).expect("the range fits the buffer");
/// assert_eq!(buffer.to_string(), "goodbye world\n");
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditTransaction {
    cursor_before: CharPosition,
    changes: Vec<TextChange>,
}

impl EditTransaction {
    /// Creates a transaction from one change.
    #[must_use]
    pub fn single(cursor_before: CharPosition, change: TextChange) -> Self {
        Self {
            cursor_before,
            changes: vec![change],
        }
    }

    /// Creates a transaction from several changes.
    ///
    /// # Errors
    ///
    /// Returns [`TransactionError::Empty`] for an empty change list,
    /// [`TransactionError::TooManyChanges`] above [`TRANSACTION_CHANGES_MAX`],
    /// and [`TransactionError::OverlappingChanges`] when the changes do not
    /// ascend or when two changes overlap.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::{BufferBytesMax, EditTransaction, TextBuffer, TextChange};
    ///
    /// let mut buffer = TextBuffer::from_text("ab\ncd\n", BufferBytesMax::default())
    ///     .expect("the text is small");
    /// let cursor = buffer.char_position(0).expect("the position exists");
    /// let first = buffer.char_position(0).expect("the position exists");
    /// let second = buffer.char_position(3).expect("the position exists");
    ///
    /// // One block insert changes two lines and stays one undo step.
    /// let transaction = EditTransaction::new(
    ///     cursor,
    ///     vec![TextChange::insert(first, "> "), TextChange::insert(second, "> ")],
    /// )
    /// .expect("the changes ascend");
    /// buffer.apply(transaction).expect("the positions fit the buffer");
    /// assert_eq!(buffer.to_string(), "> ab\n> cd\n");
    /// ```
    pub fn new(
        cursor_before: CharPosition,
        changes: Vec<TextChange>,
    ) -> Result<Self, TransactionError> {
        if changes.is_empty() {
            return Err(TransactionError::Empty);
        }
        if changes.len() > TRANSACTION_CHANGES_MAX {
            return Err(TransactionError::TooManyChanges {
                count: changes.len(),
            });
        }

        let mut previous_end = 0;
        for (index, change) in changes.iter().enumerate() {
            let start = change.range.start.get();
            if index > 0 && start < previous_end {
                return Err(TransactionError::OverlappingChanges {
                    start,
                    previous_end,
                });
            }
            previous_end = change.range.end.get();
        }

        Ok(Self {
            cursor_before,
            changes,
        })
    }

    /// Returns the cursor position that the buffer held before the change.
    #[must_use]
    pub const fn cursor_before(&self) -> CharPosition {
        self.cursor_before
    }

    /// Returns the changes in ascending order.
    #[must_use]
    pub fn changes(&self) -> &[TextChange] {
        &self.changes
    }
}

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod tests;
