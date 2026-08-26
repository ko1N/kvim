//! Tests for the which-key column layout, the bounds, and the rendering.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::which_key::{ColumnLayout, column_layout};
use crate::{
    WHICH_KEY_HINTS_MAX, WHICH_KEY_TEXT_CHARS_MAX, WhichKeyError, WhichKeyIcon, WhichKeyOverlay,
    WhichKeyOverlayRow, WhichKeyPlacement, WhichKeyStyles,
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
fn painted(hints: &[WhichKeyOverlayRow<'_>], width: u16, height: u16) -> Buffer {
    painted_page(hints, width, height, 0).0
}

/// Renders one page of a hint list and returns the buffer with its report.
fn painted_page(
    hints: &[WhichKeyOverlayRow<'_>],
    width: u16,
    height: u16,
    page: usize,
) -> (Buffer, WhichKeyPlacement) {
    let body = Rect::new(0, 0, width, height);
    let mut target = Buffer::empty(body);
    let drawn = WhichKeyOverlay::new(TITLE, hints, styles())
        .expect("the hints stay inside every bound")
        .at_page(page)
        .render(&mut target, body)
        .expect("the band covers the whole cell buffer");
    (target, drawn)
}

/// Builds one hint list of the named length, with one distinct key per row.
fn long_hints(rows: usize) -> (Vec<String>, Vec<String>) {
    let keys = (0..rows).map(|index| format!("k{index}")).collect();
    let labels = (0..rows).map(|index| format!("Command {index}")).collect();
    (keys, labels)
}

/// Zips one key list and one label list into hints.
fn hints_of<'a>(keys: &'a [String], labels: &'a [String]) -> Vec<WhichKeyOverlayRow<'a>> {
    keys.iter()
        .zip(labels)
        .map(|(key, label)| WhichKeyOverlayRow::new(key, label))
        .collect()
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
        WhichKeyOverlayRow::new("/", "Toggle the comment"),
        WhichKeyOverlayRow::new("C-w", "+3 commands"),
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
        WhichKeyOverlayRow::new("a", "First").with_icon(icon),
        WhichKeyOverlayRow::new("b", "Second"),
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
fn a_key_style_marks_the_key_without_an_icon() {
    let abandons = Style::default().fg(Color::Red);
    let hints = [
        WhichKeyOverlayRow::new("a", "First").with_key_style(abandons),
        WhichKeyOverlayRow::new("b", "Second"),
    ];
    let target = painted(&hints, 16, 8);
    assert_eq!(row_of(&target, 6), " a  First");
    assert_eq!(row_of(&target, 7), " b  Second");
    assert_eq!(
        target
            .cell((1, 6))
            .expect("the overlay paints the marked key")
            .style()
            .fg,
        Some(Color::Red),
        "the row's own key style overrides the overlay's key style"
    );
    assert_eq!(
        target
            .cell((1, 7))
            .expect("the overlay paints the unmarked key")
            .style()
            .fg,
        Some(ACCENT),
        "a row without a key style keeps the overlay's key style"
    );
}

#[test]
fn a_row_carries_an_icon_and_a_key_style_at_once() {
    let icon = WhichKeyIcon {
        glyph: "!",
        style: Style::default().fg(Color::Magenta),
    };
    let abandons = Style::default().fg(Color::Red);
    let hints = [
        WhichKeyOverlayRow::new("a", "First")
            .with_icon(icon)
            .with_key_style(abandons),
        WhichKeyOverlayRow::new("b", "Second"),
    ];
    let target = painted(&hints, 16, 8);
    assert_eq!(row_of(&target, 6), " ! a  First");
    assert_eq!(
        row_of(&target, 7),
        "   b  Second",
        "an unmarked row keeps the reserved icon cell blank"
    );
    assert_eq!(
        target
            .cell((1, 6))
            .expect("the overlay paints the icon")
            .style()
            .fg,
        Some(Color::Magenta),
        "the icon keeps its own style, which the key style does not touch"
    );
    assert_eq!(
        target
            .cell((3, 6))
            .expect("the overlay paints the marked key")
            .style()
            .fg,
        Some(Color::Red),
        "the key style marks the row apart from its icon"
    );
    assert_eq!(
        target
            .cell((3, 7))
            .expect("the overlay paints the unmarked key")
            .style()
            .fg,
        Some(ACCENT),
        "an unmarked row keeps the overlay's own key style beside a marked one"
    );
}

#[test]
fn the_title_row_reports_the_hints_that_no_column_holds() {
    let keys: Vec<String> = (0..12).map(|index| index.to_string()).collect();
    let labels: Vec<String> = (0..12).map(|index| format!("Command {index}")).collect();
    let hints: Vec<WhichKeyOverlayRow<'_>> = keys
        .iter()
        .zip(&labels)
        .map(|(key, label)| WhichKeyOverlayRow::new(key, label))
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
    let hints = [WhichKeyOverlayRow::new("a", "First")];
    let target = painted(&hints, 30, 3);
    assert!(
        (0..3).all(|y| row_of(&target, y).is_empty()),
        "the body keeps its own text instead of a title without a row"
    );
}

#[test]
fn the_overlay_states_both_of_its_bounds() {
    let many = vec![WhichKeyOverlayRow::new("a", "First"); WHICH_KEY_HINTS_MAX + 1];
    assert_eq!(
        WhichKeyOverlay::new(TITLE, &many, styles()).unwrap_err(),
        WhichKeyError::Hints {
            hints: WHICH_KEY_HINTS_MAX + 1,
            max: WHICH_KEY_HINTS_MAX,
        }
    );
    let most = vec![WhichKeyOverlayRow::new("a", "First"); WHICH_KEY_HINTS_MAX];
    assert!(
        WhichKeyOverlay::new(TITLE, &most, styles()).is_ok(),
        "the pages reach the hints of an accepted list, and the bound stands"
    );

    let long = "a".repeat(WHICH_KEY_TEXT_CHARS_MAX + 1);
    let hints = [WhichKeyOverlayRow::new("a", &long)];
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
        WhichKeyOverlayRow::new("\u{ff21}", "First"),
        WhichKeyOverlayRow::new("b", "Second"),
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
        WhichKeyOverlayRow::new("a", "First"),
        WhichKeyOverlayRow::new("b", "Second"),
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

#[test]
fn the_pages_of_a_long_list_reach_every_hint_exactly_once() {
    // Ninety-one keys is the size of one measured host idle list, and no
    // terminal band of this height holds them together.
    let (keys, labels) = long_hints(91);
    let hints = hints_of(&keys, &labels);

    let mut reached: Vec<usize> = Vec::new();
    let mut page = 0;
    let pages = loop {
        let (target, drawn) = painted_page(&hints, 60, 24, page);
        assert_eq!(drawn.total(), 91, "the report names the complete list");
        assert_eq!(drawn.page(), page);
        let range = drawn.drawn();
        assert!(!range.is_empty(), "every page holds one hint");
        // The row below the title starts the first column, so it names the
        // first hint of the reported range. The widest key is three cells.
        let title_row = (0..target.area.bottom())
            .find(|y| row_of(&target, *y).starts_with(" Which Key"))
            .expect("the page paints its title row");
        let key = &keys[range.start];
        let padding = " ".repeat(3 - key.chars().count() + 2);
        assert!(
            row_of(&target, title_row + 1)
                .starts_with(&format!(" {key}{padding}{}", labels[range.start])),
            "the first painted row is the first hint of the reported range"
        );
        reached.extend(range);
        if !drawn.has_next_page() {
            break drawn.pages();
        }
        assert!(drawn.has_previous_page() == (page > 0));
        page += 1;
    };

    assert!(pages > 1, "one frame does not hold ninety-one hints");
    assert_eq!(page + 1, pages, "the walk ends on the last page");
    let mut sorted = reached.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), reached.len(), "no hint appears on two pages");
    assert_eq!(
        reached.len(),
        91,
        "the pages together cover every hint of the list"
    );
    assert_eq!(
        sorted,
        (0..91).collect::<Vec<usize>>(),
        "no hint is skipped"
    );
}

#[test]
fn a_list_that_fits_holds_one_page_that_no_step_changes() {
    let hints = [
        WhichKeyOverlayRow::new("/", "Toggle the comment"),
        WhichKeyOverlayRow::new("C-w", "+3 commands"),
    ];
    let (first, drawn) = painted_page(&hints, 40, 12, 0);
    assert_eq!(drawn.drawn(), 0..2, "the one page draws every hint");
    assert_eq!(drawn.total(), 2);
    assert_eq!(drawn.pages(), 1);
    assert!(!drawn.has_next_page());
    assert!(!drawn.has_previous_page());

    let (stepped, again) = painted_page(&hints, 40, 12, 3);
    assert_eq!(again, drawn, "a step of a single page changes nothing");
    assert_eq!(stepped, first, "the painted cells stay the same");
}

#[test]
fn a_page_past_the_end_draws_the_last_page() {
    let (keys, labels) = long_hints(91);
    let hints = hints_of(&keys, &labels);
    let (last, drawn) = painted_page(&hints, 60, 24, usize::MAX);
    assert_eq!(drawn.page(), drawn.pages() - 1, "the page clamps");
    assert!(!drawn.has_next_page());
    assert_eq!(drawn.drawn().end, 91, "the last page ends at the last hint");

    let (same, again) = painted_page(&hints, 60, 24, drawn.pages() - 1);
    assert_eq!(again, drawn);
    assert_eq!(same, last, "the clamped page paints the last page");
}

#[test]
fn a_frame_that_holds_no_hint_reports_no_page() {
    let hints = [WhichKeyOverlayRow::new("a", "First")];
    let (_, drawn) = painted_page(&hints, 30, 3, 0);
    assert_eq!(drawn.pages(), 0, "the band holds no title row with a hint");
    assert_eq!(drawn.drawn(), 0..0);
    assert_eq!(drawn.total(), 1, "the report still names the whole list");

    let (_, empty) = painted_page(&[], 30, 12, 0);
    assert_eq!(empty.total(), 0);
    assert_eq!(empty.pages(), 0);
    assert!(!empty.has_next_page());
}

#[test]
fn a_key_style_on_some_rows_leaves_the_page_width_unchanged() {
    // Every key holds three cells, so a marker that added width would move
    // the label column of a page that opens on a marked row.
    let keys: Vec<String> = (0..40).map(|index| format!("k{index:02}")).collect();
    let labels: Vec<String> = (0..40).map(|index| format!("Command {index}")).collect();
    let abandons = Style::default().fg(Color::Red);
    let hints: Vec<WhichKeyOverlayRow<'_>> = keys
        .iter()
        .zip(&labels)
        .enumerate()
        .map(|(index, (key, label))| {
            let hint = WhichKeyOverlayRow::new(key, label);
            // Every third row abandons the pending sequence, so a page opens
            // on a marked row and the next page opens on an unmarked one.
            if index % 3 == 0 {
                hint.with_key_style(abandons)
            } else {
                hint
            }
        })
        .collect();

    let (first_page, drawn_first) = painted_page(&hints, 30, 12, 0);
    let (second_page, drawn_second) = painted_page(&hints, 30, 12, 1);
    assert!(drawn_first.pages() > 1, "the list outgrows one page");
    assert_ne!(
        drawn_first.drawn(),
        drawn_second.drawn(),
        "the two pages hold different rows"
    );
    assert_eq!(
        first_page
            .cell((1, 7))
            .expect("the first page paints its first key")
            .style()
            .fg,
        Some(Color::Red),
        "the first page opens on a marked row"
    );
    assert_eq!(
        second_page
            .cell((1, 7))
            .expect("the second page paints its first key")
            .style()
            .fg,
        Some(ACCENT),
        "the second page opens on an unmarked row"
    );

    // The label column starts six cells in on both pages: one pad cell, the
    // three-cell key column, and the two-cell gap. The key style marker adds
    // no cell, so the column stays put whether the opening row carries it.
    assert_eq!(row_of(&first_page, 7).chars().nth(6), Some('C'));
    assert_eq!(row_of(&second_page, 7).chars().nth(6), Some('C'));
}

#[test]
fn the_pure_placement_agrees_with_the_rendered_placement() {
    // Ninety-one hints outgrow the frame, so the list holds several pages and
    // the agreement holds across a step, not only for the first page.
    let (keys, labels) = long_hints(91);
    let hints = hints_of(&keys, &labels);
    let body = Rect::new(0, 0, 60, 24);

    for page in [0, 1, usize::MAX] {
        let overlay = WhichKeyOverlay::new(TITLE, &hints, styles())
            .expect("the hints stay inside every bound")
            .at_page(page);
        let pure = overlay.placement_for(body);
        let mut target = Buffer::empty(body);
        let drawn = overlay
            .render(&mut target, body)
            .expect("the band covers the whole cell buffer");
        assert_eq!(
            pure, drawn,
            "the pure answer and the rendered answer name the same page"
        );
    }
}

#[test]
fn a_host_reads_the_placement_without_a_buffer() {
    let hints = [
        WhichKeyOverlayRow::new("/", "Toggle the comment"),
        WhichKeyOverlayRow::new("C-w", "+3 commands"),
    ];
    let overlay =
        WhichKeyOverlay::new(TITLE, &hints, styles()).expect("the hints stay inside every bound");
    let body = Rect::new(0, 0, 40, 12);

    // No `Buffer` exists at this point, and `placement_for` takes `&self`,
    // so a host reads the count before it draws anything.
    let placement = overlay.placement_for(body);
    assert_eq!(placement.drawn(), 0..2, "the one page holds every hint");
    assert_eq!(placement.total(), 2);
    assert_eq!(placement.pages(), 1);
    assert!(!placement.has_next_page());
}

#[test]
fn a_body_that_holds_no_hint_answers_zero_pages_from_the_pure_call() {
    let hints = [WhichKeyOverlayRow::new("a", "First")];
    let narrow_band = WhichKeyOverlay::new(TITLE, &hints, styles())
        .expect("the hints stay inside every bound")
        .placement_for(Rect::new(0, 0, 30, 3));
    assert_eq!(
        narrow_band.pages(),
        0,
        "the band holds no title row with a hint"
    );
    assert_eq!(narrow_band.drawn(), 0..0);
    assert_eq!(
        narrow_band.total(),
        1,
        "the report still names the whole list"
    );

    let empty_list = WhichKeyOverlay::new(TITLE, &[], styles())
        .expect("an empty list stays inside every bound")
        .placement_for(Rect::new(0, 0, 30, 12));
    assert_eq!(empty_list.total(), 0);
    assert_eq!(empty_list.pages(), 0);
    assert!(!empty_list.has_next_page());
}
