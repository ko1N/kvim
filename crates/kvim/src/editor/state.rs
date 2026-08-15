//! The editing state and the command dispatcher of this slice.
//!
//! [`EditingState`] owns the cursor, the mode, and the selection anchor. It
//! executes the motion, selection, search, and viewport-alignment commands that
//! `input` already names. It changes no text: Slice 6 owns the operators.

use std::num::NonZeroU32;

use crate::core::{LineIndex, SourceColumn, TextBuffer};
use crate::input::{Command, Mode};
use crate::settings::{COUNT_MAX, EditorSettings};

use super::cursor::Cursor;
use super::motion;
use super::search::{SearchDirection, SearchQuery};
use super::selection::{BlockAnchor, ModeState, Selection};
use super::viewport::{Viewport, ViewportAlignment};

/// The largest number of repetitions that one motion performs.
///
/// The value is the count maximum of the input resolver, so a motion cannot
/// repeat more often than the resolver accepts.
pub const MOTION_COUNT_MAX: usize = COUNT_MAX as usize;

/// Everything that one command reads beside the editing state.
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

/// The result of one command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutcome {
    /// The command changed the cursor, the mode, or the viewport.
    Applied,
    /// The command names a search, but no query is active or no match exists.
    SearchMissed,
    /// The command names no behavior of this module.
    Unhandled,
}

/// The cursor, the mode, and the selection anchor of one buffer view.
///
/// # Examples
///
/// ```
/// use std::num::NonZeroU16;
///
/// use kvim::core::TextBuffer;
/// use kvim::editor::{CommandContext, CommandOutcome, EditingState, Viewport};
/// use kvim::input::{Command, Mode};
/// use kvim::settings::{EditorSettings, FileSettings};
///
/// let buffer = TextBuffer::from_text("alpha beta\ngamma\n", &FileSettings::default())
///     .expect("the text is small");
/// let settings = EditorSettings::default();
/// let context = CommandContext { buffer: &buffer, settings: &settings, search: None };
/// let rows = NonZeroU16::new(10).expect("the literal 10 is not zero");
/// let cells = NonZeroU16::new(80).expect("the literal 80 is not zero");
///
/// let mut state = EditingState::new(&buffer);
/// let mut viewport = Viewport::new(rows, cells);
/// assert_eq!(state.mode(), Mode::Normal);
///
/// let outcome = state.apply(&context, &mut viewport, Command::MoveNextWordStart, None);
/// assert_eq!(outcome, CommandOutcome::Applied);
/// assert_eq!(state.cursor().column().get(), 6);
///
/// // Visual mode keeps the anchor where the selection started.
/// state.apply(&context, &mut viewport, Command::EnterVisual, None);
/// assert_eq!(state.mode(), Mode::Visual);
/// assert!(state.selection(&buffer).is_some());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditingState {
    mode: ModeState,
    cursor: Cursor,
}

