//! Optional worktree-backed editor facade.
//!
//! `crates/kvim-embed/examples/worktree_editor.rs` demonstrates its complete
//! lifecycle.

use std::cell::Cell;
use std::error::Error as StdError;
use std::fmt;
use std::num::{NonZeroU16, NonZeroU32};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::{
    DialogAnswer, DialogInput, DialogInputOutcome, DialogOpenError, DialogRequest, DialogSnapshot,
};
use kvim_input::{
    BindingManifest, BindingProfile, BindingScope, Command, CommandLineCommand, CommandLineError,
    CommandOwner, ContextGeneration, Dispatch, InputContextSnapshot, Key, Mode, PasteText,
    TypedText, is_register_name,
};
use kvim_keymap::PointerEvent;
use kvim_language::{LanguageRegistry, LanguageServices};
use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{FileWatcher, RuntimeLimits};
use kvim_settings::EditorSettings;
use kvim_tui::__private::{
    ClipboardAccess as TuiClipboardAccess, Completed, CursorShape as TuiCursorShape,
    EditorAccess as TuiEditorAccess, EditorCapacity as TuiEditorCapacity,
    EditorEvent as TuiEditorEvent, EditorFormatterStatus as TuiFormatterStatus,
    EditorShutdown as TuiEditorShutdown, EmbeddedEditor, FileRow as TuiFileRow,
    FileRowDimming as TuiFileRowDimming, FileRowGit as TuiFileRowGit,
    FileRowIdentity as TuiFileRowIdentity, FileRowKind as TuiFileRowKind,
    FileRowNoticeKind as TuiFileRowNoticeKind,
    FileSidebarClipboardOperation as TuiFileSidebarClipboardOperation,
    FileSidebarClipboardOutcome as TuiFileSidebarClipboardOutcome,
    FileSidebarClipboardRefusal as TuiFileSidebarClipboardRefusal,
    FileSidebarInput as TuiFileSidebarInput, FileSidebarOperation as TuiFileSidebarOperation,
    FileSidebarOperationOutcome as TuiFileSidebarOperationOutcome,
    FileSidebarOutcome as TuiFileSidebarOutcome, GeometryError as TuiGeometryError,
    HostReportRequest as TuiHostReportRequest, HostWorkspace as TuiHostWorkspace,
    IconRole as TuiIconRole, InputRequest as TuiInputRequest, ListMotion as TuiListMotion,
    PublishedEvent as TuiPublishedEvent, RecoveryDecision as TuiRecoveryDecision,
    RecoveryDecisionError as TuiRecoveryDecisionError, RecoveryIdentity as TuiRecoveryIdentity,
    RecoveryStatus as TuiRecoveryStatus, Redraw as TuiRedraw, Reduction as TuiReduction,
    ReductionOutcome as TuiReductionOutcome, Refusal as TuiRefusal, RunState as TuiRunState,
    SourceAnnotation as TuiSourceAnnotation, SourcePresentation as TuiSourcePresentation,
    SourcePresentationRefusal as TuiSourcePresentationRefusal, TerminalEvent as TuiTerminalEvent,
};
use kvim_ui::Direction;
use kvim_workspace::{EntryKind, FileOperation, TransferMode};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use thiserror::Error;
use tokio::runtime::Runtime as TokioRuntime;

/// The maximum annotations in one generic source presentation.
pub const SOURCE_ANNOTATIONS_MAX: usize = 256;
/// The maximum Unicode scalar values in one annotation message.
pub const SOURCE_ANNOTATION_MESSAGE_CHARS_MAX: usize = 4_096;
/// The maximum one-based source line accepted before a buffer is known.
pub const SOURCE_LINE_MAX: u32 = 1_000_000;

/// One validated one-based inclusive source line range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceLineRange {
    first: NonZeroU32,
    last: NonZeroU32,
}

impl SourceLineRange {
    /// Creates an ordered range inside the published source-line bound.
    pub const fn new(first: u32, last: u32) -> Result<Self, SourcePresentationBuildError> {
        let Some(first) = NonZeroU32::new(first) else {
            return Err(SourcePresentationBuildError::Range);
        };
        let Some(last) = NonZeroU32::new(last) else {
            return Err(SourcePresentationBuildError::Range);
        };
        if first.get() > last.get() || last.get() > SOURCE_LINE_MAX {
            return Err(SourcePresentationBuildError::Range);
        }
        Ok(Self { first, last })
    }

    /// Returns the first one-based line.
    #[must_use]
    pub const fn first(self) -> u32 {
        self.first.get()
    }

    /// Returns the last one-based line.
    #[must_use]
    pub const fn last(self) -> u32 {
        self.last.get()
    }
}

/// One bounded generic source annotation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAnnotation {
    range: SourceLineRange,
    message: String,
}

impl SourceAnnotation {
    /// Creates one annotation with a bounded message.
    pub fn new(
        range: SourceLineRange,
        message: impl Into<String>,
    ) -> Result<Self, SourcePresentationBuildError> {
        let message = message.into();
        if message.chars().count() > SOURCE_ANNOTATION_MESSAGE_CHARS_MAX {
            return Err(SourcePresentationBuildError::Message);
        }
        Ok(Self { range, message })
    }

    /// Returns the inclusive line range.
    #[must_use]
    pub const fn range(&self) -> SourceLineRange {
        self.range
    }

    /// Returns the bounded message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// One contained path and nonempty ordered annotation set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePresentation {
    path: WorktreeRelativePath,
    annotations: Vec<SourceAnnotation>,
}

impl SourcePresentation {
    /// Creates one bounded generic presentation.
    pub fn new(
        path: WorktreeRelativePath,
        annotations: Vec<SourceAnnotation>,
    ) -> Result<Self, SourcePresentationBuildError> {
        if annotations.is_empty() {
            return Err(SourcePresentationBuildError::Empty);
        }
        if annotations.len() > SOURCE_ANNOTATIONS_MAX {
            return Err(SourcePresentationBuildError::TooMany);
        }
        Ok(Self { path, annotations })
    }

    /// Returns the contained path.
    #[must_use]
    pub const fn path(&self) -> &WorktreeRelativePath {
        &self.path
    }

    /// Returns annotations in their supplied order.
    #[must_use]
    pub fn annotations(&self) -> &[SourceAnnotation] {
        &self.annotations
    }
}

/// Why bounded source-presentation construction failed.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SourcePresentationBuildError {
    /// A range is zero, reversed, or above the published bound.
    #[error("the source line range is outside its published bounds")]
    Range,
    /// An annotation message exceeds its bound.
    #[error("the source annotation message exceeds its published bound")]
    Message,
    /// The annotation set is empty.
    #[error("a source presentation requires an annotation")]
    Empty,
    /// The annotation set exceeds its bound.
    #[error("the source presentation has too many annotations")]
    TooMany,
}

/// The accepted state of a source-presentation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourcePresentationOutcome {
    /// The presentation replaced visible state synchronously.
    Presented,
    /// The target file was queued through the bounded file lane.
    Queued,
}

/// A cheap borrowed view of the current generic source presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourcePresentationSnapshot<'a> {
    path: &'a WorktreeRelativePath,
    range: SourceLineRange,
    message: &'a str,
    selected_index: usize,
    count: usize,
}

impl<'a> SourcePresentationSnapshot<'a> {
    /// Returns the contained path.
    #[must_use]
    pub const fn path(self) -> &'a WorktreeRelativePath {
        self.path
    }
    /// Returns the selected one-based inclusive range.
    #[must_use]
    pub const fn range(self) -> SourceLineRange {
        self.range
    }
    /// Returns the selected bounded message.
    #[must_use]
    pub const fn message(self) -> &'a str {
        self.message
    }
    /// Returns the zero-based selected index.
    #[must_use]
    pub const fn selected_index(self) -> usize {
        self.selected_index
    }
    /// Returns the annotation count.
    #[must_use]
    pub const fn count(self) -> usize {
        self.count
    }
}

/// Why a generic source-presentation operation changed no presentation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SourcePresentationError {
    /// The editor no longer accepts presentation operations.
    #[error("the editor is closed")]
    NoEditor,
    /// Another active file contains unsaved text.
    #[error("the active file contains unsaved text")]
    DifferentDirtyBuffer,
    /// Another file operation occupies the bounded file lane.
    #[error("the file operation lane is busy")]
    Busy,
    /// At least one range leaves the requested in-memory buffer.
    #[error("a source line range is outside the requested buffer")]
    RangeOutsideBuffer,
    /// Selection already names the first annotation.
    #[error("the first source annotation is already selected")]
    AtFirst,
    /// Selection already names the last annotation.
    #[error("the last source annotation is already selected")]
    AtLast,
    /// No source presentation exists.
    #[error("no source presentation exists")]
    NoPresentation,
    /// The requested file could not be opened.
    #[error("the source file could not be opened")]
    OpenFailed,
}

/// The maximum number of queued background completions.
pub const COMPLETION_CAPACITY_MAX: usize = kvim_runtime::EVENT_QUEUE_CAPACITY_MAX;
/// The maximum number of concurrent blocking jobs.
pub const WORKER_CAPACITY_MAX: usize = kvim_runtime::WORKER_CONCURRENCY_LIMIT_MAX;
/// The maximum number of concurrent child processes.
pub const PROCESS_CAPACITY_MAX: usize = kvim_runtime::PROCESS_CONCURRENCY_LIMIT;
/// The maximum number of queued editor events.
pub const EVENT_CAPACITY_MAX: usize = kvim_tui::__private::EDITOR_EVENTS_MAX;

/// Validated bounded execution capacity for one editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorktreeCapacity {
    completions: usize,
    workers: usize,
    processes: usize,
}

impl WorktreeCapacity {
    /// Creates explicit nonzero capacities within the published maxima.
    pub const fn new(
        completions: usize,
        workers: usize,
        processes: usize,
    ) -> Result<Self, CapacityError> {
        if completions == 0 || completions > COMPLETION_CAPACITY_MAX {
            return Err(CapacityError::Completions);
        }
        if workers == 0 || workers > WORKER_CAPACITY_MAX {
            return Err(CapacityError::Workers);
        }
        if processes == 0 || processes > PROCESS_CAPACITY_MAX {
            return Err(CapacityError::Processes);
        }
        Ok(Self {
            completions,
            workers,
            processes,
        })
    }
}

impl Default for WorktreeCapacity {
    fn default() -> Self {
        let limits = RuntimeLimits::default();
        Self {
            completions: limits.event_queue(),
            workers: limits.workers(),
            processes: limits.processes(),
        }
    }
}

/// One invalid execution-capacity dimension.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CapacityError {
    /// Completion capacity is zero or too large.
    #[error("completion capacity is outside its published bounds")]
    Completions,
    /// Worker capacity is zero or too large.
    #[error("worker capacity is outside its published bounds")]
    Workers,
    /// Process capacity is zero or too large.
    #[error("process capacity is outside its published bounds")]
    Processes,
}

/// What filesystem and text access the host grants.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorktreeAccess {
    /// Permit editing and filesystem mutations.
    #[default]
    ReadWrite,
    /// Permit viewing only.
    ViewOnly,
}

/// A logical one-based cursor position in buffer text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditorCursorPosition {
    line: usize,
    column: usize,
}

impl EditorCursorPosition {
    /// Returns the one-based logical line number.
    #[must_use]
    pub const fn line(self) -> usize {
        self.line
    }

    /// Returns the one-based source-character column.
    ///
    /// This is not a terminal-cell column.
    #[must_use]
    pub const fn column(self) -> usize {
        self.column
    }
}

/// Bounded diagnostic counts for the active buffer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EditorDiagnosticSummary {
    errors: u16,
    warnings: u16,
    information: u16,
    hints: u16,
}

impl EditorDiagnosticSummary {
    /// Returns the number of error diagnostics.
    #[must_use]
    pub const fn errors(self) -> u16 {
        self.errors
    }
    /// Returns the number of warning diagnostics.
    #[must_use]
    pub const fn warnings(self) -> u16 {
        self.warnings
    }
    /// Returns the number of information diagnostics.
    #[must_use]
    pub const fn information(self) -> u16 {
        self.information
    }
    /// Returns the number of hint diagnostics.
    #[must_use]
    pub const fn hints(self) -> u16 {
        self.hints
    }
    /// Returns the bounded total diagnostic count.
    #[must_use]
    pub const fn total(self) -> u16 {
        self.errors
            .saturating_add(self.warnings)
            .saturating_add(self.information)
            .saturating_add(self.hints)
    }
}

/// Semantic formatter state for the active buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorFormatterState {
    /// No formatter serves this buffer.
    Unavailable,
    /// A formatter is available and format-on-save is disabled.
    AvailableDisabled,
    /// A formatter is available and format-on-save is enabled.
    AvailableEnabled,
}

/// Maximum rows in one published host-owned file sidebar snapshot.
pub const FILE_SIDEBAR_ROWS_MAX: usize = kvim_tui::__private::FILE_SIDEBAR_ROWS_MAX;

/// Maximum characters in one published file-sidebar row label.
pub const FILE_SIDEBAR_LABEL_CHARS_MAX: usize = kvim_tui::__private::FILE_SIDEBAR_LABEL_CHARS_MAX;
/// Maximum bytes in the published worktree-root label.
pub const FILE_SIDEBAR_ROOT_LABEL_BYTES_MAX: usize =
    kvim_tui::__private::FILE_SIDEBAR_ROOT_LABEL_BYTES_MAX;
/// Maximum depth of one published file-sidebar row.
pub const FILE_SIDEBAR_DEPTH_MAX: u16 = kvim_tui::__private::FILE_SIDEBAR_DEPTH_MAX;

/// Stable semantic kind of one file-sidebar notice.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FileSidebarNoticeKind {
    /// A directory listing exceeded its entry bound.
    Truncated,
    /// A directory listing failed.
    Unreadable,
    /// Hidden entries were omitted.
    Hidden,
}

/// Stable identity of one file-sidebar row for this editor lifetime.
///
/// Obtain this opaque identity from [`FileSidebarRow::id`], then pass it to
/// [`FileSidebarCommand::Select`].
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileSidebarRowId(FileSidebarRowIdentity);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum FileSidebarRowIdentity {
    Entry(WorktreeRelativePath),
    Notice {
        parent: Option<WorktreeRelativePath>,
        kind: FileSidebarNoticeKind,
    },
}

/// Semantic kind and expansion state of one file-sidebar row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSidebarRowKind {
    /// A file.
    File,
    /// A collapsed directory.
    DirectoryCollapsed,
    /// An expanded directory with a loaded listing.
    DirectoryExpanded,
    /// An expanded directory whose listing is pending.
    DirectoryLoading,
    /// A non-selectable bounded notice of the given kind.
    Notice(FileSidebarNoticeKind),
}

/// Recorded Git state of one file-sidebar row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSidebarGitState {
    /// Ignored by Git.
    Ignored,
    /// Not tracked by Git.
    Untracked,
    /// Changed in the index.
    Staged,
    /// Changed in the working tree.
    Modified,
    /// Changed in both the index and working tree.
    StagedAndModified,
    /// Contains an unresolved conflict.
    Conflicted,
}

/// Symbolic-link state of one file-sidebar row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSidebarSymlinkState {
    /// The row names its target directly.
    Direct,
    /// The row names a symbolic link.
    Symlink,
}

/// Semantic icon role for a host-drawn file-sidebar row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSidebarIconRole {
    /// A directory.
    Directory,
    /// Source code.
    Code,
    /// Configuration or structured data.
    Configuration,
    /// Prose documentation.
    Document,
    /// An executable script.
    Script,
    /// Version-control metadata.
    VersionControl,
    /// Generated output.
    Generated,
    /// An image or binary asset.
    Media,
    /// An uncategorized file.
    Unknown,
}

/// Why kvim dims one file-sidebar entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSidebarDimming {
    /// A generated name or Git ignored state marks machine output.
    Generated,
    /// The file-operation clipboard holds this entry.
    Held(WorkspaceTransfer),
}

/// One bounded match in a file-sidebar label.
///
/// Both values count Unicode scalar-value characters. They are not byte
/// offsets or terminal-cell columns, and their end does not exceed the label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileSidebarLabelMatch {
    start: usize,
    len: usize,
}

impl FileSidebarLabelMatch {
    /// Returns the zero-based matched character position in the label.
    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    /// Returns the number of matched characters.
    #[must_use]
    pub const fn len(self) -> usize {
        self.len
    }

