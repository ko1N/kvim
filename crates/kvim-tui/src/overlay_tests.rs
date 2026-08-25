use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect, Size};

use crate::completion::{CompletionCycle, LineCompletion};
use crate::theme::{Theme, ThemeRole};

use super::{
    COMPLETION_ROWS_MAX, OVERFLOW_NOTE, completion_first_row, float_area, render_completion,
    text_cells,
};

/// One editor window that starts at the top left corner.
const WINDOW: Rect = Rect {
    x: 0,
    y: 0,
    width: 40,
    height: 20,
};

/// The right window of one vertical split, which starts inside the body.
const SPLIT: Rect = Rect {
    x: 20,
    y: 3,
    width: 20,
    height: 10,
};

#[test]
fn a_float_sits_below_the_cursor_line() {
    let area = float_area(WINDOW, Position::new(5, 2), Size::new(12, 4));
    assert_eq!(
        area,
        Rect::new(5, 3, 12, 4),
        "the float starts one row down"
    );
}

#[test]
fn a_float_flips_above_the_cursor_line_when_the_space_below_is_too_small() {
    // Three rows follow the cursor line, and the float needs four.
    let area = float_area(WINDOW, Position::new(5, 16), Size::new(12, 4));
    assert_eq!(area, Rect::new(5, 12, 12, 4));
    assert_eq!(area.bottom(), 16, "the float never covers the cursor line");
}

#[test]
fn a_float_takes_the_larger_side_and_clips_when_neither_side_holds_it() {
    let narrow = Rect::new(0, 0, 40, 7);
    // Two rows sit above the cursor line and four below it.
    let area = float_area(narrow, Position::new(5, 2), Size::new(12, 9));
    assert_eq!(area, Rect::new(5, 3, 12, 4));
    // Four rows sit above the cursor line and two below it.
    let area = float_area(narrow, Position::new(5, 4), Size::new(12, 9));
    assert_eq!(area, Rect::new(5, 0, 12, 4));
}

#[test]
fn a_float_moves_left_until_its_right_edge_sits_inside_the_window() {
    let area = float_area(WINDOW, Position::new(36, 2), Size::new(12, 4));
    assert_eq!(area, Rect::new(28, 3, 12, 4));
    assert_eq!(area.right(), WINDOW.right());
}

#[test]
fn a_float_that_is_wider_than_the_window_starts_at_the_window_edge() {
    let area = float_area(WINDOW, Position::new(36, 2), Size::new(60, 4));
    assert_eq!(area, Rect::new(0, 3, 40, 4));
}

#[test]
fn a_float_of_a_split_stays_inside_that_window() {
    // The cursor sits near the right edge and near the bottom of the split,
    // so both rules act, and both keep the float inside the split.
    let area = float_area(SPLIT, Position::new(38, 11), Size::new(14, 5));
    assert_eq!(area, Rect::new(26, 6, 14, 5));
    assert!(
        SPLIT.contains(Position::new(area.x, area.y))
            && area.right() <= SPLIT.right()
            && area.bottom() <= SPLIT.bottom(),
        "the float of a split never reaches outside that window"
    );
}

#[test]
fn a_window_of_one_row_leaves_no_space_beside_the_cursor_line() {
    let single = Rect::new(0, 0, 40, 1);
    let area = float_area(single, Position::new(0, 0), Size::new(12, 4));
    assert_eq!(area.height, 0, "no row remains beside the cursor line");
}

/// The character bound of a prompt that accepts every test candidate.
const CHARS_MAX: usize = 32;

/// The candidates of a completion that is longer than the row bound.
const MANY: [&str; 12] = [
    "c00", "c01", "c02", "c03", "c04", "c05", "c06", "c07", "c08", "c09", "c10", "c11",
];

/// Opens one completion over `candidates`, with the first one selected.
fn completion(candidates: &[&str]) -> LineCompletion {
    let candidates = candidates.iter().map(|text| (*text).to_owned()).collect();
    LineCompletion::open("c", candidates, CHARS_MAX, CompletionCycle::Next)
        .expect("the test offers at least one candidate")
}

/// Renders one candidate list over the body band `body`.
fn draw_completion(body: Rect, completion: &LineCompletion) -> CellBuffer {
    let mut target = CellBuffer::empty(body);
    render_completion(&mut target, body, Theme::new(), completion);
    target
}

/// Returns one rendered row as text, without the trailing blanks.
///
/// A wide character owns two cells, and the cell buffer fills the second one
/// with a blank, so the scan skips it. The result then reads as the terminal
/// shows the row.
fn row_of(target: &CellBuffer, y: u16) -> String {
    let area = *target.area();
    let mut text = String::new();
    let mut tail = 0;
    for x in area.x..area.right() {
        let Some(cell) = target.cell((x, y)) else {
            continue;
        };
        if tail > 0 {
            tail -= 1;
            continue;
        }
        tail = text_cells(cell.symbol()).saturating_sub(1);
        text.push_str(cell.symbol());
    }
    text.trim_end().to_owned()
}

