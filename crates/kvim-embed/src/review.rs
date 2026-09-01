//! Pure standalone review over immutable host-supplied candidates.

#[cfg(feature = "worktree")]
use std::collections::HashMap;
use std::collections::VecDeque;
#[cfg(feature = "worktree")]
use std::error::Error as StdError;
#[cfg(feature = "worktree")]
use std::fmt;
use std::num::NonZeroU32;
#[cfg(feature = "worktree")]
use std::num::NonZeroU64;
#[cfg(feature = "worktree")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(feature = "worktree")]
use std::sync::Arc;
#[cfg(feature = "worktree")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "worktree")]
use std::time::Duration;

use kvim_input::{
    BINDING_OVERRIDES_MAX, BindingManifest, BindingOverride, BindingProfileError, Command,
    ReviewBindingProfile,
};
use kvim_path::WorktreeRelativePath;
#[cfg(feature = "worktree")]
use kvim_path::WorktreeRoot;
#[cfg(feature = "worktree")]
use kvim_runtime::{
    EventReceiver, PublicationGate, RequestId, RequestSlot, Runtime, RuntimeDrain, RuntimeEvent,
    RuntimeLimits,
};
use kvim_settings::{DiffSettings, DiffView};
use kvim_tui::__review::{
    PanelPlacement as PrivatePanelPlacement, PanelRow as PrivatePanelRow,
    ReviewFocus as PrivateFocus, ReviewModel, ReviewOutcome, ReviewPainter,
    ReviewPanelGitState as PrivatePanelGitState, ReviewPanelRowId as PrivatePanelRowId,
    ReviewPanelSection as PrivatePanelSection, ReviewPanelSectionKind as PrivatePanelSectionKind,
    ReviewPanelSnapshot as PrivatePanelSnapshot, Theme,
};
use kvim_workspace::{
    CandidateAuthority, CommentBody as PrivateCommentBody, DIFF_FILE_HUNKS_MAX, DIFF_FILES_MAX,
    DIFF_HUNK_LINES_MAX, DIFF_LINE_BYTES_MAX, DIFF_LINE_NUMBER_MAX, DiffChange, DiffContent,
    DiffLimit, DiffOldSide, DiffTarget, DiffTruncation, FileDiff as PrivateFile, FileMode,
    FileSide, HeadAuthority, Hunk, HunkId, IndexAuthority, LineEnding, LineOrigin, NewLine,
    NewLineRange, OldLine, OldLineRange, ReviewAnchor as PrivateAnchor, TextDiff, WorktreeDiff,
};
#[cfg(feature = "worktree")]
use kvim_workspace::{DiffComparison, WorktreeDiffFailure, WorktreeDiffRead, WorktreeDiffRequest};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
};
use thiserror::Error;
#[cfg(feature = "worktree")]
use tokio::runtime::Runtime as TokioRuntime;

/// Maximum bytes in a supplied candidate identity.
pub const REVIEW_CANDIDATE_ID_BYTES_MAX: usize = 128;
/// Maximum queued review events.
pub const REVIEW_EVENTS_MAX: usize = 64;
/// Maximum anchors in a persisted review snapshot.
pub const REVIEW_SNAPSHOT_ANCHORS_MAX: usize =
    REVIEW_CANDIDATES_MAX * REVIEW_FILES_MAX * REVIEW_FILE_HUNKS_MAX + 1;
/// Maximum rows in a changed-file panel snapshot.
pub const REVIEW_PANEL_ROWS_MAX: usize = kvim_ui::SIDEBAR_ROWS_MAX;
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
        validate_hunk_lines(old_first, old_count, new_first, new_count, lines)?;
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
///
/// [`ReviewFile::new`] declares a complete file. Use
/// [`ReviewFile::with_truncation`] when a collection bound omitted file content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewFile {
    path: WorktreeRelativePath,
    change: ReviewFileChange,
    hunks: Vec<ReviewHunk>,
    truncation: DiffTruncation,
}
impl ReviewFile {
    /// Validates and owns one complete text file diff.
    ///
    /// Use [`Self::with_truncation`] when the supplied file omitted content at
    /// a published collection bound.
    pub fn new(
        path: WorktreeRelativePath,
        change: ReviewFileChange,
        hunks: &[ReviewHunk],
    ) -> Result<Self, ReviewError> {
        Self::with_truncation(path, change, hunks, DiffTruncation::Complete)
    }

