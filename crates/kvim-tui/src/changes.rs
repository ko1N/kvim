//! The changed files of one review, beside the diff that shows them.
//!
//! The panel names the files of the captured candidates, not the files of the
//! live status. The panel and the diff therefore always agree, because both
//! read one value. See `docs/diff-view.md`.
//!
//! The module is pure. It reads no clock, no filesystem, and no process.

// The session installs the review surface in a later change of this plan. The
// tests of this module already reach every value, so the expectation belongs to
// the build that holds no test.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the review surface reaches these values when the session installs it"
    )
)]

use std::num::NonZeroU16;

use kvim_path::WorktreeRelativePath;
use kvim_ui::{RowKind, SidebarRow, SidebarState};
use kvim_workspace::{DiffChange, DiffContent, FileDiff, LineOrigin, ReviewState};

/// The two sections that the panel shows.
///
/// Each section holds its own capture, so the panel never merges the staged
/// half with the unstaged half. See `docs/git.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChangeSection {
    /// The changes that the index holds.
    Staged,
    /// The changes that the working tree holds beyond the index.
    Unstaged,
}

impl ChangeSection {
    /// Returns the heading of the section.
    pub(super) const fn heading(self) -> &'static str {
        match self {
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
        }
    }
}

/// One row identity of the changes panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ChangesRow {
    /// The heading of one section, which takes no selection.
    Heading(ChangeSection),
    /// One changed file of one section.
    File {
        /// The section that publishes the file.
        section: ChangeSection,
        /// The file.
        path: WorktreeRelativePath,
    },
}

/// One changed file of one section, with the counts that its row shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ChangeEntry {
    /// The path that the row names.
    pub(super) path: WorktreeRelativePath,
    /// The marker of the change kind.
    pub(super) mark: char,
    /// The number of added lines that the candidate published.
    pub(super) added: usize,
    /// The number of removed lines that the candidate published.
    pub(super) removed: usize,
    /// The number of published hunks that stay unread.
    pub(super) unread: usize,
    /// One bound stopped the collection of this file.
    pub(super) truncated: bool,
}

impl ChangeEntry {
    /// Reports whether the reader finished every published hunk of the file.
    ///
    /// A truncated file never reads as complete, because the candidate holds
    /// content that the reader cannot reach.
    pub(super) const fn is_complete(&self) -> bool {
        self.unread == 0 && !self.truncated
    }

    /// Returns the row text of the entry.
    pub(super) fn label(&self) -> String {
        let name = self.path.as_path().display();
        let counts = format!("+{} -{}", self.added, self.removed);
        let state = if self.truncated {
            " …".to_owned()
        } else if self.unread == 0 {
            String::new()
        } else {
            format!(" ({} unread)", self.unread)
        };
        format!("{} {name}  {counts}{state}", self.mark)
    }
}

/// Returns one entry for each changed file of one review.
pub(super) fn entries(review: &ReviewState) -> Vec<ChangeEntry> {
    review
        .candidate()
        .files()
        .iter()
        .map(|file| entry(review, file))
        .collect()
}

/// Returns the rows that one pair of reviews publishes.
///
/// A section without a review publishes no heading at all, so a workspace with
/// nothing staged shows one section instead of an empty one.
pub(super) fn rows(
    staged: Option<&ReviewState>,
    unstaged: Option<&ReviewState>,
) -> Vec<SidebarRow<ChangesRow>> {
    let mut rows = Vec::new();
    for (section, review) in [
        (ChangeSection::Staged, staged),
        (ChangeSection::Unstaged, unstaged),
    ] {
        let Some(review) = review else {
            continue;
        };
        let files = entries(review);
        if files.is_empty() {
            continue;
        }
        rows.push(SidebarRow::new(
            ChangesRow::Heading(section),
            ONE_ROW,
            RowKind::Inert,
        ));
        for file in files {
            rows.push(SidebarRow::new(
                ChangesRow::File {
                    section,
                    path: file.path,
                },
                ONE_ROW,
                RowKind::Selectable,
            ));
        }
    }
    rows
}

/// Installs the rows of one pair of reviews into one sidebar.
pub(super) fn refresh(
    sidebar: &mut SidebarState<ChangesRow>,
    staged: Option<&ReviewState>,
    unstaged: Option<&ReviewState>,
) {
    let rows = rows(staged, unstaged);
    // Every row holds one terminal row, so the sidebar accepts the list
    // whenever the panel built it.
    let _ = sidebar.set_rows(rows);
}

/// The height of every row of the panel.
const ONE_ROW: NonZeroU16 = NonZeroU16::new(1).expect("the literal one is not zero");

/// Returns the entry of one changed file.
fn entry(review: &ReviewState, file: &FileDiff) -> ChangeEntry {
    let (added, removed, truncated) = match file.content() {
        DiffContent::Text(text) => {
            let mut added = 0;
            let mut removed = 0;
            for hunk in text.hunks() {
                for line in hunk.lines() {
                    match line.origin() {
                        LineOrigin::Added { .. } => added += 1,
                        LineOrigin::Removed { .. } => removed += 1,
                        LineOrigin::Context { .. } => {}
                    }
                }
            }
            (added, removed, text.truncation().is_truncated())
        }
        // A file without text publishes no line to count and no hunk to read.
        _ => (0, 0, false),
    };
    ChangeEntry {
        path: file.path().clone(),
        mark: change_mark(file.change()),
        added,
        removed,
        unread: review.unread_hunks(file.path()),
        truncated,
    }
}

/// Returns the marker of one change kind.
fn change_mark(change: &DiffChange) -> char {
    match change {
        DiffChange::Added { .. } => 'A',
        DiffChange::Deleted { .. } => 'D',
        DiffChange::Modified { .. } => 'M',
        DiffChange::Renamed { .. } => 'R',
    }
}

#[cfg(test)]
#[path = "changes_tests.rs"]
mod tests;
