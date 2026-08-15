//! Staged edit plans and the text changes that are not operators.
//!
//! Every function here is pure. It reads the current buffer and returns one
//! [`EditPlan`]. The plan holds the complete transaction, the register value,
//! and the cursor result. [`EditingState`](super::EditingState) commits the plan
//! in one step, so a rejected plan leaves the buffer unchanged.
//!
//! The module builds no plan that changes text outside one
//! [`EditTransaction`], which `docs/text-model.md` requires.

use crate::core::{
    CharPosition, CharRange, EditTransaction, IndentPolicy, LineIndex, ShiftDirection, TextBuffer,
    TextChange,
};

use super::cursor::Cursor;
use super::register::{RegisterShape, RegisterValue};
use super::selection::Selection;

/// The mode that the editor holds after one plan commits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NextMode {
    /// Keep the current mode, including its selection anchor.
    Keep,
    /// Return to Normal mode.
    Normal,
    /// Enter Insert mode.
    Insert,
}

/// Where the cursor stands after one plan commits.
///
/// The editor resolves the target against the buffer that the transaction
/// produced, so a plan never names a position of the previous buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CursorTarget {
    /// Place the cursor at one line and one column.
    At {
        /// The wanted line.
        line: usize,
        /// The wanted source column.
        column: usize,
    },
    /// Place the cursor at the first non-blank character of one line.
    FirstNonBlank {
        /// The wanted line.
        line: usize,
    },
    /// Place the cursor at one character position of the new buffer.
    Position(usize),
    /// Keep the current line and column, and clamp both again.
    Unchanged,
}

/// One complete, validated change candidate.
#[derive(Clone, Debug)]
pub(super) struct EditPlan {
    /// The transaction, or `None` when the plan changes no text.
    pub(super) transaction: Option<EditTransaction>,
    /// The register value that the plan writes, or `None` to keep the register.
    pub(super) value: Option<RegisterValue>,
    /// The cursor position after the change.
    pub(super) cursor: CursorTarget,
    /// The mode after the change.
    pub(super) next_mode: NextMode,
}

impl EditPlan {
    /// Creates a plan that changes no text and keeps the register.
    pub(super) const fn unchanged() -> Self {
        Self {
            transaction: None,
            value: None,
            cursor: CursorTarget::Unchanged,
            next_mode: NextMode::Keep,
        }
    }
}

/// The automatic indent of one new line.
///
/// A language adapter answers with a level count, and the editor multiplies
/// that count by the shift width, so `EditorSettings` keeps the tab width. Kvim
/// uses [`AutoIndent::PreviousLine`] when no adapter serves the buffer, or when
/// the parse result for the current buffer version is not yet available. The
/// editor never waits for a parse result. See `docs/text-model.md`.
///
/// # Examples
///
/// ```
/// use kvim::editor::AutoIndent;
///
/// // The language adapter reports one level inside a block.
/// let inside_block = AutoIndent::Levels(1);
/// assert_eq!(inside_block, AutoIndent::Levels(1));
/// // A buffer without a parse result keeps the previous-line rule.
/// assert_ne!(inside_block, AutoIndent::PreviousLine);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoIndent {
    /// Use the syntax-tree level count of the language adapter.
    Levels(u16),
    /// Copy the indent of the previous non-empty line.
    PreviousLine,
}

/// The side of the cursor line that a new line appears on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OpenDirection {
    /// Open the new line below the cursor line.
    Below,
    /// Open the new line above the cursor line.
    Above,
}

/// The side of the cursor that a paste puts the register text on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PastePlacement {
    /// Paste before the cursor.
    Before,
    /// Paste after the cursor.
    After,
}

/// The direction that a Visual selection moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MoveDirection {
    /// Move the selected lines one line down.
    Down,
    /// Move the selected lines one line up.
    Up,
}

/// The block edge that a block insert writes at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BlockEdge {
    /// Insert at the left edge of the rectangle.
    Left,
    /// Insert after the right edge of the rectangle.
    Right,
}

