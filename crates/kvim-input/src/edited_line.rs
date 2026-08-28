//! The line that one [`PromptEdit`] edits: its text, its cursor, and the rule
//! that every edit applies at that cursor.
//!
//! The line names no prompt kind, no prefix, and no completion. It holds the
//! text and one cursor position, and it owns every change of that text, so the
//! two can never disagree. A caller adds its own vocabulary above the line, as
//! the command line, the search prompt, the picker query, and the four
//! file-tree prompts of kvim do. `docs/input-actions.md` owns the rules that
//! this module implements.
//!
//! The position counts characters, because a character is the unit that a
//! reader inserts and deletes. The terminal counts cells instead, so the caller
//! converts the position once, where it draws the line and knows the width of
//! every character. [`EditedLine::cursor_offset`] answers the byte offset that
//! such a conversion starts from.
//!
//! `examples/edited_line.rs` holds one complete line of a host.

use std::fmt;

use super::resolver::PromptEdit;

/// A public edited-line seed did not establish its bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditedLineError {
    /// The maximum character count is zero.
    ZeroLimit,
    /// The seed exceeds the supplied character limit.
    SeedTooLong {
        /// The seed character count.
        chars: usize,
        /// The inclusive character limit.
        chars_max: usize,
    },
}

impl fmt::Display for EditedLineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroLimit => {
                formatter.write_str("the edited-line character limit must not be zero")
            }
            Self::SeedTooLong { chars, chars_max } => write!(
                formatter,
                "the edited-line seed holds {chars} characters, above the {chars_max}-character limit"
            ),
        }
    }
}

impl std::error::Error for EditedLineError {}

/// What one applied edit did to an [`EditedLine`].
///
/// The value separates a changed text from a moved cursor, because a caller
/// that holds a candidate list, a search result, or a query below the line
/// reads the text again only after the text itself changed.
///
/// The enumeration is exhaustive on purpose, for the reason that
/// [`PromptEdit`] states: a new variant must stop the build of a host that
/// decides what each one means. See `docs/architecture.md`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[must_use]
pub enum LineChange {
    /// The edit changed the text of the line.
    ///
    /// The cursor can have moved with it, because every text edit applies at
    /// the cursor.
    TextChanged,
    /// The edit moved the cursor and left the text as it is.
    CursorMoved,
    /// The edit reached the line and changed nothing.
    ///
    /// A motion at the end that it names, a delete with nothing to remove, and
    /// an insert above the bound of the line all report this.
    Unchanged,
    /// The line answers no such edit, so the caller owns it.
    ///
    /// A completion writes a candidate that the caller supplies, and an accept
    /// and a cancel end a prompt that the caller owns. The line holds neither
    /// a candidate list nor a prompt, so it reports the edit back instead of
    /// inventing one. See `docs/input-actions.md`.
    Deferred,
}

/// One line of text and the cursor that every edit of it applies at.
///
/// The cursor counts characters from the start of the text. It never passes
/// the number of characters of the text, so it always names a character
/// boundary. The type owns every change of the text, which keeps that
/// invariant in one place.
///
/// The caller states the largest number of characters that the line accepts.
/// The bound refuses rather than cuts: an insert above it changes nothing and
/// reports [`LineChange::Unchanged`], so no reader loses a character in
/// silence.
///
/// ```
/// use kvim_input::{EditedLine, LineChange, PromptEdit};
///
/// let mut line = EditedLine::opened(String::from("write"), 16)
///     .expect("the seed meets the limit");
/// assert_eq!(line.cursor(), 5);
///
/// assert_eq!(line.apply(PromptEdit::CursorLineStart), LineChange::CursorMoved);
/// assert_eq!(line.apply(PromptEdit::Insert('q')), LineChange::TextChanged);
/// assert_eq!(line.text(), "qwrite");
/// assert_eq!(line.cursor(), 1);
///
/// // A completion and a cancel end a prompt that the caller owns, so the line
/// // reports them back.
/// assert_eq!(line.apply(PromptEdit::Cancel), LineChange::Deferred);
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditedLine {
    /// The text of the line.
    text: String,
    /// The cursor position, counted in characters from the start of the text.
    cursor: usize,
    /// The largest number of characters that the line accepts.
    chars_max: usize,
}

impl EditedLine {
    /// Opens one line over `text`, with the cursor after the whole text.
    ///
    /// A reader who opens a seeded line continues at the end of the seed,
    /// exactly as they continue after the text that they typed themselves.
    ///
    /// ```
    /// use kvim_input::EditedLine;
    ///
    /// let line = EditedLine::opened(String::from("näme"), 8)
    ///     .expect("the seed meets the limit");
    /// assert_eq!(line.cursor(), 4);
    /// ```
    /// # Errors
    ///
    /// Returns [`EditedLineError`] when the limit is zero or the seed exceeds
    /// it.
    pub fn opened(text: String, chars_max: usize) -> Result<Self, EditedLineError> {
        let cursor = text.chars().count();
        Self::opened_at(text, cursor, chars_max)
    }

