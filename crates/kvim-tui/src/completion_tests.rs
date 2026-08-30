use std::num::NonZeroU16;
use std::sync::Arc;

use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_ui::ListItem;
use kvim_workspace::Candidate;

use super::*;
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
    draw_completion_menu(&mut target, body, Theme::new(), completion);
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
fn command_line_file_completion_uses_the_shared_fuzzy_order() {
    let root = Arc::new(
        WorktreeRoot::open(std::env::current_dir().expect("the test has a current directory"))
            .expect("the current directory is a worktree"),
    );
    let files = [
        Candidate::file(
            &root,
            WorktreeRelativePath::new("src/session.rs").expect("the path is contained"),
        ),
        Candidate::file(
            &root,
            WorktreeRelativePath::new("src/main.rs").expect("the path is contained"),
        ),
    ];

    assert_eq!(
        command_line_candidates("edit main", &files),
        ["edit src/main.rs"]
    );
    assert_eq!(
        command_line_candidates("edit ", &files),
        ["edit src/session.rs", "edit src/main.rs"]
    );
}

#[test]
fn persistent_completion_layout_uses_candidate_rows_and_visible_width() {
    let body = Rect::new(0, 0, 28, 8);
    let open = completion(&[
        "short",
        "tiny",
        "small",
        "brief",
        "little",
        "narrow",
        "compact",
        "visible-candidate-is-wide-123",
        "last",
    ]);
    let mut viewport = ListViewport::new(7);
    viewport.scroll(
        open.candidates()
            .iter()
            .map(|_| ListItem::new(NonZeroU16::MIN)),
        7,
        true,
    );
    let layout = completion_menu_layout(body, &open, Some(&viewport))
        .expect("the listed completion has geometry");

    assert_eq!(layout.shown, 7, "the overflow note owns the eighth row");
    assert_eq!(layout.first, 2);
    assert_eq!(layout.area.height, 8);
    assert_eq!(
        layout.area.width, 28,
        "the painted wide candidate determines the interactive width"
    );
}

#[test]
fn one_row_completion_area_has_no_candidate_viewport_underflow() {
    let body = Rect::new(0, 0, 20, 1);
    let open = completion(&MANY);
    let layout = completion_menu_layout(body, &open, None)
        .expect("the overflow note can occupy the one available row");
    assert_eq!(layout.shown, 0);
    assert_eq!(layout.area.height, 1);
    let mut target = CellBuffer::empty(body);
    draw_completion_menu(&mut target, body, Theme::new(), &open);
    assert_eq!(row_of(&target, 0), format!(" {OVERFLOW_NOTE}"));
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

#[test]
fn the_completion_rejects_an_empty_list_and_refuses_a_list_above_the_bound() {
    let none = LineCompletion::open("q", Vec::new(), CHARS_MAX, CompletionCycle::Next);
    assert!(none.is_none(), "an empty completion is unrepresentable");

    // The prompt rejects a longer text, so the completion never writes one.
    let long = "a".repeat(CHARS_MAX + 1);
    let dropped = LineCompletion::open(
        "a",
        vec![long, "ab".to_owned()],
        CHARS_MAX,
        CompletionCycle::Next,
    )
    .expect("one candidate fits the bound");
    assert_eq!(dropped.selected(), "ab");
    assert_eq!(dropped.outcome(), CompletionOutcome::Completed);

    // The bound refuses rather than cuts, so no candidate ever disappears
    // without the caller learning of it.
    let full: Vec<String> = (0..COMPLETION_CANDIDATES_MAX)
        .map(|index| format!("c{index}"))
        .collect();
    let bounded = LineCompletion::open("c", full, CHARS_MAX, CompletionCycle::Next)
        .expect("a list of exactly the bound opens");
    assert_eq!(bounded.candidates().len(), COMPLETION_CANDIDATES_MAX);

    let over: Vec<String> = (0..COMPLETION_CANDIDATES_MAX + 1)
        .map(|index| format!("c{index}"))
        .collect();
    assert!(
        LineCompletion::open("c", over, CHARS_MAX, CompletionCycle::Next).is_none(),
        "a longer list is refused instead of cut"
    );
}

#[test]
fn a_host_cycles_its_own_candidates_and_a_cancel_restores_the_typed_text() {
    // The host offers candidates of its own vocabulary. They carry no prompt
    // prefix, because the prompt line of the host shows one already.
    let candidates = vec![
        "deploy".to_owned(),
        "describe".to_owned(),
        "detach".to_owned(),
    ];
    let mut menu = LineCompletion::open("de", candidates, CHARS_MAX, CompletionCycle::Next)
        .expect("the host offers three candidates");
    assert_eq!(menu.outcome(), CompletionOutcome::Listed);
    assert_eq!(menu.selected(), "deploy");
    assert_eq!(menu.selected_row(), 0);

    menu.cycle(CompletionCycle::Next);
    assert_eq!(menu.selected(), "describe");
    menu.cycle(CompletionCycle::Previous);
    menu.cycle(CompletionCycle::Previous);
    assert_eq!(
        menu.selected(),
        "detach",
        "the cycle wraps at the first row"
    );

    // Every entry names the candidate alone, so no row repeats the prefix that
    // the prompt line of the host already shows.
    for entry in menu.candidates() {
        assert!(
            !entry.starts_with(':'),
            "the entry {entry} carries the candidate alone"
        );
    }

    assert_eq!(menu.into_typed(), "de", "a cancel restores the typed text");
}

#[test]
fn the_menu_draws_the_candidate_without_the_prompt_prefix() {
    let body = Rect::new(0, 0, 20, 6);
    let target = draw_completion(body, &completion(&["write", "wqall"]));
    // The command line paints the `:` prefix, so the row above it holds the
    // candidate alone.
    assert_eq!(row_of(&target, body.bottom() - 2), " write");
    assert_eq!(row_of(&target, body.bottom() - 1), " wqall");
}
