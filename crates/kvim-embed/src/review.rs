//! Pure standalone review over immutable host-supplied candidates.

use std::collections::VecDeque;
use std::num::NonZeroU32;

use kvim_input::Command;
use kvim_path::WorktreeRelativePath;
use kvim_settings::{DiffSettings, DiffView};
use kvim_tui::__review::{
    ReviewFocus as PrivateFocus, ReviewModel, ReviewOutcome, ReviewPainter, Theme,
};
use kvim_workspace::{
    CandidateAuthority, CommentBody as PrivateCommentBody, DIFF_FILE_HUNKS_MAX, DIFF_FILES_MAX,
    DIFF_HUNK_LINES_MAX, DIFF_LINE_BYTES_MAX, DIFF_LINE_NUMBER_MAX, DiffChange, DiffContent,
    DiffOldSide, DiffTarget, DiffTruncation, FileDiff as PrivateFile, FileMode, FileSide,
    HeadAuthority, Hunk, HunkId, IndexAuthority, LineEnding, LineOrigin, NewLine, NewLineRange,
    OldLine, OldLineRange, ReviewAnchor as PrivateAnchor, TextDiff, WorktreeDiff,
};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
};
use thiserror::Error;

/// Maximum bytes in a supplied candidate identity.
pub const REVIEW_CANDIDATE_ID_BYTES_MAX: usize = 128;
/// Maximum queued review events.
pub const REVIEW_EVENTS_MAX: usize = 64;
/// Maximum anchors in a persisted review snapshot.
pub const REVIEW_SNAPSHOT_ANCHORS_MAX: usize =
    REVIEW_CANDIDATES_MAX * REVIEW_FILES_MAX * REVIEW_FILE_HUNKS_MAX + 1;
/// Maximum bytes in the review panel heading.
pub const REVIEW_ROOT_LABEL_BYTES_MAX: usize = 256;
/// Maximum candidates in one supplied review.
pub const REVIEW_CANDIDATES_MAX: usize = 2;
/// Minimum restored review panel width in terminal cells.
const REVIEW_PANEL_CELLS_MIN: u16 = 16;
/// Maximum restored review panel width in terminal cells.
const REVIEW_PANEL_CELLS_MAX: u16 = 80;
/// Maximum files in one supplied candidate.
pub const REVIEW_FILES_MAX: usize = DIFF_FILES_MAX;
/// Maximum hunks in one supplied file.
pub const REVIEW_FILE_HUNKS_MAX: usize = DIFF_FILE_HUNKS_MAX;
/// Maximum lines in one supplied hunk.
pub const REVIEW_HUNK_LINES_MAX: usize = DIFF_HUNK_LINES_MAX;

/// One section of a standalone review.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReviewSection {
    /// Changes that a host classifies as staged.
    Staged,
    /// Changes that a host classifies as unstaged.
    Unstaged,
}

/// One immutable host identity for a supplied candidate.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReviewCandidateId(Box<str>);
impl ReviewCandidateId {
    /// Validates and owns a nonempty bounded identity.
    pub fn new(value: &str) -> Result<Self, ReviewError> {
        if value.is_empty() || value.len() > REVIEW_CANDIDATE_ID_BYTES_MAX {
            return Err(ReviewError::CandidateIdentity);
        }
        Ok(Self(value.into()))
    }
    /// Returns the host-supplied identity.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        &self.0
    }
}

/// The origin and line number of one supplied line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewLineOrigin {
    /// An unchanged line that exists on both sides.
    Context {
        /// The old-side line number.
        old: u32,
        /// The new-side line number.
        new: u32,
    },
    /// A line that exists only on the old side.
    Removed {
        /// The old-side line number.
        old: u32,
    },
    /// A line that exists only on the new side.
    Added {
        /// The new-side line number.
        new: u32,
    },
}

/// One bounded line of a supplied diff.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewLine {
    origin: ReviewLineOrigin,
    text: Box<str>,
    final_line: bool,
}
impl ReviewLine {
    /// Validates and owns one bounded diff line.
    pub fn new(origin: ReviewLineOrigin, text: &str) -> Result<Self, ReviewError> {
        if text.len() > DIFF_LINE_BYTES_MAX || text.as_bytes().contains(&b'\n') {
            return Err(ReviewError::Candidate("invalid diff line text".into()));
        }
        validate_origin(origin)?;
        Ok(Self {
            origin,
            text: text.into(),
            final_line: false,
        })
    }
    /// Marks this line as the final line without a line feed.
    #[must_use]
    pub fn without_line_ending(mut self) -> Self {
        self.final_line = true;
        self
    }
    /// Returns the origin and line numbers.
    #[must_use]
    pub const fn origin(&self) -> ReviewLineOrigin {
        self.origin
    }
    /// Returns the line text.
    #[must_use]
    pub const fn text(&self) -> &str {
        &self.text
    }
}

