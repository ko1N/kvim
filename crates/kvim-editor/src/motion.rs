//! The cursor motions of the first release.
//!
//! Every motion clamps to the buffer limits, so a count past the first or the
//! last line stops at that line instead of failing. A horizontal motion sets the
//! preferred column. A vertical motion keeps it. See `docs/input-actions.md`.

use kvim_core::{LineIndex, TextBuffer};

use super::cursor::{ColumnLimit, Cursor, PreferredColumn};

/// The character class that the word motions compare.
///
/// Vim moves over runs of one class. A run of word characters, a run of
/// punctuation, and a run of blanks are three separate words, so `w` stops
/// between `foo` and `.` in `foo.bar`.
///
/// A word character is a letter, a digit, or the underscore. The rule follows
/// the reference Vim `iskeyword` default, and it accepts every Unicode letter
/// and digit, so a word motion crosses a multi-byte word as one run.
///
/// # Examples
///
/// ```
/// use kvim_editor::CharClass;
///
/// assert_eq!(CharClass::of('a'), CharClass::Word);
/// assert_eq!(CharClass::of('_'), CharClass::Word);
/// assert_eq!(CharClass::of('ß'), CharClass::Word);
/// assert_eq!(CharClass::of('.'), CharClass::Punctuation);
/// assert_eq!(CharClass::of('\t'), CharClass::Blank);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CharClass {
    /// A space, a tab, or another whitespace character.
    Blank,
    /// A letter, a digit, or the underscore.
    Word,
    /// Any other visible character.
    Punctuation,
}

impl CharClass {
    /// Returns the class of one character.
    #[must_use]
    pub fn of(character: char) -> Self {
        if character.is_whitespace() {
            Self::Blank
        } else if character.is_alphanumeric() || character == '_' {
            Self::Word
        } else {
            Self::Punctuation
        }
    }
}

/// Moves the cursor left, and stops at the first column of the line.
pub(super) fn move_left(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
) -> Cursor {
    let column = cursor.column().get().saturating_sub(count);
    Cursor::clamped(buffer, cursor.line().get(), column, limit)
}

/// Moves the cursor right, and stops at the last column of the line.
pub(super) fn move_right(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
) -> Cursor {
    let column = cursor.column().get().saturating_add(count);
    Cursor::clamped(buffer, cursor.line().get(), column, limit)
}

/// Moves the cursor down, and keeps the preferred column.
pub(super) fn move_down(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    rows: usize,
) -> Cursor {
    let line = cursor.line().get().saturating_add(rows);
    move_to_line_keeping_column(buffer, cursor, limit, line)
}

/// Moves the cursor up, and keeps the preferred column.
pub(super) fn move_up(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    rows: usize,
) -> Cursor {
    let line = cursor.line().get().saturating_sub(rows);
    move_to_line_keeping_column(buffer, cursor, limit, line)
}

/// Moves the cursor to the first column of its line.
pub(super) fn move_first_column(buffer: &TextBuffer, cursor: Cursor, limit: ColumnLimit) -> Cursor {
    Cursor::clamped(buffer, cursor.line().get(), 0, limit)
}

/// Moves the cursor to the first non-blank character of its line.
pub(super) fn move_first_non_blank(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
) -> Cursor {
    let column = first_non_blank_column(buffer, cursor.line());
    Cursor::clamped(buffer, cursor.line().get(), column, limit)
}

/// Moves the cursor to the end of its line, or to the end of a later line.
///
/// The motion selects [`PreferredColumn::LineEnd`], so later vertical movement
/// stays at the end of every line.
pub(super) fn move_line_end(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
) -> Cursor {
    debug_assert!(count > 0, "the resolver rejects a zero count");
    let line = cursor.line().get().saturating_add(count - 1);
    Cursor::clamped_with_preferred(buffer, line, usize::MAX, PreferredColumn::LineEnd, limit)
}

/// Moves the cursor to the first non-blank character of one line.
///
/// The `gg` and `G` motions use this rule.
pub(super) fn move_to_line(buffer: &TextBuffer, limit: ColumnLimit, line: usize) -> Cursor {
    let clamped = line.min(buffer.line_count() - 1);
    let index = buffer
        .line_index(clamped)
        .expect("the clamp keeps the line index inside the buffer");
    Cursor::clamped(
        buffer,
        clamped,
        first_non_blank_column(buffer, index),
        limit,
    )
}

/// Moves the cursor to the start of the next word.
pub(super) fn move_next_word_start(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
) -> Cursor {
    repeat_walk(buffer, cursor, limit, count, WordWalker::next_word_start)
}

/// Moves the cursor to the start of the previous word.
pub(super) fn move_previous_word_start(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
) -> Cursor {
    repeat_walk(
        buffer,
        cursor,
        limit,
        count,
        WordWalker::previous_word_start,
    )
}

/// Moves the cursor to the end of the next word.
pub(super) fn move_next_word_end(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
) -> Cursor {
    repeat_walk(buffer, cursor, limit, count, WordWalker::next_word_end)
}

fn move_to_line_keeping_column(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    line: usize,
) -> Cursor {
    let clamped = line.min(buffer.line_count() - 1);
    let index = buffer
        .line_index(clamped)
        .expect("the clamp keeps the line index inside the buffer");
    let column = cursor
        .preferred_column()
        .resolve(limit.last_column(buffer.line_len_chars(index)));
    Cursor::clamped_with_preferred(buffer, clamped, column, cursor.preferred_column(), limit)
}

