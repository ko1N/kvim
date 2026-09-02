//! Tests for the which-key column layout, the bounds, and the rendering.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::which_key::{ColumnLayout, column_layout};
use crate::{
    Cell, WHICH_KEY_HINTS_MAX, WHICH_KEY_LEGEND_ENTRIES_MAX, WHICH_KEY_TEXT_CHARS_MAX,
    WhichKeyError, WhichKeyFooter, WhichKeyIcon, WhichKeyLegendEntry, WhichKeyMarker,
    WhichKeyOverlay, WhichKeyOverlayRow, WhichKeyPlacement, WhichKeyStyles,
};

/// The marker that the standalone editor paints between a key and its label.
const MARKER: &str = "\u{2192}";

/// The breadcrumb that the standalone editor gives its overlay.
const BREADCRUMB: &str = "SPC";

/// The navigation keys that the standalone editor names.
const LEGEND: [WhichKeyLegendEntry<'static>; 2] = [
    WhichKeyLegendEntry {
        key: "ESC",
        action: "close",
    },
    WhichKeyLegendEntry {
        key: "BS",
        action: "back",
    },
];

/// The color of the footer and of every key.
const ACCENT: Color = Color::Yellow;

/// The footer of the tests that read the hint rows alone.
///
/// It carries the breadcrumb and no legend, so the last row of the overlay
/// stays short and every hint row keeps its own assertion.
const fn footer() -> WhichKeyFooter<'static> {
    WhichKeyFooter {
        breadcrumb: BREADCRUMB,
        legend: &[],
    }
}

/// The marker of every test overlay: one arrow in the muted style.
fn marker() -> WhichKeyMarker<'static> {
    WhichKeyMarker {
        glyph: MARKER,
        style: Style::default().fg(Color::DarkGray),
    }
}