    /// Reports whether the span is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// One bounded semantic row of a host-owned file sidebar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSidebarRow {
    id: FileSidebarRowId,
    label: String,
    path: Option<WorktreeRelativePath>,
    depth: u16,
    kind: FileSidebarRowKind,
    selected: bool,
    git: Option<FileSidebarGitState>,
    symlink: FileSidebarSymlinkState,
    dimming: Option<FileSidebarDimming>,
    matched: Option<FileSidebarLabelMatch>,
    icon: Option<FileSidebarIconRole>,
    icon_glyph: Option<&'static str>,
}

impl FileSidebarRow {
    /// Returns the stable row identity.
    #[must_use]
    pub const fn id(&self) -> &FileSidebarRowId {
        &self.id
    }
    /// Returns the bounded display label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
    /// Returns the contained entry path. Notice rows return `None`.
    #[must_use]
    pub const fn path(&self) -> Option<&WorktreeRelativePath> {
        self.path.as_ref()
    }
    /// Returns the depth below the worktree root.
    #[must_use]
    pub const fn depth(&self) -> u16 {
        self.depth
    }
    /// Returns the row kind and directory state.
    #[must_use]
    pub const fn kind(&self) -> FileSidebarRowKind {
        self.kind
    }
    /// Reports whether this row owns the selection.
    #[must_use]
    pub const fn is_selected(&self) -> bool {
        self.selected
    }
    /// Returns the last recorded Git state.
    #[must_use]
    pub const fn git(&self) -> Option<FileSidebarGitState> {
        self.git
    }
    /// Returns the symbolic-link state.
    #[must_use]
    pub const fn symlink(&self) -> FileSidebarSymlinkState {
        self.symlink
    }
    /// Returns why kvim dims this entry, independently of its Git state.
    #[must_use]
    pub const fn dimming(&self) -> Option<FileSidebarDimming> {
        self.dimming
    }
    /// Returns the typed notice kind. Entry rows return `None`.
    #[must_use]
    pub const fn notice_kind(&self) -> Option<FileSidebarNoticeKind> {
        match self.kind {
            FileSidebarRowKind::Notice(kind) => Some(kind),
            FileSidebarRowKind::File
            | FileSidebarRowKind::DirectoryCollapsed
            | FileSidebarRowKind::DirectoryExpanded
            | FileSidebarRowKind::DirectoryLoading => None,
        }
    }
    /// Returns the current bounded search match in label-character positions.
    #[must_use]
    pub const fn matched_characters(&self) -> Option<FileSidebarLabelMatch> {
        self.matched
    }
    /// Returns the semantic icon role.
    #[must_use]
    pub const fn icon(&self) -> Option<FileSidebarIconRole> {
        self.icon
    }
    /// Returns the exact one-cell icon glyph used by kvim.
    ///
    /// The glyph requires the patched font described in `docs/files.md`.
    #[must_use]
    pub const fn icon_glyph(&self) -> Option<&'static str> {
        self.icon_glyph
    }
}

/// One cheap bounded snapshot of the host-owned file sidebar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSidebarSnapshot {
    instance: WorktreeInstanceId,
    root_label: String,
    rows: Vec<FileSidebarRow>,
}

impl FileSidebarSnapshot {
    /// Returns the addressed editor.
    #[must_use]
    pub const fn instance(&self) -> WorktreeInstanceId {
        self.instance
    }
    /// Returns the bounded worktree-root display label.
    #[must_use]
    pub fn root_label(&self) -> &str {
        &self.root_label
    }
    /// Returns the visible rows without host rows merged into them.
    #[must_use]
    pub fn rows(&self) -> &[FileSidebarRow] {
        &self.rows
    }
}

/// One semantic operation for a host-owned file-sidebar clipboard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSidebarClipboardOperation {
    /// Hold the selected entry for copying.
    Copy,
    /// Hold the selected entry for moving.
    Cut,
    /// Paste the held entries into the selected destination.
    Paste,
}

/// Why a host-owned file-sidebar clipboard operation was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSidebarClipboardRefusal {
    /// The tree shows no selected entry.
    NoSelection,
    /// The selected entry or destination disappeared from the workspace tree.
    EntryGone,
    /// The file-operation clipboard holds no entry.
    ClipboardEmpty,
    /// The selected destination is outside the workspace.
    OutsideWorkspace,
    /// One workspace operation is already running.
    Busy,
    /// The editor has view-only access.
    ViewOnly,
    /// A bounded event queue has no remaining capacity.
    Saturated,
}

/// Result of one host-owned file-sidebar clipboard operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSidebarClipboardOutcome {
    /// Kvim accepted the operation.
    Applied,
    /// Kvim refused the operation before changing the workspace.
    Refused(FileSidebarClipboardRefusal),
}

/// One semantic input for a host-owned file sidebar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileSidebarCommand {
    /// Select the currently published row with this identity.
    ///
    /// The command selects entry rows only. A notice or an identity from an
    /// earlier snapshot returns [`FileSidebarOutcome::NotSelected`] and keeps
    /// the current selection unchanged.
    Select(FileSidebarRowId),
    /// Move to the previous selectable row.
    MoveUp,
    /// Move to the next selectable row.
    MoveDown,
    /// Move to the first selectable row.
    MoveFirst,
    /// Move to the last selectable row.
    MoveLast,
    /// Move to the selected row's parent.
    MoveParent,
    /// Expand the selected directory, or activate a selected file.
    Expand,
    /// Collapse the selected directory, or move to its parent.
    Collapse,
    /// Toggle a directory, or activate a file.
    Activate,
    /// Re-read expanded directories and Git state through bounded work routing.
    Refresh,
    /// Reports that host focus traversal reached this sidebar boundary.
    ///
    /// Kvim does not track host focus. The host owns focus and applies the
    /// returned direction after it has satisfied any editor cancellation
    /// protocol required by its focused surface.
    FocusBoundary(Direction),
}

/// Result of one host-owned file-sidebar command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileSidebarOutcome {
    /// The requested row was absent or does not accept selection.
    ///
    /// The sidebar selection and editor body remain unchanged.
    NotSelected(FileSidebarRowId),
    /// Sidebar state was applied.
    Applied(WorktreeUpdate),
    /// A file activation was queued in the editor.
    Activated {
        /// Activated contained path.
        path: WorktreeRelativePath,
        /// Whether visible state changed immediately.
        update: WorktreeUpdate,
    },
    /// Host focus traversal reached the sidebar boundary.
    HostFocusBoundary(Direction),
    /// The sidebar is embedded and does not accept host-sidebar commands.
    Embedded,
}

/// Maximum characters in one accepted file-sidebar search query.
pub const FILE_SIDEBAR_SEARCH_CHARS_MAX: usize = kvim_workspace::TREE_SEARCH_CHARS_MAX;

/// Facade identity of one file-sidebar search prompt or accepted search.
///
/// A host obtains this opaque identity from
/// [`WorktreeEditor::begin_file_sidebar_search`]. Identities address one
/// editor. A later prompt makes an earlier prompt stale.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileSidebarSearchId {
    instance: WorktreeInstanceId,
    sequence: u64,
}

/// Why a host-owned file-sidebar operation was rejected.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FileSidebarOperationError {
    /// The file sidebar is rendered inside kvim.
    #[error("the file sidebar is embedded")]
    Embedded,
    /// The search identity belongs to another editor.
    #[error("the file-sidebar search addresses another editor")]
    WrongInstance,
    /// The search prompt or accepted search is no longer current.
    #[error("the file-sidebar search identity is stale")]
    StaleSearch,
    /// The query exceeds [`FILE_SIDEBAR_SEARCH_CHARS_MAX`] characters.
    #[error("the file-sidebar search query exceeds its character bound")]
    QueryTooLong,
}

/// Result of moving between file-sidebar search matches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSidebarSearchOutcome {
    /// The selection moved to a matching entry.
    Applied(WorktreeUpdate),
    /// The active query has no matching entry.
    SearchMissed,
}

/// Maximum candidates in one facade command completion.
pub const EDITOR_COMMAND_COMPLETION_CANDIDATES_MAX: usize = 64;

/// Facade identity of one visible host-owned command line.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EditorCommandSessionId(u64);

impl EditorCommandSessionId {
    /// Returns the process-local session number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Facade identity of one completion request within a command session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EditorCommandRequestId(u64);

impl EditorCommandRequestId {
    /// Creates a nonzero host-chosen request identity.
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the host-chosen request number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One bounded pure command-name completion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorCommandNameCompletion {
    candidates: Vec<&'static str>,
}

impl EditorCommandNameCompletion {
    /// Returns matching canonical names and aliases in catalog order.
    #[must_use]
    pub fn candidates(&self) -> &[&'static str] {
        &self.candidates
    }
}

/// One finished asynchronous contained-path completion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorCommandPathCompletion {
    instance: WorktreeInstanceId,
    session: EditorCommandSessionId,
    request: EditorCommandRequestId,
    candidates: Vec<String>,
}

impl EditorCommandPathCompletion {
    /// Returns the addressed editor.
    #[must_use]
    pub const fn instance(&self) -> WorktreeInstanceId {
        self.instance
    }
    /// Returns the command session that requested these candidates.
    #[must_use]
    pub const fn session(&self) -> EditorCommandSessionId {
        self.session
    }
    /// Returns the host request identity.
    #[must_use]
    pub const fn request(&self) -> EditorCommandRequestId {
        self.request
    }
    /// Returns bounded complete command lines containing confined paths.
    #[must_use]
    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }
}

/// Why a host-owned command session request was rejected.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EditorCommandSessionError {
    /// The command line belongs to an earlier or another session.
    #[error("the host command session is stale")]
    StaleSession,
    /// The line is not a bounded contained-path completion request.
    #[error("the command line does not request contained-path completion")]
    InvalidCompletion,
}

/// Maximum descriptors in one editor command catalog.
pub const EDITOR_COMMAND_DESCRIPTORS_MAX: usize = 16;

/// Stable editor command identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EditorCommandId {
    /// Write the active buffer.
    Write,
    /// Close the focused window.
    Quit,
    /// Close and discard unsaved changes.
    QuitDiscard,
    /// Write, then close the focused window.
    WriteQuit,
    /// Open a contained file.
    Edit,
    /// Reload the active file.
    Reload,
    /// Reload and discard unsaved changes.
    ReloadDiscard,
    /// Open the editor log.
    Log,
    /// Open host diagnostics.
    Diagnostics,
    /// Move to a one-based line number.
    GoToLine,
}

/// Argument schema for one editor command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorCommandArguments {
    /// The command takes no argument.
    None,
    /// The command takes one contained path.
    ContainedPath,
    /// The command takes one positive line number.
    LineNumber,
}

/// Whether the command can run in the current editor state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorCommandAvailability {
    /// The command is available.
    Available,
    /// The command requires write access.
    RequiresWriteAccess,
    /// The command requires a file-backed active buffer.
    RequiresFile,
}

/// Whether the command supports asynchronous argument completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorCommandCompletion {
    /// No argument completion exists.
    None,
    /// Complete contained worktree paths through facade routing.
    ContainedPath,
}

/// One bounded semantic editor command descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditorCommandDescriptor {
    id: EditorCommandId,
    name: &'static str,
    aliases: &'static [&'static str],
    description: &'static str,
    arguments: EditorCommandArguments,
    availability: EditorCommandAvailability,
    completion: EditorCommandCompletion,
}

impl EditorCommandDescriptor {
    /// Returns the stable command identity.
    #[must_use]
    pub const fn id(self) -> EditorCommandId {
        self.id
    }
    /// Returns the canonical unqualified name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }
    /// Returns accepted aliases.
    #[must_use]
    pub const fn aliases(self) -> &'static [&'static str] {
        self.aliases
    }
    /// Returns the semantic description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        self.description
    }
    /// Returns the argument schema.
    #[must_use]
    pub const fn arguments(self) -> EditorCommandArguments {
        self.arguments
    }
    /// Returns current availability.
    #[must_use]
    pub const fn availability(self) -> EditorCommandAvailability {
        self.availability
    }
    /// Returns argument-completion capability.
    #[must_use]
    pub const fn completion(self) -> EditorCommandCompletion {
        self.completion
    }
    /// Returns the deterministic qualified editor name.
    #[must_use]
    pub const fn qualified_name(self) -> &'static str {
        match self.id {
            EditorCommandId::Write => "editor.write",
            EditorCommandId::Quit => "editor.quit",
            EditorCommandId::QuitDiscard => "editor.quit!",
            EditorCommandId::WriteQuit => "editor.wq",
            EditorCommandId::Edit => "editor.edit",
            EditorCommandId::Reload => "editor.reload",
            EditorCommandId::ReloadDiscard => "editor.reload!",
            EditorCommandId::Log => "editor.logs",
            EditorCommandId::Diagnostics => "editor.diagnostics",
            EditorCommandId::GoToLine => "editor.line",
        }
    }
}

/// One bounded command catalog for an addressed editor state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorCommandCatalog {
    instance: WorktreeInstanceId,
    generation: ContextGeneration,
    descriptors: Vec<EditorCommandDescriptor>,
}

impl EditorCommandCatalog {
    /// Returns the addressed editor.
    #[must_use]
    pub const fn instance(&self) -> WorktreeInstanceId {
        self.instance
    }
    /// Returns the state generation used for execution.
    #[must_use]
    pub const fn generation(&self) -> ContextGeneration {
        self.generation
    }
    /// Returns bounded descriptors in stable identity order.
    #[must_use]
    pub fn descriptors(&self) -> &[EditorCommandDescriptor] {
        &self.descriptors
    }
    /// Completes editor command names without I/O or editor mutation.
    #[must_use]
    pub fn complete_names(&self, typed: &str) -> EditorCommandNameCompletion {
        let mut candidates = Vec::new();
        for descriptor in &self.descriptors {
            if descriptor.name.starts_with(typed) {
                candidates.push(descriptor.name);
            }
            for alias in descriptor.aliases {
                if alias.starts_with(typed) {
                    candidates.push(alias);
                }
            }
        }
        debug_assert!(
            candidates.len() <= EDITOR_COMMAND_COMPLETION_CANDIDATES_MAX,
            "bounded catalog aliases keep name completion bounded"
        );
        EditorCommandNameCompletion { candidates }
    }

    /// Addresses one selected editor command.
    #[must_use]
    pub const fn address(&self, id: EditorCommandId) -> AddressedEditorCommand {
        AddressedEditorCommand {
            instance: self.instance,
            generation: self.generation,
            id,
        }
    }
}

/// One selected editor command addressed to the catalog that published it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddressedEditorCommand {
    instance: WorktreeInstanceId,
    generation: ContextGeneration,
    id: EditorCommandId,
}

impl AddressedEditorCommand {
    /// Returns the addressed editor instance.
    #[must_use]
    pub const fn instance(self) -> WorktreeInstanceId {
        self.instance
    }
    /// Returns the addressed catalog generation.
    #[must_use]
    pub const fn generation(self) -> ContextGeneration {
        self.generation
    }
    /// Returns the selected semantic identity.
    #[must_use]
    pub const fn id(self) -> EditorCommandId {
        self.id
    }
}

/// A rejected addressed command execution.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EditorCommandExecutionError {
    /// The command addresses another editor.
    #[error("the command addresses another editor")]
    WrongInstance,
    /// The editor state changed after catalog publication.
    #[error("the command catalog is stale")]
    StaleGeneration,
    /// The selected identity does not match the parsed command line.
    #[error("the command identity does not match the parsed command line")]
    IdentityMismatch,
    /// The command is unavailable in current state.
    #[error("the command is unavailable in the current editor state")]
    Unavailable,
    /// The command line is invalid.
    #[error(transparent)]
    Parse(#[from] CommandLineError),
}

/// A cheap semantic snapshot for host-owned status presentation.
///
/// The snapshot borrows its bounded contained path from the editor. It owns no
/// formatted statusline text and performs no input or output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditorStatusSnapshot<'a> {
    instance: WorktreeInstanceId,
    mode: Mode,
    path: Option<&'a WorktreeRelativePath>,
    modified: bool,
    cursor: EditorCursorPosition,
    access: WorktreeAccess,
    diagnostics: EditorDiagnosticSummary,
    formatter: EditorFormatterState,
}

