//! The editing state and the one command dispatcher of the editor.
//!
//! [`EditingState`] owns the cursor, the mode, the operator-pending state, the
//! pending block insert, and the last repeatable change. It executes every
//! motion, selection, search, viewport, operator, register, paste, and repeat
//! command that `input` names.
//!
//! Every text change leaves this module as one [`EditTransaction`], staged as an
//! [`EditPlan`] and committed in one step. The module reads no clock, no
//! filesystem, and no process. The system clipboard stays outside: the caller
//! passes the register value in and out. See `docs/clipboard.md`.

use std::num::NonZeroU32;

use crate::core::{IndentPolicy, LineIndex, ShiftDirection, SourceColumn, TextBuffer};
use crate::input::{Command, Mode};
use crate::settings::{COUNT_MAX, EditorSettings};

use super::cursor::Cursor;
use super::edit::{
    self, AutoIndent, BlockEdge, CursorTarget, EditPlan, MoveDirection, NextMode, OpenDirection,
    PastePlacement, PendingBlockInsert,
};
use super::motion;
use super::operator::{MotionKind, Operator, OperatorRange, motion_kind, plan_operator};
use super::register::Registers;
use super::search::{SearchDirection, SearchQuery};
use super::selection::{BlockAnchor, ModeState, Selection};
use super::viewport::{Viewport, ViewportAlignment};

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
        IndentPolicy::from_settings(&self.settings.indent)
    }
}

/// The result of one command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutcome {
    /// The command changed the buffer text.
    Changed,
    /// The command changed the cursor, the mode, the register, or the viewport.
    Applied,
    /// The operator waits for a motion or for a repeated operator key.
    OperatorPending,
    /// The pending operator received no motion, so it changed nothing.
    OperatorAborted,
    /// The command pastes, but the unnamed register holds no value.
    RegisterEmpty,
    /// The command undoes or redoes, but the history holds no further step.
    HistoryExhausted,
    /// The command names a search, but no query is active or no match exists.
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

/// The cursor, the mode, the operator-pending state, and the repeat description.
///
/// # Examples
///
/// ```
/// use std::num::NonZeroU16;
///
/// use kvim::core::TextBuffer;
/// use kvim::editor::{CommandOutcome, EditContext, EditingState, Registers, Viewport};
/// use kvim::input::{Command, Mode};
/// use kvim::settings::{EditorSettings, FileSettings};
///
/// let mut buffer = TextBuffer::from_text("alpha beta\ngamma\n", &FileSettings::default())
///     .expect("the text is small");
/// let settings = EditorSettings::default();
/// let mut registers = Registers::default();
/// let mut context = EditContext {
///     buffer: &mut buffer,
///     settings: &settings,
///     search: None,
///     registers: &mut registers,
/// };
///
/// let rows = NonZeroU16::new(10).expect("the literal 10 is not zero");
/// let cells = NonZeroU16::new(80).expect("the literal 80 is not zero");
/// let mut viewport = Viewport::new(rows, cells);
/// let mut state = EditingState::new(context.buffer);
///
/// // `d` waits for a motion, and `w` completes it.
/// let pending = state.apply(&mut context, &mut viewport, Command::DeleteOverMotion, None);
/// assert_eq!(pending, CommandOutcome::OperatorPending);
/// let changed = state.apply(&mut context, &mut viewport, Command::MoveNextWordStart, None);
/// assert_eq!(changed, CommandOutcome::Changed);
/// assert_eq!(context.buffer.to_string(), "beta\ngamma\n");
///
/// // One undo reverses the complete change.
/// state.apply(&mut context, &mut viewport, Command::Undo, None);
/// assert_eq!(context.buffer.to_string(), "alpha beta\ngamma\n");
/// assert_eq!(state.mode(), Mode::Normal);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditingState {
    mode: ModeState,
    cursor: Cursor,
    pending: Option<PendingOperator>,
    block_insert: Option<PendingBlockInsert>,
    repeat: Option<RepeatableChange>,
}

