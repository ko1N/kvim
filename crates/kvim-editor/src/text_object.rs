//! The word, bracket, and quote text objects.
//!
//! A text object names a range around the cursor without moving it first, so an
//! operator takes it as a target and a Visual mode takes it as a selection. The
//! module is pure: it reads the buffer and returns a range. Every scan is
//! bounded, and an object that finds no pair returns `None`, so the buffer stays
//! unchanged. See `docs/input-actions.md`.

use kvim_core::{CharRange, TextBuffer};
use kvim_input::Command;

use super::cursor::Cursor;
use super::motion::CharClass;

/// The largest number of characters that one delimiter scan reads in one
/// direction.
///
/// A balanced pair matches across lines, so the scan needs an explicit stop. A
/// pair that does not close inside this bound names no object.
pub const TEXT_OBJECT_SCAN_CHARS_MAX: usize = 1024 * 1024;

/// How much of one text object the caller takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextObjectScope {
    /// The text between the delimiters, or the word run alone. Vim writes `i`.
    Inner,
    /// The text with its delimiters, or the word run with its blanks. Vim
    /// writes `a`.
    Around,
}

/// The run of characters that one word object selects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WordKind {
    /// `w`: one run of word characters, one run of punctuation, or one run of
    /// blanks.
    Word,
    /// `W`: one run of non-blank characters, or one run of blanks.
    LongWord,
}

impl WordKind {
    /// Returns the run class of one character.
    ///
    /// `W` joins punctuation and word characters into one run, which is the
    /// only difference between the two word objects.
    #[must_use]
    pub fn class(self, character: char) -> CharClass {
        let class = CharClass::of(character);
        match self {
            Self::Word => class,
            Self::LongWord => match class {
                CharClass::Blank => CharClass::Blank,
                CharClass::Word | CharClass::Punctuation => CharClass::Word,
            },
        }
    }
}

/// One delimiter pair that a text object matches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delimiter {
    /// The round brackets `(` and `)`.
    Paren,
    /// The square brackets `[` and `]`.
    Bracket,
    /// The curly brackets `{` and `}`.
    Brace,
    /// The angle brackets `<` and `>`.
    Angle,
    /// The double quote `"`.
    DoubleQuote,
    /// The single quote `'`.
    SingleQuote,
    /// The backtick.
    Backtick,
}

/// The characters of one delimiter pair, and how the pair repeats inside itself.
///
/// The two facts are correlated: a pair whose open character equals its close
/// character cannot nest, because no reader tells the two apart. The shape
/// carries both, so an unbalanced combination cannot be built.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DelimiterShape {
    /// The open character differs from the close character, so one pair holds
    /// another pair and a count names an outer pair. The scan crosses lines.
    Balanced {
        /// The character that opens the pair.
        open: char,
        /// The character that closes the pair.
        close: char,
    },
    /// One character both opens and closes the pair, so the pair never nests.
    /// The scan pairs the quotes of the cursor line from its first column,
    /// because that is the only unambiguous rule for a repeated character.
    Flat {
        /// The character that opens and closes the pair.
        quote: char,
    },
}

impl Delimiter {
    /// The pairs that the matching-bracket motion jumps between.
    ///
    /// The list follows the reference Vim `matchpairs` default. The angle
    /// brackets stay out of it, because `<` and `>` are comparison operators in
    /// most languages, so `%` would jump between two unrelated characters.
    ///
    /// ```
    /// use kvim_editor::{Delimiter, DelimiterShape};
    ///
    /// for delimiter in Delimiter::MATCH_PAIRS {
    ///     assert!(matches!(delimiter.shape(), DelimiterShape::Balanced { .. }));
    /// }
    /// ```
    pub const MATCH_PAIRS: &'static [Self] = &[Self::Paren, Self::Bracket, Self::Brace];

    /// Returns the canonical shape of the pair.
    ///
    /// This table is the only place that names the delimiter characters.
    ///
    /// ```
    /// use kvim_editor::{Delimiter, DelimiterShape};
    ///
    /// assert_eq!(
    ///     Delimiter::Paren.shape(),
    ///     DelimiterShape::Balanced { open: '(', close: ')' }
    /// );
    /// assert_eq!(
    ///     Delimiter::Backtick.shape(),
    ///     DelimiterShape::Flat { quote: '`' }
    /// );
    /// ```
    #[must_use]
    pub const fn shape(self) -> DelimiterShape {
        match self {
            Self::Paren => DelimiterShape::Balanced {
                open: '(',
                close: ')',
            },
            Self::Bracket => DelimiterShape::Balanced {
                open: '[',
                close: ']',
            },
            Self::Brace => DelimiterShape::Balanced {
                open: '{',
                close: '}',
            },
            Self::Angle => DelimiterShape::Balanced {
                open: '<',
                close: '>',
            },
            Self::DoubleQuote => DelimiterShape::Flat { quote: '"' },
            Self::SingleQuote => DelimiterShape::Flat { quote: '\'' },
            Self::Backtick => DelimiterShape::Flat { quote: '`' },
        }
    }
}

