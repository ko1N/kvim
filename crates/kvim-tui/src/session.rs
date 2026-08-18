//! The visible editor state and the pure transitions of the event loop.
//!
//! [`Session`] owns every value that the terminal shows: the loaded buffers,
//! the window tree, the editing state, the input resolver, the active search,
//! the open prompt, and the last message. It performs no filesystem work, no
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

use std::collections::BTreeMap;
use std::num::NonZeroU16;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ratatui::Frame;
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
    BindingScope, COMMAND_LINE_CHARS_MAX, Command, CommandLineCommand, Mode, PromptEdit,
    PromptKind, Registry, Resolution, Resolver, TreePrompt, WhichKeyRow,
};
use kvim_language::{
    Analysis, AnalysisError, AnalysisInput, BufferSyntax, ContentChange, Diagnostic, DiagnosticSet,
    DocumentPosition, FormatEdits, HighlightSpan, LanguageAdapter, LanguageEvent, LanguageOutcome,
    LanguageRegistry, LanguageRequestId, LspError, Publication, SourceLocation, SyntaxTree,
};
use kvim_runtime::{ProcessOutput, ProcessRequest};
use kvim_settings::EditorSettings;
use kvim_terminal::{Chord, Key, KeyCode, TerminalEvent};
use kvim_workspace::{
    Acceptance, BUFFERS_MAX, BufferId, Buffers, Candidate, EntryKind, FileBuffer, FileOperation,
    FileRequest, FileResult, FileTree, MutationError, MutationOutcome, OpenRequest, OpenedFile,
    PICKER_QUERY_CHARS_MAX, PickerKind, PickerRequest, PickerResult, PickerSlot, SaveError,
    SaveRequest, SavedBuffer, TREE_FILTER_CHARS_MAX, TransferMode, WorkspaceRequest,
    WorkspaceResult, render_content,
};

use super::buffer_view::{WINBAR_ROWS, gutter_cells};
use super::chrome::shell_areas;
use super::clipboard::{ClipboardStep, SessionClipboard, register_value};
use super::language::{
    AfterSave, DiagnosticJump, Float, FormatOnSave, LanguageNotice, LanguageQuery, LanguageRequest,
    LanguageRequestKind, LanguageState, PendingJump, PendingQuery, QueryPurpose, jump_target,
};
use super::layout::RegionKind;
use super::picker::{PickerFailure, PickerState, RIPGREP_MISSING_NOTE, picker_areas};
use super::theme::Theme;
use super::tree::{TREE_NAME_CHARS_MAX, TREE_TITLE_ROWS, TreeMotion, TreeRefusal, TreeSidebar};
use super::window::{SidebarSide, WindowId, WindowOutcome, Windows};

/// The largest message that the message line keeps, in characters.
///
/// Every message comes from a bounded label or from a typed error, so the bound
/// only protects the line against an unexpectedly long path.
pub const MESSAGE_CHARS_MAX: usize = 512;

/// Whether the visible state changed and the terminal needs a new frame.
///
/// Kvim renders only after a visible state change. It runs no unconditional
/// frame loop. See `docs/responsiveness.md`.
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

/// Whether a close command may discard unsaved changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnsavedChanges {
    /// Refuse the close while the active buffer holds unsaved changes.
    Refuse,
    /// Close the window and discard the unsaved changes.
    Discard,
}

/// The file operation that the editor waits for.
///
/// The editor runs one file operation at a time, so a second request cannot
/// apply an obsolete result over a newer buffer state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingFile {
    /// One file is loading.
    Open,
    /// One buffer is saving.
    Save {
        /// The buffer that the save belongs to.
        buffer: BufferId,
        /// The step that follows the save.
        then: AfterSave,
    },
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