    /// Opens one line over `text`, with the cursor at `cursor` characters.
    ///
    /// A caller that seeds a text and a position of its own reaches the line
    /// through this constructor. The rename prompt of kvim opens at the end of
    /// the stem of a file name that way. A position above the characters of
    /// the text lands after the last one, so the invariant of the cursor holds
    /// from the first keypress.
    ///
    /// ```
    /// use kvim_input::EditedLine;
    ///
    /// let line = EditedLine::opened_at(String::from("notes.md"), 5, 16)
    ///     .expect("the seed meets the limit");
    /// assert_eq!(line.cursor(), 5);
    /// ```
    /// # Errors
    ///
    /// Returns [`EditedLineError`] when the limit is zero or the seed exceeds
    /// it. A cursor above the seed still clamps to the end.
    pub fn opened_at(
        text: String,
        cursor: usize,
        chars_max: usize,
    ) -> Result<Self, EditedLineError> {
        if chars_max == 0 {
            return Err(EditedLineError::ZeroLimit);
        }
        let chars = text.chars().count();
        if chars > chars_max {
            return Err(EditedLineError::SeedTooLong { chars, chars_max });
        }
        Ok(Self {
            text,
            cursor: cursor.min(chars),
            chars_max,
        })
    }

    /// Returns the text of the line.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the cursor position, counted in characters.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Returns the largest number of characters that the line accepts.
    #[must_use]
    pub const fn chars_max(&self) -> usize {
        self.chars_max
    }

    /// Returns the byte offset of the cursor inside the text.
    ///
    /// The cursor counts characters, and every edit of this type keeps it
    /// inside the text, so the walk misses only at the end of the line. A
    /// caller that draws the line measures the text before this offset in the
    /// cells of its own terminal.
    #[must_use]
    pub fn cursor_offset(&self) -> usize {
        debug_assert!(
            self.cursor <= self.text.chars().count(),
            "every edit of this type keeps the cursor inside the text"
        );
        self.text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(offset, _)| offset)
    }

    /// Applies one edit of a prompt to the line.
    ///
    /// The three text edits and the six motions change the line itself. The two
    /// completion edits, the accept, and the cancel report
    /// [`LineChange::Deferred`], because a candidate list and a prompt belong
    /// to the caller and never to one line.
    pub fn apply(&mut self, edit: PromptEdit) -> LineChange {
        match edit {
            PromptEdit::Insert(value) => self.insert(value),
            PromptEdit::DeleteBackward => self.delete_backward(),
            PromptEdit::DeleteWordBackward => self.delete_word_backward(),
            PromptEdit::CursorLeft => self.move_cursor(LineMotion::CharacterBackward),
            PromptEdit::CursorRight => self.move_cursor(LineMotion::CharacterForward),
            PromptEdit::CursorWordBackward => self.move_cursor(LineMotion::WordBackward),
            PromptEdit::CursorWordForward => self.move_cursor(LineMotion::WordForward),
            PromptEdit::CursorLineStart => self.move_cursor(LineMotion::LineStart),
            PromptEdit::CursorLineEnd => self.move_cursor(LineMotion::LineEnd),
            PromptEdit::CompleteNext
            | PromptEdit::CompletePrevious
            | PromptEdit::Accept
            | PromptEdit::Cancel => LineChange::Deferred,
        }
    }

    /// Writes one character before the cursor and steps the cursor over it.
    ///
    /// The bound counts the whole line and not the text before the cursor, so
    /// an insert in the middle of the line meets the same limit as one at its
    /// end. A line at its bound changes nothing.
    pub fn insert(&mut self, value: char) -> LineChange {
        if self.text.chars().count() >= self.chars_max {
            return LineChange::Unchanged;
        }
        let offset = self.cursor_offset();
        self.text.insert(offset, value);
        self.cursor += 1;
        LineChange::TextChanged
    }

    /// Removes the character before the cursor and steps the cursor back.
    ///
    /// A cursor at the start of the line removes nothing, because no character
    /// stands before it.
    pub fn delete_backward(&mut self) -> LineChange {
        let offset = self.cursor_offset();
        let Some(removed) = self.text[..offset].chars().next_back() else {
            return LineChange::Unchanged;
        };
        self.text.remove(offset - removed.len_utf8());
        self.cursor -= 1;
        LineChange::TextChanged
    }

    /// Removes the word before the cursor, and the blanks before that word.
    ///
    /// The text after the cursor stays, and the cursor steps back over every
    /// removed character.
    pub fn delete_word_backward(&mut self) -> LineChange {
        let offset = self.cursor_offset();
        let Some(start) = word_start_before(&self.text, offset) else {
            return LineChange::Unchanged;
        };
        let removed = self.text[start..offset].chars().count();
        debug_assert!(
            removed <= self.cursor,
            "the removed characters all stand before the cursor"
        );
        self.text.replace_range(start..offset, "");
        self.cursor -= removed;
        LineChange::TextChanged
    }

    /// Writes one whole line and places the cursor after it.
    ///
    /// A completion replaces the whole line with its candidate, so the reader
    /// continues after that candidate, as they do in Vim and in readline. The
    /// restore of a cancelled completion replaces the whole line as well and
    /// follows the same rule.
    ///
    /// A text above the bound of the line changes nothing, because the bound
    /// refuses rather than cuts.
    pub fn write(&mut self, text: String) -> LineChange {
        if text.chars().count() > self.chars_max {
            return LineChange::Unchanged;
        }
        self.cursor = text.chars().count();
        self.text = text;
        LineChange::TextChanged
    }

    /// Moves the cursor and reports whether it moved.
    ///
    /// Every motion stops at the end that it names and never wraps to the other
    /// end, so a reader who holds a motion key down reaches a stable position.
    /// A motion that lands where the cursor already stands changes nothing.
    fn move_cursor(&mut self, motion: LineMotion) -> LineChange {
        let chars = self.text.chars().count();
        let offset = self.cursor_offset();
        let moved = match motion {
            LineMotion::CharacterBackward => self.cursor.saturating_sub(1),
            LineMotion::CharacterForward => self.cursor.saturating_add(1).min(chars),
            // The backward motion lands where the backward word delete cuts, so
            // the two keys always name the same word.
            LineMotion::WordBackward => word_start_before(&self.text, offset)
                .map_or(0, |start| self.text[..start].chars().count()),
            LineMotion::WordForward => word_start_after(&self.text, offset)
                .map_or(chars, |start| self.text[..start].chars().count()),
            LineMotion::LineStart => 0,
            LineMotion::LineEnd => chars,
        };
        debug_assert!(
            moved <= chars,
            "every motion clamps to the characters of the line"
        );
        if moved == self.cursor {
            return LineChange::Unchanged;
        }
        self.cursor = moved;
        LineChange::CursorMoved
    }
}