    /// Validates and owns one text file diff with its collection state.
    ///
    /// A truncated file remains visibly incomplete after all supplied hunks
    /// are read. The changed-file panel reports the bound and never dims it.
    ///
    /// ```
    /// use kvim_embed::{
    ///     DiffLimit, DiffTruncation, ReviewFile, ReviewFileChange, ReviewHunk, ReviewLine,
    ///     ReviewLineOrigin,
    /// };
    /// use kvim_path::WorktreeRelativePath;
    ///
    /// let line = ReviewLine::new(ReviewLineOrigin::Added { new: 1 }, "published")?;
    /// let hunk = ReviewHunk::new(1, 0, 1, 1, &[line])?;
    /// let file = ReviewFile::with_truncation(
    ///     WorktreeRelativePath::new("src/lib.rs")?,
    ///     ReviewFileChange::Added,
    ///     &[hunk],
    ///     DiffTruncation::Truncated(DiffLimit::Lines),
    /// )?;
    /// assert_eq!(file.truncation(), Some(DiffLimit::Lines));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn with_truncation(
        path: WorktreeRelativePath,
        change: ReviewFileChange,
        hunks: &[ReviewHunk],
        truncation: DiffTruncation,
    ) -> Result<Self, ReviewError> {
        if hunks.len() > REVIEW_FILE_HUNKS_MAX {
            return Err(ReviewError::Candidate("too many hunks in one file".into()));
        }
        Ok(Self {
            path,
            change,
            hunks: hunks.to_vec(),
            truncation,
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
    /// Returns the collection bound that stopped this file, if any.
    #[must_use]
    pub const fn truncation(&self) -> Option<DiffLimit> {
        match self.truncation {
            DiffTruncation::Complete => None,
            DiffTruncation::Truncated(limit) => Some(limit),
        }
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
    binding_profile: ReviewBindingProfile,
    binding_overrides: Vec<BindingOverride>,
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
            binding_profile: ReviewBindingProfile::Standalone,
            binding_overrides: Vec::new(),
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
    /// Selects an independent review binding profile.
    #[must_use]
    pub fn binding_profile(mut self, profile: ReviewBindingProfile) -> Self {
        self.binding_profile = profile;
        self
    }

    /// Sets bounded semantic review binding overrides.
    ///
    /// The method validates the count before it copies the caller's slice.
    /// Remaining review-domain and registry validation occurs during construction.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Bindings`] when the override count exceeds the
    /// published limit.
    pub fn binding_overrides(mut self, overrides: &[BindingOverride]) -> Result<Self, ReviewError> {
        if overrides.len() > BINDING_OVERRIDES_MAX {
            return Err(ReviewError::Bindings(
                BindingProfileError::TooManyOverrides {
                    overrides: overrides.len(),
                },
            ));
        }
        self.binding_overrides = overrides.to_vec();
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
    /// Reports the result of one worktree capture request.
    #[cfg(feature = "worktree")]
    CaptureFinished {
        /// The facade-owned capture request.
        request: ReviewRequestId,
    },
    /// Reports one typed worktree capture failure.
    #[cfg(feature = "worktree")]
    CaptureFailed {
        /// The facade-owned capture request.
        request: ReviewRequestId,
        /// The typed failure category.
        failure: ReviewCaptureFailure,
    },
    /// Reports explicit cancellation of one worktree capture request.
    #[cfg(feature = "worktree")]
    CaptureCancelled {
        /// The facade-owned capture request.
        request: ReviewRequestId,
    },
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

/// One section identity in a changed-file panel snapshot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReviewPanelSection {
    /// Staged changes.
    Staged,
    /// Unstaged changes.
    Unstaged,
}

/// One repository state drawn for a changed-file row or section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewPanelGitState {
    /// The candidate belongs to the index.
    Staged,
    /// The candidate belongs to the working tree.
    Modified,
}

/// One stable identity from the current changed-file panel model.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ReviewPanelRowId {
    /// A directory grouping row.
    Directory {
        /// The owning section.
        section: ReviewPanelSection,
        /// The worktree-relative directory path.
        path: PathBuf,
    },
    /// A selectable changed-file row.
    File {
        /// The owning section.
        section: ReviewPanelSection,
        /// The validated worktree-relative path.
        path: WorktreeRelativePath,
    },
}

/// One section heading shown by the review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewPanelHeading {
    section: ReviewPanelSection,
    label: Box<str>,
    active: bool,
    git: ReviewPanelGitState,
    repository_mark: Box<str>,
}
impl ReviewPanelHeading {
    /// Returns the section identity.
    #[must_use]
    pub const fn section(&self) -> ReviewPanelSection {
        self.section
    }
    /// Returns the exact heading text, including its repository-mark cell.
    #[must_use]
    pub const fn label(&self) -> &str {
        &self.label
    }
    /// Reports whether this section owns the current rows.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }
    /// Returns the repository state used for the section mark.
    #[must_use]
    pub const fn git(&self) -> ReviewPanelGitState {
        self.git
    }
    /// Returns the exact repository mark glyph.
    #[must_use]
    pub const fn repository_mark(&self) -> &str {
        &self.repository_mark
    }
}

/// One immutable changed-file panel row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewPanelRow {
    id: ReviewPanelRowId,
    text: Box<str>,
    label: Box<str>,
    guides: Box<str>,
    icon: Box<str>,
    depth: usize,
    directory: bool,
    complete: bool,
    truncation: Option<DiffLimit>,
    git: Option<ReviewPanelGitState>,
    repository_mark: Option<Box<str>>,
}
impl ReviewPanelRow {
    /// Returns the stable model identity.
    #[must_use]
    pub const fn id(&self) -> &ReviewPanelRowId {
        &self.id
    }
    /// Returns the exact composed row text used by Kvim's painter.
    #[must_use]
    pub const fn text(&self) -> &str {
        &self.text
    }
    /// Returns the file or directory label.
    #[must_use]
    pub const fn label(&self) -> &str {
        &self.label
    }
    /// Returns exact indent-guide characters.
    #[must_use]
    pub const fn guides(&self) -> &str {
        &self.guides
    }
    /// Returns the exact icon glyph.
    #[must_use]
    pub const fn icon(&self) -> &str {
        &self.icon
    }
    /// Returns tree depth.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }
    /// Reports whether this is an inert directory row.
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        self.directory
    }
    /// Reports whether every published hunk is read.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }
    /// Returns the collection bound that stopped this file, if any.
    #[must_use]
    pub const fn truncation(&self) -> Option<DiffLimit> {
        self.truncation
    }
    /// Reports whether collection stopped at a file bound.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.truncation.is_some()
    }
    /// Returns the repository state for a file row.
    #[must_use]
    pub const fn git(&self) -> Option<ReviewPanelGitState> {
        self.git
    }
    /// Returns the exact repository mark glyph, for a file row.
    #[must_use]
    pub fn repository_mark(&self) -> Option<&str> {
        self.repository_mark.as_deref()
    }
}

/// One visible row rectangle from the current panel viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReviewPanelPlacement {
    row: usize,
    area: Rect,
    first_line: u16,
    lines: u16,
}
impl ReviewPanelPlacement {
    /// Returns the row index in [`ReviewPanelSnapshot::rows`].
    #[must_use]
    pub const fn row(&self) -> usize {
        self.row
    }
    /// Returns the visible row rectangle.
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
    }
    /// Returns the first visible line within the row.
    #[must_use]
    pub const fn first_line(&self) -> u16 {
        self.first_line
    }
    /// Returns the number of visible lines.
    #[must_use]
    pub const fn lines(&self) -> u16 {
        self.lines
    }
}