impl<'a> EditorStatusSnapshot<'a> {
    /// Returns the editor routing identity.
    #[must_use]
    pub const fn instance(self) -> WorktreeInstanceId {
        self.instance
    }
    /// Returns the modal editing mode.
    #[must_use]
    pub const fn mode(self) -> Mode {
        self.mode
    }
    /// Returns the active contained path, or `None` for a generated buffer.
    #[must_use]
    pub const fn path(self) -> Option<&'a WorktreeRelativePath> {
        self.path
    }
    /// Reports whether active text differs from its saved state.
    #[must_use]
    pub const fn is_modified(self) -> bool {
        self.modified
    }
    /// Returns the logical text cursor position.
    #[must_use]
    pub const fn cursor(self) -> EditorCursorPosition {
        self.cursor
    }
    /// Returns the access granted by the host.
    #[must_use]
    pub const fn access(self) -> WorktreeAccess {
        self.access
    }
    /// Returns bounded diagnostics for the active buffer.
    #[must_use]
    pub const fn diagnostics(self) -> EditorDiagnosticSummary {
        self.diagnostics
    }
    /// Returns semantic formatter availability and format-on-save state.
    #[must_use]
    pub const fn formatter(self) -> EditorFormatterState {
        self.formatter
    }
}

/// Policy for an optional built-in service.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ServicePolicy {
    /// Do not construct or start the service.
    #[default]
    Disabled,
    /// Use kvim's production built-in service and fail opening if it cannot start.
    BuiltIn,
    /// Use kvim's production built-in service, but continue without it if startup fails.
    BestEffortBuiltIn,
}

/// Explicit optional capabilities of one worktree editor.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorktreeCapabilities {
    /// Git status process capability.
    pub git: ServicePolicy,
    /// Filesystem watcher capability.
    pub watcher: ServicePolicy,
    /// Language analysis and Language Server Protocol capability.
    pub language: ServicePolicy,
    /// System clipboard process capability.
    pub clipboard: ServicePolicy,
}

/// The kind of a created workspace entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceEntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
}

/// Whether a transfer keeps its sources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceTransfer {
    /// Keep each source.
    Copy,
    /// Remove each source after transfer.
    Move,
}

/// The maximum number of paths in one published workspace operation.
pub const WORKSPACE_OPERATION_PATHS_MAX: usize = kvim_workspace::MUTATION_PATHS_MAX;

/// The discoverable kind of one workspace operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceOperationKind {
    /// Create one entry.
    Create,
    /// Delete entries.
    Delete,
    /// Rename one entry.
    Rename,
    /// Copy or move entries.
    Transfer,
}

/// Facade-owned description of one bounded workspace operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceOperation {
    data: WorkspaceOperationData,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkspaceOperationData {
    Create {
        path: WorktreeRelativePath,
        kind: WorkspaceEntryKind,
    },
    Delete {
        paths: Vec<WorktreeRelativePath>,
    },
    Rename {
        from: WorktreeRelativePath,
        to: WorktreeRelativePath,
    },
    Transfer {
        mode: WorkspaceTransfer,
        sources: Vec<WorktreeRelativePath>,
        destination: Option<WorktreeRelativePath>,
    },
}

impl WorkspaceOperation {
    /// Returns the operation kind.
    #[must_use]
    pub const fn kind(&self) -> WorkspaceOperationKind {
        match &self.data {
            WorkspaceOperationData::Create { .. } => WorkspaceOperationKind::Create,
            WorkspaceOperationData::Delete { .. } => WorkspaceOperationKind::Delete,
            WorkspaceOperationData::Rename { .. } => WorkspaceOperationKind::Rename,
            WorkspaceOperationData::Transfer { .. } => WorkspaceOperationKind::Transfer,
        }
    }

    /// Returns the created path and entry kind, when this is a create operation.
    #[must_use]
    pub const fn create(&self) -> Option<(&WorktreeRelativePath, WorkspaceEntryKind)> {
        match &self.data {
            WorkspaceOperationData::Create { path, kind } => Some((path, *kind)),
            _ => None,
        }
    }

    /// Returns the deleted paths, when this is a delete operation.
    #[must_use]
    pub fn deleted_paths(&self) -> Option<&[WorktreeRelativePath]> {
        match &self.data {
            WorkspaceOperationData::Delete { paths } => Some(paths),
            _ => None,
        }
    }

    /// Returns the old and new paths, when this is a rename operation.
    #[must_use]
    pub const fn rename(&self) -> Option<(&WorktreeRelativePath, &WorktreeRelativePath)> {
        match &self.data {
            WorkspaceOperationData::Rename { from, to } => Some((from, to)),
            _ => None,
        }
    }

    /// Returns transfer facts, when this is a transfer operation.
    #[must_use]
    pub fn transfer(
        &self,
    ) -> Option<(
        WorkspaceTransfer,
        &[WorktreeRelativePath],
        Option<&WorktreeRelativePath>,
    )> {
        match &self.data {
            WorkspaceOperationData::Transfer {
                mode,
                sources,
                destination,
            } => Some((*mode, sources, destination.as_ref())),
            _ => None,
        }
    }
}

/// The workspace root used by one host diagnostics report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeHostWorkspace {
    /// The caller resolved the root.
    Resolved {
        /// The resolved root.
        root: PathBuf,
    },
    /// The caller could not resolve the root.
    Unresolved {
        /// Why root resolution failed.
        reason: String,
    },
}

/// One bounded diagnostics probe for a worktree host.
#[derive(Clone)]
pub struct WorktreeHostReportRequest(TuiHostReportRequest);

impl WorktreeHostReportRequest {
    /// Creates a report request with kvim's built-in language registry.
    #[must_use]
    pub fn built_in(workspace: WorktreeHostWorkspace) -> Self {
        let workspace = match workspace {
            WorktreeHostWorkspace::Resolved { root } => TuiHostWorkspace::Resolved { root },
            WorktreeHostWorkspace::Unresolved { reason } => TuiHostWorkspace::Unresolved { reason },
        };
        Self(TuiHostReportRequest::new(
            LanguageRegistry::first_release(),
            workspace,
        ))
    }

    /// Probes the host and returns the plain-text report.
    #[must_use]
    pub fn run(self) -> String {
        self.0.run()
    }
}

/// Who owns one selected presentation surface.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SurfaceOwnership {
    /// Kvim renders the surface inside its accepted rectangle.
    #[default]
    Embedded,
    /// The host renders the surface from semantic state.
    HostOwned,
}

/// Presentation ownership implemented by one worktree editor.
///
/// The four choices are independent. Construction fixes them for the editor
/// lifetime, so every frame uses one realized layout.
///
/// # Examples
///
/// ```
/// use kvim_embed::{SurfaceOwnership, WorktreePresentation};
///
/// let presentation = WorktreePresentation::standalone()
///     .command_line(SurfaceOwnership::HostOwned)
///     .statusline(SurfaceOwnership::Embedded);
/// assert_eq!(
///     presentation.command_line_ownership(),
///     SurfaceOwnership::HostOwned,
/// );
/// assert_eq!(
///     presentation.statusline_ownership(),
///     SurfaceOwnership::Embedded,
/// );
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorktreePresentation {
    command_line: SurfaceOwnership,
    statusline: SurfaceOwnership,
    which_key: SurfaceOwnership,
    file_sidebar: SurfaceOwnership,
}

impl Default for WorktreePresentation {
    fn default() -> Self {
        Self::standalone()
    }
}

impl WorktreePresentation {
    /// Selects kvim-owned presentation for every surface.
    #[must_use]
    pub const fn standalone() -> Self {
        Self {
            command_line: SurfaceOwnership::Embedded,
            statusline: SurfaceOwnership::Embedded,
            which_key: SurfaceOwnership::Embedded,
            file_sidebar: SurfaceOwnership::Embedded,
        }
    }

    /// Selects host-owned presentation for every surface.
    #[must_use]
    pub const fn integrated_host() -> Self {
        Self {
            command_line: SurfaceOwnership::HostOwned,
            statusline: SurfaceOwnership::HostOwned,
            which_key: SurfaceOwnership::HostOwned,
            file_sidebar: SurfaceOwnership::HostOwned,
        }
    }

    /// Selects command-line presentation ownership.
    #[must_use]
    pub const fn command_line(mut self, ownership: SurfaceOwnership) -> Self {
        self.command_line = ownership;
        self
    }

    /// Selects statusline presentation ownership.
    #[must_use]
    pub const fn statusline(mut self, ownership: SurfaceOwnership) -> Self {
        self.statusline = ownership;
        self
    }

    /// Selects which-key presentation ownership.
    #[must_use]
    pub const fn which_key(mut self, ownership: SurfaceOwnership) -> Self {
        self.which_key = ownership;
        self
    }

    /// Selects file-sidebar presentation ownership.
    #[must_use]
    pub const fn file_sidebar(mut self, ownership: SurfaceOwnership) -> Self {
        self.file_sidebar = ownership;
        self
    }

    /// Returns command-line presentation ownership.
    #[must_use]
    pub const fn command_line_ownership(self) -> SurfaceOwnership {
        self.command_line
    }

    /// Returns statusline presentation ownership.
    #[must_use]
    pub const fn statusline_ownership(self) -> SurfaceOwnership {
        self.statusline
    }

    /// Returns which-key presentation ownership.
    #[must_use]
    pub const fn which_key_ownership(self) -> SurfaceOwnership {
        self.which_key
    }

    /// Returns file-sidebar presentation ownership.
    #[must_use]
    pub const fn file_sidebar_ownership(self) -> SurfaceOwnership {
        self.file_sidebar
    }
}

/// Proof that a host-owned command line has a visible host surface.
///
/// The marker carries no callback. Later command lifecycle APIs use the same
/// construction capability without changing presentation ownership.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorktreeCommandSurface;

impl WorktreeCommandSurface {
    /// Declares that the host can present editor command-line requests.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// How one worktree editor resolves physical bindings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorktreeBindingMode {
    /// Kvim resolves the standalone binding profile inside the facade.
    #[default]
    FacadeResolved,
    /// The host resolves the embedded profile and reserves one escape key.
    HostResolved {
        /// One normalized physical key that the host always handles before kvim.
        ///
        /// [`Key`] represents exactly one non-empty key press. It cannot
        /// represent an empty or multi-key sequence.
        reserved_escape: Key,
    },
}

impl WorktreeBindingMode {
    const fn profile(self) -> BindingProfile {
        match self {
            Self::FacadeResolved => BindingProfile::Standalone,
            Self::HostResolved { .. } => BindingProfile::Embedded,
        }
    }
}

/// Current bounded input metadata for a host-owned resolver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorktreeBindingContext {
    instance: WorktreeInstanceId,
    context: InputContextSnapshot<BindingScope>,
    overlay_scope: Option<BindingScope>,
    reserved_escape: Key,
}

impl WorktreeBindingContext {
    /// Returns the addressed editor instance.
    #[must_use]
    pub const fn instance(&self) -> WorktreeInstanceId {
        self.instance
    }
    /// Returns the context generation and semantic phases.
    #[must_use]
    pub const fn context(&self) -> InputContextSnapshot<BindingScope> {
        self.context
    }
    /// Returns the optional scope that precedes the focused prompt scope.
    #[must_use]
    pub const fn overlay_scope(&self) -> Option<BindingScope> {
        self.overlay_scope
    }
    /// Returns the physical key that the host always reserves.
    #[must_use]
    pub const fn reserved_escape(&self) -> Key {
        self.reserved_escape
    }
}

/// One addressed decision produced by a host-owned resolver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeSemanticDispatch {
    instance: WorktreeInstanceId,
    generation: ContextGeneration,
    decision: WorktreeDispatchDecision,
}

impl WorktreeSemanticDispatch {
    /// Addresses one decision to the context that produced it.
    #[must_use]
    pub const fn new(
        instance: WorktreeInstanceId,
        generation: ContextGeneration,
        decision: WorktreeDispatchDecision,
    ) -> Self {
        Self {
            instance,
            generation,
            decision,
        }
    }
}

/// The semantic result of one host-owned resolution request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeDispatchDecision {
    /// One editor command completed.
    Complete {
        /// The semantic editor command selected by the host resolver.
        command: Command,
    },
    /// A static key sequence waits in the host-owned resolver.
    ///
    /// This does not alter kvim's semantic reducer. The host owns and clears
    /// the static prefix.
    Pending,
    /// A validated `i` or `a` prefix waits for a text-object selection.
    ///
    /// This decision is accepted only in a scope that binds text objects.
    TextObjectPending,
    /// The current context takes literal text.
    TextFallback(TypedText),
    /// No binding or text owner accepted the input.
    Unbound,
    /// A preceding host scope interrupted kvim's pending input.
    Interrupted,
}

/// What addressed semantic dispatch did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeDispatchOutcome {
    /// The complete command was applied.
    Complete(WorktreeInputOutcome),
    /// Kvim retained semantic state for a later decision.
    Pending,
    /// Literal text was applied.
    TextFallback(WorktreeInputOutcome),
    /// The open dialog consumed the addressed decision.
    Consumed,
    /// Kvim cleared pending state after an unbound decision.
    Unbound,
    /// The host must complete a cancel-pending transition before acting.
    Interrupted(CancelPendingProposal),
}

/// One instance- and generation-bound cancellation proposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelPendingProposal {
    instance: WorktreeInstanceId,
    generation: ContextGeneration,
}

/// A validated idle context returned after atomic cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelPendingResume {
    instance: WorktreeInstanceId,
    context: InputContextSnapshot<BindingScope>,
}

impl CancelPendingResume {
    /// Returns the addressed editor instance.
    #[must_use]
    pub const fn instance(self) -> WorktreeInstanceId {
        self.instance
    }
    /// Returns the validated idle context.
    #[must_use]
    pub const fn context(self) -> InputContextSnapshot<BindingScope> {
        self.context
    }
}

/// Why addressed input or cancellation was refused.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeDispatchError {
    /// The value addresses another editor.
    #[error("the input transition addresses another editor")]
    WrongInstance,
    /// The editor has published another context generation.
    #[error("the input transition uses a stale context generation")]
    StaleGeneration,
    /// Host-owned dispatch is not enabled.
    #[error("the editor owns its binding resolver")]
    FacadeResolved,
    /// Kvim has no semantic input to cancel.
    #[error("the editor has no pending semantic input")]
    NoPending,
    /// The completed command is not bound in the addressed current context.
    #[error("the resolved command is not valid in the current binding context")]
    InvalidResolvedCommand,
}

/// One normalized, terminal-neutral input supplied by a host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeInput {
    /// A normalized key press or repeat.
    Key(Key),
    /// One validated bracketed-paste block.
    Paste(PasteText),
    /// One pointer action. Pointer input bypasses key-binding arbitration.
    Pointer(PointerEvent),
    /// A new terminal or host-surface size.
    Resize {
        /// Width in terminal cells.
        columns: u16,
        /// Height in terminal cells.
        rows: u16,
    },
    /// Input that no binding accepts.
    Unsupported,
}

/// Why normalized physical input was refused before mutation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeInputError {
    /// The host owns physical key and paste arbitration for this editor.
    #[error("the host owns physical key and paste resolution")]
    HostResolved,
}

/// Why an input was refused before a side effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeRefusal {
    /// The host granted view-only access.
    ViewOnly,
    /// A bounded internal queue has no capacity.
    Saturated,
}

/// A synchronous host request produced by input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeInputRequest {
    /// The host must show its command line for this facade session.
    OpenCommandLine(EditorCommandSessionId),
    /// Focus reached an outer edge.
    FocusBoundary(Direction),
    /// The last window closed.
    CloseRequested,
}

/// Result of one synchronous input reduction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeInputOutcome {
    /// The editor accepted the input.
    Applied,
    /// The editor asks the host to act.
    Request(WorktreeInputRequest),
    /// The editor refused the input.
    Refused(WorktreeRefusal),
}

/// Whether a transition requests a new frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeUpdate {
    /// Visible state did not change.
    Unchanged,
    /// Visible state changed.
    Redraw,
}

/// Whether one editor still accepts input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeRunState {
    /// The editor accepts input.
    Running,
    /// The editor requested exit.
    ExitRequested,
}

/// The cursor requested by one rendered frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorktreeCursor {
    /// Cursor cell, or `None` when hidden.
    pub position: Option<Position>,
    /// Cursor shape.
    pub shape: WorktreeCursorShape,
}

/// A terminal-neutral cursor shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeCursorShape {
    /// A block cursor.
    Block,
    /// A vertical bar cursor.
    Bar,
}

/// Facade-owned identity of one recovery candidate.
///
/// The identity is opaque and bounded. Route it back to the editor named by
/// [`Self::instance`] when resolving the candidate.
#[derive(Clone, Eq, PartialEq)]
pub struct WorktreeRecoveryId {
    instance: WorktreeInstanceId,
    inner: TuiRecoveryIdentity,
}

impl WorktreeRecoveryId {
    /// Returns the editor that published this candidate.
    #[must_use]
    pub const fn instance(&self) -> WorktreeInstanceId {
        self.instance
    }
}