/// The rectangle that a pending block insert writes into.
///
/// The value survives the mode change into Insert mode, so the complete typed
/// text reaches every selected line as one transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PendingBlockInsert {
    pub(super) first_line: usize,
    pub(super) last_line: usize,
    pub(super) left: usize,
    pub(super) right: usize,
    pub(super) edge: BlockEdge,
}

/// Returns the text of one character range.
///
/// The reader joins the lines with the line ending of the buffer, because the
/// buffer keeps one line ending for the whole file.
pub(super) fn text_in_range(buffer: &TextBuffer, range: CharRange) -> String {
    let first = buffer.char_to_line(range.start());
    let last = buffer.char_to_line(range.end());
    let start_column = buffer.char_to_column(range.start()).get();
    let end_column = buffer.char_to_column(range.end()).get();
    if first == last {
        return line_slice(buffer, first, start_column, end_column);
    }

    let ending = buffer.line_ending().as_str();
    let mut text = line_slice(buffer, first, start_column, usize::MAX);
    for index in (first.get() + 1)..last.get() {
        let line = line_at(buffer, index);
        text.push_str(ending);
        text.push_str(&buffer.line_text(line));
    }
    text.push_str(ending);
    text.push_str(&line_slice(buffer, last, 0, end_column));
    text
}

/// Returns the text of complete lines, with one line ending after each line.
pub(super) fn lines_text(buffer: &TextBuffer, first: LineIndex, last: LineIndex) -> String {
    let ending = buffer.line_ending().as_str();
    let mut text = String::new();
    for index in first.get()..=last.get() {
        text.push_str(&buffer.line_text(line_at(buffer, index)));
        text.push_str(ending);
    }
    text
}

/// Returns the range that a linewise delete removes.
///
/// The range holds the line ending after the last line. The last line of the
/// buffer holds no line ending, so the range absorbs the line ending before the
/// first line instead. This keeps the remaining lines terminated.
pub(super) fn linewise_delete_range(
    buffer: &TextBuffer,
    first: LineIndex,
    last: LineIndex,
) -> CharRange {
    let (start, end) = if last.get() + 1 < buffer.line_count() {
        let next = line_at(buffer, last.get() + 1);
        (
            buffer.line_start(first).get(),
            buffer.line_start(next).get(),
        )
    } else if first.get() > 0 {
        let previous = line_at(buffer, first.get() - 1);
        (line_content_end(buffer, previous), buffer.len_chars())
    } else {
        (buffer.line_start(first).get(), buffer.len_chars())
    };
    char_range(buffer, start, end)
}

/// Returns the range that holds the content of complete lines.
///
/// The range stops before the line ending of the last line, so a replacement
/// keeps the line structure around the range.
pub(super) fn line_content_range(
    buffer: &TextBuffer,
    first: LineIndex,
    last: LineIndex,
) -> CharRange {
    char_range(
        buffer,
        buffer.line_start(first).get(),
        line_content_end(buffer, last),
    )
}

/// Returns the first and the last line that one selection covers.
pub(super) fn selection_lines(buffer: &TextBuffer, selection: Selection) -> (LineIndex, LineIndex) {
    match selection {
        Selection::Characterwise(range) => {
            let last = range.end().get().saturating_sub(1).max(range.start().get());
            let last = buffer
                .char_position(last.min(buffer.len_chars()))
                .expect("the clamp keeps the position inside the buffer");
            (
                buffer.char_to_line(range.start()),
                buffer.char_to_line(last),
            )
        }
        Selection::Linewise { first, last } => (first, last),
        Selection::Block {
            first_line,
            last_line,
            ..
        } => (first_line, last_line),
    }
}

/// Returns the indent width that the automatic-indent fallback rule produces.
///
/// The rule copies the indent of the previous non-empty line, counted from the
/// given line upward. The syntax-tree rule replaces it whenever a parse result
/// for the current buffer version exists. See `docs/text-model.md`.
pub(super) fn fallback_indent_columns(
    buffer: &TextBuffer,
    indent: IndentPolicy,
    from: LineIndex,
) -> usize {
    for index in (0..=from.get()).rev() {
        let text = buffer.line_text(line_at(buffer, index));
        if !text.trim().is_empty() {
            return indent.measure(&text).columns;
        }
    }
    0
}

