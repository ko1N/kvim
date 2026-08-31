use kvim_core::{BufferBytesMax, TextBuffer};
use kvim_settings::DisplaySettings;
use kvim_terminal::CellPosition;
use ratatui::layout::Rect;

use super::source_at_cell;

fn buffer(text: &str) -> TextBuffer {
    TextBuffer::from_text(text, BufferBytesMax::default()).expect("the test text fits")
}

fn position(text: &str, cell: CellPosition, first_line: usize, left_column: usize) -> usize {
    source_at_cell(
        &buffer(text),
        Rect::new(5, 7, 20, 4),
        &DisplaySettings::default(),
        4,
        first_line,
        left_column,
        cell,
    )
    .expect("the cell belongs to text")
    .get()
}

#[test]
fn winbar_cells_do_not_map_to_source_text() {
    assert!(
        source_at_cell(
            &buffer("abc"),
            Rect::new(5, 7, 20, 4),
            &DisplaySettings::default(),
            4,
            0,
            0,
            CellPosition::new(5, 7),
        )
        .is_none()
    );
}

#[test]
fn scrollbar_cells_do_not_map_to_source_text() {
    assert!(
        source_at_cell(
            &buffer("abc"),
            Rect::new(5, 7, 20, 4),
            &DisplaySettings::default(),
            4,
            0,
            0,
            CellPosition::new(24, 8),
        )
        .is_none()
    );
}

#[test]
fn gutter_and_viewport_rows_map_to_visible_source_lines() {
    assert_eq!(
        position("one\ntwo\nthree", CellPosition::new(5, 8), 1, 0),
        4
    );
    assert_eq!(
        position("one\ntwo\nthree", CellPosition::new(10, 9), 1, 0),
        8
    );
}

#[test]
fn tabs_combining_marks_and_wide_tails_map_to_their_source_character() {
    // The default gutter consumes five cells. Text starts at column 10.
    assert_eq!(position("\tab", CellPosition::new(12, 8), 0, 0), 0);
    assert_eq!(position("e\u{301}x", CellPosition::new(10, 8), 0, 0), 0);
    assert_eq!(position("e\u{301}x", CellPosition::new(11, 8), 0, 0), 2);
    assert_eq!(position("a漢b", CellPosition::new(11, 8), 0, 0), 1);
    assert_eq!(position("a漢b", CellPosition::new(12, 8), 0, 0), 1);
}

#[test]
fn horizontal_offset_and_end_of_line_cells_clamp_to_source_positions() {
    assert_eq!(position("abcdef", CellPosition::new(10, 8), 0, 2), 2);
    assert_eq!(position("ab", CellPosition::new(18, 8), 0, 0), 2);
}
