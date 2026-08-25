//! The visible editor state and the pure transitions of the event loop.
//!
//! [`Session`] owns every value that the terminal shows: the loaded buffers,
//! the window tree, the editing state, the input resolver, the active search,
//! the open prompt, the last message, and the bounded log of every report that
//! the editor made. It performs no filesystem work, no
//! process work, and no language work, so the event loop stays inside the
//! latency budget of `docs/responsiveness.md`.
//!
//! A file command builds one [`FileRequest`] and puts it in the outbox. The
//! event loop takes that request, hands it to the bounded worker service, and
//! returns the typed result to [`Session::apply_file_result`]. See
//! `docs/files.md`.
//!
//! A buffer that a language adapter serves builds one [`AnalysisRequest`] on
//! the same path. The result reaches [`Session::apply_analysis_result`], which
//! rejects a result for an obsolete buffer version. Parsing therefore never
//! runs on the event loop. See `docs/language-services.md`.
//!
//! A yank, a delete, and a change write the unnamed register, and a paste reads
//! the system clipboard. Both directions leave the event loop through
//! [`Session::take_clipboard_request`] and return through
//! [`Session::apply_clipboard_result`], so no clipboard command ever runs on
//! this path. See `docs/clipboard.md`.
//!
//! The session reads no clock. The event loop measures the elapsed time and
//! passes it in, which keeps every transition deterministic and testable.

use std::collections::{BTreeMap, VecDeque};
use std::mem;
use std::num::NonZeroU8;
use std::num::NonZeroU16;
use std::num::NonZeroU32;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratatui::Frame;
use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use tokio_util::sync::CancellationToken;

use kvim_clipboard::{ClipboardFailure, ClipboardNotice, ClipboardRead};
use kvim_core::{BufferVersion, CharPosition, EditTransaction, LineIndex, TextBuffer};
use kvim_editor::{
    AutoIndent, CommandContext, CommandOutcome, Cursor, EditContext, EditingState,
    MOTION_COUNT_MAX, MoveDirection, RegisterValue, Registers, SEARCH_QUERY_CHARS_MAX,
    SearchDirection, SearchQuery, Selection, Viewport, WindowState, selection_move_indent_line,
};
use kvim_input::{
    BindingScope, COMMAND_LINE_CHARS_MAX, Command, CommandAuthority, CommandLineCommand,
    ConfirmAnswer, ConfirmEdit, InputContextSnapshot, Mode, PasteText, PromptEdit, PromptKind,
    Registry, Resolution, Resolver, TreePrompt, WhichKeyRow,
};
use kvim_language::{
    Analysis, AnalysisError, AnalysisInput, BufferSyntax, Diagnostic, DiagnosticSet,
    DocumentPosition, FormatEdits, FormattedDocument, FormatterFailure, FormatterRequest,
    HighlightSpan, LanguageAdapter, LanguageEvent, LanguageFormatter, LanguageOutcome,
    LanguageRegistry, LanguageRequestId, LanguageServerId, LspError, Publication, ServerReport,
    SyntaxHighlighter, SyntaxTree, buffer_position, content_changes, document_position,
};
use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{ProcessOutput, ProcessRequest, WatchBatch, WatchCoverage, watch_limit_setting};
use kvim_settings::EditorSettings;
use kvim_terminal::{Key, TerminalEvent};
use kvim_ui::{Direction, RegionKind, SidebarSide, WindowId};
use kvim_workspace::{
    Acceptance, BUFFERS_MAX, BufferId, Buffers, Candidate, DiffComparison, DiffTarget, EntryKind,
    ExternalChange, FileBuffer, FileOperation, FileRequest, FileResult, FileTarget, FileTree,
    GitStatusFailure, GitStatusRead, GitStatusRequest, MutationError, MutationOutcome, OpenError,
    OpenRequest, OpenedFile, Overwrite, PICKER_QUERY_CHARS_MAX, PickerKind, PickerRequest,
    PickerResult, PickerSlot, RELOAD_TARGETS_MAX, ReloadOutcome, ReloadRequest, ReloadTarget,
    ReloadTrigger, ReloadedBuffer, SaveApplyOutcome, SaveError, SaveRequest, SavedBuffer,
    TREE_SEARCH_CHARS_MAX, TakenDestination, TransferMode, WorkspaceRequest, WorkspaceResult,
    WorktreeDiffFailure, WorktreeDiffRead, WorktreeDiffRequest, render_content,
};

use super::buffer_view::{WINBAR_ROWS, gutter_cells};
use super::changes::ChangeSection;
use super::chrome::shell_areas;
use super::clipboard::{ClipboardAccess, ClipboardStep, SessionClipboard, register_value};
use super::completion::{
    CompletionCycle, CompletionOutcome, LineCompletion, command_line_candidates,
};
use super::diagnostics::{HOST_BUFFER_NAME, HostReportRequest, HostWorkspace};
use super::embed::{
    CursorRequest, CursorShape, EditorAccess, EditorEvent, EditorInstanceId, EditorOutbox,
    EventReservation, GeometryError, InputRequest, PublishedEvent, Reduction, ReductionOutcome,
    Refusal, fits,
};
use super::jumps::{JumpDirection, JumpEntry, JumpStep};
use super::language::{
    AcceptedQuery, AfterSave, Answer, DiagnosticJump, Float, FormatOnSave, LanguageNotice,
    LanguageQuery, LanguageRequest, LanguageRequestKind, LanguageState, PendingJump,
    PendingPosition, PendingQuery, QueryPurpose, QueryState, Refusal as LanguageRefusal, formatter,
    has_formatter, jump_target,
};
use super::log::{EditorLog, LOG_BUFFER_NAME, LogSource};
use super::notify::NotificationBoard;
use super::picker::{PickerFailure, PickerState, RIPGREP_MISSING_NOTE, picker_areas};
use super::review::{ReviewOutcome, ReviewSurface};
use super::theme::Theme;
use super::tree::{
    GitPublication, TREE_NAME_CHARS_MAX, TREE_TITLE_ROWS, TreeMatchOutcome, TreeMotion,
    TreeRefusal, TreeSidebar, delete_question, overwrite_question,
};
use super::window::{WindowOutcome, Windows};

/// The largest message that the message line keeps, in characters.
///
/// Every message comes from a bounded label or from a typed error, so the bound
/// only protects the line against an unexpectedly long path.
pub const MESSAGE_CHARS_MAX: usize = 512;

/// The largest answer that one confirmation accepts, in characters.
///
/// The accepted words are `y` and `yes`, so a longer answer cancels the action
/// already. The bound keeps the question and its answer inside one row. See
/// `docs/input-actions.md`.
pub const CONFIRM_ANSWER_CHARS_MAX: usize = 32;

/// The name of the buffer analysis in the editor log.
///
/// A job name holds no buffer name and no path, so every repeat of one outcome
/// carries the same text and collapses into one entry. See
/// `docs/responsiveness.md`.
pub(super) const JOB_ANALYSIS: &str = "analysis";

/// The name of the path-completion walk in the editor log.
pub(super) const JOB_WALK: &str = "walk";

/// The name of the external formatter in the editor log.
const JOB_FORMATTER: &str = "formatter";

/// The outcome of one result that a newer buffer version displaced.
///
/// Every job that a buffer version gates names this same outcome, so a reader
/// searches one text for every obsolete result.
pub(super) const JOB_OBSOLETE: &str = "rejected: the buffer changed";

/// The outcome of one analysis that the bounded worker service refused.
pub(super) const JOB_REFUSED: &str = "refused: the worker service accepted no job";

/// The report that the message line shows while one host probe runs.
///
/// The probe reads the executable search path for every declared program, so
/// the command answers later than a command that reads editor state alone. The
/// message names that wait, and the editor stays fully usable during it. See
/// `docs/architecture.md`.
const HOST_REPORT_RUNNING: &str = "the host report is running; its buffer opens when it answers";

/// Whether the visible state changed and the terminal needs a new frame.
///
/// kvim renders only after a visible state change. It runs no unconditional
/// frame loop. See `docs/responsiveness.md`.
///
/// Every publication path of the session marks its answer `#[must_use]`. A
/// dropped [`Redraw::Needed`] leaves a changed message, marker, or overlay off
/// the screen until an unrelated event paints the next frame, so the compiler
/// refuses the drop instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Redraw {
    /// The visible state changed.
    Needed,
    /// The visible state is unchanged.
    Skipped,
}

impl Redraw {
    /// Returns [`Redraw::Needed`] when either side needs a new frame.
    #[must_use]
    pub const fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::Skipped, Self::Skipped) => Self::Skipped,
            _ => Self::Needed,
        }
    }
}

/// Whether the editor keeps reading terminal events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunState {
    /// The editor keeps running.
    Running,
    /// The editor closed its last window and shuts down.
    Finished,
}

/// Whether one typed command asks before it discards unsaved changes.
///
/// `:q` and `:e` ask, and `:q!` and `:e!` discard at once, because only the
/// user can decide to lose work. See `docs/files.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnsavedChanges {
    /// Ask before the command discards the unsaved changes of the buffer.
    Ask,
    /// Discard the unsaved changes without a question.
    Discard,
}

/// The file operation that the editor waits for.
///
/// The editor runs one file operation at a time, so a second request cannot
/// apply an obsolete result over a newer buffer state.
#[derive(Debug, Eq, PartialEq)]
enum PendingFile {
    /// One file is loading.
    Open,
    /// One buffer is saving.
    Save {
        /// The buffer that the save belongs to.
        buffer: BufferId,
        /// The step that follows the save.
        then: AfterSave,
        /// The format state that the save report names.
        format: FormatBeforeSave,
    },
    /// Loaded buffers are checked against their files.
    Reload {
        /// The buffers that the result may replace, bounded by the buffer list.
        targets: Vec<PendingReload>,
        /// Who asked for the check.
        origin: ReloadOrigin,
    },
}

/// What the save report names about the format that ran before it.
///
/// The save writes the message line after the format, so a format that writes
/// its own message loses it to the save report. The format therefore hands its
/// state to the save, and the save names that state beside its own result. The
/// note qualifies a message that every save writes, so it adds no message and
/// repeats none. See `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FormatBeforeSave {
    /// The save reports its own result alone.
    ///
    /// The formatter produced the content that the save writes, the buffer
    /// already matched its formatter, or no format ran before this save.
    Silent,
    /// The host holds no such program, which is a normal state.
    NotInstalled,
    /// The formatter produced no usable document.
    Failed,
}

impl FormatBeforeSave {
    /// Returns the state that one failed run of an external formatter leaves.
    ///
    /// An obsolete answer stays silent. The user typed while the formatter ran,
    /// and the save writes exactly the content that the user typed.
    #[must_use]
    const fn of(failure: FormatterFailure) -> Self {
        match failure {
            FormatterFailure::NotInstalled => Self::NotInstalled,
            FormatterFailure::Unavailable => Self::Failed,
            FormatterFailure::Obsolete => Self::Silent,
        }
    }

    /// Returns the reason that the save report names after its own result.
    #[must_use]
    const fn reason(self) -> Option<&'static str> {
        match self {
            Self::Silent => None,
            Self::NotInstalled => Some(FORMATTER_MISSING_NOTE),
            Self::Failed => Some(FORMATTER_FAILED_NOTE),
        }
    }

    /// Returns the level of the save report that carries this state.
    ///
    /// A formatter that the host does not hold is a normal state, so the save
    /// keeps the level of an ordinary report. A formatter that refused the
    /// document needs attention.
    #[must_use]
    const fn level(self) -> MessageLevel {
        match self {
            Self::Silent | Self::NotInstalled => MessageLevel::Info,
            Self::Failed => MessageLevel::Warning,
        }
    }
}

/// Who asked for one reload check.
///
/// A check that the user typed reports its outcome. A background check reports
/// only an external change that the editor cannot follow, so a workspace that
/// changes often never fills the message line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReloadOrigin {
    /// The user typed `:e` or `:e!`.
    Command,
    /// The workspace watcher reported a change.
    Watch,
}

/// Whether one queued reload may replace text that no file holds.
///
/// This is the safety rule of the reload path. Only `:e!` reaches
/// [`UnsavedText::Discard`], because only the user can decide to lose work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnsavedText {
    /// The reload must never replace it, so a modified buffer keeps its text.
    Keep,
    /// The user asked to discard it, which `:e!` does.
    Discard,
}

/// One buffer that waits for the outcome of a reload check.
///
/// The recorded target and version form the publication gate. A buffer that
/// moved or changed while the check ran makes the outcome obsolete.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingReload {
    /// The buffer that the outcome belongs to.
    buffer: BufferId,
    /// The file target at the moment that the request left.
    target: FileTarget,
    /// The buffer version at the moment that the request left.
    version: BufferVersion,
    /// Whether the reload may replace unsaved text.
    unsaved: UnsavedText,
}

/// Whether one search recomputation may trust the recorded buffer version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchRefresh {
    /// Recompute the matches only when the buffer version changed.
    OnVersionChange,
    /// Recompute them whatever the version says.
    ///
    /// A reload replaces the buffer, and the new buffer counts its versions
    /// from the start, so the version gate cannot see that change.
    Always,
}

/// The system clipboard operation that the editor waits for.
///
/// The session runs one clipboard operation at a time. A newer operation
/// resolves the operation that it displaces, so a deferred paste can never
/// wait for a result that no longer arrives.
#[derive(Debug)]
enum ClipboardWork {
    /// The system clipboard receives this unnamed-register value.
    Copy(RegisterValue),
    /// This paste runs after the system clipboard read resolves.
    Paste {
        /// The paste command that the key named.
        command: Command,
        /// The count that the key named.
        count: Option<NonZeroU32>,
        /// The register that the key named, which is always the unnamed one.
        ///
        /// A paste that names another register reads that register directly and
        /// starts no clipboard work, so only `None` and `"` arrive here.
        register: Option<char>,
    },
}

/// How far the pending system clipboard operation has progressed.
///
/// A command exists only while an operation holds it, and an operation waits
/// for an output only after the event loop took that command. The variants
/// carry the command and the operation together, so neither half can exist
/// without the other. Every transition below is a method, so no caller writes
/// the progression by hand.
#[derive(Debug)]
enum ClipboardActivity {
    /// No clipboard operation runs.
    Idle,
    /// One operation waits for the event loop to take its command.
    Queued {
        /// The command that the bounded process service must run.
        request: ProcessRequest,
        /// The operation that the output of that command finishes.
        work: ClipboardWork,
    },
    /// One operation waits for the output of the command that it handed over.
    Running {
        /// The operation that the output finishes.
        work: ClipboardWork,
    },
}

impl ClipboardActivity {
    /// Holds one operation until the event loop takes its command.
    fn queue(&mut self, request: ProcessRequest, work: ClipboardWork) {
        *self = Self::Queued { request, work };
    }

    /// Hands the queued command over and waits for its output.
    ///
    /// Returns the command exactly once, because the operation leaves the
    /// queued state with it.
    fn dispatch(&mut self) -> Option<ProcessRequest> {
        match std::mem::replace(self, Self::Idle) {
            Self::Queued { request, work } => {
                *self = Self::Running { work };
                Some(request)
            }
            // A running operation already handed its command over.
            Self::Running { work } => {
                *self = Self::Running { work };
                None
            }
            Self::Idle => None,
        }
    }

    /// Ends the activity and returns the operation that it held.
    ///
    /// A queued command that no one took never runs, which is exactly what a
    /// displaced operation needs.
    fn finish(&mut self) -> Option<ClipboardWork> {
        match std::mem::replace(self, Self::Idle) {
            Self::Idle => None,
            Self::Queued { work, .. } | Self::Running { work } => Some(work),
        }
    }
}

/// The reason that one file request produced no result.
///
/// The event loop maps every runtime failure onto one of these values, so the
/// session never reads an error message text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileRequestFailure {
    /// The bounded runtime held no free permit or result slot.
    Saturated,
    /// A newer request or the shutdown cancelled this request.
    Cancelled,
    /// The operation passed its deadline.
    Timeout,
}

impl FileRequestFailure {
    /// Returns the message that the message line shows.
    const fn message(self) -> &'static str {
        match self {
            Self::Saturated => "the editor is busy; try the file operation again",
            Self::Cancelled => "the file operation was cancelled",
            Self::Timeout => "the file operation passed its deadline",
        }
    }
}

/// The reason that one host probe produced no report.
///
/// The event loop maps every runtime failure onto one of these values, so the
/// session never reads an error message text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostProbeFailure {
    /// The bounded runtime held no free permit or result slot.
    Saturated,
    /// A newer request or the shutdown cancelled this probe.
    Cancelled,
    /// The probe passed its deadline.
    Timeout,
}

impl HostProbeFailure {
    /// Returns the message that the message line shows.
    const fn message(self) -> &'static str {
        match self {
            Self::Saturated => "the editor is busy; run the host report again",
            Self::Cancelled => "the host report was cancelled",
            Self::Timeout => "the host report passed its deadline",
        }
    }
}

/// One analysis job that the bounded worker service runs.
///
/// The session builds the job and never runs it, so parsing stays off the
/// terminal event loop. See `docs/language-services.md`.
pub struct AnalysisRequest {
    buffer: BufferId,
    adapter: &'static dyn LanguageAdapter,
    input: AnalysisInput,
    /// The highlighter that the session owns.
    ///
    /// One analysis runs at a time, so the jobs share one highlighter and every
    /// later analysis of a language reuses its compiled query. The lock is free
    /// whenever a job starts, because the publication gate admits one analysis.
    highlighter: Arc<Mutex<SyntaxHighlighter>>,
}

impl AnalysisRequest {
    /// Returns the buffer that the job analyzes.
    #[must_use]
    pub const fn buffer(&self) -> BufferId {
        self.buffer
    }

    /// Parses the source and collects the highlight spans.
    ///
    /// The call runs on the worker service. It checks the cancellation token,
    /// so a superseded job stops as early as the parser allows.
    #[must_use]
    pub fn run(self, cancellation: &CancellationToken) -> AnalysisResult {
        let mut highlighter = self
            .highlighter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        AnalysisResult {
            buffer: self.buffer,
            outcome: self
                .adapter
                .analyze(&self.input, &mut highlighter, cancellation),
        }
    }
}

/// The result of one analysis job.
///
/// Highlighting is decoration, so a typed failure renders plain text and
/// changes no buffer content.
pub struct AnalysisResult {
    buffer: BufferId,
    outcome: Result<Analysis, AnalysisError>,
}

/// The analysis state of one buffer.
///
/// The holder keeps the newest accepted result and the tree that the next parse
/// reuses. The reuse entry carries the buffer version that its tree describes,
/// so a version that the session did not move the tree over cannot serve a
/// later parse.
#[derive(Debug, Default)]
pub(super) struct BufferAnalysis {
    syntax: BufferSyntax,
    reuse: Option<(BufferVersion, SyntaxTree)>,
}

impl BufferAnalysis {
    /// Returns the highlight spans of the newest accepted result.
    pub(super) fn highlights(&self) -> &[HighlightSpan] {
        self.syntax.highlights()
    }
}

/// The severity of one message-line entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageLevel {
    /// The editor rejected a command.
    Error,
    /// The command succeeded, but the result needs attention.
    Warning,
    /// The message reports a normal result.
    Info,
}

/// Clips one text to the [`MESSAGE_CHARS_MAX`] characters of the message line.
///
/// The message and the question of a confirmation share the row, so both take
/// the same bound.
fn clip_message_line(text: impl Into<String>) -> String {
    let text = text.into();
    if text.chars().count() <= MESSAGE_CHARS_MAX {
        return text;
    }
    text.chars().take(MESSAGE_CHARS_MAX).collect()
}

/// Removes the word before the end of one written line.
///
/// The prompt line and the answer of a confirmation both end at the cursor, so
/// the edit works on the end of the text. It removes the run of trailing blanks
/// first and then the run of trailing non-blanks, which is the rule of Vim, of
/// readline, and of every terminal shell. An empty text changes nothing.
fn delete_word_backward(text: &mut String) -> Redraw {
    if text.is_empty() {
        return Redraw::Skipped;
    }
    let start = text
        .trim_end()
        .char_indices()
        .rev()
        .find(|&(_, value)| value.is_whitespace())
        .map_or(0, |(index, value)| index + value.len_utf8());
    debug_assert!(
        start < text.len(),
        "the last character of a written line is a blank or a non-blank, so the walk always \
         removes at least one character"
    );
    text.truncate(start);
    Redraw::Needed
}

/// Reports whether one operation can replace the entry of a destination.
///
/// A create writes one new entry, and a delete names no destination, so neither
/// offers an overwrite. See `docs/files.md`.
fn replaces_an_entry(operation: &FileOperation) -> bool {
    matches!(
        operation,
        FileOperation::Rename { .. } | FileOperation::Transfer { .. }
    )
}

/// Returns the cursor shape that one editor mode asks for.
///
/// Insert mode asks for a vertical bar, and every other mode asks for a block.
/// The host decides whether to apply the request. See `docs/windows.md`.
const fn cursor_shape(mode: Mode) -> CursorShape {
    match mode {
        Mode::Insert => CursorShape::Bar,
        Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock => CursorShape::Block,
    }
}

/// Returns the direction that one focus command names.
///
/// Only a focus command can reach the outer edge of this editor, so the window
/// commands that resize, split, and close name no direction here.
const fn focus_direction(command: Command) -> Option<Direction> {
    match command {
        Command::FocusWindowLeft => Some(Direction::Left),
        Command::FocusWindowDown => Some(Direction::Down),
        Command::FocusWindowUp => Some(Direction::Up),
        Command::FocusWindowRight => Some(Direction::Right),
        _ => None,
    }
}

/// One message-line entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    /// The text of the message, bounded by [`MESSAGE_CHARS_MAX`].
    pub(super) text: String,
    /// The severity of the message.
    pub(super) level: MessageLevel,
}

impl Message {
    /// Creates a message and clips it to [`MESSAGE_CHARS_MAX`] characters.
    fn new(text: impl Into<String>, level: MessageLevel) -> Self {
        Self {
            text: clip_message_line(text),
            level,
        }
    }

    /// Returns the text of the message.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the severity of the message.
    #[must_use]
    pub const fn level(&self) -> MessageLevel {
        self.level
    }
}

/// One open line prompt and the text that it holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PromptLine {
    /// The prompt that reads the line.
    pub(super) kind: PromptKind,
    /// The text after the prompt character.
    pub(super) text: String,
    /// The open completion of the line, while one candidate is written.
    ///
    /// The completion belongs to the prompt, so a closed prompt drops it and no
    /// caller keeps the two in step. See `docs/input-actions.md`.
    pub(super) completion: Option<LineCompletion>,
}

impl PromptLine {
    /// Returns the largest number of characters that the prompt accepts.
    const fn chars_max(&self) -> usize {
        match self.kind {
            PromptKind::CommandLine => COMMAND_LINE_CHARS_MAX,
            PromptKind::Search => SEARCH_QUERY_CHARS_MAX,
            PromptKind::Tree(TreePrompt::Search) => TREE_SEARCH_CHARS_MAX,
            PromptKind::Tree(
                TreePrompt::AddFile | TreePrompt::AddDirectory | TreePrompt::Rename,
            ) => TREE_NAME_CHARS_MAX,
            PromptKind::Picker => PICKER_QUERY_CHARS_MAX,
        }
    }

    /// Writes one completion candidate into the line.
    ///
    /// The first call over one typed text asks `candidates` for the list of the
    /// line. Every later call cycles the open completion and asks nothing, so
    /// the candidates stay anchored to the text that the user typed and one
    /// cycle never narrows them.
    ///
    /// An empty candidate list changes nothing and reports nothing, because a
    /// text that names no command is a normal state of a line that the user
    /// still types.
    fn complete(
        &mut self,
        candidates: impl FnOnce(&str) -> Vec<String>,
        cycle: CompletionCycle,
    ) -> CompletionOutcome {
        let chars_max = self.chars_max();
        let completion = match self.completion.take() {
            Some(mut open) => {
                open.cycle(cycle);
                open
            }
            None => {
                let offered = candidates(&self.text);
                let Some(open) = LineCompletion::open(&self.text, offered, chars_max, cycle) else {
                    return CompletionOutcome::Missed;
                };
                open
            }
        };
        self.text = completion.selected().to_owned();
        let outcome = completion.outcome();
        self.completion = Some(completion);
        outcome
    }
}

/// The action that one confirmed question performs.
///
/// The editor holds one variant for each action that asks before it destroys
/// data. A variant names what the action destroys, never the staged operation,
/// because the world can change while the question waits. The confirmed arm
/// stages the operation again. See `docs/files.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ConfirmedAction {
    /// Remove the named workspace entries.
    DeleteEntries {
        /// The entries that the question named.
        paths: Vec<PathBuf>,
    },
    /// Replace the entries of the named destinations.
    ///
    /// The variant holds the operation as well, because the destinations name
    /// what the overwrite destroys, not what it writes there. The operation
    /// already names every path, so it reads no tree state again.
    Overwrite {
        /// The operation that the user asked for.
        operation: FileOperation,
        /// The destinations that the question named, with the kind that the
        /// staging observed.
        destinations: Vec<TakenDestination>,
    },
    /// Close the focused window and lose the changes of the named buffer.
    DiscardOnQuit {
        /// The buffer that the question named.
        buffer: BufferId,
    },
    /// Read the file of the named buffer again and lose its changes.
    DiscardOnReload {
        /// The buffer that the question named.
        buffer: BufferId,
    },
    /// Report one message on the message line.
    ///
    /// The tests of the confirmation itself read that message, so they need no
    /// workspace and no destructive action. The variant is a test seam, never
    /// editor behavior.
    #[cfg(test)]
    Report,
}

/// One open confirmation, its typed answer, and the action that waits for it.
///
/// The question holds no answer hint. The message line adds `? [y/N]:` when it
/// draws the row, so every question takes the same form. See `docs/windows.md`.
///
/// The confirmation stays beside the prompt model, because a question can open
/// over an open prompt and that prompt keeps its own text. One value therefore
/// holds the question, the answer, and the action together, so no two fields of
/// the session can disagree. See `docs/input-actions.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Confirmation {
    /// The question, bounded by [`MESSAGE_CHARS_MAX`].
    pub(super) question: String,
    /// The answer that the user typed, bounded by
    /// [`CONFIRM_ANSWER_CHARS_MAX`].
    pub(super) answer: String,
    /// The action that a `y` answer performs.
    action: ConfirmedAction,
}