/// Returns the row of the list that carries the selection color.
fn selected_row_of(target: &CellBuffer) -> Option<u16> {
    let area = *target.area();
    let selected = Theme::new().style(ThemeRole::PopupSelection).bg;
    (area.y..area.bottom()).find(|y| {
        target
            .cell((area.x, *y))
            .is_some_and(|cell| Some(cell.bg) == selected)
    })
}

#[test]
fn the_candidate_list_bounds_its_rows_and_reports_the_hidden_candidates() {
    let body = Rect::new(0, 0, 20, 20);
    let target = draw_completion(body, &completion(&MANY));

    // The list ends at the last body row, so the statusline and the message
    // line below the body stay visible.
    let first = body.bottom() - u16::try_from(COMPLETION_ROWS_MAX).expect("the bound is small");
    let rows: Vec<String> = (first..body.bottom()).map(|y| row_of(&target, y)).collect();
    assert_eq!(rows.len(), COMPLETION_ROWS_MAX);
    // The last row reports the candidates that the bound hides, so no
    // candidate disappears without a note.
    assert_eq!(rows[COMPLETION_ROWS_MAX - 1], format!(" {OVERFLOW_NOTE}"));
    for (offset, row) in rows[..COMPLETION_ROWS_MAX - 1].iter().enumerate() {
        assert_eq!(
            row,
            &format!(" {}", MANY[offset]),
            "row {offset} of the list"
        );
    }
    // The row above the list keeps the text below it, so the list covers the
    // last rows of the body alone.
    assert_eq!(row_of(&target, first - 1), "");
}

#[test]
fn the_candidate_list_moves_its_rows_with_the_selection() {
    let body = Rect::new(0, 0, 20, 20);
    let mut open = completion(&MANY);
    let first = body.bottom() - u16::try_from(COMPLETION_ROWS_MAX).expect("the bound is small");
    assert_eq!(selected_row_of(&draw_completion(body, &open)), Some(first));

    // Seven rows hold candidates, and the eighth holds the note, so the
    // seventh cycle still reaches the last of those rows.
    for _ in 0..6 {
        open.cycle(CompletionCycle::Next);
    }
    let target = draw_completion(body, &open);
    assert_eq!(selected_row_of(&target), Some(first + 6));
    assert_eq!(row_of(&target, first), format!(" {}", MANY[0]));

    // The next cycle leaves no row for the selection, so the shown
    // candidates move instead of hiding it.
    open.cycle(CompletionCycle::Next);
    let target = draw_completion(body, &open);
    assert_eq!(selected_row_of(&target), Some(first + 6));
    assert_eq!(row_of(&target, first), format!(" {}", MANY[1]));
    assert_eq!(row_of(&target, first + 6), format!(" {}", MANY[7]));
    assert_eq!(row_of(&target, first + 7), format!(" {OVERFLOW_NOTE}"));
}

#[test]
fn the_candidate_list_clips_the_start_of_a_wide_candidate_without_splitting_it() {
    // The list keeps one cell beside its text, so a body of seven cells
    // leaves five for the candidate. The marker takes one of those cells,
    // and the wide character no longer fits in the four that remain.
    let body = Rect::new(0, 0, 7, 4);
    let target = draw_completion(body, &completion(&["\u{6e2c}\u{8a66}abc", "ab"]));
    let row = row_of(&target, body.bottom() - 2);
    // The end of the candidate always survives, because the file name at
    // the end of a path names the file that the user looks for.
    assert_eq!(row, " <abc");
    // The cell that the wide character would have split stays blank.
    let cell = target
        .cell((5, body.bottom() - 2))
        .expect("the cell is inside");
    assert_eq!(cell.symbol(), " ");
}

#[test]
fn one_candidate_opens_no_list() {
    let body = Rect::new(0, 0, 20, 20);
    let target = draw_completion(body, &completion(&["only"]));
    for y in body.y..body.bottom() {
        assert_eq!(row_of(&target, y), "", "row {y} stays empty");
    }
}

#[test]
fn the_shown_candidates_always_hold_the_selection() {
    // The window start is the pure rule behind the moving rows above, so it
    // answers every selection of one bounded list.
    for shown in 1..=4usize {
        for selected in 0..12usize {
            let first = completion_first_row(12, selected, shown);
            assert!(
                first <= selected && selected < first + shown,
                "the window [{first}, {}) holds {selected}",
                first + shown
            );
            assert!(first + shown <= 12, "the window stays inside the list");
        }
    }
}