fn styles() -> WhichKeyStyles {
    let accent = Style::default().fg(ACCENT);
    WhichKeyStyles {
        surface: Style::default().bg(Color::Black),
        key: accent,
        note: accent,
        breadcrumb: accent,
        legend_key: accent,
        legend_action: Style::default().fg(Color::Gray),
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
    let drawn = WhichKeyOverlay::new(footer(), hints, marker(), styles())
        .expect("the hints stay inside every bound")
        .at_page(page)
        .render(&mut target, body)
        .expect("the band covers the whole cell buffer");
    (target, drawn)
}

/// Renders one hint list under a footer that carries the breadcrumb and the
/// legend.
fn painted_with_legend(hints: &[WhichKeyOverlayRow<'_>], width: u16, height: u16) -> Buffer {
    let body = Rect::new(0, 0, width, height);
    let mut target = Buffer::empty(body);
    let footer = WhichKeyFooter {
        breadcrumb: BREADCRUMB,
        legend: &LEGEND,
    };
    WhichKeyOverlay::new(footer, hints, marker(), styles())
        .expect("the hints stay inside every bound")
        .render(&mut target, body)
        .expect("the band covers the whole cell buffer");
    target
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
    // The body holds twelve rows, so the overlay takes one padding row, two
    // hint rows, one further padding row, and the footer row at the bottom.
    assert_eq!(
        row_of(&target, 8),
        format!("    /   {MARKER} Toggle the comment")
    );
    assert_eq!(
        row_of(&target, 9),
        format!("    C-w {MARKER} +3 commands"),
        "the marker and the label column both start behind the widest key"
    );
    assert_eq!(
        row_of(&target, 11),
        "    SPC",
        "the footer holds the last row of the overlay"
    );
    assert_eq!(
        target
            .cell((4, 8))
            .expect("the overlay shows its first key")
            .style()
            .fg,
        Some(ACCENT),
        "the caller style reaches every key"
    );
}

#[test]
fn one_row_paints_the_key_the_marker_the_icon_and_the_label_in_that_order() {
    let icon = WhichKeyIcon {
        glyph: "*",
        style: Style::default().fg(Color::Red),
    };
    let hints = [WhichKeyOverlayRow::new("C-w", "Write the buffer").with_icon(icon)];
    let target = painted(&hints, 40, 12);
    let row = 9;
    let symbol = |x: u16| {
        target
            .cell((x, row))
            .expect("the overlay paints inside the band")
            .symbol()
            .to_owned()
    };
    // Four pad cells open the row. The key column holds three cells, one gap
    // follows it, the marker holds one cell, one gap follows it, the icon holds
    // one cell, and one gap separates it from the label.
    assert_eq!(symbol(4), "C");
    assert_eq!(symbol(7), " ");
    assert_eq!(symbol(8), MARKER);
    assert_eq!(symbol(9), " ");
    assert_eq!(symbol(10), "*");
    assert_eq!(symbol(11), " ");
    assert_eq!(symbol(12), "W");
    assert_eq!(
        row_of(&target, row),
        format!("    C-w {MARKER} * Write the buffer")
    );
    assert_eq!(
        target
            .cell((8, row))
            .expect("the overlay paints the marker cell")
            .style()
            .fg,
        Some(Color::DarkGray),
        "the marker carries the style of the caller"
    );
}

#[test]
fn the_overlay_pads_the_row_above_the_hints_and_the_row_below_them() {
    let hints = [
        WhichKeyOverlayRow::new("/", "Toggle the comment"),
        WhichKeyOverlayRow::new("C-w", "+3 commands"),
    ];
    let body = Rect::new(0, 0, 40, 12);
    let overlay =
        WhichKeyOverlay::new(footer(), &hints, marker(), styles()).expect("bounded hints");
    let placement = overlay.placement_for(body);
    let target = painted(&hints, 40, 12);

    let first = placement.rows()[0];
    let last = placement.rows()[1];
    assert_eq!(first.area.y, 8, "one blank row opens the overlay");
    assert_eq!(last.area.y, 9);
    assert_eq!(
        row_of(&target, first.area.y - 1),
        "",
        "the row above the first hint stays blank"
    );
    assert_eq!(
        row_of(&target, last.area.y + 1),
        "",
        "the row between the last hint and the footer stays blank"
    );
    assert_eq!(row_of(&target, last.area.y + 2), "    SPC");
    assert_eq!(
        target
            .cell((0, first.area.y - 1))
            .expect("the overlay paints its padding row")
            .style()
            .bg,
        Some(Color::Black),
        "the padding row carries the surface of the overlay"
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
    let target = painted(&hints, 16, 14);
    assert_eq!(row_of(&target, 10), format!("    a {MARKER} * First"));
    assert_eq!(
        row_of(&target, 11),
        format!("    b {MARKER}   Second"),
        "a row without an icon keeps its marker and the reserved cells blank"
    );
    assert_eq!(
        target
            .cell((8, 10))
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
    let target = painted(&hints, 16, 14);
    // No hint carries an icon, so the icon column reserves no cell and the
    // marker follows the key directly.
    assert_eq!(row_of(&target, 10), format!("    a {MARKER} First"));
    assert_eq!(row_of(&target, 11), format!("    b {MARKER} Second"));
    assert_eq!(
        target
            .cell((4, 10))
            .expect("the overlay paints the marked key")
            .style()
            .fg,
        Some(Color::Red),
        "the row's own key style overrides the overlay's key style"
    );
    assert_eq!(
        target
            .cell((4, 11))
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
    let target = painted(&hints, 16, 14);
    assert_eq!(row_of(&target, 10), format!("    a {MARKER} ! First"));
    assert_eq!(
        row_of(&target, 11),
        format!("    b {MARKER}   Second"),
        "an unmarked row keeps the reserved icon cell blank"
    );
    assert_eq!(
        target
            .cell((8, 10))
            .expect("the overlay paints the icon")
            .style()
            .fg,
        Some(Color::Magenta),
        "the icon keeps its own style, which the key style does not touch"
    );
    assert_eq!(
        target
            .cell((4, 10))
            .expect("the overlay paints the marked key")
            .style()
            .fg,
        Some(Color::Red),
        "the key style marks the row apart from its icon"
    );
    assert_eq!(
        target
            .cell((4, 11))
            .expect("the overlay paints the unmarked key")
            .style()
            .fg,
        Some(ACCENT),
        "an unmarked row keeps the overlay's own key style beside a marked one"
    );
}

#[test]
fn the_footer_row_reports_the_hints_that_no_column_holds() {
    let keys: Vec<String> = (0..12).map(|index| index.to_string()).collect();
    let labels: Vec<String> = (0..12).map(|index| format!("Command {index}")).collect();
    let hints: Vec<WhichKeyOverlayRow<'_>> = keys
        .iter()
        .zip(&labels)
        .map(|(key, label)| WhichKeyOverlayRow::new(key, label))
        .collect();

    // One column of three rows fits into the half of a body band of twelve
    // rows, beside the two padding rows and the footer row, so nine hints stay
    // out of the overlay.
    let target = painted(&hints, 30, 12);
    assert_eq!(row_of(&target, 7), format!("    0  {MARKER} Command 0"));
    assert_eq!(
        row_of(&target, 11),
        format!("    SPC{}+9 more", " ".repeat(15)),
        "the footer row reports the hints that no column holds"
    );
    assert_eq!(
        row_of(&target, 5),
        "",
        "the text above the overlay survives"
    );
}

#[test]
fn a_body_band_that_cannot_hold_the_chrome_and_one_row_paints_nothing() {
    let hints = [WhichKeyOverlayRow::new("a", "First")];
    // The overlay covers half of the band, and its chrome holds two padding
    // rows and the footer row, so a band of seven rows holds no hint at all.
    for height in 0..=7 {
        let target = painted(&hints, 30, height);
        assert!(
            (0..height).all(|y| row_of(&target, y).is_empty()),
            "a band of {height} rows keeps its own text instead of a clipped overlay"
        );
    }
    let target = painted(&hints, 30, 8);
    assert_eq!(
        row_of(&target, 5),
        format!("    a {MARKER} First"),
        "the smallest band that holds the chrome and one hint paints it"
    );
}

#[test]
fn the_overlay_states_its_bounds() {
    let many = vec![WhichKeyOverlayRow::new("a", "First"); WHICH_KEY_HINTS_MAX + 1];
    assert_eq!(
        WhichKeyOverlay::new(footer(), &many, marker(), styles()).unwrap_err(),
        WhichKeyError::Hints {
            hints: WHICH_KEY_HINTS_MAX + 1,
            max: WHICH_KEY_HINTS_MAX,
        }
    );
    let most = vec![WhichKeyOverlayRow::new("a", "First"); WHICH_KEY_HINTS_MAX];
    assert!(
        WhichKeyOverlay::new(footer(), &most, marker(), styles()).is_ok(),
        "the pages reach the hints of an accepted list, and the bound stands"
    );

    let long = "a".repeat(WHICH_KEY_TEXT_CHARS_MAX + 1);
    let hints = [WhichKeyOverlayRow::new("a", &long)];
    let too_long = WhichKeyError::Text {
        chars: WHICH_KEY_TEXT_CHARS_MAX + 1,
        max: WHICH_KEY_TEXT_CHARS_MAX,
    };
    assert_eq!(
        WhichKeyOverlay::new(footer(), &hints, marker(), styles()).unwrap_err(),
        too_long
    );
    let long_breadcrumb = WhichKeyFooter {
        breadcrumb: &long,
        legend: &[],
    };
    assert_eq!(
        WhichKeyOverlay::new(long_breadcrumb, &[], marker(), styles()).unwrap_err(),
        too_long,
        "the bound covers the breadcrumb as well"
    );
    let long_legend = [WhichKeyLegendEntry {
        key: "ESC",
        action: &long,
    }];
    let long_action = WhichKeyFooter {
        breadcrumb: BREADCRUMB,
        legend: &long_legend,
    };
    assert_eq!(
        WhichKeyOverlay::new(long_action, &[], marker(), styles()).unwrap_err(),
        too_long,
        "the bound covers every legend text as well"
    );

    let entries = vec![LEGEND[0]; WHICH_KEY_LEGEND_ENTRIES_MAX + 1];
    let crowded = WhichKeyFooter {
        breadcrumb: BREADCRUMB,
        legend: &entries,
    };
    assert_eq!(
        WhichKeyOverlay::new(crowded, &[], marker(), styles()).unwrap_err(),
        WhichKeyError::Legend {
            entries: WHICH_KEY_LEGEND_ENTRIES_MAX + 1,
            max: WHICH_KEY_LEGEND_ENTRIES_MAX,
        }
    );
}

#[test]
fn a_wide_key_reserves_two_cells_of_the_key_column() {
    let hints = [
        WhichKeyOverlayRow::new("\u{ff21}", "First"),
        WhichKeyOverlayRow::new("b", "Second"),
    ];
    let target = painted(&hints, 20, 12);
    // The wide key occupies two cells, so both markers and both labels start
    // behind two cells and the gap. A measurement that counted characters would
    // move them left.
    let symbol = |x: u16, y: u16| target.cell((x, y)).map(|cell| cell.symbol().to_owned());
    assert_eq!(symbol(7, 8), Some(MARKER.to_owned()));
    assert_eq!(symbol(7, 9), Some(MARKER.to_owned()));
    assert_eq!(symbol(9, 8), Some("F".to_owned()));
    assert_eq!(symbol(9, 9), Some("S".to_owned()));
    assert_eq!(
        symbol(4, 9),
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
    let overlay = WhichKeyOverlay::new(footer(), &hints, marker(), styles())
        .expect("the hints stay in bounds");

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
        // The top row of the overlay starts the first column, so it names the
        // first hint of the reported range. The widest key is three cells.
        let first_row = drawn.rows()[0].area.y;
        let key = &keys[range.start];
        let padding = " ".repeat(3 - key.chars().count() + 1);
        assert!(
            row_of(&target, first_row).starts_with(&format!(
                "    {key}{padding}{MARKER} {}",
                labels[range.start]
            )),
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
    assert_eq!(drawn.pages(), 0, "the band holds no footer row with a hint");
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
            // Every fourth row abandons the pending sequence, so a page opens
            // on a marked row and the next page opens on an unmarked one.
            if index % 4 == 0 {
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
            .cell((4, 7))
            .expect("the first page paints its first key")
            .style()
            .fg,
        Some(Color::Red),
        "the first page opens on a marked row"
    );
    assert_eq!(
        second_page
            .cell((4, 7))
            .expect("the second page paints its first key")
            .style()
            .fg,
        Some(ACCENT),
        "the second page opens on an unmarked row"
    );

    // The label column starts ten cells in on both pages: four pad cells, the
    // three-cell key column, one gap, the one-cell marker, and one further gap.
    // The key style marker adds no cell, so the column stays put whether the
    // opening row carries it.
    assert_eq!(row_of(&first_page, 7).chars().nth(10), Some('C'));
    assert_eq!(row_of(&second_page, 7).chars().nth(10), Some('C'));
}

#[test]
fn the_pure_placement_agrees_with_the_rendered_placement() {
    // Ninety-one hints outgrow the frame, so the list holds several pages and
    // the agreement holds across a step, not only for the first page.
    let (keys, labels) = long_hints(91);
    let hints = hints_of(&keys, &labels);
    let body = Rect::new(0, 0, 60, 24);

    for page in [0, 1, usize::MAX] {
        let overlay = WhichKeyOverlay::new(footer(), &hints, marker(), styles())
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
    let overlay = WhichKeyOverlay::new(footer(), &hints, marker(), styles())
        .expect("the hints stay inside every bound");
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
fn row_placements_use_the_rendered_non_zero_origin_layout() {
    let hints = [
        WhichKeyOverlayRow::new("a", "Alpha"),
        WhichKeyOverlayRow::new("b", "Beta"),
    ];
    let body = Rect::new(11, 7, 30, 12);
    let overlay =
        WhichKeyOverlay::new(footer(), &hints, marker(), styles()).expect("bounded hints");
    let placement = overlay.placement_for(body);

    assert_eq!(placement.rows().len(), 2);
    assert_eq!(placement.rows()[0].index, 0);
    // The band holds twenty-six cells behind its left margin, so the two
    // columns take thirteen cells each and the second one ends at the right
    // margin.
    assert_eq!(placement.rows()[0].area, Rect::new(15, 16, 13, 1));
    assert_eq!(placement.rows()[1].index, 1);
    assert_eq!(placement.rows()[1].area, Rect::new(28, 16, 13, 1));
    assert_eq!(
        placement.rows()[1].area.right(),
        body.right(),
        "the last column ends at the right margin of the band"
    );

    let mut target = Buffer::empty(Rect::new(0, 0, 48, 24));
    let drawn = overlay.render(&mut target, body).expect("body fits buffer");
    assert_eq!(drawn, placement, "rendering uses the published row layout");
    assert_eq!(
        target
            .cell((placement.rows()[0].area.x, placement.rows()[0].area.y))
            .expect("first key cell")
            .symbol(),
        "a"
    );
    assert_eq!(
        target
            .cell((placement.rows()[1].area.x, placement.rows()[1].area.y))
            .expect("second key cell")
            .symbol(),
        "b"
    );
}

#[test]
fn the_columns_spread_evenly_and_the_last_one_ends_at_the_right_margin() {
    let hints = [
        WhichKeyOverlayRow::new("a", "Alpha"),
        WhichKeyOverlayRow::new("b", "Beta"),
        WhichKeyOverlayRow::new("c", "Gamma"),
    ];
    let body = Rect::new(0, 0, 60, 12);
    let overlay =
        WhichKeyOverlay::new(footer(), &hints, marker(), styles()).expect("bounded hints");
    let placement = overlay.placement_for(body);
    let target = painted(&hints, 60, 12);

    // The band holds fifty-six cells behind its left margin, so the three
    // columns take eighteen cells each and the first two take one of the two
    // remaining cells.
    let starts: Vec<u16> = placement.rows().iter().map(|row| row.area.x).collect();
    assert_eq!(starts, vec![4, 23, 42]);
    assert_eq!(
        placement.rows()[2].area.right(),
        body.right(),
        "the last column ends at the right margin"
    );
    assert_eq!(
        row_of(&target, placement.rows()[0].area.y),
        format!(
            "    a {MARKER} Alpha{}b {MARKER} Beta{}c {MARKER} Gamma",
            " ".repeat(10),
            " ".repeat(11)
        ),
        "the painted columns follow the spread"
    );

    // The free cells of one slot answer the hint of that column, so a pointer
    // beside a short label selects the row it stands over.
    let first = placement.rows()[0];
    assert_eq!(placement.row_at(Cell::new(18, first.area.y)), Some(&first));
    assert_eq!(placement.row_at(Cell::new(22, first.area.y)), Some(&first));
    assert_eq!(
        placement.row_at(Cell::new(23, first.area.y)),
        Some(&placement.rows()[1]),
        "the next column starts where the slot of the first one ends"
    );
}

#[test]
fn a_band_that_the_columns_exactly_fill_keeps_the_content_width() {
    let hints = [
        WhichKeyOverlayRow::new("a", "Alpha"),
        WhichKeyOverlayRow::new("b", "Beta"),
        WhichKeyOverlayRow::new("c", "Gamma"),
    ];
    // Three columns of eleven cells fill the thirty-three cells that a band of
    // thirty-seven cells leaves behind its left margin, so the even division
    // adds no cell and every column keeps its content width.
    let body = Rect::new(0, 0, 37, 12);
    let placement = WhichKeyOverlay::new(footer(), &hints, marker(), styles())
        .expect("bounded hints")
        .placement_for(body);

    let starts: Vec<u16> = placement.rows().iter().map(|row| row.area.x).collect();
    assert_eq!(starts, vec![4, 15, 26]);
    assert_eq!(placement.rows()[2].area.right(), body.right());
}

#[test]
fn row_placements_clip_to_the_overlay_and_hit_test_half_open_cells() {
    let hints = [
        WhichKeyOverlayRow::new("a", "Alpha"),
        WhichKeyOverlayRow::new("b", "Beta"),
    ];
    let body = Rect::new(9, 5, 5, 12);
    let placement = WhichKeyOverlay::new(footer(), &hints, marker(), styles())
        .expect("bounded hints")
        .placement_for(body);
    let first = placement.rows()[0];

    assert_eq!(first.area, Rect::new(13, 13, 1, 1));
    assert!(first.area.x >= body.x && first.area.right() <= body.right());
    assert!(first.area.y >= body.y && first.area.bottom() <= body.bottom());
    assert_eq!(
        placement.row_at(Cell::new(first.area.x, first.area.y)),
        Some(&first)
    );
    assert_eq!(
        placement.row_at(Cell::new(first.area.right(), first.area.y)),
        None,
        "the right edge is outside a ratatui rectangle"
    );
    let last = placement.rows()[1];
    assert_eq!(
        placement.row_at(Cell::new(last.area.x, last.area.bottom())),
        None,
        "the bottom edge is outside a ratatui rectangle"
    );
}

#[test]
fn empty_or_too_small_bodies_publish_no_row_placements() {
    let hints = [WhichKeyOverlayRow::new("a", "Alpha")];
    let overlay =
        WhichKeyOverlay::new(footer(), &hints, marker(), styles()).expect("bounded hints");

    for body in [Rect::new(4, 3, 0, 8), Rect::new(4, 3, 30, 3)] {
        let placement = overlay.placement_for(body);
        assert!(placement.rows().is_empty());
        assert_eq!(placement.row_at(Cell::new(body.x, body.y)), None);
    }
}

#[test]
fn a_body_that_holds_no_hint_answers_zero_pages_from_the_pure_call() {
    let hints = [WhichKeyOverlayRow::new("a", "First")];
    let narrow_band = WhichKeyOverlay::new(footer(), &hints, marker(), styles())
        .expect("the hints stay inside every bound")
        .placement_for(Rect::new(0, 0, 30, 3));
    assert_eq!(
        narrow_band.pages(),
        0,
        "the band holds no footer row with a hint"
    );
    assert_eq!(narrow_band.drawn(), 0..0);
    assert_eq!(
        narrow_band.total(),
        1,
        "the report still names the whole list"
    );

    let empty_list = WhichKeyOverlay::new(footer(), &[], marker(), styles())
        .expect("an empty list stays inside every bound")
        .placement_for(Rect::new(0, 0, 30, 12));
    assert_eq!(empty_list.total(), 0);
    assert_eq!(empty_list.pages(), 0);
    assert!(!empty_list.has_next_page());
}

#[test]
fn the_footer_row_ends_the_legend_at_the_right_margin() {
    let hints = [
        WhichKeyOverlayRow::new("/", "Toggle the comment"),
        WhichKeyOverlayRow::new("C-w", "+3 commands"),
    ];
    let target = painted_with_legend(&hints, 40, 12);
    // The legend holds eighteen cells, so a row of forty cells starts it at the
    // twenty-second cell and ends it at the right margin.
    assert_eq!(
        row_of(&target, 11),
        format!("    SPC{}ESC close  BS back", " ".repeat(15))
    );
    assert_eq!(
        target
            .cell((22, 11))
            .expect("the footer paints the first legend key")
            .style()
            .fg,
        Some(ACCENT),
        "the legend key carries the key style of the caller"
    );
    assert_eq!(
        target
            .cell((26, 11))
            .expect("the footer paints the first legend action")
            .style()
            .fg,
        Some(Color::Gray),
        "the action word carries its own style beside the key glyph"
    );
}

#[test]
fn the_footer_row_holds_the_note_left_of_the_legend() {
    let (keys, labels) = long_hints(20);
    let hints = hints_of(&keys, &labels);

    // Forty cells hold the breadcrumb, the note of the fourteen hints behind
    // the drawn page, and the legend that ends at the right margin.
    let target = painted_with_legend(&hints, 40, 12);
    assert_eq!(
        row_of(&target, 11),
        format!("    SPC{}+14 more ESC close  BS back", " ".repeat(6)),
        "the note stands left of the legend, and the legend ends at the margin"
    );
}

#[test]
fn a_narrow_footer_row_drops_the_note_before_the_legend() {
    let (keys, labels) = long_hints(20);
    let hints = hints_of(&keys, &labels);

    // Thirty cells hold the breadcrumb and the legend, but not the note that
    // counts the seventeen hints behind the page.
    let target = painted_with_legend(&hints, 30, 12);
    assert_eq!(
        row_of(&target, 11),
        format!("    SPC{}ESC close  BS back", " ".repeat(5))
    );

    // Twenty cells hold neither the legend nor the legend beside the note, so
    // the row keeps the breadcrumb and reports the count again.
    let narrow = painted_with_legend(&hints, 20, 12);
    assert_eq!(
        row_of(&narrow, 11),
        format!("    SPC{}+17 more", " ".repeat(4)),
        "a row without the legend still counts the hints behind the page"
    );
}

#[test]
fn no_row_placement_names_a_padding_row_or_the_footer_row() {
    let hints = [
        WhichKeyOverlayRow::new("/", "Toggle the comment"),
        WhichKeyOverlayRow::new("C-w", "+3 commands"),
    ];
    let body = Rect::new(0, 0, 40, 12);
    let overlay =
        WhichKeyOverlay::new(footer(), &hints, marker(), styles()).expect("bounded hints");
    let placement = overlay.placement_for(body);
    let mut target = Buffer::empty(body);
    overlay
        .render(&mut target, body)
        .expect("the band covers the whole cell buffer");

    // One blank row opens the overlay, so a pointer over a hint row selects
    // that hint, and neither padding row nor the footer row selects one.
    let first = placement.rows()[0];
    let last = placement.rows()[1];
    assert_eq!(first.area.y, 8, "one blank row opens the overlay");
    assert_eq!(last.area.y, 9);
    assert_eq!(
        placement.row_at(Cell::new(first.area.x, first.area.y)),
        Some(&first)
    );
    assert_eq!(
        placement.row_at(Cell::new(last.area.x, last.area.y)),
        Some(&last)
    );
    assert_eq!(
        placement.row_at(Cell::new(first.area.x, first.area.y - 1)),
        None,
        "the padding row above the hints answers no hint"
    );
    assert_eq!(
        placement.row_at(Cell::new(first.area.x, last.area.y + 1)),
        None,
        "the padding row below the hints answers no hint"
    );
    assert_eq!(
        placement.row_at(Cell::new(first.area.x, last.area.y + 2)),
        None,
        "the footer row answers no hint"
    );
    assert_eq!(row_of(&target, last.area.y + 2), "    SPC");
}
