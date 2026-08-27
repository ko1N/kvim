//! The editing state and the one command dispatcher of the editor.
//!
//! [`EditingState`] owns the mode, the operator-pending state, the pending
//! block insert, and the last repeatable change. [`WindowState`] owns the
//! cursor, the selection anchor, and the viewport, because one window owns its
//! view into a buffer. Every command therefore receives the window that the keys
//! act on, and a move in one window never moves another window. The mode stays
//! global, as it is in Vim. See `docs/windows.md`.
//!
//! [`EditingState`] executes every motion, selection, search, viewport,
//! operator, register, paste, and repeat command that `input` names.
//!
//! Every text change leaves this module as one [`EditTransaction`], staged as an
//! [`EditPlan`] and committed in one step. The module reads no clock, no
//! filesystem, and no process. The system clipboard stays outside: the caller
//! passes the register value in and out. See `docs/clipboard.md`.

use std::num::{NonZeroU8, NonZeroU32};

use kvim_core::{EditError, EditTransaction, IndentPolicy, ShiftDirection, TextBuffer};
use kvim_input::{Command, Mode};
use kvim_settings::{COUNT_MAX, EditorSettings};

use super::cursor::{ColumnLimit, Cursor};
use super::edit::{
    self, AutoIndent, BlockEdge, CursorTarget, EditPlan, MoveDirection, NextMode, OpenDirection,
    PastePlacement, PendingBlockInsert,
};
use super::motion;
use super::operator::{MotionKind, Operator, OperatorRange, motion_kind, plan_operator};
use super::register::Registers;
use super::search::{SearchDirection, SearchQuery};
use super::selection::{AnchorPoint, ModeState, Selection};
use super::text_object::TextObject;
use super::viewport::ViewportAlignment;
use super::window::WindowState;

/// The largest number of repetitions that one motion performs.
///
/// The value is the count maximum of the input resolver, so a motion cannot
/// repeat more often than the resolver accepts.
pub const MOTION_COUNT_MAX: usize = COUNT_MAX as usize;

/// The largest text that one Insert session hands to the editor, in bytes.
///
/// The bound keeps one insert transaction and its history entry bounded, and it
/// keeps a block insert from multiplying an unbounded text over every selected
/// line.
pub const INSERT_TEXT_BYTES_MAX: usize = 64 * 1024;

/// Everything that one read-only command reads beside the editing state.
///
/// The context holds borrowed values only, so the caller keeps the buffer, the
/// settings, and the active search query.
#[derive(Clone, Copy, Debug)]
pub struct CommandContext<'a> {
    /// The buffer that the window shows.
    pub buffer: &'a TextBuffer,
    /// The active editor settings.
    pub settings: &'a EditorSettings,
    /// The query of the last search, when the user ran one.
    pub search: Option<&'a SearchQuery>,
}

/// Everything that one command reads and changes beside the editing state.
///
/// The dispatcher needs the buffer and the registers as mutable values, because
/// an operator changes text and writes the unnamed register.
#[derive(Debug)]
pub struct EditContext<'a> {
    /// The buffer that the window shows.
    pub buffer: &'a mut TextBuffer,
    /// The active editor settings.
    pub settings: &'a EditorSettings,
    /// The query of the last search, when the user ran one.
    pub search: Option<&'a SearchQuery>,
    /// The number of cells that one indent level takes in the language of the
    /// buffer.
    ///
    /// Only a language adapter knows the width of its language, and this module
    /// names no adapter, so the caller resolves the value and passes it in,
    /// exactly as it passes the comment token. `None` means that no adapter
    /// serves the buffer, so the settings width applies. See
    /// `docs/settings.md`.
    pub language_indent_width: Option<NonZeroU8>,
    /// The registers of the editor session.
    pub registers: &'a mut Registers,
}

impl EditContext<'_> {
    /// Borrows the read-only part of the context.
    #[must_use]
    pub fn read(&self) -> CommandContext<'_> {
        CommandContext {
            buffer: self.buffer,
            settings: self.settings,
            search: self.search,
        }
    }

    fn indent(&self) -> IndentPolicy {
        let settings = &self.settings.indent;
        IndentPolicy::new(
            settings.expand_tab,
            settings.tab_width,
            settings.indent_columns(self.language_indent_width),
        )
    }
}

/// The typed result and incremental effect of one command.
///
/// One command applies at most one transaction. Undo and redo change text but
/// return no incremental transaction because they replay buffer history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandResult {
    outcome: CommandOutcome,
    transaction: Option<EditTransaction>,
}

impl CommandResult {
    fn new(outcome: CommandOutcome, transaction: Option<EditTransaction>) -> Self {
        debug_assert!(
            transaction.is_none() || outcome == CommandOutcome::Changed,
            "only a changed command can expose its newly applied transaction"
        );
        Self {
            outcome,
            transaction,
        }
    }

    /// Returns the semantic outcome of the command.
    #[must_use]
    pub const fn outcome(&self) -> CommandOutcome {
        self.outcome
    }

    /// Returns the transaction applied by the command, when incremental reuse
    /// can consume it.
    #[must_use]
    pub const fn transaction(&self) -> Option<&EditTransaction> {
        self.transaction.as_ref()
    }
}

/// The result of one command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutcome {
    /// The command changed the buffer text.
    Changed,
    /// The command changed the cursor, the mode, the register, or the viewport.
    Applied,
    /// The operator waits for a motion, a text object, or a repeated operator
    /// key.
    OperatorPending,
    /// The operator or the Visual selection received no target, so it changed
    /// nothing.
    OperatorAborted,
    /// The command pastes, but the named register holds no value.
    RegisterEmpty,
    /// The command undoes or redoes, but the history holds no further step.
    HistoryExhausted,
    /// The command names a search or a bracket jump, but no query is active and
    /// no match exists.
    SearchMissed,
    /// The input passes a bound of this module.
    Rejected,
    /// The command names no behavior of this module.
    Unhandled,
}

/// The pending operator and the count that arrived before it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingOperator {
    operator: Operator,
    count: Option<NonZeroU32>,
}