impl fmt::Debug for WorktreeRecoveryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorktreeRecoveryId")
            .field("instance", &self.instance)
            .finish_non_exhaustive()
    }
}

/// Whether a recovery candidate matches the file opened from disk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeRecoveryStatus {
    /// The baseline matches, so restore, discard, and defer are available.
    Current,
    /// The file changed or disappeared, so restore is not available.
    Stale,
}

/// A host choice for one addressed recovery candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeRecoveryDecision {
    /// Apply recovered text as one undoable dirty edit.
    Restore,
    /// Keep disk text and delete the recovery record.
    Discard,
    /// Keep disk text and the recovery record.
    Defer,
}

/// The result of an accepted recovery decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeRecoveryOutcome {
    /// Recovered text replaced the opened disk text.
    Restored,
    /// Disk text remained and record deletion was queued.
    Discarded,
    /// Disk text and the record remained.
    Deferred,
}

/// Why an addressed recovery decision changed no state.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeRecoveryError {
    /// The identity belongs to another editor.
    #[error("the recovery candidate belongs to another editor")]
    WrongInstance,
    /// The candidate is no longer pending or its address is obsolete.
    #[error("the recovery candidate is stale")]
    Stale,
    /// Changed disk content prevents restoration.
    #[error("a stale recovery candidate cannot be restored")]
    RestoreForbidden,
}

/// One facade-owned editor fact or host request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeEvent {
    /// The active file changed.
    ActiveFileChanged {
        /// Active contained path.
        path: Option<WorktreeRelativePath>,
    },
    /// A file write committed.
    FileWritten {
        /// Written contained path.
        path: WorktreeRelativePath,
    },
    /// A workspace operation committed.
    WorkspaceChanged {
        /// Committed operation.
        operation: WorkspaceOperation,
    },
    /// A save has an uncertain durable result and requires reconciliation.
    SaveReconciliationRequired {
        /// Path requiring reconciliation.
        path: WorktreeRelativePath,
    },
    /// A workspace operation has an uncertain durable result.
    WorkspaceReconciliationRequired {
        /// Operation requiring reconciliation.
        operation: WorkspaceOperation,
    },
    /// Crash recovery needs a host choice.
    RecoveryCandidate {
        /// Opaque address required to resolve this candidate.
        id: WorktreeRecoveryId,
        /// Bounded contained target path.
        path: WorktreeRelativePath,
        /// Neutral baseline status for host presentation.
        status: WorktreeRecoveryStatus,
    },
    /// The sidebar activated a file.
    FileActivated {
        /// Activated contained path.
        path: WorktreeRelativePath,
    },
    /// The user answered a host-opened action-agnostic dialog.
    DialogAnswered(DialogAnswer),
    /// Visible state changed.
    RedrawRequested,
    /// Focus reached an outer edge.
    FocusBoundary(Direction),
    /// The last window closed.
    CloseRequested,
}

/// Geometry rejected by a worktree editor.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeGeometryError {
    /// The rectangle contains no cell.
    #[error("the editor rectangle {area:?} contains no cell")]
    Empty {
        /// Rejected rectangle.
        area: Rect,
    },
    /// The rectangle leaves the supplied buffer.
    #[error("the editor rectangle {area:?} leaves the buffer {buffer:?}")]
    OutsideBuffer {
        /// Rejected rectangle.
        area: Rect,
        /// Cell-buffer rectangle.
        buffer: Rect,
    },
    /// The draw rectangle differs from the accepted rectangle.
    #[error("the editor rectangle {area:?} differs from {accepted:?}")]
    Unreconciled {
        /// Supplied rectangle.
        area: Rect,
        /// Accepted rectangle.
        accepted: Rect,
    },
}

/// Stable classification of a worktree open failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeOpenErrorKind {
    /// The root is not a usable confined worktree.
    Root,
    /// Settings are invalid.
    Settings,
    /// Geometry is invalid.
    Geometry,
    /// The selected binding resolver and which-key owner are inconsistent.
    Presentation,
    /// The selected presentation requires a missing host capability.
    CommandSurface,
    /// The private asynchronous executor could not start.
    Executor,
    /// Built-in language services rejected the root.
    Language,
    /// The requested filesystem watcher could not start.
    Watcher,
}

/// Failure while opening a worktree editor.
#[derive(Debug)]
pub struct WorktreeOpenError {
    kind: WorktreeOpenErrorKind,
    path: Option<PathBuf>,
    source: Box<dyn StdError + Send + Sync>,
}

impl WorktreeOpenError {
    fn new(
        kind: WorktreeOpenErrorKind,
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
    pub const fn kind(&self) -> WorktreeOpenErrorKind {
        self.kind
    }

    /// Returns the supplied root path for a root failure.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl fmt::Display for WorktreeOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            WorktreeOpenErrorKind::Root => write!(
                formatter,
                "cannot open worktree root {:?}",
                self.path.as_deref().unwrap_or_else(|| Path::new(""))
            ),
            WorktreeOpenErrorKind::Settings => formatter.write_str("invalid editor settings"),
            WorktreeOpenErrorKind::Geometry => self.source.fmt(formatter),
            WorktreeOpenErrorKind::Presentation => {
                formatter.write_str("binding resolution and which-key ownership are inconsistent")
            }
            WorktreeOpenErrorKind::CommandSurface => formatter
                .write_str("host-owned command-line presentation requires a command surface"),
            WorktreeOpenErrorKind::Executor => {
                formatter.write_str("cannot start the editor executor")
            }
            WorktreeOpenErrorKind::Language => {
                formatter.write_str("cannot configure built-in language services")
            }
            WorktreeOpenErrorKind::Watcher => {
                formatter.write_str("cannot start the built-in filesystem watcher")
            }
        }
    }
}

impl StdError for WorktreeOpenError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

/// A host supplied an invalid resolved command.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeCommandError {
    /// The character cannot name a register.
    #[error("{name:?} is not a valid register name")]
    InvalidRegisterName {
        /// The rejected character.
        name: char,
    },
}

/// Facade-owned identity of one worktree editor.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorktreeInstanceId(u64);

impl WorktreeInstanceId {
    /// Returns the process-local instance number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The kind of a failed completion application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeApplyErrorKind {
    /// The completion belongs to another editor.
    WrongInstance {
        /// The editor that received the completion.
        editor: WorktreeInstanceId,
        /// The editor that produced the completion.
        completion: WorktreeInstanceId,
    },
}

/// A completion that was routed to another worktree editor.
///
/// The error retains the opaque completion. Call [`Self::into_completion`] to
/// route it to its owning editor without losing a reserved result.
pub struct WorktreeApplyError {
    kind: WorktreeApplyErrorKind,
    completion: WorktreeCompletion,
}

impl WorktreeApplyError {
    /// Returns the typed rejection kind.
    #[must_use]
    pub const fn kind(&self) -> WorktreeApplyErrorKind {
        self.kind
    }

    /// Recovers the unapplied completion for correct routing.
    pub fn into_completion(self) -> WorktreeCompletion {
        self.completion
    }
}

impl fmt::Debug for WorktreeApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorktreeApplyError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for WorktreeApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let WorktreeApplyErrorKind::WrongInstance { editor, completion } = self.kind;
        write!(
            formatter,
            "completion belongs to instance {completion:?}, not editor {editor:?}"
        )
    }
}

impl StdError for WorktreeApplyError {}

/// One opaque completed unit returned by [`WorktreeEditor::ready`].
#[must_use = "apply the completion to the editor that produced it"]
pub struct WorktreeCompletion {
    instance: WorktreeInstanceId,
    inner: Completed,
}

impl fmt::Debug for WorktreeCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorktreeCompletion(..)")
    }
}

/// Remaining mandatory events after a bounded shutdown deadline.
#[must_use = "complete the drain to observe durable-operation events"]
pub struct WorktreeDrain {
    runtime: TokioRuntime,
    drain: kvim_tui::__private::EditorDrain,
}

impl fmt::Debug for WorktreeDrain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorktreeDrain(..)")
    }
}

impl WorktreeDrain {
    /// Waits for committing work and returns all remaining events.
    pub async fn complete(self) -> Vec<WorktreeEvent> {
        let Self { runtime, drain } = self;
        let _guard = runtime.enter();
        let events = drain
            .complete()
            .await
            .into_iter()
            .map(convert_published)
            .collect();
        drop(_guard);
        runtime.shutdown_background();
        events
    }
}

/// Result of consuming shutdown.
#[must_use = "a drain still owns mandatory durable-operation events"]
#[derive(Debug)]
pub enum WorktreeShutdown {
    /// Every task finished before the deadline.
    Finished {
        /// Remaining facade-owned events.
        events: Vec<WorktreeEvent>,
    },
    /// Committing work still needs to deliver events.
    Draining(Box<WorktreeDrain>),
}

/// Builder for one worktree editor with explicit capabilities.
#[derive(Debug)]
pub struct WorktreeEditorBuilder {
    root: PathBuf,
    area: Rect,
    settings: EditorSettings,
    access: WorktreeAccess,
    capacity: WorktreeCapacity,
    capabilities: WorktreeCapabilities,
    binding_mode: WorktreeBindingMode,
    presentation: WorktreePresentation,
    command_surface: Option<WorktreeCommandSurface>,
    recovery_state_directory: Option<PathBuf>,
}

impl WorktreeEditorBuilder {
    /// Sets validated editor settings.
    #[must_use]
    pub fn settings(mut self, settings: EditorSettings) -> Self {
        self.settings = settings;
        self
    }
    /// Sets filesystem and text access.
    #[must_use]
    pub fn access(mut self, access: WorktreeAccess) -> Self {
        self.access = access;
        self
    }
    /// Sets bounded execution capacity.
    #[must_use]
    pub fn capacity(mut self, capacity: WorktreeCapacity) -> Self {
        self.capacity = capacity;
        self
    }
    /// Sets all optional service policies.
    #[must_use]
    pub fn capabilities(mut self, capabilities: WorktreeCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
    /// Selects facade-owned or host-owned physical binding resolution.
    #[must_use]
    pub fn binding_mode(mut self, binding_mode: WorktreeBindingMode) -> Self {
        self.binding_mode = binding_mode;
        self
    }
    /// Selects independent presentation ownership for the editor lifetime.
    #[must_use]
    pub fn presentation(mut self, presentation: WorktreePresentation) -> Self {
        self.presentation = presentation;
        self
    }
    /// Proves that the host can show a host-owned command line.
    #[must_use]
    pub fn command_surface(mut self, capability: WorktreeCommandSurface) -> Self {
        self.command_surface = Some(capability);
        self
    }
    /// Sets an absolute application state directory for crash recovery.
    #[must_use]
    pub fn recovery_state_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.recovery_state_directory = Some(directory.into());
        self
    }
    /// Opens the editor and its private executor.
    pub fn open(self) -> Result<WorktreeEditor, WorktreeOpenError> {
        if self.presentation.command_line_ownership() == SurfaceOwnership::HostOwned
            && self.command_surface.is_none()
        {
            return Err(WorktreeOpenError::new(
                WorktreeOpenErrorKind::CommandSurface,
                None,
                WorktreePresentationError::MissingCommandSurface,
            ));
        }
        if self
            .recovery_state_directory
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(WorktreeOpenError::new(
                WorktreeOpenErrorKind::Settings,
                None,
                WorktreeRecoveryPathError,
            ));
        }
        let valid_ownership = matches!(
            (self.binding_mode, self.presentation.which_key_ownership()),
            (
                WorktreeBindingMode::FacadeResolved,
                SurfaceOwnership::Embedded
            ) | (
                WorktreeBindingMode::HostResolved { .. },
                SurfaceOwnership::HostOwned
            )
        );
        if !valid_ownership {
            return Err(WorktreeOpenError::new(
                WorktreeOpenErrorKind::Presentation,
                None,
                WorktreePresentationError::ResolverWhichKeyMismatch,
            ));
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|source| {
                WorktreeOpenError::new(WorktreeOpenErrorKind::Executor, None, source)
            })?;
        let root_path = self.root;
        let root = Arc::new(WorktreeRoot::open(&root_path).map_err(|source| {
            WorktreeOpenError::new(WorktreeOpenErrorKind::Root, Some(root_path.clone()), source)
        })?);
        let limits = RuntimeLimits::new(
            self.capacity.completions,
            self.capacity.workers,
            self.capacity.processes,
        )
        .expect("WorktreeCapacity validates runtime limits");
        let _guard = runtime.enter();
        let registry = self
            .binding_mode
            .profile()
            .registry()
            .expect("built-in binding profiles are validated by focused tests");
        let manifest = self
            .binding_mode
            .profile()
            .manifest()
            .expect("built-in binding profiles are validated by focused tests");
        let mut builder = EmbeddedEditor::builder(Arc::clone(&root), self.area)
            .registry(registry)
            .presentation(kvim_tui::__private::EditorPresentation::new(
                self.presentation.command_line_ownership() == SurfaceOwnership::Embedded,
                self.presentation.statusline_ownership() == SurfaceOwnership::Embedded,
                self.presentation.which_key_ownership() == SurfaceOwnership::Embedded,
                self.presentation.file_sidebar_ownership() == SurfaceOwnership::Embedded,
            ))
            .settings(self.settings)
            .recovery_state_directory(self.recovery_state_directory)
            .access(match self.access {
                WorktreeAccess::ReadWrite => TuiEditorAccess::ReadWrite,
                WorktreeAccess::ViewOnly => TuiEditorAccess::ViewOnly,
            })
            .capacity(TuiEditorCapacity::Isolated(limits))
            .git_status(self.capabilities.git != ServicePolicy::Disabled)
            .clipboard(match self.capabilities.clipboard {
                ServicePolicy::Disabled => TuiClipboardAccess::None,
                ServicePolicy::BuiltIn | ServicePolicy::BestEffortBuiltIn => {
                    TuiClipboardAccess::System
                }
            });
        let language = construct_service(self.capabilities.language, || {
            LanguageServices::new(
                LanguageRegistry::first_release(),
                root.as_path().to_path_buf(),
                self.settings,
            )
        })
        .map_err(|source| WorktreeOpenError::new(WorktreeOpenErrorKind::Language, None, source))?;
        if let Some(language) = language {
            builder = builder.language(language);
        }
        let watcher = match self.capabilities.watcher {
            ServicePolicy::Disabled => None,
            ServicePolicy::BuiltIn => Some(
                FileWatcher::start(Arc::clone(&root), &kvim_tui::__private::GENERATED_NAMES)
                    .map_err(|source| {
                        WorktreeOpenError::new(WorktreeOpenErrorKind::Watcher, None, source)
                    })?,
            ),
            ServicePolicy::BestEffortBuiltIn => {
                match FileWatcher::start(Arc::clone(&root), &kvim_tui::__private::GENERATED_NAMES) {
                    Ok(watcher) => Some(watcher),
                    Err(_) => {
                        builder = builder.watcher_unavailable();
                        None
                    }
                }
            }
        };
        if let Some(watcher) = watcher {
            builder = builder.watcher(watcher);
        }
        let inner = builder.open().map_err(|error| match error {
            kvim_tui::__private::EditorOpenError::Settings(source) => {
                WorktreeOpenError::new(WorktreeOpenErrorKind::Settings, None, source)
            }
            kvim_tui::__private::EditorOpenError::Geometry(source) => WorktreeOpenError::new(
                WorktreeOpenErrorKind::Geometry,
                None,
                WorktreeGeometryError::from(source),
            ),
            kvim_tui::__private::EditorOpenError::LanguageRootMismatch { .. } => {
                unreachable!("facade constructs services from the same validated root")
            }
        })?;
        drop(_guard);
        let generation = inner.input_context().generation;
        Ok(WorktreeEditor {
            instance: WorktreeInstanceId(inner.instance().get()),
            inner: Some(inner),
            runtime: Some(runtime),
            binding_mode: self.binding_mode,
            manifest: Arc::new(manifest),
            presentation: self.presentation,
            dialog: crate::dialog::DialogHost::new(),
            facade_generation: Cell::new(generation),
            inner_generation: Cell::new(generation),
            dialog_generation: Cell::new(ContextGeneration::FIRST),
            file_sidebar_search_sequence: 0,
            file_sidebar_search_prompt: None,
            active_file_sidebar_search: None,
            #[cfg(test)]
            capabilities: self.capabilities,
        })
    }
}

#[derive(Clone, Copy, Debug, Error)]
#[error("the recovery state directory must be absolute")]
struct WorktreeRecoveryPathError;