/// The key that closes one open confirmation.
///
/// `Esc` and `Ctrl-C` cancel at any time, so they read no answer. `Enter` reads
/// the typed answer instead. See `docs/input-actions.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfirmationClose {
    /// `Enter` closes the question and reads the typed answer.
    Accept,
    /// `Esc` or `Ctrl-C` closes the question and cancels the action.
    Cancel,
}

/// The outcome of one request to open a confirmation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConfirmationRequest {
    /// The confirmation opened and waits for the typed answer.
    Opened,
    /// Another confirmation waits already, so the editor opened none.
    Refused,
}

/// The accepted search query and the matches that it produced.
///
/// The matches follow one buffer version. A later edit makes them obsolete, and
/// the session recomputes them before the next frame.
#[derive(Clone, Debug)]
pub(super) struct ActiveSearch {
    pub(super) query: SearchQuery,
    pub(super) matches: Vec<CharPosition>,
    version: BufferVersion,
}

/// Everything that one frame reads.
///
/// The borrow set keeps rendering a pure function of visible state: a renderer
/// cannot change the session through this value.
pub(super) struct Visible<'a> {
    pub(super) area: Rect,
    pub(super) theme: Theme,
    pub(super) settings: &'a EditorSettings,
    pub(super) windows: &'a Windows,
    /// The file-tree sidebar, which paints its own region.
    pub(super) tree: &'a TreeSidebar,
    /// The open picker, which covers every other region.
    pub(super) picker: Option<&'a PickerState>,
    /// The open review, which draws the body instead of the window tree.
    pub(super) review: Option<&'a ReviewSurface>,
    /// Every loaded buffer, because each window shows its own buffer.
    pub(super) buffers: &'a Buffers,
    /// The buffer that the editing state and the active search belong to.
    pub(super) active: BufferId,
    pub(super) analysis: &'a BTreeMap<BufferId, BufferAnalysis>,
    /// The language adapters of this build, which decide whether a buffer
    /// reports a format-on-save state.
    pub(super) languages: LanguageRegistry,
    /// The published diagnostics and the language-service state.
    pub(super) language: &'a LanguageState,
    pub(super) editing: &'a EditingState,
    pub(super) search: Option<&'a ActiveSearch>,
    pub(super) prompt: Option<&'a PromptLine>,
    /// The open confirmation, which owns the message line while it waits.
    pub(super) confirmation: Option<&'a Confirmation>,
    pub(super) message: Option<&'a Message>,
    pub(super) float: Option<&'a Float>,
    pub(super) which_key: Option<&'a [WhichKeyRow]>,
    /// The notification board that the bottom-right overlay paints.
    pub(super) notifications: &'a NotificationBoard,
}

impl Visible<'_> {
    /// Returns the highlight spans of one buffer.
    ///
    /// An empty list renders plain text, which every unsupported, cancelled, or
    /// rejected analysis must also do.
    pub(super) fn highlights(&self, buffer: BufferId) -> &[HighlightSpan] {
        self.analysis
            .get(&buffer)
            .map_or(&[][..], BufferAnalysis::highlights)
    }

    /// Returns the diagnostics of one buffer, in ascending position order.
    ///
    /// An empty list renders no decoration and no sign, which every buffer
    /// without a language server also does.
    pub(super) fn diagnostics(&self, buffer: BufferId) -> &[Diagnostic] {
        self.language.diagnostics(buffer)
    }

    /// Returns the format-on-save state that the focused window shows.
    ///
    /// The state belongs to one buffer, so a focus change to a window over
    /// another buffer reports the state of that other buffer. A buffer that no
    /// formatter can format reports no state, because the state would promise
    /// an action that no save can perform.
    pub(super) fn focused_format_on_save(&self) -> Option<FormatOnSave> {
        let Some(buffer) = self.windows.buffer(self.windows.focused_window()) else {
            debug_assert!(false, "the focused window is always a leaf of the tree");
            return None;
        };
        let path = self.buffers.get(buffer).and_then(FileBuffer::path);
        if !has_formatter(self.languages, path) {
            return None;
        }
        let default = FormatOnSave::from_setting(self.settings.files.format_on_save);
        Some(self.language.format_on_save(buffer, default))
    }
}

/// The workspace walk that completes the path argument of one command line.
///
/// The state records whether the open command line already asked for its walk,
/// so a later character of the same line asks for no second walk. A line that
/// holds no path argument asks for nothing, and a closed line returns to
/// [`CompletionWalk::Unasked`]. See `docs/files.md`.
enum CompletionWalk {
    /// The line asked for no walk, because it holds no path argument.
    Unasked,
    /// The line asked for the walk, and the request waits for the event loop.
    Queued(PickerRequest),
    /// The event loop took the request of the line.
    ///
    /// The files stay empty while the walk runs, and after a cancelled or timed
    /// out walk, so the completion then offers no path and the command line
    /// stays usable.
    Taken(Vec<Candidate>),
}

impl CompletionWalk {
    /// Returns the workspace files that the finished walk collected.
    fn files(&self) -> &[Candidate] {
        match self {
            Self::Unasked | Self::Queued(_) => &[],
            Self::Taken(files) => files,
        }
    }

    /// Takes the queued request and records that the line asked for its walk.
    ///
    /// The recorded state outlives the request, so the next character of the
    /// same line queues no second walk.
    fn take_request(&mut self) -> Option<PickerRequest> {
        match mem::replace(self, Self::Taken(Vec::new())) {
            Self::Queued(request) => Some(request),
            // The line asked for no walk, or the event loop already took the
            // request, so the state stays as it was.
            unchanged @ (Self::Unasked | Self::Taken(_)) => {
                *self = unchanged;
                None
            }
        }
    }
}

/// The host probe that the `:diagnostics` command asked for.
///
/// The state records whether one probe already runs, so a second command starts
/// no second probe and the buffer opens once for each request. The probe reads
/// the executable search path, which is filesystem work, so the event loop
/// hands the request to the bounded worker service. See
/// `docs/architecture.md`.
enum HostProbe {
    /// No command asked for a host report.
    Unasked,
    /// One command asked for the report, and the request waits for the event
    /// loop.
    Queued(HostReportRequest),
    /// The event loop took the request, and the buffer opens when it answers.
    Running,
}

impl HostProbe {
    /// Takes the queued request and records that the probe runs.
    ///
    /// The recorded state outlives the request, so a second command queues no
    /// second probe.
    fn take_request(&mut self) -> Option<HostReportRequest> {
        match mem::replace(self, Self::Running) {
            Self::Queued(request) => Some(request),
            // No command asked for a report, or the event loop already took the
            // request, so the state stays as it was.
            unchanged @ (Self::Unasked | Self::Running) => {
                *self = unchanged;
                None
            }
        }
    }
}

#[cfg(test)]
pub(super) fn test_root(path: PathBuf) -> Arc<WorktreeRoot> {
    Arc::new(WorktreeRoot::open(path).expect("the test workspace root exists"))
}

/// The visible editor state of one terminal.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use ratatui::layout::Rect;
///
/// use kvim_settings::EditorSettings;
/// use kvim_terminal::{Key, KeyCode, TerminalEvent};
/// use kvim_tui::{Redraw, Session};
///
/// let root = std::sync::Arc::new(
///     kvim_path::WorktreeRoot::open(
///         std::env::current_dir().expect("the process holds a working directory"),
///     )
///     .expect("the working directory is a worktree"),
/// );
/// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root);
/// let now = Duration::ZERO;
///
/// // `i` enters Insert mode, and a printable key inserts one character.
/// session.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('i'))), now);
/// let redraw = session.handle_event(
///     TerminalEvent::Key(Key::plain(KeyCode::Char('x'))),
///     now,
/// );
/// assert_eq!(redraw, Redraw::Needed);
/// // The buffer terminates its last line, so the text ends with a line ending.
/// assert_eq!(session.buffer().to_string(), "x\n");
/// ```
pub struct Session {
    /// The identity that every published event of this editor carries.
    instance: EditorInstanceId,
    /// What the host granted this editor.
    access: EditorAccess,
    /// The bounded facts that the host still has to read.
    events: EditorOutbox,
    /// What the running input reduction reports to the host.
    ///
    /// A focus boundary, a close request, and a refusal leave the editor with
    /// the answer of the input that produced them, so none of them needs a
    /// queue slot. See `docs/embedding.md`.
    outcome: ReductionOutcome,
    /// The outbox slot that the running save owns.
    ///
    /// The editor runs one file operation at a time, so one slot covers every
    /// save. The reservation exists before the write starts, so a completed
    /// write always owns the slot of its `FileWritten` fact.
    write_slot: Option<EventReservation>,
    /// The outbox slot that the running workspace mutation owns.
    mutation_slot: Option<EventReservation>,
    area: Rect,
    settings: EditorSettings,
    theme: Theme,
    root: Arc<WorktreeRoot>,
    /// The highlighter that every analysis job of this session shares.
    ///
    /// One analysis runs at a time, so one highlighter keeps the compiled query
    /// of each language that the session opened, and it releases every one of
    /// them with the session.
    highlighter: Arc<Mutex<SyntaxHighlighter>>,
    buffers: Buffers,
    active: BufferId,
    /// The file operation that waits for the bounded worker service.
    file_outbox: Option<FileRequest>,
    /// The file operation that the editor waits for.
    file_pending: Option<PendingFile>,
    /// Reports whether a workspace change still needs its reload check.
    ///
    /// The editor runs one file operation at a time, so a burst that arrives
    /// during a save waits for that save instead of displacing it.
    reload_due: bool,
    windows: Windows,
    /// The file-tree sidebar, its clipboard, and its workspace requests.
    tree: TreeSidebar,
    /// The sidebar region of the window tree, while the tree ever opened it.
    tree_region: Option<WindowId>,
    /// The picker that covers the terminal, while one is open.
    ///
    /// The picker owns its own prompt, so it lives exactly as long as that
    /// prompt. See `docs/files.md`.
    picker: Option<PickerState>,
    /// The review of one captured diff.
    ///
    /// The value survives a close, so a reader that jumps into a file and
    /// returns keeps every read mark and the cursor. See `docs/diff-view.md`.
    review: Option<ReviewSurface>,
    /// Reports whether the review owns the frame and the keys.
    review_open: bool,
    /// The diff captures that the bounded process service must run.
    diff_outbox: VecDeque<(ChangeSection, WorktreeDiffRequest)>,
    /// Reports whether the editor already named a refused diff capture.
    diff_reported: bool,
    /// Reports whether the editor already named the missing `rg` command.
    ///
    /// A missing command is a normal state, so the editor reports it once and
    /// stays usable.
    ripgrep_reported: bool,
    /// Reports whether the editor already named the missing `git` command.
    ///
    /// A missing command is a normal state, so the editor reports it once and
    /// stays usable without the repository state. See `docs/git.md`.
    git_reported: bool,
    /// Reports whether the editor already named the state of the watcher.
    ///
    /// A host that refuses the watch is a normal state, so the editor reports
    /// it once and stays usable with the manual refresh. The missing watcher
    /// and the workspace that carries a watch in part share this flag, so one
    /// session shows one watch report and a later burst adds no noise. See
    /// `docs/files.md`.
    watch_reported: bool,
    editing: EditingState,
    registers: Registers,
    /// The system clipboard boundary that the composition root selected.
    clipboard: SessionClipboard,
    /// The policy that the host granted for the system clipboard.
    clipboard_access: ClipboardAccess,
    /// The clipboard operation that runs now, and how far it progressed.
    clipboard_activity: ClipboardActivity,
    /// The unnamed-register revision that the system clipboard already holds.
    ///
    /// The revision counts every register write, so one comparison covers every
    /// yank, delete, and change without a text comparison. See
    /// `docs/clipboard.md`.
    clipboard_revision: u64,
    resolver: Resolver,
    /// The language adapters of this build. Only an adapter selects a path.
    languages: LanguageRegistry,
    /// The analysis state of every buffer that a language adapter serves.
    analysis: BTreeMap<BufferId, BufferAnalysis>,
    /// The buffer and version of the analysis job that runs now.
    ///
    /// One job runs at a time, so a newer buffer version replaces the job that
    /// it supersedes instead of adding a second one.
    analysis_pending: Option<(BufferId, BufferVersion)>,
    /// The language-server requests, diagnostics, and per-buffer settings.
    ///
    /// The session never speaks the protocol. It builds bounded requests, and
    /// the event loop hands them to the language services. See
    /// `docs/language-services.md`.
    language: LanguageState,
    search: Option<ActiveSearch>,
    prompt: Option<PromptLine>,
    /// The confirmation that waits for a typed answer, while one is open.
    ///
    /// At most one question waits, so a second request opens none. See
    /// `docs/input-actions.md`.
    confirmation: Option<Confirmation>,
    /// The workspace walk that completes the path argument of `:e`.
    ///
    /// The command line asks for the walk when it first holds a path argument,
    /// so one open line asks for exactly one walk and most lines ask for none.
    /// The session walks no directory, so the event loop hands the request to
    /// the bounded worker service. The completion then filters the collected
    /// files while the user types. See `docs/files.md`.
    completion_walk: CompletionWalk,
    /// The host probe that `:diagnostics` asked for.
    ///
    /// The session reads no executable search path, so the event loop hands the
    /// request to the bounded worker service and the buffer opens when the
    /// probe answers. See `docs/architecture.md`.
    host_probe: HostProbe,
    message: Option<Message>,
    /// The bounded history of every report that the editor made.
    ///
    /// The message line keeps one message, so a replaced message would be gone
    /// without this log. `:logs` opens one snapshot of it as a buffer. See
    /// `docs/windows.md`.
    log: EditorLog,
    /// The open floating overlay of the language services.
    float: Option<Float>,
    which_key: Option<Vec<WhichKeyRow>>,
    /// The progress of every language server and every mirrored message.
    notifications: NotificationBoard,
    /// The elapsed time that the event loop reported last.
    ///
    /// The session reads no clock. The loop passes the elapsed time into every
    /// entry point, and a report that carries no time of its own, such as a
    /// message, uses this value. See `docs/responsiveness.md`.
    clock: Duration,
    run: RunState,
}

impl Session {
    /// Creates a session that shows one empty scratch buffer.
    ///
    /// The workspace root is the capability and directory that the file tree
    /// shows. The caller resolves it once, because the session performs no
    /// filesystem work. See `docs/files.md`.
    ///
    /// # Panics
    ///
    /// Panics when the hardcoded first-release binding table is invalid. This
    /// is a cold-path bootstrap check, so an invalid table must fail at start.
    #[must_use]
    pub fn new(area: Rect, settings: EditorSettings, root: Arc<WorktreeRoot>) -> Self {
        let (buffers, active) = Buffers::new(FileBuffer::scratch(&settings.files));
        let instance = EditorInstanceId::allocate();
        let mut session = Self {
            instance,
            access: EditorAccess::ReadWrite,
            events: EditorOutbox::new(instance),
            outcome: ReductionOutcome::Applied,
            write_slot: None,
            mutation_slot: None,
            area,
            settings,
            theme: Theme::new(),
            root: Arc::clone(&root),
            highlighter: Arc::new(Mutex::new(SyntaxHighlighter::new())),
            buffers,
            active,
            file_outbox: None,
            file_pending: None,
            reload_due: false,
            windows: Windows::new(active, shell_areas(area).body, settings.windows),
            tree: TreeSidebar::new(Arc::clone(&root)),
            tree_region: None,
            picker: None,
            review: None,
            review_open: false,
            diff_outbox: VecDeque::new(),
            diff_reported: false,
            ripgrep_reported: false,
            git_reported: false,
            watch_reported: false,
            editing: EditingState::new(),
            registers: Registers::default(),
            clipboard: SessionClipboard::default(),
            clipboard_access: ClipboardAccess::None,
            clipboard_activity: ClipboardActivity::Idle,
            clipboard_revision: 0,
            resolver: Resolver::new(Registry::first_release(), settings.input),
            languages: LanguageRegistry::first_release(),
            analysis: BTreeMap::new(),
            analysis_pending: None,
            language: LanguageState::default(),
            search: None,
            prompt: None,
            confirmation: None,
            completion_walk: CompletionWalk::Unasked,
            host_probe: HostProbe::Unasked,
            message: None,
            log: EditorLog::default(),
            float: None,
            which_key: None,
            notifications: NotificationBoard::default(),
            clock: Duration::ZERO,
            run: RunState::Running,
        };
        session.reconcile_viewports();
        session
    }

