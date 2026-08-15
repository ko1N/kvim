//! The visible editor state and the pure transitions of the event loop.
//!
//! [`Session`] owns every value that the terminal shows: the buffer, the window
//! tree, the editing state, the input resolver, the active search, the open
//! prompt, and the last message. It performs no filesystem work, no process
//! work, and no language work, so the event loop stays inside the latency
//! budget of `docs/responsiveness.md`.
//!
//! The session reads no clock. The event loop measures the elapsed time and
//! passes it in, which keeps every transition deterministic and testable.

use std::num::NonZeroU16;
use std::num::NonZeroU32;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::core::{BufferVersion, CharPosition, TextBuffer};
use crate::editor::{
    CommandContext, CommandOutcome, EditContext, EditingState, Registers, SEARCH_QUERY_CHARS_MAX,
    SearchDirection, SearchQuery, Viewport,
};
use crate::input::{
    COMMAND_LINE_CHARS_MAX, Command, CommandLineCommand, Expiry, InputContext, Mode, PromptEdit,
    PromptKind, Registry, Resolution, Resolver, WhichKeyRow,
};
use crate::settings::EditorSettings;
use crate::terminal::{Chord, Key, KeyCode, TerminalEvent};

use super::buffer_view::{WINBAR_ROWS, gutter_cells};
use super::chrome::shell_areas;
use super::layout::RegionKind;
use super::theme::Theme;
use super::window::{BufferId, WindowId, WindowOutcome, Windows};

/// The largest message that the message line keeps, in characters.
///
/// Every message comes from a bounded label or from a typed error, so the bound
/// only protects the line against an unexpectedly long path.
pub const MESSAGE_CHARS_MAX: usize = 512;

/// The name of the buffer that Kvim opens without a file argument.
///
/// Slice 9 adds file loading and replaces this name with the file path.
const SCRATCH_BUFFER_NAME: &str = "[Scratch]";