/// A bounded immutable snapshot of the current changed-file panel.
///
/// The snapshot is current only until the next successful state-changing
/// review operation. Request it again after [`ReviewUpdate::Changed`] or a
/// [`ReviewEvent::Redraw`]. Geometry is the same geometry Kvim uses to paint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewPanelSnapshot {
    header: Rect,
    rows_area: Rect,
    root_label: Box<str>,
    headings: Vec<ReviewPanelHeading>,
    rows: Vec<ReviewPanelRow>,
    selected: Option<ReviewPanelRowId>,
    selection_mark: Box<str>,
    focus: ReviewFocus,
    first_line: u32,
    height_rows: u16,
    total_lines: u32,
    placements: Vec<ReviewPanelPlacement>,
}
impl ReviewPanelSnapshot {
    /// Returns the panel header rectangle.
    #[must_use]
    pub const fn header(&self) -> Rect {
        self.header
    }
    /// Returns the rectangle that contains panel rows.
    #[must_use]
    pub const fn rows_area(&self) -> Rect {
        self.rows_area
    }
    /// Returns the exact header label.
    #[must_use]
    pub const fn root_label(&self) -> &str {
        &self.root_label
    }
    /// Returns available section headings.
    #[must_use]
    pub fn headings(&self) -> &[ReviewPanelHeading] {
        &self.headings
    }
    /// Returns all bounded model rows.
    #[must_use]
    pub fn rows(&self) -> &[ReviewPanelRow] {
        &self.rows
    }
    /// Returns the selected row identity.
    #[must_use]
    pub const fn selected(&self) -> Option<&ReviewPanelRowId> {
        self.selected.as_ref()
    }
    /// Returns the exact selected-row mark glyph.
    #[must_use]
    pub const fn selection_mark(&self) -> &str {
        &self.selection_mark
    }
    /// Returns the focused review region.
    #[must_use]
    pub const fn focus(&self) -> ReviewFocus {
        self.focus
    }
    /// Returns the first visible terminal line.
    #[must_use]
    pub const fn first_line(&self) -> u32 {
        self.first_line
    }
    /// Returns the viewport height in rows.
    #[must_use]
    pub const fn height_rows(&self) -> u16 {
        self.height_rows
    }
    /// Returns total terminal lines in the panel.
    #[must_use]
    pub const fn total_lines(&self) -> u32 {
        self.total_lines
    }
    /// Returns exact visible row placements.
    #[must_use]
    pub fn placements(&self) -> &[ReviewPanelPlacement] {
        &self.placements
    }
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
    #[allow(clippy::too_many_arguments)]
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
    /// The independent review binding profile is invalid.
    #[error(transparent)]
    Bindings(#[from] BindingProfileError),
    /// Worktree capture is unavailable for a supplied-candidate surface.
    #[cfg(feature = "worktree")]
    #[error("this review surface has no worktree capture lifecycle")]
    NotWorktree,
    /// Bounded execution refused a capture submission.
    #[cfg(feature = "worktree")]
    #[error("bounded review execution refused capture work")]
    Dispatch,
    /// The surface-local capture request identity space is exhausted.
    #[cfg(feature = "worktree")]
    #[error("review capture request identity space is exhausted")]
    IdentityExhausted,
}

/// Stable classification of a worktree review open failure.
#[cfg(feature = "worktree")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewOpenErrorKind {
    /// Review configuration is invalid.
    Config,
    /// The supplied root is not a usable confined worktree.
    WorktreeRoot,
    /// The private asynchronous executor could not start.
    Executor,
    /// The process-local review identity space is exhausted.
    IdentityExhausted,
}

/// Failure while opening a worktree review surface.
#[cfg(feature = "worktree")]
#[derive(Debug)]
pub struct ReviewOpenError {
    kind: ReviewOpenErrorKind,
    path: Option<PathBuf>,
    source: Box<dyn StdError + Send + Sync>,
}

#[cfg(feature = "worktree")]
impl ReviewOpenError {
    fn new(
        kind: ReviewOpenErrorKind,
        path: Option<PathBuf>,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            path,
            source: Box::new(source),
        }
    }

    /// Returns the stable failure classification.
    #[must_use]
    pub const fn kind(&self) -> ReviewOpenErrorKind {
        self.kind
    }

    /// Returns the supplied path for a root failure.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

#[cfg(feature = "worktree")]
impl fmt::Display for ReviewOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ReviewOpenErrorKind::Config => formatter.write_str("invalid review configuration"),
            ReviewOpenErrorKind::WorktreeRoot => {
                formatter.write_str("cannot open the review worktree root")
            }
            ReviewOpenErrorKind::Executor => {
                formatter.write_str("cannot start the review executor")
            }
            ReviewOpenErrorKind::IdentityExhausted => {
                formatter.write_str("review instance identity space is exhausted")
            }
        }
    }
}

#[cfg(feature = "worktree")]
impl StdError for ReviewOpenError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