/// What one text object selects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextObjectKind {
    /// A run of characters, as `w` and `W` name it.
    Word(WordKind),
    /// The text of one delimiter pair.
    Delimiter(Delimiter),
}

/// One text object: what it selects, and how much of it the caller takes.
///
/// # Examples
///
/// ```
/// use kvim_core::TextBuffer;
/// use kvim_editor::{ColumnLimit, Cursor, TextObject, TextObjectScope};
/// use kvim_input::Command;
/// use kvim_settings::FileSettings;
///
/// let buffer = TextBuffer::from_text("call(alpha)\n", &FileSettings::default())
///     .expect("the text is small");
/// let cursor = Cursor::clamped(&buffer, 0, 6, ColumnLimit::LastCharacter);
///
/// let object = TextObject::of_command(Command::SelectInnerParen)
///     .expect("the command names a text object");
/// assert_eq!(object.scope(), TextObjectScope::Inner);
///
/// let range = object.range(&buffer, cursor, 1).expect("the pair closes");
/// assert_eq!(range.start().get(), 5);
/// assert_eq!(range.end().get(), 10);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextObject {
    kind: TextObjectKind,
    scope: TextObjectScope,
}

impl TextObject {
    /// Returns the text object that one command names.
    ///
    /// Returns `None` for every command that is no text object, which aborts a
    /// waiting operator, exactly as a command that is no motion does.
    #[must_use]
    pub const fn of_command(command: Command) -> Option<Self> {
        use TextObjectScope::{Around, Inner};

        let (kind, scope) = match command {
            Command::SelectInnerWord => (TextObjectKind::Word(WordKind::Word), Inner),
            Command::SelectAroundWord => (TextObjectKind::Word(WordKind::Word), Around),
            Command::SelectInnerLongWord => (TextObjectKind::Word(WordKind::LongWord), Inner),
            Command::SelectAroundLongWord => (TextObjectKind::Word(WordKind::LongWord), Around),
            Command::SelectInnerParen => (TextObjectKind::Delimiter(Delimiter::Paren), Inner),
            Command::SelectAroundParen => (TextObjectKind::Delimiter(Delimiter::Paren), Around),
            Command::SelectInnerBracket => (TextObjectKind::Delimiter(Delimiter::Bracket), Inner),
            Command::SelectAroundBracket => (TextObjectKind::Delimiter(Delimiter::Bracket), Around),
            Command::SelectInnerBrace => (TextObjectKind::Delimiter(Delimiter::Brace), Inner),
            Command::SelectAroundBrace => (TextObjectKind::Delimiter(Delimiter::Brace), Around),
            Command::SelectInnerAngle => (TextObjectKind::Delimiter(Delimiter::Angle), Inner),
            Command::SelectAroundAngle => (TextObjectKind::Delimiter(Delimiter::Angle), Around),
            Command::SelectInnerDoubleQuote => {
                (TextObjectKind::Delimiter(Delimiter::DoubleQuote), Inner)
            }
            Command::SelectAroundDoubleQuote => {
                (TextObjectKind::Delimiter(Delimiter::DoubleQuote), Around)
            }
            Command::SelectInnerSingleQuote => {
                (TextObjectKind::Delimiter(Delimiter::SingleQuote), Inner)
            }
            Command::SelectAroundSingleQuote => {
                (TextObjectKind::Delimiter(Delimiter::SingleQuote), Around)
            }
            Command::SelectInnerBacktick => (TextObjectKind::Delimiter(Delimiter::Backtick), Inner),
            Command::SelectAroundBacktick => {
                (TextObjectKind::Delimiter(Delimiter::Backtick), Around)
            }
            _ => return None,
        };
        Some(Self { kind, scope })
    }

    /// Returns what the object selects.
    #[must_use]
    pub const fn kind(self) -> TextObjectKind {
        self.kind
    }

    /// Returns how much of the object the caller takes.
    #[must_use]
    pub const fn scope(self) -> TextObjectScope {
        self.scope
    }

