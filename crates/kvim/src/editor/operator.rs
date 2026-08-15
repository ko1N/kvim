//! The operators, the ranges that they act on, and the plan that they produce.
//!
//! An operator alone changes nothing. It needs one [`OperatorRange`], which only
//! a motion or a Visual selection produces, so an operator without a target
//! cannot be applied.

use crate::core::{CharRange, EditTransaction, IndentPolicy, LineIndex, TextBuffer, TextChange};
use crate::input::Command;

use super::cursor::Cursor;
use super::edit::{
    CursorTarget, EditPlan, NextMode, line_content_range, lines_text, linewise_delete_range,
    text_in_range,
};
use super::register::RegisterValue;
use super::selection::Selection;

/// The three operators of the first release.
///
/// # Examples
///
/// ```
/// use kvim::editor::Operator;
/// use kvim::input::Command;
///
/// // A repeated operator key means linewise: `dd`, `cc`, and `yy`.
/// assert_eq!(Operator::Delete.line_command(), Command::DeleteLine);
/// assert_eq!(Operator::Yank.line_command(), Command::YankLine);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operator {
    /// Remove the range and write the unnamed register.
    Delete,
    /// Remove the range, write the unnamed register, and enter Insert mode.
    Change,
    /// Write the unnamed register and keep the text.
    Yank,
}

impl Operator {
    /// Returns the command that starts this operator over a motion.
    #[must_use]
    pub const fn motion_command(self) -> Command {
        match self {
            Self::Delete => Command::DeleteOverMotion,
            Self::Change => Command::ChangeOverMotion,
            Self::Yank => Command::YankOverMotion,
        }
    }

    /// Returns the linewise command that a repeated operator key names.
    ///
    /// The mapping registry binds no key to these commands. The
    /// operator-pending state emits them when the operator key repeats.
    #[must_use]
    pub const fn line_command(self) -> Command {
        match self {
            Self::Delete => Command::DeleteLine,
            Self::Change => Command::ChangeLine,
            Self::Yank => Command::YankLine,
        }
    }

    const fn next_mode(self) -> NextMode {
        match self {
            Self::Change => NextMode::Insert,
            Self::Delete | Self::Yank => NextMode::Normal,
        }
    }
}

/// How an operator turns one motion into a range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MotionKind {
    /// The range stops before the motion target.
    Exclusive,
    /// The range holds the motion target.
    Inclusive,
    /// The range holds every line between the two positions.
    Linewise,
}

/// Returns how an operator treats one motion command.
///
/// Returns `None` for a command that is not a motion, which aborts a pending
/// operator.
///
/// # Examples
///
/// ```
/// use kvim::editor::{MotionKind, motion_kind};
/// use kvim::input::Command;
///
/// assert_eq!(motion_kind(Command::MoveNextWordStart), Some(MotionKind::Exclusive));
/// assert_eq!(motion_kind(Command::MoveNextWordEnd), Some(MotionKind::Inclusive));
/// assert_eq!(motion_kind(Command::MoveDown), Some(MotionKind::Linewise));
/// assert_eq!(motion_kind(Command::SaveBuffer), None);
/// ```
#[must_use]
pub const fn motion_kind(command: Command) -> Option<MotionKind> {
    match command {
        Command::MoveLeft
        | Command::MoveRight
        | Command::MoveNextWordStart
        | Command::MovePreviousWordStart
        | Command::MoveFirstColumn
        | Command::MoveFirstNonBlank
        | Command::SearchNext
        | Command::SearchPrevious => Some(MotionKind::Exclusive),
        Command::MoveNextWordEnd | Command::MoveLineEnd => Some(MotionKind::Inclusive),
        Command::MoveDown
        | Command::MoveUp
        | Command::MoveFirstLine
        | Command::MoveLastLine
        | Command::MoveHalfPageDown
        | Command::MoveHalfPageUp
        | Command::MoveFullPageDown
        | Command::MoveFullPageUp => Some(MotionKind::Linewise),
        _ => None,
    }
}

/// The text that one operator acts on.
///
/// Only [`OperatorRange::from_motion`] and [`OperatorRange::from_selection`]
/// build a range, so every operator carries a target that the buffer produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OperatorRange {
    /// A run of characters.
    Characterwise(CharRange),
    /// Complete lines.
    Linewise { first: LineIndex, last: LineIndex },
    /// A rectangle of columns over a run of lines.
    Block {
        first_line: LineIndex,
        last_line: LineIndex,
        left: usize,
        right: usize,
    },
}

impl OperatorRange {
    /// Builds the range that one Visual selection names.
    pub(super) const fn from_selection(selection: Selection) -> Self {
        match selection {
            Selection::Characterwise(range) => Self::Characterwise(range),
            Selection::Linewise { first, last } => Self::Linewise { first, last },
            Selection::Block {
                first_line,
                last_line,
                left,
                right,
            } => Self::Block {
                first_line,
                last_line,
                left: left.get(),
                right: right.get(),
            },
        }
    }

