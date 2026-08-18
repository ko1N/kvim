//! Terminal-cell measurement for one buffer line.
//!
//! Rendering measures cells, never bytes and never characters. A wide character
//! occupies two cells. A tab expands to the next tab stop. `core` defines the
//! terminal-cell column, and this module is the boundary that measures it. See
//! `docs/text-model.md`.
//!
//! Line wrapping stays disabled, so one buffer line produces one terminal row.
//! A long line scrolls horizontally and clips at the window edge.

use std::borrow::Cow;

use unicode_width::UnicodeWidthChar;

use kvim_core::TerminalColumn;

/// The glyph that marks a text whose start does not fit the available cells.
pub(super) const TRUNCATION_MARKER: &str = "<";

/// The largest number of source characters that one rendered row reads.
///
/// One terminal row shows far fewer characters than this bound. The bound keeps
/// the horizontal scan finite for a single line that holds a whole file, and it
/// clips such a line deterministically.
pub(super) const ROW_SCAN_CHARS_MAX: usize = 64 * 1024;

/// The text that one rendered cell shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RowSymbol {
    /// One source character starts in this cell.
    Char(char),
    /// The cell continues the wide character that started in the cell before it.
    WideTail,
    /// The cell shows one blank: tab padding, a clipped wide character, a
    /// control character, or space beyond the end of the line.
    Blank,
}

impl RowSymbol {
    /// Returns the text that the terminal writes into the cell.
    ///
    /// The continuation of a wide character writes an empty text, which is how
    /// a terminal cell buffer records the second half of that character.
    pub(super) fn as_str(self, scratch: &mut String) -> &str {
        match self {
            Self::Char(value) => {
                scratch.clear();
                scratch.push(value);
                scratch.as_str()
            }
            Self::WideTail => "",
            Self::Blank => " ",
        }
    }
}

/// One rendered cell of one buffer line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RowCell {
    /// The text that the cell shows.
    pub(super) symbol: RowSymbol,
    /// The source column that owns the cell.
    ///
    /// Every cell of one tab expansion and both cells of one wide character
    /// carry the same source column, so a selection or a match styles them
    /// together. A cell beyond the end of the line carries the next column, so
    /// a rectangular selection and the Insert cursor still find a cell.
    pub(super) column: usize,
}

/// Returns the cells that one character occupies, and the text it shows.
///
/// A tab reaches the next tab stop from `cell`. A combining mark occupies no
/// cell and joins the character before it. A control character occupies one
/// blank cell, because writing it would move the terminal cursor.
fn measure(value: char, cell: usize, tab_width: usize) -> (usize, Option<char>) {
    debug_assert!(tab_width >= 1, "the settings hold a non-zero tab width");
    if value == '\t' {
        return (tab_width - cell % tab_width, None);
    }
    match value.width() {
        None => (1, None),
        Some(0) => (0, None),
        Some(width) => (width, Some(value)),
    }
}

/// Returns the number of cells that one character occupies outside a line.
///
/// The rule matches [`measure`] for every character except the tab, because a
/// tab reaches a tab stop that only a rendered buffer line defines.
fn char_cells(value: char) -> usize {
    value.width().unwrap_or(1)
}

/// Returns the number of terminal cells that one text occupies.
///
/// The measurement never counts bytes and never counts characters: a wide
/// character occupies two cells, a combining mark occupies none, and a control
/// character occupies one blank cell. The text carries no tab, because a tab
/// stop belongs to one rendered buffer line.
pub(super) fn text_cells(text: &str) -> usize {
    text.chars().take(ROW_SCAN_CHARS_MAX).map(char_cells).sum()
}

/// Shortens one text on the left so that it occupies at most `cells` cells.
///
/// The end of the text always survives, because the file name at the end of a
/// path names the buffer. A shortened text starts with [`TRUNCATION_MARKER`],
/// so a reader sees that the start is missing. The cut never splits a wide
/// character, so the result never overflows the available cells.
pub(super) fn truncate_cells_left(text: &str, cells: usize) -> Cow<'_, str> {
    if text_cells(text) <= cells {
        return Cow::Borrowed(text);
    }
    let Some(budget) = cells.checked_sub(text_cells(TRUNCATION_MARKER)) else {
        // A band without room for the marker shows nothing of the text.
        return Cow::Borrowed("");
    };
    let mut used = 0;
    let mut start = text.len();
    for (index, value) in text.char_indices().rev().take(ROW_SCAN_CHARS_MAX) {
        let width = char_cells(value);
        if used + width > budget {
            break;
        }
        used += width;
        start = index;
    }
    debug_assert!(used <= budget, "the loop stops before the budget overflows");
    Cow::Owned(format!("{TRUNCATION_MARKER}{}", &text[start..]))
}