/// A pure rendered review over host-supplied immutable candidates.
///
/// `crates/kvim-embed/examples/supplied_review.rs` demonstrates the pure
/// supplied-candidate lifecycle. `crates/kvim-embed/examples/worktree_review.rs`
/// demonstrates bounded worktree capture.
pub struct ReviewSurface {
    model: ReviewModel,
    config: ReviewConfig,
    staged_id: Option<ReviewCandidateId>,
    unstaged_id: Option<ReviewCandidateId>,
    events: VecDeque<ReviewEvent>,
    bindings: BindingManifest,
    #[cfg(feature = "worktree")]
    worktree: Option<WorktreeReview>,
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
        validate_candidates(candidates)?;
        let bindings = config
            .binding_profile
            .manifest_with_overrides(&config.binding_overrides)?;
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
            config.diff,
            config.resize_step_cells,
            config.area.height,
        );
        Ok(Self {
            model,
            config,
            staged_id,
            unstaged_id,
            events: VecDeque::with_capacity(REVIEW_EVENTS_MAX),
            bindings,
            #[cfg(feature = "worktree")]
            worktree: None,
        })
    }

    /// Returns the bounded review-only binding manifest.
    ///
    /// This manifest is independent from editor entry bindings. Removing
    /// `Command::OpenReview` from an editor profile cannot change it.
    #[must_use]
    pub const fn bindings(&self) -> &BindingManifest {
        &self.bindings
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

    /// Returns the bounded current changed-file panel snapshot.
    ///
    /// The snapshot owns no mutable private model. Its rows and placements
    /// come from the same values that [`Self::render`] consumes.
    ///
    /// ```
    /// use kvim_embed::{ReviewCandidate, ReviewCandidateId, ReviewConfig, ReviewSection, ReviewSurface};
    /// use ratatui::layout::Rect;
    /// let candidate = ReviewCandidate::new(
    ///     ReviewCandidateId::new("patch-1")?,
    ///     ReviewSection::Unstaged,
    ///     &[],
    /// )?;
    /// let review = ReviewSurface::from_candidates(
    ///     &[candidate],
    ///     ReviewConfig::new(Rect::new(2, 1, 40, 12)),
    /// )?;
    /// let panel = review.panel_snapshot();
    /// assert_eq!(panel.header().y, 1);
    /// assert!(panel.rows().len() <= kvim_embed::REVIEW_PANEL_ROWS_MAX);
    /// # Ok::<(), kvim_embed::ReviewError>(())
    /// ```
    #[must_use]
    pub fn panel_snapshot(&self) -> ReviewPanelSnapshot {
        convert_panel_snapshot(
            self.model
                .panel_snapshot(self.config.area, &self.config.root_label),
        )
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
        ReviewPainter::new(Theme::default(), self.config.diff, &self.config.root_label).draw(
            target,
            self.config.area,
            &self.model,
        );
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
        if let Some((section, anchor)) = &snapshot.cursor
            && !staged_model
                .restore_cursor_anchor(matches!(section, ReviewSection::Staged), &anchor.0)
        {
            stale_events += 1;
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

    /// Opens a standalone review over one contained worktree root.
    ///
    /// Construction starts no editor, file tree, language service, watcher,
    /// clipboard, or key resolver. Call [`Self::dispatch`] to start the paired
    /// staged and unstaged capture.
    /// ```no_run
    /// use kvim_embed::{ReviewConfig, ReviewSurface};
    /// use ratatui::layout::Rect;
    ///
    /// let mut review = ReviewSurface::for_worktree(
    ///     ".",
    ///     ReviewConfig::new(Rect::new(0, 0, 80, 24)),
    /// )?;
    /// review.dispatch()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "worktree")]
    pub fn for_worktree(
        root: impl AsRef<Path>,
        config: ReviewConfig,
    ) -> Result<Self, ReviewOpenError> {
        validate_review_config(&config)
            .map_err(|source| ReviewOpenError::new(ReviewOpenErrorKind::Config, None, source))?;
        let root_path = root.as_ref().to_path_buf();
        let root = Arc::new(WorktreeRoot::open(&root_path).map_err(|source| {
            ReviewOpenError::new(
                ReviewOpenErrorKind::WorktreeRoot,
                Some(root_path.clone()),
                source,
            )
        })?);
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|source| ReviewOpenError::new(ReviewOpenErrorKind::Executor, None, source))?;
        let limits = RuntimeLimits::new(4, 1, 2).expect("fixed review runtime limits are valid");
        let _guard = executor.enter();
        let (runtime, receiver) = Runtime::with_limits(limits);
        drop(_guard);
        let instance = allocate_review_instance().ok_or_else(|| {
            ReviewOpenError::new(
                ReviewOpenErrorKind::IdentityExhausted,
                None,
                std::io::Error::other("the monotonic review instance counter reached u64::MAX"),
            )
        })?;
        let mut worktree = WorktreeReview {
            instance,
            root,
            runtime: Some(runtime),
            executor: Some(executor),
            receiver,
            gate: PublicationGate::default(),
            active: ReviewRequestId(0),
            next_request: 0,
            queued: VecDeque::with_capacity(2),
            staged: None,
            unstaged: None,
            failed: false,
            requests: HashMap::with_capacity(4),
        };
        worktree
            .queue_pair()
            .expect("a newly created worktree review has request identity capacity");
        let mut surface = Self::from_validated_worktree_pair(None, None, config);
        surface.worktree = Some(worktree);
        Ok(surface)
    }

    /// Returns this worktree review instance identity.
    #[cfg(feature = "worktree")]
    #[must_use]
    pub fn instance(&self) -> Option<ReviewInstanceId> {
        self.worktree.as_ref().map(|worktree| worktree.instance)
    }

    /// Returns the active paired capture identity.
    #[cfg(feature = "worktree")]
    #[must_use]
    pub fn active_request(&self) -> Option<ReviewRequestId> {
        self.worktree.as_ref().map(|worktree| worktree.active)
    }

    /// Queues a replacement pair and cancels both halves of the prior pair.
    #[cfg(feature = "worktree")]
    pub fn request_reload(&mut self) -> Result<ReviewRequestId, ReviewError> {
        let worktree = self.worktree.as_mut().ok_or(ReviewError::NotWorktree)?;
        worktree.queue_pair().ok_or(ReviewError::IdentityExhausted)
    }

    /// Cancels the active capture pair without changing visible review state.
    #[cfg(feature = "worktree")]
    pub fn cancel_capture(&mut self) -> Result<ReviewRequestId, ReviewError> {
        self.reserve_events(1)?;
        let request = {
            let worktree = self.worktree.as_mut().ok_or(ReviewError::NotWorktree)?;
            worktree.gate.cancel_all();
            worktree.queued.clear();
            worktree.failed = true;
            worktree.active
        };
        self.events
            .push_back(ReviewEvent::CaptureCancelled { request });
        Ok(request)
    }

    /// Submits every currently queued capture command to private bounded execution.
    ///
    /// One call submits both halves of a new pair. Later calls submit all
    /// follow-up commands queued by applied completions.
    #[cfg(feature = "worktree")]
    pub fn dispatch(&mut self) -> Result<ReviewUpdate, ReviewError> {
        let worktree = self.worktree.as_mut().ok_or(ReviewError::NotWorktree)?;
        while let Some((section, capture)) = worktree.queued.pop_front() {
            let slot = match section {
                CaptureSection::Staged => REVIEW_STAGED_SLOT,
                CaptureSection::Unstaged => REVIEW_UNSTAGED_SLOT,
            };
            let runtime = worktree
                .runtime
                .as_ref()
                .expect("shutdown consumes the runtime");
            let handle = worktree.gate.begin(slot, &runtime.cancellation_root());
            let runtime_request = handle.id();
            let request = worktree.active;
            let command = capture.command();
            let submitted_capture = capture.clone();
            if runtime
                .submit_process(handle, command, move |output| CaptureResult {
                    section,
                    read: submitted_capture.publish(&output),
                })
                .is_err()
            {
                worktree.queued.push_front((section, capture));
                return Err(ReviewError::Dispatch);
            }
            worktree.requests.insert(runtime_request, request);
        }
        Ok(ReviewUpdate::Unchanged)
    }

    /// Waits for one opaque capture completion.
    #[cfg(feature = "worktree")]
    pub async fn ready(&mut self) -> Result<ReviewCompletion, ReviewError> {
        let worktree = self.worktree.as_mut().ok_or(ReviewError::NotWorktree)?;
        let executor = worktree
            .executor
            .as_ref()
            .expect("shutdown consumes the executor");
        let _guard = executor.enter();
        let event = worktree
            .receiver
            .recv()
            .await
            .ok_or(ReviewError::Dispatch)?;
        Ok(ReviewCompletion {
            instance: worktree.instance,
            event,
        })
    }

    /// Applies one opaque capture completion atomically.
    #[cfg(feature = "worktree")]
    #[allow(clippy::result_large_err)]
    pub fn apply(
        &mut self,
        completion: ReviewCompletion,
    ) -> Result<ReviewUpdate, ReviewApplyError> {
        let Some(worktree) = self.worktree.as_ref() else {
            return Err(ReviewApplyError {
                kind: ReviewApplyErrorKind::NotWorktree,
                completion,
            });
        };
        if completion.instance != worktree.instance {
            return Err(ReviewApplyError {
                kind: ReviewApplyErrorKind::WrongInstance {
                    surface: worktree.instance,
                    completion: completion.instance,
                },
                completion,
            });
        }
        let runtime_request = completion.event.request.id();
        let Some(pair) = worktree.requests.get(&runtime_request).copied() else {
            return Err(ReviewApplyError {
                kind: ReviewApplyErrorKind::UnknownCompletion,
                completion,
            });
        };
        if pair != worktree.active || !worktree.gate.accepts(&completion.event.request) {
            let kind = ReviewApplyErrorKind::StaleRequest {
                active: worktree.active,
                completion: pair,
            };
            self.worktree
                .as_mut()
                .expect("worktree presence was checked before stale routing")
                .requests
                .remove(&runtime_request);
            return Err(ReviewApplyError { kind, completion });
        }

        let section = completion
            .event
            .result
            .as_ref()
            .ok()
            .map(|result| result.section);
        let completes_pair = match completion.event.result.as_ref() {
            Ok(CaptureResult {
                section: CaptureSection::Staged,
                read: Ok(WorktreeDiffRead::Published(_)),
            })
            | Ok(CaptureResult {
                section: CaptureSection::Staged,
                read: Err(WorktreeDiffFailure::BaseUnavailable),
            }) => worktree.unstaged.is_some(),
            Ok(CaptureResult {
                section: CaptureSection::Unstaged,
                read: Ok(WorktreeDiffRead::Published(_)),
            }) => worktree.staged.is_some(),
            _ => false,
        };
        let event_reservation = if completes_pair {
            4
        } else if matches!(
            completion.event.result.as_ref(),
            Err(_) | Ok(CaptureResult { read: Err(_), .. })
        ) {
            1
        } else {
            0
        };
        if self.reserve_events(event_reservation).is_err() {
            return Err(ReviewApplyError {
                kind: ReviewApplyErrorKind::EventCapacity,
                completion,
            });
        }

        let worktree = self
            .worktree
            .as_mut()
            .expect("worktree presence was checked before completion validation");
        let removed = worktree.requests.remove(&runtime_request);
        debug_assert_eq!(
            removed,
            Some(pair),
            "the validated completion mapping is removed once"
        );
        let result = match completion.event.result {
            Ok(result) => result.read,
            Err(error) => Err(WorktreeDiffFailure::from_runtime(&error)),
        };
        let active = worktree.active;
        match result {
            Ok(WorktreeDiffRead::Pending(next)) => {
                let section = section.expect("a successful capture result carries its section");
                worktree.queued.push_back((section, *next));
                Ok(ReviewUpdate::Unchanged)
            }
            Ok(WorktreeDiffRead::Published(candidate)) => {
                let section = section.expect("a successful capture result carries its section");
                match section {
                    CaptureSection::Staged => worktree.staged = Some(Some(*candidate)),
                    CaptureSection::Unstaged => worktree.unstaged = Some(Some(*candidate)),
                }
                self.publish_pair_if_ready(active)
            }
            Err(WorktreeDiffFailure::BaseUnavailable)
                if section == Some(CaptureSection::Staged) =>
            {
                worktree.staged = Some(None);
                self.publish_pair_if_ready(active)
            }
            Err(failure) => {
                worktree.failed = true;
                worktree.gate.cancel_all();
                worktree.queued.clear();
                self.events.push_back(ReviewEvent::CaptureFailed {
                    request: active,
                    failure: capture_failure(failure),
                });
                Ok(ReviewUpdate::Event)
            }
        }
    }

    #[cfg(feature = "worktree")]
    fn publish_pair_if_ready(
        &mut self,
        request: ReviewRequestId,
    ) -> Result<ReviewUpdate, ReviewApplyError> {
        let (staged, unstaged) = {
            let worktree = self
                .worktree
                .as_ref()
                .expect("pair publication follows worktree validation");
            if worktree.failed || worktree.staged.is_none() || worktree.unstaged.is_none() {
                return Ok(ReviewUpdate::Unchanged);
            }
            (
                worktree
                    .staged
                    .as_ref()
                    .expect("pair readiness checked")
                    .clone(),
                worktree
                    .unstaged
                    .as_ref()
                    .expect("pair readiness checked")
                    .clone(),
            )
        };
        let snapshot = self.snapshot();
        let mut replacement =
            Self::from_validated_worktree_pair(staged, unstaged, self.config.clone());
        replacement.worktree = self.worktree.take();
        replacement.restore_relocated(&snapshot);
        replacement
            .events
            .push_back(ReviewEvent::CaptureFinished { request });
        replacement.events.push_back(ReviewEvent::Redraw);
        let mut pending_events = std::mem::take(&mut self.events);
        pending_events.append(&mut replacement.events);
        replacement.events = pending_events;
        *self = replacement;
        Ok(ReviewUpdate::Changed)
    }

    #[cfg(feature = "worktree")]
    fn restore_relocated(&mut self, snapshot: &ReviewSnapshot) {
        if snapshot.anchor_count() == 0 {
            return;
        }
        let staged: Vec<_> = snapshot
            .staged_read
            .iter()
            .map(|anchor| anchor.0.clone())
            .collect();
        let unstaged: Vec<_> = snapshot
            .unstaged_read
            .iter()
            .map(|anchor| anchor.0.clone())
            .collect();
        let mut model = self.model.clone();
        let result = model.restore_read_anchors(&staged, &unstaged);
        let mut cursor_stale = false;
        if let Some((section, anchor)) = &snapshot.cursor {
            cursor_stale =
                !model.restore_cursor_anchor(matches!(section, ReviewSection::Staged), &anchor.0);
        }
        model.restore_presentation(
            match snapshot.focus {
                ReviewFocus::Diff => PrivateFocus::Diff,
                ReviewFocus::Panel => PrivateFocus::Panel,
            },
            snapshot.view,
            snapshot.panel_cells,
        );
        self.model = model;
        if cursor_stale {
            self.events.push_back(ReviewEvent::StaleSnapshotAnchor);
        }
        if result.stale > 0 {
            self.events.push_back(ReviewEvent::StaleSnapshotAnchor);
        }
    }

    /// Consumes this surface and performs bounded capture shutdown.
    #[cfg(feature = "worktree")]
    pub async fn shutdown(mut self, timeout: Duration) -> Result<ReviewShutdown, ReviewError> {
        let Some(mut worktree) = self.worktree.take() else {
            return Ok(ReviewShutdown::Finished {
                events: self.events.drain(..).collect(),
            });
        };
        worktree.gate.cancel_all();
        let runtime = worktree
            .runtime
            .take()
            .expect("live worktree surface owns runtime");
        let drain = runtime.begin_shutdown();
        let executor = worktree
            .executor
            .take()
            .expect("live worktree surface owns executor");
        let _guard = executor.enter();
        let pending_events: Vec<_> = self.events.drain(..).collect();
        if tokio::time::timeout(timeout, drain.wait()).await.is_ok() {
            drop(_guard);
            executor.shutdown_background();
            return Ok(ReviewShutdown::Finished {
                events: pending_events,
            });
        }
        drop(_guard);
        Ok(ReviewShutdown::Draining(ReviewDrain {
            runtime: executor,
            drain,
            events: pending_events,
        }))
    }

    fn reserve_events(&self, additional: usize) -> Result<(), ReviewError> {
        if additional > REVIEW_EVENTS_MAX.saturating_sub(self.events.len()) {
            return Err(ReviewError::EventCapacity);
        }
        Ok(())
    }

    #[cfg(feature = "worktree")]
    fn from_validated_worktree_pair(
        staged: Option<WorktreeDiff>,
        unstaged: Option<WorktreeDiff>,
        config: ReviewConfig,
    ) -> Self {
        debug_assert!(
            validate_review_config(&config).is_ok(),
            "worktree review configuration was validated before side effects"
        );
        let staged_id = staged
            .as_ref()
            .map(|candidate| ReviewCandidateId(candidate.revision().to_hex().into_boxed_str()));
        let unstaged_id = unstaged
            .as_ref()
            .map(|candidate| ReviewCandidateId(candidate.revision().to_hex().into_boxed_str()));
        let bindings = config
            .binding_profile
            .manifest_with_overrides(&config.binding_overrides)
            .expect("worktree review bindings were validated before side effects");
        let model = ReviewModel::new(
            staged,
            unstaged,
            config.diff,
            config.resize_step_cells,
            config.area.height,
        );
        Self {
            model,
            config,
            staged_id,
            unstaged_id,
            events: VecDeque::with_capacity(REVIEW_EVENTS_MAX),
            bindings,
            worktree: None,
        }
    }
}

impl Drop for ReviewSurface {
    fn drop(&mut self) {
        #[cfg(feature = "worktree")]
        if let Some(mut worktree) = self.worktree.take() {
            worktree.gate.cancel_all();
            drop(worktree.runtime.take());
            if let Some(executor) = worktree.executor.take() {
                executor.shutdown_background();
            }
        }
    }
}

#[cfg(feature = "worktree")]
const REVIEW_STAGED_SLOT: RequestSlot = RequestSlot::new(1);
#[cfg(feature = "worktree")]
const REVIEW_UNSTAGED_SLOT: RequestSlot = RequestSlot::new(2);
#[cfg(feature = "worktree")]
static NEXT_REVIEW_INSTANCE: AtomicU64 = AtomicU64::new(1);

#[cfg(feature = "worktree")]
fn allocate_review_instance() -> Option<ReviewInstanceId> {
    NEXT_REVIEW_INSTANCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .ok()
        .and_then(NonZeroU64::new)
        .map(ReviewInstanceId)
}

/// Facade-owned identity of one worktree review surface.
#[cfg(feature = "worktree")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReviewInstanceId(NonZeroU64);
#[cfg(feature = "worktree")]
impl ReviewInstanceId {
    /// Returns the process-local identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Facade-owned identity of one paired capture request.
#[cfg(feature = "worktree")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReviewRequestId(u64);
#[cfg(feature = "worktree")]
impl ReviewRequestId {
    /// Returns the surface-local request identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Typed failure of one worktree capture.
#[cfg(feature = "worktree")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewCaptureFailure {
    /// The `git` executable is unavailable.
    CommandMissing,
    /// Git or its answer is unavailable.
    Unavailable,
    /// The repository has no commit for its staged half.
    BaseUnavailable,
    /// The request was cancelled or replaced.
    Cancelled,
    /// One command exceeded its explicit deadline.
    DeadlineExpired,
    /// One command exceeded its output capacity.
    OutputLimit,
    /// The repository changed throughout bounded capture retries.
    ChangedDuringCapture,
}

#[cfg(feature = "worktree")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureSection {
    Staged,
    Unstaged,
}

#[cfg(feature = "worktree")]
struct CaptureResult {
    section: CaptureSection,
    read: Result<WorktreeDiffRead, WorktreeDiffFailure>,
}

/// One opaque completion returned by [`ReviewSurface::ready`].
#[cfg(feature = "worktree")]
#[must_use = "apply the completion to the review surface that produced it"]
pub struct ReviewCompletion {
    instance: ReviewInstanceId,
    event: RuntimeEvent<CaptureResult>,
}
#[cfg(feature = "worktree")]
impl fmt::Debug for ReviewCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReviewCompletion(..)")
    }
}