    /// Grants what this editor may reach of the system clipboard.
    ///
    /// [`ClipboardAccess::System`] performs the platform selection once, here,
    /// because it reads the target platform and the executable search path. A
    /// session without this call keeps [`ClipboardAccess::None`] and reaches no
    /// clipboard command at all, which keeps every test free from the host
    /// clipboard. See `docs/clipboard.md`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::{ClipboardAccess, Session};
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root)
    ///     .with_clipboard(ClipboardAccess::System);
    /// assert_eq!(session.clipboard_access(), ClipboardAccess::System);
    /// ```
    #[must_use]
    pub fn with_clipboard(mut self, access: ClipboardAccess) -> Self {
        self.clipboard = access.realize();
        self.clipboard_access = access;
        self
    }

    /// Returns what this editor may reach of the system clipboard.
    #[inline]
    #[must_use]
    pub const fn clipboard_access(&self) -> ClipboardAccess {
        self.clipboard_access
    }

    /// Injects one explicit clipboard boundary.
    ///
    /// The tests of this crate drive the deferred boundary without a platform
    /// command, so no test reaches the host clipboard.
    #[cfg(test)]
    #[must_use]
    pub(super) fn with_session_clipboard(mut self, clipboard: SessionClipboard) -> Self {
        self.clipboard = clipboard;
        self
    }

    /// Grants the access that the host decided for this editor.
    ///
    /// [`EditorAccess::ViewOnly`] refuses every text change, every save, every
    /// format, and every workspace mutation. The default is
    /// [`EditorAccess::ReadWrite`], which keeps the standalone behavior. See
    /// `docs/embedding.md`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::{EditorAccess, Session};
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root)
    ///     .with_access(EditorAccess::ViewOnly);
    /// assert_eq!(session.access(), EditorAccess::ViewOnly);
    /// ```
    #[must_use]
    pub fn with_access(mut self, access: EditorAccess) -> Self {
        self.access = access;
        self
    }

    /// Returns the identity that every published event of this editor carries.
    #[must_use]
    pub const fn instance(&self) -> EditorInstanceId {
        self.instance
    }

    /// Returns what the host granted this editor.
    #[must_use]
    pub const fn access(&self) -> EditorAccess {
        self.access
    }

    /// Returns the cursor shape that the current mode asks for.
    ///
    /// The host owns the terminal, so it decides whether to apply the request.
    #[must_use]
    pub const fn cursor_shape(&self) -> CursorShape {
        cursor_shape(self.editing.mode())
    }

    /// Takes the next fact or request that the host must read.
    ///
    /// The mandatory facts of the durable operations leave first, then the
    /// active file, then the coalesced redraw request. See `docs/embedding.md`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_input::Command;
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::{EditorEvent, Session};
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root);
    /// let reduction = session.apply_command(Command::SplitAdaptive, None, None, Duration::ZERO);
    /// assert_eq!(reduction.instance, session.instance());
    ///
    /// let published = session.take_event().expect("the split needs one frame");
    /// assert_eq!(published.instance, session.instance());
    /// assert_eq!(published.event, EditorEvent::RedrawRequested);
    /// ```
    #[must_use]
    pub fn take_event(&mut self) -> Option<PublishedEvent> {
        self.events.take()
    }

    /// Returns the terminal rectangle that the session renders into.
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
    }

    /// Accepts one new rectangle for this editor.
    ///
    /// The layout, the viewports, and the cursor all follow the accepted
    /// rectangle, so a host changes the geometry with this call and renders
    /// into the same rectangle afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`GeometryError::Empty`] for a rectangle without a cell. The
    /// editor keeps the rectangle that it accepted before.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::Session;
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root);
    /// let area = Rect::new(4, 2, 40, 12);
    /// session.set_area(area).expect("the rectangle holds cells");
    /// assert_eq!(session.area(), area);
    /// assert!(session.set_area(Rect::new(4, 2, 0, 12)).is_err());
    /// ```
    pub fn set_area(&mut self, area: Rect) -> Result<Redraw, GeometryError> {
        if area.width == 0 || area.height == 0 {
            return Err(GeometryError::Empty { area });
        }
        let redraw = self.resize(area);
        self.reconcile_viewports();
        self.reconcile_tree();
        self.reconcile_picker();
        self.note_redraw(redraw);
        Ok(redraw)
    }

    /// Renders one frame into the supplied cells.
    ///
    /// The editor writes only inside `area` and returns the cursor that the
    /// frame asks for. The host decides whether to apply that request.
    ///
    /// # Errors
    ///
    /// Returns [`GeometryError::Empty`] for a rectangle without a cell,
    /// [`GeometryError::OutsideBuffer`] for a rectangle that leaves the cell
    /// buffer, and [`GeometryError::Unreconciled`] for a rectangle that the
    /// editor did not accept through [`Session::set_area`]. Every error leaves
    /// every cell of the buffer unchanged.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::buffer::Buffer;
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::Session;
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let area = Rect::new(2, 1, 40, 12);
    /// let mut session = Session::new(area, EditorSettings::default(), root);
    /// let mut cells = Buffer::empty(Rect::new(0, 0, 60, 20));
    /// let cursor = session.draw(&mut cells, area).expect("the rectangle fits");
    /// assert!(cursor.position.is_some());
    ///
    /// // A rectangle that leaves the buffer changes no cell.
    /// let before = cells.clone();
    /// assert!(session.draw(&mut cells, Rect::new(40, 1, 40, 12)).is_err());
    /// assert_eq!(cells, before);
    /// ```
    pub fn draw(
        &self,
        buffer: &mut CellBuffer,
        area: Rect,
    ) -> Result<CursorRequest, GeometryError> {
        if area.width == 0 || area.height == 0 {
            return Err(GeometryError::Empty { area });
        }
        if area != self.area {
            return Err(GeometryError::Unreconciled {
                area,
                accepted: self.area,
            });
        }
        if !fits(area, buffer.area) {
            return Err(GeometryError::OutsideBuffer {
                area,
                buffer: buffer.area,
            });
        }
        let position = super::render::draw(buffer, &self.visible());
        Ok(CursorRequest {
            position,
            shape: cursor_shape(self.editing.mode()),
        })
    }

    /// Opens one file of this worktree.
    ///
    /// The path is relative to the root that the host supplied, so the editor
    /// reaches no file outside that root. The open leaves the editor as one
    /// file request, because the editor reads no file itself. See
    /// `docs/embedding.md` and `docs/files.md`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_path::WorktreeRelativePath;
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::Session;
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root);
    /// let path = WorktreeRelativePath::new("Cargo.toml").expect("the path is contained");
    /// let _ = session.open(path);
    ///
    /// // The host hands the request to its bounded worker service.
    /// let request = session.take_file_request().expect("the open needs one file read");
    /// session.apply_file_result(request.run());
    /// assert_eq!(session.buffers().len(), 2);
    /// ```
    #[must_use]
    pub fn open(&mut self, path: WorktreeRelativePath) -> Redraw {
        let display_path = self.root.as_path().join(path.as_path());
        if let Some(id) = self.buffers.find_path(&display_path) {
            let redraw = self.switch_to(id);
            self.note_redraw(redraw);
            return redraw;
        }
        let files = self.settings.files;
        let redraw = self.start_file_request(
            FileRequest::Open(OpenRequest {
                root: Arc::clone(&self.root),
                path,
                files,
            }),
            PendingFile::Open,
        );
        self.note_redraw(redraw);
        redraw
    }

    /// Applies one resolved command.
    ///
    /// The host owns the key resolver, so the editor receives the command and
    /// its count instead of a key. See `docs/embedding.md`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_input::Command;
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::{EditorAccess, Refusal, Session};
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root)
    ///     .with_access(EditorAccess::ViewOnly);
    /// let reduction = session.apply_command(Command::DeleteLine, None, None, Duration::ZERO);
    /// assert_eq!(reduction.refusal(), Some(Refusal::ViewOnly));
    /// ```
    #[must_use]
    pub fn apply_command(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
        register: Option<char>,
        now: Duration,
    ) -> Reduction {
        self.begin_input(now);
        let redraw = self.dispatch_command(command, count, register);
        self.finish_input(redraw, now)
    }

    /// Inserts one run of literal text.
    ///
    /// The host owns the text fallback of its focused scope, so it hands the
    /// literal characters to the editor. An open question and an open prompt
    /// line take them first. Insert mode is the only buffer mode that takes
    /// them.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_input::Command;
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::Session;
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root);
    /// session.apply_command(Command::InsertBeforeCursor, None, None, Duration::ZERO);
    /// session.insert_literal("hi", Duration::ZERO);
    /// assert_eq!(session.buffer().to_string(), "hi\n");
    /// ```
    #[must_use]
    pub fn insert_literal(&mut self, text: &str, now: Duration) -> Reduction {
        self.begin_input(now);
        let redraw = self.insert_owned_text(text);
        self.finish_input(redraw, now)
    }

    /// Applies one bounded paste as literal text.
    ///
    /// [`PasteText`] carries the bound, so no paste can exceed it. See
    /// `docs/embedding.md`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_input::{Command, PasteText};
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::Session;
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root);
    /// session.apply_command(Command::InsertBeforeCursor, None, None, Duration::ZERO);
    /// let block = PasteText::new("pasted").expect("the block is bounded");
    /// session.paste(&block, Duration::ZERO);
    /// assert_eq!(session.buffer().to_string(), "pasted\n");
    /// ```
    #[must_use]
    pub fn paste(&mut self, text: &PasteText, now: Duration) -> Reduction {
        self.begin_input(now);
        let redraw = self.insert_owned_text(text.as_str());
        self.finish_input(redraw, now)
    }

    /// Returns the input context that this editor publishes.
    ///
    /// The host supplies this value to the shared resolver of its workspace,
    /// so the next resolution reads the current scope, the grammar phases, the
    /// text fallback, and the generation of this editor. See
    /// `docs/embedding.md`.
    #[must_use]
    pub fn input_context(&self) -> InputContextSnapshot<BindingScope> {
        self.resolver.snapshot()
    }

    /// Cancels every pending semantic phase of this editor.
    ///
    /// A workspace composer proposes this effect before it moves focus or
    /// overlay ownership. The call closes an open question and an open prompt
    /// line, and it clears the count, the operator, the register, and the text
    /// object. The host then reads [`Session::input_context`] and resumes the
    /// proposed transition.
    #[must_use]
    pub fn cancel_pending(&mut self, now: Duration) -> Reduction {
        self.begin_input(now);
        let mut redraw = Redraw::Skipped;
        // The question owns the keys above the prompt, so it closes first and
        // the prompt below it closes next.
        if self.confirmation.is_some() {
            redraw = self.edit_confirmation(ConfirmEdit::Cancel).or(redraw);
        }
        if self.prompt.is_some() {
            redraw = self.apply_prompt(PromptEdit::Cancel).or(redraw);
        }
        // A waiting operator reports one command, which aborts it and changes
        // nothing else.
        if let Resolution::Command {
            command,
            count,
            register,
        } = self.resolver.cancel()
        {
            redraw = self.dispatch_command(command, count, register).or(redraw);
        }
        self.finish_input(redraw, now)
    }

    /// Starts one input reduction.
    fn begin_input(&mut self, now: Duration) {
        self.advance_clock(now);
        self.outcome = ReductionOutcome::Applied;
    }

    /// Settles the editor and reports what the input produced.
    fn finish_input(&mut self, redraw: Redraw, now: Duration) -> Reduction {
        let settled = self.settle(now).or(redraw);
        self.note_redraw(settled);
        Reduction {
            instance: self.instance,
            outcome: mem::replace(&mut self.outcome, ReductionOutcome::Applied),
        }
    }

    /// Latches the redraw request of one transition.
    fn note_redraw(&mut self, redraw: Redraw) {
        if redraw == Redraw::Needed {
            self.events.request_redraw();
        }
    }

    /// Refuses one durable change and reports the reason.
    ///
    /// The refusal reaches the host through the reduction and the message
    /// line, so no durable change ever fails without a report. See
    /// `docs/embedding.md`.
    fn refuse(&mut self, refusal: Refusal) -> Redraw {
        self.outcome = ReductionOutcome::Refused(refusal);
        self.set_message(refusal.note(), MessageLevel::Warning);
        Redraw::Needed
    }

    /// Records the one request that this input hands to the host.
    fn note_request(&mut self, request: InputRequest) {
        self.outcome = ReductionOutcome::Request(request);
    }

    /// Publishes the mandatory fact of one completed write.
    fn publish_write(&mut self, slot: Option<EventReservation>, path: WorktreeRelativePath) {
        match slot {
            Some(slot) => self.events.commit(slot, EditorEvent::FileWritten { path }),
            None => debug_assert!(
                false,
                "every save reserves its slot before the write starts"
            ),
        }
    }

    /// Returns the slot of one operation that produced no durable change.
    fn release_slot(&mut self, slot: Option<EventReservation>) {
        if let Some(slot) = slot {
            self.events.release(slot);
        }
    }

    /// Ends this editor after its last window closed.
    ///
    /// The standalone loop reads [`Session::run_state`], and an embedding host
    /// reads the close request of the input that produced it. See
    /// `docs/embedding.md`.
    fn close_editor(&mut self) {
        self.run = RunState::Finished;
        self.note_request(InputRequest::CloseRequested);
    }

    /// Returns the text of the active buffer.
    #[must_use]
    pub fn buffer(&self) -> &TextBuffer {
        self.active_buffer().text()
    }

    /// Returns the active buffer.
    #[must_use]
    pub fn active_buffer(&self) -> &FileBuffer {
        self.buffers
            .get(self.active)
            .expect("the session always keeps the active buffer loaded")
    }

    /// Returns the loaded buffers.
    #[must_use]
    pub const fn buffers(&self) -> &Buffers {
        &self.buffers
    }

    /// Returns the identity of the active buffer.
    #[must_use]
    pub const fn active(&self) -> BufferId {
        self.active
    }

    /// Returns the active editor mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.editing.mode()
    }

    /// Returns the window tree and its layout.
    #[must_use]
    pub const fn windows(&self) -> &Windows {
        &self.windows
    }

    /// Returns the workspace file tree that the sidebar shows.
    #[must_use]
    pub const fn file_tree(&self) -> &FileTree {
        self.tree.tree()
    }

    /// Returns the last message, or `None` while the line is empty.
    #[must_use]
    pub fn message(&self) -> Option<&Message> {
        self.message.as_ref()
    }

    /// Reports whether the editor keeps reading terminal events.
    #[must_use]
    pub const fn run_state(&self) -> RunState {
        self.run
    }

    /// Returns the elapsed time of the next state change that no event causes.
    ///
    /// Two changes reach this path. The which-key overlay appears after its
    /// delay, and a pending sequence holds no deadline and waits for the next
    /// key. The notification overlay advances its spinner and removes a
    /// finished item after its lifetime. The event loop therefore waits for a
    /// terminal event or for the earlier of these two times, never for a frame
    /// interval. See `docs/responsiveness.md`.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        let overlay = self
            .resolver
            .overlay_deadline()
            .filter(|_| self.which_key.is_none());
        let notifications = self
            .notifications
            .next_deadline(self.settings.notifications);
        match (overlay, notifications) {
            (Some(overlay), Some(notifications)) => Some(overlay.min(notifications)),
            (Some(time), None) | (None, Some(time)) => Some(time),
            (None, None) => None,
        }
    }

    /// Records the elapsed time that the event loop reported.
    ///
    /// The loop calls this before it applies a background result, because such
    /// a result carries no time of its own and may report a message that the
    /// notification overlay must expire later. See `docs/responsiveness.md`.
    pub fn advance_clock(&mut self, now: Duration) {
        self.clock = self.clock.max(now);
    }

    /// Applies one normalized terminal event.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_settings::EditorSettings;
    /// use kvim_terminal::{Key, KeyCode, PasteText, TerminalEvent};
    /// use kvim_tui::Session;
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root);
    ///
    /// let insert = TerminalEvent::Key(Key::plain(KeyCode::Char('i')));
    /// let _ = session.handle_event(insert, Duration::ZERO);
    /// let block =
    ///     TerminalEvent::Paste(PasteText::new("two words").expect("the block is bounded"));
    /// let _ = session.handle_event(block, Duration::ZERO);
    /// assert_eq!(session.buffer().to_string(), "two words\n");
    /// ```
    pub fn handle_event(&mut self, event: TerminalEvent, now: Duration) -> Redraw {
        self.begin_input(now);
        let redraw = match event {
            TerminalEvent::Key(key) => self.handle_key(key, now),
            // One paste block is one input, so it becomes one edit transaction
            // and one undo unit. A float is decoration of one answer, so the
            // paste closes it exactly as a key does.
            TerminalEvent::Paste(text) => {
                let closed = self.close_float();
                self.insert_owned_text(text.as_str()).or(closed)
            }
            TerminalEvent::Resize { columns, rows } => self.resize(Rect::new(0, 0, columns, rows)),
            // A focus change moves no cursor and shows no new text.
            TerminalEvent::Focus(_) => Redraw::Skipped,
            // Input that no binding accepts resets every pending grammar phase,
            // so a rejected chord never runs the binding of its unmodified key.
            TerminalEvent::Unsupported => {
                let resolution = self.resolver.unsupported();
                self.apply_resolution(resolution)
            }
        };
        let settled = self.settle(now).or(redraw);
        self.note_redraw(settled);
        settled
    }

    /// Applies the state changes that the elapsed time alone causes.
    ///
    /// The which-key overlay and the notification overlay reach this path,
    /// because the pending sequence itself never expires.
    pub fn tick(&mut self, now: Duration) -> Redraw {
        self.begin_input(now);
        let settled = self.settle(now);
        self.note_redraw(settled);
        settled
    }

    /// Renders one frame of the visible state.
    ///
    /// The call changes no state, so a caller may render the same session
    /// twice and receive the same frame.
    pub fn render(&self, frame: &mut Frame<'_>) {
        super::render::frame(frame, &self.visible());
    }

    /// Returns the borrowed state that one frame reads.
    pub(super) fn visible(&self) -> Visible<'_> {
        Visible {
            area: self.area,
            theme: self.theme,
            settings: &self.settings,
            windows: &self.windows,
            tree: &self.tree,
            picker: self.picker.as_ref(),
            review: self.review.as_ref().filter(|_| self.review_open),
            buffers: &self.buffers,
            active: self.active,
            analysis: &self.analysis,
            languages: self.languages,
            language: &self.language,
            editing: &self.editing,
            search: self.search.as_ref(),
            prompt: self.prompt.as_ref(),
            confirmation: self.confirmation.as_ref(),
            message: self.message.as_ref(),
            float: self.float.as_ref(),
            which_key: self.which_key.as_deref(),
            notifications: &self.notifications,
        }
    }

    /// Restores every derived value after one transition.
    ///
    /// The overlay rows, the search matches, and the viewports all follow the
    /// state that the transition produced, so the next frame is consistent.
    fn settle(&mut self, now: Duration) -> Redraw {
        self.refresh_search(SearchRefresh::OnVersionChange);
        self.reconcile_viewports();
        self.reconcile_tree();
        self.reconcile_picker();
        let mirrored = self.reconcile_clipboard();
        let advanced = self.notifications.advance(now, self.settings.notifications);
        let rows = self.resolver.which_key(now);
        if rows.as_deref() == self.which_key.as_deref() {
            return mirrored.or(advanced);
        }
        self.which_key = rows;
        Redraw::Needed
    }

    /// Recomputes the layout for a new terminal size.
    fn resize(&mut self, area: Rect) -> Redraw {
        if area == self.area {
            return Redraw::Skipped;
        }
        self.area = area;
        self.windows.set_terminal(shell_areas(area).body);
        if let Some(review) = self.review.as_mut() {
            review.set_height_rows(review_body_rows(area));
        }
        Redraw::Needed
    }

    /// Resolves one key and applies the command, the prompt edit, or the text.
    fn handle_key(&mut self, key: Key, now: Duration) -> Redraw {
        // A float is decoration of one answer, so the next key closes it.
        let closed = self.close_float();
        self.resolve_key(key, now).or(closed)
    }

    /// Resolves one key and applies what it names.
    fn resolve_key(&mut self, key: Key, now: Duration) -> Redraw {
        let resolution = self.resolver.resolve(key, now);
        self.apply_resolution(resolution)
    }

    /// Applies what one resolution names.
    fn apply_resolution(&mut self, resolution: Resolution) -> Redraw {
        match resolution {
            Resolution::Command {
                command,
                count,
                register,
            } => self.dispatch_command(command, count, register),
            Resolution::Prompt(edit) => self.apply_prompt(edit),
            Resolution::Confirmation(edit) => self.edit_confirmation(edit),
            // A pending sequence and a cancelled sequence both change only the
            // which-key overlay, and `settle` publishes that change.
            Resolution::Pending | Resolution::Cancelled => Redraw::Skipped,
            // The Insert scope declares the editor as its text owner, so a
            // printable key becomes buffer text.
            Resolution::Text(value) => self.insert_typed(value),
            // No binding and no text owner took the key, so nothing changes.
            Resolution::NoMatch => Redraw::Skipped,
        }
    }

    /// Applies one semantic command.
    ///
    /// The window tree sees every command first, because it owns the split,
    /// focus, resize, and close commands. The editing state sees the rest.
    fn dispatch_command(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
        register: Option<char>,
    ) -> Redraw {
        // The access of the instance decides before any owner sees the command,
        // so no view-only editor reaches a text change or a workspace write.
        // See `docs/embedding.md`.
        if self.access == EditorAccess::ViewOnly && command.authority() != CommandAuthority::Read {
            return self.refuse(Refusal::ViewOnly);
        }
        // An open question and an open prompt line own the input, exactly as
        // they do for one resolved key. See `docs/input-actions.md`.
        if let Some(redraw) = self.route_to_open_line(command) {
            return redraw;
        }
        // The open review owns every key while it stays open, so a review key
        // never reaches a buffer. See `docs/diff-view.md`.
        if self.review_open {
            return self.apply_review_command(command, count);
        }
        if command == Command::OpenReview {
            return self.open_review();
        }
        let cleared = self.clear_message();
        // An open picker owns every key, so a picker key never reaches the
        // buffer of an editor window.
        if self.picker.is_some() {
            return self.apply_picker_command(command).or(cleared);
        }
        // The sidebar owns its own keys while it holds the focus. A command
        // that the sidebar does not own falls through, so a leader sequence
        // reaches its command from the sidebar as it does from a buffer. See
        // `docs/input-actions.md`.
        if self.sidebar_has_focus()
            && let Some(redraw) = self.apply_tree_command(command, count)
        {
            return redraw.or(cleared);
        }
        match command {
            Command::OpenFilePicker => return self.open_picker(PickerKind::Files).or(cleared),
            Command::OpenRipgrepPicker => return self.open_picker(PickerKind::Search).or(cleared),
            Command::OpenBufferPicker => return self.open_picker(PickerKind::Buffers).or(cleared),
            Command::OpenCommandLine => return self.open_prompt(PromptKind::CommandLine),
            Command::OpenSearchPrompt => return self.open_prompt(PromptKind::Search),
            // The file and buffer commands reach the same paths as `:w`, `:q`,
            // and the buffer list, so both entry points behave alike.
            Command::SaveBuffer => return self.save_active(AfterSave::Stay).or(cleared),
            Command::UnloadBuffer => return self.unload_active().or(cleared),
            Command::CloseWindow => {
                return self.close_window(UnsavedChanges::Ask).or(cleared);
            }
            Command::ToggleComment => return self.toggle_comment().or(cleared),
            // A paste reads the system clipboard first, so the unnamed register
            // carries an external copy as well. A paste that names another
            // register reads that register alone, so it starts no clipboard
            // work. See `docs/clipboard.md`.
            Command::PasteAfter | Command::PasteBefore => {
                if Self::names_another_register(register) {
                    return self
                        .apply_editing_command(command, count, register)
                        .or(cleared);
                }
                return self
                    .start_clipboard(ClipboardWork::Paste {
                        command,
                        count,
                        register,
                    })
                    .or(cleared);
            }
            // The language commands build one bounded request or read the
            // published diagnostics. None of them waits for a server.
            Command::GoToDefinition => {
                return self.ask_at_cursor(QueryPurpose::Definition).or(cleared);
            }
            Command::ShowHover => return self.ask_at_cursor(QueryPurpose::Hover).or(cleared),
            Command::ShowDiagnosticFloat => return self.show_diagnostic_float().or(cleared),
            Command::NextDiagnostic => {
                return self.jump_diagnostic(DiagnosticJump::Next).or(cleared);
            }
            Command::PreviousDiagnostic => {
                return self.jump_diagnostic(DiagnosticJump::Previous).or(cleared);
            }
            // The jump list of the focused window. Both steps read the recorded
            // positions and start no request of their own.
            Command::JumpBack => return self.jump(JumpDirection::Backward).or(cleared),
            Command::JumpForward => return self.jump(JumpDirection::Forward).or(cleared),
            // The motions that Vim records as jumps. Every other motion, and
            // every half-page and full-page move, records nothing. The same key
            // as the target of a waiting operator moves inside one change, so
            // it records nothing either. See `docs/input-actions.md`.
            Command::MoveFirstLine
            | Command::MoveLastLine
            | Command::MoveMatchingBracket
            | Command::SearchNext
            | Command::SearchPrevious => {
                if self.editing.pending_operator().is_none() {
                    self.record_jump();
                }
                return self
                    .apply_editing_command(command, count, register)
                    .or(cleared);
            }
            Command::EndSearch => return self.end_search().or(cleared),
            Command::ToggleFormatOnSave => return self.toggle_format_on_save().or(cleared),
            Command::RevealInFileTree => return self.reveal_active_file().or(cleared),
            // Insert-mode text entry. The Insert scope binds these four keys,
            // because none of them types a character.
            Command::InsertLineBreak => return self.insert_line_break().or(cleared),
            Command::DeleteCharacterBefore => return self.delete_character_before().or(cleared),
            Command::DeleteWordBefore => return self.delete_word_before().or(cleared),
            Command::InsertIndent => return self.insert_indent().or(cleared),
            _ => {}
        }
        match self.windows.apply(command) {
            WindowOutcome::Ignored => {}
            WindowOutcome::Changed => {
                self.follow_focused_window();
                // A focus move can reach the sidebar, which owns its own keys.
                self.sync_context();
                return Redraw::Needed;
            }
            WindowOutcome::Unchanged => {
                // A focus move that changes nothing reached the outer edge of
                // this editor, so the host decides what lies beyond it.
                if let Some(direction) = focus_direction(command) {
                    self.note_request(InputRequest::FocusBoundary(direction));
                }
                return cleared;
            }
            WindowOutcome::LastWindow => {
                self.close_editor();
                return cleared;
            }
        }
        self.apply_editing_command(command, count, register)
            .or(cleared)
    }

    /// Routes one command to the open question or the open prompt line.
    ///
    /// [`PromptEdit::of_command`] and [`ConfirmEdit::of_command`] own the one
    /// mapping, so a host-supplied command reaches the same owner as the key
    /// that names it. A command that no open line owns returns `None` and
    /// continues to the ordinary owners.
    fn route_to_open_line(&mut self, command: Command) -> Option<Redraw> {
        if self.confirmation.is_some() {
            // A question owns every key below it, so an unnamed command edits
            // nothing and reaches no owner under the question.
            let edit = ConfirmEdit::of_command(command).unwrap_or(ConfirmEdit::Ignore);
            return Some(self.edit_confirmation(edit));
        }
        // An open picker owns its own chords above the query line, so only the
        // prompt commands reach the line itself.
        let edit = PromptEdit::of_command(command)?;
        self.prompt.as_ref()?;
        Some(self.apply_prompt(edit))
    }

    /// Applies one command to the buffer of the focused window.
    ///
    /// A deferred paste reaches the same entry point after its system clipboard
    /// read resolved, so both paths run the identical transition.
    fn apply_editing_command(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
        register: Option<char>,
    ) -> Redraw {
        let auto = self.auto_indent(command);
        let outcome = self.edit(|editing, context, window| {
            editing.apply_indented_with_register(context, window, command, count, auto, register)
        });
        self.sync_context();
        self.report(outcome)
    }

    /// Points the session at the buffer of the focused window.
    ///
    /// A focus change, a split, and a close all move the focus. The session
    /// follows one active buffer, so it must follow that move. Otherwise a key
    /// would change a buffer that the focused window does not show.
    ///
    /// The move changes no cursor. Every window owns its cursor, its selection
    /// anchor, and its viewport, so a return to a window resumes exactly where
    /// it was. See `docs/windows.md`.
    fn follow_focused_window(&mut self) {
        let window = self.windows.focused_window();
        let Some(buffer) = self.windows.buffer(window) else {
            debug_assert!(false, "the focused window is always a leaf of the tree");
            return;
        };
        if self.active == buffer {
            return;
        }
        self.active = buffer;
        let path = self
            .buffers
            .get(buffer)
            .and_then(FileBuffer::target)
            .map(|target| target.relative_path().clone());
        self.events.note_active_file(path);
        // The mode, the pending operator, and the repeat description describe
        // the buffer that the keys changed.
        self.editing = EditingState::new();
        // The recorded matches belong to the previous buffer.
        self.search = None;
    }

    /// Returns the selection that the focused window shows.
    ///
    /// The mode is global and belongs to the focused window, so no other window
    /// holds a selection.
    #[must_use]
    pub fn selection(&self) -> Option<Selection> {
        let state = self.windows.state(self.windows.focused_window())?;
        self.editing.selection(self.buffer(), &state)
    }

    /// Returns the cursor of the focused window.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        let Some(state) = self.windows.state(self.windows.focused_window()) else {
            debug_assert!(false, "the focused window is always a leaf of the tree");
            return Cursor::ORIGIN;
        };
        state.cursor()
    }

    /// Places the cursor of the focused window at a line and a column.
    ///
    /// The command line `:<number>` and a language-service jump both reach this
    /// entry point. The caller reconciles the viewports afterwards.
    fn place_cursor(&mut self, line: usize, column: usize) {
        let Some(active) = self.buffers.get(self.active) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return;
        };
        let window = self.windows.focused_window();
        let Some(state) = self.windows.state_mut(window) else {
            debug_assert!(false, "the focused window is always a leaf of the tree");
            return;
        };
        self.editing.move_to(active.text(), state, line, column);
    }

    /// Returns the automatic indent that one command uses for a new line.
    ///
    /// Only `o`, `O`, a Visual selection move, and a repeat of one of them read
    /// the value. The session asks the accepted analysis of the current buffer
    /// version, and falls back to the previous-line rule when no result
    /// answers. The editor never waits for a parse result. See
    /// `docs/language-services.md`.
    fn auto_indent(&self, command: Command) -> AutoIndent {
        let buffer = self.buffer();
        let cursor_line = self.cursor().line();
        // The moved block lands behind one line, so it takes the indent of a new
        // line at the end of that line. Only the editor knows which line that
        // is, because the line follows from the selection and the direction.
        if let Some(direction) = MoveDirection::of_command(command) {
            let landing = self
                .selection()
                .and_then(|selection| selection_move_indent_line(buffer, selection, direction));
            let Some(line) = landing else {
                return AutoIndent::PreviousLine;
            };
            return self.indent_level_after(line);
        }
        let byte = match command {
            // The new line opens after the text of the cursor line.
            Command::OpenLineBelow => return self.indent_level_after(cursor_line),
            // The new line opens before the text of the cursor line.
            Command::OpenLineAbove => buffer.char_to_byte(buffer.line_start(cursor_line)).get(),
            // Every other command ignores the value, and a repeat re-reads it
            // through the command that it replays.
            _ => return AutoIndent::PreviousLine,
        };
        self.indent_level(byte)
    }

    /// Returns the syntax indent for a new line behind the text of one line.
    fn indent_level_after(&self, line: LineIndex) -> AutoIndent {
        let buffer = self.buffer();
        let end = buffer.line_len_chars(line);
        let Ok(column) = buffer.source_column(line, end) else {
            return AutoIndent::PreviousLine;
        };
        self.indent_level(
            buffer
                .char_to_byte(buffer.column_to_char(line, column))
                .get(),
        )
    }

    /// Returns the syntax indent for a new line at one byte offset.
    fn indent_level(&self, byte: usize) -> AutoIndent {
        let version = self.buffer().version();
        self.analysis
            .get(&self.active)
            .and_then(|entry| entry.syntax.indent_level(version, byte))
            .map_or(AutoIndent::PreviousLine, |level| {
                AutoIndent::Levels(level.get())
            })
    }

    /// Toggles the line comment of the cursor line or of the selection.
    ///
    /// Only a language adapter knows the comment token of a path, so the
    /// session reads it here and hands it to the editor. A buffer without an
    /// adapter, or a language without a line token, stays unchanged and the
    /// message line reports the reason.
    fn toggle_comment(&mut self) -> Redraw {
        let comment = self
            .active_buffer()
            .path()
            .and_then(|path| self.languages.adapter(path).ok())
            .and_then(|adapter| adapter.comment().line_token());
        let outcome =
            self.edit(|editing, context, window| editing.toggle_comment(context, window, comment));
        self.sync_context();
        if outcome == CommandOutcome::Unhandled {
            self.set_message(NO_COMMENT_TOKEN_NOTE, MessageLevel::Warning);
            return Redraw::Needed;
        }
        self.report(outcome)
    }

    /// Returns the number of cells that one indent level takes in the active
    /// buffer.
    ///
    /// Only a language adapter knows the width of its language, so the session
    /// reads it here and hands it to the editor, exactly as it does for the
    /// comment token. A buffer that no adapter serves answers `None`, so the
    /// settings width applies. `EditorSettings` owns the resolution order. See
    /// `docs/settings.md`.
    fn language_indent_width(&self) -> Option<NonZeroU8> {
        self.active_buffer()
            .path()
            .and_then(|path| self.languages.adapter(path).ok())
            .map(|adapter| adapter.indent_rule().width)
    }

    /// Runs one change against the buffer, the registers, and the focused
    /// viewport.
    ///
    /// The viewport travels out of the window tree and back in one place, so no
    /// caller can lose a scroll position.
    fn edit<F>(&mut self, change: F) -> CommandOutcome
    where
        F: FnOnce(&mut EditingState, &mut EditContext<'_>, &mut WindowState) -> CommandOutcome,
    {
        let language_indent_width = self.language_indent_width();
        let window = self.windows.focused_window();
        let Some(mut state) = self.windows.state(window) else {
            debug_assert!(false, "the focused window is always a leaf of the tree");
            return CommandOutcome::Unhandled;
        };
        let Some(active) = self.buffers.get_mut(self.active) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return CommandOutcome::Unhandled;
        };
        // The tree move reads the text as it was before the change, so the
        // snapshot must exist before the command runs. It shares the rope, so
        // it costs no text memory.
        let before = active.text().snapshot();
        let mut context = EditContext {
            buffer: active.text_mut(),
            settings: &self.settings,
            search: self.search.as_ref().map(|search| &search.query),
            language_indent_width,
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        let outcome = change(&mut self.editing, &mut context, &mut state);
        let applied = std::mem::take(&mut context.applied);
        // Every text change passes one access gate before it reaches this
        // point, so a view-only editor produces no transaction at all. See
        // `docs/embedding.md`.
        debug_assert!(
            self.access == EditorAccess::ReadWrite || applied.is_empty(),
            "view-only access refuses every text change before the buffer sees it"
        );
        let after = context.buffer.version();
        if let Some(slot) = self.windows.state_mut(window) {
            *slot = state;
        }
        self.advance_syntax(&before, after, &applied);
        self.synchronize_language(&before, after, &applied);
        outcome
    }

    /// Sends the applied transaction of the active buffer to its server.
    ///
    /// The changes come from the buffer as it was before the transaction, and
    /// they carry the version that the transaction produced. A change that the
    /// protocol cannot describe, such as an undo or a redo, opens the document
    /// again with its exact text instead.
    fn synchronize_language(
        &mut self,
        before: &TextBuffer,
        after: BufferVersion,
        applied: &[EditTransaction],
    ) {
        if after == before.version() {
            return;
        }
        let buffer = self.active;
        let Some(file) = self.buffers.get(buffer) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return;
        };
        let Some(path) = file.path().map(Path::to_path_buf) else {
            return;
        };
        if self.language.awaits_open(buffer) {
            // The queued open already carries the newest text.
            return;
        }
        let [transaction] = applied else {
            self.language.mark_resync(buffer);
            return;
        };
        let Ok(changes) = content_changes(before, transaction) else {
            self.language.mark_resync(buffer);
            return;
        };
        self.queue_language(LanguageRequest::Change {
            buffer,
            path,
            version: after,
            changes,
        });
    }

    /// Moves the reuse tree of the active buffer over one applied transaction.
    ///
    /// The move costs one step for each change, so the next analysis reparses
    /// incrementally. A version that the session cannot move the tree over, such
    /// as an undo or a redo, drops the tree, and the next analysis parses the
    /// complete source instead of reusing a tree that describes other text.
    fn advance_syntax(
        &mut self,
        before: &TextBuffer,
        after: BufferVersion,
        applied: &[EditTransaction],
    ) {
        let Some(entry) = self.analysis.get_mut(&self.active) else {
            return;
        };
        let Some((version, tree)) = entry.reuse.take() else {
            return;
        };
        if version != before.version() {
            return;
        }
        // One command commits at most one transaction. A longer list would need
        // the buffer between the transactions, which no caller holds.
        let [transaction] = applied else {
            if applied.is_empty() {
                entry.reuse = Some((version, tree));
            }
            return;
        };
        entry.reuse = Some((after, tree.edited(before, transaction)));
    }

    /// Inserts one typed character while Insert mode is active.
    ///
    /// The shared resolver routes every printable key of the Insert scope to
    /// this text owner, so the session compares no key.
    fn insert_typed(&mut self, value: char) -> Redraw {
        let mut text = [0_u8; 4];
        self.insert_owned_text(value.encode_utf8(&mut text))
    }

    /// Inserts one run of literal text while Insert mode is active.
    ///
    /// One typed key, one bounded paste, and one host-supplied literal all
    /// reach this owner, so all three follow the identical rule.
    fn insert_owned_text(&mut self, text: &str) -> Redraw {
        if text.is_empty() {
            return Redraw::Skipped;
        }
        // The open question and the open prompt line own literal text before
        // the buffer does, exactly as they do for one resolved key. Neither
        // one changes a file, so view-only access reaches both.
        if self.confirmation.is_some() {
            return text.chars().fold(Redraw::Skipped, |redraw, value| {
                redraw.or(self.edit_confirmation(ConfirmEdit::Insert(value)))
            });
        }
        if self.prompt.is_some() {
            return text.chars().fold(Redraw::Skipped, |redraw, value| {
                redraw.or(self.apply_prompt(PromptEdit::Insert(value)))
            });
        }
        if self.access == EditorAccess::ViewOnly {
            return self.refuse(Refusal::ViewOnly);
        }
        if self.editing.mode() != Mode::Insert {
            return Redraw::Skipped;
        }
        let text = text.to_owned();
        let outcome =
            self.edit(|editing, context, window| editing.insert_text(context, window, &text));
        self.report(outcome)
    }

    /// Opens a line break at the cursor.
    ///
    /// The `editor` module owns every text rule, so the break reaches its entry
    /// point. The break opens at the cursor, so the syntax indent answers for
    /// that byte offset.
    fn insert_line_break(&mut self) -> Redraw {
        let buffer = self.buffer();
        let byte = buffer.char_to_byte(self.cursor().position(buffer)).get();
        let auto = self.indent_level(byte);
        let outcome = self.edit(|editing, context, window| {
            editing.insert_line_break_indented(context, window, auto)
        });
        self.report(outcome)
    }

    /// Removes the character before the cursor.
    fn delete_character_before(&mut self) -> Redraw {
        let outcome =
            self.edit(|editing, context, window| editing.delete_backward(context, window));
        self.report(outcome)
    }

    /// Removes the word before the cursor.
    ///
    /// The delete also removes the blanks between the cursor and that word, so
    /// `Ctrl-W` matches Vim, readline, and every terminal shell.
    fn delete_word_before(&mut self) -> Redraw {
        let outcome =
            self.edit(|editing, context, window| editing.delete_word_backward(context, window));
        self.report(outcome)
    }

    /// Inserts one indent step.
    ///
    /// The indent settings decide between one tab character and the spaces of
    /// one indent level. `EditorSettings` resolves that width against the
    /// language of the buffer, so the tab key steps by the same width as the
    /// automatic indent and the `<` and `>` commands. See `docs/settings.md`.
    fn insert_indent(&mut self) -> Redraw {
        let indent = self.settings.indent;
        let text = if indent.expand_tab {
            let columns = indent.indent_columns(self.language_indent_width());
            " ".repeat(usize::from(columns.get()))
        } else {
            "\t".to_owned()
        };
        let outcome =
            self.edit(|editing, context, window| editing.insert_text(context, window, &text));
        self.report(outcome)
    }

    /// Opens one line prompt and moves input to it.
    ///
    /// The prompt returns input to the scope that owned it, so a file-tree
    /// prompt returns the keys to the sidebar.
    fn open_prompt(&mut self, kind: PromptKind) -> Redraw {
        self.prompt = Some(PromptLine {
            kind,
            text: String::new(),
            completion: None,
        });
        // The new line asks for its own walk, and it asks when it first holds a
        // path argument.
        self.completion_walk = CompletionWalk::Unasked;
        self.sync_context();
        Redraw::Needed
    }

    /// Opens one confirmation and moves input to it.
    ///
    /// The confirmation returns input to the owner that held it, exactly as a
    /// prompt does, so a question of the file-tree sidebar returns the keys to
    /// the sidebar, and a question over an open prompt returns them to that
    /// prompt. At most one question waits, so a request while another one waits
    /// opens nothing and changes nothing.
    pub(super) fn open_confirmation(
        &mut self,
        question: impl Into<String>,
        action: ConfirmedAction,
    ) -> ConfirmationRequest {
        if self.confirmation.is_some() {
            return ConfirmationRequest::Refused;
        }
        self.confirmation = Some(Confirmation {
            question: clip_message_line(question),
            answer: String::new(),
            action,
        });
        self.sync_context();
        ConfirmationRequest::Opened
    }

    /// Applies one edit of the answer of the open confirmation.
    ///
    /// Only `Enter`, `Esc`, and `Ctrl-C` close the question, so a `Backspace` on
    /// the empty answer keeps it open. One keypress therefore never performs the
    /// action. See `docs/input-actions.md`.
    fn edit_confirmation(&mut self, edit: ConfirmEdit) -> Redraw {
        let Some(confirmation) = self.confirmation.as_mut() else {
            debug_assert!(
                false,
                "the resolver edits a confirmation only while one is open"
            );
            return Redraw::Skipped;
        };
        match edit {
            ConfirmEdit::Insert(value) => {
                if confirmation.answer.chars().count() >= CONFIRM_ANSWER_CHARS_MAX {
                    return Redraw::Skipped;
                }
                confirmation.answer.push(value);
                Redraw::Needed
            }
            ConfirmEdit::DeleteBackward => {
                if confirmation.answer.pop().is_none() {
                    return Redraw::Skipped;
                }
                Redraw::Needed
            }
            ConfirmEdit::DeleteWordBackward => delete_word_backward(&mut confirmation.answer),
            ConfirmEdit::Accept => self.close_confirmation(ConfirmationClose::Accept),
            ConfirmEdit::Cancel => self.close_confirmation(ConfirmationClose::Cancel),
            ConfirmEdit::Ignore => Redraw::Skipped,
        }
    }

    /// Closes the open confirmation and performs the action that it approves.
    ///
    /// A cancelled question performs nothing and leaves no trace, so the editor
    /// returns to the state that it held before the question. An accepted
    /// question reads the typed answer, and only `y` and `yes` perform the
    /// action.
    fn close_confirmation(&mut self, close: ConfirmationClose) -> Redraw {
        let Some(confirmation) = self.confirmation.take() else {
            debug_assert!(
                false,
                "the resolver closes a confirmation only while one is open"
            );
            return Redraw::Skipped;
        };
        self.sync_context();
        let answer = match close {
            ConfirmationClose::Cancel => ConfirmAnswer::No,
            ConfirmationClose::Accept => ConfirmAnswer::from_text(&confirmation.answer),
        };
        match answer {
            ConfirmAnswer::No => Redraw::Needed,
            ConfirmAnswer::Yes => self
                .perform_confirmed(confirmation.action)
                .or(Redraw::Needed),
        }
    }

    /// Opens one question of an action that destroys data.
    ///
    /// Every question of this entry point follows a key that the user pressed,
    /// and an open question owns every key, so no second question can reach this
    /// point. A refusal therefore names a defect, and it destroys nothing. The
    /// overwrite question follows a worker result instead, so it opens the
    /// confirmation directly and handles the refusal itself.
    fn ask_confirmation(&mut self, question: String, action: ConfirmedAction) -> Redraw {
        match self.open_confirmation(question, action) {
            ConfirmationRequest::Opened => {}
            ConfirmationRequest::Refused => debug_assert!(
                false,
                "the resolver hands every key to the open confirmation"
            ),
        }
        Redraw::Needed
    }

    /// Performs the action that one confirmed question named.
    fn perform_confirmed(&mut self, action: ConfirmedAction) -> Redraw {
        match action {
            // The tree stages the removal here, not when the question opened,
            // so a watcher event that dropped the entry meanwhile refuses it.
            ConfirmedAction::DeleteEntries { paths } => {
                let staged = self.tree.stage_delete(paths);
                self.start_tree_mutation(staged)
            }
            // The worker stages the operation again here, so a destination
            // that changed while the question waited refuses the overwrite.
            ConfirmedAction::Overwrite {
                operation,
                destinations,
            } => self.start_tree_overwrite(operation, destinations),
            // The quit reads the focused window again here, so an open that
            // completed meanwhile keeps the unsaved changes of its own buffer.
            ConfirmedAction::DiscardOnQuit { buffer } => self.confirmed_quit(buffer),
            // The reload names its buffer, so it reads the file of the buffer
            // that the question named, never of the focused window.
            ConfirmedAction::DiscardOnReload { buffer } => {
                self.reload_buffer(buffer, UnsavedText::Discard)
            }
            #[cfg(test)]
            ConfirmedAction::Report => {
                self.set_message("the confirmation reached its action", MessageLevel::Info);
                Redraw::Needed
            }
        }
    }

    /// Returns the scope that owns the keys while no prompt is open.
    fn input_scope(&self) -> BindingScope {
        if self.review_open {
            BindingScope::Review
        } else if self.picker.is_some() {
            BindingScope::Picker
        } else if self.sidebar_has_focus() {
            BindingScope::Sidebar
        } else {
            BindingScope::Mode(self.editing.mode())
        }
    }

    /// Opens one picker over the complete terminal.
    ///
    /// The picker reads its query through the prompt of the message line, which
    /// its own overlay shows, so the editor opens no second input mechanism.
    /// The buffer picker receives its candidates at once, and the other two ask
    /// the bounded services for theirs. See `docs/files.md`.
    fn open_picker(&mut self, kind: PickerKind) -> Redraw {
        let root = Arc::clone(&self.root);
        let buffers = match kind {
            PickerKind::Buffers => self.buffer_candidates(),
            PickerKind::Files | PickerKind::Search => Vec::new(),
        };
        self.picker = Some(PickerState::open(kind, root, buffers));
        self.open_prompt(PromptKind::Picker)
    }

    /// Returns one candidate for every loaded buffer.
    fn buffer_candidates(&self) -> Vec<Candidate> {
        let root = self.tree.tree().root();
        self.buffers
            .ids()
            .into_iter()
            .filter_map(|id| {
                let file = self.buffers.get(id)?;
                Some(Candidate::buffer(root, id, file.path(), file.name()))
            })
            .collect()
    }

    /// Applies one semantic command while a picker owns the keys.
    fn apply_picker_command(&mut self, command: Command) -> Redraw {
        let Some(picker) = self.picker.as_mut() else {
            debug_assert!(false, "the caller checked that one picker is open");
            return Redraw::Skipped;
        };
        match command {
            Command::PickerSelectNext => picker.select_next(),
            Command::PickerSelectPrevious => picker.select_previous(),
            // The picker table holds no other command.
            _ => return Redraw::Skipped,
        }
        Redraw::Needed
    }

    /// Opens the accepted row and closes the picker.
    ///
    /// A row that names a file opens it at the matched line. A row that names a
    /// loaded buffer needs no file read at all.
    fn accept_picker(&mut self, picker: Option<&PickerState>) -> Redraw {
        let Some(acceptance) = picker.and_then(PickerState::accept) else {
            // An empty result list accepts nothing, and the closed picker
            // restores the previous view.
            return Redraw::Needed;
        };
        // The accepted row moves the cursor, and it can also change the buffer
        // of the focused window, so the position under the picker joins the
        // list before either move.
        self.record_jump();
        match acceptance {
            Acceptance::ShowBuffer { buffer } => self.switch_to(buffer).or(Redraw::Needed),
            Acceptance::OpenFile {
                path,
                line,
                byte_column,
            } => {
                let position = DocumentPosition::new(
                    u32::try_from(line).unwrap_or(u32::MAX),
                    u32::try_from(byte_column).unwrap_or(0),
                );
                self.open_at(path, PendingPosition::Document(position))
                    .or(Redraw::Needed)
            }
        }
    }

    /// Moves the query of the open picker to the text of its prompt.
    fn sync_picker_query(&mut self) {
        let query = self
            .prompt
            .as_ref()
            .filter(|prompt| prompt.kind == PromptKind::Picker)
            .map(|prompt| prompt.text.clone());
        let (Some(query), Some(picker)) = (query, self.picker.as_mut()) else {
            return;
        };
        picker.set_query(&query);
    }

    /// Asks for the workspace walk when the line first holds a path argument.
    ///
    /// Most command lines take no path, so `:w`, `:q`, and a line number ask
    /// for no walk of the workspace at all. The parser owns the rule that names
    /// the command that takes a path, so the parser and the completion can
    /// never disagree about it.
    ///
    /// The state of the walk records the request of the line, so every later
    /// character of that line asks for no second walk. See `docs/files.md`.
    fn sync_completion_walk(&mut self) {
        if !matches!(self.completion_walk, CompletionWalk::Unasked) {
            return;
        }
        let takes_path = self.prompt.as_ref().is_some_and(|prompt| {
            prompt.kind == PromptKind::CommandLine
                && CommandLineCommand::path_argument(&prompt.text).is_some()
        });
        if !takes_path {
            return;
        }
        self.completion_walk = CompletionWalk::Queued(PickerRequest::Files {
            root: Arc::clone(&self.root),
        });
    }

    /// Takes the picker request that the event loop must submit.
    ///
    /// The session never walks the workspace, never runs `rg`, and never reads
    /// a preview, so the event loop hands the request to the bounded worker or
    /// process service. See `docs/responsiveness.md`.
    pub fn take_picker_request(&mut self) -> Option<PickerRequest> {
        self.picker.as_mut()?.take_request()
    }

    /// Applies one completed picker operation as one state transition.
    ///
    /// A result that reaches no open picker changes nothing, because the reader
    /// already closed the overlay that asked for it.
    #[must_use]
    pub fn apply_picker_result(&mut self, result: PickerResult) -> Redraw {
        let Some(picker) = self.picker.as_mut() else {
            return Redraw::Skipped;
        };
        let redraw = picker.apply_result(result);
        self.reconcile_picker();
        redraw
    }

    /// Takes the workspace walk that the open command line asked for.
    ///
    /// The session walks no directory, so the event loop hands the request to
    /// the bounded worker service. The command line keeps every key while the
    /// walk runs. See `docs/responsiveness.md`.
    pub fn take_completion_request(&mut self) -> Option<PickerRequest> {
        self.completion_walk.take_request()
    }

    /// Applies the finished workspace walk of the command-line completion.
    ///
    /// The list opens on the next completion key, so the result changes no
    /// visible state and requests no frame.
    ///
    /// A result that reaches a closed command line changes nothing, because the
    /// user already left the line that asked for it. A line that asked for no
    /// walk holds no result either.
    #[must_use]
    pub fn apply_completion_result(&mut self, result: PickerResult) -> Redraw {
        let PickerResult::Candidates { candidates, .. } = result else {
            debug_assert!(
                false,
                "the command line asks for one workspace walk and for no preview"
            );
            return Redraw::Skipped;
        };
        debug_assert!(
            self.prompt.is_some() || matches!(self.completion_walk, CompletionWalk::Unasked),
            "the closed prompt drops the walk of the line that asked for it"
        );
        let CompletionWalk::Taken(files) = &mut self.completion_walk else {
            return Redraw::Skipped;
        };
        *files = candidates;
        Redraw::Skipped
    }

    /// Reports that one picker request produced no result.
    ///
    /// A missing external command is a normal state. The editor names it once
    /// and stays fully usable without the search picker.
    #[must_use]
    pub fn abandon_picker_request(&mut self, slot: PickerSlot, failure: PickerFailure) -> Redraw {
        let Some(picker) = self.picker.as_mut() else {
            return Redraw::Skipped;
        };
        let redraw = picker.abandon(slot, failure);
        if failure == PickerFailure::CommandMissing && !self.ripgrep_reported {
            self.ripgrep_reported = true;
            self.set_message(RIPGREP_MISSING_NOTE, MessageLevel::Warning);
            return Redraw::Needed;
        }
        redraw
    }

    /// Moves the visible result rows so the selected row stays visible.
    fn reconcile_picker(&mut self) {
        let rows = usize::from(picker_areas(self.area).results.height);
        if let Some(picker) = self.picker.as_mut() {
            picker.reconcile(rows);
        }
    }

    /// Applies one edit of the open prompt line.
    fn apply_prompt(&mut self, edit: PromptEdit) -> Redraw {
        let Some(prompt) = self.prompt.as_mut() else {
            debug_assert!(
                false,
                "the resolver reports a prompt edit only while one is open"
            );
            return Redraw::Skipped;
        };
        match edit {
            PromptEdit::Insert(value) => {
                if prompt.text.chars().count() >= prompt.chars_max() {
                    return Redraw::Skipped;
                }
                // The insert continues from the line as it is shown, so the
                // closed completion leaves the written candidate in the text
                // and the next completion starts from the new line.
                prompt.completion = None;
                prompt.text.push(value);
                self.sync_picker_query();
                self.sync_completion_walk();
                Redraw::Needed
            }
            PromptEdit::DeleteBackward => {
                prompt.completion = None;
                // Backspace on the empty line cancels the prompt, like Vim.
                if prompt.text.pop().is_none() {
                    self.close_prompt();
                    return Redraw::Needed;
                }
                self.sync_picker_query();
                self.sync_completion_walk();
                Redraw::Needed
            }
            PromptEdit::DeleteWordBackward => {
                prompt.completion = None;
                // A host can bind `Ctrl-W` as its own prefix, so a stray chord
                // must never close a prompt of this editor. Unlike `Backspace`,
                // the chord therefore leaves the empty line open.
                if delete_word_backward(&mut prompt.text) == Redraw::Skipped {
                    return Redraw::Skipped;
                }
                self.sync_picker_query();
                self.sync_completion_walk();
                Redraw::Needed
            }
            PromptEdit::CompleteNext => self.complete_prompt(CompletionCycle::Next),
            PromptEdit::CompletePrevious => self.complete_prompt(CompletionCycle::Previous),
            PromptEdit::Cancel => {
                // The open candidate list takes the cancel first and restores
                // the typed text, so a second cancel closes the prompt. Only
                // the command line completes, so no picker query changes here.
                if let Some(completion) = prompt.completion.take() {
                    prompt.text = completion.into_typed();
                    return Redraw::Needed;
                }
                self.close_prompt();
                Redraw::Needed
            }
            PromptEdit::Accept => self.accept_prompt(),
        }
    }

    /// Writes the next or the previous completion candidate into the prompt.
    ///
    /// Only the command line offers candidates today. Every other prompt reads
    /// text alone, so it ignores the two completion keys. See
    /// `docs/input-actions.md`.
    ///
    /// The producer reads the collected workspace files and no directory, so
    /// the key never waits for the filesystem.
    fn complete_prompt(&mut self, cycle: CompletionCycle) -> Redraw {
        let Self {
            prompt,
            completion_walk,
            ..
        } = self;
        let Some(prompt) = prompt.as_mut() else {
            debug_assert!(
                false,
                "the resolver reports a prompt edit only while one is open"
            );
            return Redraw::Skipped;
        };
        if prompt.kind != PromptKind::CommandLine {
            return Redraw::Skipped;
        }
        let outcome = prompt.complete(
            |line| command_line_candidates(line, completion_walk.files()),
            cycle,
        );
        match outcome {
            // A line that names no command, and a path that the collected files
            // do not hold, are both normal states of a line that the user still
            // types, so the miss reports nothing.
            CompletionOutcome::Missed => Redraw::Skipped,
            CompletionOutcome::Completed | CompletionOutcome::Listed => Redraw::Needed,
        }
    }

    /// Runs the accepted prompt line and closes the prompt.
    fn accept_prompt(&mut self) -> Redraw {
        let Some(prompt) = self.prompt.take() else {
            debug_assert!(false, "the caller holds an open prompt");
            return Redraw::Skipped;
        };
        // The picker lives exactly as long as its prompt, so the accepted row
        // leaves the session with it.
        let picker = self.picker.take();
        self.close_prompt();
        match prompt.kind {
            PromptKind::CommandLine => self.run_command_line(&prompt.text),
            PromptKind::Search => self.run_search(&prompt.text),
            PromptKind::Tree(tree) => self.run_tree_prompt(tree, &prompt.text),
            PromptKind::Picker => self.accept_picker(picker.as_ref()),
        }
    }

    /// Runs one parsed command line.
    fn run_command_line(&mut self, line: &str) -> Redraw {
        let command = match CommandLineCommand::parse(line) {
            Ok(command) => command,
            Err(error) => {
                self.set_message(error.to_string(), MessageLevel::Error);
                return Redraw::Needed;
            }
        };
        match command {
            CommandLineCommand::Write => return self.save_active(AfterSave::Stay),
            CommandLineCommand::WriteQuit => return self.save_active(AfterSave::CloseWindow),
            CommandLineCommand::Edit(path) => return self.open_path(path),
            CommandLineCommand::Reload => return self.reload_active(UnsavedChanges::Ask),
            CommandLineCommand::ReloadDiscard => {
                return self.reload_active(UnsavedChanges::Discard);
            }
            CommandLineCommand::Log => return self.open_log(),
            CommandLineCommand::Diagnostics => return self.open_diagnostics(),
            CommandLineCommand::Quit => return self.close_window(UnsavedChanges::Ask),
            CommandLineCommand::QuitDiscard => {
                return self.close_window(UnsavedChanges::Discard);
            }
            CommandLineCommand::GoToLine(line) => {
                // `:<number>` is a jump, so the line under the command line
                // joins the list before the cursor leaves it.
                self.record_jump();
                let target = usize::try_from(line.get()).unwrap_or(usize::MAX);
                self.place_cursor(target - 1, 0);
            }
        }
        Redraw::Needed
    }

    /// Opens one snapshot of the editor log in a new buffer.
    ///
    /// The snapshot is a value, so the buffer never changes while it is open,
    /// and an edit of that buffer changes no entry. A second call opens a
    /// second buffer that holds the log of that later moment. The call reads
    /// editor state only, so it performs no filesystem work. See
    /// `docs/windows.md`.
    fn open_log(&mut self) -> Redraw {
        let snapshot = self.log.snapshot();
        self.open_generated(LOG_BUFFER_NAME, &snapshot)
    }

    /// Opens one generated text in a new buffer of the focused window.
    ///
    /// The buffer is an ordinary scratch buffer that carries no path, so the
    /// reader edits it, searches it, and closes it as any other buffer. The
    /// caller owns the text, so this step performs no filesystem work. See
    /// `docs/windows.md`.
    fn open_generated(&mut self, name: &str, snapshot: &str) -> Redraw {
        let text = match TextBuffer::from_text(snapshot, &self.settings.files) {
            Ok(text) => text,
            Err(error) => {
                self.set_message(error.to_string(), MessageLevel::Error);
                return Redraw::Needed;
            }
        };
        let buffer = FileBuffer::generated(name, text);
        let Some(id) = self.buffers.insert(buffer) else {
            self.report_buffer_limit();
            return Redraw::Needed;
        };
        self.switch_to(id).or(Redraw::Needed)
    }

    /// Opens the host report of this machine in a new buffer.
    ///
    /// The report names every external program that kvim runs, so the probe
    /// reads the executable search path. The event loop reads no path, so the
    /// command queues one bounded job and the buffer opens when the job
    /// answers. The message line reports that the probe runs, and the editor
    /// stays fully usable while it runs. A second command reports the same
    /// state and starts no second probe. See `docs/architecture.md`.
    fn open_diagnostics(&mut self) -> Redraw {
        if matches!(self.host_probe, HostProbe::Unasked) {
            let root = self.tree.tree().root().to_path_buf();
            self.host_probe = HostProbe::Queued(HostReportRequest::new(
                self.languages,
                HostWorkspace::Resolved { root },
            ));
        }
        self.set_message(HOST_REPORT_RUNNING, MessageLevel::Info);
        Redraw::Needed
    }

    /// Takes the host probe that the `:diagnostics` command asked for.
    ///
    /// The session reads no executable search path, so the event loop hands the
    /// request to the bounded worker service. See `docs/responsiveness.md`.
    pub fn take_host_request(&mut self) -> Option<HostReportRequest> {
        self.host_probe.take_request()
    }

    /// Opens the finished host report in a new buffer.
    ///
    /// A report that reaches no running probe changes nothing, because the
    /// session already abandoned the request that asked for it.
    #[must_use]
    pub fn apply_host_report(&mut self, report: &str) -> Redraw {
        if !matches!(self.host_probe, HostProbe::Running) {
            return Redraw::Skipped;
        }
        self.host_probe = HostProbe::Unasked;
        // The probe answered, so the note that named the wait is stale.
        let cleared = self.clear_message();
        self.open_generated(HOST_BUFFER_NAME, report).or(cleared)
    }

    /// Reports that one host probe produced no report.
    ///
    /// The user asked for the report, so the failure reaches the message line
    /// and the next `:diagnostics` starts a fresh probe.
    #[must_use]
    pub fn abandon_host_request(&mut self, failure: HostProbeFailure) -> Redraw {
        self.host_probe = HostProbe::Unasked;
        self.set_message(failure.message(), MessageLevel::Error);
        Redraw::Needed
    }

    /// Reports that the buffer list holds no room for another buffer.
    fn report_buffer_limit(&mut self) {
        self.set_message(
            format!("the editor holds the maximum of {BUFFERS_MAX} buffers"),
            MessageLevel::Error,
        );
    }

    /// Opens one host path in the focused window.
    ///
    /// The command line and the standalone start argument both name a path
    /// that a user typed, so this boundary turns that path into one contained
    /// path and hands it to [`Session::open`]. A path outside the worktree
    /// root reports its refusal and opens nothing.
    ///
    /// A host that already holds one validated [`WorktreeRelativePath`] calls
    /// [`Session::open`] instead, because that path needs no repair.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    ///
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::{Redraw, Session};
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root);
    ///
    /// // A path above the worktree root opens nothing and reports its refusal.
    /// assert_eq!(session.open_path(PathBuf::from("../outside.rs")), Redraw::Needed);
    /// assert!(session.message().is_some());
    /// ```
    pub fn open_path(&mut self, path: PathBuf) -> Redraw {
        let relative = if path.is_absolute() {
            path.strip_prefix(self.root.as_path())
        } else {
            Ok(path.as_path())
        };
        let relative = match relative
            .map_err(|_| "the path is outside the worktree".to_owned())
            .map(|path| {
                path.components()
                    .filter_map(|component| match component {
                        Component::CurDir => None,
                        other => Some(other.as_os_str()),
                    })
                    .collect::<PathBuf>()
            })
            .and_then(|path| WorktreeRelativePath::new(path).map_err(|error| error.to_string()))
        {
            Ok(relative) => relative,
            Err(error) => {
                self.set_message(
                    format!("cannot open {}: {error}", path.display()),
                    MessageLevel::Error,
                );
                return Redraw::Needed;
            }
        };
        self.open(relative)
    }

    /// Takes the file request that the event loop must submit.
    ///
    /// The session never runs the request itself, so the event loop stays free
    /// from filesystem work.
    pub fn take_file_request(&mut self) -> Option<FileRequest> {
        self.file_outbox.take()
    }

    /// Returns the analysis job that the active buffer needs, if any.
    ///
    /// The session never parses, so the event loop hands the job to the bounded
    /// worker service. The job carries the buffer version of its source, and
    /// [`Session::apply_analysis_result`] rejects an obsolete result. A buffer
    /// without a path, or without an adapter, needs no job and renders plain
    /// text. See `docs/language-services.md`.
    pub fn take_analysis_request(&mut self) -> Option<AnalysisRequest> {
        let buffer = self.active;
        let file = self.buffers.get(buffer)?;
        let adapter = self.languages.adapter(file.path()?).ok()?;
        let version = file.text().version();
        if self.analysis_pending == Some((buffer, version)) {
            return None;
        }
        let entry = self.analysis.entry(buffer).or_default();
        if entry
            .syntax
            .analysis()
            .is_some_and(|analysis| analysis.version() == version)
        {
            return None;
        }
        let mut input = AnalysisInput::new(version, Arc::from(file.text().to_string()));
        if let Some((reuse_version, tree)) = &entry.reuse
            && *reuse_version == version
        {
            input = input.reusing(tree.clone());
        }
        self.analysis_pending = Some((buffer, version));
        Some(AnalysisRequest {
            buffer,
            adapter,
            input,
            highlighter: Arc::clone(&self.highlighter),
        })
    }

    /// Publishes one completed analysis behind the buffer-version gate.
    ///
    /// A result for an obsolete buffer version changes nothing and enters no
    /// cache. A typed failure renders plain text and keeps the buffer editable.
    ///
    /// Neither outcome reaches the message line, because highlighting is
    /// decoration. Both therefore reach the editor log, so a user reads why one
    /// file lost its highlighting. See `docs/responsiveness.md`.
    #[must_use]
    pub fn apply_analysis_result(&mut self, result: AnalysisResult) -> Redraw {
        self.analysis_pending = None;
        let Some(file) = self.buffers.get(result.buffer) else {
            // The buffer left the list while the job ran.
            return Redraw::Skipped;
        };
        let current = file.text().version();
        let analysis = match result.outcome {
            Ok(analysis) => analysis,
            Err(error) => {
                self.record_job(
                    JOB_ANALYSIS,
                    MessageLevel::Warning,
                    &format!("failed: {error}"),
                );
                return Redraw::Skipped;
            }
        };
        let Some(entry) = self.analysis.get_mut(&result.buffer) else {
            debug_assert!(
                false,
                "the session creates the entry when it builds the job"
            );
            return Redraw::Skipped;
        };
        if entry.syntax.accept(current, analysis) == Publication::Rejected {
            self.record_job(JOB_ANALYSIS, MessageLevel::Info, JOB_OBSOLETE);
            return Redraw::Skipped;
        }
        entry.reuse = entry
            .syntax
            .analysis()
            .map(|accepted| (current, accepted.tree().clone()));
        Redraw::Needed
    }

    /// Reports that one analysis job produced no result.
    ///
    /// The buffer keeps its previous spans, and the next transition asks for the
    /// job again.
    pub fn abandon_analysis_request(&mut self) {
        self.analysis_pending = None;
    }

    /// Takes the language request that the event loop must send.
    ///
    /// The session never speaks the protocol, so the event loop hands the
    /// request to the language services. A buffer that needs a fresh open comes
    /// first, and its request carries the exact text of the current buffer
    /// version. See `docs/language-services.md`.
    pub fn take_language_request(&mut self) -> Option<LanguageRequest> {
        while let Some(buffer) = self.language.take() {
            if let Some(request) = self.open_request(buffer) {
                return Some(request);
            }
        }
        self.language.take_queued()
    }

    /// Reports how the language services answered one dispatched request.
    ///
    /// The services assign the identity of a query, and the session records it
    /// so a later answer reaches the question that asked for it. A refused
    /// synchronization can leave one server copy behind the buffer, so the
    /// answer names the request that it belongs to.
    #[must_use]
    pub fn apply_language_dispatch(
        &mut self,
        request: &LanguageRequest,
        result: Result<Vec<AcceptedQuery>, LspError>,
    ) -> Redraw {
        match result {
            Ok(accepted) => {
                if request.kind() != LanguageRequestKind::Query {
                    return Redraw::Skipped;
                }
                let Some(pending) = self.language.pending.as_mut() else {
                    return Redraw::Skipped;
                };
                debug_assert!(
                    !accepted.is_empty(),
                    "the language services name the identity of every accepted question"
                );
                pending.accept(accepted);
                Redraw::Skipped
            }
            Err(error) => {
                let redraw = self.report_language_error(&error);
                match request.kind() {
                    LanguageRequestKind::Query => self.abandon_query().or(redraw),
                    LanguageRequestKind::Synchronization => {
                        self.repair_document(request, &error).or(redraw)
                    }
                }
            }
        }
    }

    /// Opens one document again after a running session dropped its request.
    ///
    /// The session holds a copy of that document, and the dropped request
    /// leaves that copy behind the buffer. A drifted copy answers with
    /// diagnostics on the wrong lines, and it computes an edit against text
    /// that the buffer no longer holds. The fresh open carries the complete
    /// text of the current buffer version, and it supersedes every queued
    /// change of that buffer.
    ///
    /// Every other refusal names a state where no session holds a copy, so
    /// nothing drifted and nothing opens again. See
    /// `docs/language-services.md`.
    fn repair_document(&mut self, request: &LanguageRequest, error: &LspError) -> Redraw {
        if LanguageRefusal::of(error) == LanguageRefusal::NoCopyHeld {
            return Redraw::Skipped;
        }
        let Some(buffer) = request.buffer() else {
            // A close names one path that no buffer holds any longer, so no
            // fresh open repairs that copy.
            return Redraw::Skipped;
        };
        self.language.mark_resync(buffer);
        self.set_message(
            "the language server queue is full; the editor opens the buffer again",
            MessageLevel::Warning,
        );
        Redraw::Needed
    }

    /// Applies one typed result of the language services.
    ///
    /// Every result passes the buffer-version gate before it changes visible
    /// state, so an obsolete answer never reaches the screen.
    #[must_use]
    pub fn apply_language_event(&mut self, event: LanguageEvent) -> Redraw {
        let server = event.server;
        match event.outcome {
            LanguageOutcome::Diagnostics(set) => self.publish_diagnostics(server, set),
            LanguageOutcome::Progress(report) => {
                self.notifications
                    .report(server, &report, self.clock, self.settings.notifications)
            }
            LanguageOutcome::Definition {
                request,
                version,
                locations,
            } => self.answer_query(request, Some(version), Answer::Definition(locations)),
            LanguageOutcome::Hover {
                request,
                version,
                markup,
            } => self.answer_query(
                request,
                Some(version),
                markup.map_or(Answer::Empty, Answer::Hover),
            ),
            LanguageOutcome::Formatting { request, edits } => {
                self.answer_query(request, None, Answer::Formatting(edits))
            }
            LanguageOutcome::Failed { request, error } => {
                let redraw = match request {
                    Some(request) => self.answer_query(request, None, Answer::Empty),
                    // A session failure carries no request, so every question
                    // that the failed server took loses its answer.
                    None => {
                        self.record_server(
                            server,
                            MessageLevel::Error,
                            &format!("failed: {error}"),
                        );
                        self.abandon_server(server)
                    }
                };
                self.report_language_error(&error).or(redraw)
            }
            LanguageOutcome::Unavailable => {
                self.record_server(server, MessageLevel::Info, "is not installed");
                let redraw = self.report_language_notice(LanguageNotice::NotInstalled);
                self.abandon_server(server).or(redraw)
            }
            LanguageOutcome::Restarted => {
                self.record_server(server, MessageLevel::Warning, "restarted");
                self.reopen_documents()
            }
            LanguageOutcome::Stopped => {
                self.record_server(server, MessageLevel::Info, "stopped");
                let redraw = self.report_language_notice(LanguageNotice::Stopped);
                self.abandon_server(server).or(redraw)
            }
            LanguageOutcome::Reported(report) => self.record_server_report(server, &report),
        }
    }

    /// Records one report about the server process in the editor log.
    ///
    /// The report changes no visible state, so it needs no redraw. The message
    /// line reports exactly what it reports without the log. See
    /// `docs/windows.md`.
    fn record_server_report(&mut self, server: LanguageServerId, report: &ServerReport) -> Redraw {
        match report {
            ServerReport::Started => self.record_server(server, MessageLevel::Info, "started"),
            // A healthy server writes notes while it runs, so its text is no
            // failure by itself. The lifecycle entry beside it carries the
            // severity of the state.
            ServerReport::Output(text) => self.record_server(server, MessageLevel::Info, text),
            ServerReport::OutputBound => self.record_server(
                server,
                MessageLevel::Warning,
                "wrote more than the editor records; the log holds no further output of this attempt",
            ),
        }
        Redraw::Skipped
    }

    /// Records one language-server entry in the editor log.
    ///
    /// The entry names the adapter and the server, because one language runs
    /// several servers and a reader must know which server made the report.
    fn record_server(&mut self, server: LanguageServerId, level: MessageLevel, text: &str) {
        self.log.record(
            self.clock,
            LogSource::LanguageServer,
            level,
            &format!("{}/{} {text}", server.adapter(), server.server()),
        );
    }

    /// Records the outcome of one background job in the editor log.
    ///
    /// The job reached no message line, so the log is the one place that holds
    /// the outcome. The entry names the job first and the outcome second, and
    /// it stays one line. See `docs/responsiveness.md`.
    pub(super) fn record_job(&mut self, job: &str, level: MessageLevel, outcome: &str) {
        self.log.record(
            self.clock,
            LogSource::BackgroundJob,
            level,
            &format!("{job} {outcome}"),
        );
    }

    /// Returns the open request of one buffer, with its exact current text.
    fn open_request(&self, buffer: BufferId) -> Option<LanguageRequest> {
        let file = self.buffers.get(buffer)?;
        let path = file.path()?.to_path_buf();
        let text = file.text();
        Some(LanguageRequest::Open {
            buffer,
            path,
            version: text.version(),
            text: Arc::from(text.to_string()),
        })
    }

    /// Queues one language request for the event loop.
    ///
    /// A full outbox means the event loop stopped draining it. Every server
    /// copy is then unreliable, so the session opens every document again
    /// instead of sending a change that describes text the server never
    /// received.
    fn queue_language(&mut self, request: LanguageRequest) {
        if self.language.queue(request) {
            return;
        }
        debug_assert!(
            false,
            "the event loop drains the language outbox after every step"
        );
        self.language.resync_all(self.buffers.ids());
    }

    /// Opens every document again after the server restarted.
    ///
    /// The new server holds no document, so every published diagnostic belongs
    /// to a server that no longer runs.
    fn reopen_documents(&mut self) -> Redraw {
        self.language.resync_all(self.buffers.ids());
        self.language.clear_diagnostics();
        let redraw = self.abandon_query();
        self.set_message(
            "the language server restarted; the editor opened its buffers again",
            MessageLevel::Warning,
        );
        redraw.or(Redraw::Needed)
    }

    /// Reports one normal language-service state exactly once.
    fn report_language_notice(&mut self, notice: LanguageNotice) -> Redraw {
        if !self.language.report(notice) {
            return Redraw::Skipped;
        }
        self.set_message(notice.message(), MessageLevel::Info);
        Redraw::Needed
    }

    /// Reports one language-service failure on the message line.
    ///
    /// A normal state reaches the line once. Every other failure is a transient
    /// warning, and the buffer stays editable in both cases.
    fn report_language_error(&mut self, error: &LspError) -> Redraw {
        match LanguageNotice::of(error) {
            Some(notice) => self.report_language_notice(notice),
            None => {
                self.set_message(error.to_string(), MessageLevel::Warning);
                Redraw::Needed
            }
        }
    }

    /// Releases the waiting question and completes a save that waited for it.
    ///
    /// A save must never depend on a language server, so a lost formatter
    /// answer still writes the buffer content that the user typed.
    fn abandon_query(&mut self) -> Redraw {
        let Some(pending) = self.language.pending.take() else {
            return Redraw::Skipped;
        };
        self.release_query(&pending)
    }

    /// Releases one question without applying an answer.
    ///
    /// A released format question describes a buffer version that the user
    /// already left, or a server that reported its own state once. The save
    /// therefore reports its own result alone.
    fn release_query(&mut self, pending: &PendingQuery) -> Redraw {
        match pending.purpose {
            QueryPurpose::FormatBeforeSave(then) => self.start_save(then, FormatBeforeSave::Silent),
            QueryPurpose::Definition | QueryPurpose::Hover => Redraw::Skipped,
        }
    }

    /// Records one answer of one server for the question that the editor asked.
    ///
    /// The merge runs as soon as every server that took the question answered.
    /// An answer of an obsolete buffer version releases the complete question,
    /// because the buffer that every server described no longer exists.
    fn answer_query(
        &mut self,
        request: LanguageRequestId,
        version: Option<BufferVersion>,
        answer: Answer,
    ) -> Redraw {
        let Some(mut pending) = self.language.pending.take() else {
            return Redraw::Skipped;
        };
        if !pending.owns(request) {
            // The answer belongs to a question that the editor already
            // released, so it changes nothing.
            self.language.pending = Some(pending);
            return Redraw::Skipped;
        }
        if version.is_some_and(|version| !self.answers_current_buffer(&pending, version)) {
            return self.release_query(&pending);
        }
        let state = pending.resolve(request, answer);
        self.settle_query(pending, state)
    }

    /// Releases every answer that one server still owes.
    ///
    /// A missing server, a stopped session, and a session failure carry no
    /// request identity. The question therefore continues with the servers that
    /// still run, and it never waits for a server that no longer answers.
    fn abandon_server(&mut self, server: LanguageServerId) -> Redraw {
        let Some(mut pending) = self.language.pending.take() else {
            return Redraw::Skipped;
        };
        let state = pending.abandon(server);
        self.settle_query(pending, state)
    }

    /// Applies one question whose servers all answered, or keeps it waiting.
    fn settle_query(&mut self, pending: PendingQuery, state: QueryState) -> Redraw {
        match state {
            QueryState::Waiting => {
                self.language.pending = Some(pending);
                Redraw::Skipped
            }
            QueryState::Complete => self.complete_query(&pending),
        }
    }

    /// Applies the merged answer of one completed question.
    fn complete_query(&mut self, pending: &PendingQuery) -> Redraw {
        match pending.purpose {
            QueryPurpose::Definition => self.follow_definition(pending),
            QueryPurpose::Hover => self.show_hover(pending),
            QueryPurpose::FormatBeforeSave(then) => {
                let (redraw, format) = match pending.formatting() {
                    Some(edits) => (
                        self.apply_format_edits(pending.buffer, edits),
                        FormatBeforeSave::Silent,
                    ),
                    // A save must never depend on a language server, so a lost
                    // formatter answer still writes the buffer content. The
                    // save report names that the file holds that content.
                    None => (Redraw::Skipped, FormatBeforeSave::Failed),
                };
                self.start_save(then, format).or(redraw)
            }
        }
    }

    /// Asks one question about the symbol under the cursor.
    fn ask_at_cursor(&mut self, purpose: QueryPurpose) -> Redraw {
        if self.language.pending.is_some() {
            self.set_message(
                "one language question is already running",
                MessageLevel::Warning,
            );
            return Redraw::Needed;
        }
        let buffer = self.active;
        let Some(file) = self.buffers.get(buffer) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return Redraw::Skipped;
        };
        let Some(path) = file.path().map(Path::to_path_buf) else {
            return self.report_language_notice(LanguageNotice::NoServer);
        };
        let text = file.text();
        let version = text.version();
        let position = document_position(text, self.cursor().position(text));
        let query = match purpose {
            QueryPurpose::Definition => LanguageQuery::Definition(position),
            QueryPurpose::Hover => LanguageQuery::Hover(position),
            QueryPurpose::FormatBeforeSave(_) => {
                debug_assert!(false, "a format is not a question about one position");
                return Redraw::Skipped;
            }
        };
        self.language.pending = Some(PendingQuery::new(buffer, version, purpose));
        self.queue_language(LanguageRequest::Query {
            buffer,
            path,
            version,
            query,
        });
        Redraw::Skipped
    }

    /// Publishes one diagnostic set behind the buffer-version gate.
    ///
    /// Diagnostics are decoration. They change no buffer text, no line mapping,
    /// and no cursor position, and an obsolete set changes nothing at all. The
    /// set replaces the previous set of its own server alone, so a second
    /// server of the same language keeps the diagnostics that it reported.
    fn publish_diagnostics(&mut self, server: LanguageServerId, set: DiagnosticSet) -> Redraw {
        let Some(buffer) = self.buffers.find_path(set.path()) else {
            // The server may describe a file that no buffer holds.
            return Redraw::Skipped;
        };
        let Some(file) = self.buffers.get(buffer) else {
            debug_assert!(false, "the path lookup returns a loaded buffer");
            return Redraw::Skipped;
        };
        if !set.is_current(file.text().version()) {
            return Redraw::Skipped;
        }
        self.language.publish(buffer, server, set);
        Redraw::Needed
    }

    /// Moves the cursor to one definition target, in this buffer or another.
    ///
    /// The merge takes the first non-empty answer in declaration order, so a
    /// second server of the same language answers only while the first one
    /// finds nothing. See `docs/language-services.md`.
    fn follow_definition(&mut self, pending: &PendingQuery) -> Redraw {
        let Some(location) = pending.definition().first() else {
            self.set_message("no definition found", MessageLevel::Warning);
            return Redraw::Needed;
        };
        // The answer names a target, so the cursor leaves the call site. The
        // record runs before both branches move it, so one backward step
        // returns to that call site in this file and in another file alike.
        self.record_jump();
        if self.buffers.get(pending.buffer).and_then(FileBuffer::path) == Some(&location.path) {
            return self.move_to_position(location.span.start);
        }
        self.open_at(
            location.path.clone(),
            PendingPosition::Document(location.span.start),
        )
    }

    /// Opens one path in the focused window and places the cursor at one
    /// position.
    ///
    /// A definition jump, an accepted picker row, and a step of the jump list
    /// all need this step, so all of them use one path. A buffer that the editor
    /// already holds moves the cursor at once. Every other path needs one file
    /// read, and the recorded jump waits for the completed load.
    fn open_at(&mut self, path: PathBuf, position: PendingPosition) -> Redraw {
        self.language.jump = Some(PendingJump {
            path: path.clone(),
            position,
        });
        let redraw = self.open_path(path);
        self.follow_jump().or(redraw)
    }

    /// Shows the merged hover answer as a float.
    ///
    /// The merge joins the non-empty answers in declaration order, and one
    /// blank row separates the answers of two servers. The float joins the
    /// document of each markdown answer, and one answer of plain text keeps
    /// the whole join as text. See `docs/language-services.md`.
    fn show_hover(&mut self, pending: &PendingQuery) -> Redraw {
        let answers = pending.hover();
        if answers.is_empty() {
            self.set_message("no hover information", MessageLevel::Info);
            return Redraw::Needed;
        }
        self.float = Some(Float::hover(HOVER_TITLE, &answers));
        Redraw::Needed
    }

    /// Applies the accepted formatter edits as one undoable transaction.
    ///
    /// An obsolete answer, a malformed range, and a buffer that already matches
    /// the formatter all leave the buffer as it is. The save follows either way.
    fn apply_format_edits(&mut self, buffer: BufferId, edits: &FormatEdits) -> Redraw {
        if self.access == EditorAccess::ViewOnly {
            return self.refuse(Refusal::ViewOnly);
        }
        if buffer != self.active {
            return Redraw::Skipped;
        }
        let Some(file) = self.buffers.get(buffer) else {
            return Redraw::Skipped;
        };
        let cursor = self.cursor().position(file.text());
        let transaction = match edits.transaction(file.text(), cursor) {
            Ok(Some(transaction)) => transaction,
            // The buffer already matches the formatter.
            Ok(None) => return Redraw::Skipped,
            Err(error) => {
                self.set_message(
                    format!("the formatting answer was discarded: {error}"),
                    MessageLevel::Warning,
                );
                return Redraw::Needed;
            }
        };
        let outcome = self.edit(|editing, context, window| {
            editing.apply_transaction(context, window, transaction)
        });
        self.sync_context();
        self.report(outcome)
    }

    /// Applies one formatted document as one undoable transaction.
    ///
    /// An obsolete answer leaves the buffer as it is, because the user typed
    /// while the formatter ran and the save writes what the user typed.
    fn commit_format(&mut self, buffer: BufferId, document: &FormattedDocument) -> Redraw {
        if self.access == EditorAccess::ViewOnly {
            return self.refuse(Refusal::ViewOnly);
        }
        if buffer != self.active {
            return Redraw::Skipped;
        }
        let Some(file) = self.buffers.get(buffer) else {
            return Redraw::Skipped;
        };
        let cursor = self.cursor().position(file.text());
        let transaction = match document.transaction(file.text(), cursor) {
            Ok(transaction) => transaction,
            // The user typed while the formatter ran, so its answer describes
            // content that the buffer no longer holds.
            Err(FormatterFailure::Obsolete) => return Redraw::Skipped,
            Err(FormatterFailure::NotInstalled | FormatterFailure::Unavailable) => {
                debug_assert!(
                    false,
                    "a transaction of one document fails only for an obsolete buffer version"
                );
                return Redraw::Skipped;
            }
        };
        let outcome = self.edit(|editing, context, window| {
            editing.apply_transaction(context, window, transaction)
        });
        self.sync_context();
        self.report(outcome)
    }

    /// Reports whether one answer still describes the current buffer version.
    fn answers_current_buffer(&self, pending: &PendingQuery, version: BufferVersion) -> bool {
        if pending.version != version {
            return false;
        }
        self.buffers
            .get(pending.buffer)
            .is_some_and(|file| file.text().version() == version)
    }

    /// Moves the cursor to the recorded jump target of the active buffer.
    fn follow_jump(&mut self) -> Redraw {
        let Some(jump) = self.language.jump.take() else {
            return Redraw::Skipped;
        };
        if self.buffers.get(self.active).and_then(FileBuffer::path) != Some(&jump.path) {
            // The buffer is still loading, so the jump waits for it.
            self.language.jump = Some(jump);
            return Redraw::Skipped;
        }
        match jump.position {
            PendingPosition::Document(position) => self.move_to_position(position),
            PendingPosition::Recorded { line, column } => self.move_to_recorded(line, column),
        }
    }

    /// Places the cursor at one protocol position of the active buffer.
    fn move_to_position(&mut self, position: DocumentPosition) -> Redraw {
        let Some(active) = self.buffers.get(self.active) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return Redraw::Skipped;
        };
        let text = active.text();
        let Ok(target) = buffer_position(position, text) else {
            self.set_message(OUTSIDE_BUFFER_NOTE, MessageLevel::Warning);
            return Redraw::Needed;
        };
        let line = text.char_to_line(target).get();
        let column = text.char_to_column(target).get();
        self.place_cursor(line, column);
        self.reconcile_viewports();
        Redraw::Needed
    }

    /// Places the cursor at one recorded line and column of the active buffer.
    ///
    /// The position comes from an earlier moment of the same text, so an edit
    /// may have removed the line that it names. [`Session::place_cursor`] clamps
    /// through the editor, so a line past the end of the buffer lands on the
    /// last line and the column lands inside that line. The editor therefore
    /// adjusts no recorded position while the user types, and the edit path
    /// stays free of that work. See `docs/windows.md`.
    fn move_to_recorded(&mut self, line: usize, column: usize) -> Redraw {
        self.place_cursor(line, column);
        self.reconcile_viewports();
        Redraw::Needed
    }

    /// Returns the entry that names the position of the cursor now.
    ///
    /// The entry carries the display path beside the buffer identity, so a step
    /// back into a buffer that the editor has dropped reopens its file.
    fn jump_entry(&self) -> JumpEntry {
        let cursor = self.cursor();
        let path = self
            .buffers
            .get(self.active)
            .and_then(FileBuffer::path)
            .map(Path::to_path_buf);
        JumpEntry::new(
            self.active,
            path,
            cursor.line().get(),
            cursor.column().get(),
        )
    }

    /// Records the position that the cursor holds now in the focused window.
    ///
    /// Every jump records its starting position *before* it moves the cursor. A
    /// record after the move would store the destination, and the backward step
    /// would then move nothing. The list belongs to the window, so a focus move
    /// never carries a recorded position into another window. See
    /// `docs/windows.md`.
    pub(super) fn record_jump(&mut self) {
        let entry = self.jump_entry();
        let window = self.windows.focused_window();
        let Some(jumps) = self.windows.jumps_mut(window) else {
            debug_assert!(false, "the focused window is always a leaf of the tree");
            return;
        };
        jumps.push(entry);
    }

    /// Walks one entry through the jump list of the focused window.
    ///
    /// A backward step from the newest position records the current position
    /// first, so the matching forward step returns to it. Both ends of the list
    /// report themselves instead of moving the cursor.
    fn jump(&mut self, direction: JumpDirection) -> Redraw {
        let current = self.jump_entry();
        let window = self.windows.focused_window();
        let Some(jumps) = self.windows.jumps_mut(window) else {
            debug_assert!(false, "the focused window is always a leaf of the tree");
            return Redraw::Skipped;
        };
        let step = jumps.step(direction, current);
        match step {
            JumpStep::Moved(entry) => self.follow_jump_entry(&entry),
            JumpStep::AtOldest => {
                self.set_message(OLDEST_JUMP_NOTE, MessageLevel::Info);
                Redraw::Needed
            }
            JumpStep::AtNewest => {
                self.set_message(NEWEST_JUMP_NOTE, MessageLevel::Info);
                Redraw::Needed
            }
        }
    }

    /// Moves the cursor to one recorded position of the jump list.
    ///
    /// A buffer identity never returns after its buffer is gone, so a recorded
    /// identity either names the same buffer or names none. A loaded buffer
    /// therefore moves the cursor at once, and the focused window shows it first
    /// when it holds other text. A buffer that the editor dropped reopens
    /// through its recorded path, and a dropped buffer without a file reports
    /// that the position is unreachable.
    fn follow_jump_entry(&mut self, entry: &JumpEntry) -> Redraw {
        let line = entry.line();
        let column = entry.column();
        if self.buffers.get(entry.buffer()).is_some() {
            let shown = if entry.buffer() == self.active {
                Redraw::Skipped
            } else {
                self.switch_to(entry.buffer())
            };
            return self.move_to_recorded(line, column).or(shown);
        }
        let Some(path) = entry.path() else {
            self.set_message(UNLOADED_JUMP_NOTE, MessageLevel::Warning);
            return Redraw::Needed;
        };
        self.open_at(
            path.to_path_buf(),
            PendingPosition::Recorded { line, column },
        )
        .or(Redraw::Needed)
    }

    /// Moves the cursor to the next or the previous diagnostic of the buffer.
    ///
    /// The diagnostics ascend by position, so the jump is deterministic. It
    /// wraps at both ends, and an empty set moves no cursor.
    fn jump_diagnostic(&mut self, jump: DiagnosticJump) -> Redraw {
        let Some(cursor) = self.cursor_position() else {
            return Redraw::Skipped;
        };
        let target = jump_target(self.language.diagnostics(self.active), cursor, jump);
        let Some(target) = target else {
            self.set_message(NO_DIAGNOSTIC_NOTE, MessageLevel::Info);
            return Redraw::Needed;
        };
        self.move_to_position(target)
    }

    /// Shows the diagnostics at the cursor position as a float.
    fn show_diagnostic_float(&mut self) -> Redraw {
        let Some(cursor) = self.cursor_position() else {
            return Redraw::Skipped;
        };
        let found: Vec<&Diagnostic> = self
            .language
            .diagnostics(self.active)
            .iter()
            .filter(|diagnostic| diagnostic.span.contains(cursor))
            .collect();
        if found.is_empty() {
            self.set_message(NO_DIAGNOSTIC_AT_CURSOR_NOTE, MessageLevel::Info);
            return Redraw::Needed;
        }
        // The float names the producer of each diagnostic only while this
        // buffer carries more than one producer name. The state reads the
        // complete buffer, so a name never appears or disappears as the cursor
        // moves between two positions.
        let naming = self.language.diagnostic_naming(self.active);
        self.float = Some(Float::diagnostics(DIAGNOSTIC_TITLE, &found, naming));
        Redraw::Needed
    }

    /// Returns the protocol position of the cursor in the active buffer.
    fn cursor_position(&self) -> Option<DocumentPosition> {
        let text = self.buffers.get(self.active)?.text();
        Some(document_position(text, self.cursor().position(text)))
    }

    /// Returns the format-on-save state of one buffer.
    fn format_on_save(&self, buffer: BufferId) -> FormatOnSave {
        let default = FormatOnSave::from_setting(self.settings.files.format_on_save);
        self.language.format_on_save(buffer, default)
    }

    /// Toggles format-on-save for the active buffer and reports the new state.
    ///
    /// The toggle is per buffer, so it changes no other buffer and no default.
    /// A buffer that no formatter can format keeps its state and reports the
    /// missing formatter, because a toggle there would change nothing that a
    /// save can perform.
    fn toggle_format_on_save(&mut self) -> Redraw {
        let buffer = self.active;
        let path = self.buffers.get(buffer).and_then(FileBuffer::path);
        if !has_formatter(self.languages, path) {
            self.set_message(NO_FORMATTER_NOTE, MessageLevel::Info);
            return Redraw::Needed;
        }
        let next = self.format_on_save(buffer).toggled();
        self.language.set_format_on_save(buffer, next);
        self.set_message(next.message(), MessageLevel::Info);
        Redraw::Needed
    }

    /// Closes the open float and reports whether one was open.
    fn close_float(&mut self) -> Redraw {
        if self.float.take().is_some() {
            Redraw::Needed
        } else {
            Redraw::Skipped
        }
    }

    /// Reports whether the file-tree sidebar owns the keys.
    fn sidebar_has_focus(&self) -> bool {
        self.tree_region
            .is_some_and(|id| self.windows.focused_region() == id)
    }

    /// Shows the file-tree sidebar and returns its region.
    ///
    /// The sidebar keeps one identity for the complete session, so hiding and
    /// showing it never builds a second region. Returns `None` when the window
    /// tree cannot issue a region identity.
    fn show_sidebar(&mut self) -> Option<WindowId> {
        match self.tree_region {
            Some(id) => {
                self.windows.set_sidebar_visible(SidebarSide::Right, true);
                Some(id)
            }
            None => {
                let id = self.windows.open_sidebar(
                    SidebarSide::Right,
                    self.settings.windows.file_tree_width_cells,
                )?;
                self.tree_region = Some(id);
                Some(id)
            }
        }
    }

    /// Opens the sidebar, expands the ancestors of the active file, and selects
    /// it.
    ///
    /// The sidebar then owns the keys, so the tree bindings act at once. A
    /// second `Ctrl-E` reaches the sidebar table, which closes the sidebar and
    /// returns the focus to the editor. See `docs/input-actions.md`.
    fn reveal_active_file(&mut self) -> Redraw {
        let path = self.active_buffer().path().map(Path::to_path_buf);
        let region = self.show_sidebar();
        match path {
            Some(path) => self.tree.reveal(&path),
            None => self.set_message(NO_REVEAL_PATH_NOTE, MessageLevel::Info),
        }
        if let Some(region) = region {
            self.windows.focus_region(region);
        }
        self.sync_context();
        Redraw::Needed
    }

    /// Applies one semantic command while the sidebar holds the keys.
    ///
    /// The navigation commands are the buffer commands, so the tree moves by
    /// the same rule. The sidebar bounds every move by its own rows.
    fn apply_tree_command(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
    ) -> Option<Redraw> {
        if !tree_owns(command) {
            return None;
        }
        Some(self.apply_owned_tree_command(command, count))
    }

    /// Applies one command that the sidebar owns.
    fn apply_owned_tree_command(&mut self, command: Command, count: Option<NonZeroU32>) -> Redraw {
        let repeat = count
            .map_or(1, |value| value.get() as usize)
            .min(MOTION_COUNT_MAX);
        match command {
            Command::MoveDown => self.tree.move_selection(TreeMotion::Down(repeat)),
            Command::MoveUp => self.tree.move_selection(TreeMotion::Up(repeat)),
            Command::MoveHalfPageDown => {
                let rows = self.tree_viewport().map_or(1, Viewport::half_page_rows);
                self.tree
                    .move_selection(TreeMotion::Down(rows.saturating_mul(repeat)));
            }
            Command::MoveHalfPageUp => {
                let rows = self.tree_viewport().map_or(1, Viewport::half_page_rows);
                self.tree
                    .move_selection(TreeMotion::Up(rows.saturating_mul(repeat)));
            }
            Command::MoveFullPageDown => {
                let rows = self.tree_viewport().map_or(1, Viewport::full_page_rows);
                self.tree
                    .move_selection(TreeMotion::Down(rows.saturating_mul(repeat)));
            }
            Command::MoveFullPageUp => {
                let rows = self.tree_viewport().map_or(1, Viewport::full_page_rows);
                self.tree
                    .move_selection(TreeMotion::Up(rows.saturating_mul(repeat)));
            }
            // A count before `gg` or `G` names one row, not a number of steps,
            // exactly as it names one line in a buffer window.
            Command::MoveFirstLine => {
                let row = count.map_or(0, |value| value.get() as usize - 1);
                self.tree.move_selection(TreeMotion::ToRow(row));
            }
            Command::MoveLastLine => {
                let motion = count.map_or(TreeMotion::LastRow, |value| {
                    TreeMotion::ToRow(value.get() as usize - 1)
                });
                self.tree.move_selection(motion);
            }
            Command::SearchNext => return self.select_tree_match(SearchDirection::Forward),
            Command::SearchPrevious => return self.select_tree_match(SearchDirection::Backward),
            // `Esc` and `Ctrl-C` cancel the sidebar work of the user, so
            // they end the search and release the held entry together.
            Command::EndSearch => {
                self.tree.end_search();
                self.tree.release_hold();
            }
            Command::TreeSelectParent => self.tree.select_parent(),
            Command::TreeToggleEntry => self.tree.toggle_selected(),
            Command::TreeCollapseEntry => self.tree.collapse_selected(),
            Command::TreeExpandEntry => return self.expand_selected_entry(),
            Command::TreeRefresh => self.tree.refresh_all(),
            Command::TreeToggleHidden => self.tree.toggle_hidden(),
            Command::TreeOpenEntry => return self.open_selected_entry(),
            Command::TreeSearch => return self.open_prompt(PromptKind::Tree(TreePrompt::Search)),
            Command::TreeAddFile => {
                return self.open_prompt(PromptKind::Tree(TreePrompt::AddFile));
            }
            Command::TreeAddDirectory => {
                return self.open_prompt(PromptKind::Tree(TreePrompt::AddDirectory));
            }
            Command::TreeRename => return self.open_prompt(PromptKind::Tree(TreePrompt::Rename)),
            Command::TreeCopyEntry => return self.hold_entry(TransferMode::Copy),
            Command::TreeCutEntry => return self.hold_entry(TransferMode::Move),
            Command::TreeDelete => return self.confirm_tree_delete(),
            Command::TreePasteEntries => {
                let staged = self.tree.stage_paste();
                return self.start_tree_mutation(staged);
            }
            Command::SaveBuffer => return self.save_active(AfterSave::Stay),
            Command::CloseWindow
            | Command::FocusWindowLeft
            | Command::FocusWindowDown
            | Command::FocusWindowUp
            | Command::FocusWindowRight
            | Command::ResizeWindowLeft
            | Command::ResizeWindowDown
            | Command::ResizeWindowUp
            | Command::ResizeWindowRight => return self.sidebar_window_command(command),
            // The sidebar table holds no other command.
            _ => return Redraw::Skipped,
        }
        Redraw::Needed
    }

    /// Applies one window command that the focused sidebar answers.
    ///
    /// The focus keys leave the sidebar and the close key hides it. The resize
    /// keys move the inner border of the sidebar and keep the focus on it, so
    /// the user widens the file tree without leaving it first.
    fn sidebar_window_command(&mut self, command: Command) -> Redraw {
        match self.windows.apply(command) {
            WindowOutcome::Changed => {
                self.follow_focused_window();
                self.sync_context();
                Redraw::Needed
            }
            WindowOutcome::LastWindow => {
                debug_assert!(false, "closing a focused sidebar keeps every editor window");
                Redraw::Skipped
            }
            WindowOutcome::Ignored | WindowOutcome::Unchanged => {
                if let Some(direction) = focus_direction(command) {
                    self.note_request(InputRequest::FocusBoundary(direction));
                }
                Redraw::Skipped
            }
        }
    }

    /// Opens the selected file, or expands the selected directory.
    ///
    /// A file opens in the editor window that held the focus before the
    /// sidebar, and the focus follows it, so the user types in the new buffer.
    fn open_selected_entry(&mut self) -> Redraw {
        let selected = self.tree.open_selected();
        self.open_tree_selection(selected)
    }

    /// Expands the selected directory, or opens the selected file.
    ///
    /// `l` reaches this entry point. An already expanded directory stays open.
    /// See `docs/input-actions.md`.
    fn expand_selected_entry(&mut self) -> Redraw {
        let selected = self.tree.expand_selected();
        self.open_tree_selection(selected)
    }

    /// Opens one file that the sidebar selected, and moves the focus to it.
    fn open_tree_selection(&mut self, selected: Option<PathBuf>) -> Redraw {
        let Some(path) = selected else {
            return Redraw::Needed;
        };
        // The sidebar holds the keys, but the file opens in the focused editor
        // window, so the record names that window and runs before the open
        // changes the buffer that it shows.
        self.record_jump();
        let window = self.windows.focused_window();
        self.windows.focus_region(window);
        self.sync_context();
        self.open_path(path).or(Redraw::Needed)
    }

    /// Holds the selected entry in the file-operation clipboard.
    fn hold_entry(&mut self, mode: TransferMode) -> Redraw {
        match self.tree.hold(mode) {
            Ok(path) => {
                let verb = match mode {
                    TransferMode::Copy => "copied",
                    TransferMode::Move => "cut",
                };
                self.set_message(
                    format!("{} is {verb} for the next paste", path.display()),
                    MessageLevel::Info,
                );
            }
            Err(refusal) => self.set_message(refusal.message(), MessageLevel::Warning),
        }
        Redraw::Needed
    }

    /// Asks the user before the editor removes the selected entries.
    ///
    /// The tree reads its selection first, so a refusal reaches the user with
    /// no question. Only a delete that would proceed asks. The answer stages
    /// the removal, so the entries reach the worker at answer time. See
    /// `docs/files.md`.
    fn confirm_tree_delete(&mut self) -> Redraw {
        let paths = match self.tree.delete_selection() {
            Ok(paths) => paths,
            Err(refusal) => {
                self.set_message(refusal.message(), MessageLevel::Warning);
                return Redraw::Needed;
            }
        };
        debug_assert!(
            !paths.is_empty(),
            "a selection that names no entry refuses instead"
        );
        let question = delete_question(&paths);
        self.ask_confirmation(question, ConfirmedAction::DeleteEntries { paths })
    }

    /// Queues one staged workspace mutation, or reports the refusal.
    ///
    /// The mutation destroys no entry that holds a destination. A taken
    /// destination returns as one refusal, which opens the question of the
    /// overwrite. See `docs/files.md`.
    fn start_tree_mutation(&mut self, staged: Result<FileOperation, TreeRefusal>) -> Redraw {
        let operation = match staged {
            Ok(operation) => operation,
            Err(refusal) => {
                self.set_message(refusal.message(), MessageLevel::Warning);
                return Redraw::Needed;
            }
        };
        self.queue_tree_mutation(operation, Overwrite::Refuse)
    }

    /// Queues the operation that one confirmed overwrite approved.
    ///
    /// The request names every destination that loses its entry, so the worker
    /// replaces exactly the entries that the question named.
    fn start_tree_overwrite(
        &mut self,
        operation: FileOperation,
        destinations: Vec<TakenDestination>,
    ) -> Redraw {
        debug_assert!(
            !destinations.is_empty(),
            "a question of an overwrite names at least one destination"
        );
        self.queue_tree_mutation(operation, Overwrite::Replace(destinations))
    }

    /// Hands one operation to the bounded worker service.
    fn queue_tree_mutation(&mut self, operation: FileOperation, overwrite: Overwrite) -> Redraw {
        if self.access == EditorAccess::ViewOnly {
            return self.refuse(Refusal::ViewOnly);
        }
        // The mutation owns the slot of its `WorkspaceChanged` fact before the
        // filesystem sees it. See `docs/embedding.md`.
        debug_assert!(
            self.mutation_slot.is_none(),
            "the editor runs one workspace mutation at a time"
        );
        let Ok(slot) = self.events.reserve() else {
            return self.refuse(Refusal::Saturated);
        };
        // The worker validates the operation against the loaded buffers, so it
        // receives the complete list with the request.
        let buffers = self.buffers.open_buffers(&self.root);
        if let Err(refusal) = self.tree.start_mutation(operation, overwrite, buffers) {
            self.events.release(slot);
            self.set_message(refusal.message(), MessageLevel::Warning);
            return Redraw::Needed;
        }
        self.mutation_slot = Some(slot);
        Redraw::Needed
    }

    /// Runs one accepted file-tree prompt line.
    fn run_tree_prompt(&mut self, prompt: TreePrompt, text: &str) -> Redraw {
        match prompt {
            TreePrompt::Search => {
                self.tree.start_search(text);
                Redraw::Needed
            }
            TreePrompt::AddFile => {
                let staged = self.tree.stage_create(text, EntryKind::File);
                self.start_tree_mutation(staged)
            }
            TreePrompt::AddDirectory => {
                let staged = self.tree.stage_create(text, EntryKind::Directory);
                self.start_tree_mutation(staged)
            }
            TreePrompt::Rename => {
                let staged = self.tree.stage_rename(text);
                self.start_tree_mutation(staged)
            }
        }
    }

    /// Takes the workspace request that the event loop must submit.
    ///
    /// The session never reads a directory and never changes a file itself, so
    /// the event loop hands the request to the bounded worker service. See
    /// `docs/responsiveness.md`.
    pub fn take_workspace_request(&mut self) -> Option<WorkspaceRequest> {
        self.tree.take_request()
    }

    /// Opens the review of the two halves of the worktree.
    ///
    /// The session captures both halves through the bounded process service, so
    /// the review opens at once and fills as the captures resolve. A review
    /// that a reader closed earlier keeps its read marks, and the captures
    /// reload into it. See `docs/diff-view.md`.
    fn open_review(&mut self) -> Redraw {
        self.review_open = true;
        if let Some(review) = self.review.as_mut() {
            review.set_height_rows(review_body_rows(self.area));
        }
        self.request_diff_captures();
        self.sync_context();
        Redraw::Needed
    }

    /// Leaves the review and gives the frame back to the window tree.
    ///
    /// The window tree, every viewport, and every buffer stay exactly as they
    /// were, because the review drew over them and changed none of them. The
    /// surface itself survives, so a later review keeps the read marks.
    fn close_review(&mut self) -> Redraw {
        self.review_open = false;
        self.sync_context();
        Redraw::Needed
    }

    /// Captures the worktree again while the review stays open.
    ///
    /// The captures carry the read marks and the selection forward, because a
    /// reload relocates both onto the later candidate. A hunk that the change
    /// did not touch therefore stays read.
    ///
    /// The call queues nothing while the review holds captures that have not
    /// resolved, so a burst of changes never grows the outbox.
    fn refresh_review(&mut self) -> Redraw {
        if !self.review_open || !self.diff_outbox.is_empty() {
            return Redraw::Skipped;
        }
        self.request_diff_captures();
        Redraw::Skipped
    }

    /// Queues the two captures that the review shows.
    fn request_diff_captures(&mut self) {
        let root = Arc::clone(&self.root);
        for (section, comparison) in [
            (ChangeSection::Staged, DiffComparison::HeadToIndex),
            (ChangeSection::Unstaged, DiffComparison::IndexToWorktree),
        ] {
            self.diff_outbox.push_back((
                section,
                WorktreeDiffRequest::new(Arc::clone(&root), comparison, DiffTarget::Worktree),
            ));
        }
    }

    /// Applies one review command to the open review.
    fn apply_review_command(&mut self, command: Command, count: Option<NonZeroU32>) -> Redraw {
        let Some(review) = self.review.as_mut() else {
            // The captures have not resolved yet, so the review holds nothing
            // to walk. Leaving it still works, because the session owns that.
            return if command == Command::CloseReview {
                self.close_review()
            } else {
                Redraw::Skipped
            };
        };
        match review.apply(command, count) {
            ReviewOutcome::Unchanged | ReviewOutcome::Unhandled => Redraw::Skipped,
            ReviewOutcome::Changed => Redraw::Needed,
            ReviewOutcome::Close => self.close_review(),
            ReviewOutcome::OpenFile { path, line } => {
                let closed = self.close_review();
                // The jump records itself, so `Ctrl-O` returns to where the
                // reader stood before the review opened the file.
                self.record_jump();
                // The file opens in the focused editor window. A reader who
                // opened the review from the file tree left the keys there, so
                // the jump moves them to the window that shows the file.
                let window = self.windows.focused_window();
                self.windows.focus_region(window);
                // The review names a one-based line and the document position
                // counts from zero.
                let position = DocumentPosition::new(line.saturating_sub(1), 0);
                let target = self.root.as_path().join(path.as_path());
                let opened = self.open_at(target, PendingPosition::Document(position));
                self.sync_context();
                opened.or(closed)
            }
        }
    }

    /// Takes the diff capture that the bounded process service must run.
    ///
    /// The session never runs `git` itself, so every capture leaves the session
    /// as a request and returns as one candidate. See `docs/git.md`.
    pub(super) fn take_diff_request(&mut self) -> Option<(ChangeSection, WorktreeDiffRequest)> {
        self.diff_outbox.pop_front()
    }

    /// Applies one finished diff capture as one state transition.
    ///
    /// One capture takes more than one command, so a step that needs a further
    /// command returns to the outbox instead of publishing. A refused capture
    /// reaches the message line once and leaves a usable editor.
    #[must_use]
    pub(super) fn apply_diff_result(
        &mut self,
        section: ChangeSection,
        result: Result<WorktreeDiffRead, WorktreeDiffFailure>,
    ) -> Redraw {
        let failure = match result {
            Ok(WorktreeDiffRead::Pending(request)) => {
                self.diff_outbox.push_back((section, *request));
                return Redraw::Skipped;
            }
            Ok(WorktreeDiffRead::Published(candidate)) => {
                match self.review.as_mut() {
                    Some(review) => review.reload(section, *candidate),
                    None => {
                        let (staged, unstaged) = match section {
                            ChangeSection::Staged => (Some(*candidate), None),
                            ChangeSection::Unstaged => (None, Some(*candidate)),
                        };
                        self.review = Some(ReviewSurface::new(
                            staged,
                            unstaged,
                            self.settings.diff,
                            review_body_rows(self.area),
                        ));
                    }
                }
                return Redraw::Needed;
            }
            Err(failure) => failure,
        };
        // A repository without a commit publishes no staged half, which is a
        // normal state and not a failure of the review.
        if failure != WorktreeDiffFailure::BaseUnavailable && !self.diff_reported {
            self.diff_reported = true;
            self.set_message(DIFF_UNAVAILABLE_NOTE, MessageLevel::Warning);
            return Redraw::Needed;
        }
        Redraw::Skipped
    }

    /// Takes the Git status read that the bounded process service must run.
    ///
    /// The session never runs `git` itself, so the command leaves the session
    /// as a request and returns as one snapshot. See `docs/git.md`.
    pub fn take_git_request(&mut self) -> Option<GitStatusRequest> {
        self.tree.take_git_request()
    }

    /// Applies one completed Git status read as one state transition.
    ///
    /// A refused submission and a failed command both reach this entry point as
    /// a typed failure. The file tree keeps every row and every key, and it
    /// keeps the marks of the last successful read, so no failure removes
    /// workspace state. A missing `git` command reaches the message line once
    /// for each session.
    ///
    /// One status read takes more than one command, so a step that needs a
    /// further command returns to the outbox instead of publishing.
    #[must_use]
    pub fn apply_git_result(&mut self, result: Result<GitStatusRead, GitStatusFailure>) -> Redraw {
        let failure = match result {
            Ok(GitStatusRead::Pending(request)) => {
                self.tree.resume_git_status(request);
                return Redraw::Skipped;
            }
            Ok(GitStatusRead::Published(snapshot)) => match self.tree.apply_git_status(snapshot) {
                GitPublication::Applied => return Redraw::Needed,
                GitPublication::Obsolete => return Redraw::Skipped,
            },
            Err(failure) => failure,
        };
        if failure == GitStatusFailure::CommandMissing && !self.git_reported {
            self.git_reported = true;
            self.set_message(GIT_MISSING_NOTE, MessageLevel::Warning);
            return Redraw::Needed;
        }
        Redraw::Skipped
    }

    /// Takes the formatter run that the bounded process service must run.
    ///
    /// The session never starts the program itself, so the run leaves the
    /// session as a request and returns as one formatted document. See
    /// `docs/language-services.md`.
    pub fn take_format_request(&mut self) -> Option<FormatterRequest> {
        self.language.take_format_request()
    }

    /// Applies one completed run of the external formatter of one buffer.
    ///
    /// A refused submission and a failed program both reach this entry point as
    /// a typed failure. The save that waited for the formatter follows every
    /// path, so no formatter state ever loses the content that the user typed.
    /// The save report names the failure, because the save writes the message
    /// line after this transition.
    #[must_use]
    pub fn apply_format_result(
        &mut self,
        result: Result<Option<FormattedDocument>, FormatterFailure>,
    ) -> Redraw {
        let Some(pending) = self.language.take_format() else {
            // No save waits for this answer, so it changes nothing.
            return Redraw::Skipped;
        };
        let (formatted, format) = match result {
            Ok(Some(document)) => (
                self.commit_format(pending.buffer, &document),
                FormatBeforeSave::Silent,
            ),
            // The buffer already matches its formatter.
            Ok(None) => (Redraw::Skipped, FormatBeforeSave::Silent),
            Err(failure) => {
                // The save report names every other failure, so the log holds
                // the one answer that reaches no message line.
                if matches!(failure, FormatterFailure::Obsolete) {
                    self.record_job(JOB_FORMATTER, MessageLevel::Info, JOB_OBSOLETE);
                }
                (Redraw::Skipped, FormatBeforeSave::of(failure))
            }
        };
        self.start_save(pending.then, format).or(formatted)
    }

    /// Applies one coalesced burst of workspace filesystem changes.
    ///
    /// The burst names the directories that changed, so the file tree reads
    /// only those and keeps its expansion, its selection, and its first visible
    /// row. The rows change when those reads return, so the burst itself paints
    /// nothing.
    ///
    /// The burst also starts one bounded check of every loaded buffer against
    /// its file, because a content change names no path and a removed file
    /// names only its directory. The buffers change when that check returns.
    /// See `docs/files.md`.
    ///
    /// The burst carries the part of the workspace that carries no watch, so
    /// this call also reports a registration that covers the workspace in part.
    #[must_use]
    pub fn apply_watch_batch(&mut self, batch: &WatchBatch) -> Redraw {
        if batch.root().is_some_and(|root| root != self.root.as_ref()) {
            return Redraw::Skipped;
        }
        self.tree.apply_watch(batch);
        self.reconcile_tree();
        self.start_watch_reload();
        // An open review shows the worktree, so a change of that worktree
        // captures it again. An agent that writes files therefore updates the
        // diff without a key. See `docs/diff-view.md`.
        self.refresh_review();
        self.report_watch_coverage(batch.coverage())
    }

    /// Reports that no watcher observes the workspace, once for each session.
    ///
    /// The editor stays fully usable: the refresh command reads the workspace
    /// again by hand. See `docs/files.md`.
    #[must_use]
    pub fn report_watch_unavailable(&mut self) -> Redraw {
        if self.watch_reported {
            return Redraw::Skipped;
        }
        self.watch_reported = true;
        self.set_message(WATCH_MISSING_NOTE, MessageLevel::Warning);
        Redraw::Needed
    }

    /// Reports a workspace that carries a watch in part, once for each session.
    ///
    /// A registration that covers the whole workspace reports nothing. Every
    /// other registration names its cause, because the watch limit of the host
    /// and the bound of the editor need two different actions. The editor stays
    /// fully usable in both cases, and the refresh command reads the workspace
    /// again by hand.
    ///
    /// The report shares the flag of the missing watcher, so one session shows
    /// one watch report and every later burst stays quiet. See
    /// `docs/files.md`.
    #[must_use]
    fn report_watch_coverage(&mut self, coverage: WatchCoverage) -> Redraw {
        if coverage.is_complete() || self.watch_reported {
            return Redraw::Skipped;
        }
        self.watch_reported = true;
        self.set_message(
            watch_coverage_note(coverage, watch_limit_setting()),
            MessageLevel::Warning,
        );
        Redraw::Needed
    }

    /// Applies one completed workspace operation as one state transition.
    #[must_use]
    pub fn apply_workspace_result(&mut self, result: WorkspaceResult) -> Redraw {
        let redraw = match result {
            WorkspaceResult::Directory { path, outcome } => {
                self.tree.apply_directory(&path, outcome);
                Redraw::Needed
            }
            WorkspaceResult::Mutated { outcome } => self.publish_mutation(outcome),
        };
        self.reconcile_tree();
        redraw
    }

    /// Reports that one workspace operation produced no result.
    ///
    /// The workspace and the buffers keep the state that they held before the
    /// request, so the user can repeat the operation.
    #[must_use]
    pub fn abandon_workspace_request(&mut self, failure: FileRequestFailure) -> Redraw {
        let slot = self.mutation_slot.take();
        self.release_slot(slot);
        self.tree.abandon_request();
        self.set_message(failure.message(), MessageLevel::Error);
        Redraw::Needed
    }

    /// Publishes one completed mutation as one visible state change.
    ///
    /// The buffer paths, the affected directories, and the new selection change
    /// together, so no window shows a path that the workspace no longer holds.
    fn publish_mutation(&mut self, outcome: Result<MutationOutcome, MutationError>) -> Redraw {
        let slot = self.mutation_slot.take();
        // The pending operation names what the workspace performed, so the
        // fact reads it before the tree releases its request.
        let operation = self.tree.pending_mutation();
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(MutationError::Collision { entries }) => {
                self.release_slot(slot);
                return self.report_collision(entries);
            }
            Err(error) => {
                self.release_slot(slot);
                self.tree.abandon_request();
                self.set_message(error.to_string(), MessageLevel::Error);
                return Redraw::Needed;
            }
        };
        match (slot, operation) {
            // The workspace changed, so its mandatory fact publishes through
            // the slot that the mutation reserved. See `docs/embedding.md`.
            (Some(slot), Some(operation)) => {
                self.events
                    .commit(slot, EditorEvent::WorkspaceChanged { operation });
            }
            (slot, _) => {
                debug_assert!(
                    slot.is_none(),
                    "a queued mutation keeps its operation until its result arrives"
                );
                self.release_slot(slot);
            }
        }
        // The language server holds each document by its path, so the previous
        // path closes before the buffer takes the new one.
        let closed: Vec<PathBuf> = outcome
            .updates
            .iter()
            .filter_map(|update| {
                self.buffers
                    .get(update.buffer)
                    .and_then(FileBuffer::path)
                    .map(Path::to_path_buf)
            })
            .collect();
        self.buffers.apply_path_updates(&outcome.updates);
        for path in closed {
            self.queue_language(LanguageRequest::Close { path });
        }
        for update in &outcome.updates {
            self.language.mark_resync(update.buffer);
        }
        self.tree.release_hold();
        self.tree.apply_mutation(&outcome);
        Redraw::Needed
    }

    /// Asks the user before one mutation replaces the entries of its
    /// destinations, or reports the collision.
    ///
    /// The staging refused every taken destination, so the workspace still
    /// holds every entry here. Only a rename and a transfer offer an overwrite.
    /// A create keeps the plain refusal, because it writes one new entry.
    ///
    /// The refusal arrives from the worker, so another question can wait
    /// already. The editor then reports the collision and destroys nothing.
    fn report_collision(&mut self, entries: Vec<TakenDestination>) -> Redraw {
        let operation = self.tree.pending_mutation();
        self.tree.abandon_request();
        let report = MutationError::Collision {
            entries: entries.clone(),
        }
        .to_string();
        let Some(operation) = operation.filter(replaces_an_entry) else {
            self.set_message(report, MessageLevel::Error);
            return Redraw::Needed;
        };
        let question = overwrite_question(&entries);
        let action = ConfirmedAction::Overwrite {
            operation,
            destinations: entries,
        };
        if self.open_confirmation(question, action) == ConfirmationRequest::Refused {
            self.set_message(report, MessageLevel::Error);
        }
        Redraw::Needed
    }

    /// Returns the visible region of the file-tree sidebar.
    ///
    /// The layout owns the rectangle of the sidebar, and the title row sits
    /// above the entries, so the region of the rows is one row shorter. The
    /// page moves read the half-page and the full-page rule from this value, so
    /// `Ctrl-D` covers the same fraction of the sidebar as it covers of a
    /// buffer window.
    ///
    /// A hidden sidebar, and a sidebar that holds the title alone, report no
    /// region.
    fn tree_viewport(&self) -> Option<Viewport> {
        let area = self
            .tree_region
            .and_then(|id| self.windows.layout().area(id))?;
        let rows = NonZeroU16::new(area.height.saturating_sub(TREE_TITLE_ROWS))?;
        let cells = NonZeroU16::new(area.width)?;
        Some(Viewport::new(rows, cells))
    }

    /// Moves the visible tree rows so the selected row keeps the scroll margin.
    ///
    /// The sidebar reads the same margin as a buffer window, so both regions
    /// keep the same number of rows around the reader.
    fn reconcile_tree(&mut self) {
        let viewport = self.tree_viewport();
        self.tree
            .reconcile(viewport, usize::from(self.settings.display.scrolloff_rows));
    }

    /// Applies one completed file operation as one state transition.
    #[must_use]
    pub fn apply_file_result(&mut self, result: FileResult) -> Redraw {
        let redraw = self.publish_file_result(result);
        if self.reload_due {
            self.start_watch_reload();
        }
        redraw
    }

    /// Publishes one completed file operation.
    fn publish_file_result(&mut self, result: FileResult) -> Redraw {
        let pending = self.file_pending.take();
        match result {
            FileResult::Opened { requested, outcome } => match outcome {
                Ok(file) => self.publish_open(file),
                Err(error) => {
                    let requested = self.root.as_path().join(requested.as_path());
                    self.set_message(
                        format!("cannot open {}: {error}", requested.display()),
                        MessageLevel::Error,
                    );
                    Redraw::Needed
                }
            },
            FileResult::Saved {
                buffer,
                requested,
                outcome,
            } => {
                let (then, format) = match pending {
                    Some(PendingFile::Save { then, format, .. }) => (then, format),
                    Some(PendingFile::Open | PendingFile::Reload { .. }) | None => {
                        (AfterSave::Stay, FormatBeforeSave::Silent)
                    }
                };
                self.publish_save(buffer, requested.as_path(), outcome, then, format)
            }
            FileResult::Reloaded { buffers } => {
                let Some(PendingFile::Reload { targets, origin }) = pending else {
                    // A newer request displaced this check, so its outcome
                    // describes buffer states that the editor already left.
                    return Redraw::Skipped;
                };
                self.publish_reload(buffers, &targets, origin)
            }
        }
    }

    /// Reports that one file request produced no result.
    ///
    /// The buffer keeps every unsaved change, so the user can repeat the
    /// operation.
    #[must_use]
    pub fn abandon_file_request(&mut self, failure: FileRequestFailure) -> Redraw {
        let pending = self.file_pending.take();
        let slot = self.write_slot.take();
        self.release_slot(slot);
        self.file_outbox = None;
        let background = matches!(
            pending,
            Some(PendingFile::Reload {
                origin: ReloadOrigin::Watch,
                ..
            })
        );
        if self.reload_due {
            self.start_watch_reload();
        }
        if background {
            // The user asked for no background check, so its failure reports
            // nothing. The next burst asks again.
            return Redraw::Skipped;
        }
        self.set_message(failure.message(), MessageLevel::Error);
        Redraw::Needed
    }

    /// Takes the clipboard command that the bounded process service must run.
    ///
    /// The event loop never runs a clipboard command itself, so the command
    /// leaves the session as a request and returns as one output. See
    /// `docs/responsiveness.md`.
    pub fn take_clipboard_request(&mut self) -> Option<ProcessRequest> {
        self.clipboard_activity.dispatch()
    }

    /// Applies the output of one clipboard command.
    ///
    /// A refused submission and a failed command both reach this entry point as
    /// a typed failure. A failed write keeps the unnamed register, and a failed
    /// read falls back to it, so no clipboard failure loses editor data. See
    /// `docs/clipboard.md`.
    #[must_use]
    pub fn apply_clipboard_result(
        &mut self,
        output: Result<ProcessOutput, ClipboardFailure>,
    ) -> Redraw {
        let Some(work) = self.clipboard_activity.finish() else {
            // A newer operation displaced this one, so its output is obsolete.
            return Redraw::Skipped;
        };
        match work {
            ClipboardWork::Copy(value) => {
                let notice = self.clipboard.finish_copy(&value, output);
                self.report_clipboard(notice)
            }
            ClipboardWork::Paste {
                command,
                count,
                register,
            } => {
                let read = self.clipboard.finish_read(output);
                self.publish_paste(command, count, register, read)
            }
        }
    }

    /// Mirrors a new unnamed-register value into the system clipboard.
    ///
    /// A yank, a delete, and a change all write the unnamed register, so the
    /// register revision alone reports every value that the system clipboard
    /// must receive. See `docs/clipboard.md`.
    fn reconcile_clipboard(&mut self) -> Redraw {
        if self.registers.revision() == self.clipboard_revision {
            return Redraw::Skipped;
        }
        let redraw = match self.registers.unnamed().cloned() {
            Some(value) => self.start_clipboard(ClipboardWork::Copy(value)),
            None => Redraw::Skipped,
        };
        // A displaced paste can write the register again with the value that it
        // read from the system clipboard, and that value needs no write back.
        self.clipboard_revision = self.registers.revision();
        redraw
    }

    /// Starts one clipboard operation and resolves the one it displaces.
    fn start_clipboard(&mut self, work: ClipboardWork) -> Redraw {
        let displaced = self.abandon_clipboard();
        let started = match work {
            ClipboardWork::Copy(value) => match self.clipboard.copy(&value) {
                ClipboardStep::Done(notice) => self.report_clipboard(notice),
                ClipboardStep::Waiting(request) => {
                    self.defer_clipboard(request, ClipboardWork::Copy(value))
                }
            },
            ClipboardWork::Paste {
                command,
                count,
                register,
            } => match self.clipboard.read() {
                ClipboardStep::Done(read) => self.publish_paste(command, count, register, read),
                ClipboardStep::Waiting(request) => self.defer_clipboard(
                    request,
                    ClipboardWork::Paste {
                        command,
                        count,
                        register,
                    },
                ),
            },
        };
        started.or(displaced)
    }

    /// Holds one operation until the bounded process service returns its output.
    fn defer_clipboard(&mut self, request: ProcessRequest, work: ClipboardWork) -> Redraw {
        self.clipboard_activity.queue(request, work);
        Redraw::Skipped
    }

    /// Resolves the pending clipboard operation from internal state alone.
    fn abandon_clipboard(&mut self) -> Redraw {
        match self.clipboard_activity.finish() {
            // The unnamed register still holds the value, so a dropped write
            // loses nothing.
            None | Some(ClipboardWork::Copy(_)) => Redraw::Skipped,
            Some(ClipboardWork::Paste {
                command,
                count,
                register,
            }) => self.publish_paste(command, count, register, ClipboardRead::Fallback(None)),
        }
    }

    /// Reports whether one resolved name selects a register beside the unnamed one.
    ///
    /// `None` and `"` both name the unnamed register, which the system
    /// clipboard mirrors. Every other name belongs to the editor alone, so a
    /// paste from it needs no clipboard read. See `docs/clipboard.md`.
    const fn names_another_register(register: Option<char>) -> bool {
        !matches!(register, None | Some('"'))
    }

    /// Applies one paste over the register value that the read resolved.
    ///
    /// A value from the system clipboard becomes the unnamed register first, so
    /// an external copy pastes exactly like a kvim yank.
    fn publish_paste(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
        register: Option<char>,
        read: ClipboardRead,
    ) -> Redraw {
        let notice = match read {
            ClipboardRead::Value(value) => {
                let ending = self.buffer().line_ending();
                self.registers.set_unnamed(register_value(value, ending));
                // The value came from the system clipboard, so it needs no
                // write back.
                self.clipboard_revision = self.registers.revision();
                None
            }
            // A failed read falls back to the internal register, so a paste
            // always works.
            ClipboardRead::Fallback(notice) => notice,
        };
        // The focus can move while the read runs, and a picker or the file tree
        // owns every key while it holds the focus.
        if self.picker.is_some() || self.sidebar_has_focus() {
            return self.report_clipboard(notice);
        }
        let applied = self.apply_editing_command(command, count, register);
        // A clipboard answer arrives outside `handle_event`, so no `settle`
        // transition follows it. Refresh positions before the next frame reads
        // them against text that a Visual paste can shorten.
        self.refresh_search(SearchRefresh::OnVersionChange);
        let reported = self.report_clipboard(notice);
        self.reconcile_viewports();
        applied.or(reported)
    }

    /// Reports one clipboard notice on the message line.
    ///
    /// Every notice describes a clipboard that did not receive or return the
    /// value. The editor register still holds it, so the level stays a warning.
    fn report_clipboard(&mut self, notice: Option<ClipboardNotice>) -> Redraw {
        let Some(notice) = notice else {
            return Redraw::Skipped;
        };
        self.set_message(notice.to_string(), MessageLevel::Warning);
        Redraw::Needed
    }

    /// Queues one file request while no other operation runs.
    ///
    /// The editor runs one file operation at a time, so no result can arrive
    /// for a buffer state that a newer operation already replaced.
    fn start_file_request(&mut self, request: FileRequest, pending: PendingFile) -> Redraw {
        if self.file_pending.is_some() {
            return self.refuse_running_file_operation();
        }
        self.queue_file_request(request, pending);
        Redraw::Needed
    }

    /// Reports that one file operation still runs.
    fn refuse_running_file_operation(&mut self) -> Redraw {
        self.set_message(
            "one file operation is already running",
            MessageLevel::Warning,
        );
        Redraw::Needed
    }

    /// Puts one file request in the outbox for the event loop.
    fn queue_file_request(&mut self, request: FileRequest, pending: PendingFile) {
        debug_assert!(
            self.file_pending.is_none(),
            "the editor runs one file operation at a time"
        );
        debug_assert!(
            self.file_outbox.is_none(),
            "the event loop takes the queued request before the next command runs"
        );
        self.file_outbox = Some(request);
        self.file_pending = Some(pending);
    }

    /// Reads the file of the focused window again.
    ///
    /// `:e` reaches this entry point with [`UnsavedChanges::Ask`], and `:e!`
    /// with [`UnsavedChanges::Discard`]. A buffer that holds unsaved changes
    /// asks first, because the buffer text is then the only copy of that work.
    /// See `docs/files.md`.
    fn reload_active(&mut self, unsaved: UnsavedChanges) -> Redraw {
        let buffer = self.active;
        let Some(file) = self.buffers.get(buffer) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return Redraw::Skipped;
        };
        if file.path().is_none() {
            self.set_message(NO_FILE_NAME_NOTE, MessageLevel::Error);
            return Redraw::Needed;
        }
        if unsaved == UnsavedChanges::Discard {
            return self.reload_buffer(buffer, UnsavedText::Discard);
        }
        if !file.is_modified() {
            return self.reload_buffer(buffer, UnsavedText::Keep);
        }
        // The question names the buffer, so the user reads which file the
        // reload replaces before the answer.
        let question = format!("Reload {} and discard the unsaved changes", file.name());
        self.ask_confirmation(question, ConfirmedAction::DiscardOnReload { buffer })
    }

    /// Reads the file of one named buffer again.
    ///
    /// The caller names the buffer, so a confirmed reload reads the file of the
    /// buffer that its question named. An open that completes while the
    /// question waits makes another buffer active, and that buffer keeps its
    /// own text. See `docs/files.md`.
    fn reload_buffer(&mut self, buffer: BufferId, unsaved: UnsavedText) -> Redraw {
        let Some(file) = self.buffers.get(buffer) else {
            // Only an unload removes a buffer, and no key reaches the editor
            // while a question waits, so the buffer is always loaded here.
            debug_assert!(
                false,
                "the buffer list holds every buffer that a question named"
            );
            return Redraw::Skipped;
        };
        let Some(target) = file.target().cloned() else {
            self.set_message(NO_FILE_NAME_NOTE, MessageLevel::Error);
            return Redraw::Needed;
        };
        let version = file.text().version();
        self.start_file_request(
            FileRequest::Reload(ReloadRequest {
                targets: vec![ReloadTarget {
                    buffer,
                    target: target.clone(),
                    trigger: ReloadTrigger::Read,
                }],
                files: self.settings.files,
            }),
            PendingFile::Reload {
                targets: vec![PendingReload {
                    buffer,
                    target,
                    version,
                    unsaved,
                }],
                origin: ReloadOrigin::Command,
            },
        )
    }

    /// Checks every loaded buffer against its file after a workspace change.
    ///
    /// The burst names no buffer: a content change carries no path, and a lost
    /// event leaves the named directories incomplete. The check therefore
    /// covers every loaded buffer, which the buffer list bounds. A buffer that
    /// holds unsaved changes is compared and never read, so no reload can
    /// replace work that no file holds.
    fn start_watch_reload(&mut self) {
        if self.file_pending.is_some() {
            // The editor runs one file operation at a time, so the check
            // follows the operation that already runs.
            self.reload_due = true;
            return;
        }
        self.reload_due = false;
        let ids = self.buffers.ids();
        debug_assert!(
            ids.len() <= RELOAD_TARGETS_MAX,
            "the buffer list bounds itself at BUFFERS_MAX entries"
        );
        let mut targets = Vec::new();
        let mut pending = Vec::new();
        for id in ids {
            let Some(file) = self.buffers.get(id) else {
                debug_assert!(false, "the list answers for every identity that it named");
                continue;
            };
            let Some(target) = file.target().cloned() else {
                // A buffer without a file name has no file to compare.
                continue;
            };
            let identity = file.identity();
            targets.push(ReloadTarget {
                buffer: id,
                target: target.clone(),
                trigger: if file.is_modified() {
                    ReloadTrigger::Compare(identity)
                } else {
                    ReloadTrigger::Refresh(identity)
                },
            });
            pending.push(PendingReload {
                buffer: id,
                target,
                version: file.text().version(),
                unsaved: UnsavedText::Keep,
            });
        }
        if targets.is_empty() {
            return;
        }
        self.queue_file_request(
            FileRequest::Reload(ReloadRequest {
                targets,
                files: self.settings.files,
            }),
            PendingFile::Reload {
                targets: pending,
                origin: ReloadOrigin::Watch,
            },
        );
    }

    /// Saves the active buffer, and formats it first when the buffer asks.
    ///
    /// A formatter failure, a timeout, and an obsolete answer all still save
    /// the buffer, so the user never loses work to a language server. See
    /// `docs/language-services.md`.
    fn save_active(&mut self, then: AfterSave) -> Redraw {
        if self.access == EditorAccess::ViewOnly {
            return self.refuse(Refusal::ViewOnly);
        }
        // A save can format first, so the capacity check runs here, before the
        // operation starts. The write itself reserves the slot that publishes
        // its fact. See `docs/embedding.md`.
        if !self.events.has_free_slot() {
            return self.refuse(Refusal::Saturated);
        }
        if self.awaits_format() {
            self.set_message("one save is already running", MessageLevel::Warning);
            return Redraw::Needed;
        }
        if self.formats_before_save() {
            return self.request_format(then);
        }
        self.start_save(then, FormatBeforeSave::Silent)
    }

    /// Reports whether a save already waits for its formatter answer.
    fn awaits_format(&self) -> bool {
        self.language.formats()
            || matches!(
                self.language.pending,
                Some(PendingQuery {
                    purpose: QueryPurpose::FormatBeforeSave(_),
                    ..
                })
            )
    }

    /// Reports whether the active buffer formats before its next save.
    ///
    /// A buffer that no formatter serves, and a buffer whose question would
    /// replace another running question, saves without a format instead. The
    /// formatter test covers a buffer without a file name and a path that no
    /// adapter owns, so neither one starts a question that no formatter can
    /// answer. The per-buffer state stays unchanged in every case. See
    /// `docs/language-services.md`.
    fn formats_before_save(&self) -> bool {
        if self.language.pending.is_some() || self.language.formats() {
            return false;
        }
        let path = self.buffers.get(self.active).and_then(FileBuffer::path);
        if !has_formatter(self.languages, path) {
            return false;
        }
        self.format_on_save(self.active) == FormatOnSave::Enabled
    }

    /// Formats the active buffer before its save.
    ///
    /// An external formatter takes precedence over a formatting server, so the
    /// adapter of the path decides which path runs. See
    /// `docs/language-services.md`.
    fn request_format(&mut self, then: AfterSave) -> Redraw {
        if self.access == EditorAccess::ViewOnly {
            return self.refuse(Refusal::ViewOnly);
        }
        let buffer = self.active;
        let Some(file) = self.buffers.get(buffer) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return Redraw::Skipped;
        };
        let Some(path) = file.path().map(Path::to_path_buf) else {
            debug_assert!(
                false,
                "the caller checked that the buffer holds a file name"
            );
            return self.start_save(then, FormatBeforeSave::Silent);
        };
        let version = file.text().version();
        if let Some(LanguageFormatter::External(declaration)) =
            formatter(self.languages, Some(&path))
        {
            // The run carries the exact text of this version, because the
            // answer replaces that text.
            let content = file.text().to_string();
            let request = FormatterRequest::new(declaration, path, version, content);
            self.language.start_format(buffer, then, request);
            return Redraw::Needed;
        }
        self.language.pending = Some(PendingQuery::new(
            buffer,
            version,
            QueryPurpose::FormatBeforeSave(then),
        ));
        self.queue_language(LanguageRequest::Query {
            buffer,
            path,
            version,
            query: LanguageQuery::Format,
        });
        Redraw::Needed
    }

    /// Writes the active buffer and runs the step that follows the save.
    ///
    /// `format` names what a format before this save produced, and the save
    /// report names that state beside its own result.
    fn start_save(&mut self, then: AfterSave, format: FormatBeforeSave) -> Redraw {
        if self.access == EditorAccess::ViewOnly {
            return self.refuse(Refusal::ViewOnly);
        }
        let buffer = self.active;
        // Build the complete request before the operation starts, so a rejected
        // save never changes the buffer.
        let staged = self.buffers.get(buffer).and_then(|active| {
            let target = active.target()?.clone();
            Some(SaveRequest {
                buffer,
                content: render_content(active.text()),
                version: active.text().version(),
                expected: active.identity(),
                snapshot: active.text().clone(),
                files: self.settings.files,
                target,
            })
        });
        let Some(request) = staged else {
            self.set_message(NO_FILE_NAME_NOTE, MessageLevel::Error);
            return Redraw::Needed;
        };
        // The write owns the slot of its `FileWritten` fact before the write
        // starts, so a completed write can never lose that fact. A saturated
        // outbox refuses the save before the filesystem sees it. See
        // `docs/embedding.md`.
        debug_assert!(
            self.write_slot.is_none(),
            "the editor runs one file operation at a time"
        );
        if self.file_pending.is_some() {
            return self.refuse_running_file_operation();
        }
        let Ok(slot) = self.events.reserve() else {
            return self.refuse(Refusal::Saturated);
        };
        let redraw = self.start_file_request(
            FileRequest::Save(request),
            PendingFile::Save {
                buffer,
                then,
                format,
            },
        );
        self.write_slot = Some(slot);
        redraw
    }

    /// Publishes one loaded buffer.
    fn publish_open(&mut self, file: OpenedFile) -> Redraw {
        // Two spellings of one path reach the same file, so the completed load
        // returns the buffer that already owns it.
        if let Some(existing) = self.buffers.find_target(&file.target) {
            return self.switch_to(existing).or(Redraw::Needed);
        }
        let name = file.target.as_path().display().to_string();
        let lines = file.text.line_count();
        let bytes = file.text.len_bytes();
        let loaded = FileBuffer::loaded(file.text, file.target, file.identity);
        let Some(id) = self.buffers.insert(loaded) else {
            self.set_message(
                format!("the editor holds the maximum of {BUFFERS_MAX} buffers"),
                MessageLevel::Error,
            );
            return Redraw::Needed;
        };
        let redraw = self.switch_to(id);
        // The language services learn about the buffer through one open that
        // carries its exact text and version.
        self.language.mark_resync(id);
        self.set_message(format!("\"{name}\" {lines}L, {bytes}B"), MessageLevel::Info);
        self.follow_jump().or(redraw).or(Redraw::Needed)
    }

    /// Publishes one completed reload check.
    ///
    /// Each outcome passes the publication gate of its own buffer, so a buffer
    /// that moved or changed while the check ran keeps its text. See
    /// `docs/files.md`.
    fn publish_reload(
        &mut self,
        buffers: Vec<ReloadedBuffer>,
        targets: &[PendingReload],
        origin: ReloadOrigin,
    ) -> Redraw {
        let mut redraw = Redraw::Skipped;
        for result in buffers {
            let Some(target) = targets.iter().find(|target| target.buffer == result.buffer) else {
                debug_assert!(false, "the worker answers for the targets that it received");
                continue;
            };
            if !self.accepts_reload(target, &result) {
                continue;
            }
            redraw = redraw.or(match result.outcome {
                // The buffer already holds what the file holds.
                Ok(ReloadOutcome::Unchanged) => Redraw::Skipped,
                Ok(ReloadOutcome::Loaded(file)) => self.publish_reloaded_text(target, file, origin),
                Ok(ReloadOutcome::Conflict) => {
                    self.report_external(result.buffer, ExternalChange::Changed, origin)
                }
                Ok(ReloadOutcome::Missing) => {
                    self.report_external(result.buffer, ExternalChange::Missing, origin)
                }
                Err(error) => {
                    self.report_reload_error(result.buffer, result.target.as_path(), &error, origin)
                }
            });
        }
        redraw
    }

    /// Reports whether one reload outcome still describes its buffer.
    ///
    /// A buffer that moved or changed while the check ran no longer describes
    /// the target and text that the worker checked.
    fn accepts_reload(&self, target: &PendingReload, result: &ReloadedBuffer) -> bool {
        if result.target != target.target {
            return false;
        }
        self.buffers.get(target.buffer).is_some_and(|file| {
            file.target() == Some(&target.target) && file.text().version() == target.version
        })
    }

    /// Replaces one buffer with the text that its file holds now.
    ///
    /// Every window that shows the buffer keeps its own cursor and its own
    /// first visible row, and both clamp to the new text, so a file that became
    /// shorter leaves no cursor beyond its end.
    fn publish_reloaded_text(
        &mut self,
        target: &PendingReload,
        file: OpenedFile,
        origin: ReloadOrigin,
    ) -> Redraw {
        let buffer = target.buffer;
        let Some(loaded) = self.buffers.get_mut(buffer) else {
            debug_assert!(false, "the publication gate found the buffer");
            return Redraw::Skipped;
        };
        if target.unsaved == UnsavedText::Keep && loaded.is_modified() {
            // The safety rule of this path: only the user decides to lose work,
            // and only `:e!` carries that decision. A modified buffer receives
            // the comparing trigger, which reads no text at all, and a buffer
            // that changed while the check ran fails the version gate above.
            debug_assert!(false, "a comparing target never carries reloaded text");
            return Redraw::Skipped;
        }
        debug_assert!(
            loaded.target() == Some(&file.target),
            "a reload reads the path that its own buffer holds"
        );
        let lines = file.text.line_count();
        let bytes = file.text.len_bytes();
        loaded.reload(file.text, file.identity);
        let name = loaded.name().to_owned();
        self.clamp_windows_of(buffer);
        // The reloaded buffer counts its versions from the start, so every
        // value that a buffer version guards must restart with it.
        self.language.mark_resync(buffer);
        self.language.forget_diagnostics(buffer);
        if let Some(entry) = self.analysis.get_mut(&buffer) {
            *entry = BufferAnalysis::default();
        }
        if self.analysis_pending.is_some_and(|(id, _)| id == buffer) {
            self.analysis_pending = None;
        }
        if buffer == self.active {
            self.refresh_search(SearchRefresh::Always);
        }
        self.reconcile_viewports();
        if origin == ReloadOrigin::Command {
            self.set_message(
                format!("\"{name}\" {lines}L, {bytes}B reloaded"),
                MessageLevel::Info,
            );
        }
        Redraw::Needed
    }

    /// Clamps the cursor of every window that shows one buffer.
    fn clamp_windows_of(&mut self, buffer: BufferId) {
        let Some(file) = self.buffers.get(buffer) else {
            debug_assert!(false, "the caller names a loaded buffer");
            return;
        };
        let text = file.text();
        for window in self.windows.window_ids() {
            if self.windows.buffer(window) != Some(buffer) {
                continue;
            }
            let Some(state) = self.windows.state_mut(window) else {
                debug_assert!(false, "the identity list names leaves of the window tree");
                continue;
            };
            let cursor = state.cursor();
            self.editing
                .move_to(text, state, cursor.line().get(), cursor.column().get());
        }
    }

    /// Records what another program did to the file of one buffer.
    ///
    /// The buffer keeps its text and stays editable, because that text is the
    /// only copy that kvim can still write. The answer names whether the editor
    /// must report the state: a background check reports one state once, so a
    /// workspace that changes often never fills the message line.
    fn mark_external(
        &mut self,
        buffer: BufferId,
        change: ExternalChange,
        origin: ReloadOrigin,
    ) -> Redraw {
        let Some(file) = self.buffers.get_mut(buffer) else {
            debug_assert!(false, "the publication gate found the buffer");
            return Redraw::Skipped;
        };
        let known = file.external_change() == Some(change);
        file.mark_external_change(change);
        if known && origin == ReloadOrigin::Watch {
            return Redraw::Skipped;
        }
        Redraw::Needed
    }

    /// Reports one external change that the editor cannot follow.
    fn report_external(
        &mut self,
        buffer: BufferId,
        change: ExternalChange,
        origin: ReloadOrigin,
    ) -> Redraw {
        if self.mark_external(buffer, change, origin) == Redraw::Skipped {
            return Redraw::Skipped;
        }
        let name = self
            .buffers
            .get(buffer)
            .map_or_else(String::new, |file| file.name().to_owned());
        self.set_message(
            match change {
                ExternalChange::Changed => {
                    format!("{name} changed on disk; the buffer keeps its unsaved changes")
                }
                ExternalChange::Missing => {
                    format!("{name} is gone from disk; the buffer keeps the only copy")
                }
            },
            MessageLevel::Warning,
        );
        Redraw::Needed
    }

    /// Reports one file that changed but could not be read again.
    ///
    /// The buffer keeps its text, so a file that grew past the size limit, or
    /// that another program filled with bytes that are not text, still leaves
    /// the editor usable.
    fn report_reload_error(
        &mut self,
        buffer: BufferId,
        path: &Path,
        error: &OpenError,
        origin: ReloadOrigin,
    ) -> Redraw {
        if self.mark_external(buffer, ExternalChange::Changed, origin) == Redraw::Skipped {
            return Redraw::Skipped;
        }
        self.set_message(
            format!("cannot reload {}: {error}", path.display()),
            MessageLevel::Error,
        );
        Redraw::Needed
    }

    /// Publishes one completed save.
    ///
    /// The save writes the message line last, so its report also names a
    /// format that produced no document. A user therefore always reads whether
    /// the written file holds formatted content. See
    /// `docs/language-services.md`.
    fn publish_save(
        &mut self,
        buffer: BufferId,
        requested: &Path,
        outcome: Result<SavedBuffer, SaveError>,
        then: AfterSave,
        format: FormatBeforeSave,
    ) -> Redraw {
        let slot = self.write_slot.take();
        let saved = match outcome {
            Ok(saved) => saved,
            // A failed save keeps the buffer dirty and usable, so the user can
            // repeat it. The write produced no durable change, so its reserved
            // slot returns to the outbox.
            Err(error) => {
                self.release_slot(slot);
                self.set_message(
                    format!("cannot save {}: {error}", requested.display()),
                    MessageLevel::Error,
                );
                return Redraw::Needed;
            }
        };
        // The write reached the filesystem, so its mandatory fact publishes
        // through the slot that the save reserved. See `docs/embedding.md`.
        self.publish_write(slot, saved.target.relative_path().clone());
        let Some(target) = self.buffers.get_mut(buffer) else {
            // The buffer left the list while the save ran.
            return Redraw::Skipped;
        };
        let lines = saved.lines;
        let name = saved.target.as_path().display().to_string();
        let bytes = saved.bytes;
        let applied = target.apply_save(saved.target, saved.identity, saved.version);
        // The saved file changed the working tree, so the recorded state of the
        // workspace changed with it.
        self.tree.request_git_status();
        let written = format!("\"{name}\" {lines}L, {bytes}B written");
        // The message line clips at the terminal width, and a path is the one
        // unbounded part of the report. The reason therefore leads, so a narrow
        // terminal clips the path and never the state of the written file.
        let report = match format.reason() {
            Some(reason) => format!("{reason}; {written}"),
            None => written,
        };
        self.set_message(report, format.level());
        match (then, applied) {
            (AfterSave::Stay, _) | (AfterSave::CloseWindow, SaveApplyOutcome::Stale) => {
                Redraw::Needed
            }
            (AfterSave::CloseWindow, SaveApplyOutcome::Current) => self
                .close_window(UnsavedChanges::Discard)
                .or(Redraw::Needed),
        }
    }

    /// Closes the focused window and ends the editor after the last window.
    ///
    /// The close destroys data only while the last window shows a buffer that
    /// holds unsaved changes, so only that close asks. See `docs/files.md`.
    fn close_window(&mut self, unsaved: UnsavedChanges) -> Redraw {
        let last_window = self.windows.window_count() == 1;
        if last_window && unsaved == UnsavedChanges::Ask && self.active_buffer().is_modified() {
            let buffer = self.active;
            let question = format!(
                "Quit and discard the unsaved changes of {}",
                self.active_buffer().name()
            );
            return self.ask_confirmation(question, ConfirmedAction::DiscardOnQuit { buffer });
        }
        match self.windows.apply(Command::CloseWindow) {
            WindowOutcome::LastWindow => {
                self.close_editor();
                Redraw::Needed
            }
            WindowOutcome::Changed => Redraw::Needed,
            WindowOutcome::Ignored | WindowOutcome::Unchanged => Redraw::Skipped,
        }
    }

    /// Closes the focused window after the user confirmed the lost changes.
    ///
    /// The answer names the buffer that the question named. An open that
    /// completes while the question waits makes another buffer active, and the
    /// user approved no loss of that buffer, so the quit stops there. See
    /// `docs/files.md`.
    fn confirmed_quit(&mut self, buffer: BufferId) -> Redraw {
        if self.active != buffer {
            self.set_message(QUIT_BUFFER_CHANGED_NOTE, MessageLevel::Warning);
            return Redraw::Needed;
        }
        self.close_window(UnsavedChanges::Discard)
    }

    /// Removes the active buffer from the buffer list.
    fn unload_active(&mut self) -> Redraw {
        let id = self.active;
        if self.active_buffer().is_modified() {
            self.set_message(UNSAVED_UNLOAD_NOTE, MessageLevel::Error);
            return Redraw::Needed;
        }
        // Stage the replacement before the removal, so no window ever points at
        // a buffer that the list no longer holds.
        let next = match self.buffers.ids().into_iter().find(|other| *other != id) {
            Some(next) => next,
            None => {
                let scratch = FileBuffer::scratch(&self.settings.files);
                let Some(next) = self.buffers.insert(scratch) else {
                    debug_assert!(false, "one loaded buffer leaves room for a scratch buffer");
                    return Redraw::Skipped;
                };
                next
            }
        };
        let closed = self
            .active_buffer()
            .path()
            .map(|path| LanguageRequest::Close {
                path: path.to_path_buf(),
            });
        self.buffers.remove(id);
        self.language.forget(id);
        self.analysis.remove(&id);
        if let Some(request) = closed {
            self.queue_language(request);
        }
        for window in self.windows.window_ids() {
            if self.windows.buffer(window) == Some(id) {
                self.windows.set_buffer(window, next);
                // The window shows other text now, so its view restarts.
                self.restart_window_view(window);
            }
        }
        let redraw = self.switch_to(next);
        self.set_message("the buffer is unloaded", MessageLevel::Info);
        redraw.or(Redraw::Needed)
    }

    /// Shows one loaded buffer in the focused window.
    ///
    /// A window that starts to show other text restarts its cursor and its
    /// selection anchor, because both describe the previous buffer. Every other
    /// window keeps its own view.
    fn switch_to(&mut self, id: BufferId) -> Redraw {
        let window = self.windows.focused_window();
        let previous = self.windows.buffer(window);
        self.windows.set_buffer(window, id);
        let shows_new_text = previous != Some(id);
        if shows_new_text {
            self.restart_window_view(window);
        }
        if self.active == id {
            return if shows_new_text {
                Redraw::Needed
            } else {
                Redraw::Skipped
            };
        }
        debug_assert!(
            self.buffers.get(id).is_some(),
            "a caller switches only to a loaded buffer"
        );
        self.follow_focused_window();
        self.reconcile_viewports();
        Redraw::Needed
    }

    /// Restarts the view of one window at the top of the buffer that it shows.
    fn restart_window_view(&mut self, window: WindowId) {
        let Some(state) = self.windows.state_mut(window) else {
            debug_assert!(false, "the caller names a leaf of the window tree");
            return;
        };
        *state = state.showing_new_buffer();
    }

    /// Runs one accepted search query.
    fn run_search(&mut self, text: &str) -> Redraw {
        let query = match SearchQuery::new(text, SearchDirection::Forward) {
            Ok(query) => query,
            Err(error) => {
                self.set_message(error.to_string(), MessageLevel::Error);
                return Redraw::Needed;
            }
        };
        // The query is valid, so the search moves the cursor to its first
        // match. Vim treats that move as a jump, so the position under the
        // search prompt joins the list before the move. A query that finds no
        // match records the line that the cursor already holds, which the list
        // folds into the entry that names it.
        self.record_jump();
        let Some(active) = self.buffers.get(self.active) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return Redraw::Skipped;
        };
        let matches = query.matches(active.text(), &self.settings.search);
        let version = active.text().version();
        let window = self.windows.focused_window();
        let Some(mut state) = self.windows.state(window) else {
            debug_assert!(false, "the layout always keeps the focused window visible");
            return Redraw::Needed;
        };
        let context = CommandContext {
            buffer: active.text(),
            settings: &self.settings,
            search: Some(&query),
        };
        let outcome = self.editing.search(&context, &mut state, &query);
        if let Some(slot) = self.windows.state_mut(window) {
            *slot = state;
        }
        self.search = Some(ActiveSearch {
            query,
            matches,
            version,
        });
        if outcome == CommandOutcome::SearchMissed {
            self.set_message("no match", MessageLevel::Warning);
        }
        Redraw::Needed
    }

    /// Ends the active buffer search.
    ///
    /// The highlighted matches disappear, and the cursor stays where the last
    /// move left it. `Esc` and `Ctrl-C` both reach this entry point.
    fn end_search(&mut self) -> Redraw {
        if self.search.take().is_none() {
            return Redraw::Skipped;
        }
        Redraw::Needed
    }

    /// Moves the file-tree selection to the next or the previous match.
    fn select_tree_match(&mut self, direction: SearchDirection) -> Redraw {
        if self.tree.select_match(direction) == TreeMatchOutcome::Missed {
            self.set_message("no match", MessageLevel::Warning);
        }
        Redraw::Needed
    }

    /// Closes the open prompt and restores the editor mode.
    ///
    /// A picker owns its own prompt, so the closed prompt also closes the
    /// picker. The editor state below the overlay never changed, so the
    /// previous view returns exactly as it was.
    fn close_prompt(&mut self) {
        self.prompt = None;
        self.picker = None;
        // The completion belongs to the command line, so its walk and its files
        // leave the session with it and fill no list of a later line.
        self.completion_walk = CompletionWalk::Unasked;
        self.sync_context();
    }

    /// Moves input to the owner that holds the keys.
    ///
    /// Three owners can be open at the same time, and they own the keys in one
    /// order. An open confirmation owns them first, because it draws over the
    /// prompt and reads its own answer. One `Enter` therefore reaches the
    /// confirmation alone. An open prompt owns them next. The scope
    /// of the focus owns them last. Each owner therefore returns the keys to
    /// the next owner that is still open, so a question that opened over a
    /// prompt returns them to that prompt and not to the scope below it.
    ///
    /// The function derives the owner from the open state alone, so no caller
    /// records what to return to. Every entry point that opens or closes an
    /// owner calls it.
    fn sync_context(&mut self) {
        let below = self.input_scope().context();
        let context = if self.confirmation.is_some() {
            below.open_confirmation()
        } else if let Some(prompt) = self.prompt.as_ref() {
            below.open_prompt(prompt.kind)
        } else {
            below
        };
        self.resolver.set_context(context);
    }

    /// Reports one command outcome on the message line.
    fn report(&mut self, outcome: CommandOutcome) -> Redraw {
        match outcome {
            CommandOutcome::RegisterEmpty => {
                self.set_message("the register is empty", MessageLevel::Warning);
                Redraw::Needed
            }
            CommandOutcome::HistoryExhausted => {
                self.set_message("no further change", MessageLevel::Warning);
                Redraw::Needed
            }
            CommandOutcome::SearchMissed => {
                self.set_message("no match", MessageLevel::Warning);
                Redraw::Needed
            }
            CommandOutcome::Rejected => {
                self.set_message("the input passes an editor limit", MessageLevel::Error);
                Redraw::Needed
            }
            CommandOutcome::Changed | CommandOutcome::Applied => Redraw::Needed,
            // A pending operator, an aborted operator, and an unhandled command
            // all leave the visible state as it was.
            CommandOutcome::OperatorPending
            | CommandOutcome::OperatorAborted
            | CommandOutcome::Unhandled => Redraw::Skipped,
        }
    }

    /// Replaces the message line.
    fn set_message(&mut self, text: impl Into<String>, level: MessageLevel) {
        // The message line and the statusline own every editor report. The
        // notification overlay carries language server progress alone, so an
        // ordinary report never reaches a second surface. See
        // `docs/language-services.md`.
        let message = Message::new(text, level);
        // Every message reaches the message line through this one call, so the
        // log holds every message, including one that another message replaces.
        // See `docs/windows.md`.
        self.log
            .record(self.clock, LogSource::MessageLine, level, message.text());
        self.message = Some(message);
    }

    /// Empties the message line and reports whether it held a message.
    fn clear_message(&mut self) -> Redraw {
        if self.message.take().is_some() {
            Redraw::Needed
        } else {
            Redraw::Skipped
        }
    }

    /// Recomputes the search matches after the buffer changed.
    ///
    /// The scan is bounded by the search limits of the `editor` module, so it
    /// stays inside the event-loop budget.
    fn refresh_search(&mut self, refresh: SearchRefresh) {
        let Some(active) = self.buffers.get(self.active) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return;
        };
        let version = active.text().version();
        let Some(search) = self.search.as_mut() else {
            return;
        };
        if search.version == version && refresh == SearchRefresh::OnVersionChange {
            return;
        }
        search.matches = search.query.matches(active.text(), &self.settings.search);
        search.version = version;
    }

    /// Resizes every visible viewport to its text area and follows its cursor.
    ///
    /// The window tree sizes a viewport to the complete window rectangle,
    /// because it holds no buffer and no settings. The session knows the winbar
    /// row and the gutter width, so it publishes the real text area here. The
    /// scroll margin then applies to the cells that the reader actually sees.
    ///
    /// Every window reconciles against its own buffer and its own cursor, so a
    /// move in one window scrolls that window alone. See `docs/windows.md`.
    fn reconcile_viewports(&mut self) {
        let display = self.settings.display;
        let regions: Vec<(WindowId, u16, u16)> = self
            .windows
            .layout()
            .regions()
            .iter()
            .filter(|region| region.kind == RegionKind::Surface)
            .map(|region| (region.id, region.area.width, region.area.height))
            .collect();
        for (id, area_width, area_height) in regions {
            let Some(buffer) = self.windows.buffer(id) else {
                continue;
            };
            let Some(file) = self.buffers.get(buffer) else {
                debug_assert!(false, "every window points at one loaded buffer");
                continue;
            };
            let text = file.text();
            let gutter = gutter_cells(text, &display, area_width);
            let width =
                NonZeroU16::new(area_width.saturating_sub(gutter)).unwrap_or(NonZeroU16::MIN);
            let height =
                NonZeroU16::new(area_height.saturating_sub(WINBAR_ROWS)).unwrap_or(NonZeroU16::MIN);
            let Some(slot) = self.windows.state_mut(id) else {
                continue;
            };
            *slot = slot.resized(height, width).reconciled(text, &display);
        }
    }
}