/// One supplied hunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewHunk {
    old_first: u32,
    old_count: u32,
    new_first: u32,
    new_count: u32,
    lines: Vec<ReviewLine>,
}
impl ReviewHunk {
    /// Validates and owns one bounded hunk.
    pub fn new(
        old_first: u32,
        old_count: u32,
        new_first: u32,
        new_count: u32,
        lines: &[ReviewLine],
    ) -> Result<Self, ReviewError> {
        validate_range(old_first, old_count)?;
        validate_range(new_first, new_count)?;
        if lines.is_empty() || lines.len() > REVIEW_HUNK_LINES_MAX {
            return Err(ReviewError::Candidate("invalid hunk line count".into()));
        }
        validate_hunk_lines(old_first, old_count, new_first, new_count, &lines)?;
        Ok(Self {
            old_first,
            old_count,
            new_first,
            new_count,
            lines: lines.to_vec(),
        })
    }
    /// Returns the supplied lines.
    #[must_use]
    pub fn lines(&self) -> &[ReviewLine] {
        &self.lines
    }
}

/// The change kind of one supplied file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewFileChange {
    /// A file added on the new side.
    Added,
    /// A file removed from the old side.
    Deleted,
    /// A file changed at the same path.
    Modified,
    /// A file moved from `old_path`.
    Renamed {
        /// The old validated facade-supported path.
        old_path: WorktreeRelativePath,
    },
}

/// One bounded file in a supplied candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewFile {
    path: WorktreeRelativePath,
    change: ReviewFileChange,
    hunks: Vec<ReviewHunk>,
}
impl ReviewFile {
    /// Validates and owns one text file diff.
    pub fn new(
        path: WorktreeRelativePath,
        change: ReviewFileChange,
        hunks: &[ReviewHunk],
    ) -> Result<Self, ReviewError> {
        if hunks.len() > REVIEW_FILE_HUNKS_MAX {
            return Err(ReviewError::Candidate("too many hunks in one file".into()));
        }
        Ok(Self {
            path,
            change,
            hunks: hunks.to_vec(),
        })
    }
    /// Returns the validated candidate path.
    #[must_use]
    pub const fn path(&self) -> &WorktreeRelativePath {
        &self.path
    }
    /// Returns the hunks.
    #[must_use]
    pub fn hunks(&self) -> &[ReviewHunk] {
        &self.hunks
    }
}

/// One immutable supplied candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewCandidate {
    id: ReviewCandidateId,
    section: ReviewSection,
    files: Vec<ReviewFile>,
}
impl ReviewCandidate {
    /// Validates and owns one candidate.
    pub fn new(
        id: ReviewCandidateId,
        section: ReviewSection,
        files: &[ReviewFile],
    ) -> Result<Self, ReviewError> {
        if files.len() > REVIEW_FILES_MAX {
            return Err(ReviewError::Candidate("too many files".into()));
        }
        Ok(Self {
            id,
            section,
            files: files.to_vec(),
        })
    }
    /// Returns the immutable host identity.
    #[must_use]
    pub const fn id(&self) -> &ReviewCandidateId {
        &self.id
    }
    /// Returns the candidate section.
    #[must_use]
    pub const fn section(&self) -> ReviewSection {
        self.section
    }
    /// Returns the changed files.
    #[must_use]
    pub fn files(&self) -> &[ReviewFile] {
        &self.files
    }
}

/// Standalone review presentation configuration.
#[derive(Clone, Debug)]
pub struct ReviewConfig {
    area: Rect,
    root_label: Box<str>,
    diff: DiffSettings,
    resize_step_cells: u16,
}
impl ReviewConfig {
    /// Creates configuration for one caller-owned rectangle.
    #[must_use]
    pub fn new(area: Rect) -> Self {
        Self {
            area,
            root_label: "Review".into(),
            diff: DiffSettings::default(),
            resize_step_cells: 6,
        }
    }
    /// Sets and owns the bounded panel heading.
    pub fn with_root_label(mut self, label: &str) -> Result<Self, ReviewError> {
        if label.len() > REVIEW_ROOT_LABEL_BYTES_MAX {
            return Err(ReviewError::RootLabelCapacity);
        }
        self.root_label = label.into();
        Ok(self)
    }
    /// Selects the initial view.
    #[must_use]
    pub fn view(mut self, view: DiffView) -> Self {
        self.diff.view = view;
        self
    }
}