/// Returns the indent width of one new line.
///
/// A level count from the language adapter becomes a column count here, so the
/// shift width of `EditorSettings` stays the only source of the indent size.
fn auto_indent_columns(
    buffer: &TextBuffer,
    indent: IndentPolicy,
    from: LineIndex,
    auto: AutoIndent,
) -> usize {
    match auto {
        AutoIndent::Levels(levels) => {
            usize::from(levels).saturating_mul(usize::from(indent.shift_width().get()))
        }
        AutoIndent::PreviousLine => fallback_indent_columns(buffer, indent, from),
    }
}

/// Plans one new line above or below the cursor line.
///
/// The new line and its automatic indent are one transaction, so one undo
/// reverses both.
pub(super) fn plan_open_line(
    buffer: &TextBuffer,
    indent: IndentPolicy,
    cursor: Cursor,
    direction: OpenDirection,
    auto: AutoIndent,
) -> EditPlan {
    let line = cursor.line();
    let ending = buffer.line_ending().as_str();
    let rendered = indent.render(auto_indent_columns(buffer, indent, line, auto));
    let column = rendered.chars().count();
    let (at, new_line, text) = match direction {
        OpenDirection::Below => (
            line_content_end(buffer, line),
            line.get() + 1,
            format!("{ending}{rendered}"),
        ),
        OpenDirection::Above => (
            buffer.line_start(line).get(),
            line.get(),
            format!("{rendered}{ending}"),
        ),
    };
    let at = position(buffer, at);
    EditPlan {
        transaction: Some(EditTransaction::single(
            cursor.position(buffer),
            TextChange::insert(at, text),
        )),
        value: None,
        cursor: CursorTarget::At {
            line: new_line,
            column,
        },
        next_mode: NextMode::Insert,
    }
}

/// Plans one line break at the cursor, with the automatic indent.
///
/// The plan uses the same automatic indent as `o` and `O`, so `Enter` receives
/// the same result. The line break and its indent are one transaction, so one
/// undo reverses both. See `docs/text-model.md`.
///
/// The text after the cursor moves to the new line, behind the indent.
pub(super) fn plan_line_break(
    buffer: &TextBuffer,
    indent: IndentPolicy,
    cursor: Cursor,
    auto: AutoIndent,
) -> EditPlan {
    let at = cursor.position(buffer);
    let rendered = indent.render(auto_indent_columns(buffer, indent, cursor.line(), auto));
    let text = format!("{}{rendered}", buffer.line_ending().as_str());
    let end = at.get() + text.chars().count();
    EditPlan {
        transaction: Some(EditTransaction::single(at, TextChange::insert(at, text))),
        value: None,
        cursor: CursorTarget::Position(end),
        next_mode: NextMode::Keep,
    }
}

/// Plans one delete of the character before the cursor.
///
/// At column zero the plan removes the line ending before the cursor line, so
/// the two lines join. At the start of the buffer the plan changes nothing. The
/// plan writes no register value, because a backward delete does not fill a
/// register in Vim.
pub(super) fn plan_delete_backward(buffer: &TextBuffer, cursor: Cursor) -> EditPlan {
    let at = cursor.position(buffer);
    let line = cursor.line();
    let range = if cursor.column().get() > 0 {
        char_range(buffer, at.get() - 1, at.get())
    } else if line.get() > 0 {
        // The line ending is two characters in a CRLF buffer, so the range
        // starts at the content end of the previous line instead of one
        // character back.
        let previous = line_at(buffer, line.get() - 1);
        char_range(buffer, line_content_end(buffer, previous), at.get())
    } else {
        return EditPlan::unchanged();
    };
    EditPlan {
        transaction: Some(EditTransaction::single(at, TextChange::delete(range))),
        value: None,
        cursor: CursorTarget::Position(range.start().get()),
        next_mode: NextMode::Keep,
    }
}

