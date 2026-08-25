//! Tests for the shared sidebar tree indent-guide rule.

use super::*;
use crate::sidebar::{RowKind, SidebarRow};

/// One row identity that names its own path, for a readable failure message.
type RowId = &'static str;

/// Returns one row at the named depth, one terminal row tall.
fn row(name: RowId, depth: usize) -> SidebarRow<RowId> {
    SidebarRow::single(name, RowKind::Selectable).with_depth(depth)
}

/// Returns one collapsed row at the named depth.
fn collapsed_row(name: RowId, depth: usize) -> SidebarRow<RowId> {
    row(name, depth).with_collapsed(true)
}

/// Returns the representative two-directory tree that both present copies of
/// the guide rule draw the same way, with no collapsed row.
fn representative_tree() -> Vec<SidebarRow<RowId>> {
    vec![
        row("Staged", 0), // 0: the section heading, the top level of changes.rs
        row("a", 1),      // 1: a directory
        row("a/x", 2),    // 2: a file inside it
        row("a/y", 2),    // 3: the last file inside it
        row("b", 1),      // 4: the last directory of the section
        row("b/z", 2),    // 5: a file inside it, alone
    ]
}

#[test]
fn the_shared_rule_reproduces_the_changes_panel_strings() {
    // `crates/kvim-tui/src/changes.rs:92` draws these same six rows through
    // its own `row_guides`, over `1..=depth`. This test traces that algorithm
    // by hand and pins the result, so a change to the shared rule that moves
    // a trunk or an elbow fails here first.
    let rows = representative_tree();

    assert_eq!(sidebar_guides(&rows, 0), "");
    assert_eq!(sidebar_guides(&rows, 1), SIDEBAR_GUIDE_TRUNK);
    assert_eq!(
        sidebar_guides(&rows, 2),
        format!("{SIDEBAR_GUIDE_TRUNK}{SIDEBAR_GUIDE_TRUNK}"),
    );
    assert_eq!(
        sidebar_guides(&rows, 3),
        format!("{SIDEBAR_GUIDE_TRUNK}{SIDEBAR_GUIDE_ELBOW}"),
    );
    assert_eq!(sidebar_guides(&rows, 4), SIDEBAR_GUIDE_ELBOW);
    assert_eq!(
        sidebar_guides(&rows, 5),
        format!("{SIDEBAR_GUIDE_BLANK}{SIDEBAR_GUIDE_ELBOW}"),
    );
}

#[test]
fn one_leading_blank_reproduces_the_file_tree_strings() {
    // `crates/kvim-tui/src/tree.rs:1039` draws the same six rows through its
    // own `row_guides`, over `0..=depth`, with level 0 always
    // `GUIDE_BLANK` because the workspace-root header above the rows is no
    // sibling. That loop and this one agree from level 1 onward, so
    // prepending one blank reproduces the file tree's strings exactly.
    let rows = representative_tree();

    for index in 0..rows.len() {
        let file_tree_form = format!("{SIDEBAR_GUIDE_BLANK}{}", sidebar_guides(&rows, index));
        let leading_blank_and_rest: Vec<char> = file_tree_form.chars().collect();
        assert_eq!(
            leading_blank_and_rest[..SIDEBAR_GUIDE_INDENT_CELLS],
            [' ', ' '],
            "row {index} keeps the file tree's blank header level",
        );
    }
    // Traced by hand against `tree.rs`'s `0..=depth` loop, over the same
    // six rows.
    assert_eq!(
        format!("{SIDEBAR_GUIDE_BLANK}{}", sidebar_guides(&rows, 2)),
        format!("{SIDEBAR_GUIDE_BLANK}{SIDEBAR_GUIDE_TRUNK}{SIDEBAR_GUIDE_TRUNK}"),
    );
    assert_eq!(
        format!("{SIDEBAR_GUIDE_BLANK}{}", sidebar_guides(&rows, 5)),
        format!("{SIDEBAR_GUIDE_BLANK}{SIDEBAR_GUIDE_BLANK}{SIDEBAR_GUIDE_ELBOW}"),
    );
}

/// Returns the tree that the nested-collapse tests share: `p` is visible,
/// `outer` is a collapsed child of `p`, and `inner`, itself also collapsed,
/// sits inside `outer`'s already-hidden subtree. `q` is the next depth-1 row
/// past both collapses, and `after-outer` is the next depth-2 row past them.
fn tree_with_a_nested_collapse() -> Vec<SidebarRow<RowId>> {
    vec![
        row("root", 0),            // 0
        row("p", 1),               // 1
        collapsed_row("outer", 2), // 2
        collapsed_row("inner", 3), // 3, hidden below outer
        row("inner/child", 4),     // 4, hidden below inner and outer
        row("outer/second", 3),    // 5, hidden below outer alone
        row("after-outer", 2),     // 6, visible again
        row("q", 1),               // 7, visible again
    ]
}

#[test]
fn a_nested_collapse_never_blocks_the_scan_from_a_sibling_beyond_it() {
    // The scan must rise past both the outer collapse and the inner collapse
    // that sits inside it to find `q`, the next depth-1 row.
    let rows = tree_with_a_nested_collapse();

    assert_eq!(sidebar_guides(&rows, 1), SIDEBAR_GUIDE_TRUNK);
}

#[test]
fn a_collapsed_row_is_still_found_as_a_sibling_of_the_same_level() {
    // `outer` is itself collapsed, so its own guide must still show a trunk
    // when a later row, `after-outer`, sits at its exact depth. The rows
    // hidden between them, including the nested `inner` collapse, must not
    // stop the scan from reaching `after-outer`.
    let rows = tree_with_a_nested_collapse();

    assert_eq!(
        sidebar_guides(&rows, 2),
        format!("{SIDEBAR_GUIDE_TRUNK}{SIDEBAR_GUIDE_TRUNK}"),
    );
}

#[test]
fn a_collapsed_subtree_changes_no_guide_of_a_visible_row() {
    // The rows below hold one collapsed directory whose two children stay in
    // the list and stay hidden. Every hidden row sits deeper than the level
    // that a guide closes, so no guide of a visible row may move. The strings
    // below are traced by hand over three depths.
    let rows = vec![
        row("root", 0),            // 0
        row("a", 1),               // 1
        collapsed_row("a/mid", 2), // 2
        row("a/mid/x", 3),         // 3, hidden
        row("a/mid/y", 3),         // 4, hidden
        row("a/end", 2),           // 5
        row("b", 1),               // 6
    ];

    assert_eq!(sidebar_guides(&rows, 1), SIDEBAR_GUIDE_TRUNK);
    assert_eq!(
        sidebar_guides(&rows, 2),
        format!("{SIDEBAR_GUIDE_TRUNK}{SIDEBAR_GUIDE_TRUNK}"),
    );
    assert_eq!(
        sidebar_guides(&rows, 5),
        format!("{SIDEBAR_GUIDE_TRUNK}{SIDEBAR_GUIDE_ELBOW}"),
    );
}
