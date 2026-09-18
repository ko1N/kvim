//! The generic presentation of one edited line.
//!
//! A host supplies the prefix, text, byte cursor, and semantic styles. The
//! painter clips horizontally on character boundaries and returns the real
//! terminal cursor position. It paints no cursor glyph and knows no prompt
//! kind or input action. See `docs/windows.md`.

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::Style,
};
use thiserror::Error;
use unicode_width::UnicodeWidthChar;

use crate::layout::fits;

/// The largest number of characters in one input-field prefix.
pub const LINE_INPUT_PREFIX_CHARS_MAX: usize = 64;
/// The largest number of characters in one input-field text.
pub const LINE_INPUT_TEXT_CHARS_MAX: usize = 4_096;

/// The semantic styles of one [`LineInput`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LineInputStyles {
    /// The style applied to every cell of the field before text is painted.
    pub field: Style,
    /// The style of the caller-owned prefix.
    pub prefix: Style,
    /// The style of the edited text.
    pub text: Style,
}

/// The reason that a one-line input field could not be painted.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LineInputError {
    /// The prefix passed its character bound.
    #[error("an input prefix holds at most {max} characters, and the caller supplied more")]
    PrefixChars {
        /// The inclusive character bound.
        max: usize,
    },
    /// The edited text passed its character bound.
    #[error("an input text holds at most {max} characters, and the caller supplied more")]
    TextChars {
        /// The inclusive character bound.
        max: usize,
    },
    /// The prefix or text contains a line break.
    #[error("a one-line input field cannot contain a line break")]
    LineBreak,
    /// The cursor byte offset is after the edited text.
    #[error("cursor byte offset {cursor} is after the input text of {bytes} bytes")]
    CursorOutOfBounds {
        /// The supplied cursor byte offset.
        cursor: usize,
        /// The byte length of the edited text.
        bytes: usize,
    },
    /// The cursor byte offset splits one UTF-8 character.
    #[error("cursor byte offset {cursor} is not a character boundary")]
    CursorNotCharBoundary {
        /// The supplied cursor byte offset.
        cursor: usize,
    },
    /// The field rectangle is empty.
    #[error("a one-line input field needs a non-empty rectangle")]
    EmptyArea,
    /// The field rectangle names more than one row.
    #[error("a one-line input field needs one row, and the caller supplied {rows}")]
    AreaRows {
        /// The supplied row count.
        rows: u16,
    },
    /// The field rectangle names a cell outside the target buffer.
    #[error("input area {area:?} is outside buffer area {buffer:?}")]
    Area {
        /// The requested field rectangle.
        area: Rect,
        /// The rectangle held by the target buffer.
        buffer: Rect,
    },
}

/// Paints one caller-owned prefix and one edited line.
///
/// `cursor_offset` is a byte offset inside `text`. A host that uses
/// `kvim_input::EditedLine` passes `EditedLine::cursor_offset()` directly. The
/// painter keeps the cursor visible through deterministic horizontal scrolling
/// and returns its terminal position. It never inserts a cursor character or
/// applies a cursor style.
///
/// All validation finishes before the first buffer mutation. An error leaves
/// `target` unchanged.
///
/// # Examples
///
/// ```
/// use kvim_ui::{LineInput, LineInputStyles};
/// use ratatui::{buffer::Buffer, layout::{Position, Rect}, style::Style};
///
/// let area = Rect::new(0, 0, 8, 1);
/// let mut target = Buffer::empty(area);
/// let text = "語x";
/// let cursor = LineInput::render(
///     &mut target,
///     area,
///     "> ",
///     text,
///     text.len(),
///     LineInputStyles {
///         field: Style::default(),
///         prefix: Style::default(),
///         text: Style::default(),
///     },
/// )?;
///
/// assert_eq!(cursor, Position::new(5, 0));
/// assert_eq!(target.content()[5].symbol(), " ");
/// # Ok::<(), kvim_ui::LineInputError>(())
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LineInput;