/// One semantic standalone review command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewCommand {
    /// Moves down in the focused region.
    MoveDown,
    /// Moves up in the focused region.
    MoveUp,
    /// Selects the next hunk.
    NextHunk,
    /// Selects the previous hunk.
    PreviousHunk,
    /// Selects the next unread hunk.
    NextUnread,
    /// Selects the previous unread hunk.
    PreviousUnread,
    /// Selects the next changed file.
    NextFile,
    /// Selects the previous changed file.
    PreviousFile,
    /// Selects the next candidate section.
    NextSection,
    /// Selects the previous candidate section.
    PreviousSection,
    /// Moves to the first item.
    First,
    /// Moves to the last item.
    Last,
    /// Moves down by half a page.
    HalfPageDown,
    /// Moves up by half a page.
    HalfPageUp,
    /// Moves down by one page.
    FullPageDown,
    /// Moves up by one page.
    FullPageUp,
    /// Focuses the diff body.
    FocusDiff,
    /// Focuses the changed-file panel.
    FocusPanel,
    /// Widens the changed-file panel.
    WidenPanel,
    /// Narrows the changed-file panel.
    NarrowPanel,
    /// Toggles inline and side-by-side views.
    ToggleView,
    /// Marks the selected hunk as read.
    MarkRead,
    /// Requests that the host open the selected file.
    OpenFile,
    /// Submits one neutral comment event.
    SubmitComment(ReviewCommentBody),
    /// Requests that the host close the review.
    Close,
}

/// One command with an optional repeat count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewInput {
    command: ReviewCommand,
    count: Option<NonZeroU32>,
}
impl ReviewInput {
    /// Creates one uncounted command.
    #[must_use]
    pub const fn command(command: ReviewCommand) -> Self {
        Self {
            command,
            count: None,
        }
    }
    /// Adds a nonzero repeat count.
    #[must_use]
    pub const fn with_count(mut self, count: NonZeroU32) -> Self {
        self.count = Some(count);
        self
    }
}

/// Bounded neutral review comment text.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReviewCommentBody(PrivateCommentBody);
impl ReviewCommentBody {
    /// Validates and owns nonempty bounded comment text.
    pub fn new(text: &str) -> Result<Self, ReviewError> {
        PrivateCommentBody::new(text)
            .map(Self)
            .map_err(|_| ReviewError::Comment)
    }
    /// Returns the comment text.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// A facade-owned durable anchor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewAnchor(PrivateAnchor);
impl ReviewAnchor {
    /// Returns the validated candidate path without losing platform identity.
    #[must_use]
    pub const fn path(&self) -> &WorktreeRelativePath {
        self.0.path()
    }
}

/// One event for host-owned persistence or navigation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewEvent {
    /// Requests a host redraw.
    Redraw,
    /// Reports a changed set of read marks.
    ReadStateChanged,
    /// Publishes one bounded comment for host persistence.
    CommentSubmitted {
        /// The durable neutral target.
        anchor: ReviewAnchor,
        /// The bounded neutral comment text.
        body: ReviewCommentBody,
    },
    /// Requests that the host open one contained candidate path.
    OpenFile {
        /// The validated facade-supported relative path.
        path: WorktreeRelativePath,
        /// The one-based target line.
        line: u32,
    },
    /// Reports one missing or ambiguous snapshot anchor.
    StaleSnapshotAnchor,
    /// Reports that changed candidate identities reset prior review state.
    ReplacedCandidate,
    /// Requests that the host close the surface.
    Close,
}

/// The result of reducing one input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewUpdate {
    /// The input changed no visible state.
    Unchanged,
    /// The input changed visible state.
    Changed,
    /// The input published a host event only.
    Event,
}

/// Placement facts from one render.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReviewRenderOutcome {
    cursor: Option<Position>,
}
impl ReviewRenderOutcome {
    /// The review paints its active row as cell styling and owns no terminal
    /// cursor. This value is therefore `None` for supplied review surfaces.
    #[must_use]
    pub const fn cursor(self) -> Option<Position> {
        self.cursor
    }
}

/// The focused review region in a snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewFocus {
    /// The diff body owns input.
    Diff,
    /// The changed-file panel owns input.
    Panel,
}