/// One analysis job that the bounded worker service runs.
///
/// The session builds the job and never runs it, so parsing stays off the
/// terminal event loop. See `docs/language-services.md`.
pub struct AnalysisRequest {
    buffer: BufferId,
    adapter: &'static dyn LanguageAdapter,
    input: AnalysisInput,
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
        AnalysisResult {
            buffer: self.buffer,
            outcome: self.adapter.analyze(&self.input, cancellation),
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
        let mut text = text.into();
        if text.chars().count() > MESSAGE_CHARS_MAX {
            text = text.chars().take(MESSAGE_CHARS_MAX).collect();
        }
        Self { text, level }
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
}

impl PromptLine {
    /// Returns the largest number of characters that the prompt accepts.
    const fn chars_max(&self) -> usize {
        match self.kind {
            PromptKind::CommandLine => COMMAND_LINE_CHARS_MAX,
            PromptKind::Search => SEARCH_QUERY_CHARS_MAX,
            PromptKind::Tree(TreePrompt::Filter) => TREE_FILTER_CHARS_MAX,
            PromptKind::Tree(
                TreePrompt::AddFile | TreePrompt::AddDirectory | TreePrompt::Rename,
            ) => TREE_NAME_CHARS_MAX,
            PromptKind::Picker => PICKER_QUERY_CHARS_MAX,
        }
    }
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
    /// Every loaded buffer, because each window shows its own buffer.
    pub(super) buffers: &'a Buffers,
    /// The buffer that the editing state and the active search belong to.
    pub(super) active: BufferId,
    pub(super) analysis: &'a BTreeMap<BufferId, BufferAnalysis>,
    /// The published diagnostics and the language-service state.
    pub(super) language: &'a LanguageState,
    pub(super) editing: &'a EditingState,
    pub(super) search: Option<&'a ActiveSearch>,
    pub(super) prompt: Option<&'a PromptLine>,
    pub(super) message: Option<&'a Message>,
    pub(super) float: Option<&'a Float>,
    pub(super) which_key: Option<&'a [WhichKeyRow]>,
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
/// let root = std::env::current_dir().expect("the test process holds a working directory");
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
/// assert_eq!(session.buffer().to_string(), "x");
/// ```
pub struct Session {
    area: Rect,
    settings: EditorSettings,
    theme: Theme,
    buffers: Buffers,
    active: BufferId,
    /// The file operation that waits for the bounded worker service.
    file_outbox: Option<FileRequest>,
    /// The file operation that the editor waits for.
    file_pending: Option<PendingFile>,
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
    /// Reports whether the editor already named the missing `rg` command.
    ///
    /// A missing command is a normal state, so the editor reports it once and
    /// stays usable.
    ripgrep_reported: bool,
    editing: EditingState,
    registers: Registers,
    /// The system clipboard boundary that the composition root selected.
    clipboard: SessionClipboard,
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
    message: Option<Message>,
    /// The open floating overlay of the language services.
    float: Option<Float>,
    which_key: Option<Vec<WhichKeyRow>>,
    run: RunState,
}

impl Session {
    /// Creates a session that shows one empty scratch buffer.
    ///
    /// The workspace root is the directory that the file tree shows. The
    /// caller resolves it once, because the session performs no filesystem
    /// work. See `docs/files.md`.
    ///
    /// # Panics
    ///
    /// Panics when the hardcoded first-release binding table is invalid. This
    /// is a cold-path bootstrap check, so an invalid table must fail at start.
    #[must_use]
    pub fn new(area: Rect, settings: EditorSettings, root: PathBuf) -> Self {
        let (buffers, active) = Buffers::new(FileBuffer::scratch(&settings.files));
        let mut session = Self {
            area,
            settings,
            theme: Theme::new(settings.theme),
            buffers,
            active,
            file_outbox: None,
            file_pending: None,
            windows: Windows::new(active, shell_areas(area).body, settings.windows),
            tree: TreeSidebar::new(root),
            tree_region: None,
            picker: None,
            ripgrep_reported: false,
            editing: EditingState::new(),
            registers: Registers::default(),
            clipboard: SessionClipboard::default(),
            clipboard_activity: ClipboardActivity::Idle,
            clipboard_revision: 0,
            resolver: Resolver::new(Registry::first_release(), settings.input),
            languages: LanguageRegistry::first_release(),
            analysis: BTreeMap::new(),
            analysis_pending: None,
            language: LanguageState::default(),
            search: None,
            prompt: None,
            message: None,
            float: None,
            which_key: None,
            run: RunState::Running,
        };
        session.reconcile_viewports();
        session
    }

    /// Injects the system clipboard that the composition root selected.
    ///
    /// A session without this call reaches no clipboard command at all, which
    /// keeps every test free from the host clipboard. See `docs/clipboard.md`.
    #[must_use]
    pub(super) fn with_clipboard(mut self, clipboard: SessionClipboard) -> Self {
        self.clipboard = clipboard;
        self
    }

    /// Returns the terminal rectangle that the session renders into.
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
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
    /// The which-key overlay is the only such change: a pending sequence holds
    /// no deadline and waits for the next key. The event loop therefore waits
    /// for a terminal event or for this time, never for a frame interval.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        self.resolver
            .overlay_deadline()
            .filter(|_| self.which_key.is_none())
    }

    /// Applies one normalized terminal event.
    pub fn handle_event(&mut self, event: TerminalEvent, now: Duration) -> Redraw {
        let redraw = match event {
            TerminalEvent::Key(key) => self.handle_key(key, now),
            TerminalEvent::Resize { columns, rows } => self.resize(Rect::new(0, 0, columns, rows)),
            // A focus change moves no cursor and shows no new text.
            TerminalEvent::Focus(_) => Redraw::Skipped,
        };
        self.settle(now).or(redraw)
    }

    /// Applies the state changes that the elapsed time alone causes.
    ///
    /// Only the which-key overlay reaches this path, because the pending
    /// sequence itself never expires.
    pub fn tick(&mut self, now: Duration) -> Redraw {
        self.settle(now)
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
            buffers: &self.buffers,
            active: self.active,
            analysis: &self.analysis,
            language: &self.language,
            editing: &self.editing,
            search: self.search.as_ref(),
            prompt: self.prompt.as_ref(),
            message: self.message.as_ref(),
            float: self.float.as_ref(),
            which_key: self.which_key.as_deref(),
        }
    }