#[derive(Clone, Copy, Debug, Error)]
enum WorktreePresentationError {
    #[error("the effective resolver must own which-key presentation")]
    ResolverWhichKeyMismatch,
    #[error("host-owned command-line presentation requires a command surface")]
    MissingCommandSurface,
}

fn construct_service<T, E>(
    policy: ServicePolicy,
    construct: impl FnOnce() -> Result<T, E>,
) -> Result<Option<T>, E> {
    match policy {
        ServicePolicy::Disabled => Ok(None),
        ServicePolicy::BuiltIn => construct().map(Some),
        ServicePolicy::BestEffortBuiltIn => Ok(construct().ok()),
    }
}

/// A rendered worktree editor with owned bounded orchestration.
///
/// The default constructor starts no Git, watcher, language, or clipboard
/// service. Filesystem open, edit, render, and save remain available.
/// Public asynchronous methods require the host to poll their futures. All
/// internal work executes on this editor's private runtime.
///
/// Call [`WorktreeEditor::shutdown`] to observe every mandatory event from
/// durable work. Dropping the editor cancels its private runtime as a
/// best-effort fallback and does not guarantee durable event delivery.
pub struct WorktreeEditor {
    instance: WorktreeInstanceId,
    inner: Option<EmbeddedEditor>,
    runtime: Option<TokioRuntime>,
    binding_mode: WorktreeBindingMode,
    manifest: Arc<BindingManifest>,
    presentation: WorktreePresentation,
    dialog: crate::dialog::DialogHost,
    facade_generation: Cell<ContextGeneration>,
    inner_generation: Cell<ContextGeneration>,
    dialog_generation: Cell<ContextGeneration>,
    file_sidebar_search_sequence: u64,
    file_sidebar_search_prompt: Option<FileSidebarSearchId>,
    active_file_sidebar_search: Option<FileSidebarSearchId>,
    #[cfg(test)]
    capabilities: WorktreeCapabilities,
}

impl fmt::Debug for WorktreeEditor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorktreeEditor(..)")
    }
}

impl WorktreeEditor {
    /// Starts construction over one filesystem root and nonempty rectangle.
    #[must_use]
    pub fn builder(root: impl AsRef<Path>, area: Rect) -> WorktreeEditorBuilder {
        WorktreeEditorBuilder {
            root: root.as_ref().to_path_buf(),
            area,
            settings: EditorSettings::default(),
            access: WorktreeAccess::default(),
            capacity: WorktreeCapacity::default(),
            capabilities: WorktreeCapabilities::default(),
            binding_mode: WorktreeBindingMode::default(),
            presentation: WorktreePresentation::default(),
            command_surface: None,
            recovery_state_directory: None,
        }
    }
    #[cfg(test)]
    fn region_areas(&self) -> Vec<(kvim_ui::RegionKind, Rect)> {
        self.inner().region_areas()
    }

    #[cfg(test)]
    fn capabilities(&self) -> WorktreeCapabilities {
        self.capabilities
    }

    #[cfg(test)]
    fn git_status_enabled(&self) -> bool {
        self.inner().git_status_enabled()
    }

    #[cfg(test)]
    fn git_request_queued(&self) -> bool {
        self.inner().git_request_queued()
    }

    fn inner(&self) -> &EmbeddedEditor {
        self.inner.as_ref().expect("shutdown consumes the editor")
    }
    fn inner_mut(&mut self) -> &mut EmbeddedEditor {
        self.inner.as_mut().expect("shutdown consumes the editor")
    }
    /// Returns this editor's facade-owned routing identity.
    #[must_use]
    pub const fn instance(&self) -> WorktreeInstanceId {
        self.instance
    }

