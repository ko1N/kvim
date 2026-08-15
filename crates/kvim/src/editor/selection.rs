//! The mode of the editor and the selection that the mode owns.
//!
//! The selection shape follows the mode. [`ModeState`] carries the anchor inside
//! the mode variant, so a rectangular selection in Normal mode, or a
//! characterwise anchor in Visual Block mode, cannot be built.

use crate::core::{CharPosition, CharRange, LineIndex, SourceColumn, TextBuffer};
use crate::input::Mode;

use super::cursor::{ColumnLimit, Cursor};

/// The corner of a rectangular selection that the cursor left behind.
///
/// A block anchor keeps a line and a column, because the rectangle needs both.
/// The column belongs to the anchor line. A shorter line inside the rectangle
/// receives no change. See `docs/input-actions.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockAnchor {
    /// The line of the anchor corner.
    pub line: LineIndex,
    /// The source column of the anchor corner.
    pub column: SourceColumn,
}

/// The active mode and its selection anchor.
///
/// # Examples
///
/// ```
/// use kvim::core::TextBuffer;
/// use kvim::editor::{ColumnLimit, ModeState};
/// use kvim::input::Mode;
/// use kvim::settings::FileSettings;
///
/// let buffer = TextBuffer::from_text("alpha\n", &FileSettings::default())
///     .expect("the text is small");
/// let start = buffer.char_position(0).expect("the position exists");
///
/// assert_eq!(ModeState::Normal.mode(), Mode::Normal);
/// assert_eq!(ModeState::Normal.column_limit(), ColumnLimit::LastCharacter);
/// assert_eq!(ModeState::Insert.column_limit(), ColumnLimit::AfterLastCharacter);
/// assert_eq!(ModeState::Visual { anchor: start }.mode(), Mode::Visual);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModeState {
    /// Motions, operators, and commands act on the buffer.
    #[default]
    Normal,
    /// Printable keys insert text through edit transactions.
    Insert,
    /// A characterwise selection follows the cursor.
    Visual {
        /// The character position where the selection started.
        anchor: CharPosition,
    },
    /// A linewise selection follows the cursor.
    VisualLine {
        /// The line where the selection started.
        anchor: LineIndex,
    },
    /// A rectangular selection follows the cursor.
    VisualBlock {
        /// The corner where the selection started.
        anchor: BlockAnchor,
    },
}

impl ModeState {
    /// Returns the mode that the state holds.
    #[must_use]
    pub const fn mode(self) -> Mode {
        match self {
            Self::Normal => Mode::Normal,
            Self::Insert => Mode::Insert,
            Self::Visual { .. } => Mode::Visual,
            Self::VisualLine { .. } => Mode::VisualLine,
            Self::VisualBlock { .. } => Mode::VisualBlock,
        }
    }

    /// Returns the last column that the cursor may hold in this mode.
    #[must_use]
    pub const fn column_limit(self) -> ColumnLimit {
        match self {
            Self::Insert => ColumnLimit::AfterLastCharacter,
            Self::Normal
            | Self::Visual { .. }
            | Self::VisualLine { .. }
            | Self::VisualBlock { .. } => ColumnLimit::LastCharacter,
        }
    }

    /// Returns the selection between the anchor and the cursor.
    ///
    /// Returns `None` in Normal mode and in Insert mode, because neither mode
    /// holds an anchor.
    #[must_use]
    pub fn selection(self, buffer: &TextBuffer, cursor: Cursor) -> Option<Selection> {
        match self {
            Self::Normal | Self::Insert => None,
            Self::Visual { anchor } => {
                let head = cursor.position(buffer);
                let first = anchor.min(head);
                let last = anchor.max(head);
                // The Visual selection is inclusive, so the range ends after the
                // last selected character.
                let end = buffer
                    .char_position((last.get() + 1).min(buffer.len_chars()))
                    .expect("the clamp keeps the position inside the buffer");
                let range = CharRange::new(first, end)
                    .expect("the smaller position never follows the larger position");
                Some(Selection::Characterwise(range))
            }
            Self::VisualLine { anchor } => {
                let head = cursor.line();
                Some(Selection::Linewise {
                    first: anchor.min(head),
                    last: anchor.max(head),
                })
            }
            Self::VisualBlock { anchor } => {
                let head_line = cursor.line();
                let head_column = cursor.column();
                Some(Selection::Block {
                    first_line: anchor.line.min(head_line),
                    last_line: anchor.line.max(head_line),
                    left: anchor.column.min(head_column),
                    right: anchor.column.max(head_column),
                })
            }
        }
    }
}

/// The text that the active Visual mode selects.
///
/// Slice 6 builds the edit transactions from these values. This slice produces
/// them only.
///
/// # Examples
///
/// ```
/// use kvim::core::TextBuffer;
/// use kvim::editor::{ColumnLimit, Cursor, ModeState, Selection};
/// use kvim::settings::FileSettings;
///
/// let buffer = TextBuffer::from_text("alpha\nbeta\n", &FileSettings::default())
///     .expect("the text is small");
/// let anchor = buffer.line_index(0).expect("the first line exists");
/// let cursor = Cursor::clamped(&buffer, 1, 0, ColumnLimit::LastCharacter);
///
/// let selection = ModeState::VisualLine { anchor }
///     .selection(&buffer, cursor)
///     .expect("a Visual mode always holds a selection");
/// assert!(matches!(selection, Selection::Linewise { .. }));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Selection {
    /// A run of characters, from the first selected character to the position
    /// after the last selected character.
    Characterwise(CharRange),
    /// Complete lines, from the first line to the last line.
    Linewise {
        /// The first selected line.
        first: LineIndex,
        /// The last selected line.
        last: LineIndex,
    },
    /// A rectangle of columns over a run of lines.
    ///
    /// The columns are the block edges. A line that is shorter than the left
    /// edge receives no change.
    Block {
        /// The first selected line.
        first_line: LineIndex,
        /// The last selected line.
        last_line: LineIndex,
        /// The left edge of the rectangle.
        left: SourceColumn,
        /// The right edge of the rectangle.
        right: SourceColumn,
    },
}