/// Why a review completion was rejected before mutation.
#[cfg(feature = "worktree")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewApplyErrorKind {
    /// The receiving surface has no worktree lifecycle.
    NotWorktree,
    /// The completion has no registered request mapping.
    UnknownCompletion,
    /// The facade event queue cannot hold the atomic transition.
    EventCapacity,
    /// The completion belongs to another surface.
    WrongInstance {
        /// The receiving surface.
        surface: ReviewInstanceId,
        /// The producing surface.
        completion: ReviewInstanceId,
    },
    /// A newer paired capture replaced this completion.
    StaleRequest {
        /// The active paired capture.
        active: ReviewRequestId,
        /// The obsolete paired capture.
        completion: ReviewRequestId,
    },
}

/// An unapplied worktree review completion.
#[cfg(feature = "worktree")]
pub struct ReviewApplyError {
    kind: ReviewApplyErrorKind,
    completion: ReviewCompletion,
}
#[cfg(feature = "worktree")]
impl ReviewApplyError {
    /// Returns the typed rejection.
    #[must_use]
    pub const fn kind(&self) -> ReviewApplyErrorKind {
        self.kind
    }
    /// Recovers the opaque completion for correct routing.
    pub fn into_completion(self) -> ReviewCompletion {
        self.completion
    }
}
#[cfg(feature = "worktree")]
impl fmt::Debug for ReviewApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReviewApplyError")
            .field("kind", &self.kind)
            .finish()
    }
}
#[cfg(feature = "worktree")]
impl fmt::Display for ReviewApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "review completion rejected: {:?}", self.kind)
    }
}
#[cfg(feature = "worktree")]
impl std::error::Error for ReviewApplyError {}