/// Plans one insertion of typed text at the cursor.
pub(super) fn plan_insert_text(buffer: &TextBuffer, cursor: Cursor, text: &str) -> EditPlan {
    let at = cursor.position(buffer);
    EditPlan {
        transaction: Some(EditTransaction::single(at, TextChange::insert(at, text))),
        value: None,
        cursor: CursorTarget::Position(at.get() + text.chars().count()),
        next_mode: NextMode::Keep,
    }
}

/// Plans one block insert over every selected line.
///
/// A line that is shorter than the block left edge receives no change. The
/// complete block is one transaction, so one undo reverses the whole block.
pub(super) fn plan_block_insert(
    buffer: &TextBuffer,
    cursor: Cursor,
    block: PendingBlockInsert,
    text: &str,
) -> EditPlan {
    let mut changes = Vec::new();
    let mut cursor_column = block.left;
    for index in block.first_line..=block.last_line.min(buffer.line_count() - 1) {
        let line = line_at(buffer, index);
        let len_chars = buffer.line_len_chars(line);
        if len_chars < block.left {
            continue;
        }
        let column = match block.edge {
            BlockEdge::Left => block.left,
            BlockEdge::Right => (block.right + 1).min(len_chars),
        };
        if index == block.first_line {
            cursor_column = column;
        }
        let at = position(buffer, buffer.line_start(line).get() + column);
        changes.push(TextChange::insert(at, text));
    }

    let Ok(transaction) = EditTransaction::new(cursor.position(buffer), changes) else {
        return EditPlan::unchanged();
    };
    EditPlan {
        transaction: Some(transaction),
        value: None,
        cursor: CursorTarget::At {
            line: block.first_line,
            column: cursor_column + text.chars().count(),
        },
        next_mode: NextMode::Keep,
    }
}

/// Plans one line-comment toggle over complete lines.
///
/// The plan removes the token when every affected non-blank line already starts
/// with it, and adds the token otherwise. The token goes behind the existing
/// indent of each line, so the indent survives the toggle. A blank line
/// receives no change. The complete toggle is one transaction, so one undo
/// reverses it. See `docs/language-services.md`.
pub(super) fn plan_toggle_comment(
    buffer: &TextBuffer,
    indent: IndentPolicy,
    cursor: Cursor,
    first: LineIndex,
    last: LineIndex,
    token: &str,
) -> EditPlan {
    let lines: Vec<(LineIndex, String)> = (first.get()..=last.get())
        .map(|index| {
            let line = line_at(buffer, index);
            (line, buffer.line_text(line))
        })
        .filter(|(_, text)| !text.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return EditPlan::unchanged();
    }
    let remove = lines
        .iter()
        .all(|(_, text)| text.trim_start().starts_with(token));

    let mut changes = Vec::with_capacity(lines.len());
    for (line, text) in &lines {
        let start = buffer.line_start(*line).get() + indent.measure(text).char_len;
        let change = if remove {
            // The toggle also removes the one separating space that it wrote.
            let rest: String = text
                .trim_start()
                .chars()
                .skip(token.chars().count())
                .collect();
            let removed = token.chars().count() + usize::from(rest.starts_with(' '));
            TextChange::delete(char_range(buffer, start, start + removed))
        } else {
            TextChange::insert(position(buffer, start), format!("{token} "))
        };
        changes.push(change);
    }

    let Ok(transaction) = EditTransaction::new(cursor.position(buffer), changes) else {
        return EditPlan::unchanged();
    };
    EditPlan {
        transaction: Some(transaction),
        value: None,
        cursor: CursorTarget::Unchanged,
        next_mode: NextMode::Normal,
    }
}