/// Bounded state that can outlive a review surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSnapshot {
    staged_id: Option<ReviewCandidateId>,
    unstaged_id: Option<ReviewCandidateId>,
    staged_read: Vec<ReviewAnchor>,
    unstaged_read: Vec<ReviewAnchor>,
    cursor: Option<(ReviewSection, ReviewAnchor)>,
    focus: ReviewFocus,
    view: DiffView,
    panel_cells: u16,
}
impl ReviewSnapshot {
    /// Validates one facade-owned durable snapshot.
    pub fn new(
        staged_id: Option<ReviewCandidateId>,
        unstaged_id: Option<ReviewCandidateId>,
        staged_read: Vec<ReviewAnchor>,
        unstaged_read: Vec<ReviewAnchor>,
        cursor: Option<(ReviewSection, ReviewAnchor)>,
        focus: ReviewFocus,
        view: DiffView,
        panel_cells: u16,
    ) -> Result<Self, ReviewError> {
        let snapshot = Self {
            staged_id,
            unstaged_id,
            staged_read,
            unstaged_read,
            cursor,
            focus,
            view,
            panel_cells,
        };
        if snapshot.anchor_count() > REVIEW_SNAPSHOT_ANCHORS_MAX {
            return Err(ReviewError::SnapshotCapacity);
        }
        if !(REVIEW_PANEL_CELLS_MIN..=REVIEW_PANEL_CELLS_MAX).contains(&snapshot.panel_cells) {
            return Err(ReviewError::SnapshotPanelWidth);
        }
        Ok(snapshot)
    }

    /// Returns the staged candidate identity.
    #[must_use]
    pub const fn staged_id(&self) -> Option<&ReviewCandidateId> {
        self.staged_id.as_ref()
    }

    /// Returns the unstaged candidate identity.
    #[must_use]
    pub const fn unstaged_id(&self) -> Option<&ReviewCandidateId> {
        self.unstaged_id.as_ref()
    }

    /// Returns the staged read anchors.
    #[must_use]
    pub fn staged_read(&self) -> &[ReviewAnchor] {
        &self.staged_read
    }

    /// Returns the unstaged read anchors.
    #[must_use]
    pub fn unstaged_read(&self) -> &[ReviewAnchor] {
        &self.unstaged_read
    }

    /// Returns the cursor section and durable anchor.
    #[must_use]
    pub fn cursor(&self) -> Option<(ReviewSection, &ReviewAnchor)> {
        self.cursor
            .as_ref()
            .map(|(section, anchor)| (*section, anchor))
    }

    /// Returns the focused region.
    #[must_use]
    pub const fn focus(&self) -> ReviewFocus {
        self.focus
    }

    /// Returns the selected diff view.
    #[must_use]
    pub const fn view(&self) -> DiffView {
        self.view
    }

    /// Returns the requested panel width in terminal cells.
    #[must_use]
    pub const fn panel_cells(&self) -> u16 {
        self.panel_cells
    }

    /// Returns the number of stored anchors.
    #[must_use]
    pub fn anchor_count(&self) -> usize {
        self.staged_read.len() + self.unstaged_read.len() + usize::from(self.cursor.is_some())
    }
}

/// A supplied-review contract failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ReviewError {
    /// The configured rectangle has no cells.
    #[error("the review rectangle must have nonzero width and height")]
    EmptyGeometry,
    /// The configured rectangle does not fit the target buffer.
    #[error("the review rectangle is outside the cell buffer")]
    GeometryOutsideBuffer,
    /// A candidate identity is empty or too large.
    #[error(
        "a candidate identity must be nonempty and at most {REVIEW_CANDIDATE_ID_BYTES_MAX} bytes"
    )]
    CandidateIdentity,
    /// The supplied collection has more than two sections.
    #[error("a review accepts at most {REVIEW_CANDIDATES_MAX} candidates")]
    CandidateCapacity,
    /// Two candidates name the same section.
    #[error("the candidate sections must be unique")]
    DuplicateSection,
    /// The panel heading is too large.
    #[error("the review panel heading exceeds {REVIEW_ROOT_LABEL_BYTES_MAX} bytes")]
    RootLabelCapacity,
    /// A supplied diff value violates a candidate invariant.
    #[error("the supplied candidate is invalid: {0}")]
    Candidate(Box<str>),
    /// A comment body is empty or too large.
    #[error("the comment body is empty or too large")]
    Comment,
    /// A transition cannot reserve all required host events.
    #[error("the event queue is full")]
    EventCapacity,
    /// A snapshot exceeds its anchor bound.
    #[error("the snapshot has too many anchors")]
    SnapshotCapacity,
    /// A snapshot panel width is outside the supported range.
    #[error("the snapshot panel width is outside the supported range")]
    SnapshotPanelWidth,
    /// A snapshot names different candidate identities.
    #[error("the snapshot candidate identity does not match this review")]
    SnapshotCandidate,
}