/// Result of consuming worktree review shutdown.
#[cfg(feature = "worktree")]
#[must_use = "a drain must be completed after a bounded shutdown timeout"]
pub enum ReviewShutdown {
    /// Every capture stopped within the deadline.
    Finished {
        /// Remaining facade-owned host events.
        events: Vec<ReviewEvent>,
    },
    /// Capture cleanup still runs.
    Draining(ReviewDrain),
}

/// Remaining private capture work after shutdown reached its deadline.
#[cfg(feature = "worktree")]
pub struct ReviewDrain {
    runtime: TokioRuntime,
    drain: RuntimeDrain,
    events: Vec<ReviewEvent>,
}
#[cfg(feature = "worktree")]
impl ReviewDrain {
    /// Waits for all private read-only capture work to stop.
    pub async fn complete(self) -> Vec<ReviewEvent> {
        let Self {
            runtime,
            drain,
            events,
        } = self;
        let _guard = runtime.enter();
        drain.wait().await;
        drop(_guard);
        runtime.shutdown_background();
        events
    }
}

#[cfg(feature = "worktree")]
struct WorktreeReview {
    instance: ReviewInstanceId,
    root: Arc<WorktreeRoot>,
    runtime: Option<Runtime<CaptureResult>>,
    executor: Option<TokioRuntime>,
    receiver: EventReceiver<CaptureResult>,
    gate: PublicationGate,
    active: ReviewRequestId,
    next_request: u64,
    queued: VecDeque<(CaptureSection, WorktreeDiffRequest)>,
    staged: Option<Option<WorktreeDiff>>,
    unstaged: Option<Option<WorktreeDiff>>,
    failed: bool,
    requests: HashMap<RequestId, ReviewRequestId>,
}

