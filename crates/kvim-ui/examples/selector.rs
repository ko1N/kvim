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

use kvim_ui::{SELECTOR_CANDIDATES_MAX, SELECTOR_QUERY_CHARS_MAX, Selector, SelectorCandidate};

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