    /// Returns the character range that the object names around the cursor.
    ///
    /// Returns `None` when no pair encloses the cursor inside the scan bound,
    /// so the caller changes nothing. The count names an outer pair, which only
    /// a balanced pair has.
    #[must_use]
    pub fn range(self, buffer: &TextBuffer, cursor: Cursor, count: usize) -> Option<CharRange> {
        debug_assert!(
            count > 0,
            "the caller turns the optional count into at least one repetition"
        );
        let (start, end) = match self.kind {
            TextObjectKind::Word(word) => word_bounds(buffer, cursor, word, self.scope, count)?,
            TextObjectKind::Delimiter(delimiter) => match delimiter.shape() {
                DelimiterShape::Balanced { open, close } => {
                    balanced_bounds(buffer, cursor, open, close, self.scope, count)?
                }
                DelimiterShape::Flat { quote } => {
                    flat_bounds(buffer, cursor, quote, self.scope, count)?
                }
            },
        };
        let start = buffer.char_position(start).ok()?;
        let end = buffer.char_position(end).ok()?;
        CharRange::new(start, end).ok()
    }
}

/// The characters of one loaded line and the buffer position of its first
/// character.
struct LoadedLine {
    start: usize,
    chars: Vec<char>,
}

/// A character reader over the whole buffer.
///
/// The reader keeps the characters of one line, because a delimiter scan visits
/// neighbouring positions and reads the same line many times. It answers in
/// characters, never in bytes, so a multi-byte body reads like any other.
pub(super) struct CharReader<'a> {
    buffer: &'a TextBuffer,
    len_chars: usize,
    line: Option<LoadedLine>,
}

impl<'a> CharReader<'a> {
    pub(super) fn new(buffer: &'a TextBuffer) -> Self {
        Self {
            buffer,
            len_chars: buffer.len_chars(),
            line: None,
        }
    }

    /// Returns the character at one buffer position.
    ///
    /// A position on a line terminator answers with a line break, which equals
    /// no delimiter. A position at the end of the buffer answers with `None`.
    pub(super) fn char_at(&mut self, position: usize) -> Option<char> {
        if position >= self.len_chars {
            return None;
        }
        let line = self.load(position);
        Some(
            line.chars
                .get(position - line.start)
                .copied()
                .unwrap_or('\n'),
        )
    }

    fn load(&mut self, position: usize) -> &LoadedLine {
        let loaded = self.line.as_ref().is_some_and(|line| {
            position >= line.start && position <= line.start + line.chars.len()
        });
        if !loaded {
            let at = self
                .buffer
                .char_position(position)
                .expect("the caller stays inside the buffer");
            let index = self.buffer.char_to_line(at);
            self.line = Some(LoadedLine {
                start: self.buffer.line_start(index).get(),
                chars: self.buffer.line_text(index).chars().collect(),
            });
        }
        self.line
            .as_ref()
            .expect("the branch above loads the line of the position")
    }
}

/// Returns the first and the last character position of one balanced pair.
///
/// The count walks outward: each repetition looks for the pair that holds the
/// previous one, and a pair that does not hold it ends the walk.
fn balanced_bounds(
    buffer: &TextBuffer,
    cursor: Cursor,
    open: char,
    close: char,
    scope: TextObjectScope,
    count: usize,
) -> Option<(usize, usize)> {
    let mut reader = CharReader::new(buffer);
    let len_chars = buffer.len_chars();
    let from = cursor.position(buffer).get();
    let mut pair = enclosing_pair(&mut reader, len_chars, from, open, close)?;
    for _ in 1..count {
        let outside = pair.0.checked_sub(1)?;
        let outer = enclosing_pair(&mut reader, len_chars, outside, open, close)?;
        if outer.0 >= pair.0 || outer.1 <= pair.1 {
            return None;
        }
        pair = outer;
    }
    let (open_index, close_index) = pair;
    Some(match scope {
        TextObjectScope::Inner => (open_index + 1, close_index),
        TextObjectScope::Around => (open_index, close_index + 1),
    })
}

/// Returns the pair that holds one position, inclusive of both delimiters.
///
/// A cursor on a delimiter belongs to that pair, so the scan starts on the
/// other side of it.
fn enclosing_pair(
    reader: &mut CharReader<'_>,
    len_chars: usize,
    from: usize,
    open: char,
    close: char,
) -> Option<(usize, usize)> {
    let at = reader.char_at(from);
    let open_index = if at == Some(open) {
        from
    } else {
        let start = if at == Some(close) {
            from.checked_sub(1)?
        } else {
            from
        };
        scan_backward(reader, start, open, close)?
    };
    let close_index = if at == Some(close) {
        from
    } else {
        let start = if at == Some(open) { from + 1 } else { from };
        scan_forward(reader, len_chars, start, open, close)?
    };
    debug_assert!(
        open_index <= close_index,
        "the scan starts inside the pair, so the open delimiter never follows the close delimiter"
    );
    Some((open_index, close_index))
}

/// Scans backward for the open delimiter that no closer inside the run matched.
pub(super) fn scan_backward(
    reader: &mut CharReader<'_>,
    from: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0usize;
    let mut position = from;
    for _ in 0..TEXT_OBJECT_SCAN_CHARS_MAX {
        let character = reader.char_at(position)?;
        if character == close {
            depth += 1;
        } else if character == open {
            if depth == 0 {
                return Some(position);
            }
            depth -= 1;
        }
        position = position.checked_sub(1)?;
    }
    None
}