#[cfg(feature = "worktree")]
impl WorktreeReview {
    fn queue_pair(&mut self) -> Option<ReviewRequestId> {
        let next_request = self.next_request.checked_add(1)?;
        self.gate.cancel_all();
        self.next_request = next_request;
        self.active = ReviewRequestId(next_request);
        self.queued.clear();
        self.staged = None;
        self.unstaged = None;
        self.failed = false;
        self.queued.push_back((
            CaptureSection::Staged,
            WorktreeDiffRequest::new(
                Arc::clone(&self.root),
                DiffComparison::HeadToIndex,
                DiffTarget::Worktree,
            ),
        ));
        self.queued.push_back((
            CaptureSection::Unstaged,
            WorktreeDiffRequest::new(
                Arc::clone(&self.root),
                DiffComparison::IndexToWorktree,
                DiffTarget::Worktree,
            ),
        ));
        Some(self.active)
    }
}

#[cfg(feature = "worktree")]
fn validate_review_config(config: &ReviewConfig) -> Result<(), ReviewError> {
    if config.area.width == 0 || config.area.height == 0 {
        return Err(ReviewError::EmptyGeometry);
    }
    if config.root_label.len() > REVIEW_ROOT_LABEL_BYTES_MAX {
        return Err(ReviewError::RootLabelCapacity);
    }
    config
        .binding_profile
        .manifest_with_overrides(&config.binding_overrides)?;
    Ok(())
}

