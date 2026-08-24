//! Tests for the which-key column layout, the bounds, and the rendering.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::which_key::{ColumnLayout, column_layout};
use crate::{
    WHICH_KEY_HINTS_MAX, WHICH_KEY_TEXT_CHARS_MAX, WhichKeyError, WhichKeyHint, WhichKeyIcon,
    WhichKeyOverlay, WhichKeyStyles,
};

/// The title that the standalone editor gives its overlay.
const TITLE: &str = " Which Key ";

/// The color of the title row and of every key.
const ACCENT: Color = Color::Yellow;

fn styles() -> WhichKeyStyles {
    let accent = Style::default().fg(ACCENT);
    WhichKeyStyles {
        surface: Style::default().bg(Color::Black),
        title: accent,
        key: accent,
    }
}

/// Returns the painted text of one buffer row, without its trailing blanks.
fn row_of(target: &Buffer, y: u16) -> String {
    let mut text = String::new();
    for x in target.area.left()..target.area.right() {
        text.push_str(target.cell((x, y)).map_or(" ", |cell| cell.symbol()));
    }
    text.trim_end().to_owned()
}

/// Renders one hint list into a body band of the supplied size.
fn painted(hints: &[WhichKeyHint<'_>], width: u16, height: u16) -> Buffer {
    let body = Rect::new(0, 0, width, height);
    let mut target = Buffer::empty(body);
    WhichKeyOverlay::new(TITLE, hints, styles())
        .expect("the hints stay inside every bound")
        .render(&mut target, body)
        .expect("the band covers the whole cell buffer");
    target
}

#[test]
fn a_wide_terminal_spreads_the_rows_over_columns() {
    // Five columns of twenty cells fit into one hundred cells, so ten rows
    // need two rows in each column.
    let layout = column_layout(10, 20, 100, 10);
    assert_eq!(
        layout,
        ColumnLayout {
            columns: 5,
            rows_per_column: 2,
        }
    );
    assert_eq!(layout.shown(10), 10, "every row fits");
}

#[test]
fn a_narrow_terminal_keeps_one_column() {
    let layout = column_layout(6, 40, 30, 10);
    assert_eq!(
        layout,
        ColumnLayout {
            columns: 1,
            rows_per_column: 6,
        }
    );
    // A terminal narrower than the widest row still shows that one column,
    // which clips at the body edge.
    assert_eq!(column_layout(3, 40, 5, 10).columns, 1);
}

#[test]
fn the_height_bound_drops_the_rows_that_no_column_holds() {
    let layout = column_layout(30, 20, 100, 4);
    assert_eq!(
        layout,
        ColumnLayout {
            columns: 5,
            rows_per_column: 4,
        }
    );
    assert_eq!(
        layout.shown(30),
        20,
        "ten rows stay out of the bounded overlay"
    );
}

#[test]
fn no_column_of_the_overlay_stays_empty() {
    // Three columns fit, but four rows spread over three columns would leave
    // the third one empty, so two columns of two rows remain.
    let layout = column_layout(4, 20, 70, 10);
    assert_eq!(
        layout,
        ColumnLayout {
            columns: 2,
            rows_per_column: 2,
        }
    );
}

#[test]
fn an_overlay_without_rows_or_without_space_paints_nothing() {
    assert_eq!(column_layout(0, 20, 100, 10).shown(0), 0);
    assert_eq!(column_layout(5, 20, 100, 0).shown(5), 0, "no row fits");
    assert_eq!(column_layout(5, 20, 0, 10).shown(5), 0, "no cell is free");
}

#[test]
fn the_overlay_aligns_its_keys_and_its_labels_at_the_bottom_of_the_body() {
    let hints = [
        WhichKeyHint::new("/", "Toggle the comment"),
        WhichKeyHint::new("C-w", "+3 commands"),
    ];
    let target = painted(&hints, 40, 12);
    // The body holds twelve rows, so the overlay takes the title row and two
    // hint rows at the bottom.
    assert_eq!(row_of(&target, 9), " Which Key");
    assert_eq!(row_of(&target, 10), " /    Toggle the comment");
    assert_eq!(
        row_of(&target, 11),
        " C-w  +3 commands",
        "the label column starts behind the widest key"
    );
    assert_eq!(
        target
            .cell((1, 10))
            .expect("the overlay shows its first key")
            .style()
            .fg,
        Some(ACCENT),
        "the caller style reaches every key"
    );
}

#[test]
fn one_icon_reserves_the_same_width_in_every_row() {
    let icon = WhichKeyIcon {
        glyph: "*",
        style: Style::default().fg(Color::Red),
    };
    let hints = [
        WhichKeyHint::new("a", "First").with_icon(icon),
        WhichKeyHint::new("b", "Second"),
    ];
    let target = painted(&hints, 16, 8);
    assert_eq!(row_of(&target, 6), " * a  First");
    assert_eq!(
        row_of(&target, 7),
        "   b  Second",
        "a row without an icon keeps the reserved cells blank"
    );
    assert_eq!(
        target
            .cell((1, 6))
            .expect("the overlay paints the icon cell")
            .style()
            .fg,
        Some(Color::Red),
        "the icon carries the style of the caller"
    );
}

#[test]
fn the_title_row_reports_the_hints_that_no_column_holds() {
    let keys: Vec<String> = (0..12).map(|index| index.to_string()).collect();
    let labels: Vec<String> = (0..12).map(|index| format!("Command {index}")).collect();
    let hints: Vec<WhichKeyHint<'_>> = keys
        .iter()
        .zip(&labels)
        .map(|(key, label)| WhichKeyHint::new(key, label))
        .collect();

    // One column of five rows fits into the half of a body band of twelve
    // rows, so seven hints stay out of the overlay.
    let target = painted(&hints, 30, 12);
    assert_eq!(
        row_of(&target, 6),
        format!(" Which Key{}+7 more", " ".repeat(12)),
        "the title row reports the hints that no column holds"
    );
    assert_eq!(row_of(&target, 7), " 0   Command 0");
    assert_eq!(
        row_of(&target, 5),
        "",
        "the text above the overlay survives"
    );
}

#[test]
fn a_body_band_that_cannot_hold_the_title_and_one_row_paints_nothing() {
    let hints = [WhichKeyHint::new("a", "First")];
    let target = painted(&hints, 30, 3);
    assert!(
        (0..3).all(|y| row_of(&target, y).is_empty()),
        "the body keeps its own text instead of a title without a row"
    );
}

#[test]
fn the_overlay_states_both_of_its_bounds() {
    let many = vec![WhichKeyHint::new("a", "First"); WHICH_KEY_HINTS_MAX + 1];
    assert_eq!(
        WhichKeyOverlay::new(TITLE, &many, styles()).unwrap_err(),
        WhichKeyError::Hints {
            hints: WHICH_KEY_HINTS_MAX + 1,
            max: WHICH_KEY_HINTS_MAX,
        }
    );

    let long = "a".repeat(WHICH_KEY_TEXT_CHARS_MAX + 1);
    let hints = [WhichKeyHint::new("a", &long)];
    let too_long = WhichKeyError::Text {
        chars: WHICH_KEY_TEXT_CHARS_MAX + 1,
        max: WHICH_KEY_TEXT_CHARS_MAX,
    };
    assert_eq!(
        WhichKeyOverlay::new(TITLE, &hints, styles()).unwrap_err(),
        too_long
    );
    assert_eq!(
        WhichKeyOverlay::new(&long, &[], styles()).unwrap_err(),
        too_long,
        "the bound covers the title as well"
    );
}

#[test]
fn a_wide_key_reserves_two_cells_of_the_key_column() {
    let hints = [
        WhichKeyHint::new("\u{ff21}", "First"),
        WhichKeyHint::new("b", "Second"),
    ];
    let target = painted(&hints, 20, 8);
    // The wide key occupies two cells, so both labels start behind two cells
    // and the gap. A measurement that counted characters would move them left.
    let symbol = |x: u16, y: u16| target.cell((x, y)).map(|cell| cell.symbol().to_owned());
    assert_eq!(symbol(5, 6), Some("F".to_owned()));
    assert_eq!(symbol(5, 7), Some("S".to_owned()));
    assert_eq!(
        symbol(1, 7),
        Some("b".to_owned()),
        "every key starts at the same cell"
    );
}

#[test]
fn a_band_outside_the_buffer_returns_the_error_and_changes_no_cell() {
    let hints = [
        WhichKeyHint::new("a", "First"),
        WhichKeyHint::new("b", "Second"),
    ];
    let buffer = Rect::new(0, 0, 30, 8);
    let mut target = Buffer::empty(buffer);
    let untouched = target.clone();
    let overlay = WhichKeyOverlay::new(TITLE, &hints, styles()).expect("the hints stay in bounds");

    // The band starts inside the buffer and reaches past its last row, which
    // is the shape that a host produces from a stale frame size.
    let body = Rect::new(0, 4, 30, 8);
    assert_eq!(
        overlay.render(&mut target, body).unwrap_err(),
        WhichKeyError::Area { body, buffer }
    );
    assert_eq!(target, untouched, "a refused band paints no cell");
}