    /// Returns the accepted rectangle.
    #[must_use]
    pub fn area(&self) -> Rect {
        self.inner().area()
    }
    /// Changes the accepted rectangle.
    ///
    /// An accepted resize closes an open dialog without an answer when its
    /// fixed body rectangle no longer fits.
    pub fn resize(&mut self, area: Rect) -> Result<WorktreeUpdate, WorktreeGeometryError> {
        let update = self
            .inner_mut()
            .set_area(area)
            .map(convert_redraw)
            .map_err(WorktreeGeometryError::from)?;
        if !self.dialog.body_fits(area) {
            let closed = self.dialog.close();
            debug_assert!(closed, "a non-fitting body implies an open dialog");
        } else {
            self.dialog.invalidate();
        }
        Ok(update)
    }
    /// Queues one contained file for asynchronous opening.
    pub fn open_file(&mut self, path: WorktreeRelativePath) -> WorktreeUpdate {
        convert_redraw(self.inner_mut().open_file(path))
    }
    /// Presents bounded annotations for one contained source file.
    ///
    /// The current in-memory file is never reloaded. A clean different file
    /// uses the normal bounded `dispatch`, `ready`, and `apply` lifecycle.
    /// Inspect [`Self::take_source_presentation_result`] after applying a
    /// completion to observe the asynchronous result.
    ///
    /// ```
    /// use kvim_embed::{SourceAnnotation, SourceLineRange, SourcePresentation, WorktreeEditor};
    /// use kvim_path::WorktreeRelativePath;
    /// use ratatui::layout::Rect;
    ///
    /// let root = std::env::temp_dir().join("kvim-source-presentation-doctest");
    /// std::fs::create_dir_all(&root)?;
    /// let mut editor = WorktreeEditor::builder(&root, Rect::new(0, 0, 40, 6)).open()?;
    /// let annotation = SourceAnnotation::new(SourceLineRange::new(1, 1)?, "entry")?;
    /// let request = SourcePresentation::new(WorktreeRelativePath::new("note.txt")?, vec![annotation])?;
    /// let _ = editor.present_source(request)?;
    /// # std::fs::remove_dir_all(root)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn present_source(
        &mut self,
        presentation: SourcePresentation,
    ) -> Result<SourcePresentationOutcome, SourcePresentationError> {
        let path = presentation.path.clone();
        let annotations = presentation
            .annotations
            .iter()
            .map(|annotation| {
                TuiSourceAnnotation::new(
                    usize::try_from(annotation.range.first() - 1)
                        .expect("the published line bound fits usize"),
                    usize::try_from(annotation.range.last() - 1)
                        .expect("the published line bound fits usize"),
                    annotation.message.clone(),
                )
            })
            .collect();
        let internal = TuiSourcePresentation::new(path.clone(), annotations);
        self.inner_mut()
            .present_source(internal)
            .map_err(convert_source_refusal)?;
        let active = self.status().path().is_some_and(|active| active == &path);
        Ok(if active {
            SourcePresentationOutcome::Presented
        } else {
            SourcePresentationOutcome::Queued
        })
    }

    /// Returns a borrowed snapshot of the current presentation.
    #[must_use]
    pub fn source_presentation(&self) -> Option<SourcePresentationSnapshot<'_>> {
        let presentation = self.inner().source_presentation()?;
        let selected = presentation.selected();
        Some(SourcePresentationSnapshot {
            path: presentation.path(),
            range: SourceLineRange::new(
                u32::try_from(selected.first_line() + 1).expect("published bounds fit u32"),
                u32::try_from(selected.last_line() + 1).expect("published bounds fit u32"),
            )
            .expect("private values came from validated facade ranges"),
            message: selected.message(),
            selected_index: presentation.selected_index(),
            count: presentation.annotation_count(),
        })
    }

    /// Selects the next annotation without wrapping.
    pub fn next_source_annotation(&mut self) -> Result<WorktreeUpdate, SourcePresentationError> {
        self.inner_mut()
            .next_source_annotation()
            .map(convert_redraw)
            .map_err(convert_source_refusal)
    }

    /// Selects the previous annotation without wrapping.
    pub fn previous_source_annotation(
        &mut self,
    ) -> Result<WorktreeUpdate, SourcePresentationError> {
        self.inner_mut()
            .previous_source_annotation()
            .map(convert_redraw)
            .map_err(convert_source_refusal)
    }

    /// Removes only generic source-presentation state.
    pub fn clear_source_presentation(&mut self) -> WorktreeUpdate {
        convert_redraw(self.inner_mut().clear_source_presentation())
    }

    /// Takes the newest asynchronous presentation result.
    #[must_use]
    pub fn take_source_presentation_result(
        &mut self,
    ) -> Option<Result<(), SourcePresentationError>> {
        self.inner_mut()
            .take_source_presentation_result()
            .map(|result| result.map_err(convert_source_refusal))
    }

    /// Returns a bounded semantic snapshot for a host-owned file sidebar.
    ///
    /// This copies loaded tree state only. Directory and Git reads continue
    /// through [`Self::dispatch`], [`Self::ready`], and [`Self::apply`].
    ///
    /// ```
    /// use kvim_embed::{SurfaceOwnership, WorktreeEditor, WorktreePresentation};
    /// use ratatui::layout::Rect;
    ///
    /// let root = std::env::temp_dir().join("kvim-sidebar-snapshot-example");
    /// std::fs::create_dir_all(&root)?;
    /// let presentation = WorktreePresentation::standalone()
    ///     .file_sidebar(SurfaceOwnership::HostOwned);
    /// let editor = WorktreeEditor::builder(&root, Rect::new(0, 0, 40, 6))
    ///     .presentation(presentation)
    ///     .open()?;
    /// let snapshot = editor.file_sidebar_snapshot().expect("the host owns the sidebar");
    /// assert_eq!(snapshot.instance(), editor.instance());
    /// # std::fs::remove_dir_all(root)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn file_sidebar_snapshot(&self) -> Option<FileSidebarSnapshot> {
        if self.presentation.file_sidebar_ownership() == SurfaceOwnership::Embedded {
            return None;
        }
        let rows = self.inner().file_rows();
        debug_assert!(
            rows.len() <= FILE_SIDEBAR_ROWS_MAX,
            "the private tree enforces the published sidebar row bound"
        );
        Some(FileSidebarSnapshot {
            instance: self.instance,
            root_label: self.inner().file_root_label(),
            rows: rows.into_iter().map(convert_file_sidebar_row).collect(),
        })
    }

    /// Applies one typed file clipboard operation to the host-owned sidebar.
    ///
    /// Kvim owns the held entries and returns the authoritative operation
    /// result. A refused paste preserves those entries. An accepted paste
    /// consumes them only after the workspace mutation completes successfully.
    ///
    /// # Errors
    ///
    /// Returns [`FileSidebarOperationError::NotHostOwned`] unless the host owns
    /// the file-sidebar presentation.
    pub fn file_sidebar_clipboard(
        &mut self,
        operation: FileSidebarClipboardOperation,
    ) -> Result<FileSidebarClipboardOutcome, FileSidebarOperationError> {
        self.require_host_file_sidebar()?;
        let operation = match operation {
            FileSidebarClipboardOperation::Copy => TuiFileSidebarClipboardOperation::Copy,
            FileSidebarClipboardOperation::Cut => TuiFileSidebarClipboardOperation::Cut,
            FileSidebarClipboardOperation::Paste => TuiFileSidebarClipboardOperation::Paste,
        };
        Ok(
            match self.inner_mut().file_sidebar_clipboard_operation(operation) {
                TuiFileSidebarClipboardOutcome::Applied => FileSidebarClipboardOutcome::Applied,
                TuiFileSidebarClipboardOutcome::Refused(refusal) => {
                    FileSidebarClipboardOutcome::Refused(match refusal {
                        TuiFileSidebarClipboardRefusal::NoSelection => {
                            FileSidebarClipboardRefusal::NoSelection
                        }
                        TuiFileSidebarClipboardRefusal::EntryGone => {
                            FileSidebarClipboardRefusal::EntryGone
                        }
                        TuiFileSidebarClipboardRefusal::ClipboardEmpty => {
                            FileSidebarClipboardRefusal::ClipboardEmpty
                        }
                        TuiFileSidebarClipboardRefusal::OutsideWorkspace => {
                            FileSidebarClipboardRefusal::OutsideWorkspace
                        }
                        TuiFileSidebarClipboardRefusal::Busy => FileSidebarClipboardRefusal::Busy,
                        TuiFileSidebarClipboardRefusal::ViewOnly => {
                            FileSidebarClipboardRefusal::ViewOnly
                        }
                        TuiFileSidebarClipboardRefusal::Saturated => {
                            FileSidebarClipboardRefusal::Saturated
                        }
                    })
                }
            },
        )
    }

    /// Starts a host-owned file-sidebar search prompt.
    ///
    /// The prompt is independent of command-line presentation ownership. A
    /// newer prompt makes an earlier open prompt stale, but it does not end an
    /// already accepted search.
    ///
    /// This lifecycle uses a host-owned sidebar. The host presents the query,
    /// draws [`FileSidebarRow::matched_characters`], and continues normal
    /// bounded work routing with [`WorktreeEditor::dispatch`],
    /// [`WorktreeEditor::ready`], and [`WorktreeEditor::apply`].
    ///
    /// ```no_run
    /// use std::num::NonZeroU16;
    ///
    /// use kvim_embed::{SurfaceOwnership, WorktreeEditor, WorktreePresentation};
    /// use ratatui::layout::Rect;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let presentation = WorktreePresentation::standalone()
    ///     .file_sidebar(SurfaceOwnership::HostOwned);
    /// let mut editor = WorktreeEditor::builder("project", Rect::new(0, 0, 80, 24))
    ///     .presentation(presentation)
    ///     .open()?;
    /// let search = editor.begin_file_sidebar_search()?;
    /// editor.accept_file_sidebar_search(search, "main")?;
    /// let snapshot = editor.file_sidebar_snapshot().expect("host-owned sidebar");
    /// let spans = snapshot.rows().iter().filter_map(|row| row.matched_characters());
    /// # let _ = spans.count();
    /// let _ = editor.next_file_sidebar_match(search)?;
    /// editor.record_file_sidebar_viewport(
    ///     NonZeroU16::new(12).expect("nonzero body height"),
    ///     NonZeroU16::new(30).expect("nonzero body width"),
    /// )?;
    /// editor.move_file_sidebar_half_page_down()?;
    /// editor.end_file_sidebar_search(search)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn begin_file_sidebar_search(
        &mut self,
    ) -> Result<FileSidebarSearchId, FileSidebarOperationError> {
        self.require_host_file_sidebar()?;
        self.file_sidebar_search_sequence = self
            .file_sidebar_search_sequence
            .checked_add(1)
            .expect("one editor cannot exhaust file-sidebar search identities");
        let id = FileSidebarSearchId {
            instance: self.instance,
            sequence: self.file_sidebar_search_sequence,
        };
        self.file_sidebar_search_prompt = Some(id);
        Ok(id)
    }

    /// Accepts the bounded query of the addressed open search prompt.
    ///
    /// An empty query ends the active search. An oversized query fails before
    /// changing the prompt or tree.
    pub fn accept_file_sidebar_search(
        &mut self,
        id: FileSidebarSearchId,
        query: &str,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.require_search_prompt(id)?;
        if query.chars().count() > FILE_SIDEBAR_SEARCH_CHARS_MAX {
            return Err(FileSidebarOperationError::QueryTooLong);
        }
        let before = self.inner().file_rows();
        let operation = if query.is_empty() {
            TuiFileSidebarOperation::EndSearch
        } else {
            TuiFileSidebarOperation::StartSearch(query.to_owned())
        };
        let outcome = self.inner_mut().file_sidebar_operation(operation);
        debug_assert_eq!(
            outcome,
            TuiFileSidebarOperationOutcome::Applied,
            "starting or ending search always applies at the private boundary"
        );
        self.file_sidebar_search_prompt = None;
        self.active_file_sidebar_search = (!query.is_empty()).then_some(id);
        Ok(self.file_sidebar_update_since(before))
    }

    /// Replaces the bounded query of the addressed accepted search.
    ///
    /// An empty query ends the accepted search.
    pub fn update_file_sidebar_search(
        &mut self,
        id: FileSidebarSearchId,
        query: &str,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.require_active_search(id)?;
        if query.chars().count() > FILE_SIDEBAR_SEARCH_CHARS_MAX {
            return Err(FileSidebarOperationError::QueryTooLong);
        }
        let before = self.inner().file_rows();
        let operation = if query.is_empty() {
            TuiFileSidebarOperation::EndSearch
        } else {
            TuiFileSidebarOperation::StartSearch(query.to_owned())
        };
        let outcome = self.inner_mut().file_sidebar_operation(operation);
        debug_assert_eq!(
            outcome,
            TuiFileSidebarOperationOutcome::Applied,
            "updating or ending search always applies at the private boundary"
        );
        if query.is_empty() {
            self.active_file_sidebar_search = None;
        }
        Ok(self.file_sidebar_update_since(before))
    }

    /// Cancels the addressed open prompt without ending an accepted search.
    pub fn cancel_file_sidebar_search_prompt(
        &mut self,
        id: FileSidebarSearchId,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.require_search_prompt(id)?;
        self.file_sidebar_search_prompt = None;
        Ok(WorktreeUpdate::Unchanged)
    }

    /// Ends the addressed accepted file-sidebar search.
    pub fn end_file_sidebar_search(
        &mut self,
        id: FileSidebarSearchId,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.require_active_search(id)?;
        let before = self.inner().file_rows();
        let outcome = self
            .inner_mut()
            .file_sidebar_operation(TuiFileSidebarOperation::EndSearch);
        debug_assert_eq!(
            outcome,
            TuiFileSidebarOperationOutcome::Applied,
            "ending search always applies at the private boundary"
        );
        self.active_file_sidebar_search = None;
        Ok(self.file_sidebar_update_since(before))
    }

    /// Releases the file-operation clipboard of a host-owned sidebar.
    ///
    /// This operation does not require or change an accepted search. A host
    /// can call it beside [`Self::end_file_sidebar_search`] when one input
    /// must end search and release the hold together.
    pub fn release_file_sidebar_hold(
        &mut self,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.require_host_file_sidebar()?;
        let before = self.inner().file_rows();
        let outcome = self
            .inner_mut()
            .file_sidebar_operation(TuiFileSidebarOperation::ReleaseHold);
        debug_assert_eq!(
            outcome,
            TuiFileSidebarOperationOutcome::Applied,
            "releasing a file-sidebar hold always applies at the private boundary"
        );
        Ok(self.file_sidebar_update_since(before))
    }

    /// Selects the next match of the addressed accepted search.
    pub fn next_file_sidebar_match(
        &mut self,
        id: FileSidebarSearchId,
    ) -> Result<FileSidebarSearchOutcome, FileSidebarOperationError> {
        self.file_sidebar_match(id, TuiFileSidebarOperation::NextMatch)
    }

    /// Selects the previous match of the addressed accepted search.
    pub fn previous_file_sidebar_match(
        &mut self,
        id: FileSidebarSearchId,
    ) -> Result<FileSidebarSearchOutcome, FileSidebarOperationError> {
        self.file_sidebar_match(id, TuiFileSidebarOperation::PreviousMatch)
    }

    /// Records the visible host-sidebar body geometry for semantic page moves.
    pub fn record_file_sidebar_viewport(
        &mut self,
        height_rows: NonZeroU16,
        width_cells: NonZeroU16,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.require_host_file_sidebar()?;
        let outcome =
            self.inner_mut()
                .file_sidebar_operation(TuiFileSidebarOperation::RecordViewport {
                    height_rows,
                    width_cells,
                });
        debug_assert_eq!(
            outcome,
            TuiFileSidebarOperationOutcome::Applied,
            "recording valid nonzero geometry always applies"
        );
        Ok(WorktreeUpdate::Unchanged)
    }

    /// Moves the file-sidebar selection down by half the recorded page.
    pub fn move_file_sidebar_half_page_down(
        &mut self,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.file_sidebar_page(TuiFileSidebarOperation::HalfPageDown)
    }

    /// Moves the file-sidebar selection up by half the recorded page.
    pub fn move_file_sidebar_half_page_up(
        &mut self,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.file_sidebar_page(TuiFileSidebarOperation::HalfPageUp)
    }

    /// Moves the file-sidebar selection down by one recorded page.
    pub fn move_file_sidebar_full_page_down(
        &mut self,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.file_sidebar_page(TuiFileSidebarOperation::FullPageDown)
    }

    /// Moves the file-sidebar selection up by one recorded page.
    pub fn move_file_sidebar_full_page_up(
        &mut self,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.file_sidebar_page(TuiFileSidebarOperation::FullPageUp)
    }

    /// Opens one validated host dialog above this editor.
    pub fn open_dialog(&mut self, request: DialogRequest) -> Result<(), DialogOpenError> {
        crate::dialog::validate_dialog_body(&request, self.area())?;
        self.dialog.open(request)
    }

    /// Closes an open host dialog without producing an answer event.
    #[must_use]
    pub fn close_dialog(&mut self) -> bool {
        self.dialog.close()
    }

    /// Returns the current host-dialog snapshot.
    #[must_use]
    pub fn dialog_snapshot(&self) -> Option<DialogSnapshot> {
        self.dialog.snapshot()
    }

    /// Returns whether a host dialog currently owns all input.
    #[must_use]
    pub fn dialog_is_open(&self) -> bool {
        self.dialog.is_open()
    }

    /// Drives dialog-owned physical input before host-global or editor input.
    #[must_use]
    pub fn dialog_input(&mut self, input: DialogInput) -> DialogInputOutcome {
        self.dialog.input(input)
    }

    /// Applies one semantic command to a host-owned file sidebar.
    ///
    /// Expansion and refresh only queue work. Activation queues the selected
    /// file open, so kvim retains file-activation ownership.
    #[must_use]
    pub fn file_sidebar_command(&mut self, command: FileSidebarCommand) -> FileSidebarOutcome {
        if self.presentation.file_sidebar_ownership() == SurfaceOwnership::Embedded {
            return FileSidebarOutcome::Embedded;
        }
        if let FileSidebarCommand::FocusBoundary(direction) = command {
            return FileSidebarOutcome::HostFocusBoundary(direction);
        }
        if let FileSidebarCommand::Select(id) = command {
            let before = self.inner().file_rows();
            let input = TuiFileSidebarInput::Select(convert_file_sidebar_identity(&id));
            return match self.inner_mut().file_sidebar(input) {
                TuiFileSidebarOutcome::NotSelected => FileSidebarOutcome::NotSelected(id),
                TuiFileSidebarOutcome::Applied => {
                    let update = if self.inner().file_rows() == before {
                        WorktreeUpdate::Unchanged
                    } else {
                        WorktreeUpdate::Redraw
                    };
                    FileSidebarOutcome::Applied(update)
                }
                TuiFileSidebarOutcome::Activated { .. } => {
                    debug_assert!(false, "selecting a sidebar row cannot activate a file");
                    FileSidebarOutcome::NotSelected(id)
                }
            };
        }
        let input = match command {
            FileSidebarCommand::Select(_) => unreachable!("selection commands return above"),
            FileSidebarCommand::MoveUp => TuiFileSidebarInput::Move(TuiListMotion::Up(1)),
            FileSidebarCommand::MoveDown => TuiFileSidebarInput::Move(TuiListMotion::Down(1)),
            FileSidebarCommand::MoveFirst => TuiFileSidebarInput::Move(TuiListMotion::ToRow(0)),
            FileSidebarCommand::MoveLast => TuiFileSidebarInput::Move(TuiListMotion::LastRow),
            FileSidebarCommand::MoveParent => TuiFileSidebarInput::Move(TuiListMotion::Parent),
            FileSidebarCommand::Expand => TuiFileSidebarInput::Open,
            FileSidebarCommand::Collapse => TuiFileSidebarInput::Close,
            FileSidebarCommand::Activate => TuiFileSidebarInput::Activate,
            FileSidebarCommand::Refresh => TuiFileSidebarInput::Refresh,
            FileSidebarCommand::FocusBoundary(_) => unreachable!("focus commands return above"),
        };
        let before = self.inner().file_rows();
        match self.inner_mut().file_sidebar(input) {
            TuiFileSidebarOutcome::NotSelected => {
                FileSidebarOutcome::Applied(WorktreeUpdate::Unchanged)
            }
            TuiFileSidebarOutcome::Applied => {
                let update = if self.inner().file_rows() == before {
                    WorktreeUpdate::Unchanged
                } else {
                    WorktreeUpdate::Redraw
                };
                FileSidebarOutcome::Applied(update)
            }
            TuiFileSidebarOutcome::Activated { path } => {
                let update = convert_redraw(self.inner_mut().open_file(path.clone()));
                FileSidebarOutcome::Activated { path, update }
            }
        }
    }

    fn require_host_file_sidebar(&self) -> Result<(), FileSidebarOperationError> {
        if self.presentation.file_sidebar_ownership() == SurfaceOwnership::Embedded {
            return Err(FileSidebarOperationError::Embedded);
        }
        Ok(())
    }

    fn require_search_prompt(
        &self,
        id: FileSidebarSearchId,
    ) -> Result<(), FileSidebarOperationError> {
        self.require_host_file_sidebar()?;
        if id.instance != self.instance {
            return Err(FileSidebarOperationError::WrongInstance);
        }
        if self.file_sidebar_search_prompt != Some(id) {
            return Err(FileSidebarOperationError::StaleSearch);
        }
        Ok(())
    }

    fn require_active_search(
        &self,
        id: FileSidebarSearchId,
    ) -> Result<(), FileSidebarOperationError> {
        self.require_host_file_sidebar()?;
        if id.instance != self.instance {
            return Err(FileSidebarOperationError::WrongInstance);
        }
        if self.active_file_sidebar_search != Some(id) {
            return Err(FileSidebarOperationError::StaleSearch);
        }
        Ok(())
    }

    fn file_sidebar_match(
        &mut self,
        id: FileSidebarSearchId,
        operation: TuiFileSidebarOperation,
    ) -> Result<FileSidebarSearchOutcome, FileSidebarOperationError> {
        self.require_active_search(id)?;
        let before = self.inner().file_rows();
        Ok(match self.inner_mut().file_sidebar_operation(operation) {
            TuiFileSidebarOperationOutcome::Applied => {
                FileSidebarSearchOutcome::Applied(self.file_sidebar_update_since(before))
            }
            TuiFileSidebarOperationOutcome::SearchMissed => FileSidebarSearchOutcome::SearchMissed,
        })
    }

    fn file_sidebar_page(
        &mut self,
        operation: TuiFileSidebarOperation,
    ) -> Result<WorktreeUpdate, FileSidebarOperationError> {
        self.require_host_file_sidebar()?;
        let before = self.inner().file_rows();
        let outcome = self.inner_mut().file_sidebar_operation(operation);
        debug_assert_eq!(
            outcome,
            TuiFileSidebarOperationOutcome::Applied,
            "page operations always apply at the private boundary"
        );
        Ok(self.file_sidebar_update_since(before))
    }

    fn file_sidebar_update_since(&self, before: Vec<TuiFileRow>) -> WorktreeUpdate {
        if self.inner().file_rows() == before {
            WorktreeUpdate::Unchanged
        } else {
            WorktreeUpdate::Redraw
        }
    }

    /// Applies one normalized host input.
    ///
    /// An open dialog consumes every non-resize input. An accepted resize
    /// closes the dialog without an answer if its fixed body no longer fits.
    /// Host-resolved editors reject other raw keys, paste, and unsupported input.
    pub fn input(
        &mut self,
        input: WorktreeInput,
        now: Duration,
    ) -> Result<WorktreeUpdate, WorktreeInputError> {
        if self.dialog.is_open() {
            let dialog_input = match input {
                WorktreeInput::Key(key) => Some(DialogInput::Key(key)),
                WorktreeInput::Paste(_) => Some(DialogInput::Paste),
                WorktreeInput::Pointer(pointer) => Some(DialogInput::Pointer(pointer)),
                WorktreeInput::Unsupported => Some(DialogInput::Unsupported),
                WorktreeInput::Resize { .. } => None,
            };
            if let Some(input) = dialog_input {
                return Ok(match self.dialog.input(input) {
                    DialogInputOutcome::Redraw | DialogInputOutcome::Answered => {
                        WorktreeUpdate::Redraw
                    }
                    DialogInputOutcome::Consumed | DialogInputOutcome::NotOpen => {
                        WorktreeUpdate::Unchanged
                    }
                });
            }
        }
        if matches!(self.binding_mode, WorktreeBindingMode::HostResolved { .. })
            && !matches!(
                input,
                WorktreeInput::Resize { .. } | WorktreeInput::Pointer(_)
            )
        {
            return Err(WorktreeInputError::HostResolved);
        }
        let resized = matches!(input, WorktreeInput::Resize { .. });
        let input = match input {
            WorktreeInput::Key(key) => TuiTerminalEvent::Key(key),
            WorktreeInput::Paste(text) => TuiTerminalEvent::Paste(text),
            WorktreeInput::Pointer(pointer) => TuiTerminalEvent::Pointer(pointer),
            WorktreeInput::Resize { columns, rows } => TuiTerminalEvent::Resize { columns, rows },
            WorktreeInput::Unsupported => TuiTerminalEvent::Unsupported,
        };
        let update = convert_redraw(self.inner_mut().input(input, now));
        if resized {
            let area = self.area();
            if !self.dialog.body_fits(area) {
                let closed = self.dialog.close();
                debug_assert!(closed, "a non-fitting body implies an open dialog");
            } else {
                self.dialog.invalidate();
            }
        }
        Ok(update)
    }
    /// Applies one terminal-neutral pointer event.
    ///
    /// This method bypasses physical key-binding arbitration and remains
    /// available in both binding modes.
    #[must_use]
    pub fn pointer(&mut self, pointer: PointerEvent, now: Duration) -> WorktreeUpdate {
        if self.dialog.is_open() {
            return match self.dialog.input(DialogInput::Pointer(pointer)) {
                DialogInputOutcome::Redraw | DialogInputOutcome::Answered => WorktreeUpdate::Redraw,
                DialogInputOutcome::Consumed | DialogInputOutcome::NotOpen => {
                    WorktreeUpdate::Unchanged
                }
            };
        }
        convert_redraw(
            self.inner_mut()
                .input(TuiTerminalEvent::Pointer(pointer), now),
        )
    }

    /// Applies one direct semantic command.
    ///
    /// This method does not claim that a physical binding resolved in the
    /// current scope. It remains available in both binding modes for host
    /// actions such as menus and command palettes. Use [`Self::semantic_dispatch`]
    /// for a command produced by the host-owned physical resolver.
    ///
    /// Returns [`WorktreeCommandError::InvalidRegisterName`] before changing
    /// editor state when `register` is not a canonical register name.
    pub fn command(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
        register: Option<char>,
        now: Duration,
    ) -> Result<WorktreeInputOutcome, WorktreeCommandError> {
        if self.dialog.is_open() {
            return Ok(WorktreeInputOutcome::Applied);
        }
        if let Some(name) = register
            && !is_register_name(name)
        {
            return Err(WorktreeCommandError::InvalidRegisterName { name });
        }
        Ok(convert_reduction(
            self.inner_mut().command(command, count, register, now),
        ))
    }
    /// Inserts direct semantic text in the active input context.
    ///
    /// This method bypasses physical binding resolution. It remains available
    /// in both binding modes for host-owned text entry.
    pub fn literal(&mut self, text: &str, now: Duration) -> WorktreeInputOutcome {
        if self.dialog.is_open() {
            return WorktreeInputOutcome::Applied;
        }
        convert_reduction(self.inner_mut().insert_literal(text, now))
    }
    /// Applies direct semantic pasted text.
    ///
    /// This method bypasses physical binding resolution. In host-resolved mode,
    /// the host must arbitrate the physical paste event before it calls this
    /// method. [`Self::input`] rejects the raw paste path in that mode.
    pub fn paste(&mut self, text: &PasteText, now: Duration) -> WorktreeInputOutcome {
        if self.dialog.is_open() {
            return WorktreeInputOutcome::Applied;
        }
        convert_reduction(self.inner_mut().paste(text, now))
    }
    /// Returns the bounded editor command catalog for the current context.
    ///
    /// The host can merge these descriptors with its own catalog. Qualified
    /// names remain distinct when an unqualified host name collides.
    ///
    /// ```
    /// use kvim_embed::{EditorCommandAvailability, WorktreeEditor};
    /// use ratatui::layout::Rect;
    ///
    /// let directory = std::env::temp_dir().join("kvim-command-catalog-example");
    /// std::fs::create_dir_all(&directory)?;
    /// let editor = WorktreeEditor::builder(&directory, Rect::new(0, 0, 40, 6)).open()?;
    /// let catalog = editor.command_catalog();
    /// assert!(catalog.descriptors().iter().all(|entry| entry.qualified_name().starts_with("editor.")));
    /// assert!(catalog.descriptors().iter().any(|entry| entry.availability() == EditorCommandAvailability::Available));
    /// # std::fs::remove_dir_all(directory)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn command_catalog(&self) -> EditorCommandCatalog {
        let has_file = self.status().path().is_some();
        let writable = self.status().access() == WorktreeAccess::ReadWrite;
        let write_availability = if writable && has_file {
            EditorCommandAvailability::Available
        } else if !writable {
            EditorCommandAvailability::RequiresWriteAccess
        } else {
            EditorCommandAvailability::RequiresFile
        };
        let file_availability = if has_file {
            EditorCommandAvailability::Available
        } else {
            EditorCommandAvailability::RequiresFile
        };
        let descriptors = vec![
            command_descriptor(
                EditorCommandId::Write,
                "write",
                &["w"],
                "Write the active file",
                EditorCommandArguments::None,
                write_availability,
                EditorCommandCompletion::None,
            ),
            command_descriptor(
                EditorCommandId::Quit,
                "quit",
                &["q"],
                "Close the focused window",
                EditorCommandArguments::None,
                EditorCommandAvailability::Available,
                EditorCommandCompletion::None,
            ),
            command_descriptor(
                EditorCommandId::QuitDiscard,
                "quit!",
                &["q!"],
                "Close and discard unsaved changes",
                EditorCommandArguments::None,
                EditorCommandAvailability::Available,
                EditorCommandCompletion::None,
            ),
            command_descriptor(
                EditorCommandId::WriteQuit,
                "wq",
                &[],
                "Write, then close the focused window",
                EditorCommandArguments::None,
                write_availability,
                EditorCommandCompletion::None,
            ),
            command_descriptor(
                EditorCommandId::Edit,
                "edit",
                &["e"],
                "Open a contained file",
                EditorCommandArguments::ContainedPath,
                EditorCommandAvailability::Available,
                EditorCommandCompletion::ContainedPath,
            ),
            command_descriptor(
                EditorCommandId::Reload,
                "reload",
                &[],
                "Reload the active file",
                EditorCommandArguments::None,
                file_availability,
                EditorCommandCompletion::None,
            ),
            command_descriptor(
                EditorCommandId::ReloadDiscard,
                "reload!",
                &[],
                "Reload and discard unsaved changes",
                EditorCommandArguments::None,
                file_availability,
                EditorCommandCompletion::None,
            ),
            command_descriptor(
                EditorCommandId::Log,
                "logs",
                &["l"],
                "Open the editor log",
                EditorCommandArguments::None,
                EditorCommandAvailability::Available,
                EditorCommandCompletion::None,
            ),
            command_descriptor(
                EditorCommandId::Diagnostics,
                "diagnostics",
                &["d"],
                "Open host diagnostics",
                EditorCommandArguments::None,
                EditorCommandAvailability::Available,
                EditorCommandCompletion::None,
            ),
            command_descriptor(
                EditorCommandId::GoToLine,
                "line",
                &[],
                "Move to a one-based line",
                EditorCommandArguments::LineNumber,
                EditorCommandAvailability::Available,
                EditorCommandCompletion::None,
            ),
        ];
        debug_assert!(
            descriptors.len() <= EDITOR_COMMAND_DESCRIPTORS_MAX,
            "the fixed command catalog respects its published bound"
        );
        EditorCommandCatalog {
            instance: self.instance,
            generation: self.input_context().generation,
            descriptors,
        }
    }

    /// Queues asynchronous contained-path completion for a host-owned line.
    ///
    /// Call [`Self::dispatch`], await [`Self::ready`], and call [`Self::apply`].
    /// Then poll [`Self::take_command_completion`]. A newer request cancels and
    /// makes the prior request obsolete. The host retains line editing and history.
    pub fn request_command_completion(
        &mut self,
        session: EditorCommandSessionId,
        request: EditorCommandRequestId,
        line: &str,
    ) -> Result<(), EditorCommandSessionError> {
        if !self.inner().host_command_session_is_current(session.get()) {
            return Err(EditorCommandSessionError::StaleSession);
        }
        if CommandLineCommand::path_argument(line).is_none()
            || line.chars().count() > kvim_input::COMMAND_LINE_CHARS_MAX
        {
            return Err(EditorCommandSessionError::InvalidCompletion);
        }
        let queued =
            self.inner_mut()
                .request_host_command_completion(session.get(), request.get(), line);
        debug_assert!(
            queued,
            "the facade validates the session, request, and command line before queuing"
        );
        Ok(())
    }

    /// Takes the newest finished path completion, if one was applied.
    #[must_use]
    pub fn take_command_completion(&mut self) -> Option<EditorCommandPathCompletion> {
        let (session, request, candidates) = self.inner_mut().take_host_command_completion()?;
        debug_assert!(
            candidates.len() <= EDITOR_COMMAND_COMPLETION_CANDIDATES_MAX,
            "the internal completion producer applies the facade candidate bound"
        );
        Some(EditorCommandPathCompletion {
            instance: self.instance,
            session: EditorCommandSessionId(session),
            request: EditorCommandRequestId(request),
            candidates,
        })
    }

    /// Closes one host-owned command line and cancels its pending path request.
    ///
    /// The returned context lets the host validate that kvim has no semantic
    /// input to cancel before it changes focus.
    pub fn close_command_session(
        &mut self,
        session: EditorCommandSessionId,
    ) -> Result<InputContextSnapshot<BindingScope>, EditorCommandSessionError> {
        if !self.inner_mut().close_host_command_session(session.get()) {
            return Err(EditorCommandSessionError::StaleSession);
        }
        Ok(self.input_context())
    }

    /// Parses and executes one addressed command from a host-owned session.
    pub fn execute_session_command(
        &mut self,
        session: EditorCommandSessionId,
        addressed: AddressedEditorCommand,
        line: &str,
    ) -> Result<WorktreeUpdate, EditorCommandExecutionError> {
        if !self.inner().host_command_session_is_current(session.get()) {
            return Err(EditorCommandExecutionError::StaleGeneration);
        }
        let update = self.execute_addressed_command(addressed, line)?;
        let _closed = self.inner_mut().close_host_command_session(session.get());
        debug_assert!(
            _closed,
            "the session was validated before command execution"
        );
        Ok(update)
    }

    /// Parses and executes one addressed editor command line.
    pub fn execute_addressed_command(
        &mut self,
        addressed: AddressedEditorCommand,
        line: &str,
    ) -> Result<WorktreeUpdate, EditorCommandExecutionError> {
        if addressed.instance != self.instance {
            return Err(EditorCommandExecutionError::WrongInstance);
        }
        if addressed.generation != self.input_context().generation {
            return Err(EditorCommandExecutionError::StaleGeneration);
        }
        let descriptor = self
            .command_catalog()
            .descriptors
            .into_iter()
            .find(|entry| entry.id == addressed.id)
            .expect("the addressed command ID belongs to the fixed catalog");
        if descriptor.availability != EditorCommandAvailability::Available {
            return Err(EditorCommandExecutionError::Unavailable);
        }
        let parsed = CommandLineCommand::parse(line)?;
        if command_line_id(&parsed) != addressed.id {
            return Err(EditorCommandExecutionError::IdentityMismatch);
        }
        Ok(convert_redraw(
            self.inner_mut().run_command_line_command(parsed),
        ))
    }

    /// Returns one cheap semantic status snapshot.
    ///
    /// Poll this after [`WorktreeUpdate::Redraw`] when the host owns statusline
    /// presentation. The snapshot borrows the active path and allocates no text.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_embed::{EditorFormatterState, WorktreeEditor};
    /// use ratatui::layout::Rect;
    ///
    /// let directory = std::env::temp_dir().join("kvim-status-example");
    /// std::fs::create_dir_all(&directory)?;
    /// let editor = WorktreeEditor::builder(&directory, Rect::new(0, 0, 40, 6)).open()?;
    /// let status = editor.status();
    /// assert_eq!(status.instance(), editor.instance());
    /// assert_eq!(status.cursor().line(), 1);
    /// assert_eq!(status.formatter(), EditorFormatterState::Unavailable);
    /// # std::fs::remove_dir_all(directory)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn status(&self) -> EditorStatusSnapshot<'_> {
        let status = self.inner().status();
        let diagnostics = status.diagnostics;
        EditorStatusSnapshot {
            instance: WorktreeInstanceId(status.instance.get()),
            mode: status.mode,
            path: status.path,
            modified: status.modified,
            cursor: EditorCursorPosition {
                line: status.cursor.line().get() + 1,
                column: status.cursor.column().get() + 1,
            },
            access: match status.access {
                TuiEditorAccess::ReadWrite => WorktreeAccess::ReadWrite,
                TuiEditorAccess::ViewOnly => WorktreeAccess::ViewOnly,
            },
            diagnostics: EditorDiagnosticSummary {
                errors: diagnostics.errors,
                warnings: diagnostics.warnings,
                information: diagnostics.information,
                hints: diagnostics.hints,
            },
            formatter: match status.formatter {
                TuiFormatterStatus::Unavailable => EditorFormatterState::Unavailable,
                TuiFormatterStatus::AvailableDisabled => EditorFormatterState::AvailableDisabled,
                TuiFormatterStatus::AvailableEnabled => EditorFormatterState::AvailableEnabled,
            },
        }
    }

    /// Returns the current modal mode.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.inner().mode()
    }
    /// Returns the current resolved-input context.
    #[must_use]
    pub fn input_context(&self) -> InputContextSnapshot<BindingScope> {
        let inner = self.inner().input_context();
        let dialog_generation = self.dialog.generation();
        if inner.generation != self.inner_generation.get()
            || dialog_generation != self.dialog_generation.get()
        {
            self.inner_generation.set(inner.generation);
            self.dialog_generation.set(dialog_generation);
            self.facade_generation
                .set(self.facade_generation.get().advanced());
        }
        let generation = self.facade_generation.get();
        if self.dialog.is_open() {
            InputContextSnapshot {
                generation,
                ..InputContextSnapshot::idle(BindingScope::Confirmation)
            }
        } else {
            InputContextSnapshot {
                generation,
                ..inner
            }
        }
    }
    /// Returns bounded changing metadata for host-owned resolution.
    #[must_use]
    pub fn binding_context(&self) -> Option<WorktreeBindingContext> {
        let WorktreeBindingMode::HostResolved { reserved_escape } = self.binding_mode else {
            return None;
        };
        let (inner_context, overlay_scope) = self.inner().binding_context();
        let context = self.input_context();
        debug_assert_eq!(
            inner_context.generation,
            self.inner_generation.get(),
            "input_context records the current private generation"
        );
        Some(WorktreeBindingContext {
            instance: self.instance,
            context,
            overlay_scope: if self.dialog.is_open() {
                None
            } else {
                overlay_scope
            },
            reserved_escape,
        })
    }
    /// Returns the immutable embedded binding manifest for host-owned resolution.
    ///
    /// Polling this accessor does not clone or allocate binding entries.
    #[must_use]
    pub fn binding_manifest(&self) -> Option<&BindingManifest> {
        matches!(self.binding_mode, WorktreeBindingMode::HostResolved { .. })
            .then_some(self.manifest.as_ref())
    }
    /// Applies one addressed physical-resolution decision.
    ///
    /// Unlike [`Self::command`], this method validates a completed command
    /// against the active focus and overlay scopes. Every decision is bound to
    /// the instance and context generation that produced it.
    pub fn semantic_dispatch(
        &mut self,
        dispatch: WorktreeSemanticDispatch,
        now: Duration,
    ) -> Result<WorktreeDispatchOutcome, WorktreeDispatchError> {
        if self.dialog.is_open() {
            let current = self.input_context();
            if dispatch.instance != self.instance {
                return Err(WorktreeDispatchError::WrongInstance);
            }
            if dispatch.generation != current.generation {
                return Err(WorktreeDispatchError::StaleGeneration);
            }
            return Ok(WorktreeDispatchOutcome::Consumed);
        }
        if !matches!(self.binding_mode, WorktreeBindingMode::HostResolved { .. }) {
            return Err(WorktreeDispatchError::FacadeResolved);
        }
        let current = self.input_context();
        if dispatch.instance != self.instance {
            return Err(WorktreeDispatchError::WrongInstance);
        }
        if dispatch.generation != current.generation {
            return Err(WorktreeDispatchError::StaleGeneration);
        }
        let outcome = match dispatch.decision {
            WorktreeDispatchDecision::Interrupted => {
                if current.phases.is_idle() {
                    return Err(WorktreeDispatchError::NoPending);
                }
                return Ok(WorktreeDispatchOutcome::Interrupted(
                    CancelPendingProposal {
                        instance: self.instance,
                        generation: current.generation,
                    },
                ));
            }
            WorktreeDispatchDecision::Complete { command } => {
                let (_, overlay_scope) = self.inner().binding_context();
                if !resolved_command_is_valid(
                    self.manifest.as_ref(),
                    current.scope,
                    overlay_scope,
                    command,
                ) {
                    return Err(WorktreeDispatchError::InvalidResolvedCommand);
                }
                WorktreeDispatchOutcome::Complete(convert_reduction(
                    self.inner_mut()
                        .semantic_dispatch(Dispatch::Surface { command }, now),
                ))
            }
            WorktreeDispatchDecision::Pending => WorktreeDispatchOutcome::Pending,
            WorktreeDispatchDecision::TextObjectPending => {
                if !current.scope.binds_text_objects() {
                    return Err(WorktreeDispatchError::InvalidResolvedCommand);
                }
                let _ = self.inner_mut().semantic_dispatch(Dispatch::Pending, now);
                WorktreeDispatchOutcome::Pending
            }
            WorktreeDispatchDecision::TextFallback(text) => {
                if current.text_fallback.owner() != Some(CommandOwner::Surface) {
                    return Err(WorktreeDispatchError::InvalidResolvedCommand);
                }
                WorktreeDispatchOutcome::TextFallback(convert_reduction(
                    self.inner_mut().semantic_dispatch(
                        Dispatch::Text {
                            owner: CommandOwner::Surface,
                            text,
                        },
                        now,
                    ),
                ))
            }
            WorktreeDispatchDecision::Unbound => {
                let _ = self.inner_mut().semantic_dispatch(Dispatch::Unbound, now);
                WorktreeDispatchOutcome::Unbound
            }
        };
        Ok(outcome)
    }
    /// Atomically clears pending editor input for one addressed proposal.
    ///
    /// The transition applies all cancellation effects, including operator
    /// cancellation, before it returns the validated idle context.
    pub fn cancel_pending(
        &mut self,
        proposal: CancelPendingProposal,
        now: Duration,
    ) -> Result<CancelPendingResume, WorktreeDispatchError> {
        if proposal.instance != self.instance {
            return Err(WorktreeDispatchError::WrongInstance);
        }
        let before = self.input_context();
        if proposal.generation != before.generation {
            return Err(WorktreeDispatchError::StaleGeneration);
        }
        if before.phases.is_idle() {
            return Err(WorktreeDispatchError::NoPending);
        }
        let _ = self.inner_mut().cancel_pending(now);
        let context = self.input_context();
        assert!(
            context.phases.is_idle(),
            "the semantic reducer cancel transition clears every pending phase"
        );
        assert_ne!(
            context.generation, before.generation,
            "the semantic reducer cancel transition always advances its generation"
        );
        Ok(CancelPendingResume {
            instance: self.instance,
            context,
        })
    }
    /// Returns the next host elapsed-time deadline.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        self.inner().next_deadline()
    }
    /// Applies a host elapsed-time transition.
    pub fn tick(&mut self, now: Duration) -> WorktreeUpdate {
        convert_redraw(self.inner_mut().tick(now))
    }
    /// Reports whether input remains active.
    #[must_use]
    pub fn run_state(&self) -> WorktreeRunState {
        match self.inner().run_state() {
            TuiRunState::Running => WorktreeRunState::Running,
            TuiRunState::Finished => WorktreeRunState::ExitRequested,
        }
    }
    /// Renders the editor and an open host dialog into host-owned cells.
    ///
    /// Successful request and resize validation make dialog composition
    /// infallible after the base editor accepts the target buffer.
    pub fn render(&self, cells: &mut Buffer) -> Result<WorktreeCursor, WorktreeGeometryError> {
        let cursor = self.inner().draw(cells, self.inner().area())?;
        self.dialog
            .render(cells)
            .expect("dialog request and accepted resize keep render geometry valid");
        Ok(WorktreeCursor {
            position: cursor.position,
            shape: match cursor.shape {
                TuiCursorShape::Block => WorktreeCursorShape::Block,
                TuiCursorShape::Bar => WorktreeCursorShape::Bar,
            },
        })
    }
    /// Takes one facade-owned event.
    #[must_use]
    pub fn take_event(&mut self) -> Option<WorktreeEvent> {
        if let Some(answer) = self.dialog.take_answer() {
            return Some(WorktreeEvent::DialogAnswered(answer));
        }
        self.inner_mut().take_event().map(convert_published)
    }
    /// Resolves one recovery event by its facade-owned address.
    ///
    /// A wrong editor or stale identity returns before visible state changes or
    /// another candidate is consumed.
    pub fn decide_recovery(
        &mut self,
        id: &WorktreeRecoveryId,
        decision: WorktreeRecoveryDecision,
    ) -> Result<WorktreeRecoveryOutcome, WorktreeRecoveryError> {
        if id.instance != self.instance {
            return Err(WorktreeRecoveryError::WrongInstance);
        }
        let decision = match decision {
            WorktreeRecoveryDecision::Restore => TuiRecoveryDecision::Restore,
            WorktreeRecoveryDecision::Discard => TuiRecoveryDecision::Discard,
            WorktreeRecoveryDecision::Defer => TuiRecoveryDecision::Defer,
        };
        self.inner_mut()
            .decide_recovery(&id.inner, decision)
            .map_err(|error| match error {
                TuiRecoveryDecisionError::WrongInstance => WorktreeRecoveryError::WrongInstance,
                TuiRecoveryDecisionError::Stale => WorktreeRecoveryError::Stale,
                TuiRecoveryDecisionError::RestoreForbidden => {
                    WorktreeRecoveryError::RestoreForbidden
                }
            })?;
        Ok(match decision {
            TuiRecoveryDecision::Restore => WorktreeRecoveryOutcome::Restored,
            TuiRecoveryDecision::Discard => WorktreeRecoveryOutcome::Discarded,
            TuiRecoveryDecision::Defer => WorktreeRecoveryOutcome::Deferred,
        })
    }

    /// Submits all queued work to owned bounded execution capacity.
    pub fn dispatch(&mut self) -> WorktreeUpdate {
        let runtime = self
            .runtime
            .as_ref()
            .expect("shutdown consumes the executor");
        let _guard = runtime.enter();
        convert_redraw(self.inner_mut().dispatch())
    }
    /// Waits until one internal result is ready.
    pub async fn ready(&mut self) -> WorktreeCompletion {
        let runtime = self
            .runtime
            .as_ref()
            .expect("shutdown consumes the executor");
        let _guard = runtime.enter();
        let completion = self.inner_mut().recv().await;
        WorktreeCompletion {
            instance: WorktreeInstanceId(completion.instance().get()),
            inner: completion,
        }
    }
    /// Applies one completion returned by this editor.
    ///
    /// Returns [`WorktreeApplyErrorKind::WrongInstance`] before any mutation when
    /// another editor produced the completion.
    #[allow(clippy::result_large_err)]
    pub fn apply(
        &mut self,
        completion: WorktreeCompletion,
        now: Duration,
    ) -> Result<WorktreeUpdate, WorktreeApplyError> {
        if self.instance != completion.instance {
            return Err(WorktreeApplyError {
                kind: WorktreeApplyErrorKind::WrongInstance {
                    editor: self.instance,
                    completion: completion.instance,
                },
                completion,
            });
        }
        let redraw = self
            .inner_mut()
            .apply(completion.inner, now)
            .expect("facade identity validation matches the internal owner");
        Ok(convert_redraw(redraw))
    }
    /// Consumes the editor and performs bounded shutdown.
    pub async fn shutdown(mut self, deadline: Duration) -> WorktreeShutdown {
        let inner = self
            .inner
            .take()
            .expect("shutdown consumes one live editor");
        let runtime = self
            .runtime
            .as_ref()
            .expect("shutdown consumes the executor");
        let _guard = runtime.enter();
        match inner.shutdown(deadline).await {
            TuiEditorShutdown::Finished { events } => {
                drop(_guard);
                self.runtime
                    .take()
                    .expect("shutdown owns the executor")
                    .shutdown_background();
                WorktreeShutdown::Finished {
                    events: events.into_iter().map(convert_published).collect(),
                }
            }
            TuiEditorShutdown::Draining(drain) => {
                drop(_guard);
                let runtime = self.runtime.take().expect("shutdown owns the executor");
                WorktreeShutdown::Draining(Box::new(WorktreeDrain {
                    runtime,
                    drain: *drain,
                }))
            }
        }
    }
}

