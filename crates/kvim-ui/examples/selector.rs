//! Narrow one host-owned list to one entry with one query.
//!
//! Run it with:
//!
//! ```sh
//! cargo run -p kvim-ui --example selector
//! ```
//!
//! The selector is domain-neutral. It names no path, no buffer, and no file.
//! This host owns a board of tasks, and every candidate carries a task title
//! and the board column that holds it. The selector reads neither meaning: it
//! ranks the two strings, keeps the ranked order, and keeps the selection on
//! the same task while the query still matches it.
//!
//! The example needs no editor, no filesystem, and no terminal. It prints what
//! the selector answers and asserts every fact that it prints.

use kvim_ui::{
    ListMotion, SELECTOR_CANDIDATES_MAX, SELECTOR_QUERY_CHARS_MAX, Selector, SelectorCandidate,
};

/// The identity of one task of this host. The selector copies it and reads
/// nothing inside it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TaskId(u32);

/// What the host knows about one task. The selector reads the title and the
/// column alone, and only to rank them.
struct Task {
    id: TaskId,
    title: &'static str,
    column: &'static str,
}

/// The board of this host, in the order that the host stores it.
const BOARD: [Task; 5] = [
    Task {
        id: TaskId(1),
        title: "Review the parser",
        column: "Inbox",
    },
    Task {
        id: TaskId(2),
        title: "Rebase the release branch",
        column: "Release",
    },
    Task {
        id: TaskId(3),
        title: "Draft the release notes",
        column: "Release",
    },
    Task {
        id: TaskId(4),
        title: "Triage the crash reports",
        column: "Inbox",
    },
    Task {
        id: TaskId(5),
        title: "Rename the guide constants",
        column: "Backlog",
    },
];

fn main() {
    let mut selector = Selector::default();
    selector.set_candidates(
        BOARD
            .iter()
            .map(|task| SelectorCandidate::new(task.id, task.title, task.column))
            .collect(),
        false,
    );
    assert!(
        !selector.is_truncated(),
        "five candidates stay inside the bound"
    );

    // An empty query keeps every candidate in the order that the host stored.
    assert_eq!(selector.matches(), [0, 1, 2, 3, 4]);
    println!("the empty query keeps {} tasks", selector.matches().len());

    // Two characters already rank the list. The best match takes the
    // selection, so a reader who accepts at once accepts the top row.
    selector.set_query("re");
    print_rows(&selector);
    assert_eq!(
        selector.selected().map(SelectorCandidate::id),
        Some(&TaskId(1)),
        "the densest match of the shortest candidate ranks first"
    );

    // The reader steps down one row. The list ends at both edges, so no move
    // wraps past the best match.
    selector.select_next();
    assert_eq!(selector.selected_row(), Some(1));
    assert_eq!(
        selector.selected().map(SelectorCandidate::id),
        Some(&TaskId(2))
    );
    selector.select_previous();
    selector.select_previous();
    assert_eq!(
        selector.selected_row(),
        Some(0),
        "the first row is the top of the list"
    );
    selector.select_next();

    // One further character drops three candidates. The selected task still
    // matches, so the selection stays on it and only its row number changes.
    let before = *selector
        .selected()
        .expect("the second row is selected")
        .id();
    selector.set_query("rel");
    print_rows(&selector);
    assert_eq!(
        selector.matches().len(),
        2,
        "three candidates lost the query"
    );
    assert_eq!(
        selector.selected().map(SelectorCandidate::id),
        Some(&before),
        "the selection follows its task through the refiltering"
    );
    assert_eq!(
        selector.selected_row(),
        Some(0),
        "the same task now sits at the top row"
    );
    println!("the selection stayed on {before:?} and moved to row 0");

    show_the_bounds();
    show_the_window();
}

/// Prints the ranked rows of one selector, best row first.
fn print_rows(selector: &Selector<TaskId>) {
    println!("query {:?}", selector.query());
    for (row, index) in selector.matches().iter().enumerate() {
        let candidate = selector
            .candidate(*index)
            .expect("every match names one stored candidate");
        let mark = if selector.selected_row() == Some(row) {
            '>'
        } else {
            ' '
        };
        println!(
            "{mark} {row} {:?} {} [{}]",
            candidate.id(),
            candidate.name(),
            candidate.container()
        );
    }
}