/// Breaks one text into rows of at most `cells` terminal cells each.
///
/// The break prefers the last space that fits, so a word survives one row
/// whenever it can. A word that is wider than the whole row breaks inside
/// itself, because no later break would ever fit it. The cut counts cells, so
/// it never splits a wide character and no row overflows the available cells.
/// An empty text produces one empty row, so a blank separator row survives.
///
/// The scan reads at most [`ROW_SCAN_CHARS_MAX`] characters, so one
/// pathological text produces a finite number of rows.
pub(super) fn wrap_cells(text: &str, cells: usize) -> Vec<String> {
    debug_assert!(cells >= 1, "the caller reserves at least one cell for text");
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    let mut used = 0;
    // The byte offset inside `row` that follows the last space, or `None` while
    // the row holds no space that a break may use.
    let mut after_space: Option<usize> = None;

    for value in text.chars().take(ROW_SCAN_CHARS_MAX) {
        let width = char_cells(value);
        if width > cells {
            // A character that is wider than a complete row fits no row at all,
            // so the wrap drops it instead of overflowing one.
            continue;
        }
        if used + width > cells {
            // A break at the last space keeps the word whole, but only while it
            // leaves visible text behind. Leading indentation holds no word, so
            // the row breaks inside the word instead of emitting a blank row.
            match after_space.filter(|at| !row[..*at].trim_end_matches(' ').is_empty()) {
                Some(at) => {
                    let rest = row.split_off(at);
                    while row.ends_with(' ') {
                        row.pop();
                    }
                    rows.push(std::mem::replace(&mut row, rest));
                }
                None => rows.push(std::mem::take(&mut row)),
            }
            used = text_cells(&row);
            after_space = None;
        }
        row.push(value);
        used += width;
        if value == ' ' {
            after_space = Some(row.len());
        }
    }
    rows.push(row);
    debug_assert!(
        rows.iter().all(|row| text_cells(row) <= cells),
        "every break stops before the row overflows the available cells"
    );
    rows
}

/// Returns the terminal column of one source column inside one line.
///
/// The scan stops after [`ROW_SCAN_CHARS_MAX`] characters, so the result of a
/// pathological line stays finite and deterministic.
pub(super) fn terminal_column(text: &str, tab_width: usize, column: usize) -> TerminalColumn {
    let mut cell = 0;
    for (index, value) in text.chars().take(ROW_SCAN_CHARS_MAX).enumerate() {
        if index >= column {
            break;
        }
        cell += measure(value, cell, tab_width).0;
    }
    TerminalColumn::from_measured_cells(cell)
}

/// Lays out one buffer line into exactly `width` terminal cells.
///
/// `first_cell` is the terminal column of the first visible cell, so the caller
/// scrolls horizontally by moving that value. A character that the left edge or
/// the right edge splits renders as a blank, so clipping never writes half of a
/// wide character.
pub(super) fn layout_row(
    text: &str,
    tab_width: usize,
    first_cell: TerminalColumn,
    width: usize,
) -> Vec<RowCell> {
    let first_cell = first_cell.get();
    let end = first_cell.saturating_add(width);
    let mut cells: Vec<RowCell> = Vec::with_capacity(width);
    let mut cell = 0;
    let mut column = 0;

    for value in text.chars().take(ROW_SCAN_CHARS_MAX) {
        if cell >= end {
            break;
        }
        let (used, visible) = measure(value, cell, tab_width);
        if used == 0 {
            // A combining mark joins the character before it and owns no cell.
            column += 1;
            continue;
        }
        let complete = cell >= first_cell && cell + used <= end;
        for step in 0..used {
            let at = cell + step;
            if at < first_cell || at >= end {
                continue;
            }
            let symbol = match (step, visible) {
                (0, Some(value)) if complete => RowSymbol::Char(value),
                (_, Some(_)) if complete => RowSymbol::WideTail,
                _ => RowSymbol::Blank,
            };
            cells.push(RowCell { symbol, column });
        }
        cell += used;
        column += 1;
    }

    while cells.len() < width {
        cells.push(RowCell {
            symbol: RowSymbol::Blank,
            column,
        });
        column += 1;
    }
    debug_assert_eq!(
        cells.len(),
        width,
        "the layout produces one cell for every window column"
    );
    cells
}

#[cfg(test)]
mod tests {
    use kvim_core::TerminalColumn;

    use super::{
        RowSymbol, layout_row, terminal_column, text_cells, truncate_cells_left, wrap_cells,
    };

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
}
