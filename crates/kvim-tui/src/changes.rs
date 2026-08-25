//! The changed files of one review, beside the diff that shows them.
//!
//! The panel names the files of the captured candidates, not the files of the
//! live status. The panel and the diff therefore always agree, because both
//! read one value. See `docs/diff-view.md`.
//!
//! The module is pure. It reads no clock, no filesystem, and no process.

use std::ffi::OsString;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

use kvim_path::WorktreeRelativePath;
use kvim_ui::{RowKind, SidebarRow, SidebarState};

use kvim_workspace::{DiffChange, DiffContent, FileDiff, GitStatus, LineOrigin, ReviewState};

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

    /// Returns the repository state that the files of the section carry.
    ///
    /// The panel then draws the mark and the color that the file tree draws for
    /// the same state, so one reader reads one vocabulary. See `docs/git.md`.
    pub(super) const fn git_status(self) -> GitStatus {
        match self {
            Self::Staged => GitStatus::Staged,
            Self::Unstaged => GitStatus::Modified,
        }
    }
}

/// One row identity of the changes panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ChangesRow {
    /// One directory that holds changed files below it.
    ///
    /// The panel groups the changed files by directory, exactly as the file
    /// tree groups the workspace, so one reader reads one shape.
    Directory {
        /// The section that publishes the directory.
        section: ChangeSection,
        /// The directory, relative to the workspace root.
        path: PathBuf,
        /// The depth of the directory below the section.
        depth: usize,
    },
    /// One changed file of one section.
    File {
        /// The section that publishes the file.
        section: ChangeSection,
        /// The file.
        path: WorktreeRelativePath,
        /// The depth of the file below the section.
        depth: usize,
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
    ///
    /// The row names the file alone, exactly as the file tree names one entry.
    /// The directory rows above it carry the rest of the path, the mark column
    /// carries the repository state, and the header of the diff carries the
    /// counts. A truncated file names its bound, because the panel is the one
    /// place that can state it.
    pub(super) fn label(&self) -> String {
        let name = self
            .path
            .as_path()
            .file_name()
            .unwrap_or_else(|| self.path.as_path().as_os_str())
            .to_string_lossy();
        if self.truncated {
            return format!("{name} …");
        }
        name.into_owned()
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

/// Returns the rows that one section of the review publishes.
///
/// The tab strip names the section, so the panel lists the files of the active
/// section alone. The files group by directory, so the panel reads like the
/// file tree.
pub(super) fn rows(
    section: ChangeSection,
    review: Option<&ReviewState>,
) -> Vec<SidebarRow<ChangesRow>> {
    let mut rows = Vec::new();
    {
        let Some(review) = review else {
            return rows;
        };
        let files = entries(review);
        if files.is_empty() {
            return rows;
        }
        push_grouped(&mut rows, section, &files);
    }
    rows
}

/// Adds the rows of one section, grouped by the directory of each file.
///
/// The files arrive in the published order of the candidate, which sorts by
/// path, so one walk opens each directory once and closes none.
fn push_grouped(
    rows: &mut Vec<SidebarRow<ChangesRow>>,
    section: ChangeSection,
    files: &[ChangeEntry],
) {
    let mut open: Vec<OsString> = Vec::new();
    for file in files {
        let components: Vec<OsString> = file
            .path
            .as_path()
            .parent()
            .into_iter()
            .flat_map(Path::components)
            .map(|component| component.as_os_str().to_os_string())
            .collect();

        // The directories that this file does not share with the last one
        // close, and the rest open.
        let shared = open
            .iter()
            .zip(&components)
            .take_while(|(held, wanted)| held == wanted)
            .count();
        open.truncate(shared);
        for component in components.into_iter().skip(shared) {
            open.push(component);
            rows.push(
                SidebarRow::new(
                    ChangesRow::Directory {
                        section,
                        path: open.iter().collect(),
                        depth: open.len() - 1,
                    },
                    ONE_ROW,
                    RowKind::Inert,
                )
                .with_depth(open.len() - 1),
            );
        }
        rows.push(
            SidebarRow::new(
                ChangesRow::File {
                    section,
                    path: file.path.clone(),
                    depth: open.len(),
                },
                ONE_ROW,
                RowKind::Selectable,
            )
            .with_depth(open.len()),
        );
    }
}

/// Installs the rows of one section into one sidebar.
pub(super) fn refresh(
    sidebar: &mut SidebarState<ChangesRow>,
    section: ChangeSection,
    review: Option<&ReviewState>,
) {
    let rows = rows(section, review);
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
