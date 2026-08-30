//! Pure inverse mapping from a terminal cell to buffer source text.
//!
//! The renderer and this module share `layout_row`, so tabs, combining marks,
//! wide glyphs, horizontal clipping, and end-of-line cells have one rule.

use ratatui::layout::Rect;

use kvim_core::{CharPosition, TextBuffer};
use kvim_settings::DisplaySettings;
use kvim_terminal::CellPosition;

use super::buffer_view::{WINBAR_ROWS, gutter_cells};
use super::cells::{layout_row, terminal_column};

/// Resolves one terminal cell in a published window rectangle to source text.
///
/// The result is `None` outside the text rows. A cell in the gutter maps to the
/// first source position of its visible line. A cell beyond the line end maps
/// to its end. A wide glyph maps both of its cells to its source character.
pub(super) fn source_at_cell(
    buffer: &TextBuffer,
    area: Rect,
    display: &DisplaySettings,
    tab_width: usize,
    first_line: usize,
    left_column: usize,
    cell: CellPosition,
) -> Option<CharPosition> {
    let text = Rect::new(
        area.x,
        area.y.saturating_add(WINBAR_ROWS),
        area.width,
        area.height.saturating_sub(WINBAR_ROWS),
    );
    if !contains(text, cell) || text.width == 0 {
        return None;
    }
    debug_assert!(
        tab_width > 0,
        "the realized indent settings keep tab width non-zero"
    );

    let visible_row = usize::from(cell.row().saturating_sub(text.y));
    let line = first_line
        .saturating_add(visible_row)
        .min(buffer.line_count().saturating_sub(1));
    let index = buffer.line_index(line).ok()?;
    let line_len = buffer.line_len_chars(index);
    let gutter = gutter_cells(buffer, display, text.width);
    let text_x = text.x.saturating_add(gutter);
    let column = if cell.column() < text_x {
        0
    } else {
        let width = usize::from(text.width.saturating_sub(gutter));
        let offset = usize::from(cell.column().saturating_sub(text_x));
        let content = buffer.line_text(index);
        let first_cell = terminal_column(&content, tab_width, left_column);
        layout_row(&content, tab_width, first_cell, width)
            .get(offset)
            .map_or(line_len, |mapped| mapped.column.min(line_len))
    };
    buffer
        .char_position(buffer.line_start(index).get().saturating_add(column))
        .ok()
}

const fn contains(area: Rect, cell: CellPosition) -> bool {
    cell.column() >= area.x
        && cell.column() < area.x.saturating_add(area.width)
        && cell.row() >= area.y
        && cell.row() < area.y.saturating_add(area.height)
}

#[cfg(test)]
#[path = "pointer_tests.rs"]
mod tests;