/// The message that a confirmed quit shows after the focus moved.
const QUIT_BUFFER_CHANGED_NOTE: &str =
    "the focused window shows another buffer now, so the editor kept running";

/// The message that a refused unload shows.
const UNSAVED_UNLOAD_NOTE: &str = "the buffer holds unsaved changes; save it before the unload";

/// The message that a save without a file name shows.
const NO_FILE_NAME_NOTE: &str = "the buffer holds no file name; use :e <path> to name one";

/// The message that the format-on-save toggle of a buffer without a formatter
/// shows.
const NO_FORMATTER_NOTE: &str = "no formatter serves this buffer";

/// The reason that the save report of a failed formatter run names.
const FORMATTER_FAILED_NOTE: &str = "the formatter failed, so the file holds unformatted content";

/// The reason that the save report of a formatter that is absent names.
const FORMATTER_MISSING_NOTE: &str =
    "the formatter is not installed, so the file holds unformatted content";

/// The message that a comment toggle without a line-comment token shows.
const NO_COMMENT_TOKEN_NOTE: &str =
    "no language adapter provides a line-comment token for this buffer";

/// The message that a diagnostic jump without any diagnostic shows.
const NO_DIAGNOSTIC_NOTE: &str = "the buffer holds no diagnostic";

/// The message that the diagnostic float shows while the cursor marks none.
const NO_DIAGNOSTIC_AT_CURSOR_NOTE: &str = "no diagnostic at the cursor";

