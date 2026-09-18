use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Style},
};

use super::*;

fn styles() -> LineInputStyles {
    LineInputStyles {
        field: Style::default().bg(Color::Blue),
        prefix: Style::default().fg(Color::Yellow),
        text: Style::default().fg(Color::Green),
    }
}

fn row(target: &Buffer, area: Rect) -> String {
    (area.x..area.right())
        .map(|x| {
            target
                .cell((x, area.y))
                .expect("the test reads inside its buffer")
                .symbol()
        })
        .collect()
}

#[test]
fn rejects_an_empty_area_without_changing_the_buffer() {
    let buffer_area = Rect::new(2, 3, 4, 1);
    let mut target = Buffer::filled(buffer_area, ratatui::buffer::Cell::new("x"));
    let before = target.clone();

    assert_eq!(
        LineInput::render(
            &mut target,
            Rect::new(2, 3, 0, 1),
            "> ",
            "text",
            0,
            styles(),
        ),
        Err(LineInputError::EmptyArea)
    );
    assert_eq!(target, before);
}

#[test]
fn rejects_an_area_outside_the_buffer_without_changing_it() {
    let buffer_area = Rect::new(2, 3, 4, 1);
    let area = Rect::new(2, 3, 5, 1);
    let mut target = Buffer::filled(buffer_area, ratatui::buffer::Cell::new("x"));
    let before = target.clone();

    assert_eq!(
        LineInput::render(&mut target, area, "", "text", 0, styles()),
        Err(LineInputError::Area {
            area,
            buffer: buffer_area,
        })
    );
    assert_eq!(target, before);
}

#[test]
fn rejects_a_cursor_inside_a_character_without_changing_the_buffer() {
    let area = Rect::new(0, 0, 8, 1);
    let mut target = Buffer::filled(area, ratatui::buffer::Cell::new("x"));
    let before = target.clone();

    assert_eq!(
        LineInput::render(&mut target, area, "> ", "語", 1, styles()),
        Err(LineInputError::CursorNotCharBoundary { cursor: 1 })
    );
    assert_eq!(target, before);
}

#[test]
fn returns_visible_positions_at_the_start_and_end() {
    let area = Rect::new(4, 2, 8, 1);
    let mut target = Buffer::empty(area);

    let start =
        LineInput::render(&mut target, area, "> ", "abc", 0, styles()).expect("the field is valid");
    assert_eq!(start, Position::new(6, 2));
    assert_eq!(row(&target, area), "> abc   ");

    let end =
        LineInput::render(&mut target, area, "> ", "abc", 3, styles()).expect("the field is valid");
    assert_eq!(end, Position::new(9, 2));
    assert_eq!(row(&target, area), "> abc   ");
    assert_eq!(
        target
            .cell((end.x, end.y))
            .expect("the cursor is visible")
            .symbol(),
        " ",
        "the painter does not insert a fake cursor glyph"
    );
}

#[test]
fn a_wide_character_before_the_cursor_occupies_two_cells() {
    let area = Rect::new(0, 0, 8, 1);
    let mut target = Buffer::empty(area);
    let text = "語x";

    let cursor = LineInput::render(&mut target, area, ">", text, text.len(), styles())
        .expect("the field is valid");

    assert_eq!(cursor, Position::new(4, 0));
    assert_eq!(target.cell((0, 0)).expect("prefix cell").symbol(), ">");
    assert_eq!(target.cell((1, 0)).expect("wide cell").symbol(), "語");
    assert_eq!(target.cell((3, 0)).expect("text cell").symbol(), "x");
}

#[test]
fn long_text_scrolls_on_character_boundaries_and_keeps_the_cursor_visible() {
    let area = Rect::new(0, 0, 4, 1);
    let mut target = Buffer::empty(area);

    let cursor = LineInput::render(&mut target, area, ">", "abcdef", 6, styles())
        .expect("the field is valid");

    assert_eq!(cursor, Position::new(3, 0));
    assert_eq!(row(&target, area), "def ");
}

#[test]
fn a_middle_cursor_scrolls_the_prefix_and_keeps_suffix_text() {
    let area = Rect::new(0, 0, 6, 1);
    let mut target = Buffer::empty(area);

    let cursor = LineInput::render(&mut target, area, "marker: ", "abcdef", 3, styles())
        .expect("the field is valid");

    assert_eq!(cursor, Position::new(5, 0));
    assert_eq!(row(&target, area), ": abcd");
    assert_eq!(
        target
            .cell((cursor.x, cursor.y))
            .expect("the cursor is visible")
            .symbol(),
        "d",
        "suffix text remains under the real cursor at the insertion boundary"
    );
}

#[test]
fn scrolling_never_paints_half_of_a_wide_character() {
    let area = Rect::new(0, 0, 2, 1);
    let mut target = Buffer::empty(area);
    let text = "語";

    let cursor = LineInput::render(&mut target, area, "", text, text.len(), styles())
        .expect("the field is valid");

    assert_eq!(cursor, Position::new(0, 0));
    assert_eq!(row(&target, area), "  ");
}