#[cfg(feature = "worktree")]
fn capture_failure(failure: WorktreeDiffFailure) -> ReviewCaptureFailure {
    match failure {
        WorktreeDiffFailure::CommandMissing => ReviewCaptureFailure::CommandMissing,
        WorktreeDiffFailure::Unavailable => ReviewCaptureFailure::Unavailable,
        WorktreeDiffFailure::BaseUnavailable => ReviewCaptureFailure::BaseUnavailable,
        WorktreeDiffFailure::Cancelled => ReviewCaptureFailure::Cancelled,
        WorktreeDiffFailure::DeadlineExpired => ReviewCaptureFailure::DeadlineExpired,
        WorktreeDiffFailure::ProcessOutputLimit => ReviewCaptureFailure::OutputLimit,
        WorktreeDiffFailure::ChangedDuringCapture => ReviewCaptureFailure::ChangedDuringCapture,
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

fn convert_panel_snapshot(snapshot: PrivatePanelSnapshot) -> ReviewPanelSnapshot {
    debug_assert_eq!(
        snapshot.rows.len(),
        snapshot.identities.len(),
        "the private panel builds one identity for every painter row"
    );
    debug_assert!(
        snapshot.rows.len() <= REVIEW_PANEL_ROWS_MAX,
        "the neutral sidebar enforces the public panel row bound"
    );
    let rows = snapshot
        .rows
        .into_iter()
        .zip(snapshot.identities)
        .map(|(row, id)| convert_panel_row(row, id))
        .collect();
    ReviewPanelSnapshot {
        header: snapshot.header,
        rows_area: snapshot.rows_area,
        root_label: snapshot.root.into_boxed_str(),
        headings: snapshot
            .sections
            .into_iter()
            .map(convert_panel_heading)
            .collect(),
        rows,
        selected: snapshot.selected.map(convert_panel_row_id),
        selection_mark: snapshot.selection_mark.into(),
        focus: if snapshot.focused {
            ReviewFocus::Panel
        } else {
            ReviewFocus::Diff
        },
        first_line: snapshot.first_line,
        height_rows: snapshot.height_rows,
        total_lines: snapshot.total_lines,
        placements: snapshot
            .placements
            .into_iter()
            .map(|placement: PrivatePanelPlacement| ReviewPanelPlacement {
                row: placement.index,
                area: placement.area,
                first_line: placement.first_line,
                lines: placement.lines,
            })
            .collect(),
    }
}

fn convert_panel_heading(heading: PrivatePanelSection) -> ReviewPanelHeading {
    ReviewPanelHeading {
        section: convert_panel_section(heading.section),
        label: heading.heading.into_boxed_str(),
        active: heading.active,
        git: convert_panel_git(heading.git),
        repository_mark: heading.repository_mark.into(),
    }
}

fn convert_panel_row(row: PrivatePanelRow, id: PrivatePanelRowId) -> ReviewPanelRow {
    ReviewPanelRow {
        id: convert_panel_row_id(id),
        text: row.text.into_boxed_str(),
        label: row.label.into_boxed_str(),
        guides: row.guides.into_boxed_str(),
        icon: row.icon.into(),
        depth: row.depth,
        directory: row.directory,
        complete: row.complete,
        truncation: row.truncation,
        git: row.git.map(convert_panel_git),
        repository_mark: row.repository_mark.map(Into::into),
    }
}

fn convert_panel_row_id(id: PrivatePanelRowId) -> ReviewPanelRowId {
    match id {
        PrivatePanelRowId::Directory { section, path } => ReviewPanelRowId::Directory {
            section: convert_panel_section(section),
            path,
        },
        PrivatePanelRowId::File { section, path } => ReviewPanelRowId::File {
            section: convert_panel_section(section),
            path,
        },
    }
}

const fn convert_panel_section(section: PrivatePanelSectionKind) -> ReviewPanelSection {
    match section {
        PrivatePanelSectionKind::Staged => ReviewPanelSection::Staged,
        PrivatePanelSectionKind::Unstaged => ReviewPanelSection::Unstaged,
    }
}

const fn convert_panel_git(git: PrivatePanelGitState) -> ReviewPanelGitState {
    match git {
        PrivatePanelGitState::Staged => ReviewPanelGitState::Staged,
        PrivatePanelGitState::Modified => ReviewPanelGitState::Modified,
    }
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
        DiffContent::Text(TextDiff::new(hunks, file.truncation).map_err(display_candidate)?),
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