/// A pure rendered review over host-supplied immutable candidates.
pub struct ReviewSurface {
    model: ReviewModel,
    config: ReviewConfig,
    staged_id: Option<ReviewCandidateId>,
    unstaged_id: Option<ReviewCandidateId>,
    events: VecDeque<ReviewEvent>,
}
impl ReviewSurface {
    /// Creates a standalone review without filesystem, Git, process, editor, watcher, clipboard, language, or runtime work.
    ///
    /// ```
    /// use kvim_embed::{ReviewCandidate, ReviewCandidateId, ReviewConfig, ReviewSection, ReviewSurface};
    /// use ratatui::layout::Rect;
    /// let candidate = ReviewCandidate::new(
    ///     ReviewCandidateId::new("patch-1")?,
    ///     ReviewSection::Unstaged,
    ///     &[],
    /// )?;
    /// let mut review = ReviewSurface::from_candidates(&[candidate], ReviewConfig::new(Rect::new(0, 0, 40, 12)))?;
    /// assert_eq!(review.snapshot().anchor_count(), 0);
    /// # Ok::<(), kvim_embed::ReviewError>(())
    /// ```
    pub fn from_candidates(
        candidates: &[ReviewCandidate],
        config: ReviewConfig,
    ) -> Result<Self, ReviewError> {
        if config.area.width == 0 || config.area.height == 0 {
            return Err(ReviewError::EmptyGeometry);
        }
        if candidates.len() > REVIEW_CANDIDATES_MAX {
            return Err(ReviewError::CandidateCapacity);
        }
        if config.root_label.len() > REVIEW_ROOT_LABEL_BYTES_MAX {
            return Err(ReviewError::RootLabelCapacity);
        }
        validate_candidates(&candidates)?;
        let mut staged = None;
        let mut unstaged = None;
        let mut staged_id = None;
        let mut unstaged_id = None;
        for candidate in candidates.iter().cloned() {
            let id = candidate.id.clone();
            let section = candidate.section;
            let converted = convert_candidate(candidate)?;
            let (slot, id_slot) = match section {
                ReviewSection::Staged => (&mut staged, &mut staged_id),
                ReviewSection::Unstaged => (&mut unstaged, &mut unstaged_id),
            };
            if slot.is_some() {
                return Err(ReviewError::DuplicateSection);
            }
            *slot = Some(converted);
            *id_slot = Some(id);
        }
        let model = ReviewModel::new(
            staged,
            unstaged,
            config.diff.clone(),
            config.resize_step_cells,
            config.area.height,
        );
        Ok(Self {
            model,
            config,
            staged_id,
            unstaged_id,
            events: VecDeque::with_capacity(REVIEW_EVENTS_MAX),
        })
    }

    /// Reduces one semantic input.
    pub fn input(&mut self, input: ReviewInput) -> Result<ReviewUpdate, ReviewError> {
        if let ReviewCommand::SubmitComment(body) = input.command {
            let Some(anchor) = self.model.cursor_anchor() else {
                return Ok(ReviewUpdate::Unchanged);
            };
            self.reserve_events(1)?;
            self.events.push_back(ReviewEvent::CommentSubmitted {
                anchor: ReviewAnchor(anchor),
                body,
            });
            return Ok(ReviewUpdate::Event);
        }
        let mut staged = self.model.clone();
        let marks_before: Vec<_> = staged.read_anchors().to_vec();
        let command = map_command(&input.command);
        match staged.apply(command, input.count) {
            ReviewOutcome::Unchanged | ReviewOutcome::Unhandled => Ok(ReviewUpdate::Unchanged),
            ReviewOutcome::Changed => {
                let read_changed = staged.read_anchors() != marks_before;
                self.reserve_events(1 + usize::from(read_changed))?;
                self.model = staged;
                if read_changed {
                    self.events.push_back(ReviewEvent::ReadStateChanged);
                }
                self.events.push_back(ReviewEvent::Redraw);
                Ok(ReviewUpdate::Changed)
            }
            ReviewOutcome::Close => {
                self.reserve_events(1)?;
                self.events.push_back(ReviewEvent::Close);
                Ok(ReviewUpdate::Event)
            }
            ReviewOutcome::OpenFile { path, line } => {
                self.reserve_events(1)?;
                self.events.push_back(ReviewEvent::OpenFile { path, line });
                Ok(ReviewUpdate::Event)
            }
        }
    }