impl LineInput {
    /// Paints the field and returns the visible terminal cursor position.
    ///
    /// # Errors
    ///
    /// Returns [`LineInputError`] for content above a bound, a line break, an
    /// invalid cursor byte offset, invalid one-row geometry, or an area outside
    /// the target buffer.
    pub fn render(
        target: &mut Buffer,
        area: Rect,
        prefix: &str,
        text: &str,
        cursor_offset: usize,
        styles: LineInputStyles,
    ) -> Result<Position, LineInputError> {
        validate_content(prefix, text, cursor_offset)?;
        validate_area(area, *target.area())?;

        let cursor_cells = text_width(prefix).saturating_add(text_width(&text[..cursor_offset]));
        let cells_before = visible_cells_before_cursor(prefix, &text[..cursor_offset], area.width);
        let scroll_cells = cursor_cells.saturating_sub(cells_before);

        clear_area(target, area, styles.field);
        paint_visible(target, area, prefix, text, scroll_cells, styles);

        let cursor_x = area.x.saturating_add(
            u16::try_from(cells_before).expect("the visible width is below the u16 row width"),
        );
        debug_assert!(
            cursor_x < area.right(),
            "one reserved cursor cell keeps the returned position inside the field"
        );
        Ok(Position::new(cursor_x, area.y))
    }
}

fn validate_content(prefix: &str, text: &str, cursor_offset: usize) -> Result<(), LineInputError> {
    if prefix.chars().take(LINE_INPUT_PREFIX_CHARS_MAX + 1).count() > LINE_INPUT_PREFIX_CHARS_MAX {
        return Err(LineInputError::PrefixChars {
            max: LINE_INPUT_PREFIX_CHARS_MAX,
        });
    }
    if text.chars().take(LINE_INPUT_TEXT_CHARS_MAX + 1).count() > LINE_INPUT_TEXT_CHARS_MAX {
        return Err(LineInputError::TextChars {
            max: LINE_INPUT_TEXT_CHARS_MAX,
        });
    }
    if prefix.contains(['\n', '\r']) || text.contains(['\n', '\r']) {
        return Err(LineInputError::LineBreak);
    }
    if cursor_offset > text.len() {
        return Err(LineInputError::CursorOutOfBounds {
            cursor: cursor_offset,
            bytes: text.len(),
        });
    }
    if !text.is_char_boundary(cursor_offset) {
        return Err(LineInputError::CursorNotCharBoundary {
            cursor: cursor_offset,
        });
    }
    Ok(())
}

fn validate_area(area: Rect, buffer: Rect) -> Result<(), LineInputError> {
    if area.is_empty() {
        return Err(LineInputError::EmptyArea);
    }
    if area.height != 1 {
        return Err(LineInputError::AreaRows { rows: area.height });
    }
    if !fits(area, buffer) {
        return Err(LineInputError::Area { area, buffer });
    }
    Ok(())
}

fn visible_cells_before_cursor(prefix: &str, before_cursor: &str, width: u16) -> usize {
    let available = usize::from(width.saturating_sub(1));
    let mut visible = 0_usize;
    for value in prefix.chars().chain(before_cursor.chars()).rev() {
        let cells = char_width(value);
        if visible.saturating_add(cells) > available {
            break;
        }
        visible += cells;
    }
    visible
}

fn clear_area(target: &mut Buffer, area: Rect, style: Style) {
    for x in area.x..area.right() {
        let cell = target
            .cell_mut((x, area.y))
            .expect("validated geometry keeps every field cell inside the buffer");
        cell.reset();
        cell.set_style(style);
    }
}

fn paint_visible(
    target: &mut Buffer,
    area: Rect,
    prefix: &str,
    text: &str,
    scroll_cells: usize,
    styles: LineInputStyles,
) {
    let (prefix_start, prefix_skip) = byte_offset_after_cells(prefix, scroll_cells);
    let prefix_visible = &prefix[prefix_start..];
    let prefix_width = text_width(prefix_visible).min(usize::from(area.width));
    if !prefix_visible.is_empty() {
        target.set_stringn(
            area.x,
            area.y,
            prefix_visible,
            usize::from(area.width),
            styles.prefix,
        );
    }

    let text_skip = scroll_cells.saturating_sub(prefix_skip);
    let (text_start, _) = byte_offset_after_cells(text, text_skip);
    let text_x = area
        .x
        .saturating_add(u16::try_from(prefix_width).unwrap_or(area.width));
    let remaining = usize::from(area.right().saturating_sub(text_x));
    if remaining > 0 {
        target.set_stringn(text_x, area.y, &text[text_start..], remaining, styles.text);
    }
}

fn byte_offset_after_cells(text: &str, cells: usize) -> (usize, usize) {
    let mut skipped = 0_usize;
    for (offset, value) in text.char_indices() {
        if skipped >= cells {
            return (offset, skipped);
        }
        skipped = skipped.saturating_add(char_width(value));
    }
    (text.len(), skipped)
}

fn text_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(value: char) -> usize {
    value.width().unwrap_or(1)
}

#[cfg(test)]
#[path = "line_input_tests.rs"]
mod tests;