fn first_non_blank_column(buffer: &TextBuffer, line: LineIndex) -> usize {
    let text = buffer.line_text(line);
    // A line of blanks holds no non-blank character. Vim moves to its last
    // character, so the cursor stays inside the line.
    text.chars()
        .position(|character| !character.is_whitespace())
        .unwrap_or_else(|| text.chars().count().saturating_sub(1))
}

/// Repeats one word walk and stops as soon as the walk makes no progress.
///
/// The early stop bounds the work of a large count at the buffer limits.
fn repeat_walk<'a>(
    buffer: &'a TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
    step: fn(&mut WordWalker<'a>, WalkPosition) -> WalkPosition,
) -> Cursor {
    let mut walker = WordWalker::new(buffer);
    let mut position = WalkPosition {
        line: cursor.line().get(),
        column: cursor.column().get(),
    };
    for _ in 0..count {
        let next = step(&mut walker, position);
        if next == position {
            break;
        }
        position = next;
    }
    Cursor::clamped(buffer, position.line, position.column, limit)
}

/// One position in the character stream of the buffer.
///
/// The column at the line length is the line terminator. On the last line it is
/// the end of the buffer. The walker needs that extra slot, because a word run
/// must stop at a line break.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WalkPosition {
    line: usize,
    column: usize,
}

/// A character reader for the word motions.
///
/// The reader keeps the characters of one line, because a word walk moves
/// through neighboring positions and reads the same line many times.
struct WordWalker<'a> {
    buffer: &'a TextBuffer,
    line_count: usize,
    cached_line: Option<usize>,
    cached_chars: Vec<char>,
}

impl<'a> WordWalker<'a> {
    fn new(buffer: &'a TextBuffer) -> Self {
        Self {
            buffer,
            line_count: buffer.line_count(),
            cached_line: None,
            cached_chars: Vec::new(),
        }
    }

    fn chars(&mut self, line: usize) -> &[char] {
        if self.cached_line != Some(line) {
            let index = self
                .buffer
                .line_index(line)
                .expect("the walker never leaves the buffer lines");
            self.cached_chars = self.buffer.line_text(index).chars().collect();
            self.cached_line = Some(line);
        }
        &self.cached_chars
    }

    fn line_len(&mut self, line: usize) -> usize {
        self.chars(line).len()
    }

    fn class_at(&mut self, position: WalkPosition) -> CharClass {
        self.chars(position.line)
            .get(position.column)
            .map_or(CharClass::Blank, |character| CharClass::of(*character))
    }

    /// Reports whether the position is the only slot of an empty line.
    ///
    /// Vim treats an empty line as one word, so `w` and `b` stop on it.
    fn is_empty_line(&mut self, position: WalkPosition) -> bool {
        self.line_len(position.line) == 0
    }

    fn next(&mut self, position: WalkPosition) -> Option<WalkPosition> {
        if position.column < self.line_len(position.line) {
            return Some(WalkPosition {
                line: position.line,
                column: position.column + 1,
            });
        }
        if position.line + 1 < self.line_count {
            return Some(WalkPosition {
                line: position.line + 1,
                column: 0,
            });
        }
        None
    }

    fn previous(&mut self, position: WalkPosition) -> Option<WalkPosition> {
        if position.column > 0 {
            return Some(WalkPosition {
                line: position.line,
                column: position.column - 1,
            });
        }
        if position.line > 0 {
            let line = position.line - 1;
            return Some(WalkPosition {
                line,
                column: self.line_len(line),
            });
        }
        None
    }

    fn last(&mut self) -> WalkPosition {
        let line = self.line_count - 1;
        WalkPosition {
            line,
            column: self.line_len(line),
        }
    }

    fn next_word_start(&mut self, from: WalkPosition) -> WalkPosition {
        let start_class = self.class_at(from);
        let mut position = from;
        if start_class != CharClass::Blank {
            while let Some(next) = self.next(position) {
                if self.class_at(next) != start_class {
                    break;
                }
                position = next;
            }
        }
        let Some(mut position) = self.next(position) else {
            return self.last();
        };
        loop {
            if self.class_at(position) != CharClass::Blank || self.is_empty_line(position) {
                return position;
            }
            match self.next(position) {
                Some(next) => position = next,
                None => return position,
            }
        }
    }

    fn next_word_end(&mut self, from: WalkPosition) -> WalkPosition {
        let Some(mut position) = self.next(from) else {
            return from;
        };
        // An empty line holds no character, so `e` passes over it.
        while self.class_at(position) == CharClass::Blank {
            match self.next(position) {
                Some(next) => position = next,
                None => return position,
            }
        }
        let class = self.class_at(position);
        while let Some(next) = self.next(position) {
            if self.class_at(next) != class {
                break;
            }
            position = next;
        }
        position
    }

    fn previous_word_start(&mut self, from: WalkPosition) -> WalkPosition {
        let Some(mut position) = self.previous(from) else {
            return from;
        };
        loop {
            if self.class_at(position) != CharClass::Blank {
                break;
            }
            if self.is_empty_line(position) {
                return position;
            }
            match self.previous(position) {
                Some(previous) => position = previous,
                None => return position,
            }
        }
        let class = self.class_at(position);
        while let Some(previous) = self.previous(position) {
            if self.class_at(previous) != class {
                break;
            }
            position = previous;
        }
        position
    }
}