/// Plans one shift of every selected line by one shift width.
///
/// An empty line keeps its shape, like Vim. The plan keeps the current mode, so
/// the Visual selection survives the change.
pub(super) fn plan_shift_lines(
    buffer: &TextBuffer,
    indent: IndentPolicy,
    cursor: Cursor,
    first: LineIndex,
    last: LineIndex,
    direction: ShiftDirection,
) -> EditPlan {
    let mut changes = Vec::new();
    for index in first.get()..=last.get() {
        let line = line_at(buffer, index);
        let text = buffer.line_text(line);
        if text.is_empty() {
            continue;
        }
        let measured = indent.measure(&text);
        let rendered = indent.render(indent.shift_columns(measured.columns, direction));
        let existing: String = text.chars().take(measured.char_len).collect();
        if rendered == existing {
            continue;
        }
        let start = buffer.line_start(line).get();
        let range = char_range(buffer, start, start + measured.char_len);
        changes.push(TextChange::replace(range, rendered));
    }

    let Ok(transaction) = EditTransaction::new(cursor.position(buffer), changes) else {
        return EditPlan::unchanged();
    };
    EditPlan {
        transaction: Some(transaction),
        value: None,
        cursor: CursorTarget::Unchanged,
        next_mode: NextMode::Keep,
    }
}

/// Plans one move of the selected lines by one line, with a reindent.
///
/// The moved lines keep their relative indent. The first moved line takes the
/// indent of the previous non-empty line, which is the automatic-indent
/// fallback rule of `docs/text-model.md`. Returns `None` at the buffer limits,
/// where no move is possible.
pub(super) fn plan_move_lines(
    buffer: &TextBuffer,
    indent: IndentPolicy,
    cursor: Cursor,
    first: LineIndex,
    last: LineIndex,
    direction: MoveDirection,
) -> Option<EditPlan> {
    let (region_first, region_last, block_offset, cursor_line) = match direction {
        MoveDirection::Down => {
            if last.get() + 1 >= buffer.line_count() {
                return None;
            }
            (first.get(), last.get() + 1, 1, cursor.line().get() + 1)
        }
        MoveDirection::Up => {
            if first.get() == 0 {
                return None;
            }
            (
                first.get() - 1,
                last.get(),
                0,
                cursor.line().get().saturating_sub(1),
            )
        }
    };

    let mut lines: Vec<String> = (first.get()..=last.get())
        .map(|index| buffer.line_text(line_at(buffer, index)))
        .collect();
    match direction {
        MoveDirection::Down => lines.insert(0, buffer.line_text(line_at(buffer, region_last))),
        MoveDirection::Up => lines.push(buffer.line_text(line_at(buffer, region_first))),
    }
    reindent_block(indent, buffer, &mut lines, region_first, block_offset);

    let range = char_range(
        buffer,
        buffer.line_start(line_at(buffer, region_first)).get(),
        line_content_end(buffer, line_at(buffer, region_last)),
    );
    let replacement = lines.join(buffer.line_ending().as_str());
    Some(EditPlan {
        transaction: Some(EditTransaction::single(
            cursor.position(buffer),
            TextChange::replace(range, replacement),
        )),
        value: None,
        cursor: CursorTarget::At {
            line: cursor_line,
            column: cursor.column().get(),
        },
        next_mode: NextMode::Keep,
    })
}

/// Plans one paste of a register value beside the cursor.
pub(super) fn plan_paste(
    buffer: &TextBuffer,
    cursor: Cursor,
    value: &RegisterValue,
    placement: PastePlacement,
) -> EditPlan {
    if value.is_empty() {
        return EditPlan::unchanged();
    }
    match value.shape() {
        RegisterShape::Characterwise => plan_characterwise_paste(buffer, cursor, value, placement),
        RegisterShape::Linewise => plan_linewise_paste(buffer, cursor, value, placement),
        RegisterShape::Blockwise => plan_blockwise_paste(buffer, cursor, value, placement),
    }
}

