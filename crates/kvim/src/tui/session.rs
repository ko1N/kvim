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

use crate::core::{BufferVersion, CharPosition, EditTransaction, TextBuffer};
use crate::editor::{
    AutoIndent, CommandContext, CommandOutcome, EditContext, EditingState, Registers,
    SEARCH_QUERY_CHARS_MAX, SearchDirection, SearchQuery, Viewport,
};
use crate::input::{
    COMMAND_LINE_CHARS_MAX, Command, CommandLineCommand, InputContext, Mode, PromptEdit,
    PromptKind, Registry, Resolution, Resolver, WhichKeyRow,
};
use crate::language::{
    Analysis, AnalysisError, AnalysisInput, BufferSyntax, HighlightSpan, LanguageAdapter,
    LanguageRegistry, Publication, SyntaxTree,
};
use crate::settings::EditorSettings;
use crate::terminal::{Chord, Key, KeyCode, TerminalEvent};
use crate::workspace::{
    BUFFERS_MAX, BufferId, Buffers, FileBuffer, FileRequest, FileResult, OpenRequest, OpenedFile,
    SaveError, SaveRequest, SavedBuffer, render_content,
};

use super::buffer_view::{WINBAR_ROWS, gutter_cells};
use super::chrome::shell_areas;
use super::layout::RegionKind;
use super::theme::Theme;
use super::window::{WindowId, WindowOutcome, Windows};

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

/// The step that follows one successful save.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AfterSave {
    /// Keep the window open.
    Stay,
    /// Close the focused window, like `:wq`.
    CloseWindow,
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
    /// Every loaded buffer, because each window shows its own buffer.
    pub(super) buffers: &'a Buffers,
    /// The buffer that the editing state and the active search belong to.
    pub(super) active: BufferId,
    pub(super) analysis: &'a BTreeMap<BufferId, BufferAnalysis>,
    pub(super) editing: &'a EditingState,
    pub(super) search: Option<&'a ActiveSearch>,
    pub(super) prompt: Option<&'a PromptLine>,
    pub(super) message: Option<&'a Message>,
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
/// use kvim::settings::EditorSettings;
/// use kvim::terminal::{Key, KeyCode, TerminalEvent};
/// use kvim::tui::{Redraw, Session};
///
/// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default());
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
    editing: EditingState,
    registers: Registers,
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
    search: Option<ActiveSearch>,
    prompt: Option<PromptLine>,
    message: Option<Message>,
    which_key: Option<Vec<WhichKeyRow>>,
    run: RunState,
}

