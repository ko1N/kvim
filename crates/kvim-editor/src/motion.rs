//! The cursor motions of the first release.
//!
//! Every motion clamps to the buffer limits, so a count past the first or the
//! last line stops at that line instead of failing. A horizontal motion sets the
//! preferred column. A vertical motion keeps it. See `docs/input-actions.md`.

use kvim_core::{CharPosition, LineIndex, TextBuffer};

use super::cursor::{ColumnLimit, Cursor, PreferredColumn};
use super::grapheme;
use super::text_object::{CharReader, Delimiter, DelimiterShape, scan_backward, scan_forward};

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
///
/// The count names grapheme clusters, so one step passes a letter and every
/// combining mark that belongs to it.
pub(super) fn move_left(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
) -> Cursor {
    let column = grapheme::column_left(buffer, cursor.line(), cursor.column().get(), count);
    Cursor::clamped(buffer, cursor.line().get(), column, limit)
}

/// Moves the cursor right, and stops at the last column of the line.
///
/// The count names grapheme clusters, exactly as [`move_left`] does.
pub(super) fn move_right(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
) -> Cursor {
    let column = grapheme::column_right(buffer, cursor.line(), cursor.column().get(), count);
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

/// Moves the cursor to the last non-blank character of its line, or of a later
/// line.
///
/// `g_` and `$` differ only when a line ends with blanks: `$` reaches the last
/// column, and this motion reaches the last visible character. The motion keeps
/// the reached column as the preferred column, so later vertical movement
/// returns to it instead of following the line end.
pub(super) fn move_last_non_blank(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
) -> Cursor {
    debug_assert!(count > 0, "the resolver rejects a zero count");
    let line = cursor.line().get().saturating_add(count - 1);
    let clamped = line.min(buffer.line_count() - 1);
    let index = buffer
        .line_index(clamped)
        .expect("the clamp keeps the line index inside the buffer");
    Cursor::clamped(buffer, clamped, last_non_blank_column(buffer, index), limit)
}

/// Moves the cursor to the bracket that matches the pair at or after it.
///
/// Returns `None` when the line of the cursor holds no bracket at or after it,
/// or when the found bracket has no partner inside the scan bound. The caller
/// then changes nothing, as it does for a text object that finds no pair.
///
/// A count repeats the jump, which is the rule for every motion of
/// `docs/input-actions.md`. A jump that finds no further match ends the
/// repetition at the last matched bracket.
pub(super) fn move_matching_bracket(
    buffer: &TextBuffer,
    cursor: Cursor,
    limit: ColumnLimit,
    count: usize,
) -> Option<Cursor> {
    debug_assert!(count > 0, "the resolver rejects a zero count");
    let mut position = cursor.position(buffer);
    let mut matched = None;
    for _ in 0..count {
        let Some(next) = matching_bracket(buffer, position) else {
            break;
        };
        position = next;
        matched = Some(next);
    }
    matched.map(|position| Cursor::at_position(buffer, position, limit))
}

/// Returns the bracket that matches the pair at or after one position.
///
/// The search first reads the line of `from` forward for the first character of
/// [`Delimiter::MATCH_PAIRS`], so a `%` before a bracket jumps to the partner of
/// that bracket, as the reference Vim does. It then walks the buffer toward the
/// partner and counts the nested pairs of the same delimiter, so an inner pair
/// never ends the walk.
///
/// The walk crosses lines inside
/// [`TEXT_OBJECT_SCAN_CHARS_MAX`](super::text_object::TEXT_OBJECT_SCAN_CHARS_MAX),
/// the bound that the bracket text objects already use, because both read the
/// same nesting.
///
/// The search reads text alone. This crate holds no comment and no string
/// region, so a bracket inside a comment or a string literal matches like every
/// other bracket.
///
/// # Examples
///
/// ```
/// use kvim_core::TextBuffer;
/// use kvim_editor::matching_bracket;
/// use kvim_settings::FileSettings;
///
/// let buffer = TextBuffer::from_text("call(alpha)\n", &FileSettings::default())
///     .expect("the text is small");
/// let start = buffer.char_position(0).expect("the buffer holds the position");
/// // The cursor stands before the pair, so the search finds `(` first.
/// assert_eq!(matching_bracket(&buffer, start).map(|found| found.get()), Some(10));
/// ```
#[must_use]
pub fn matching_bracket(buffer: &TextBuffer, from: CharPosition) -> Option<CharPosition> {
    let line = buffer.char_to_line(from);
    let line_start = buffer.line_start(line).get();
    let column = from.get() - line_start;
    let (offset, pair) = buffer
        .line_text(line)
        .chars()
        .enumerate()
        .skip(column)
        .find_map(|(index, character)| BracketPair::of(character).map(|pair| (index, pair)))?;

    let mut reader = CharReader::new(buffer);
    let position = line_start + offset;
    let matched = match pair.side {
        BracketSide::Open => scan_forward(
            &mut reader,
            buffer.len_chars(),
            position + 1,
            pair.open,
            pair.close,
        )?,
        BracketSide::Close => {
            scan_backward(&mut reader, position.checked_sub(1)?, pair.open, pair.close)?
        }
    };
    buffer.char_position(matched).ok()
}

/// Which end of its pair one bracket character names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BracketSide {
    /// The character opens the pair, so the partner follows it.
    Open,
    /// The character closes the pair, so the partner precedes it.
    Close,
}