/// The description of the last change that `.` replays.
///
/// The editor replays the description, never the recorded result, which
/// `docs/text-model.md` requires.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepeatableChange {
    /// One command that changed text on its own.
    Command {
        command: Command,
        count: Option<NonZeroU32>,
    },
    /// One operator that a motion completed.
    OperatorMotion {
        operator: Operator,
        count: Option<NonZeroU32>,
        motion: Command,
        motion_count: Option<NonZeroU32>,
    },
}

/// The result of one motion lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MotionResult {
    /// The motion produced this cursor.
    Moved(Cursor),
    /// The command names a search that found no match.
    Missed,
    /// The command is not a motion.
    NotAMotion,
}

/// The mode, the operator-pending state, and the repeat description.
///
/// The cursor, the selection anchor, and the viewport belong to one window, so
/// [`WindowState`] holds them and every command receives the window that the
/// keys act on. The mode is global, as it is in Vim. See `docs/windows.md`.
///
/// # Examples
///
/// ```
/// use std::num::NonZeroU16;
///
/// use kvim_core::TextBuffer;
/// use kvim_editor::{
///     CommandOutcome, EditContext, EditingState, Registers, Viewport, WindowState,
/// };
/// use kvim_input::{Command, Mode};
/// use kvim_settings::{EditorSettings, FileSettings};
///
/// let mut buffer = TextBuffer::from_text("alpha beta\ngamma\n", kvim_core::BufferBytesMax::default())
///     .expect("the text is small");
/// let settings = EditorSettings::default();
/// let mut registers = Registers::default();
/// let mut context = EditContext {
///     buffer: &mut buffer,
///     settings: &settings,
///     search: None,
///     language_indent_width: None,
///     registers: &mut registers,
/// };
///
/// let rows = NonZeroU16::new(10).expect("the literal 10 is not zero");
/// let cells = NonZeroU16::new(80).expect("the literal 80 is not zero");
/// let mut window = WindowState::new(Viewport::new(rows, cells));
/// let mut state = EditingState::new();
///
/// // `d` waits for a motion, and `w` completes it.
/// let pending = state.apply(&mut context, &mut window, Command::DeleteOverMotion, None);
/// assert_eq!(pending.outcome(), CommandOutcome::OperatorPending);
/// let changed = state.apply(&mut context, &mut window, Command::MoveNextWordStart, None);
/// assert_eq!(changed.outcome(), CommandOutcome::Changed);
/// assert_eq!(context.buffer.to_string(), "beta\ngamma\n");
///
/// // One undo reverses the complete change.
/// state.apply(&mut context, &mut window, Command::Undo, None);
/// assert_eq!(context.buffer.to_string(), "alpha beta\ngamma\n");
/// assert_eq!(state.mode(), Mode::Normal);
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EditingState {
    mode: Mode,
    pending: Option<PendingOperator>,
    block_insert: Option<PendingBlockInsert>,
    repeat: Option<RepeatableChange>,
    applied: Option<EditTransaction>,
    /// The register that qualifies the operation that is being composed.
    ///
    /// `"` and its name arrive with the first command of the operation, and an
    /// operator reads its motion afterwards, so the value stays until the
    /// operation completes.
    register: Option<char>,
}