impl Drop for WorktreeEditor {
    fn drop(&mut self) {
        let inner = self.inner.take();
        if inner.is_none() {
            return;
        }
        drop(inner);
        self.runtime
            .take()
            .expect("a live editor owns its executor")
            .shutdown_background();
    }
}

fn convert_source_refusal(refusal: TuiSourcePresentationRefusal) -> SourcePresentationError {
    match refusal {
        TuiSourcePresentationRefusal::NoEditor => SourcePresentationError::NoEditor,
        TuiSourcePresentationRefusal::DifferentDirtyBuffer => {
            SourcePresentationError::DifferentDirtyBuffer
        }
        TuiSourcePresentationRefusal::Busy => SourcePresentationError::Busy,
        TuiSourcePresentationRefusal::RangeOutsideBuffer => {
            SourcePresentationError::RangeOutsideBuffer
        }
        TuiSourcePresentationRefusal::AtFirst => SourcePresentationError::AtFirst,
        TuiSourcePresentationRefusal::AtLast => SourcePresentationError::AtLast,
        TuiSourcePresentationRefusal::NoPresentation => SourcePresentationError::NoPresentation,
        TuiSourcePresentationRefusal::OpenFailed => SourcePresentationError::OpenFailed,
    }
}

fn convert_file_sidebar_identity(id: &FileSidebarRowId) -> TuiFileRowIdentity {
    match &id.0 {
        FileSidebarRowIdentity::Entry(path) => TuiFileRowIdentity::Entry(path.clone()),
        FileSidebarRowIdentity::Notice { parent, kind } => TuiFileRowIdentity::Notice {
            parent: parent.clone(),
            kind: match kind {
                FileSidebarNoticeKind::Truncated => TuiFileRowNoticeKind::Truncated,
                FileSidebarNoticeKind::Unreadable => TuiFileRowNoticeKind::Unreadable,
                FileSidebarNoticeKind::Hidden => TuiFileRowNoticeKind::Hidden,
            },
        },
    }
}