/// Shows the two bounds that one selector enforces.
///
/// A host that offers more candidates than the bound keeps learns that the
/// list is short, so it narrows its own source instead of showing a silent
/// part of it. A query that passes the character bound keeps its first
/// characters, so the ranking cost of one keystroke stays fixed.
fn show_the_bounds() {
    let mut selector = Selector::default();
    let offered = SELECTOR_CANDIDATES_MAX + 1;
    selector.set_candidates(
        (0..offered)
            .map(|index| {
                SelectorCandidate::new(
                    TaskId(u32::try_from(index).expect("the bound stays inside u32")),
                    format!("task {index}"),
                    "Backlog",
                )
            })
            .collect(),
        false,
    );
    assert!(
        selector.is_truncated(),
        "the host offered {offered} candidates, and the bound keeps {SELECTOR_CANDIDATES_MAX}"
    );
    println!("the selector reports the truncation of {offered} candidates");

    let typed = "t".repeat(SELECTOR_QUERY_CHARS_MAX + 64);
    selector.set_query(&typed);
    assert_eq!(
        selector.query().chars().count(),
        SELECTOR_QUERY_CHARS_MAX,
        "the query keeps its first characters and nothing after them"
    );
    println!(
        "the selector clipped a {} character query to {SELECTOR_QUERY_CHARS_MAX}",
        typed.chars().count()
    );
}

/// Shows the window that a host paints without computing an offset.
///
/// A host whose overlay holds two rows and whose selector matches five reads
/// [`Selector::placements`] alone. The selector moves the window and stops it
/// at the end of the list, so the host writes no offset rule of its own.
fn show_the_window() {
    let mut selector = Selector::default();
    selector.set_candidates(
        BOARD
            .iter()
            .map(|task| SelectorCandidate::new(task.id, task.title, task.column))
            .collect(),
        false,
    );
    selector.set_height_rows(2);
    assert_eq!(selector.first_line(), 0, "the window starts at the top");

    // The window follows the selection down the list and stops at the end,
    // instead of scrolling past the last row.
    for _ in 0..BOARD.len() {
        selector.select_next();
    }
    assert_eq!(selector.selected_row(), Some(BOARD.len() - 1));
    assert_eq!(
        selector.first_line(),
        3,
        "the window of two rows stops at the end of a five row list"
    );
    println!(
        "the window stopped at row {} for {} candidates",
        selector.first_line(),
        selector.candidates_len()
    );

    // A picker jumps straight to a row, exactly as a sidebar does. Neither
    // move was reachable through `select_next` and `select_previous` alone.
    selector.apply_motion(ListMotion::ToRow(1));
    assert_eq!(
        selector.selected_row(),
        Some(1),
        "the jump lands on the named row, not a step away from it"
    );
    selector.apply_motion(ListMotion::LastRow);
    assert_eq!(selector.selected_row(), Some(BOARD.len() - 1));
    println!("a direct jump reached row {}", BOARD.len() - 1);

    // An empty list and a query that keeps nothing both show no placement,
    // but the candidate count tells the two apart.
    let empty = Selector::<u32>::default();
    assert_eq!(empty.candidates_len(), 0, "the list holds no candidate");

    let mut narrowed = Selector::default();
    narrowed.set_candidates(vec![SelectorCandidate::new(1_u32, "one", "")], false);
    narrowed.set_height_rows(2);
    narrowed.set_query("zzz");
    assert!(narrowed.matches().is_empty(), "the query keeps no row");
    assert_eq!(
        narrowed.candidates_len(),
        1,
        "one candidate exists, so the empty match list names a lost query"
    );
    println!(
        "an empty list holds {} candidates, and a lost query still holds {}",
        empty.candidates_len(),
        narrowed.candidates_len()
    );
}
