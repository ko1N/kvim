//! The open review of one captured diff.
//!
//! The surface owns the two captures that the changes panel shows, the view
//! that draws them, and the cursor that walks their hunks. It performs no Git
//! work and starts no process: the session captures and hands the candidates
//! in. See `docs/diff-view.md`.
//!
//! The module is pure. It reads no clock, no filesystem, and no process.

use std::num::NonZeroU32;

use kvim_input::Command;
use kvim_path::WorktreeRelativePath;
use kvim_settings::{DiffSettings, DiffView};
use kvim_ui::{SidebarInput, SidebarMotion, SidebarRow, SidebarState};
use kvim_workspace::{
    DiffContent, Expansion, Hunk, HunkId, HunkStep, ReviewState, WorktreeDiff, align_hunk,
};

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;

use crate::cells::clip_cells;
use crate::changes::{ChangeEntry, ChangeSection, ChangesRow, entries, refresh};
use crate::diff_view::{
    RowBand, draw_inline_rows, draw_side_rows, inline_rows, side_rows, view_of,
};
use crate::icons::{directory_icon, file_icon};
use crate::theme::{Theme, ThemeRole};
use crate::tree::TREE_INDENT_CELLS;

/// The region of the review that owns the keys.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ReviewFocus {
    /// The changes panel, which selects one changed file.
    Panel,
    /// The diff body, which scrolls the rows of the selected file.
    #[default]
    Diff,
}

/// One row of the diff body.
///
/// The body holds every published row of one file, so a reader scrolls through
/// its hunks instead of stepping between them. See `docs/diff-view.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BodyRow {
    /// The header of one hunk, which names its ranges.
    Header {
        /// The hunk that the header names.
        hunk: HunkId,
        /// The text of the header.
        text: String,
        /// Reports whether the reader marked the hunk read.
        read: bool,
    },
    /// One aligned row of one hunk.
    Line {
        /// The hunk that publishes the row.
        hunk: HunkId,
        /// The index of the row inside that hunk.
        index: usize,
    },
}

impl BodyRow {
    /// Returns the hunk that publishes the row.
    pub(super) const fn hunk(&self) -> HunkId {
        match self {
            Self::Header { hunk, .. } | Self::Line { hunk, .. } => *hunk,
        }
    }