    /// Renders into a caller-owned cell buffer.
    pub fn render(&self, target: &mut Buffer) -> Result<ReviewRenderOutcome, ReviewError> {
        if !target
            .area
            .contains(Position::new(self.config.area.x, self.config.area.y))
            || !target.area.contains(Position::new(
                self.config.area.right() - 1,
                self.config.area.bottom() - 1,
            ))
        {
            return Err(ReviewError::GeometryOutsideBuffer);
        }
        ReviewPainter::new(
            Theme::default(),
            self.config.diff.clone(),
            &self.config.root_label,
        )
        .draw(target, self.config.area, &self.model);
        Ok(ReviewRenderOutcome { cursor: None })
    }

    /// Replaces supplied candidates.
    ///
    /// Matching identities relocate durable state. Changed identities replace
    /// the logical candidates and reset state safely.
    pub fn reload(&mut self, candidates: &[ReviewCandidate]) -> Result<ReviewUpdate, ReviewError> {
        let snapshot = self.snapshot();
        let mut replacement = Self::from_candidates(candidates, self.config.clone())?;
        if replacement.staged_id == snapshot.staged_id
            && replacement.unstaged_id == snapshot.unstaged_id
        {
            let update = replacement.restore(&snapshot)?;
            *self = replacement;
            return Ok(update);
        }
        replacement.reserve_events(2)?;
        replacement.events.push_back(ReviewEvent::ReplacedCandidate);
        replacement.events.push_back(ReviewEvent::Redraw);
        *self = replacement;
        Ok(ReviewUpdate::Changed)
    }

    /// Returns the next host event.
    pub fn event(&mut self) -> Option<ReviewEvent> {
        self.events.pop_front()
    }

    /// Exports bounded durable review state.
    #[must_use]
    pub fn snapshot(&self) -> ReviewSnapshot {
        let staged_read = self
            .model
            .section_read_anchors(true)
            .iter()
            .cloned()
            .map(ReviewAnchor)
            .collect();
        let unstaged_read = self
            .model
            .section_read_anchors(false)
            .iter()
            .cloned()
            .map(ReviewAnchor)
            .collect();
        ReviewSnapshot {
            staged_id: self.staged_id.clone(),
            unstaged_id: self.unstaged_id.clone(),
            staged_read,
            unstaged_read,
            cursor: self.model.cursor_anchor().map(|anchor| {
                let section = if self.model.section_is_staged() {
                    ReviewSection::Staged
                } else {
                    ReviewSection::Unstaged
                };
                (section, ReviewAnchor(anchor))
            }),
            focus: match self.model.focus() {
                PrivateFocus::Diff => ReviewFocus::Diff,
                PrivateFocus::Panel => ReviewFocus::Panel,
            },
            view: self.model.view(),
            panel_cells: self.model.panel_cells(),
        }
    }

    /// Restores a snapshot after validating bounds and logical candidate identities.
    pub fn restore(&mut self, snapshot: &ReviewSnapshot) -> Result<ReviewUpdate, ReviewError> {
        if snapshot.anchor_count() > REVIEW_SNAPSHOT_ANCHORS_MAX {
            return Err(ReviewError::SnapshotCapacity);
        }
        if snapshot.staged_id != self.staged_id || snapshot.unstaged_id != self.unstaged_id {
            return Err(ReviewError::SnapshotCandidate);
        }
        let staged: Vec<_> = snapshot.staged_read.iter().map(|a| a.0.clone()).collect();
        let unstaged: Vec<_> = snapshot.unstaged_read.iter().map(|a| a.0.clone()).collect();
        let mut staged_model = self.model.clone();
        let mut stale_events = 0_usize;
        staged_model.restore_presentation(
            match snapshot.focus {
                ReviewFocus::Diff => PrivateFocus::Diff,
                ReviewFocus::Panel => PrivateFocus::Panel,
            },
            snapshot.view,
            snapshot.panel_cells,
        );
        if let Some((section, anchor)) = &snapshot.cursor {
            if !staged_model
                .restore_cursor_anchor(matches!(section, ReviewSection::Staged), &anchor.0)
            {
                stale_events += 1;
            }
        }
        let result = staged_model.restore_read_anchors(&staged, &unstaged);
        if result.stale > 0 {
            stale_events += 1;
        }
        self.reserve_events(stale_events + 1)?;
        self.model = staged_model;
        for _ in 0..stale_events {
            self.events.push_back(ReviewEvent::StaleSnapshotAnchor);
        }
        self.events.push_back(ReviewEvent::Redraw);
        Ok(ReviewUpdate::Changed)
    }