/// The message that a reveal of a buffer without a file name shows.
const NO_REVEAL_PATH_NOTE: &str =
    "the buffer holds no file name; the file tree shows the workspace";

/// The message that a server position outside the buffer shows.
const OUTSIDE_BUFFER_NOTE: &str = "the language server named a position outside the buffer";

/// The message that a backward step at the oldest recorded position shows.
const OLDEST_JUMP_NOTE: &str = "the jump list holds no older position";

/// The message that a forward step at the newest recorded position shows.
const NEWEST_JUMP_NOTE: &str = "the jump list holds no newer position";

/// The message that a step into a buffer without a file shows.
///
/// The editor dropped the buffer and no file holds its text, so nothing can
/// bring the recorded position back.
const UNLOADED_JUMP_NOTE: &str = "the jump target buffer is gone";

/// The message that a missing `git` command shows once for each session.
/// Returns the number of rows that one region of the review shows.
///
/// The statusline and the message line take their own rows, so the review
/// draws inside the body band alone.
fn review_body_rows(area: Rect) -> u16 {
    shell_areas(area).body.height
}

/// Reports whether the sidebar owns one command.
///
/// The sidebar table binds these commands, so a key that reaches one of them
/// while the sidebar holds the focus acts on the tree. Every other command
/// falls through to the owner that the session picks next, which is how a
/// leader sequence reaches its command from the sidebar. See
/// `docs/input-actions.md`.
const fn tree_owns(command: Command) -> bool {
    matches!(
        command,
        Command::CloseWindow
            | Command::EndSearch
            | Command::FocusWindowDown
            | Command::FocusWindowLeft
            | Command::FocusWindowRight
            | Command::FocusWindowUp
            | Command::MoveDown
            | Command::MoveFirstLine
            | Command::MoveFullPageDown
            | Command::MoveFullPageUp
            | Command::MoveHalfPageDown
            | Command::MoveHalfPageUp
            | Command::MoveLastLine
            | Command::MoveUp
            | Command::ResizeWindowDown
            | Command::ResizeWindowLeft
            | Command::ResizeWindowRight
            | Command::ResizeWindowUp
            | Command::SaveBuffer
            | Command::SearchNext
            | Command::SearchPrevious
            | Command::TreeAddDirectory
            | Command::TreeAddFile
            | Command::TreeCollapseEntry
            | Command::TreeCopyEntry
            | Command::TreeCutEntry
            | Command::TreeDelete
            | Command::TreeExpandEntry
            | Command::TreeOpenEntry
            | Command::TreePasteEntries
            | Command::TreeRefresh
            | Command::TreeRename
            | Command::TreeSearch
            | Command::TreeSelectParent
            | Command::TreeToggleEntry
            | Command::TreeToggleHidden
    )
}