    /// Reports whether the row is the header of its hunk.
    pub(super) const fn is_header(&self) -> bool {
        matches!(self, Self::Header { .. })
    }
}

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
    /// The region that owns the keys.
    focus: ReviewFocus,
    /// The rows of the diff body, for the selected file.
    body: Vec<BodyRow>,
    /// The file that the body rows belong to.
    ///
    /// A hunk identity is unique inside its own file alone, so the body names
    /// its file and no lookup reaches another one.
    body_path: Option<WorktreeRelativePath>,
    /// The row of the body that the cursor stands on.
    cursor: usize,
    /// The first row of the body that the viewport shows.
    first_row: usize,
    /// The number of rows that the body viewport holds.
    height_rows: usize,
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
            focus: ReviewFocus::default(),
            body: Vec::new(),
            body_path: None,
            cursor: 0,
            first_row: 0,
            height_rows: usize::from(height_rows),
        };
        surface.refresh_changes();
        surface.rebuild_body();
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

    /// Returns the region that owns the keys.
    pub(super) const fn focus(&self) -> ReviewFocus {
        self.focus
    }

    /// Returns the rows of the diff body.
    pub(super) fn body(&self) -> &[BodyRow] {
        &self.body
    }

    /// Returns the file that the body rows belong to.
    pub(super) const fn body_path(&self) -> Option<&WorktreeRelativePath> {
        self.body_path.as_ref()
    }

    /// Returns the body row that the cursor stands on.
    pub(super) const fn cursor_row(&self) -> usize {
        self.cursor
    }

    /// Returns the first body row that the viewport shows.
    pub(super) const fn first_row(&self) -> usize {
        self.first_row
    }

    /// Tells the review how many rows each region shows.
    pub(super) fn set_height_rows(&mut self, height_rows: u16) {
        self.height_rows = usize::from(height_rows);
        self.changes.set_height_rows(height_rows);
        self.reconcile_viewport();
    }

    /// Applies one review command.
    ///
    /// Every motion reaches the region that owns the keys, so the panel and the
    /// body never move together. See `docs/diff-view.md`.
    pub(super) fn apply(&mut self, command: Command, count: Option<NonZeroU32>) -> ReviewOutcome {
        let repeat = count.map_or(1, |value| value.get() as usize);
        match command {
            Command::CloseReview => ReviewOutcome::Close,
            Command::ToggleReviewView => {
                self.view = match self.view {
                    DiffView::SideBySide => DiffView::Inline,
                    DiffView::Inline => DiffView::SideBySide,
                };
                ReviewOutcome::Changed
            }
            // The panel sits at the right edge, so `Ctrl-L` reaches it and
            // `Ctrl-H` returns to the diff.
            Command::FocusWindowLeft => self.focus_region(ReviewFocus::Diff),
            Command::FocusWindowRight => self.focus_region(ReviewFocus::Panel),
            Command::MoveDown => self.move_focused(Motion::Down(repeat)),
            Command::MoveUp => self.move_focused(Motion::Up(repeat)),
            Command::MoveHalfPageDown => self.move_focused(Motion::Down(self.half_page() * repeat)),
            Command::MoveHalfPageUp => self.move_focused(Motion::Up(self.half_page() * repeat)),
            Command::MoveFullPageDown => self.move_focused(Motion::Down(self.full_page() * repeat)),
            Command::MoveFullPageUp => self.move_focused(Motion::Up(self.full_page() * repeat)),
            Command::MoveFirstLine => self.move_focused(Motion::ToRow(
                count.map_or(0, |value| value.get() as usize - 1),
            )),
            Command::MoveLastLine => self.move_focused(Motion::LastRow),
            Command::NextHunk => self.walk_hunk(Step::Forward),
            Command::PreviousHunk => self.walk_hunk(Step::Backward),
            Command::NextUnreadHunk => self.step(ReviewState::next_unread),
            Command::PreviousUnreadHunk => self.step(ReviewState::previous_unread),
            Command::NextChangedFile => self.walk_file(Step::Forward),
            Command::PreviousChangedFile => self.walk_file(Step::Backward),
            Command::MarkHunkRead => self.mark_read(),
            Command::OpenHunkFile => self.open_file(),
            _ => ReviewOutcome::Unhandled,
        }
    }

    /// Moves the focus to one region.
    fn focus_region(&mut self, focus: ReviewFocus) -> ReviewOutcome {
        if self.focus == focus {
            return ReviewOutcome::Unchanged;
        }
        self.focus = focus;
        ReviewOutcome::Changed
    }

    /// Applies one motion to the region that owns the keys.
    fn move_focused(&mut self, motion: Motion) -> ReviewOutcome {
        match self.focus {
            ReviewFocus::Panel => self.move_panel(motion),
            ReviewFocus::Diff => self.move_body(motion),
        }
    }

    /// Moves the selection of the changes panel.
    fn move_panel(&mut self, motion: Motion) -> ReviewOutcome {
        let before = self.changes.selected().cloned();
        let input = SidebarInput::Move(match motion {
            Motion::Down(rows) => SidebarMotion::Down(rows),
            Motion::Up(rows) => SidebarMotion::Up(rows),
            Motion::ToRow(row) => SidebarMotion::ToRow(row),
            Motion::LastRow => SidebarMotion::LastRow,
        });
        let _ = self.changes.reduce(&input);
        if self.changes.selected().cloned() == before {
            return ReviewOutcome::Unchanged;
        }
        // The body always shows the file that the panel names, so a selection
        // that moves takes the body with it.
        self.follow_selection();
        ReviewOutcome::Changed
    }

    /// Moves the cursor of the diff body.
    fn move_body(&mut self, motion: Motion) -> ReviewOutcome {
        if self.body.is_empty() {
            return ReviewOutcome::Unchanged;
        }
        let last = self.body.len() - 1;
        let target = match motion {
            Motion::Down(rows) => self.cursor.saturating_add(rows).min(last),
            Motion::Up(rows) => self.cursor.saturating_sub(rows),
            Motion::ToRow(row) => row.min(last),
            Motion::LastRow => last,
        };
        if target == self.cursor {
            return ReviewOutcome::Unchanged;
        }
        self.cursor = target;
        self.follow_body_cursor();
        self.reconcile_viewport();
        ReviewOutcome::Changed
    }

    /// Walks to the header of the next or the previous hunk of the body.
    fn walk_hunk(&mut self, step: Step) -> ReviewOutcome {
        let headers: Vec<usize> = self
            .body
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is_header())
            .map(|(index, _)| index)
            .collect();
        let target = match step {
            Step::Forward => headers.into_iter().find(|index| *index > self.cursor),
            Step::Backward => headers.into_iter().rev().find(|index| *index < self.cursor),
        };
        let Some(target) = target else {
            return ReviewOutcome::Unchanged;
        };
        self.cursor = target;
        self.follow_body_cursor();
        self.reconcile_viewport();
        ReviewOutcome::Changed
    }

    /// Returns the number of rows that one half page holds.
    const fn half_page(&self) -> usize {
        if self.height_rows > 1 {
            self.height_rows / 2
        } else {
            1
        }
    }

    /// Returns the number of rows that one full page holds.
    const fn full_page(&self) -> usize {
        if self.height_rows > 1 {
            self.height_rows - 1
        } else {
            1
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
        self.rebuild_body();
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
        self.mark_body_read();
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
        // The row identity carries the depth that the panel draws, so the
        // selection finds the row of the file instead of building one.
        let target = self
            .changes
            .rows()
            .iter()
            .map(SidebarRow::id)
            .find(|id| names_file(id, section, &path))
            .cloned();
        if let Some(target) = target {
            let _ = self.changes.select(&target);
        }
    }

    /// Marks the header row of the hunk at the cursor as read.
    fn mark_body_read(&mut self) {
        let Some(hunk) = self.body.get(self.cursor).map(BodyRow::hunk) else {
            return;
        };
        for row in &mut self.body {
            if let BodyRow::Header { hunk: id, read, .. } = row
                && *id == hunk
            {
                *read = true;
            }
        }
    }

    /// Rebuilds the rows of the diff body from the file at the review cursor.
    fn rebuild_body(&mut self) {
        self.body.clear();
        self.body_path = None;
        self.cursor = 0;
        self.first_row = 0;
        let Some(review) = self.active() else {
            return;
        };
        let Some(cursor) = review.cursor() else {
            return;
        };
        let path = cursor.file.path().clone();
        let wanted = cursor.hunk.id();
        let DiffContent::Text(text) = cursor.file.content() else {
            return;
        };
        let mut rows = Vec::new();
        for hunk in text.hunks() {
            let id = hunk.id();
            rows.push(BodyRow::Header {
                hunk: id,
                text: header_text(hunk),
                read: review.is_read(&path, id),
            });
            for index in 0..align_hunk(hunk).len() {
                rows.push(BodyRow::Line { hunk: id, index });
            }
        }
        self.body = rows;
        self.body_path = Some(path);
        // The cursor opens on the hunk that the review names, so a walk that
        // reaches this file lands where the reader expects.
        self.cursor = self
            .body
            .iter()
            .position(|row| row.is_header() && row.hunk() == wanted)
            .unwrap_or(0);
        self.reconcile_viewport();
    }

    /// Moves the review cursor onto the hunk that the body cursor stands in.
    fn follow_body_cursor(&mut self) {
        let Some(hunk) = self.body.get(self.cursor).map(BodyRow::hunk) else {
            return;
        };
        let Some(path) = self.body_path.clone() else {
            return;
        };
        let Some(review) = self.active_mut() else {
            return;
        };
        // A hunk identity is unique inside its own file alone. A walk over the
        // hunks would cross into another file, where the same identity names
        // another hunk, so the review places the cursor instead of walking.
        let _ = review.select_hunk(&path, hunk);
    }

    /// Shows the file that the panel selected.
    fn follow_selection(&mut self) {
        let Some(ChangesRow::File { section, path, .. }) = self.changes.selected().cloned() else {
            return;
        };
        self.section = section;
        let Some(review) = self.active_mut() else {
            return;
        };
        // The cursor reaches the named file in either direction, so a reader
        // who walks down the list and back up returns to the file they left.
        let _ = review.select_file(&path);
        self.rebuild_body();
    }

    /// Keeps the body cursor inside its viewport.
    fn reconcile_viewport(&mut self) {
        if self.body.is_empty() || self.height_rows == 0 {
            self.first_row = 0;
            return;
        }
        let last_visible = self
            .first_row
            .saturating_add(self.height_rows)
            .saturating_sub(1);
        if self.cursor < self.first_row {
            self.first_row = self.cursor;
        } else if self.cursor > last_visible {
            self.first_row = self.cursor.saturating_sub(self.height_rows - 1);
        }
        let last_start = self.body.len().saturating_sub(self.height_rows);
        self.first_row = self.first_row.min(last_start);
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
/// The panel takes the left band and the diff takes the rest. The review draws
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
    // kvim keeps its sidebar at the right edge, so the changes panel sits there
    // as well and the diff fills the rest. See `docs/windows.md`.
    let panel_width = area.width.min(CHANGES_PANEL_CELLS);
    let body_width = area.width.saturating_sub(panel_width);
    let panel = Rect::new(
        area.x.saturating_add(body_width),
        area.y,
        panel_width,
        area.height,
    );
    target.set_style(panel, theme.style(ThemeRole::Surface));
    draw_changes(target, panel, theme, review);

    if body_width == 0 {
        return;
    }
    let body = Rect::new(area.x, area.y, body_width, area.height);
    target.set_style(body, theme.style(ThemeRole::DiffContext));
    draw_body(target, body, theme, settings, review);
}

/// Paints the rows of the diff body that the viewport shows.
fn draw_body(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    settings: DiffSettings,
    review: &ReviewSurface,
) {
    // The body names its own file, so the lookup of one hunk never reaches
    // another file, whose hunks carry the same identities.
    let Some(path) = review.body_path() else {
        return;
    };
    let Some(file) = review
        .active()
        .map(ReviewState::candidate)
        .and_then(|candidate| candidate.file(path))
    else {
        return;
    };
    let DiffContent::Text(text) = file.content() else {
        return;
    };
    let view = view_of(settings_with(settings, review.view()), area.width);
    let focused = review.focus() == ReviewFocus::Diff;

    for offset in 0..usize::from(area.height) {
        let index = review.first_row().saturating_add(offset);
        let Some(row) = review.body().get(index) else {
            return;
        };
        let y = area.y + u16::try_from(offset).unwrap_or(u16::MAX);
        let line = Rect::new(area.x, y, area.width, 1);
        // The cursor row of the focused region carries the selection band over
        // its whole width, so it reads like a Visual-line selection instead of
        // a mark at one edge.
        let band = if focused && index == review.cursor_row() {
            RowBand::Selected
        } else {
            RowBand::Plain
        };
        if band == RowBand::Selected {
            target.set_style(line, theme.style(ThemeRole::PopupSelection));
        }
        match row {
            BodyRow::Header {
                text: label, read, ..
            } => {
                let mark = if *read { "read" } else { "unread" };
                let role = if *read {
                    ThemeRole::DiffGap
                } else {
                    ThemeRole::DiffHeader
                };
                let header = format!("{label}  {mark}");
                target.set_stringn(
                    line.x,
                    y,
                    clip_cells(&header, usize::from(line.width)),
                    usize::from(line.width),
                    band.apply(theme, role),
                );
            }
            BodyRow::Line {
                hunk,
                index: row_index,
            } => {
                let Some(hunk) = text.hunk(*hunk) else {
                    continue;
                };
                match view {
                    DiffView::SideBySide => {
                        let rows = side_rows(hunk);
                        if let Some(row) = rows.get(*row_index) {
                            draw_side_rows(target, line, theme, std::slice::from_ref(row), band);
                        }
                    }
                    DiffView::Inline => {
                        let rows = inline_rows(hunk);
                        if let Some(row) = rows.get(*row_index) {
                            draw_inline_rows(target, line, theme, std::slice::from_ref(row), band);
                        }
                    }
                }
            }
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
    let focused = review.focus() == ReviewFocus::Panel;
    let mut y = area.y;
    for row in review.changes().rows() {
        if y >= area.y.saturating_add(area.height) {
            return;
        }
        let selected = review.changes().selected() == Some(row.id());
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
            // A directory row carries the shape of the workspace, exactly as
            // the file tree draws it, so one reader reads one shape.
            ChangesRow::Directory { path, depth, .. } => {
                let name = path
                    .file_name()
                    .unwrap_or_else(|| path.as_os_str())
                    .to_string_lossy();
                let icon = directory_icon(Expansion::Expanded);
                let text = format!("{}{} {name}", indent_of(*depth), icon.glyph);
                (text, ThemeRole::TreeDirectory)
            }
            ChangesRow::File {
                section,
                path,
                depth,
            } => {
                let entry = review
                    .review(*section)
                    .map(entries)
                    .and_then(|entries| entries.into_iter().find(|entry| &entry.path == path));
                let name = path
                    .as_path()
                    .file_name()
                    .unwrap_or_else(|| path.as_path().as_os_str())
                    .to_string_lossy();
                let icon = file_icon(&name);
                let label = entry
                    .as_ref()
                    .map_or_else(|| name.clone().into_owned(), ChangeEntry::label);
                let label = format!("{}{} {label}", indent_of(*depth), icon.glyph);
                // The selection band marks the row of the focused panel. An
                // unfocused panel still marks its row, in a quieter role, so a
                // reader keeps the place while the keys act elsewhere.
                let role = if selected && focused {
                    ThemeRole::PopupSelection
                } else if selected {
                    ThemeRole::DiffHeader
                } else if entry.is_some_and(|entry| entry.is_complete()) {
                    ThemeRole::DiffGap
                } else {
                    ThemeRole::DiffContext
                };
                (label, role)
            }
        };
        let line = Rect::new(area.x, y, area.width, 1);
        target.set_style(line, theme.style(role));
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

/// Returns the indent of one row of the changes panel.
///
/// The panel indents by the same number of cells as the file tree, so the two
/// sidebars read as one design. See `docs/windows.md`.
fn indent_of(depth: usize) -> String {
    " ".repeat(depth.saturating_mul(TREE_INDENT_CELLS))
}

/// Reports whether one row names one changed file of one section.
fn names_file(row: &ChangesRow, section: ChangeSection, path: &WorktreeRelativePath) -> bool {
    matches!(
        row,
        ChangesRow::File {
            section: held,
            path: named,
            ..
        } if *held == section && named == path
    )
}

/// One motion of one region of the review.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Motion {
    /// Move this number of rows towards the last one.
    Down(usize),
    /// Move this number of rows towards the first one.
    Up(usize),
    /// Move to this row.
    ToRow(usize),
    /// Move to the last row.
    LastRow,
}

/// Returns the header text of one hunk.
fn header_text(hunk: &Hunk) -> String {
    let old = hunk.old_range();
    let new = hunk.new_range();
    format!(
        "@@ -{},{} +{},{} @@",
        old.first().get(),
        old.count(),
        new.first().get(),
        new.count()
    )
}

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
