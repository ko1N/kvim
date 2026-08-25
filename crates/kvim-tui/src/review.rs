//! The open review of one captured diff.
//!
//! The surface owns the two captures that the changes panel shows, the view
//! that draws them, and the cursor that walks their hunks. It performs no Git
//! work and starts no process: the session captures and hands the candidates
//! in. See `docs/diff-view.md`.
//!
//! The module is pure. It reads no clock, no filesystem, and no process.

use kvim_input::Command;
use kvim_path::WorktreeRelativePath;
use kvim_settings::{DiffSettings, DiffView};
use kvim_ui::SidebarState;
use kvim_workspace::{HunkStep, ReviewState, WorktreeDiff};

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;

use crate::cells::clip_cells;
use crate::changes::{ChangeEntry, ChangeSection, ChangesRow, entries, refresh};
use crate::diff_view::{draw_inline_rows, draw_side_rows, inline_rows, side_rows, view_of};
use crate::theme::{Theme, ThemeRole};

/// What one review command asks the session to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ReviewOutcome {
    /// The review changed nothing that a frame shows.
    Unchanged,
    /// The review changed what a frame shows.
    Changed,
    /// The review asks the session to restore the layout that it replaced.
    Close,
    /// The review asks the session to open one file at one line.
    OpenFile {
        /// The file that the hunk at the cursor belongs to.
        path: WorktreeRelativePath,
        /// The one-based line that the hunk starts at.
        line: u32,
    },
    /// The command names no behavior of the review.
    Unhandled,
}

/// The open review of one workspace.
#[derive(Clone, Debug)]
pub(super) struct ReviewSurface {
    /// The staged half, when the capture published one.
    staged: Option<ReviewState>,
    /// The unstaged half, when the capture published one.
    unstaged: Option<ReviewState>,
    /// The section that the cursor walks.
    section: ChangeSection,
    /// The view that draws the hunks.
    view: DiffView,
    /// The rows of the changes panel.
    changes: SidebarState<ChangesRow>,
}

impl ReviewSurface {
    /// Opens one review over the two captured halves.
    ///
    /// The cursor starts in the unstaged half when it publishes a change,
    /// because that is the half a reader works on. A workspace with staged work
    /// alone starts there instead.
    pub(super) fn new(
        staged: Option<WorktreeDiff>,
        unstaged: Option<WorktreeDiff>,
        settings: DiffSettings,
        height_rows: u16,
    ) -> Self {
        let staged = staged.map(ReviewState::new);
        let unstaged = unstaged.map(ReviewState::new);
        let section = if publishes_change(unstaged.as_ref()) {
            ChangeSection::Unstaged
        } else {
            ChangeSection::Staged
        };
        let mut surface = Self {
            staged,
            unstaged,
            section,
            view: settings.view,
            changes: SidebarState::new(height_rows),
        };
        surface.refresh_changes();
        surface
    }

    /// Returns the view that draws the hunks.
    pub(super) const fn view(&self) -> DiffView {
        self.view
    }

    /// Returns the section that the cursor walks.
    pub(super) const fn section(&self) -> ChangeSection {
        self.section
    }

    /// Returns the rows of the changes panel.
    pub(super) const fn changes(&self) -> &SidebarState<ChangesRow> {
        &self.changes
    }

    /// Returns the review that the cursor walks.
    pub(super) const fn active(&self) -> Option<&ReviewState> {
        match self.section {
            ChangeSection::Staged => self.staged.as_ref(),
            ChangeSection::Unstaged => self.unstaged.as_ref(),
        }
    }

    /// Returns the review of one section.
    pub(super) const fn review(&self, section: ChangeSection) -> Option<&ReviewState> {
        match section {
            ChangeSection::Staged => self.staged.as_ref(),
            ChangeSection::Unstaged => self.unstaged.as_ref(),
        }
    }