/// The identity of the one buffer of this release.
const SCRATCH_BUFFER_ID: BufferId = BufferId::new(1);

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
    pub(super) buffer: &'a TextBuffer,
    pub(super) name: &'a str,
    pub(super) editing: &'a EditingState,
    pub(super) search: Option<&'a ActiveSearch>,
    pub(super) prompt: Option<&'a PromptLine>,
    pub(super) message: Option<&'a Message>,
    pub(super) which_key: Option<&'a [WhichKeyRow]>,
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
    buffer: TextBuffer,
    name: String,
    windows: Windows,
    editing: EditingState,
    registers: Registers,
    resolver: Resolver,
    search: Option<ActiveSearch>,
    prompt: Option<PromptLine>,
    message: Option<Message>,
    which_key: Option<Vec<WhichKeyRow>>,
    /// The elapsed time at which the which-key overlay appears.
    ///
    /// The resolver arms the same deadline from the same delay, so the event
    /// loop wakes exactly when the overlay becomes visible.
    which_key_at: Option<Duration>,
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
        let buffer = TextBuffer::from_text("", &settings.files)
            .expect("an empty buffer never passes the file size limit");
        let editing = EditingState::new(&buffer);
        let mut session = Self {
            area,
            settings,
            theme: Theme::new(settings.theme),
            buffer,
            name: SCRATCH_BUFFER_NAME.to_owned(),
            windows: Windows::new(SCRATCH_BUFFER_ID, shell_areas(area).body, settings.windows),
            editing,
            registers: Registers::default(),
            resolver: Resolver::new(Registry::first_release(), settings.input),
            search: None,
            prompt: None,
            message: None,
            which_key: None,
            which_key_at: None,
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

    /// Returns the buffer that every window shows.
    #[must_use]
    pub const fn buffer(&self) -> &TextBuffer {
        &self.buffer
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
    /// The event loop waits for a terminal event or for this deadline, never
    /// for a fixed frame interval.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        let overlay = self.which_key_at.filter(|_| self.which_key.is_none());
        match (self.resolver.deadline(), overlay) {
            (Some(first), Some(second)) => Some(first.min(second)),
            (first, second) => first.or(second),
        }
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

    /// Applies the state changes that an expired deadline causes.
    pub fn tick(&mut self, now: Duration) -> Redraw {
        let expiry = match self.resolver.expire(now) {
            Expiry::Expired => Redraw::Needed,
            Expiry::Unchanged => Redraw::Skipped,
        };
        self.settle(now).or(expiry)
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
            buffer: &self.buffer,
            name: &self.name,
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
        // The overlay deadline belongs to a pending key sequence. A completed,
        // mismatched, or expired sequence removes it.
        if self.resolver.pending_keys().is_empty() {
            self.which_key_at = None;
        }
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
            Resolution::Pending => {
                self.which_key_at = Some(saturating_deadline(
                    now,
                    self.settings.input.which_key_delay,
                ));
                Redraw::Skipped
            }
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
            _ => {}
        }
        match self.windows.apply(command) {
            WindowOutcome::Ignored => {}
            WindowOutcome::Changed => return Redraw::Needed,
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
        let outcome = self
            .edit(|editing, context, viewport| editing.apply(context, viewport, command, count));
        self.sync_context();
        self.report(outcome).or(cleared)
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
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: self.search.as_ref().map(|search| &search.query),
            registers: &mut self.registers,
        };
        let outcome = change(&mut self.editing, &mut context, &mut viewport);
        if let Some(slot) = self.windows.viewport_mut(window) {
            *slot = viewport;
        }
        outcome
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
                self.edit(|editing, context, viewport| editing.insert_line_break(context, viewport))
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
            // Slice 9 owns file loading and saving. The command line reaches the
            // same paths as the bound keys, so both report the same limit.
            CommandLineCommand::Write | CommandLineCommand::WriteQuit => {
                self.set_message(SAVE_NOTE, MessageLevel::Warning);
            }
            CommandLineCommand::Edit(path) => {
                self.set_message(
                    format!("cannot open {}; {OPEN_NOTE}", path.display()),
                    MessageLevel::Warning,
                );
            }
            CommandLineCommand::Quit | CommandLineCommand::QuitDiscard => {
                return self.apply_command(Command::CloseWindow, None);
            }
            CommandLineCommand::GoToLine(line) => {
                let target = usize::try_from(line.get()).unwrap_or(usize::MAX);
                self.editing.move_to(&self.buffer, target - 1, 0);
            }
        }
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
        let matches = query.matches(&self.buffer, &self.settings.search);
        let version = self.buffer.version();
        let window = self.windows.focused_window();
        let Some(mut viewport) = self.windows.viewport(window) else {
            debug_assert!(false, "the layout always keeps the focused window visible");
            return Redraw::Needed;
        };
        let context = CommandContext {
            buffer: &self.buffer,
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
        let version = self.buffer.version();
        let Some(search) = self.search.as_mut() else {
            return;
        };
        if search.version == version {
            return;
        }
        search.matches = search.query.matches(&self.buffer, &self.settings.search);
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
        let sizes: Vec<(WindowId, u16, u16)> = self
            .windows
            .layout()
            .regions()
            .iter()
            .filter(|region| region.kind == RegionKind::Editor)
            .map(|region| {
                let gutter = gutter_cells(&self.buffer, &display, region.area.width);
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
                .reconciled(&self.buffer, cursor, &display);
        }
    }
}

/// The message that every deferred file and buffer command reports.
const SAVE_NOTE: &str = "saving arrives in a later release";

/// The message that every deferred open command reports.
const OPEN_NOTE: &str = "file opening arrives in a later release";

/// Returns the message of a command that a later slice implements.
///
/// The first release binds every key of `docs/input-actions.md`, so a key that
/// reaches unfinished behavior must say so instead of doing nothing.
const fn deferred_note(command: Command) -> Option<&'static str> {
    match command {
        Command::SaveBuffer => Some(SAVE_NOTE),
        Command::UnloadBuffer => Some("buffer management arrives in a later release"),
        Command::RevealInFileTree => Some("the file tree arrives in a later release"),
        Command::OpenBufferPicker | Command::OpenFilePicker | Command::OpenRipgrepPicker => {
            Some("the pickers arrive in a later release")
        }
        Command::ToggleComment => Some("comment toggling arrives in a later release"),
        Command::GoToDefinition
        | Command::ShowHover
        | Command::ShowDiagnosticFloat
        | Command::NextDiagnostic
        | Command::PreviousDiagnostic
        | Command::ToggleFormatOnSave => Some("the language services arrive in a later release"),
        _ => None,
    }
}

/// Adds a delay to the elapsed time without overflow.
fn saturating_deadline(now: Duration, delay: Duration) -> Duration {
    now.checked_add(delay).unwrap_or(Duration::MAX)
}