/// Plans one paste that replaces a Visual selection.
///
/// The plan writes no register value, so the source register survives the
/// replacement. `docs/input-actions.md` requires that rule.
pub(super) fn plan_visual_paste(
    buffer: &TextBuffer,
    cursor: Cursor,
    selection: Selection,
    value: &RegisterValue,
) -> EditPlan {
    let ending = buffer.line_ending().as_str();
    match selection {
        Selection::Linewise { first, last } => {
            let text = match value.shape() {
                RegisterShape::Linewise => value
                    .text()
                    .strip_suffix(ending)
                    .unwrap_or(value.text())
                    .to_owned(),
                RegisterShape::Characterwise | RegisterShape::Blockwise => value.text().to_owned(),
            };
            let range = line_content_range(buffer, first, last);
            EditPlan {
                transaction: Some(EditTransaction::single(
                    cursor.position(buffer),
                    TextChange::replace(range, text),
                )),
                value: None,
                cursor: CursorTarget::FirstNonBlank { line: first.get() },
                next_mode: NextMode::Normal,
            }
        }
        Selection::Characterwise(range) => {
            // A linewise value keeps its own lines, so the replacement opens a
            // new line before the pasted lines and leaves the rest behind them.
            let text = match value.shape() {
                RegisterShape::Linewise => format!("{ending}{}", value.text()),
                RegisterShape::Characterwise | RegisterShape::Blockwise => value.text().to_owned(),
            };
            let end = range.start().get() + text.chars().count();
            EditPlan {
                transaction: Some(EditTransaction::single(
                    cursor.position(buffer),
                    TextChange::replace(range, text),
                )),
                value: None,
                cursor: CursorTarget::Position(end.saturating_sub(1)),
                next_mode: NextMode::Normal,
            }
        }
        Selection::Block {
            first_line,
            last_line,
            left,
            right,
        } => {
            let mut changes = Vec::new();
            let mut written = false;
            for index in first_line.get()..=last_line.get() {
                let line = line_at(buffer, index);
                let len_chars = buffer.line_len_chars(line);
                if len_chars < left.get() {
                    continue;
                }
                let start = buffer.line_start(line).get() + left.get();
                let end = buffer.line_start(line).get() + (right.get() + 1).min(len_chars);
                let range = char_range(buffer, start, end);
                if written {
                    if range.len_chars() > 0 {
                        changes.push(TextChange::delete(range));
                    }
                } else {
                    changes.push(TextChange::replace(range, value.text()));
                    written = true;
                }
            }
            let Ok(transaction) = EditTransaction::new(cursor.position(buffer), changes) else {
                return EditPlan::unchanged();
            };
            EditPlan {
                transaction: Some(transaction),
                value: None,
                cursor: CursorTarget::At {
                    line: first_line.get(),
                    column: left.get(),
                },
                next_mode: NextMode::Normal,
            }
        }
    }
}

fn plan_characterwise_paste(
    buffer: &TextBuffer,
    cursor: Cursor,
    value: &RegisterValue,
    placement: PastePlacement,
) -> EditPlan {
    let line = cursor.line();
    let len_chars = buffer.line_len_chars(line);
    let column = match placement {
        PastePlacement::Before => cursor.column().get(),
        PastePlacement::After => (cursor.column().get() + 1).min(len_chars),
    };
    let at = position(buffer, buffer.line_start(line).get() + column);
    let end = at.get() + value.text().chars().count();
    EditPlan {
        transaction: Some(EditTransaction::single(
            cursor.position(buffer),
            TextChange::insert(at, value.text()),
        )),
        value: None,
        cursor: CursorTarget::Position(end.saturating_sub(1)),
        next_mode: NextMode::Keep,
    }
}

fn plan_linewise_paste(
    buffer: &TextBuffer,
    cursor: Cursor,
    value: &RegisterValue,
    placement: PastePlacement,
) -> EditPlan {
    let ending = buffer.line_ending().as_str();
    let line = cursor.line();
    let (at, text, first_line) = match placement {
        PastePlacement::Before => (
            buffer.line_start(line).get(),
            value.text().to_owned(),
            line.get(),
        ),
        PastePlacement::After if line.get() + 1 < buffer.line_count() => (
            buffer.line_start(line_at(buffer, line.get() + 1)).get(),
            value.text().to_owned(),
            line.get() + 1,
        ),
        // The last line holds no line ending, so the paste opens one first.
        PastePlacement::After => (
            line_content_end(buffer, line),
            format!(
                "{ending}{}",
                value.text().strip_suffix(ending).unwrap_or(value.text())
            ),
            line.get() + 1,
        ),
    };
    let at = position(buffer, at);
    EditPlan {
        transaction: Some(EditTransaction::single(
            cursor.position(buffer),
            TextChange::insert(at, text),
        )),
        value: None,
        cursor: CursorTarget::FirstNonBlank { line: first_line },
        next_mode: NextMode::Keep,
    }
}