impl Session {
    /// Creates a session that shows one empty scratch buffer.
    ///
    /// # Panics
    ///
    /// Panics when the hardcoded first-release binding table is invalid. This
    /// is a cold-path bootstrap check, so an invalid table must fail at start.
    #[must_use]
    pub fn new(area: Rect, settings: EditorSettings) -> Self {
        let (buffers, active) = Buffers::new(FileBuffer::scratch(&settings.files));
        let editing = EditingState::new(
            buffers
                .get(active)
                .expect("the new buffer list holds its first buffer")
                .text(),
        );
        let mut session = Self {
            area,
            settings,
            theme: Theme::new(settings.theme),
            buffers,
            active,
            file_outbox: None,
            file_pending: None,
            windows: Windows::new(active, shell_areas(area).body, settings.windows),
            editing,
            registers: Registers::default(),
            resolver: Resolver::new(Registry::first_release(), settings.input),
            languages: LanguageRegistry::first_release(),
            analysis: BTreeMap::new(),
            analysis_pending: None,
            search: None,
            prompt: None,
            message: None,
            which_key: None,
            run: RunState::Running,
        };
        session.reconcile_viewports();
        session
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
            buffers: &self.buffers,
            active: self.active,
            analysis: &self.analysis,
            editing: &self.editing,
            search: self.search.as_ref(),
            prompt: self.prompt.as_ref(),
            message: self.message.as_ref(),
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
        let rows = self.resolver.which_key(now);
        if rows.as_deref() == self.which_key.as_deref() {
            return Redraw::Skipped;
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
        match command {
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
            _ => {}
        }
        match self.windows.apply(command) {
            WindowOutcome::Ignored => {}
            WindowOutcome::Changed => {
                self.follow_focused_window();
                return Redraw::Needed;
            }
            WindowOutcome::Unchanged => return cleared,
            WindowOutcome::LastWindow => {
                self.run = RunState::Finished;
                return cleared;
            }
        }
        if let Some(note) = deferred_note(command) {
            self.set_message(note, MessageLevel::Warning);
            return Redraw::Needed;
        }
        let auto = self.auto_indent(command);
        let outcome = self.edit(|editing, context, viewport| {
            editing.apply_indented(context, viewport, command, count, auto)
        });
        self.sync_context();
        self.report(outcome).or(cleared)
    }

    /// Points the editing state at the buffer of the focused window.
    ///
    /// A focus change, a split, and a close all move the focus. The editing
    /// state follows one buffer, so it must follow that move. Otherwise a key
    /// would change a buffer that the focused window does not show.
    fn follow_focused_window(&mut self) {
        let window = self.windows.focused_window();
        let Some(buffer) = self.windows.buffer(window) else {
            debug_assert!(false, "the focused window is always a leaf of the tree");
            return;
        };
        self.switch_to(buffer);
    }

    /// Returns the automatic indent that one command uses for a new line.
    ///
    /// Only `o`, `O`, and a repeat of either read the value. The session asks
    /// the accepted analysis of the current buffer version, and falls back to
    /// the previous-line rule when no result answers. The editor never waits
    /// for a parse result. See `docs/language-services.md`.
    fn auto_indent(&self, command: Command) -> AutoIndent {
        let buffer = self.buffer();
        let line = self.editing.cursor().line();
        let byte = match command {
            // The new line opens after the text of the cursor line.
            Command::OpenLineBelow => {
                let end = buffer.line_len_chars(line);
                let Ok(column) = buffer.source_column(line, end) else {
                    return AutoIndent::PreviousLine;
                };
                buffer
                    .char_to_byte(buffer.column_to_char(line, column))
                    .get()
            }
            // The new line opens before the text of the cursor line.
            Command::OpenLineAbove => buffer.char_to_byte(buffer.line_start(line)).get(),
            // Every other command ignores the value, and a repeat re-reads it
            // through the command that it replays.
            _ => return AutoIndent::PreviousLine,
        };
        self.indent_level(byte)
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
        let outcome = self
            .edit(|editing, context, viewport| editing.toggle_comment(context, viewport, comment));
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
        F: FnOnce(&mut EditingState, &mut EditContext<'_>, &mut Viewport) -> CommandOutcome,
    {
        let window = self.windows.focused_window();
        let Some(mut viewport) = self.windows.viewport(window) else {
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
        let outcome = change(&mut self.editing, &mut context, &mut viewport);
        let applied = std::mem::take(&mut context.applied);
        let after = context.buffer.version();
        if let Some(slot) = self.windows.viewport_mut(window) {
            *slot = viewport;
        }
        self.advance_syntax(&before, after, &applied);
        outcome
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
                let byte = buffer
                    .char_to_byte(self.editing.cursor().position(buffer))
                    .get();
                let auto = self.indent_level(byte);
                self.edit(|editing, context, viewport| {
                    editing.insert_line_break_indented(context, viewport, auto)
                })
            }
            KeyCode::Backspace => {
                self.edit(|editing, context, viewport| editing.delete_backward(context, viewport))
            }
            KeyCode::Char(value) => {
                let text = value.to_string();
                self.edit(|editing, context, viewport| {
                    editing.insert_text(context, viewport, &text)
                })
            }
            KeyCode::Tab => {
                let text = if indent.expand_tab {
                    " ".repeat(usize::from(indent.tab_width.get()))
                } else {
                    "\t".to_owned()
                };
                self.edit(|editing, context, viewport| {
                    editing.insert_text(context, viewport, &text)
                })
            }
            _ => return Redraw::Skipped,
        };
        self.report(outcome)
    }

    /// Opens one line prompt and moves input to it.
    fn open_prompt(&mut self, kind: PromptKind) -> Redraw {
        self.prompt = Some(PromptLine {
            kind,
            text: String::new(),
        });
        let context = InputContext::Mode(self.editing.mode()).open_prompt(kind);
        self.resolver.set_context(context);
        Redraw::Needed
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
                Redraw::Needed
            }
            PromptEdit::DeleteBackward => {
                // Backspace on the empty line cancels the prompt, like Vim.
                if prompt.text.pop().is_none() {
                    self.close_prompt();
                }
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
        self.close_prompt();
        match prompt.kind {
            PromptKind::CommandLine => self.run_command_line(&prompt.text),
            PromptKind::Search => self.run_search(&prompt.text),
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
                let Some(active) = self.buffers.get(self.active) else {
                    debug_assert!(false, "the session always keeps the active buffer loaded");
                    return Redraw::Skipped;
                };
                self.editing.move_to(active.text(), target - 1, 0);
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
    /// use kvim::settings::EditorSettings;
    /// use kvim::tui::Session;
    ///
    /// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default());
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

    /// Saves the active buffer and runs the step that follows the save.
    fn save_active(&mut self, then: AfterSave) -> Redraw {
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
        self.set_message(format!("\"{name}\" {lines}L, {bytes}B"), MessageLevel::Info);
        redraw.or(Redraw::Needed)
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
        self.buffers.remove(id);
        for window in self.windows.window_ids() {
            if self.windows.buffer(window) == Some(id) {
                self.windows.set_buffer(window, next);
            }
        }
        let redraw = self.switch_to(next);
        self.set_message("the buffer is unloaded", MessageLevel::Info);
        redraw.or(Redraw::Needed)
    }

    /// Shows one loaded buffer in the focused window.
    fn switch_to(&mut self, id: BufferId) -> Redraw {
        let window = self.windows.focused_window();
        self.windows.set_buffer(window, id);
        if self.active == id {
            return Redraw::Skipped;
        }
        let Some(active) = self.buffers.get(id) else {
            debug_assert!(false, "a caller switches only to a loaded buffer");
            return Redraw::Skipped;
        };
        self.active = id;
        self.editing = EditingState::new(active.text());
        // The recorded matches belong to the previous buffer.
        self.search = None;
        self.reconcile_viewports();
        Redraw::Needed
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
        let Some(mut viewport) = self.windows.viewport(window) else {
            debug_assert!(false, "the layout always keeps the focused window visible");
            return Redraw::Needed;
        };
        let context = CommandContext {
            buffer: active.text(),
            settings: &self.settings,
            search: Some(&query),
        };
        let outcome = self.editing.search(&context, &mut viewport, &query);
        if let Some(slot) = self.windows.viewport_mut(window) {
            *slot = viewport;
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
    fn close_prompt(&mut self) {
        self.prompt = None;
        self.sync_context();
    }

    /// Moves input back to the active editor mode.
    fn sync_context(&mut self) {
        if self.prompt.is_some() {
            return;
        }
        self.resolver
            .set_context(InputContext::Mode(self.editing.mode()));
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

    /// Resizes every visible viewport to its text area and follows the cursor.
    ///
    /// The window tree sizes a viewport to the complete window rectangle,
    /// because it holds no buffer and no settings. The session knows the winbar
    /// row and the gutter width, so it publishes the real text area here. The
    /// scroll margin then applies to the cells that the reader actually sees.
    fn reconcile_viewports(&mut self) {
        let display = self.settings.display;
        let cursor = self.editing.cursor();
        let Some(active) = self.buffers.get(self.active) else {
            debug_assert!(false, "the session always keeps the active buffer loaded");
            return;
        };
        let text = active.text();
        let sizes: Vec<(WindowId, u16, u16)> = self
            .windows
            .layout()
            .regions()
            .iter()
            .filter(|region| region.kind == RegionKind::Editor)
            .map(|region| {
                let gutter = gutter_cells(text, &display, region.area.width);
                (
                    region.id,
                    region.area.width.saturating_sub(gutter),
                    region.area.height.saturating_sub(WINBAR_ROWS),
                )
            })
            .collect();
        for (id, width, height) in sizes {
            let width = NonZeroU16::new(width).unwrap_or(NonZeroU16::MIN);
            let height = NonZeroU16::new(height).unwrap_or(NonZeroU16::MIN);
            let Some(slot) = self.windows.viewport_mut(id) else {
                continue;
            };
            *slot = slot
                .resized(height, width)
                .reconciled(text, cursor, &display);
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

/// Returns the message of a command that a later slice implements.
///
/// The first release binds every key of `docs/input-actions.md`, so a key that
/// reaches unfinished behavior must say so instead of doing nothing.
const fn deferred_note(command: Command) -> Option<&'static str> {
    match command {
        Command::RevealInFileTree => Some("the file tree arrives in a later release"),
        Command::OpenBufferPicker | Command::OpenFilePicker | Command::OpenRipgrepPicker => {
            Some("the pickers arrive in a later release")
        }
        Command::GoToDefinition
        | Command::ShowHover
        | Command::ShowDiagnosticFloat
        | Command::NextDiagnostic
        | Command::PreviousDiagnostic
        | Command::ToggleFormatOnSave => Some("the language services arrive in a later release"),
        _ => None,
    }
}
