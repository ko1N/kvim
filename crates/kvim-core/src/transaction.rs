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
    /// use kvim_core::{CharRange, TextBuffer};
    /// use kvim_settings::FileSettings;
    ///
    /// let buffer = TextBuffer::from_text("hello", &FileSettings::default())
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
/// use kvim_core::{CharRange, EditTransaction, TextBuffer, TextChange};
/// use kvim_settings::FileSettings;
///
/// let mut buffer = TextBuffer::from_text("hello world", &FileSettings::default())
///     .expect("the text is small");
/// let cursor = buffer.char_position(0).expect("the position exists");
/// let start = buffer.char_position(0).expect("the position exists");
/// let end = buffer.char_position(5).expect("the position exists");
/// let range = CharRange::new(start, end).expect("the range ascends");
///
/// let transaction = EditTransaction::single(cursor, TextChange::replace(range, "goodbye"));
/// buffer.apply(transaction).expect("the range fits the buffer");
/// assert_eq!(buffer.to_string(), "goodbye world");
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
    /// use kvim_core::{EditTransaction, TextBuffer, TextChange};
    /// use kvim_settings::FileSettings;
    ///
    /// let mut buffer = TextBuffer::from_text("ab\ncd\n", &FileSettings::default())
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
mod tests {
    use super::{CharRange, EditTransaction, TextChange, TransactionError};
    use crate::CharPosition;
    use crate::TextBuffer;
    use kvim_settings::FileSettings;

    fn positions(text: &str, wanted: &[usize]) -> Vec<CharPosition> {
        let buffer =
            TextBuffer::from_text(text, &FileSettings::default()).expect("the test text is small");
        wanted
            .iter()
            .map(|position| {
                buffer
                    .char_position(*position)
                    .expect("the test position exists")
            })
            .collect()
    }

    #[test]
    fn a_reversed_range_is_rejected() {
        let bounds = positions("hello", &[1, 4]);
        assert_eq!(
            CharRange::new(bounds[1], bounds[0]),
            Err(TransactionError::ReversedRange { start: 4, end: 1 })
        );
    }

    #[test]
    fn an_empty_transaction_is_rejected() {
        let cursor = positions("hello", &[0]);
        assert_eq!(
            EditTransaction::new(cursor[0], Vec::new()),
            Err(TransactionError::Empty)
        );
    }

    #[test]
    fn overlapping_changes_are_rejected() {
        let bounds = positions("hello world", &[0, 5, 3, 8]);
        let first = CharRange::new(bounds[0], bounds[1]).expect("the range ascends");
        let second = CharRange::new(bounds[2], bounds[3]).expect("the range ascends");
        assert_eq!(
            EditTransaction::new(
                bounds[0],
                vec![TextChange::delete(first), TextChange::delete(second)],
            ),
            Err(TransactionError::OverlappingChanges {
                start: 3,
                previous_end: 5,
            })
        );
    }

    #[test]
    fn descending_changes_are_rejected() {
        let bounds = positions("hello world", &[6, 0]);
        assert_eq!(
            EditTransaction::new(
                bounds[1],
                vec![
                    TextChange::insert(bounds[0], "a"),
                    TextChange::insert(bounds[1], "b"),
                ],
            ),
            Err(TransactionError::OverlappingChanges {
                start: 0,
                previous_end: 6,
            })
        );
    }

    #[test]
    fn two_insertions_at_one_position_stay_valid() {
        let bounds = positions("hello", &[2]);
        let transaction = EditTransaction::new(
            bounds[0],
            vec![
                TextChange::insert(bounds[0], "a"),
                TextChange::insert(bounds[0], "b"),
            ],
        )
        .expect("empty ranges at one position do not overlap");
        assert_eq!(transaction.changes().len(), 2);
    }
}