    /// Applies one review command.
    pub(super) fn apply(&mut self, command: Command) -> ReviewOutcome {
        match command {
            Command::CloseReview => ReviewOutcome::Close,
            Command::ToggleReviewView => {
                self.view = match self.view {
                    DiffView::SideBySide => DiffView::Inline,
                    DiffView::Inline => DiffView::SideBySide,
                };
                ReviewOutcome::Changed
            }
            Command::NextHunk => self.step(ReviewState::next_hunk),
            Command::PreviousHunk => self.step(ReviewState::previous_hunk),
            Command::NextUnreadHunk => self.step(ReviewState::next_unread),
            Command::PreviousUnreadHunk => self.step(ReviewState::previous_unread),
            Command::NextChangedFile => self.walk_file(Step::Forward),
            Command::PreviousChangedFile => self.walk_file(Step::Backward),
            Command::MarkHunkRead => self.mark_read(),
            Command::OpenHunkFile => self.open_file(),
            _ => ReviewOutcome::Unhandled,
        }
    }

    /// Replaces one half with a later capture.
    ///
    /// Every read mark and the selection follow the content, so a reader keeps
    /// the marks of every hunk that the later capture still holds.
    pub(super) fn reload(&mut self, section: ChangeSection, candidate: WorktreeDiff) {
        let slot = match section {
            ChangeSection::Staged => &mut self.staged,
            ChangeSection::Unstaged => &mut self.unstaged,
        };
        match slot {
            Some(review) => {
                review.reload(candidate);
            }
            None => *slot = Some(ReviewState::new(candidate)),
        }
        self.refresh_changes();
    }

    /// Marks the hunk at the cursor as read.
    ///
    /// A hunk that already carries a mark changes nothing, so the panel counts
    /// stay as they are and the frame needs no redraw.
    fn mark_read(&mut self) -> ReviewOutcome {
        let already_read = self
            .active()
            .and_then(|review| {
                let cursor = review.cursor()?;
                Some(review.is_read(cursor.file.path(), cursor.hunk.id()))
            })
            .unwrap_or(false);
        if already_read {
            return ReviewOutcome::Unchanged;
        }
        let Some(review) = self.active_mut() else {
            return ReviewOutcome::Unchanged;
        };
        if !review.mark_read() {
            return ReviewOutcome::Unchanged;
        }
        self.refresh_changes();
        ReviewOutcome::Changed
    }

    /// Returns the file and the line that the hunk at the cursor starts at.
    ///
    /// The review cursor names one hunk, so the jump reaches the first line of
    /// that hunk. A hunk that publishes no new line, such as a complete
    /// removal, names its old side instead.
    fn open_file(&self) -> ReviewOutcome {
        let Some(cursor) = self.active().and_then(ReviewState::cursor) else {
            return ReviewOutcome::Unchanged;
        };
        let hunk = cursor.hunk;
        let line = if hunk.new_range().count() > 0 {
            hunk.new_range().first().get()
        } else {
            hunk.old_range().first().get()
        };
        ReviewOutcome::OpenFile {
            path: cursor.file.path().clone(),
            line,
        }
    }

    /// Walks one step with the named cursor motion.
    fn step<F>(&mut self, motion: F) -> ReviewOutcome
    where
        F: FnOnce(&mut ReviewState) -> HunkStep,
    {
        let Some(review) = self.active_mut() else {
            return ReviewOutcome::Unchanged;
        };
        match motion(review) {
            HunkStep::Moved => {
                self.follow_cursor();
                ReviewOutcome::Changed
            }
            HunkStep::AtBorder => ReviewOutcome::Unchanged,
        }
    }

    /// Walks to the first hunk of the next or the previous changed file.
    fn walk_file(&mut self, step: Step) -> ReviewOutcome {
        let Some(start) = self
            .active()
            .and_then(ReviewState::cursor)
            .map(|cursor| cursor.file.path().clone())
        else {
            return ReviewOutcome::Unchanged;
        };
        loop {
            let moved = match step {
                Step::Forward => self.step(ReviewState::next_hunk),
                Step::Backward => self.step(ReviewState::previous_hunk),
            };
            if moved == ReviewOutcome::Unchanged {
                return ReviewOutcome::Unchanged;
            }
            let Some(cursor) = self.active().and_then(ReviewState::cursor) else {
                return ReviewOutcome::Unchanged;
            };
            if cursor.file.path() != &start {
                return ReviewOutcome::Changed;
            }
        }
    }

    /// Returns the review that the cursor walks, as a mutable value.
    fn active_mut(&mut self) -> Option<&mut ReviewState> {
        match self.section {
            ChangeSection::Staged => self.staged.as_mut(),
            ChangeSection::Unstaged => self.unstaged.as_mut(),
        }
    }