    fn reserve_events(&self, additional: usize) -> Result<(), ReviewError> {
        if additional > REVIEW_EVENTS_MAX.saturating_sub(self.events.len()) {
            return Err(ReviewError::EventCapacity);
        }
        Ok(())
    }
}

fn validate_candidates(candidates: &[ReviewCandidate]) -> Result<(), ReviewError> {
    let mut staged = false;
    let mut unstaged = false;
    for candidate in candidates {
        let section_seen = match candidate.section {
            ReviewSection::Staged => &mut staged,
            ReviewSection::Unstaged => &mut unstaged,
        };
        if *section_seen {
            return Err(ReviewError::DuplicateSection);
        }
        *section_seen = true;
        if candidate.files.len() > REVIEW_FILES_MAX {
            return Err(ReviewError::Candidate("too many files".into()));
        }
        for file in &candidate.files {
            if file.hunks.len() > REVIEW_FILE_HUNKS_MAX {
                return Err(ReviewError::Candidate("too many hunks in one file".into()));
            }
            for hunk in &file.hunks {
                if hunk.lines.len() > REVIEW_HUNK_LINES_MAX {
                    return Err(ReviewError::Candidate("too many lines in one hunk".into()));
                }
            }
        }
    }
    Ok(())
}

fn convert_candidate(candidate: ReviewCandidate) -> Result<WorktreeDiff, ReviewError> {
    let mut files = Vec::with_capacity(candidate.files.len());
    for file in candidate.files {
        files.push(convert_file(file)?);
    }
    files.sort_by(|a, b| a.path().cmp(b.path()));
    let digest = identity_digest(candidate.id.as_str());
    let index = IndexAuthority::from_digest(digest);
    let authority = CandidateAuthority::new(HeadAuthority::Unborn, index);
    WorktreeDiff::new(
        DiffOldSide::Index(index),
        DiffTarget::Worktree,
        &authority,
        files,
        DiffTruncation::Complete,
    )
    .map_err(display_candidate)
}

fn convert_file(file: ReviewFile) -> Result<PrivateFile, ReviewError> {
    let side = FileSide::new(file.path.clone(), FileMode::Regular);
    let change = match file.change {
        ReviewFileChange::Added => DiffChange::Added { new: side },
        ReviewFileChange::Deleted => DiffChange::Deleted { old: side },
        ReviewFileChange::Modified => DiffChange::Modified {
            old: side.clone(),
            new: side,
        },
        ReviewFileChange::Renamed { old_path } => DiffChange::Renamed {
            old: FileSide::new(old_path, FileMode::Regular),
            new: side,
        },
    };
    let mut hunks = Vec::with_capacity(file.hunks.len());
    for (index, hunk) in file.hunks.into_iter().enumerate() {
        let lines = hunk
            .lines
            .into_iter()
            .map(convert_line)
            .collect::<Result<Vec<_>, _>>()?;
        hunks.push(
            Hunk::new(
                HunkId::new(u32::try_from(index).map_err(display_candidate)?),
                OldLineRange::new(
                    OldLine::new(hunk.old_first).map_err(display_candidate)?,
                    hunk.old_count,
                )
                .map_err(display_candidate)?,
                NewLineRange::new(
                    NewLine::new(hunk.new_first).map_err(display_candidate)?,
                    hunk.new_count,
                )
                .map_err(display_candidate)?,
                lines,
            )
            .map_err(display_candidate)?,
        );
    }
    PrivateFile::new(
        change,
        DiffContent::Text(
            TextDiff::new(hunks, DiffTruncation::Complete).map_err(display_candidate)?,
        ),
    )
    .map_err(display_candidate)
}

fn convert_line(line: ReviewLine) -> Result<kvim_workspace::DiffLine, ReviewError> {
    let origin = match line.origin {
        ReviewLineOrigin::Context { old, new } => LineOrigin::Context {
            old: OldLine::new(old).map_err(display_candidate)?,
            new: NewLine::new(new).map_err(display_candidate)?,
        },
        ReviewLineOrigin::Removed { old } => LineOrigin::Removed {
            old: OldLine::new(old).map_err(display_candidate)?,
        },
        ReviewLineOrigin::Added { new } => LineOrigin::Added {
            new: NewLine::new(new).map_err(display_candidate)?,
        },
    };
    Ok(kvim_workspace::DiffLine::new(
        origin,
        kvim_workspace::DiffLineText::new(line.text.as_bytes().to_vec())
            .map_err(display_candidate)?,
        if line.final_line {
            LineEnding::EndOfFile
        } else {
            LineEnding::Newline
        },
    ))
}
fn validate_origin(origin: ReviewLineOrigin) -> Result<(), ReviewError> {
    match origin {
        ReviewLineOrigin::Context { old, new } => {
            validate_line_number(old)?;
            validate_line_number(new)
        }
        ReviewLineOrigin::Removed { old } => validate_line_number(old),
        ReviewLineOrigin::Added { new } => validate_line_number(new),
    }
}