fn convert_file_sidebar_row(row: TuiFileRow) -> FileSidebarRow {
    let id = FileSidebarRowId(match row.identity() {
        TuiFileRowIdentity::Entry(path) => FileSidebarRowIdentity::Entry(path.clone()),
        TuiFileRowIdentity::Notice { parent, kind } => FileSidebarRowIdentity::Notice {
            parent: parent.clone(),
            kind: match kind {
                TuiFileRowNoticeKind::Truncated => FileSidebarNoticeKind::Truncated,
                TuiFileRowNoticeKind::Unreadable => FileSidebarNoticeKind::Unreadable,
                TuiFileRowNoticeKind::Hidden => FileSidebarNoticeKind::Hidden,
            },
        },
    });
    let kind = match row.kind() {
        TuiFileRowKind::File => FileSidebarRowKind::File,
        TuiFileRowKind::ClosedDirectory => FileSidebarRowKind::DirectoryCollapsed,
        TuiFileRowKind::OpenDirectory => FileSidebarRowKind::DirectoryExpanded,
        TuiFileRowKind::LoadingDirectory => FileSidebarRowKind::DirectoryLoading,
        TuiFileRowKind::Note => FileSidebarRowKind::Notice(
            match row
                .notice_kind()
                .expect("the private row type gives every notice a typed identity")
            {
                TuiFileRowNoticeKind::Truncated => FileSidebarNoticeKind::Truncated,
                TuiFileRowNoticeKind::Unreadable => FileSidebarNoticeKind::Unreadable,
                TuiFileRowNoticeKind::Hidden => FileSidebarNoticeKind::Hidden,
            },
        ),
    };
    let git = row.git().map(|git| match git {
        TuiFileRowGit::Ignored => FileSidebarGitState::Ignored,
        TuiFileRowGit::Untracked => FileSidebarGitState::Untracked,
        TuiFileRowGit::Staged => FileSidebarGitState::Staged,
        TuiFileRowGit::Modified => FileSidebarGitState::Modified,
        TuiFileRowGit::StagedAndModified => FileSidebarGitState::StagedAndModified,
        TuiFileRowGit::Conflicted => FileSidebarGitState::Conflicted,
    });
    let icon = row.icon_role().map(|icon| match icon {
        TuiIconRole::Directory => FileSidebarIconRole::Directory,
        TuiIconRole::Code => FileSidebarIconRole::Code,
        TuiIconRole::Configuration => FileSidebarIconRole::Configuration,
        TuiIconRole::Document => FileSidebarIconRole::Document,
        TuiIconRole::Script => FileSidebarIconRole::Script,
        TuiIconRole::VersionControl => FileSidebarIconRole::VersionControl,
        TuiIconRole::Generated => FileSidebarIconRole::Generated,
        TuiIconRole::Media => FileSidebarIconRole::Media,
        TuiIconRole::Unknown => FileSidebarIconRole::Unknown,
        TuiIconRole::CommandSearch
        | TuiIconRole::CommandCode
        | TuiIconRole::CommandWindow
        | TuiIconRole::CommandBuffer
        | TuiIconRole::CommandTree
        | TuiIconRole::CommandReview
        | TuiIconRole::CommandOther => {
            unreachable!("file-tree rows cannot carry command icon roles")
        }
    });
    let dimming = row.dimming().map(|dimming| match dimming {
        TuiFileRowDimming::Generated => FileSidebarDimming::Generated,
        TuiFileRowDimming::HeldCopy => FileSidebarDimming::Held(WorkspaceTransfer::Copy),
        TuiFileRowDimming::HeldMove => FileSidebarDimming::Held(WorkspaceTransfer::Move),
    });
    let matched = row
        .matched_characters()
        .map(|(start, len)| FileSidebarLabelMatch { start, len });
    FileSidebarRow {
        id,
        label: row.label().to_owned(),
        path: row.path().cloned(),
        depth: row.depth(),
        kind,
        selected: row.is_selected(),
        git,
        symlink: if row.is_symlink() {
            FileSidebarSymlinkState::Symlink
        } else {
            FileSidebarSymlinkState::Direct
        },
        dimming,
        matched,
        icon,
        icon_glyph: row.icon_glyph(),
    }
}

fn command_descriptor(
    id: EditorCommandId,
    name: &'static str,
    aliases: &'static [&'static str],
    description: &'static str,
    arguments: EditorCommandArguments,
    availability: EditorCommandAvailability,
    completion: EditorCommandCompletion,
) -> EditorCommandDescriptor {
    debug_assert!(
        aliases.len() <= 2,
        "the fixed catalog bounds aliases per command"
    );
    EditorCommandDescriptor {
        id,
        name,
        aliases,
        description,
        arguments,
        availability,
        completion,
    }
}

fn command_line_id(command: &CommandLineCommand) -> EditorCommandId {
    match command {
        CommandLineCommand::Write => EditorCommandId::Write,
        CommandLineCommand::WriteQuit => EditorCommandId::WriteQuit,
        CommandLineCommand::Quit => EditorCommandId::Quit,
        CommandLineCommand::QuitDiscard => EditorCommandId::QuitDiscard,
        CommandLineCommand::Edit(_) => EditorCommandId::Edit,
        CommandLineCommand::Reload => EditorCommandId::Reload,
        CommandLineCommand::ReloadDiscard => EditorCommandId::ReloadDiscard,
        CommandLineCommand::Log => EditorCommandId::Log,
        CommandLineCommand::Diagnostics => EditorCommandId::Diagnostics,
        CommandLineCommand::GoToLine(_) => EditorCommandId::GoToLine,
    }
}

fn convert_redraw(redraw: TuiRedraw) -> WorktreeUpdate {
    match redraw {
        TuiRedraw::Skipped => WorktreeUpdate::Unchanged,
        TuiRedraw::Needed => WorktreeUpdate::Redraw,
    }
}
fn resolved_command_is_valid(
    manifest: &BindingManifest,
    focus_scope: BindingScope,
    overlay_scope: Option<BindingScope>,
    command: Command,
) -> bool {
    manifest.entries().iter().any(|entry| {
        entry.command() == command
            && (entry.scope() == focus_scope || overlay_scope == Some(entry.scope()))
    })
}

fn convert_reduction(reduction: TuiReduction) -> WorktreeInputOutcome {
    match reduction.outcome {
        TuiReductionOutcome::Applied => WorktreeInputOutcome::Applied,
        TuiReductionOutcome::Request(request) => WorktreeInputOutcome::Request(match request {
            TuiInputRequest::OpenCommandLine { session } => {
                WorktreeInputRequest::OpenCommandLine(EditorCommandSessionId(session))
            }
            TuiInputRequest::FocusBoundary(direction) => {
                WorktreeInputRequest::FocusBoundary(direction)
            }
            TuiInputRequest::CloseRequested => WorktreeInputRequest::CloseRequested,
        }),
        TuiReductionOutcome::Refused(refusal) => WorktreeInputOutcome::Refused(match refusal {
            TuiRefusal::ViewOnly => WorktreeRefusal::ViewOnly,
            TuiRefusal::Saturated => WorktreeRefusal::Saturated,
        }),
    }
}
fn convert_published(published: TuiPublishedEvent) -> WorktreeEvent {
    let instance = published.instance;
    match published.event {
        TuiEditorEvent::ActiveFileChanged { path } => WorktreeEvent::ActiveFileChanged { path },
        TuiEditorEvent::FileWritten { path } => WorktreeEvent::FileWritten { path },
        TuiEditorEvent::WorkspaceChanged { operation } => WorktreeEvent::WorkspaceChanged {
            operation: convert_workspace_operation(operation),
        },
        TuiEditorEvent::SaveReconciliationRequired { path } => {
            WorktreeEvent::SaveReconciliationRequired { path }
        }
        TuiEditorEvent::WorkspaceReconciliationRequired { operation } => {
            WorktreeEvent::WorkspaceReconciliationRequired {
                operation: convert_workspace_operation(operation),
            }
        }
        TuiEditorEvent::RecoveryCandidate { identity, status } => {
            debug_assert_eq!(
                identity.instance().get(),
                instance.get(),
                "the session publishes only recovery identities that it owns"
            );
            let path = identity.path().clone();
            WorktreeEvent::RecoveryCandidate {
                id: WorktreeRecoveryId {
                    instance: WorktreeInstanceId(instance.get()),
                    inner: identity,
                },
                path,
                status: match status {
                    TuiRecoveryStatus::Current => WorktreeRecoveryStatus::Current,
                    TuiRecoveryStatus::Stale => WorktreeRecoveryStatus::Stale,
                },
            }
        }
        TuiEditorEvent::FileActivated { path } => WorktreeEvent::FileActivated { path },
        TuiEditorEvent::RedrawRequested => WorktreeEvent::RedrawRequested,
        TuiEditorEvent::FocusBoundary(direction) => WorktreeEvent::FocusBoundary(direction),
        TuiEditorEvent::CloseRequested => WorktreeEvent::CloseRequested,
    }
}

fn convert_workspace_operation(operation: FileOperation) -> WorkspaceOperation {
    match operation {
        FileOperation::Create { path, kind } => WorkspaceOperation {
            data: WorkspaceOperationData::Create {
                path,
                kind: match kind {
                    EntryKind::File => WorkspaceEntryKind::File,
                    EntryKind::Directory => WorkspaceEntryKind::Directory,
                },
            },
        },
        FileOperation::Delete { paths } => {
            debug_assert!(
                paths.len() <= WORKSPACE_OPERATION_PATHS_MAX,
                "workspace validates mutation path bounds before publication"
            );
            WorkspaceOperation {
                data: WorkspaceOperationData::Delete { paths },
            }
        }
        FileOperation::Rename { from, to } => WorkspaceOperation {
            data: WorkspaceOperationData::Rename { from, to },
        },
        FileOperation::Transfer {
            mode,
            sources,
            destination,
        } => {
            debug_assert!(
                sources.len() <= WORKSPACE_OPERATION_PATHS_MAX,
                "workspace validates mutation path bounds before publication"
            );
            WorkspaceOperation {
                data: WorkspaceOperationData::Transfer {
                    mode: match mode {
                        TransferMode::Copy => WorkspaceTransfer::Copy,
                        TransferMode::Move => WorkspaceTransfer::Move,
                    },
                    sources,
                    destination: destination.relative_path().cloned(),
                },
            }
        }
    }
}

impl From<TuiGeometryError> for WorktreeGeometryError {
    fn from(error: TuiGeometryError) -> Self {
        match error {
            TuiGeometryError::Empty { area } => Self::Empty { area },
            TuiGeometryError::OutsideBuffer { area, buffer } => {
                Self::OutsideBuffer { area, buffer }
            }
            TuiGeometryError::Unreconciled { area, accepted } => {
                Self::Unreconciled { area, accepted }
            }
        }
    }
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;