impl EditingState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the active mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Returns the active mode together with the anchor of one window.
    #[must_use]
    pub fn mode_state(&self, buffer: &TextBuffer, window: &WindowState) -> ModeState {
        ModeState::of(self.mode, buffer, window.anchor_point(buffer))
    }

    /// Returns the last column that the cursor may hold in the active mode.
    #[must_use]
    pub const fn column_limit(&self) -> ColumnLimit {
        ColumnLimit::of(self.mode)
    }

    /// Returns the operator that waits for a motion.
    #[must_use]
    pub fn pending_operator(&self) -> Option<Operator> {
        self.pending.map(|pending| pending.operator)
    }

    /// Returns the selection that one window shows in the active Visual mode.
    #[must_use]
    pub fn selection(&self, buffer: &TextBuffer, window: &WindowState) -> Option<Selection> {
        self.mode_state(buffer, window)
            .selection(buffer, window.cursor)
    }

    /// Places the cursor of one window at a line and a column.
    ///
    /// The command line `:<number>` and a language-service jump both use this
    /// entry point. The caller reconciles the viewport afterwards.
    pub fn move_to(
        &self,
        buffer: &TextBuffer,
        window: &mut WindowState,
        line: usize,
        column: usize,
    ) {
        window.cursor = Cursor::clamped(buffer, line, column, self.column_limit());
    }

    /// Changes the mode and derives the selection anchor of one window.
    ///
    /// A change from one Visual mode into another keeps the existing anchor, so
    /// the selection does not restart. Normal mode and Insert mode drop the
    /// anchor, because neither mode holds a selection. The cursor clamps again,
    /// because Insert mode allows one more column than the other modes.
    pub fn enter_mode(&mut self, buffer: &TextBuffer, window: &mut WindowState, mode: Mode) {
        // The mode and the anchor change together, so a Visual mode always
        // holds an anchor and no other mode ever does.
        window.anchor = match mode {
            Mode::Normal | Mode::Insert => None,
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock => {
                Some(window.anchor_point(buffer))
            }
        };
        self.mode = mode;
        window.cursor = window.cursor.re_clamped(buffer, self.column_limit());
    }

    /// Executes one semantic command with the previous-line automatic indent.
    ///
    /// The viewport follows the cursor after every accepted command, except an
    /// explicit alignment command, which overrides the scroll margin. Every text
    /// change applies as one edit transaction.
    ///
    /// A caller that holds a parse result for the current buffer version uses
    /// [`EditingState::apply_indented`] instead, so `o`, `O`, and a Visual
    /// selection move follow the syntax tree.
    pub fn apply(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        command: Command,
        count: Option<NonZeroU32>,
    ) -> CommandResult {
        self.apply_indented(context, window, command, count, AutoIndent::PreviousLine)
    }

    /// Executes one semantic command that a register name qualifies.
    ///
    /// The `input` charter resolves the name: `"` opens the selection, the next
    /// character names the register, and the completed operation carries that
    /// name. The name reaches the yank, the delete, the change, and the paste of
    /// that operation alone, so the next operation reads the unnamed register
    /// again.
    ///
    /// An operator receives the name with its own key and its target afterwards,
    /// so `"add` keeps the name until the operator completes.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::num::NonZeroU16;
    ///
    /// use kvim_core::TextBuffer;
    /// use kvim_editor::{
    ///     EditContext, EditingState, RegisterValue, Registers, Viewport, WindowState,
    /// };
    /// use kvim_input::Command;
    /// use kvim_settings::{EditorSettings, FileSettings};
    ///
    /// let mut buffer = TextBuffer::from_text("alpha\nbeta\n", kvim_core::BufferBytesMax::default())
    ///     .expect("the text is small");
    /// let settings = EditorSettings::default();
    /// let mut registers = Registers::default();
    /// let mut context = EditContext {
    ///     buffer: &mut buffer,
    ///     settings: &settings,
    ///     search: None,
    ///     language_indent_width: None,
    ///     registers: &mut registers,
    /// };
    ///
    /// let rows = NonZeroU16::new(10).expect("the literal 10 is not zero");
    /// let cells = NonZeroU16::new(80).expect("the literal 80 is not zero");
    /// let mut window = WindowState::new(Viewport::new(rows, cells));
    /// let mut state = EditingState::new();
    ///
    /// // `"ayy` yanks the first line into the register `a`.
    /// state.apply_with_register(&mut context, &mut window, Command::YankOverMotion, None, Some('a'));
    /// state.apply(&mut context, &mut window, Command::YankOverMotion, None);
    /// assert_eq!(
    ///     context.registers.value(Some('a')).map(RegisterValue::text),
    ///     Some("alpha\n"),
    /// );
    /// ```
    pub fn apply_with_register(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        command: Command,
        count: Option<NonZeroU32>,
        register: Option<char>,
    ) -> CommandResult {
        self.apply_indented_with_register(
            context,
            window,
            command,
            count,
            AutoIndent::PreviousLine,
            register,
        )
    }

    /// Executes one qualified command with an explicit automatic indent.
    ///
    /// The pair of [`EditingState::apply`] and [`EditingState::apply_indented`]
    /// repeats here, so a caller that holds a parse result can also name a
    /// register.
    pub fn apply_indented_with_register(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        command: Command,
        count: Option<NonZeroU32>,
        auto: AutoIndent,
        register: Option<char>,
    ) -> CommandResult {
        if register.is_some() {
            self.register = register;
        }
        self.apply_indented(context, window, command, count, auto)
    }

    /// Executes one semantic command with an explicit automatic indent.
    ///
    /// Only `o`, `O`, and a Visual selection move read the indent. Every other
    /// command ignores it. A selection move needs the level of the line that it
    /// lands behind, which
    /// [`selection_move_indent_line`](crate::selection_move_indent_line) names.
    pub fn apply_indented(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        command: Command,
        count: Option<NonZeroU32>,
        auto: AutoIndent,
    ) -> CommandResult {
        self.applied = None;
        let outcome = self.dispatch(context, window, command, count, auto);
        // A register qualifies exactly one operation. A waiting operator holds
        // the operation open, so the name survives until its target arrives.
        if outcome != CommandOutcome::OperatorPending {
            self.register = None;
        }
        CommandResult::new(outcome, self.applied.take())
    }

    /// Executes one semantic command without the register lifetime.
    fn dispatch(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        command: Command,
        count: Option<NonZeroU32>,
        auto: AutoIndent,
    ) -> CommandOutcome {
        if let Some(pending) = self.pending.take() {
            return self.complete_operator(context, window, pending, command, count);
        }

        let motion = self.motion_target(&context.read(), window, command, count);
        match motion {
            MotionResult::Moved(cursor) => {
                window.cursor = cursor;
                self.reconcile(context, window);
                return CommandOutcome::Applied;
            }
            MotionResult::Missed => return CommandOutcome::SearchMissed,
            MotionResult::NotAMotion => {}
        }

        // Without a waiting operator a text object names the Visual selection.
        if let Some(object) = TextObject::of_command(command) {
            return self.select_text_object(context, window, object, count);
        }

        let repeat = repeat_count(count);
        match command {
            // Modes.
            Command::ReturnToNormal => {
                self.block_insert = None;
                self.enter_mode(context.buffer, window, Mode::Normal);
            }
            Command::EnterVisual => self.enter_mode(context.buffer, window, Mode::Visual),
            Command::EnterVisualLine => self.enter_mode(context.buffer, window, Mode::VisualLine),
            Command::EnterVisualBlock => self.enter_mode(context.buffer, window, Mode::VisualBlock),

            // Insert entry. Each command also places the cursor for the change
            // that follows it.
            Command::InsertBeforeCursor => self.enter_mode(context.buffer, window, Mode::Insert),
            Command::InsertAtFirstNonBlank => {
                self.enter_mode(context.buffer, window, Mode::Insert);
                window.cursor = motion::move_first_non_blank(
                    context.buffer,
                    window.cursor,
                    self.column_limit(),
                );
            }
            Command::InsertAfterCursor => {
                self.enter_mode(context.buffer, window, Mode::Insert);
                window.cursor =
                    motion::move_right(context.buffer, window.cursor, self.column_limit(), 1);
            }
            Command::InsertAtLineEnd => {
                self.enter_mode(context.buffer, window, Mode::Insert);
                window.cursor = Cursor::clamped(
                    context.buffer,
                    window.cursor.line().get(),
                    usize::MAX,
                    self.column_limit(),
                );
            }
            Command::OpenLineBelow => {
                return self.open_line(context, window, command, count, OpenDirection::Below, auto);
            }
            Command::OpenLineAbove => {
                return self.open_line(context, window, command, count, OpenDirection::Above, auto);
            }

            // Viewport alignment. An alignment changes no cursor position.
            Command::CenterCursorLine => {
                window.viewport = window
                    .viewport
                    .aligned(window.cursor, ViewportAlignment::Center);
                return CommandOutcome::Applied;
            }
            Command::AlignCursorLineTop => {
                window.viewport = window
                    .viewport
                    .aligned(window.cursor, ViewportAlignment::Top);
                return CommandOutcome::Applied;
            }
            Command::AlignCursorLineBottom => {
                window.viewport = window
                    .viewport
                    .aligned(window.cursor, ViewportAlignment::Bottom);
                return CommandOutcome::Applied;
            }

            // Operators.
            Command::DeleteOverMotion | Command::DeleteSelection => {
                return self.start_operator(context, window, Operator::Delete, count);
            }
            Command::ChangeOverMotion | Command::ChangeSelection => {
                return self.start_operator(context, window, Operator::Change, count);
            }
            Command::YankOverMotion | Command::YankSelection => {
                return self.start_operator(context, window, Operator::Yank, count);
            }
            Command::DeleteLine => {
                return self.line_operator(context, window, Operator::Delete, repeat);
            }
            Command::ChangeLine => {
                return self.line_operator(context, window, Operator::Change, repeat);
            }
            Command::YankLine => {
                return self.line_operator(context, window, Operator::Yank, repeat);
            }
            Command::DeleteToLineEnd => {
                return self.line_end_operator(context, window, Operator::Delete, command, count);
            }
            Command::ChangeToLineEnd => {
                return self.line_end_operator(context, window, Operator::Change, command, count);
            }
            Command::BlockInsertBefore => {
                return self.begin_block_insert(context, window, BlockEdge::Left);
            }
            Command::BlockInsertAfter => {
                return self.begin_block_insert(context, window, BlockEdge::Right);
            }

            // Registers and paste.
            Command::PasteAfter => {
                return self.paste(context, window, count, PastePlacement::After);
            }
            Command::PasteBefore => {
                return self.paste(context, window, count, PastePlacement::Before);
            }

            // Visual selection move and shift.
            Command::MoveSelectionDown => {
                return self.move_selection(context, window, MoveDirection::Down, auto);
            }
            Command::MoveSelectionUp => {
                return self.move_selection(context, window, MoveDirection::Up, auto);
            }
            Command::ShiftSelectionLeft => {
                return self.shift_selection(context, window, ShiftDirection::Left);
            }
            Command::ShiftSelectionRight => {
                return self.shift_selection(context, window, ShiftDirection::Right);
            }

            // History and repeat.
            Command::Undo => return self.step_history(context, window, HistoryStep::Undo),
            Command::Redo => return self.step_history(context, window, HistoryStep::Redo),
            Command::RepeatChange => return self.repeat_change(context, window, auto),

            _ => return CommandOutcome::Unhandled,
        }

        self.reconcile(context, window);
        CommandOutcome::Applied
    }

    /// Applies the complete text of one Insert session.
    ///
    /// A pending block insert writes the text into every selected line as one
    /// transaction, so one undo reverses the whole block. A line that is shorter
    /// than the block left edge receives no change.
    ///
    /// The text that a plan actually inserts stays at or below
    /// [`INSERT_TEXT_BYTES_MAX`]. A larger text returns
    /// [`CommandOutcome::Rejected`] and changes nothing. A CRLF buffer rewrites
    /// each `\n` of the supplied text to `\r\n`, so the bound applies after
    /// that rewrite; a block insert holds no such rewrite, so its bound still
    /// applies to the supplied text.
    pub fn insert_text(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        text: &str,
    ) -> CommandResult {
        self.applied = None;
        let outcome = self.insert_text_inner(context, window, text);
        CommandResult::new(outcome, self.applied.take())
    }

    fn insert_text_inner(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        text: &str,
    ) -> CommandOutcome {
        if text.is_empty() {
            return CommandOutcome::Applied;
        }
        // A block insert repeats the supplied text on each selected line and
        // rewrites no line ending, so its bound applies to that text. The
        // check runs before the take, so a rejection leaves the pending block
        // in place.
        if self.block_insert.is_some() && text.len() > INSERT_TEXT_BYTES_MAX {
            return CommandOutcome::Rejected;
        }
        let plan = match self.block_insert.take() {
            Some(block) => edit::plan_block_insert(context.buffer, window.cursor, block, text),
            // A CRLF buffer rewrites each `\n` of the supplied text, so the
            // bound applies to the text that the plan inserts.
            None => {
                let plan = edit::plan_insert_text(context.buffer, window.cursor, text);
                if plan_replacement_bytes(&plan) > INSERT_TEXT_BYTES_MAX {
                    return CommandOutcome::Rejected;
                }
                plan
            }
        };
        self.commit(context, window, plan)
    }

    /// Inserts one line break with the previous-line automatic indent.
    ///
    /// `Enter` in Insert mode reaches this entry point. The indent follows the
    /// same rule as `o` and `O`, and the line break and the indent are one
    /// transaction, so one undo reverses both. See `docs/text-model.md`.
    pub fn insert_line_break(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
    ) -> CommandResult {
        self.applied = None;
        let outcome =
            self.insert_line_break_indented_inner(context, window, AutoIndent::PreviousLine);
        CommandResult::new(outcome, self.applied.take())
    }

    /// Inserts one line break with an explicit automatic indent.
    ///
    /// A caller that holds a parse result for the current buffer version passes
    /// the syntax-tree level count. A caller without one passes
    /// [`AutoIndent::PreviousLine`] instead of waiting for a parse result.
    pub fn insert_line_break_indented(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        auto: AutoIndent,
    ) -> CommandResult {
        self.applied = None;
        let outcome = self.insert_line_break_indented_inner(context, window, auto);
        CommandResult::new(outcome, self.applied.take())
    }

    fn insert_line_break_indented_inner(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        auto: AutoIndent,
    ) -> CommandOutcome {
        // A line break moves the text after the cursor to a new line, so a
        // pending block rectangle no longer describes the buffer.
        self.block_insert = None;
        let plan = edit::plan_line_break(context.buffer, context.indent(), window.cursor, auto);
        self.commit(context, window, plan)
    }

    /// Toggles the line comment of the cursor line or of the selection.
    ///
    /// `Space /` reaches this entry point. Normal mode toggles the cursor line.
    /// Every Visual mode toggles the complete lines of the selection. The
    /// toggle is one transaction, so one undo reverses it.
    ///
    /// The caller reads the token from the adapter that serves the buffer,
    /// because only an adapter knows the language of a path. A buffer without
    /// an adapter, or a language without a line-comment token, passes `None`.
    /// The buffer then stays unchanged and the caller reports the reason.
    /// Returns [`CommandOutcome::Unhandled`] in that case. See
    /// `docs/language-services.md`.
    pub fn toggle_comment(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        comment: Option<&str>,
    ) -> CommandResult {
        self.applied = None;
        let outcome = self.toggle_comment_inner(context, window, comment);
        CommandResult::new(outcome, self.applied.take())
    }

    fn toggle_comment_inner(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        comment: Option<&str>,
    ) -> CommandOutcome {
        let Some(comment) = comment else {
            return CommandOutcome::Unhandled;
        };
        let (first, last) = match self.selection(context.buffer, window) {
            Some(selection) => edit::selection_lines(context.buffer, selection),
            None => (window.cursor.line(), window.cursor.line()),
        };
        let plan = edit::plan_toggle_comment(
            context.buffer,
            context.indent(),
            window.cursor,
            first,
            last,
            comment,
        );
        self.commit(context, window, plan)
    }

    /// Deletes the character before the cursor.
    ///
    /// `Backspace` in Insert mode reaches this entry point. At column zero the
    /// delete removes the line ending before the cursor line, so the two lines
    /// join. At the start of the buffer it changes nothing. The delete is one
    /// transaction, so one undo reverses it, and it writes no register.
    pub fn delete_backward(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
    ) -> CommandResult {
        self.applied = None;
        let outcome = self.delete_backward_inner(context, window);
        CommandResult::new(outcome, self.applied.take())
    }

    fn delete_backward_inner(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
    ) -> CommandOutcome {
        // The delete moves the text after the cursor, so a pending block
        // rectangle no longer describes the buffer.
        self.block_insert = None;
        let plan = edit::plan_delete_backward(context.buffer, window.cursor);
        self.commit(context, window, plan)
    }

    /// Deletes the word before the cursor.
    ///
    /// `Ctrl-W` in Insert mode reaches this entry point. The delete reaches
    /// back to the previous word start, so it removes the blanks before the
    /// word as well as the word itself, and it crosses a line boundary when
    /// the word start stands on an earlier line. At the start of the buffer it
    /// changes nothing. The delete is one transaction, so one undo reverses it,
    /// and it writes no register.
    pub fn delete_word_backward(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
    ) -> CommandResult {
        self.applied = None;
        let outcome = self.delete_word_backward_inner(context, window);
        CommandResult::new(outcome, self.applied.take())
    }

    fn delete_word_backward_inner(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
    ) -> CommandOutcome {
        // The delete moves the text after the cursor, so a pending block
        // rectangle no longer describes the buffer.
        self.block_insert = None;
        let plan =
            edit::plan_delete_word_backward(context.buffer, self.column_limit(), window.cursor);
        self.commit(context, window, plan)
    }

    /// Applies one transaction that another module built.
    ///
    /// The accepted formatter answer of a language server reaches this entry
    /// point. The complete answer is one transaction, so one undo reverses a
    /// complete format. The mode stays, and the cursor keeps its line and its
    /// column and clamps to the new text, because a format is decoration of the
    /// text that the user already wrote. The caller validates the transaction
    /// against the current buffer version before it calls this method. See
    /// `docs/language-services.md`.
    pub fn apply_transaction(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        transaction: EditTransaction,
    ) -> CommandResult {
        self.applied = None;
        let plan = EditPlan {
            transaction: Some(transaction),
            ..EditPlan::unchanged()
        };
        let outcome = self.commit(context, window, plan);
        CommandResult::new(outcome, self.applied.take())
    }

    /// Moves the cursor to the first match of a query.
    ///
    /// The search prompt calls this entry point when the user accepts a query.
    /// Returns [`CommandOutcome::SearchMissed`] when the buffer holds no match.
    pub fn search(
        &mut self,
        context: &CommandContext<'_>,
        window: &mut WindowState,
        query: &SearchQuery,
    ) -> CommandOutcome {
        let Some(found) = self.repeat_search(context, window, query, query.direction(), 1) else {
            return CommandOutcome::SearchMissed;
        };
        window.cursor = found;
        *window = window.reconciled(context.buffer, &context.settings.display);
        CommandOutcome::Applied
    }

    fn motion_target(
        &self,
        context: &CommandContext<'_>,
        window: &WindowState,
        command: Command,
        count: Option<NonZeroU32>,
    ) -> MotionResult {
        let buffer = context.buffer;
        let limit = self.column_limit();
        let repeat = repeat_count(count);
        let cursor = window.cursor;

        let moved = match command {
            Command::MoveLeft => motion::move_left(buffer, cursor, limit, repeat),
            Command::MoveRight => motion::move_right(buffer, cursor, limit, repeat),
            Command::MoveDown => motion::move_down(buffer, cursor, limit, repeat),
            Command::MoveUp => motion::move_up(buffer, cursor, limit, repeat),
            Command::MoveNextWordStart => {
                motion::move_next_word_start(buffer, cursor, limit, repeat)
            }
            Command::MovePreviousWordStart => {
                motion::move_previous_word_start(buffer, cursor, limit, repeat)
            }
            Command::MoveNextWordEnd => motion::move_next_word_end(buffer, cursor, limit, repeat),
            Command::MoveFirstColumn => motion::move_first_column(buffer, cursor, limit),
            Command::MoveFirstNonBlank => motion::move_first_non_blank(buffer, cursor, limit),
            Command::MoveLastNonBlank => motion::move_last_non_blank(buffer, cursor, limit, repeat),
            Command::MoveLineEnd => motion::move_line_end(buffer, cursor, limit, repeat),
            Command::MoveMatchingBracket => {
                let Some(found) = motion::move_matching_bracket(buffer, cursor, limit, repeat)
                else {
                    return MotionResult::Missed;
                };
                found
            }
            // A count before `gg` or `G` names a line, not a number of steps.
            Command::MoveFirstLine => motion::move_to_line(buffer, limit, target_line(count, 0)),
            Command::MoveLastLine => {
                let last = buffer.line_count() - 1;
                motion::move_to_line(buffer, limit, target_line(count, last))
            }
            Command::MoveHalfPageDown => {
                let rows = window.viewport.half_page_rows().saturating_mul(repeat);
                motion::move_down(buffer, cursor, limit, rows)
            }
            Command::MoveHalfPageUp => {
                let rows = window.viewport.half_page_rows().saturating_mul(repeat);
                motion::move_up(buffer, cursor, limit, rows)
            }
            Command::MoveFullPageDown => {
                let rows = window.viewport.full_page_rows().saturating_mul(repeat);
                motion::move_down(buffer, cursor, limit, rows)
            }
            Command::MoveFullPageUp => {
                let rows = window.viewport.full_page_rows().saturating_mul(repeat);
                motion::move_up(buffer, cursor, limit, rows)
            }
            Command::SearchNext | Command::SearchPrevious => {
                let Some(query) = context.search else {
                    return MotionResult::Missed;
                };
                let direction = if command == Command::SearchPrevious {
                    query.direction().reversed()
                } else {
                    query.direction()
                };
                let Some(found) = self.repeat_search(context, window, query, direction, repeat)
                else {
                    return MotionResult::Missed;
                };
                found
            }
            _ => return MotionResult::NotAMotion,
        };
        MotionResult::Moved(moved)
    }

    fn complete_operator(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        pending: PendingOperator,
        command: Command,
        count: Option<NonZeroU32>,
    ) -> CommandOutcome {
        // A repeated operator key means linewise: `dd`, `cc`, and `yy`. The
        // operator emits the linewise command that no key binding reaches.
        if command == pending.operator.motion_command() {
            let lines = repeat_count(pending.count)
                .saturating_mul(repeat_count(count))
                .min(MOTION_COUNT_MAX);
            let outcome = self.line_operator(context, window, pending.operator, lines);
            self.record(
                outcome,
                RepeatableChange::Command {
                    command: pending.operator.line_command(),
                    count: NonZeroU32::new(lines as u32),
                },
            );
            return outcome;
        }

        if let Some(object) = TextObject::of_command(command) {
            return self.operator_text_object(context, window, pending, object, command, count);
        }

        let Some(kind) = motion_kind(command) else {
            return CommandOutcome::OperatorAborted;
        };
        let effective = operator_motion_count(command, pending.count, count);
        let before = window.cursor;
        // Vim reads `w` after an operator by two rules of its own, which
        // `docs/input-actions.md` records. `cw` on a non-blank changes to the
        // end of the word, exactly as `ce` does, so the blanks after the word
        // stay. Every other operator ends at the end of the last word that the
        // motion moved over, when that word ends at the end of its line.
        let word_start = command == Command::MoveNextWordStart;
        let (kind, motion) = if word_start
            && pending.operator == Operator::Change
            && !motion::is_blank_at(context.buffer, before)
        {
            (
                MotionKind::Inclusive,
                self.motion_target(&context.read(), window, Command::MoveNextWordEnd, effective),
            )
        } else if word_start {
            let target =
                motion::operator_next_word_start(context.buffer, before, repeat_count(effective));
            (kind, MotionResult::Moved(target))
        } else {
            (
                kind,
                self.motion_target(&context.read(), window, command, effective),
            )
        };
        let MotionResult::Moved(after) = motion else {
            return CommandOutcome::OperatorAborted;
        };

        let range = OperatorRange::from_motion(context.buffer, before, after, kind);
        let plan = plan_operator(
            context.buffer,
            context.indent(),
            before,
            pending.operator,
            range,
        );
        let outcome = self.commit(context, window, plan);
        self.record(
            outcome,
            RepeatableChange::OperatorMotion {
                operator: pending.operator,
                count: pending.count,
                motion: command,
                motion_count: count,
            },
        );
        outcome
    }

    /// Applies one operator over the range that a text object names.
    ///
    /// The range reaches [`plan_operator`] as a characterwise range, so the
    /// object shares the plan, the transaction, and the repeat description of
    /// every operator over a motion.
    fn operator_text_object(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        pending: PendingOperator,
        object: TextObject,
        command: Command,
        count: Option<NonZeroU32>,
    ) -> CommandOutcome {
        let repeat = repeat_count(pending.count)
            .saturating_mul(repeat_count(count))
            .min(MOTION_COUNT_MAX);
        let Some(range) = object.range(context.buffer, window.cursor, repeat) else {
            return CommandOutcome::OperatorAborted;
        };
        let plan = plan_operator(
            context.buffer,
            context.indent(),
            window.cursor,
            pending.operator,
            OperatorRange::Characterwise(range),
        );
        let outcome = self.commit(context, window, plan);
        self.record(
            outcome,
            RepeatableChange::OperatorMotion {
                operator: pending.operator,
                count: pending.count,
                motion: command,
                motion_count: count,
            },
        );
        outcome
    }

    /// Moves the selection of one window onto the range of one text object.
    ///
    /// The anchor moves to the first character of the range and the cursor to
    /// the last, so the selection shape still follows the active Visual mode.
    /// A mode without an anchor holds no selection and takes no object.
    fn select_text_object(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        object: TextObject,
        count: Option<NonZeroU32>,
    ) -> CommandOutcome {
        if window.anchor.is_none() {
            return CommandOutcome::Unhandled;
        }
        let Some(range) = object.range(context.buffer, window.cursor, repeat_count(count)) else {
            return CommandOutcome::OperatorAborted;
        };
        let start = range.start();
        // A Visual selection is inclusive, so the cursor sits on the last
        // character of the range. An empty range keeps both ends together.
        let last = context
            .buffer
            .char_position(range.end().get().saturating_sub(1).max(start.get()))
            .expect("the range comes from this buffer, so both ends stay inside it");
        self.restore_anchor(
            context.buffer,
            window,
            context.buffer.char_to_line(start).get(),
            context.buffer.char_to_column(start).get(),
        );
        window.cursor = Cursor::at_position(context.buffer, last, self.column_limit());
        self.reconcile(context, window);
        CommandOutcome::Applied
    }

    fn start_operator(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        operator: Operator,
        count: Option<NonZeroU32>,
    ) -> CommandOutcome {
        if let Some(selection) = self.selection(context.buffer, window) {
            let plan = plan_operator(
                context.buffer,
                context.indent(),
                window.cursor,
                operator,
                OperatorRange::from_selection(selection),
            );
            return self.commit(context, window, plan);
        }
        self.pending = Some(PendingOperator { operator, count });
        CommandOutcome::OperatorPending
    }

    fn line_operator(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        operator: Operator,
        lines: usize,
    ) -> CommandOutcome {
        debug_assert!(lines > 0, "the resolver rejects a zero count");
        let first = window.cursor.line();
        let last = context
            .buffer
            .line_index((first.get() + lines - 1).min(context.buffer.line_count() - 1))
            .expect("the clamp keeps the line index inside the buffer");
        let plan = plan_operator(
            context.buffer,
            context.indent(),
            window.cursor,
            operator,
            OperatorRange::Linewise { first, last },
        );
        self.commit(context, window, plan)
    }

    fn line_end_operator(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        operator: Operator,
        command: Command,
        count: Option<NonZeroU32>,
    ) -> CommandOutcome {
        let lines = repeat_count(count);
        let last = motion::move_line_end(context.buffer, window.cursor, self.column_limit(), lines);
        let range =
            OperatorRange::from_motion(context.buffer, window.cursor, last, MotionKind::Inclusive);
        let plan = plan_operator(
            context.buffer,
            context.indent(),
            window.cursor,
            operator,
            range,
        );
        let outcome = self.commit(context, window, plan);
        self.record(outcome, RepeatableChange::Command { command, count });
        outcome
    }

    fn open_line(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        command: Command,
        count: Option<NonZeroU32>,
        direction: OpenDirection,
        auto: AutoIndent,
    ) -> CommandOutcome {
        let plan = edit::plan_open_line(
            context.buffer,
            context.indent(),
            window.cursor,
            direction,
            auto,
        );
        let outcome = self.commit(context, window, plan);
        self.record(outcome, RepeatableChange::Command { command, count });
        outcome
    }

    fn begin_block_insert(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        edge: BlockEdge,
    ) -> CommandOutcome {
        let Some(Selection::Block {
            first_line,
            last_line,
            left,
            right,
        }) = self.selection(context.buffer, window)
        else {
            return CommandOutcome::Unhandled;
        };
        self.block_insert = Some(PendingBlockInsert {
            first_line: first_line.get(),
            last_line: last_line.get(),
            left: left.get(),
            right: right.get(),
            edge,
        });
        // The mode and the anchor change together: Insert mode holds none.
        self.mode = Mode::Insert;
        window.anchor = None;
        let column = match edge {
            BlockEdge::Left => left.get(),
            BlockEdge::Right => right.get() + 1,
        };
        self.place(
            context.buffer,
            window,
            CursorTarget::At {
                line: first_line.get(),
                column,
            },
        );
        self.reconcile(context, window);
        CommandOutcome::Applied
    }

    fn paste(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        count: Option<NonZeroU32>,
        placement: PastePlacement,
    ) -> CommandOutcome {
        let value = match context.registers.value(self.register) {
            Some(stored) => stored.repeated(repeat_count(count), context.buffer.line_ending()),
            None => return CommandOutcome::RegisterEmpty,
        };

        if let Some(selection) = self.selection(context.buffer, window) {
            // A Visual paste replaces the selection and preserves the source
            // register, so a following paste repeats the same text.
            let plan = edit::plan_visual_paste(context.buffer, window.cursor, selection, &value);
            return self.commit(context, window, plan);
        }

        let plan = edit::plan_paste(context.buffer, window.cursor, &value, placement);
        let outcome = self.commit(context, window, plan);
        self.record(
            outcome,
            RepeatableChange::Command {
                command: match placement {
                    PastePlacement::After => Command::PasteAfter,
                    PastePlacement::Before => Command::PasteBefore,
                },
                count,
            },
        );
        outcome
    }

    fn move_selection(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        direction: MoveDirection,
        auto: AutoIndent,
    ) -> CommandOutcome {
        let Some(selection) = self.selection(context.buffer, window) else {
            return CommandOutcome::Unhandled;
        };
        let (first, last) = edit::selection_lines(context.buffer, selection);
        let anchor = window.anchor;
        let Some(plan) = edit::plan_move_lines(
            context.buffer,
            context.indent(),
            window.cursor,
            first,
            last,
            direction,
            auto,
        ) else {
            return CommandOutcome::Applied;
        };
        let outcome = self.commit(context, window, plan);
        if let Some(anchor) = anchor {
            // The moved lines carry the anchor with them, so the selection keeps
            // the same text.
            let line = match direction {
                MoveDirection::Down => anchor.line.get() + 1,
                MoveDirection::Up => anchor.line.get().saturating_sub(1),
            };
            self.restore_anchor(context.buffer, window, line, anchor.column.get());
        }
        outcome
    }

    fn shift_selection(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        direction: ShiftDirection,
    ) -> CommandOutcome {
        let Some(selection) = self.selection(context.buffer, window) else {
            return CommandOutcome::Unhandled;
        };
        let (first, last) = edit::selection_lines(context.buffer, selection);
        let anchor = window.anchor;
        let plan = edit::plan_shift_lines(
            context.buffer,
            context.indent(),
            window.cursor,
            first,
            last,
            direction,
        );
        let outcome = self.commit(context, window, plan);
        if let Some(anchor) = anchor {
            self.restore_anchor(
                context.buffer,
                window,
                anchor.line.get(),
                anchor.column.get(),
            );
        }
        outcome
    }

    fn step_history(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        step: HistoryStep,
    ) -> CommandOutcome {
        let position = match step {
            HistoryStep::Undo => context.buffer.undo(),
            HistoryStep::Redo => context.buffer.redo(),
        };
        let Some(position) = position else {
            return CommandOutcome::HistoryExhausted;
        };
        // An undo and a redo return to Normal mode, which holds no anchor.
        self.mode = Mode::Normal;
        window.anchor = None;
        self.block_insert = None;
        self.place(
            context.buffer,
            window,
            CursorTarget::Position(position.get()),
        );
        self.reconcile(context, window);
        CommandOutcome::Changed
    }

    fn repeat_change(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        auto: AutoIndent,
    ) -> CommandOutcome {
        let Some(change) = self.repeat else {
            return CommandOutcome::Unhandled;
        };
        match change {
            RepeatableChange::Command { command, count } => {
                let result = self.apply_indented(context, window, command, count, auto);
                self.applied = result.transaction;
                result.outcome
            }
            RepeatableChange::OperatorMotion {
                operator,
                count,
                motion,
                motion_count,
            } => {
                self.pending = Some(PendingOperator { operator, count });
                let result = self.apply_indented(context, window, motion, motion_count, auto);
                self.applied = result.transaction;
                result.outcome
            }
        }
    }

    fn commit(
        &mut self,
        context: &mut EditContext<'_>,
        window: &mut WindowState,
        plan: EditPlan,
    ) -> CommandOutcome {
        if let Some(value) = plan.value {
            let line_ending = context.buffer.line_ending();
            context.registers.write(self.register, value, line_ending);
        }
        let mut changed = false;
        if let Some(transaction) = plan.transaction {
            let recorded = transaction.clone();
            match context.buffer.apply(transaction) {
                Ok(_) => {
                    changed = true;
                    self.applied = Some(recorded);
                }
                Err(EditError::TooLarge { .. }) => return CommandOutcome::Rejected,
                Err(error) => debug_assert!(
                    false,
                    "the editor builds every range from the current buffer: {error}"
                ),
            }
        }
        // Normal mode and Insert mode hold no anchor, so the plan drops it with
        // the mode that it names.
        match plan.next_mode {
            NextMode::Keep => {}
            NextMode::Normal => {
                self.block_insert = None;
                self.mode = Mode::Normal;
                window.anchor = None;
            }
            NextMode::Insert => {
                self.mode = Mode::Insert;
                window.anchor = None;
            }
        }
        self.place(context.buffer, window, plan.cursor);
        self.reconcile(context, window);
        if changed {
            CommandOutcome::Changed
        } else {
            CommandOutcome::Applied
        }
    }

    fn record(&mut self, outcome: CommandOutcome, change: RepeatableChange) {
        if outcome == CommandOutcome::Changed {
            self.repeat = Some(change);
        }
    }

    fn place(&self, buffer: &TextBuffer, window: &mut WindowState, target: CursorTarget) {
        let limit = self.column_limit();
        window.cursor = match target {
            CursorTarget::At { line, column } => Cursor::clamped(buffer, line, column, limit),
            CursorTarget::FirstNonBlank { line } => motion::move_to_line(buffer, limit, line),
            CursorTarget::Position(position) => {
                let position = buffer
                    .char_position(position.min(buffer.len_chars()))
                    .expect("the clamp keeps the position inside the buffer");
                Cursor::at_position(buffer, position, limit)
            }
            CursorTarget::Unchanged => window.cursor.re_clamped(buffer, limit),
        };
    }

    fn reconcile(&self, context: &EditContext<'_>, window: &mut WindowState) {
        *window = window.reconciled(context.buffer, &context.settings.display);
    }

    /// Moves the selection anchor of one window to a line and a column.
    ///
    /// A selection move and a selection shift both rewrite the lines that they
    /// changed, so the anchor follows the text. A window without an anchor holds
    /// no selection and keeps none.
    fn restore_anchor(
        &self,
        buffer: &TextBuffer,
        window: &mut WindowState,
        line: usize,
        column: usize,
    ) {
        if window.anchor.is_none() {
            return;
        }
        let line = buffer
            .line_index(line.min(buffer.line_count() - 1))
            .expect("the clamp keeps the line index inside the buffer");
        let column = buffer
            .source_column(line, column.min(buffer.line_len_chars(line)))
            .expect("the clamp keeps the column inside the line");
        window.anchor = Some(AnchorPoint { line, column });
    }

    fn repeat_search(
        &self,
        context: &CommandContext<'_>,
        window: &WindowState,
        query: &SearchQuery,
        direction: SearchDirection,
        repeat: usize,
    ) -> Option<Cursor> {
        let buffer = context.buffer;
        let mut position = window.cursor.position(buffer);
        for _ in 0..repeat {
            position = query.find(buffer, position, direction, &context.settings.search)?;
        }
        Some(Cursor::at_position(buffer, position, self.column_limit()))
    }
}

