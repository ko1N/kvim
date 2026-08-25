use kvim_core::TerminalColumn;

use super::{RowSymbol, layout_row, terminal_column, text_cells, truncate_cells_left, wrap_cells};

const TAB: usize = 4;

fn row(text: &str, first_cell: usize, width: usize) -> String {
    let mut scratch = String::new();
    layout_row(
        text,
        TAB,
        TerminalColumn::from_measured_cells(first_cell),
        width,
    )
    .into_iter()
    .map(|cell| match cell.symbol {
        RowSymbol::WideTail => String::new(),
        other => other.as_str(&mut scratch).to_owned(),
    })
    .collect()
}

#[test]
fn a_tab_expands_to_the_next_tab_stop() {
    assert_eq!(row("\tx", 0, 6), "    x ");
    assert_eq!(row("ab\tc", 0, 6), "ab  c ");
    assert_eq!(terminal_column("ab\tc", TAB, 3).get(), 4);
}

#[test]
fn a_wide_character_occupies_two_cells() {
    // The second cell of the wide character writes an empty text, which is
    // how a terminal cell buffer records it.
    assert_eq!(row("漢字x", 0, 5), "漢字x");
    assert_eq!(terminal_column("漢字x", TAB, 2).get(), 4);
}

#[test]
fn a_split_wide_character_renders_as_a_blank() {
    // The left edge cuts the first wide character in half.
    assert_eq!(row("漢字", 1, 3), " 字");
    // The right edge cuts the second wide character in half.
    assert_eq!(row("漢字", 0, 3), "漢 ");
}

#[test]
fn a_combining_mark_joins_the_cell_before_it() {
    let cells = layout_row("e\u{301}x", TAB, TerminalColumn::from_measured_cells(0), 3);
    assert_eq!(cells[0].symbol, RowSymbol::Char('e'));
    assert_eq!(cells[1].symbol, RowSymbol::Char('x'));
    assert_eq!(cells[1].column, 2, "the mark still owns one source column");
}

#[test]
fn a_control_character_renders_as_one_blank() {
    assert_eq!(row("a\u{7}b", 0, 3), "a b");
}

#[test]
fn cells_beyond_the_line_end_keep_counting_source_columns() {
    let cells = layout_row("ab", TAB, TerminalColumn::from_measured_cells(0), 4);
    assert_eq!(cells.len(), 4);
    assert_eq!(cells[2].column, 2);
    assert_eq!(cells[3].column, 3);
}

#[test]
fn a_text_that_fits_survives_the_left_truncation_unchanged() {
    assert_eq!(truncate_cells_left("src/main.rs", 11), "src/main.rs");
    assert_eq!(truncate_cells_left("src/main.rs", 40), "src/main.rs");
}

#[test]
fn the_left_truncation_keeps_the_end_of_the_text_and_marks_the_cut() {
    assert_eq!(
        truncate_cells_left("my-folder/file.md", 14),
        "<older/file.md"
    );
    assert_eq!(truncate_cells_left("src/main.rs", 6), "<in.rs");
    assert_eq!(truncate_cells_left("src/main.rs", 1), "<");
    assert_eq!(truncate_cells_left("src/main.rs", 0), "");
}

#[test]
fn the_left_truncation_never_splits_a_wide_character() {
    assert_eq!(
        text_cells("漢字abc"),
        7,
        "each wide character owns two cells"
    );
    // Five cells leave four behind the marker, and the wide character at
    // the cut needs two of them, so the text drops it instead of showing
    // half of it. The result then stays one cell below the limit.
    for cells in 4..=5 {
        let shortened = truncate_cells_left("漢字abc", cells);
        assert_eq!(shortened, "<abc");
        assert!(text_cells(&shortened) <= cells);
    }
    // Six cells hold the marker and the complete wide character.
    assert_eq!(truncate_cells_left("漢字abc", 6), "<字abc");
    assert_eq!(text_cells("<字abc"), 6);
}

#[test]
fn a_wrap_breaks_at_the_last_space_that_fits() {
    assert_eq!(
        wrap_cells("cannot borrow the value twice", 12),
        vec!["cannot", "borrow the", "value twice"],
    );
    assert_eq!(wrap_cells("short", 12), vec!["short"]);
    assert_eq!(wrap_cells("", 12), vec![""], "a blank row survives");
}

#[test]
fn a_wrap_breaks_inside_a_word_that_no_row_can_hold() {
    assert_eq!(wrap_cells("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    // Leading indentation holds no word, so the row breaks inside the word
    // instead of emitting a blank row.
    assert_eq!(wrap_cells("    indented", 8), vec!["    inde", "nted"]);
}

#[test]
fn a_wrap_never_splits_a_wide_character() {
    // Three cells hold one wide character, and the second one starts the
    // next row instead of losing half of itself.
    let rows = wrap_cells("漢字漢字", 3);
    assert_eq!(rows, vec!["漢", "字", "漢", "字"]);
    for row in &rows {
        assert!(text_cells(row) <= 3, "no row overflows the available cells");
    }
    assert_eq!(wrap_cells("漢字abc", 5), vec!["漢字a", "bc"]);
    // One cell can never hold a wide character, so the wrap drops it.
    assert_eq!(wrap_cells("a漢b", 1), vec!["a", "b"]);
}

#[test]
fn a_horizontal_offset_clips_the_start_of_the_line() {
    assert_eq!(row("abcdef", 2, 3), "cde");
    assert_eq!(row("abcdef", 6, 3), "   ");
}