fn validate_line_number(number: u32) -> Result<(), ReviewError> {
    if number == 0 || number > DIFF_LINE_NUMBER_MAX {
        return Err(ReviewError::Candidate("invalid diff line number".into()));
    }
    Ok(())
}

fn validate_range(first: u32, count: u32) -> Result<(), ReviewError> {
    validate_line_number(first)?;
    let count_max = u32::try_from(REVIEW_HUNK_LINES_MAX).expect("the hunk bound fits u32");
    if count > count_max || first.saturating_add(count.saturating_sub(1)) > DIFF_LINE_NUMBER_MAX {
        return Err(ReviewError::Candidate("invalid hunk line range".into()));
    }
    Ok(())
}

fn validate_hunk_lines(
    old_first: u32,
    old_count: u32,
    new_first: u32,
    new_count: u32,
    lines: &[ReviewLine],
) -> Result<(), ReviewError> {
    let mut expected = [old_first, new_first];
    let mut seen = [0_u32, 0_u32];
    for line in lines {
        let numbers = match line.origin {
            ReviewLineOrigin::Context { old, new } => [Some(old), Some(new)],
            ReviewLineOrigin::Removed { old } => [Some(old), None],
            ReviewLineOrigin::Added { new } => [None, Some(new)],
        };
        for (index, number) in numbers.into_iter().enumerate() {
            let Some(number) = number else { continue };
            if number != expected[index] {
                return Err(ReviewError::Candidate(
                    "diff line does not match hunk range".into(),
                ));
            }
            expected[index] = expected[index].saturating_add(1);
            seen[index] = seen[index].saturating_add(1);
        }
    }
    if seen != [old_count, new_count] {
        return Err(ReviewError::Candidate(
            "diff lines do not realize hunk ranges".into(),
        ));
    }
    Ok(())
}

fn display_candidate(error: impl std::fmt::Display) -> ReviewError {
    ReviewError::Candidate(error.to_string().into_boxed_str())
}
fn identity_digest(value: &str) -> [u8; 32] {
    *blake3::hash(value.as_bytes()).as_bytes()
}
fn map_command(command: &ReviewCommand) -> Command {
    match command {
        ReviewCommand::MoveDown => Command::MoveDown,
        ReviewCommand::MoveUp => Command::MoveUp,
        ReviewCommand::NextHunk => Command::NextHunk,
        ReviewCommand::PreviousHunk => Command::PreviousHunk,
        ReviewCommand::NextUnread => Command::NextUnreadHunk,
        ReviewCommand::PreviousUnread => Command::PreviousUnreadHunk,
        ReviewCommand::NextFile => Command::NextChangedFile,
        ReviewCommand::PreviousFile => Command::PreviousChangedFile,
        ReviewCommand::NextSection => Command::NextReviewSection,
        ReviewCommand::PreviousSection => Command::PreviousReviewSection,
        ReviewCommand::First => Command::MoveFirstLine,
        ReviewCommand::Last => Command::MoveLastLine,
        ReviewCommand::HalfPageDown => Command::MoveHalfPageDown,
        ReviewCommand::HalfPageUp => Command::MoveHalfPageUp,
        ReviewCommand::FullPageDown => Command::MoveFullPageDown,
        ReviewCommand::FullPageUp => Command::MoveFullPageUp,
        ReviewCommand::FocusDiff => Command::FocusWindowLeft,
        ReviewCommand::FocusPanel => Command::FocusWindowRight,
        ReviewCommand::WidenPanel => Command::ResizeWindowLeft,
        ReviewCommand::NarrowPanel => Command::ResizeWindowRight,
        ReviewCommand::ToggleView => Command::ToggleReviewView,
        ReviewCommand::MarkRead => Command::MarkHunkRead,
        ReviewCommand::OpenFile => Command::OpenHunkFile,
        ReviewCommand::Close => Command::CloseReview,
        ReviewCommand::SubmitComment(_) => Command::PromptCancel,
    }
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;