/// The message that a refused diff capture writes once.
const DIFF_UNAVAILABLE_NOTE: &str =
    "the changes are unavailable: git refused the read of this worktree";

const GIT_MISSING_NOTE: &str =
    "the `git` command is not available; the file tree shows no repository state";

/// The message that a refused workspace watch shows once for each session.
const WATCH_MISSING_NOTE: &str =
    "the workspace watcher could not start; the file tree updates on a refresh";

/// The action that a workspace without a complete watch always offers.
const WATCH_REFRESH_ACTION: &str = "the file tree updates on a refresh";

/// Returns the report of a workspace that carries a watch in part.
///
/// The note names the cause first, because the message line shows the start of
/// a long report. The host refuses a watch at a limit that `setting` names, and
/// the user raises that limit. A bound of the editor needs no setting, because
/// the refresh command reads the workspace by hand.
///
/// A platform that publishes no name of its watch limit still reports the
/// refusal and its cause. See `docs/files.md`.
pub(super) fn watch_coverage_note(coverage: WatchCoverage, setting: Option<&str>) -> String {
    debug_assert!(
        !coverage.is_complete(),
        "a complete registration reports nothing"
    );
    let mut causes: Vec<String> = Vec::new();
    if coverage.refused > 0 {
        let refused = coverage.refused;
        causes.push(format!("the host refused {refused} workspace watches"));
    }
    if coverage.truncated {
        causes.push("the workspace passes the watch bounds of the editor".to_owned());
    }
    let action = match (coverage.at_limit, setting) {
        (true, Some(setting)) => format!("raise `{setting}`"),
        (true, None) => "raise the watch limit of the host".to_owned(),
        (false, _) => WATCH_REFRESH_ACTION.to_owned(),
    };
    format!("{}; {action}", causes.join(" and "))
}

/// The title band of the hover float.
const HOVER_TITLE: &str = " Hover ";

/// The title band of the diagnostic float.
const DIAGNOSTIC_TITLE: &str = " Diagnostics ";

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