    /// Builds the range between the cursor before and after one motion.
    pub(super) fn from_motion(
        buffer: &TextBuffer,
        before: Cursor,
        after: Cursor,
        kind: MotionKind,
    ) -> Self {
        if kind == MotionKind::Linewise {
            return Self::Linewise {
                first: before.line().min(after.line()),
                last: before.line().max(after.line()),
            };
        }

        let first = before.position(buffer);
        let second = after.position(buffer);
        let start = first.min(second);
        let mut end = first.max(second);
        if kind == MotionKind::Inclusive {
            end = buffer
                .char_position((end.get() + 1).min(buffer.len_chars()))
                .expect("the clamp keeps the position inside the buffer");
        } else if buffer.char_to_line(end) > buffer.char_to_line(start)
            && buffer.char_to_column(end).get() == 0
        {
            // Vim moves an exclusive end at the first column of a later line
            // back to the end of the previous line, so `dw` on the last word of
            // a line keeps the line break.
            let previous = buffer
                .line_index(buffer.char_to_line(end).get() - 1)
                .expect("a later line always follows an earlier line");
            let content_end = buffer.line_start(previous).get() + buffer.line_len_chars(previous);
            end = buffer
                .char_position(content_end.max(start.get()))
                .expect("the content end stays inside the buffer");
        }
        Self::Characterwise(
            CharRange::new(start, end).expect("the clamp keeps the start before the end"),
        )
    }
}

/// Builds the complete change candidate of one operator over one range.
///
/// The plan holds the transaction, the register value, the cursor result, and
/// the next mode. The caller commits it in one step.
pub(super) fn plan_operator(
    buffer: &TextBuffer,
    indent: IndentPolicy,
    cursor: Cursor,
    operator: Operator,
    range: OperatorRange,
) -> EditPlan {
    match range {
        OperatorRange::Characterwise(chars) => plan_characterwise(buffer, cursor, operator, chars),
        OperatorRange::Linewise { first, last } => {
            plan_linewise(buffer, indent, cursor, operator, first, last)
        }
        OperatorRange::Block {
            first_line,
            last_line,
            left,
            right,
        } => plan_block(buffer, cursor, operator, first_line, last_line, left, right),
    }
}

fn plan_characterwise(
    buffer: &TextBuffer,
    cursor: Cursor,
    operator: Operator,
    range: CharRange,
) -> EditPlan {
    let value = RegisterValue::characterwise(text_in_range(buffer, range));
    let start = range.start();
    let target = CursorTarget::At {
        line: buffer.char_to_line(start).get(),
        column: buffer.char_to_column(start).get(),
    };
    let transaction = match operator {
        Operator::Yank => None,
        Operator::Delete | Operator::Change if range.len_chars() == 0 => None,
        Operator::Delete | Operator::Change => Some(EditTransaction::single(
            cursor.position(buffer),
            TextChange::delete(range),
        )),
    };
    EditPlan {
        transaction,
        value: Some(value),
        cursor: target,
        next_mode: operator.next_mode(),
    }
}

fn plan_linewise(
    buffer: &TextBuffer,
    indent: IndentPolicy,
    cursor: Cursor,
    operator: Operator,
    first: LineIndex,
    last: LineIndex,
) -> EditPlan {
    let value = RegisterValue::linewise(lines_text(buffer, first, last), buffer.line_ending());
    match operator {
        Operator::Yank => EditPlan {
            transaction: None,
            value: Some(value),
            cursor: CursorTarget::At {
                line: first.get(),
                column: cursor.column().get(),
            },
            next_mode: NextMode::Normal,
        },
        Operator::Delete => {
            let range = linewise_delete_range(buffer, first, last);
            let transaction = (range.len_chars() > 0).then(|| {
                EditTransaction::single(cursor.position(buffer), TextChange::delete(range))
            });
            EditPlan {
                transaction,
                value: Some(value),
                cursor: CursorTarget::FirstNonBlank { line: first.get() },
                next_mode: NextMode::Normal,
            }
        }
        Operator::Change => {
            // A linewise change keeps one line and its indent, like Vim with
            // automatic indent, so the following insert starts at that indent.
            let rendered = indent.render(indent.measure(&buffer.line_text(first)).columns);
            let column = rendered.chars().count();
            let range = line_content_range(buffer, first, last);
            EditPlan {
                transaction: Some(EditTransaction::single(
                    cursor.position(buffer),
                    TextChange::replace(range, rendered),
                )),
                value: Some(value),
                cursor: CursorTarget::At {
                    line: first.get(),
                    column,
                },
                next_mode: NextMode::Insert,
            }
        }
    }
}

fn plan_block(
    buffer: &TextBuffer,
    cursor: Cursor,
    operator: Operator,
    first_line: LineIndex,
    last_line: LineIndex,
    left: usize,
    right: usize,
) -> EditPlan {
    let mut pieces = Vec::new();
    let mut changes = Vec::new();
    for index in first_line.get()..=last_line.get() {
        let line = buffer
            .line_index(index)
            .expect("the selection lines come from the buffer");
        let len_chars = buffer.line_len_chars(line);
        // A line that is shorter than the block left edge receives no change.
        if len_chars < left {
            pieces.push(String::new());
            continue;
        }
        let start = buffer
            .char_position(buffer.line_start(line).get() + left)
            .expect("the left edge stays inside the line");
        let end = buffer
            .char_position(buffer.line_start(line).get() + (right + 1).min(len_chars))
            .expect("the right edge clamps to the line length");
        let range = CharRange::new(start, end).expect("the left edge precedes the right edge");
        pieces.push(text_in_range(buffer, range));
        if range.len_chars() > 0 {
            changes.push(TextChange::delete(range));
        }
    }

    let value = RegisterValue::blockwise(&pieces, buffer.line_ending());
    let transaction = match operator {
        Operator::Yank => None,
        // A block larger than the transaction bound applies no change, so the
        // buffer never holds half of a rectangle.
        Operator::Delete | Operator::Change => {
            EditTransaction::new(cursor.position(buffer), changes).ok()
        }
    };
    EditPlan {
        transaction,
        value: Some(value),
        cursor: CursorTarget::At {
            line: first_line.get(),
            column: left,
        },
        next_mode: operator.next_mode(),
    }
}