impl EditingState {
    /// Creates the Normal-mode state at the start of a buffer.
    #[must_use]
    pub fn new(buffer: &TextBuffer) -> Self {
        let mode = ModeState::Normal;
        Self {
            mode,
            cursor: Cursor::at_buffer_start(buffer, mode.column_limit()),
            pending: None,
            block_insert: None,
            repeat: None,
        }
    }

    /// Returns the active mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode.mode()
    }

    /// Returns the active mode together with its selection anchor.
    #[must_use]
    pub const fn mode_state(&self) -> ModeState {
        self.mode
    }

    /// Returns the cursor.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Returns the operator that waits for a motion.
    #[must_use]
    pub fn pending_operator(&self) -> Option<Operator> {
        self.pending.map(|pending| pending.operator)
    }

    /// Returns the selection of the active Visual mode.
    #[must_use]
    pub fn selection(&self, buffer: &TextBuffer) -> Option<Selection> {
        self.mode.selection(buffer, self.cursor)
    }

    /// Places the cursor at a line and a column, clamped to the buffer.
    ///
    /// The command line `:<number>` and a language-service jump both use this
    /// entry point.
    pub fn move_to(&mut self, buffer: &TextBuffer, line: usize, column: usize) {
        self.cursor = Cursor::clamped(buffer, line, column, self.mode.column_limit());
    }

    /// Changes the mode and derives the selection anchor from the cursor.
    ///
    /// A change from one Visual mode into another keeps the existing anchor, so
    /// the selection does not restart. The cursor clamps again, because Insert
    /// mode allows one more column than the other modes.
    pub fn enter_mode(&mut self, buffer: &TextBuffer, mode: Mode) {
        let (line, column) = self
            .anchor_point(buffer)
            .unwrap_or((self.cursor.line(), self.cursor.column()));
        // The anchor of a rectangular selection may name a column that a shorter
        // line does not hold, so the anchor clamps to its own line.
        let column = buffer
            .source_column(line, column.get().min(buffer.line_len_chars(line)))
            .expect("the clamp keeps the column inside the anchor line");
        self.mode = match mode {
            Mode::Normal => ModeState::Normal,
            Mode::Insert => ModeState::Insert,
            Mode::Visual => ModeState::Visual {
                anchor: buffer.column_to_char(line, column),
            },
            Mode::VisualLine => ModeState::VisualLine { anchor: line },
            Mode::VisualBlock => ModeState::VisualBlock {
                anchor: BlockAnchor { line, column },
            },
        };
        self.cursor = self.cursor.re_clamped(buffer, self.mode.column_limit());
    }

    /// Executes one semantic command with the previous-line automatic indent.
    ///
    /// The viewport follows the cursor after every accepted command, except an
    /// explicit alignment command, which overrides the scroll margin. Every text
    /// change applies as one edit transaction.
    ///
    /// A caller that holds a parse result for the current buffer version uses
    /// [`EditingState::apply_indented`] instead, so `o` and `O` follow the
    /// syntax tree.
    pub fn apply(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        command: Command,
        count: Option<NonZeroU32>,
    ) -> CommandOutcome {
        self.apply_indented(context, viewport, command, count, AutoIndent::PreviousLine)
    }

    /// Executes one semantic command with an explicit automatic indent.
    ///
    /// Only `o` and `O` read the indent. Every other command ignores it.
    pub fn apply_indented(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        command: Command,
        count: Option<NonZeroU32>,
        auto: AutoIndent,
    ) -> CommandOutcome {
        if let Some(pending) = self.pending.take() {
            return self.complete_operator(context, viewport, pending, command, count);
        }

        let motion = self.motion_target(&context.read(), viewport, command, count);
        match motion {
            MotionResult::Moved(cursor) => {
                self.cursor = cursor;
                self.reconcile(context, viewport);
                return CommandOutcome::Applied;
            }
            MotionResult::Missed => return CommandOutcome::SearchMissed,
            MotionResult::NotAMotion => {}
        }

        let repeat = repeat_count(count);
        match command {
            // Modes.
            Command::ReturnToNormal => {
                self.block_insert = None;
                self.enter_mode(context.buffer, Mode::Normal);
            }
            Command::EnterVisual => self.enter_mode(context.buffer, Mode::Visual),
            Command::EnterVisualLine => self.enter_mode(context.buffer, Mode::VisualLine),
            Command::EnterVisualBlock => self.enter_mode(context.buffer, Mode::VisualBlock),

            // Insert entry. Each command also places the cursor for the change
            // that follows it.
            Command::InsertBeforeCursor => self.enter_mode(context.buffer, Mode::Insert),
            Command::InsertAtFirstNonBlank => {
                self.enter_mode(context.buffer, Mode::Insert);
                self.cursor = motion::move_first_non_blank(
                    context.buffer,
                    self.cursor,
                    self.mode.column_limit(),
                );
            }
            Command::InsertAfterCursor => {
                self.enter_mode(context.buffer, Mode::Insert);
                self.cursor =
                    motion::move_right(context.buffer, self.cursor, self.mode.column_limit(), 1);
            }
            Command::InsertAtLineEnd => {
                self.enter_mode(context.buffer, Mode::Insert);
                self.cursor = Cursor::clamped(
                    context.buffer,
                    self.cursor.line().get(),
                    usize::MAX,
                    self.mode.column_limit(),
                );
            }
            Command::OpenLineBelow => {
                return self.open_line(
                    context,
                    viewport,
                    command,
                    count,
                    OpenDirection::Below,
                    auto,
                );
            }
            Command::OpenLineAbove => {
                return self.open_line(
                    context,
                    viewport,
                    command,
                    count,
                    OpenDirection::Above,
                    auto,
                );
            }

            // Viewport alignment. An alignment changes no cursor position.
            Command::CenterCursorLine => {
                *viewport = viewport.aligned(self.cursor, ViewportAlignment::Center);
                return CommandOutcome::Applied;
            }
            Command::AlignCursorLineTop => {
                *viewport = viewport.aligned(self.cursor, ViewportAlignment::Top);
                return CommandOutcome::Applied;
            }
            Command::AlignCursorLineBottom => {
                *viewport = viewport.aligned(self.cursor, ViewportAlignment::Bottom);
                return CommandOutcome::Applied;
            }

            // Operators.
            Command::DeleteOverMotion | Command::DeleteSelection => {
                return self.start_operator(context, viewport, Operator::Delete, count);
            }
            Command::ChangeOverMotion | Command::ChangeSelection => {
                return self.start_operator(context, viewport, Operator::Change, count);
            }
            Command::YankOverMotion | Command::YankSelection => {
                return self.start_operator(context, viewport, Operator::Yank, count);
            }
            Command::DeleteLine => {
                return self.line_operator(context, viewport, Operator::Delete, repeat);
            }
            Command::ChangeLine => {
                return self.line_operator(context, viewport, Operator::Change, repeat);
            }
            Command::YankLine => {
                return self.line_operator(context, viewport, Operator::Yank, repeat);
            }
            Command::DeleteToLineEnd => {
                return self.line_end_operator(context, viewport, Operator::Delete, command, count);
            }
            Command::ChangeToLineEnd => {
                return self.line_end_operator(context, viewport, Operator::Change, command, count);
            }
            Command::BlockInsertBefore => {
                return self.begin_block_insert(context, viewport, BlockEdge::Left);
            }
            Command::BlockInsertAfter => {
                return self.begin_block_insert(context, viewport, BlockEdge::Right);
            }

            // Registers and paste.
            Command::PasteAfter => {
                return self.paste(context, viewport, count, PastePlacement::After);
            }
            Command::PasteBefore => {
                return self.paste(context, viewport, count, PastePlacement::Before);
            }

            // Visual selection move and shift.
            Command::MoveSelectionDown => {
                return self.move_selection(context, viewport, MoveDirection::Down);
            }
            Command::MoveSelectionUp => {
                return self.move_selection(context, viewport, MoveDirection::Up);
            }
            Command::ShiftSelectionLeft => {
                return self.shift_selection(context, viewport, ShiftDirection::Left);
            }
            Command::ShiftSelectionRight => {
                return self.shift_selection(context, viewport, ShiftDirection::Right);
            }

            // History and repeat.
            Command::Undo => return self.step_history(context, viewport, HistoryStep::Undo),
            Command::Redo => return self.step_history(context, viewport, HistoryStep::Redo),
            Command::RepeatChange => return self.repeat_change(context, viewport, auto),

            _ => return CommandOutcome::Unhandled,
        }

        self.reconcile(context, viewport);
        CommandOutcome::Applied
    }

    /// Applies the complete text of one Insert session.
    ///
    /// A pending block insert writes the text into every selected line as one
    /// transaction, so one undo reverses the whole block. A line that is shorter
    /// than the block left edge receives no change.
    ///
    /// The text stays at or below [`INSERT_TEXT_BYTES_MAX`]. A larger text
    /// returns [`CommandOutcome::Rejected`] and changes nothing.
    pub fn insert_text(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        text: &str,
    ) -> CommandOutcome {
        if text.is_empty() {
            return CommandOutcome::Applied;
        }
        if text.len() > INSERT_TEXT_BYTES_MAX {
            return CommandOutcome::Rejected;
        }
        let plan = match self.block_insert.take() {
            Some(block) => edit::plan_block_insert(context.buffer, self.cursor, block, text),
            None => edit::plan_insert_text(context.buffer, self.cursor, text),
        };
        self.commit(context, viewport, plan)
    }

    /// Inserts one line break with the previous-line automatic indent.
    ///
    /// `Enter` in Insert mode reaches this entry point. The indent follows the
    /// same rule as `o` and `O`, and the line break and the indent are one
    /// transaction, so one undo reverses both. See `docs/text-model.md`.
    pub fn insert_line_break(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
    ) -> CommandOutcome {
        self.insert_line_break_indented(context, viewport, AutoIndent::PreviousLine)
    }

    /// Inserts one line break with an explicit automatic indent.
    ///
    /// A caller that holds a parse result for the current buffer version passes
    /// the syntax-tree level count. A caller without one passes
    /// [`AutoIndent::PreviousLine`] instead of waiting for a parse result.
    pub fn insert_line_break_indented(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        auto: AutoIndent,
    ) -> CommandOutcome {
        // A line break moves the text after the cursor to a new line, so a
        // pending block rectangle no longer describes the buffer.
        self.block_insert = None;
        let plan = edit::plan_line_break(context.buffer, context.indent(), self.cursor, auto);
        self.commit(context, viewport, plan)
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
        viewport: &mut Viewport,
        comment: Option<&str>,
    ) -> CommandOutcome {
        let Some(comment) = comment else {
            return CommandOutcome::Unhandled;
        };
        let (first, last) = match self.selection(context.buffer) {
            Some(selection) => edit::selection_lines(context.buffer, selection),
            None => (self.cursor.line(), self.cursor.line()),
        };
        let plan = edit::plan_toggle_comment(
            context.buffer,
            context.indent(),
            self.cursor,
            first,
            last,
            comment,
        );
        self.commit(context, viewport, plan)
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
        viewport: &mut Viewport,
    ) -> CommandOutcome {
        // The delete moves the text after the cursor, so a pending block
        // rectangle no longer describes the buffer.
        self.block_insert = None;
        let plan = edit::plan_delete_backward(context.buffer, self.cursor);
        self.commit(context, viewport, plan)
    }

    /// Moves the cursor to the first match of a query.
    ///
    /// The search prompt calls this entry point when the user accepts a query.
    /// Returns [`CommandOutcome::SearchMissed`] when the buffer holds no match.
    pub fn search(
        &mut self,
        context: &CommandContext<'_>,
        viewport: &mut Viewport,
        query: &SearchQuery,
    ) -> CommandOutcome {
        let Some(found) = self.repeat_search(context, query, query.direction(), 1) else {
            return CommandOutcome::SearchMissed;
        };
        self.cursor = found;
        *viewport = viewport.reconciled(context.buffer, self.cursor, &context.settings.display);
        CommandOutcome::Applied
    }

    fn motion_target(
        &self,
        context: &CommandContext<'_>,
        viewport: &Viewport,
        command: Command,
        count: Option<NonZeroU32>,
    ) -> MotionResult {
        let buffer = context.buffer;
        let limit = self.mode.column_limit();
        let repeat = repeat_count(count);
        let cursor = self.cursor;

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
            Command::MoveLineEnd => motion::move_line_end(buffer, cursor, limit, repeat),
            // A count before `gg` or `G` names a line, not a number of steps.
            Command::MoveFirstLine => motion::move_to_line(buffer, limit, target_line(count, 0)),
            Command::MoveLastLine => {
                let last = buffer.line_count() - 1;
                motion::move_to_line(buffer, limit, target_line(count, last))
            }
            Command::MoveHalfPageDown => {
                let rows = viewport.half_page_rows().saturating_mul(repeat);
                motion::move_down(buffer, cursor, limit, rows)
            }
            Command::MoveHalfPageUp => {
                let rows = viewport.half_page_rows().saturating_mul(repeat);
                motion::move_up(buffer, cursor, limit, rows)
            }
            Command::MoveFullPageDown => {
                let rows = viewport.full_page_rows().saturating_mul(repeat);
                motion::move_down(buffer, cursor, limit, rows)
            }
            Command::MoveFullPageUp => {
                let rows = viewport.full_page_rows().saturating_mul(repeat);
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
                let Some(found) = self.repeat_search(context, query, direction, repeat) else {
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
        viewport: &mut Viewport,
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
            let outcome = self.line_operator(context, viewport, pending.operator, lines);
            self.record(
                outcome,
                RepeatableChange::Command {
                    command: pending.operator.line_command(),
                    count: NonZeroU32::new(lines as u32),
                },
            );
            return outcome;
        }

        let Some(kind) = motion_kind(command) else {
            return CommandOutcome::OperatorAborted;
        };
        let effective = operator_motion_count(command, pending.count, count);
        let before = self.cursor;
        let motion = self.motion_target(&context.read(), viewport, command, effective);
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
        let outcome = self.commit(context, viewport, plan);
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

    fn start_operator(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        operator: Operator,
        count: Option<NonZeroU32>,
    ) -> CommandOutcome {
        if let Some(selection) = self.selection(context.buffer) {
            let plan = plan_operator(
                context.buffer,
                context.indent(),
                self.cursor,
                operator,
                OperatorRange::from_selection(selection),
            );
            return self.commit(context, viewport, plan);
        }
        self.pending = Some(PendingOperator { operator, count });
        CommandOutcome::OperatorPending
    }

    fn line_operator(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        operator: Operator,
        lines: usize,
    ) -> CommandOutcome {
        debug_assert!(lines > 0, "the resolver rejects a zero count");
        let first = self.cursor.line();
        let last = context
            .buffer
            .line_index((first.get() + lines - 1).min(context.buffer.line_count() - 1))
            .expect("the clamp keeps the line index inside the buffer");
        let plan = plan_operator(
            context.buffer,
            context.indent(),
            self.cursor,
            operator,
            OperatorRange::Linewise { first, last },
        );
        self.commit(context, viewport, plan)
    }

    fn line_end_operator(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        operator: Operator,
        command: Command,
        count: Option<NonZeroU32>,
    ) -> CommandOutcome {
        let lines = repeat_count(count);
        let last =
            motion::move_line_end(context.buffer, self.cursor, self.mode.column_limit(), lines);
        let range =
            OperatorRange::from_motion(context.buffer, self.cursor, last, MotionKind::Inclusive);
        let plan = plan_operator(
            context.buffer,
            context.indent(),
            self.cursor,
            operator,
            range,
        );
        let outcome = self.commit(context, viewport, plan);
        self.record(outcome, RepeatableChange::Command { command, count });
        outcome
    }

    fn open_line(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        command: Command,
        count: Option<NonZeroU32>,
        direction: OpenDirection,
        auto: AutoIndent,
    ) -> CommandOutcome {
        let plan = edit::plan_open_line(
            context.buffer,
            context.indent(),
            self.cursor,
            direction,
            auto,
        );
        let outcome = self.commit(context, viewport, plan);
        self.record(outcome, RepeatableChange::Command { command, count });
        outcome
    }

    fn begin_block_insert(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        edge: BlockEdge,
    ) -> CommandOutcome {
        let Some(Selection::Block {
            first_line,
            last_line,
            left,
            right,
        }) = self.selection(context.buffer)
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
        self.mode = ModeState::Insert;
        let column = match edge {
            BlockEdge::Left => left.get(),
            BlockEdge::Right => right.get() + 1,
        };
        self.place(
            context.buffer,
            CursorTarget::At {
                line: first_line.get(),
                column,
            },
        );
        self.reconcile(context, viewport);
        CommandOutcome::Applied
    }

    fn paste(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        count: Option<NonZeroU32>,
        placement: PastePlacement,
    ) -> CommandOutcome {
        let value = match context.registers.unnamed() {
            Some(stored) => stored.repeated(repeat_count(count), context.buffer.line_ending()),
            None => return CommandOutcome::RegisterEmpty,
        };

        if let Some(selection) = self.selection(context.buffer) {
            // A Visual paste replaces the selection and preserves the source
            // register, so a following paste repeats the same text.
            let plan = edit::plan_visual_paste(context.buffer, self.cursor, selection, &value);
            return self.commit(context, viewport, plan);
        }

        let plan = edit::plan_paste(context.buffer, self.cursor, &value, placement);
        let outcome = self.commit(context, viewport, plan);
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
        viewport: &mut Viewport,
        direction: MoveDirection,
    ) -> CommandOutcome {
        let Some(selection) = self.selection(context.buffer) else {
            return CommandOutcome::Unhandled;
        };
        let (first, last) = edit::selection_lines(context.buffer, selection);
        let anchor = self.anchor_point(context.buffer);
        let Some(plan) = edit::plan_move_lines(
            context.buffer,
            context.indent(),
            self.cursor,
            first,
            last,
            direction,
        ) else {
            return CommandOutcome::Applied;
        };
        let outcome = self.commit(context, viewport, plan);
        if let Some((line, column)) = anchor {
            let line = match direction {
                MoveDirection::Down => line.get() + 1,
                MoveDirection::Up => line.get().saturating_sub(1),
            };
            self.restore_anchor(context.buffer, line, column.get());
        }
        outcome
    }

    fn shift_selection(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        direction: ShiftDirection,
    ) -> CommandOutcome {
        let Some(selection) = self.selection(context.buffer) else {
            return CommandOutcome::Unhandled;
        };
        let (first, last) = edit::selection_lines(context.buffer, selection);
        let anchor = self.anchor_point(context.buffer);
        let plan = edit::plan_shift_lines(
            context.buffer,
            context.indent(),
            self.cursor,
            first,
            last,
            direction,
        );
        let outcome = self.commit(context, viewport, plan);
        if let Some((line, column)) = anchor {
            self.restore_anchor(context.buffer, line.get(), column.get());
        }
        outcome
    }

    fn step_history(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        step: HistoryStep,
    ) -> CommandOutcome {
        let position = match step {
            HistoryStep::Undo => context.buffer.undo(),
            HistoryStep::Redo => context.buffer.redo(),
        };
        let Some(position) = position else {
            return CommandOutcome::HistoryExhausted;
        };
        self.mode = ModeState::Normal;
        self.block_insert = None;
        self.place(context.buffer, CursorTarget::Position(position.get()));
        self.reconcile(context, viewport);
        CommandOutcome::Changed
    }

    fn repeat_change(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        auto: AutoIndent,
    ) -> CommandOutcome {
        let Some(change) = self.repeat else {
            return CommandOutcome::Unhandled;
        };
        match change {
            RepeatableChange::Command { command, count } => {
                self.apply_indented(context, viewport, command, count, auto)
            }
            RepeatableChange::OperatorMotion {
                operator,
                count,
                motion,
                motion_count,
            } => {
                self.pending = Some(PendingOperator { operator, count });
                self.apply_indented(context, viewport, motion, motion_count, auto)
            }
        }
    }

    fn commit(
        &mut self,
        context: &mut EditContext<'_>,
        viewport: &mut Viewport,
        plan: EditPlan,
    ) -> CommandOutcome {
        if let Some(value) = plan.value {
            context.registers.set_unnamed(value);
        }
        let mut changed = false;
        if let Some(transaction) = plan.transaction {
            match context.buffer.apply(transaction) {
                Ok(_) => changed = true,
                Err(error) => debug_assert!(
                    false,
                    "the editor builds every range from the current buffer: {error}"
                ),
            }
        }
        match plan.next_mode {
            NextMode::Keep => {}
            NextMode::Normal => {
                self.block_insert = None;
                self.mode = ModeState::Normal;
            }
            NextMode::Insert => self.mode = ModeState::Insert,
        }
        self.place(context.buffer, plan.cursor);
        self.reconcile(context, viewport);
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

    fn place(&mut self, buffer: &TextBuffer, target: CursorTarget) {
        let limit = self.mode.column_limit();
        self.cursor = match target {
            CursorTarget::At { line, column } => Cursor::clamped(buffer, line, column, limit),
            CursorTarget::FirstNonBlank { line } => motion::move_to_line(buffer, limit, line),
            CursorTarget::Position(position) => {
                let position = buffer
                    .char_position(position.min(buffer.len_chars()))
                    .expect("the clamp keeps the position inside the buffer");
                Cursor::at_position(buffer, position, limit)
            }
            CursorTarget::Unchanged => self.cursor.re_clamped(buffer, limit),
        };
    }

    fn reconcile(&self, context: &EditContext<'_>, viewport: &mut Viewport) {
        *viewport = viewport.reconciled(context.buffer, self.cursor, &context.settings.display);
    }

    fn restore_anchor(&mut self, buffer: &TextBuffer, line: usize, column: usize) {
        let line = buffer
            .line_index(line.min(buffer.line_count() - 1))
            .expect("the clamp keeps the line index inside the buffer");
        let column = buffer
            .source_column(line, column.min(buffer.line_len_chars(line)))
            .expect("the clamp keeps the column inside the line");
        self.mode = match self.mode {
            ModeState::Visual { .. } => ModeState::Visual {
                anchor: buffer.column_to_char(line, column),
            },
            ModeState::VisualLine { .. } => ModeState::VisualLine { anchor: line },
            ModeState::VisualBlock { .. } => ModeState::VisualBlock {
                anchor: BlockAnchor { line, column },
            },
            other => other,
        };
    }

    fn repeat_search(
        &self,
        context: &CommandContext<'_>,
        query: &SearchQuery,
        direction: SearchDirection,
        repeat: usize,
    ) -> Option<Cursor> {
        let buffer = context.buffer;
        let mut position = self.cursor.position(buffer);
        for _ in 0..repeat {
            position = query.find(buffer, position, direction, &context.settings.search)?;
        }
        Some(Cursor::at_position(
            buffer,
            position,
            self.mode.column_limit(),
        ))
    }

    fn anchor_point(&self, buffer: &TextBuffer) -> Option<(LineIndex, SourceColumn)> {
        match self.mode {
            ModeState::Normal | ModeState::Insert => None,
            ModeState::Visual { anchor } => {
                Some((buffer.char_to_line(anchor), buffer.char_to_column(anchor)))
            }
            ModeState::VisualLine { anchor } => Some((
                anchor,
                buffer
                    .source_column(anchor, 0)
                    .expect("column zero exists in every line"),
            )),
            ModeState::VisualBlock { anchor } => Some((anchor.line, anchor.column)),
        }
    }
}

/// The direction of one history step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryStep {
    Undo,
    Redo,
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
