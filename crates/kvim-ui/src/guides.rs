//! The shared indent guide rule for a tree drawn as sidebar rows.
//!
//! Kvim drew this rule twice before this module: once for the file tree and
//! once for the changes panel of the diff view. Both copies held the same
//! trunk, elbow, and blank constants and the same scan. [`sidebar_guides`] is
//! the one rule that both hosts take from now on. See `docs/windows.md`.
//!
//! The module is pure. It reads no clock, no filesystem, and no terminal. A
//! host calls [`sidebar_guides`] once for every drawn row, every frame, so
//! the scan allocates nothing beyond the returned [`String`].
//!
//! `examples/sidebar.rs` draws one tree of two sections and prints the guide of
//! every visible row:
//!
//! ```sh
//! cargo run -p kvim-ui --example sidebar
//! ```

use crate::sidebar::SidebarRow;

/// The number of cells that one guide level occupies.
pub const SIDEBAR_GUIDE_INDENT_CELLS: usize = 2;

/// The indent guide of one level that holds a further row below this row.
pub const SIDEBAR_GUIDE_TRUNK: &str = "│ ";

/// The indent guide that closes the last row of one level.
pub const SIDEBAR_GUIDE_ELBOW: &str = "└ ";

/// The indent guide of one level that holds no further row.
pub const SIDEBAR_GUIDE_BLANK: &str = "  ";

/// Returns the indent guides of one row of a tree drawn as sidebar rows.
///
/// One level that holds a further row below this row draws a trunk, and the
/// last row of a level closes it with an elbow. The rule starts at depth 1,
/// so the top-level rows of the tree carry no guide of their own.
///
/// A host whose top level sits below one further header row, such as the
/// file tree's workspace-root header, prepends one [`SIDEBAR_GUIDE_BLANK`] of
/// its own before this result. That leading blank is a fact of the host's
/// header, not of this rule, so this function never adds it. See
/// `docs/windows.md`.
///
/// A host calls this function for a visible row only, the same row that it
/// draws. A collapsed subtree then changes no guide, because every row that a
/// collapse hides sits deeper than the level that the guide closes. The call
/// allocates the returned [`String`] only; it holds no other allocation.
///
/// # Examples
///
/// ```
/// use kvim_ui::{RowKind, SIDEBAR_GUIDE_ELBOW, SIDEBAR_GUIDE_TRUNK, SidebarRow, sidebar_guides};
///
/// let rows = vec![
///     SidebarRow::single("src", RowKind::Selectable),
///     SidebarRow::single("src/lib.rs", RowKind::Selectable).with_depth(1),
///     SidebarRow::single("src/main.rs", RowKind::Selectable).with_depth(1),
/// ];
///
/// // The top-level row carries no guide.
/// assert_eq!(sidebar_guides(&rows, 0), "");
/// // A further sibling below draws a trunk, and the last sibling closes it.
/// assert_eq!(sidebar_guides(&rows, 1), SIDEBAR_GUIDE_TRUNK);
/// assert_eq!(sidebar_guides(&rows, 2), SIDEBAR_GUIDE_ELBOW);
/// ```
#[must_use]
pub fn sidebar_guides<R>(rows: &[SidebarRow<R>], index: usize) -> String {
    let Some(depth) = rows.get(index).map(SidebarRow::depth) else {
        debug_assert!(false, "the caller only reads the rows that the tree holds");
        return String::new();
    };
    let mut guides = String::with_capacity(depth * SIDEBAR_GUIDE_INDENT_CELLS);
    for level in 1..=depth {
        let segment = if level_continues(rows, index, level) {
            SIDEBAR_GUIDE_TRUNK
        } else if level == depth {
            SIDEBAR_GUIDE_ELBOW
        } else {
            SIDEBAR_GUIDE_BLANK
        };
        guides.push_str(segment);
    }
    guides
}

/// Reports whether one further row of `level` follows the row at `index`.
///
/// The scan stops at the first shallower row, which closes the level, so the
/// answer covers the siblings of one row alone.
///
/// The scan needs no visibility test. Every row of the scan holds a depth of
/// `level` or deeper, so a row that holds exactly `level` takes its parent
/// from before `index`. That parent is also an ancestor of `index`, and
/// `index` names a visible row, so the parent is open. A row that a collapsed
/// row hides always sits deeper than its ancestor, so it never holds exactly
/// `level`. A collapsed subtree therefore cannot make one level look open.
fn level_continues<R>(rows: &[SidebarRow<R>], index: usize, level: usize) -> bool {
    rows.get(index.saturating_add(1)..)
        .unwrap_or_default()
        .iter()
        .take_while(|row| row.depth() >= level)
        .any(|row| row.depth() == level)
}

#[cfg(test)]
#[path = "guides_tests.rs"]
mod tests;