impl EditingState {
    /// Creates the Normal-mode state at the start of a buffer.
    #[must_use]
    pub fn new(buffer: &TextBuffer) -> Self {
        let mode = ModeState::Normal;
        Self {
            mode,
            cursor: Cursor::at_buffer_start(buffer, mode.column_limit()),
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

    /// Executes one motion, selection, search, or alignment command.
    ///
    /// The viewport follows the cursor after every accepted command, except an
    /// explicit alignment command, which overrides the scroll margin. The
    /// function changes no buffer text.
    pub fn apply(
        &mut self,
        context: &CommandContext<'_>,
        viewport: &mut Viewport,
        command: Command,
        count: Option<NonZeroU32>,
    ) -> CommandOutcome {
        let buffer = context.buffer;
        let limit = self.mode.column_limit();
        let repeat = repeat_count(count);
        let cursor = self.cursor;

        match command {
            // Modes. Slice 6 owns the Insert-mode commands, because each one
            // also places the cursor for a following text change.
            Command::ReturnToNormal => self.enter_mode(buffer, Mode::Normal),
            Command::EnterVisual => self.enter_mode(buffer, Mode::Visual),
            Command::EnterVisualLine => self.enter_mode(buffer, Mode::VisualLine),
            Command::EnterVisualBlock => self.enter_mode(buffer, Mode::VisualBlock),

            // Motions.
            Command::MoveLeft => self.cursor = motion::move_left(buffer, cursor, limit, repeat),
            Command::MoveRight => self.cursor = motion::move_right(buffer, cursor, limit, repeat),
            Command::MoveDown => self.cursor = motion::move_down(buffer, cursor, limit, repeat),
            Command::MoveUp => self.cursor = motion::move_up(buffer, cursor, limit, repeat),
            Command::MoveNextWordStart => {
                self.cursor = motion::move_next_word_start(buffer, cursor, limit, repeat);
            }
            Command::MovePreviousWordStart => {
                self.cursor = motion::move_previous_word_start(buffer, cursor, limit, repeat);
            }
            Command::MoveNextWordEnd => {
                self.cursor = motion::move_next_word_end(buffer, cursor, limit, repeat);
            }
            Command::MoveFirstColumn => {
                self.cursor = motion::move_first_column(buffer, cursor, limit);
            }
            Command::MoveFirstNonBlank => {
                self.cursor = motion::move_first_non_blank(buffer, cursor, limit);
            }
            Command::MoveLineEnd => {
                self.cursor = motion::move_line_end(buffer, cursor, limit, repeat);
            }
            // A count before `gg` or `G` names a line, not a number of steps.
            Command::MoveFirstLine => {
                self.cursor = motion::move_to_line(buffer, limit, target_line(count, 0));
            }
            Command::MoveLastLine => {
                let last = buffer.line_count() - 1;
                self.cursor = motion::move_to_line(buffer, limit, target_line(count, last));
            }
            Command::MoveHalfPageDown => {
                let rows = viewport.half_page_rows().saturating_mul(repeat);
                self.cursor = motion::move_down(buffer, cursor, limit, rows);
            }
            Command::MoveHalfPageUp => {
                let rows = viewport.half_page_rows().saturating_mul(repeat);
                self.cursor = motion::move_up(buffer, cursor, limit, rows);
            }
            Command::MoveFullPageDown => {
                let rows = viewport.full_page_rows().saturating_mul(repeat);
                self.cursor = motion::move_down(buffer, cursor, limit, rows);
            }
            Command::MoveFullPageUp => {
                let rows = viewport.full_page_rows().saturating_mul(repeat);
                self.cursor = motion::move_up(buffer, cursor, limit, rows);
            }

            // Viewport alignment. An alignment changes no cursor position.
            Command::CenterCursorLine => {
                *viewport = viewport.aligned(cursor, ViewportAlignment::Center);
                return CommandOutcome::Applied;
            }
            Command::AlignCursorLineTop => {
                *viewport = viewport.aligned(cursor, ViewportAlignment::Top);
                return CommandOutcome::Applied;
            }
            Command::AlignCursorLineBottom => {
                *viewport = viewport.aligned(cursor, ViewportAlignment::Bottom);
                return CommandOutcome::Applied;
            }

            // Search.
            Command::SearchNext | Command::SearchPrevious => {
                let Some(query) = context.search else {
                    return CommandOutcome::SearchMissed;
                };
                let direction = if command == Command::SearchPrevious {
                    query.direction().reversed()
                } else {
                    query.direction()
                };
                let Some(found) = self.repeat_search(context, query, direction, repeat) else {
                    return CommandOutcome::SearchMissed;
                };
                self.cursor = found;
            }

            _ => return CommandOutcome::Unhandled,
        }

        *viewport = viewport.reconciled(buffer, self.cursor, &context.settings.display);
        CommandOutcome::Applied
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