/// One move of the cursor of an [`EditedLine`].
///
/// The type is private, because it names the motions of one type and enforces
/// the cursor invariant of that type alone. [`PromptEdit`] publishes the same
/// vocabulary to a host, and [`EditedLine::apply`] joins the two.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineMotion {
    /// One character back.
    CharacterBackward,
    /// One character forward.
    CharacterForward,
    /// To the start of the word before the cursor.
    WordBackward,
    /// To the start of the word after the cursor.
    WordForward,
    /// Before the first character of the line.
    LineStart,
    /// After the last character of the line.
    LineEnd,
}

/// Returns where the word before `at` starts inside one written line.
///
/// `at` is a byte offset that names a character boundary. The walk passes the
/// run of blanks before `at` first and then the run of non-blanks, which is the
/// rule of Vim, of readline, and of every terminal shell. A line that holds
/// nothing before `at` holds no word to remove and returns `None`.
fn word_start_before(text: &str, at: usize) -> Option<usize> {
    let before = text.get(..at)?;
    if before.is_empty() {
        return None;
    }
    let start = before
        .trim_end()
        .char_indices()
        .rev()
        .find(|&(_, value)| value.is_whitespace())
        .map_or(0, |(index, value)| index + value.len_utf8());
    debug_assert!(
        start < at,
        "the last character before the cursor is a blank or a non-blank, so the walk always \
         removes at least one character"
    );
    Some(start)
}

/// Returns where the word after `at` starts inside one written line.
///
/// `at` is a byte offset that names a character boundary. The walk passes the
/// rest of the word under `at` first and then the run of blanks after it, so it
/// mirrors [`word_start_before`] and lands on the start of the next word. A
/// line that holds only blanks after `at` holds no next word, so the walk stops
/// at the end of the line. A line that holds nothing after `at` returns `None`.
fn word_start_after(text: &str, at: usize) -> Option<usize> {
    let after = text.get(at..)?;
    if after.is_empty() {
        return None;
    }
    let over_word = after
        .char_indices()
        .find(|&(_, value)| value.is_whitespace())
        .map_or(after.len(), |(index, _)| index);
    let over_blanks = after[over_word..]
        .char_indices()
        .find(|&(_, value)| !value.is_whitespace())
        .map_or(after.len() - over_word, |(index, _)| index);
    let start = at + over_word + over_blanks;
    debug_assert!(
        start > at,
        "the first character after the cursor is a blank or a non-blank, so the walk always \
         passes at least one character"
    );
    Some(start)
}

#[cfg(test)]
#[path = "edited_line_tests.rs"]
mod tests;