    /// Selects the panel row of the file that the cursor names.
    fn follow_cursor(&mut self) {
        let section = self.section;
        let Some(path) = self
            .active()
            .and_then(ReviewState::cursor)
            .map(|cursor| cursor.file.path().clone())
        else {
            return;
        };
        let _ = self.changes.select(&ChangesRow::File { section, path });
    }

    /// Rebuilds the rows of the changes panel from the two halves.
    fn refresh_changes(&mut self) {
        refresh(
            &mut self.changes,
            self.staged.as_ref(),
            self.unstaged.as_ref(),
        );
        self.follow_cursor();
    }
}

/// Paints the open review into one rectangle.
///
/// The panel takes the left band and the hunks take the rest. The review draws
/// over the window tree and changes nothing of it, so leaving the review
/// restores the layout by drawing it again. See `docs/diff-view.md`.
pub(super) fn draw_review(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    settings: DiffSettings,
    review: &ReviewSurface,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let panel_width = area.width.min(CHANGES_PANEL_CELLS);
    let panel = Rect::new(area.x, area.y, panel_width, area.height);
    target.set_style(panel, theme.style(ThemeRole::DiffHeader));
    draw_changes(target, panel, theme, review);

    let body_x = area.x.saturating_add(panel_width);
    let body_width = area.width.saturating_sub(panel_width);
    if body_width == 0 {
        return;
    }
    let body = Rect::new(body_x, area.y, body_width, area.height);
    target.set_style(body, theme.style(ThemeRole::DiffContext));

    let Some(cursor) = review.active().and_then(ReviewState::cursor) else {
        return;
    };
    match view_of(settings_with(settings, review.view()), body.width) {
        DiffView::SideBySide => {
            draw_side_rows(target, body, theme, &side_rows(cursor.hunk));
        }
        DiffView::Inline => {
            draw_inline_rows(target, body, theme, &inline_rows(cursor.hunk));
        }
    }
}

/// Returns the settings that name the view that the reader selected.
///
/// The reader switches the view while the review stays open, so the drawn view
/// follows the surface and not the stored default.
const fn settings_with(settings: DiffSettings, view: DiffView) -> DiffSettings {
    DiffSettings { view, ..settings }
}

/// Paints the rows of the changes panel.
fn draw_changes(target: &mut CellBuffer, area: Rect, theme: Theme, review: &ReviewSurface) {
    let mut y = area.y;
    for row in review.changes().rows() {
        if y >= area.y.saturating_add(area.height) {
            return;
        }
        let (text, role) = match row.id() {
            // The heading of the section that the cursor walks stands out, so
            // a reader sees which half the keys act on.
            ChangesRow::Heading(section) => {
                let role = if *section == review.section() {
                    ThemeRole::DiffHeader
                } else {
                    ThemeRole::DiffGap
                };
                (section.heading().to_owned(), role)
            }
            ChangesRow::File { section, path } => {
                let entry = review
                    .review(*section)
                    .map(entries)
                    .and_then(|entries| entries.into_iter().find(|entry| &entry.path == path));
                let label = entry
                    .as_ref()
                    .map_or_else(|| path.as_path().display().to_string(), ChangeEntry::label);
                let selected = review.changes().selected() == Some(row.id());
                // A file that the reader finished dims, so the panel shows the
                // work that stays at a glance.
                let role = if selected {
                    ThemeRole::PopupSelection
                } else if entry.is_some_and(|entry| entry.is_complete()) {
                    ThemeRole::DiffGap
                } else {
                    ThemeRole::DiffContext
                };
                (label, role)
            }
        };
        target.set_stringn(
            area.x,
            y,
            clip_cells(&text, usize::from(area.width)),
            usize::from(area.width),
            theme.style(role),
        );
        y = y.saturating_add(1);
    }
}

/// The width of the changes panel, in cells.
const CHANGES_PANEL_CELLS: u16 = 34;

/// The direction of one file walk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    /// Walk towards the last file of the candidate.
    Forward,
    /// Walk towards the first file of the candidate.
    Backward,
}

/// Reports whether one review publishes at least one changed file.
fn publishes_change(review: Option<&ReviewState>) -> bool {
    review.is_some_and(|review| !review.candidate().files().is_empty())
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;