fn plan_blockwise_paste(
    buffer: &TextBuffer,
    cursor: Cursor,
    value: &RegisterValue,
    placement: PastePlacement,
) -> EditPlan {
    let ending = buffer.line_ending().as_str();
    let line_count = buffer.line_count();
    let len_chars = buffer.line_len_chars(cursor.line());
    let column = match placement {
        PastePlacement::Before => cursor.column().get(),
        PastePlacement::After => (cursor.column().get() + 1).min(len_chars),
    };

    let mut changes = Vec::new();
    let mut appended = String::new();
    for (offset, block_line) in value.block_lines(buffer.line_ending()).iter().enumerate() {
        let index = cursor.line().get() + offset;
        if index >= line_count {
            appended.push_str(ending);
            appended.push_str(&" ".repeat(column));
            appended.push_str(block_line);
            continue;
        }
        let line = line_at(buffer, index);
        let line_len = buffer.line_len_chars(line);
        let insert_column = column.min(line_len);
        let padding = " ".repeat(column - insert_column);
        let at = position(buffer, buffer.line_start(line).get() + insert_column);
        changes.push(TextChange::insert(at, format!("{padding}{block_line}")));
    }
    if !appended.is_empty() {
        changes.push(TextChange::insert(
            position(buffer, buffer.len_chars()),
            appended,
        ));
    }

    let Ok(transaction) = EditTransaction::new(cursor.position(buffer), changes) else {
        return EditPlan::unchanged();
    };
    EditPlan {
        transaction: Some(transaction),
        value: None,
        cursor: CursorTarget::At {
            line: cursor.line().get(),
            column,
        },
        next_mode: NextMode::Keep,
    }
}

/// Aligns the moved block with the previous non-empty line and keeps its shape.
fn reindent_block(
    indent: IndentPolicy,
    buffer: &TextBuffer,
    lines: &mut [String],
    region_first: usize,
    block_offset: usize,
) {
    let Some(block_first) = lines.get(block_offset) else {
        return;
    };
    if block_first.trim().is_empty() {
        return;
    }

    let above = lines[..block_offset]
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| indent.measure(line).columns);
    let target = match (above, region_first) {
        (Some(columns), _) => columns,
        (None, 0) => 0,
        (None, _) => fallback_indent_columns(buffer, indent, line_at(buffer, region_first - 1)),
    };

    let current = indent.measure(block_first).columns as isize;
    let delta = target as isize - current;
    for line in lines.iter_mut().skip(block_offset) {
        if line.trim().is_empty() {
            continue;
        }
        let measured = indent.measure(line);
        let columns = (measured.columns as isize + delta).max(0) as usize;
        let rest: String = line.chars().skip(measured.char_len).collect();
        *line = format!("{}{rest}", indent.render(columns));
    }
}

fn line_at(buffer: &TextBuffer, index: usize) -> LineIndex {
    buffer
        .line_index(index.min(buffer.line_count() - 1))
        .expect("the clamp keeps the line index inside the buffer")
}

fn line_content_end(buffer: &TextBuffer, line: LineIndex) -> usize {
    buffer.line_start(line).get() + buffer.line_len_chars(line)
}

fn line_slice(buffer: &TextBuffer, line: LineIndex, start: usize, end: usize) -> String {
    let text = buffer.line_text(line);
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn position(buffer: &TextBuffer, at: usize) -> CharPosition {
    buffer
        .char_position(at.min(buffer.len_chars()))
        .expect("the clamp keeps the position inside the buffer")
}

fn char_range(buffer: &TextBuffer, start: usize, end: usize) -> CharRange {
    let start = position(buffer, start);
    let end = position(buffer, end.max(start.get()));
    CharRange::new(start, end).expect("the clamp keeps the start before the end")
}