/// The direction of one history step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryStep {
    Undo,
    Redo,
}

/// Returns the total bytes that one plan's transaction inserts or replaces.
///
/// [`EditingState::insert_text`] checks this against
/// [`INSERT_TEXT_BYTES_MAX`], because a CRLF buffer rewrite can grow the
/// supplied text past the bound after the caller already checked the
/// supplied text alone.
fn plan_replacement_bytes(plan: &EditPlan) -> usize {
    plan.transaction.as_ref().map_or(0, |transaction| {
        transaction
            .changes()
            .iter()
            .map(|change| change.replacement().len())
            .sum()
    })
}

/// Converts an optional count into a bounded number of repetitions.
fn repeat_count(count: Option<NonZeroU32>) -> usize {
    count
        .map_or(1, |value| value.get() as usize)
        .min(MOTION_COUNT_MAX)
}

/// Converts an optional count into a zero-based line index.
///
/// A count before `gg` or `G` names a one-based line number.
fn target_line(count: Option<NonZeroU32>, default_line: usize) -> usize {
    count.map_or(default_line, |value| value.get() as usize - 1)
}

/// Multiplies the operator count and the motion count into one count.
///
/// `2d3w` deletes six words. A count before `gg` or `G` names a line instead of
/// a number of steps, so those two motions keep their own count.
fn operator_motion_count(
    command: Command,
    operator_count: Option<NonZeroU32>,
    motion_count: Option<NonZeroU32>,
) -> Option<NonZeroU32> {
    if matches!(command, Command::MoveFirstLine | Command::MoveLastLine) {
        return motion_count;
    }
    let product = repeat_count(operator_count)
        .saturating_mul(repeat_count(motion_count))
        .min(MOTION_COUNT_MAX);
    NonZeroU32::new(product as u32)
}