    /// Restores every derived value after one transition.
    ///
    /// The overlay rows, the search matches, and the viewports all follow the
    /// state that the transition produced, so the next frame is consistent.
    fn settle(&mut self, now: Duration) -> Redraw {
        self.refresh_search();
        self.reconcile_viewports();
        self.reconcile_tree();
        self.reconcile_picker();
        let mirrored = self.reconcile_clipboard();
        let rows = self.resolver.which_key(now);
        if rows.as_deref() == self.which_key.as_deref() {
            return mirrored;
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
        match self.resolver.resolve(key, now) {
            Resolution::Command { command, count } => self.apply_command(command, count),
            Resolution::Prompt(edit) => self.apply_prompt(edit),
            // A pending sequence and a cancelled sequence both change only the
            // which-key overlay, and `settle` publishes that change.
            Resolution::Pending | Resolution::Cancelled => Redraw::Skipped,
            // Insert mode reaches no binding for a printable key, so the key
            // becomes buffer text.
            Resolution::NoMatch => self.insert_key(key),
        }
    }

    /// Applies one semantic command.
    ///
    /// The window tree sees every command first, because it owns the split,
    /// focus, resize, and close commands. The editing state sees the rest.
    fn apply_command(&mut self, command: Command, count: Option<NonZeroU32>) -> Redraw {
        let cleared = self.clear_message();
        // An open picker owns every key, so a picker key never reaches the
        // buffer of an editor window.
        if self.picker.is_some() {
            return self.apply_picker_command(command).or(cleared);
        }
        // The sidebar owns every key while it holds the focus, so a tree
        // command never reaches the buffer of an editor window.
        if self.sidebar_has_focus() {
            return self.apply_tree_command(command, count).or(cleared);
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
                return self.close_window(UnsavedChanges::Refuse).or(cleared);
            }
            Command::ToggleComment => return self.toggle_comment().or(cleared),
            // A paste reads the system clipboard first, so the unnamed register
            // carries an external copy as well. See `docs/clipboard.md`.
            Command::PasteAfter | Command::PasteBefore => {
                return self
                    .start_clipboard(ClipboardWork::Paste { command, count })
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
            Command::ToggleFormatOnSave => return self.toggle_format_on_save().or(cleared),
            Command::RevealInFileTree => return self.reveal_active_file().or(cleared),
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
            WindowOutcome::Unchanged => return cleared,
            WindowOutcome::LastWindow => {
                self.run = RunState::Finished;
                return cleared;
            }
        }
        self.apply_editing_command(command, count).or(cleared)
    }

    /// Applies one command to the buffer of the focused window.
    ///
    /// A deferred paste reaches the same entry point after its system clipboard
    /// read resolved, so both paths run the identical transition.
    fn apply_editing_command(&mut self, command: Command, count: Option<NonZeroU32>) -> Redraw {
        let auto = self.auto_indent(command);
        let outcome = self.edit(|editing, context, window| {
            editing.apply_indented(context, window, command, count, auto)
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

    /// Runs one change against the buffer, the registers, and the focused
    /// viewport.
    ///
    /// The viewport travels out of the window tree and back in one place, so no
    /// caller can lose a scroll position.
    fn edit<F>(&mut self, change: F) -> CommandOutcome
    where
        F: FnOnce(&mut EditingState, &mut EditContext<'_>, &mut WindowState) -> CommandOutcome,
    {
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
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        let outcome = change(&mut self.editing, &mut context, &mut state);
        let applied = std::mem::take(&mut context.applied);
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
        let Ok(changes) = ContentChange::from_transaction(before, transaction) else {
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

    /// Applies one key while Insert mode is active.
    ///
    /// The `editor` module owns every text rule, so `Enter` and `Backspace`
    /// reach its entry points instead of building a text here. Every other plain
    /// key inserts its own characters.
    fn insert_key(&mut self, key: Key) -> Redraw {
        if self.editing.mode() != Mode::Insert || key.chord() != Chord::Plain {
            return Redraw::Skipped;
        }
        let indent = self.settings.indent;
        let outcome = match key.code() {
            KeyCode::Enter => {
                // The line break opens at the cursor, so the syntax indent
                // answers for that byte offset.
                let buffer = self.buffer();
                let byte = buffer.char_to_byte(self.cursor().position(buffer)).get();
                let auto = self.indent_level(byte);
                self.edit(|editing, context, window| {
                    editing.insert_line_break_indented(context, window, auto)
                })
            }
            KeyCode::Backspace => {
                self.edit(|editing, context, window| editing.delete_backward(context, window))
            }
            KeyCode::Char(value) => {
                let text = value.to_string();
                self.edit(|editing, context, window| editing.insert_text(context, window, &text))
            }
            KeyCode::Tab => {
                let text = if indent.expand_tab {
                    " ".repeat(usize::from(indent.tab_width.get()))
                } else {
                    "\t".to_owned()
                };
                self.edit(|editing, context, window| editing.insert_text(context, window, &text))
            }
            _ => return Redraw::Skipped,
        };
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
        });
        let context = self.input_scope().context().open_prompt(kind);
        self.resolver.set_context(context);
        Redraw::Needed
    }

    /// Returns the scope that owns the keys while no prompt is open.
    fn input_scope(&self) -> BindingScope {
        if self.picker.is_some() {
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
        let root = self.tree.tree().root().to_path_buf();
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
                self.open_at(path, position).or(Redraw::Needed)
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
    pub fn apply_picker_result(&mut self, result: PickerResult) -> Redraw {
        let Some(picker) = self.picker.as_mut() else {
            return Redraw::Skipped;
        };
        let redraw = picker.apply_result(result);
        self.reconcile_picker();
        redraw
    }

    /// Reports that one picker request produced no result.
    ///
    /// A missing external command is a normal state. The editor names it once
    /// and stays fully usable without the search picker.
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
                prompt.text.push(value);
                self.sync_picker_query();
                Redraw::Needed
            }
            PromptEdit::DeleteBackward => {
                // Backspace on the empty line cancels the prompt, like Vim.
                if prompt.text.pop().is_none() {
                    self.close_prompt();
                    return Redraw::Needed;
                }
                self.sync_picker_query();
                Redraw::Needed
            }
            PromptEdit::Cancel => {
                self.close_prompt();
                Redraw::Needed
            }
            PromptEdit::Accept => self.accept_prompt(),
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
            CommandLineCommand::Quit => return self.close_window(UnsavedChanges::Refuse),
            CommandLineCommand::QuitDiscard => {
                return self.close_window(UnsavedChanges::Discard);
            }
            CommandLineCommand::GoToLine(line) => {
                let target = usize::try_from(line.get()).unwrap_or(usize::MAX);
                self.place_cursor(target - 1, 0);
            }
        }
        Redraw::Needed
    }

    /// Opens one path in the focused window.
    ///
    /// A path that a loaded buffer already owns needs no filesystem work, so
    /// the editor switches to that buffer at once. Every other path becomes one
    /// bounded request, because the event loop reads no file. See
    /// `docs/responsiveness.md`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_settings::EditorSettings;
    /// use kvim_tui::Session;
    ///
    /// let root = std::env::current_dir().expect("the test process holds a working directory");
    /// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root);
    /// session.open_path("Cargo.toml".into());
    ///
    /// // The event loop hands the request to the bounded worker service.
    /// let request = session.take_file_request().expect("the open needs one file read");
    /// session.apply_file_result(request.run());
    /// assert_eq!(session.buffers().len(), 2);
    /// ```
    pub fn open_path(&mut self, path: PathBuf) -> Redraw {
        if let Some(id) = self.buffers.find_path(&path) {
            return self.switch_to(id);
        }
        if self.buffers.len() >= BUFFERS_MAX {
            self.set_message(
                format!("the editor holds the maximum of {BUFFERS_MAX} buffers"),
                MessageLevel::Error,
            );
            return Redraw::Needed;
        }
        let files = self.settings.files;
        self.start_file_request(
            FileRequest::Open(OpenRequest { path, files }),
            PendingFile::Open,
        )
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
        })
    }

    /// Publishes one completed analysis behind the buffer-version gate.
    ///
    /// A result for an obsolete buffer version changes nothing and enters no
    /// cache. A typed failure renders plain text and keeps the buffer editable.
    pub fn apply_analysis_result(&mut self, result: AnalysisResult) -> Redraw {
        self.analysis_pending = None;
        let Some(file) = self.buffers.get(result.buffer) else {
            // The buffer left the list while the job ran.
            return Redraw::Skipped;
        };
        let current = file.text().version();
        let Some(entry) = self.analysis.get_mut(&result.buffer) else {
            debug_assert!(
                false,
                "the session creates the entry when it builds the job"
            );
            return Redraw::Skipped;
        };
        let Ok(analysis) = result.outcome else {
            return Redraw::Skipped;
        };
        if entry.syntax.accept(current, analysis) == Publication::Rejected {
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
    /// so a later answer reaches the question that asked for it.
    pub fn apply_language_dispatch(
        &mut self,
        kind: LanguageRequestKind,
        result: Result<Option<LanguageRequestId>, LspError>,
    ) -> Redraw {
        match result {
            Ok(id) => {
                if kind == LanguageRequestKind::Query
                    && let Some(pending) = self.language.pending.as_mut()
                {
                    debug_assert!(
                        id.is_some(),
                        "the language services name the identity of every accepted question"
                    );
                    pending.id = id;
                }
                Redraw::Skipped
            }
            Err(error) => {
                let redraw = self.report_language_error(&error);
                match kind {
                    LanguageRequestKind::Query => self.abandon_query().or(redraw),
                    LanguageRequestKind::Synchronization => redraw,
                }
            }
        }
    }

    /// Applies one typed result of the language services.
    ///
    /// Every result passes the buffer-version gate before it changes visible
    /// state, so an obsolete answer never reaches the screen.
    pub fn apply_language_event(&mut self, event: LanguageEvent) -> Redraw {
        match event.outcome {
            LanguageOutcome::Diagnostics(set) => self.publish_diagnostics(set),
            LanguageOutcome::Definition {
                request,
                version,
                locations,
            } => self.publish_definition(request, version, &locations),
            LanguageOutcome::Hover {
                request,
                version,
                text,
            } => self.publish_hover(request, version, text.as_deref()),
            LanguageOutcome::Formatting { request, edits } => {
                self.publish_formatting(request, &edits)
            }
            LanguageOutcome::Failed { request, error } => {
                let redraw = match request {
                    Some(request) if self.matches_pending(request) => self.abandon_query(),
                    Some(_) | None => Redraw::Skipped,
                };
                self.report_language_error(&error).or(redraw)
            }
            LanguageOutcome::Unavailable => {
                let redraw = self.report_language_notice(LanguageNotice::NotInstalled);
                self.abandon_query().or(redraw)
            }
            LanguageOutcome::Restarted => self.reopen_documents(),
            LanguageOutcome::Stopped => {
                let redraw = self.report_language_notice(LanguageNotice::Stopped);
                self.abandon_query().or(redraw)
            }
        }
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

    /// Reports whether one answer belongs to the question that the editor asked.
    fn matches_pending(&self, request: LanguageRequestId) -> bool {
        self.language
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == Some(request))
    }

    /// Releases the waiting question and completes a save that waited for it.
    ///
    /// A save must never depend on a language server, so a lost formatter
    /// answer still writes the buffer content that the user typed.
    fn abandon_query(&mut self) -> Redraw {
        let Some(pending) = self.language.pending.take() else {
            return Redraw::Skipped;
        };
        match pending.purpose {
            QueryPurpose::FormatBeforeSave(then) => self.start_save(then),
            QueryPurpose::Definition | QueryPurpose::Hover => Redraw::Skipped,
        }
    }

    /// Takes the question that one answer completes.
    fn take_pending(&mut self, request: LanguageRequestId) -> Option<PendingQuery> {
        if !self.matches_pending(request) {
            return None;
        }
        self.language.pending.take()
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
        let position = DocumentPosition::of_buffer(text, self.cursor().position(text));
        let query = match purpose {
            QueryPurpose::Definition => LanguageQuery::Definition(position),
            QueryPurpose::Hover => LanguageQuery::Hover(position),
            QueryPurpose::FormatBeforeSave(_) => {
                debug_assert!(false, "a format is not a question about one position");
                return Redraw::Skipped;
            }
        };
        self.language.pending = Some(PendingQuery {
            buffer,
            version,
            purpose,
            id: None,
        });
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
    /// and no cursor position, and an obsolete set changes nothing at all.
    fn publish_diagnostics(&mut self, set: DiagnosticSet) -> Redraw {
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
        self.language.publish(buffer, set);
        Redraw::Needed
    }

    /// Moves the cursor to one definition target, in this buffer or another.
    fn publish_definition(
        &mut self,
        request: LanguageRequestId,
        version: BufferVersion,
        locations: &[SourceLocation],
    ) -> Redraw {
        let Some(pending) = self.take_pending(request) else {
            return Redraw::Skipped;
        };
        if !self.answers_current_buffer(&pending, version) {
            return Redraw::Skipped;
        }
        let Some(location) = locations.first() else {
            self.set_message("no definition found", MessageLevel::Warning);
            return Redraw::Needed;
        };
        if self.buffers.get(pending.buffer).and_then(FileBuffer::path) == Some(&location.path) {
            return self.move_to_position(location.span.start);
        }
        self.open_at(location.path.clone(), location.span.start)
    }

    /// Opens one path in the focused window and places the cursor at one
    /// position.
    ///
    /// A definition jump and an accepted picker row both need this step, so
    /// both use one path. A buffer that the editor already holds moves the
    /// cursor at once. Every other path needs one file read, and the recorded
    /// jump waits for the completed load.
    fn open_at(&mut self, path: PathBuf, position: DocumentPosition) -> Redraw {
        self.language.jump = Some(PendingJump {
            path: path.clone(),
            position,
        });
        let redraw = self.open_path(path);
        self.follow_jump().or(redraw)
    }

    /// Shows one hover answer as a float.
    fn publish_hover(
        &mut self,
        request: LanguageRequestId,
        version: BufferVersion,
        text: Option<&str>,
    ) -> Redraw {
        let Some(pending) = self.take_pending(request) else {
            return Redraw::Skipped;
        };
        if !self.answers_current_buffer(&pending, version) {
            return Redraw::Skipped;
        }
        let Some(text) = text else {
            self.set_message("no hover information", MessageLevel::Info);
            return Redraw::Needed;
        };
        self.float = Some(Float::text(HOVER_TITLE, text));
        Redraw::Needed
    }

    /// Applies one formatter answer and writes the buffer afterwards.
    fn publish_formatting(&mut self, request: LanguageRequestId, edits: &FormatEdits) -> Redraw {
        let Some(pending) = self.take_pending(request) else {
            return Redraw::Skipped;
        };
        let QueryPurpose::FormatBeforeSave(then) = pending.purpose else {
            debug_assert!(false, "only a save asks for formatting edits");
            return Redraw::Skipped;
        };
        let redraw = self.apply_format_edits(pending.buffer, edits);
        self.start_save(then).or(redraw)
    }

    /// Applies the accepted formatter edits as one undoable transaction.
    ///
    /// An obsolete answer, a malformed range, and a buffer that already matches
    /// the formatter all leave the buffer as it is. The save follows either way.
    fn apply_format_edits(&mut self, buffer: BufferId, edits: &FormatEdits) -> Redraw {
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

    /// Reports whether one answer still describes the current buffer version.
    fn answers_current_buffer(&self, pending: &PendingQuery, version: BufferVersion) -> bool {
        if pending.version != version {
            return false;
        }
        self.buffers
            .get(pending.buffer)
            .is_some_and(|file| file.text().version() == version)
    }

    /// Moves the cursor to the recorded definition target of the active buffer.
    fn follow_jump(&mut self) -> Redraw {
        let Some(jump) = self.language.jump.take() else {
            return Redraw::Skipped;
        };
        if self.buffers.get(self.active).and_then(FileBuffer::path) != Some(&jump.path) {
            // The buffer is still loading, so the jump waits for it.
            self.language.jump = Some(jump);
            return Redraw::Skipped;
        }
        self.move_to_position(jump.position)
    }

    /// Places the cursor at one protocol position of the active buffer.
    fn move_to_position(&mut self, position: DocumentPosition) -> Redraw {
        let Some(active) = self.buffers.get(self.active) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return Redraw::Skipped;
        };
        let text = active.text();
        let Ok(target) = position.char_position(text) else {
            self.set_message(OUTSIDE_BUFFER_NOTE, MessageLevel::Warning);
            return Redraw::Needed;
        };
        let line = text.char_to_line(target).get();
        let column = text.char_to_column(target).get();
        self.place_cursor(line, column);
        self.reconcile_viewports();
        Redraw::Needed
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
        self.float = Some(Float::diagnostics(DIAGNOSTIC_TITLE, &found));
        Redraw::Needed
    }

    /// Returns the protocol position of the cursor in the active buffer.
    fn cursor_position(&self) -> Option<DocumentPosition> {
        let text = self.buffers.get(self.active)?.text();
        Some(DocumentPosition::of_buffer(
            text,
            self.cursor().position(text),
        ))
    }

    /// Returns the format-on-save state of one buffer.
    fn format_on_save(&self, buffer: BufferId) -> FormatOnSave {
        let default = if self.settings.files.format_on_save {
            FormatOnSave::Enabled
        } else {
            FormatOnSave::Disabled
        };
        self.language.format_on_save(buffer, default)
    }

    /// Toggles format-on-save for the active buffer and reports the new state.
    ///
    /// The toggle is per buffer, so it changes no other buffer and no default.
    fn toggle_format_on_save(&mut self) -> Redraw {
        let buffer = self.active;
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
    /// showing it never builds a second region.
    fn show_sidebar(&mut self) -> WindowId {
        match self.tree_region {
            Some(id) => {
                self.windows.set_sidebar_visible(SidebarSide::Right, true);
                id
            }
            None => {
                let id = self.windows.open_sidebar(
                    SidebarSide::Right,
                    self.settings.windows.file_tree_width_cells,
                );
                self.tree_region = Some(id);
                id
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
        self.windows.focus_region(region);
        self.sync_context();
        Redraw::Needed
    }

    /// Applies one semantic command while the sidebar holds the keys.
    ///
    /// The navigation commands are the buffer commands, so the tree moves by
    /// the same rule. The sidebar bounds every move by its own rows.
    fn apply_tree_command(&mut self, command: Command, count: Option<NonZeroU32>) -> Redraw {
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
            Command::TreeSelectParent => self.tree.select_parent(),
            Command::TreeToggleEntry => self.tree.toggle_selected(),
            Command::TreeCollapseEntry => self.tree.collapse_selected(),
            Command::TreeExpandEntry => return self.expand_selected_entry(),
            Command::TreeRefresh => self.tree.refresh_all(),
            Command::TreeToggleHidden => self.tree.toggle_hidden(),
            Command::TreeOpenEntry => return self.open_selected_entry(),
            Command::TreeFilter => return self.open_prompt(PromptKind::Tree(TreePrompt::Filter)),
            Command::TreeAddFile => {
                return self.open_prompt(PromptKind::Tree(TreePrompt::AddFile));
            }
            Command::TreeAddDirectory => {
                return self.open_prompt(PromptKind::Tree(TreePrompt::AddDirectory));
            }
            Command::TreeRename => return self.open_prompt(PromptKind::Tree(TreePrompt::Rename)),
            Command::TreeCopyEntry => return self.hold_entry(TransferMode::Copy),
            Command::TreeCutEntry => return self.hold_entry(TransferMode::Move),
            Command::TreeDelete => {
                let staged = self.tree.stage_delete();
                return self.start_tree_mutation(staged);
            }
            Command::TreePasteEntries => {
                let staged = self.tree.stage_paste();
                return self.start_tree_mutation(staged);
            }
            Command::SaveBuffer => return self.save_active(AfterSave::Stay),
            Command::CloseWindow
            | Command::FocusWindowLeft
            | Command::FocusWindowDown
            | Command::FocusWindowUp
            | Command::FocusWindowRight => return self.leave_sidebar(command),
            // The sidebar table holds no other command.
            _ => return Redraw::Skipped,
        }
        Redraw::Needed
    }

    /// Moves the focus out of the sidebar, or closes the sidebar.
    fn leave_sidebar(&mut self, command: Command) -> Redraw {
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
            WindowOutcome::Ignored | WindowOutcome::Unchanged => Redraw::Skipped,
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

    /// Queues one staged workspace mutation, or reports the refusal.
    fn start_tree_mutation(&mut self, staged: Result<FileOperation, TreeRefusal>) -> Redraw {
        let operation = match staged {
            Ok(operation) => operation,
            Err(refusal) => {
                self.set_message(refusal.message(), MessageLevel::Warning);
                return Redraw::Needed;
            }
        };
        // The worker validates the operation against the loaded buffers, so it
        // receives the complete list with the request.
        let buffers = self.buffers.open_buffers();
        if let Err(refusal) = self.tree.start_mutation(operation, buffers) {
            self.set_message(refusal.message(), MessageLevel::Warning);
        }
        Redraw::Needed
    }

    /// Runs one accepted file-tree prompt line.
    fn run_tree_prompt(&mut self, prompt: TreePrompt, text: &str) -> Redraw {
        match prompt {
            TreePrompt::Filter => {
                self.tree.set_query(text);
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

    /// Applies one completed workspace operation as one state transition.
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
    pub fn abandon_workspace_request(&mut self, failure: FileRequestFailure) -> Redraw {
        self.tree.abandon_request();
        self.set_message(failure.message(), MessageLevel::Error);
        Redraw::Needed
    }

    /// Publishes one completed mutation as one visible state change.
    ///
    /// The buffer paths, the affected directories, and the new selection change
    /// together, so no window shows a path that the workspace no longer holds.
    fn publish_mutation(&mut self, outcome: Result<MutationOutcome, MutationError>) -> Redraw {
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.tree.abandon_request();
                self.set_message(error.to_string(), MessageLevel::Error);
                return Redraw::Needed;
            }
        };
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
        self.tree.clear_moved_clipboard();
        self.tree.apply_mutation(&outcome);
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
    pub fn apply_file_result(&mut self, result: FileResult) -> Redraw {
        let pending = self.file_pending.take();
        match result {
            FileResult::Opened { requested, outcome } => match outcome {
                Ok(file) => self.publish_open(file),
                Err(error) => {
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
                let then = match pending {
                    Some(PendingFile::Save { then, .. }) => then,
                    Some(PendingFile::Open) | None => AfterSave::Stay,
                };
                self.publish_save(buffer, &requested, outcome, then)
            }
        }
    }

    /// Reports that one file request produced no result.
    ///
    /// The buffer keeps every unsaved change, so the user can repeat the
    /// operation.
    pub fn abandon_file_request(&mut self, failure: FileRequestFailure) -> Redraw {
        self.file_pending = None;
        self.file_outbox = None;
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
            ClipboardWork::Paste { command, count } => {
                let read = self.clipboard.finish_read(output);
                self.publish_paste(command, count, read)
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
            ClipboardWork::Paste { command, count } => match self.clipboard.read() {
                ClipboardStep::Done(read) => self.publish_paste(command, count, read),
                ClipboardStep::Waiting(request) => {
                    self.defer_clipboard(request, ClipboardWork::Paste { command, count })
                }
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
            Some(ClipboardWork::Paste { command, count }) => {
                self.publish_paste(command, count, ClipboardRead::Fallback(None))
            }
        }
    }

    /// Applies one paste over the register value that the read resolved.
    ///
    /// A value from the system clipboard becomes the unnamed register first, so
    /// an external copy pastes exactly like a Kvim yank.
    fn publish_paste(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
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
        let applied = self.apply_editing_command(command, count);
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
            self.set_message(
                "one file operation is already running",
                MessageLevel::Warning,
            );
            return Redraw::Needed;
        }
        debug_assert!(
            self.file_outbox.is_none(),
            "the event loop takes the queued request before the next command runs"
        );
        self.file_outbox = Some(request);
        self.file_pending = Some(pending);
        Redraw::Needed
    }

    /// Saves the active buffer, and formats it first when the buffer asks.
    ///
    /// A formatter failure, a timeout, and an obsolete answer all still save
    /// the buffer, so the user never loses work to a language server. See
    /// `docs/language-services.md`.
    fn save_active(&mut self, then: AfterSave) -> Redraw {
        if self.awaits_format() {
            self.set_message("one save is already running", MessageLevel::Warning);
            return Redraw::Needed;
        }
        if self.formats_before_save() {
            return self.request_format(then);
        }
        self.start_save(then)
    }

    /// Reports whether a save already waits for its formatter answer.
    fn awaits_format(&self) -> bool {
        matches!(
            self.language.pending,
            Some(PendingQuery {
                purpose: QueryPurpose::FormatBeforeSave(_),
                ..
            })
        )
    }

    /// Reports whether the active buffer formats before its next save.
    ///
    /// A buffer without a file name, and a buffer whose question would replace
    /// another running question, saves without a format instead.
    fn formats_before_save(&self) -> bool {
        if self.language.pending.is_some() {
            return false;
        }
        let named = self
            .buffers
            .get(self.active)
            .is_some_and(|file| file.path().is_some());
        named && self.format_on_save(self.active) == FormatOnSave::Enabled
    }

    /// Asks the language server of the active buffer for its formatting edits.
    fn request_format(&mut self, then: AfterSave) -> Redraw {
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
            return self.start_save(then);
        };
        let version = file.text().version();
        self.language.pending = Some(PendingQuery {
            buffer,
            version,
            purpose: QueryPurpose::FormatBeforeSave(then),
            id: None,
        });
        self.queue_language(LanguageRequest::Query {
            buffer,
            path,
            version,
            query: LanguageQuery::Format,
        });
        Redraw::Needed
    }

    /// Writes the active buffer and runs the step that follows the save.
    fn start_save(&mut self, then: AfterSave) -> Redraw {
        let buffer = self.active;
        // Build the complete request before the operation starts, so a rejected
        // save never changes the buffer.
        let staged = self.buffers.get(buffer).and_then(|active| {
            let path = active.path()?.to_path_buf();
            Some(SaveRequest {
                buffer,
                content: render_content(active.text()),
                expected: active.identity(),
                snapshot: active.text().clone(),
                files: self.settings.files,
                path,
            })
        });
        let Some(request) = staged else {
            self.set_message(NO_FILE_NAME_NOTE, MessageLevel::Error);
            return Redraw::Needed;
        };
        self.start_file_request(
            FileRequest::Save(request),
            PendingFile::Save { buffer, then },
        )
    }

    /// Publishes one loaded buffer.
    fn publish_open(&mut self, file: OpenedFile) -> Redraw {
        // Two spellings of one path reach the same file, so the completed load
        // returns the buffer that already owns it.
        if let Some(existing) = self.buffers.find_path(&file.path) {
            return self.switch_to(existing).or(Redraw::Needed);
        }
        let name = file.path.display().to_string();
        let lines = file.text.line_count();
        let bytes = file.text.len_bytes();
        let loaded = FileBuffer::loaded(file.text, file.path, file.identity);
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

    /// Publishes one completed save.
    fn publish_save(
        &mut self,
        buffer: BufferId,
        requested: &Path,
        outcome: Result<SavedBuffer, SaveError>,
        then: AfterSave,
    ) -> Redraw {
        let saved = match outcome {
            Ok(saved) => saved,
            // A failed save keeps the buffer dirty and usable, so the user can
            // repeat it.
            Err(error) => {
                self.set_message(
                    format!("cannot save {}: {error}", requested.display()),
                    MessageLevel::Error,
                );
                return Redraw::Needed;
            }
        };
        let Some(target) = self.buffers.get_mut(buffer) else {
            // The buffer left the list while the save ran.
            return Redraw::Skipped;
        };
        let lines = target.text().line_count();
        let name = saved.path.display().to_string();
        let bytes = saved.bytes;
        target.mark_saved(saved.path, saved.identity);
        self.set_message(
            format!("\"{name}\" {lines}L, {bytes}B written"),
            MessageLevel::Info,
        );
        match then {
            AfterSave::Stay => Redraw::Needed,
            AfterSave::CloseWindow => self
                .close_window(UnsavedChanges::Discard)
                .or(Redraw::Needed),
        }
    }

    /// Closes the focused window and ends the editor after the last window.
    fn close_window(&mut self, unsaved: UnsavedChanges) -> Redraw {
        let last_window = self.windows.window_count() == 1;
        if last_window && unsaved == UnsavedChanges::Refuse && self.active_buffer().is_modified() {
            self.set_message(UNSAVED_QUIT_NOTE, MessageLevel::Error);
            return Redraw::Needed;
        }
        match self.windows.apply(Command::CloseWindow) {
            WindowOutcome::LastWindow => {
                self.run = RunState::Finished;
                Redraw::Needed
            }
            WindowOutcome::Changed => Redraw::Needed,
            WindowOutcome::Ignored | WindowOutcome::Unchanged => Redraw::Skipped,
        }
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

    /// Closes the open prompt and restores the editor mode.
    ///
    /// A picker owns its own prompt, so the closed prompt also closes the
    /// picker. The editor state below the overlay never changed, so the
    /// previous view returns exactly as it was.
    fn close_prompt(&mut self) {
        self.prompt = None;
        self.picker = None;
        self.sync_context();
    }

    /// Moves input back to the scope that holds the focus.
    fn sync_context(&mut self) {
        if self.prompt.is_some() {
            return;
        }
        self.resolver.set_context(self.input_scope().context());
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
        self.message = Some(Message::new(text, level));
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
    fn refresh_search(&mut self) {
        let Some(active) = self.buffers.get(self.active) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return;
        };
        let version = active.text().version();
        let Some(search) = self.search.as_mut() else {
            return;
        };
        if search.version == version {
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
            .filter(|region| region.kind == RegionKind::Editor)
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

/// The message that a refused quit shows.
const UNSAVED_QUIT_NOTE: &str = "the buffer holds unsaved changes; use :q! to discard them";

/// The message that a refused unload shows.
const UNSAVED_UNLOAD_NOTE: &str = "the buffer holds unsaved changes; save it before the unload";

/// The message that a save without a file name shows.
const NO_FILE_NAME_NOTE: &str = "the buffer holds no file name; use :e <path> to name one";

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

/// The title band of the hover float.
const HOVER_TITLE: &str = " Hover ";

/// The title band of the diagnostic float.
const DIAGNOSTIC_TITLE: &str = " Diagnostics ";
const _BUILD_PROBE_1: u32 = 1;
const _BUILD_PROBE_2: u32 = 2;