/// One bracket character resolved against the delimiter table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BracketPair {
    open: char,
    close: char,
    side: BracketSide,
}

impl BracketPair {
    /// Returns the pair of one character, or `None` when it is no bracket.
    fn of(character: char) -> Option<Self> {
        Delimiter::MATCH_PAIRS.iter().find_map(|delimiter| {
            let DelimiterShape::Balanced { open, close } = delimiter.shape() else {
                debug_assert!(false, "Delimiter::MATCH_PAIRS names balanced pairs only");
                return None;
            };
            let side = if character == open {
                BracketSide::Open
            } else if character == close {
                BracketSide::Close
            } else {
                return None;
            };
            Some(Self { open, close, side })
        })
    }
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

/// Returns the end of an operator range over `w`.
///
/// Vim ends the operated text at the end of the last word that the motion moved
/// over, when that word ends at the end of its line. `dw` on the last word of a
/// line therefore removes that word and keeps the line. The plain `w` motion
/// stops on the last character of the line instead, because Normal mode holds
/// the cursor on a character, so an operator needs its own end.
///
/// The returned cursor can stand after the last character of its line, which
/// an exclusive range needs to reach that character.
pub(super) fn operator_next_word_start(
    buffer: &TextBuffer,
    cursor: Cursor,
    count: usize,
) -> Cursor {
    let mut walker = WordWalker::new(buffer);
    let start = WalkPosition {
        line: cursor.line().get(),
        column: cursor.column().get(),
    };
    let mut position = start;
    for _ in 0..count {
        let next = walker.next_word_start(position);
        if next == position {
            break;
        }
        position = next;
    }
    let end = |line: usize, column: usize| {
        Cursor::clamped(buffer, line, column, ColumnLimit::AfterLastCharacter)
    };
    if position.line == start.line {
        return end(position.line, position.column);
    }

    // The walk left the line, so the range stops after the last non-blank
    // character that the walk passed.
    let mut back = position;
    while let Some(previous) = walker.previous(back) {
        back = previous;
        if back == start {
            break;
        }
        if walker.class_at(back) != CharClass::Blank {
            let content_end = walker.line_len(back.line);
            return end(back.line, content_end);
        }
    }
    // The walk passed blanks alone, so it moved over no word and the plain
    // motion target is the end.
    end(position.line, position.column)
}

/// Reports whether the cursor stands on a blank character.
///
/// A position after the last character of a line holds no character, and an
/// empty line holds none either. Both count as blank.
pub(super) fn is_blank_at(buffer: &TextBuffer, cursor: Cursor) -> bool {
    buffer
        .line_text(cursor.line())
        .chars()
        .nth(cursor.column().get())
        .is_none_or(|character| CharClass::of(character) == CharClass::Blank)
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

fn last_non_blank_column(buffer: &TextBuffer, line: LineIndex) -> usize {
    // A line of blanks holds no non-blank character. Vim keeps `g_` in the
    // first column there, so the cursor stays inside the line.
    buffer
        .line_text(line)
        .chars()
        .enumerate()
        .filter(|(_, character)| !character.is_whitespace())
        .map(|(column, _)| column)
        .last()
        .unwrap_or(0)
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