/// Scans forward for the close delimiter that no opener inside the run matched.
pub(super) fn scan_forward(
    reader: &mut CharReader<'_>,
    len_chars: usize,
    from: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0usize;
    let last = len_chars.min(from.saturating_add(TEXT_OBJECT_SCAN_CHARS_MAX));
    for position in from..last {
        let character = reader.char_at(position)?;
        if character == open {
            depth += 1;
        } else if character == close {
            if depth == 0 {
                return Some(position);
            }
            depth -= 1;
        }
    }
    None
}

/// Returns the first and the last character position of one flat pair.
///
/// A flat pair never nests, so the quotes of the cursor line pair from its first
/// column, and a count above one names no outer pair. The loop reads one line,
/// which the file-size bound already limits.
fn flat_bounds(
    buffer: &TextBuffer,
    cursor: Cursor,
    quote: char,
    scope: TextObjectScope,
    count: usize,
) -> Option<(usize, usize)> {
    if count > 1 {
        return None;
    }
    let line = cursor.line();
    let line_start = buffer.line_start(line).get();
    let cursor_column = cursor.column().get();
    let mut open: Option<usize> = None;
    for (column, character) in buffer.line_text(line).chars().enumerate() {
        if character != quote {
            continue;
        }
        match open {
            None => open = Some(column),
            Some(start) => {
                // A pair that ends before the cursor holds no cursor, so the
                // next quote opens the following pair.
                if column < cursor_column {
                    open = None;
                    continue;
                }
                return Some(match scope {
                    TextObjectScope::Inner => (line_start + start + 1, line_start + column),
                    TextObjectScope::Around => (line_start + start, line_start + column + 1),
                });
            }
        }
    }
    None
}

/// Returns the first character position and the position after one word object.
///
/// A word object stays inside the cursor line, because a line break separates
/// two runs. An empty line names an empty range, so the operator changes
/// nothing there.
fn word_bounds(
    buffer: &TextBuffer,
    cursor: Cursor,
    kind: WordKind,
    scope: TextObjectScope,
    count: usize,
) -> Option<(usize, usize)> {
    let line = cursor.line();
    let line_start = buffer.line_start(line).get();
    let chars: Vec<char> = buffer.line_text(line).chars().collect();
    let Some(last_column) = chars.len().checked_sub(1) else {
        return Some((line_start, line_start));
    };
    let column = cursor.column().get().min(last_column);
    let mut start = run_start(&chars, kind, column);
    let mut end = run_end(&chars, kind, column);

    match scope {
        TextObjectScope::Inner => {
            for _ in 1..count {
                let Some(next) = next_run(&chars, end) else {
                    break;
                };
                end = run_end(&chars, kind, next);
            }
        }
        TextObjectScope::Around => {
            let mut took_counterpart = false;
            for unit in 0..count {
                if unit > 0 {
                    let Some(next) = next_run(&chars, end) else {
                        break;
                    };
                    end = run_end(&chars, kind, next);
                }
                // One unit takes the blanks that follow a word, or the word
                // that follows a run of blanks. Two runs of the same side, such
                // as a word and the punctuation behind it, stay separate.
                let taken_blank = kind.class(chars[end]) == CharClass::Blank;
                took_counterpart = match next_run(&chars, end) {
                    Some(next) if (kind.class(chars[next]) == CharClass::Blank) != taken_blank => {
                        end = run_end(&chars, kind, next);
                        !taken_blank
                    }
                    _ => false,
                };
            }
            // Vim takes the leading blanks when no trailing blank exists.
            if !took_counterpart
                && let Some(previous) = start.checked_sub(1)
                && kind.class(chars[previous]) == CharClass::Blank
            {
                start = run_start(&chars, kind, previous);
            }
        }
    }
    Some((line_start + start, line_start + end + 1))
}

/// Returns the first column of the run that holds one column.
fn run_start(chars: &[char], kind: WordKind, at: usize) -> usize {
    let class = kind.class(chars[at]);
    let mut start = at;
    while let Some(previous) = start.checked_sub(1) {
        if kind.class(chars[previous]) != class {
            break;
        }
        start = previous;
    }
    start
}

/// Returns the last column of the run that holds one column.
fn run_end(chars: &[char], kind: WordKind, at: usize) -> usize {
    let class = kind.class(chars[at]);
    let mut end = at;
    while end + 1 < chars.len() && kind.class(chars[end + 1]) == class {
        end += 1;
    }
    end
}

/// Returns the first column of the run behind one column, inside the line.
fn next_run(chars: &[char], end: usize) -> Option<usize> {
    let next = end + 1;
    (next < chars.len()).then_some(next)
}
