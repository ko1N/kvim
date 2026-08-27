//! Bounded literal search over the buffer text.
//!
//! The first release matches literal text, not a regular expression. The query
//! holds one line, so a match never crosses a line terminator. The search is
//! deterministic: equal buffer text, equal query, and equal settings produce
//! equal matches in ascending order.

use thiserror::Error;

use kvim_core::{CharPosition, TextBuffer};
use kvim_settings::{CaseSensitivity, SearchSettings};

/// The largest search query that kvim accepts, in characters.
///
/// The bound keeps one prompt line usable and keeps the match scan short.
pub const SEARCH_QUERY_CHARS_MAX: usize = 256;

/// The largest number of matches that one search collects.
///
/// A common query in a large file matches many times. The bound keeps the match
/// list small enough for highlighting and for repeated `n` and `N` movement.
pub const SEARCH_MATCHES_MAX: usize = 4_096;

/// The largest number of buffer bytes that one search reads.
///
/// The value is the maximum file size, so a search reads a complete buffer that
/// the file limit already accepted, and no more.
pub const SEARCH_SCAN_BYTES_MAX: usize = 4 * 1024 * 1024;

/// A rejected search query.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SearchError {
    /// The query holds no character.
    #[error("the search query is empty")]
    Empty,
    /// The query is longer than [`SEARCH_QUERY_CHARS_MAX`].
    #[error("the search query holds {chars} characters; the limit is {max} characters")]
    TooLong {
        /// The length of the rejected query, in characters.
        chars: usize,
        /// The accepted maximum, in characters.
        max: usize,
    },
    /// The query holds a line break.
    #[error("the search query holds a line break; one query matches inside one line")]
    MultipleLines,
}

/// The direction that one search moves through the buffer.
///
/// # Examples
///
/// ```
/// use kvim_editor::SearchDirection;
///
/// assert_eq!(SearchDirection::Forward.reversed(), SearchDirection::Backward);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchDirection {
    /// Move toward the end of the buffer.
    Forward,
    /// Move toward the start of the buffer.
    Backward,
}

impl SearchDirection {
    /// Returns the opposite direction, which the `N` command uses.
    #[must_use]
    pub const fn reversed(self) -> Self {
        match self {
            Self::Forward => Self::Backward,
            Self::Backward => Self::Forward,
        }
    }
}

/// The resolved case rule of one query.
///
/// The smart-case setting depends on the query, so the rule resolves once for
/// each search instead of once for each character.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MatchCase {
    Sensitive,
    Insensitive,
}

/// One validated search query and the direction that opened it.
///
/// # Examples
///
/// ```
/// use kvim_core::TextBuffer;
/// use kvim_editor::{SearchDirection, SearchQuery};
/// use kvim_settings::{FileSettings, SearchSettings};
///
/// let buffer = TextBuffer::from_text("foo\nFoo\n", kvim_core::BufferBytesMax::default())
///     .expect("the text is small");
/// let settings = SearchSettings::default();
///
/// // The default smart-case rule ignores the case of a lowercase query.
/// let query = SearchQuery::new("foo", SearchDirection::Forward)
///     .expect("the query holds one short line");
/// assert_eq!(query.matches(&buffer, &settings).len(), 2);
///
/// // One uppercase character makes the same query compare the case.
/// let exact = SearchQuery::new("Foo", SearchDirection::Forward)
///     .expect("the query holds one short line");
/// assert_eq!(exact.matches(&buffer, &settings).len(), 1);
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuery {
    text: String,
    direction: SearchDirection,
}

impl SearchQuery {
    /// Validates one query from the search prompt.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Empty`] for a query without a character,
    /// [`SearchError::TooLong`] beyond [`SEARCH_QUERY_CHARS_MAX`], and
    /// [`SearchError::MultipleLines`] for a query that holds a line break.
    pub fn new(text: &str, direction: SearchDirection) -> Result<Self, SearchError> {
        let chars = text.chars().count();
        if chars == 0 {
            return Err(SearchError::Empty);
        }
        if chars > SEARCH_QUERY_CHARS_MAX {
            return Err(SearchError::TooLong {
                chars,
                max: SEARCH_QUERY_CHARS_MAX,
            });
        }
        if text.contains('\n') || text.contains('\r') {
            return Err(SearchError::MultipleLines);
        }
        Ok(Self {
            text: text.to_owned(),
            direction,
        })
    }

    /// Returns the query text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the direction that opened the search.
    #[must_use]
    pub const fn direction(&self) -> SearchDirection {
        self.direction
    }

    /// Collects the match positions, in ascending order.
    ///
    /// The list holds at most [`SEARCH_MATCHES_MAX`] entries, and the scan reads
    /// at most [`SEARCH_SCAN_BYTES_MAX`] bytes. Matches may overlap, like Vim.
    #[must_use]
    pub fn matches(&self, buffer: &TextBuffer, settings: &SearchSettings) -> Vec<CharPosition> {
        let needle: Vec<char> = self.text.chars().collect();
        debug_assert!(
            !needle.is_empty(),
            "the constructor rejects an empty query, so the window length is not zero"
        );
        let case = self.match_case(settings);
        let mut found = Vec::new();
        let mut scanned_bytes = 0usize;

        for index in 0..buffer.line_count() {
            if found.len() >= SEARCH_MATCHES_MAX || scanned_bytes >= SEARCH_SCAN_BYTES_MAX {
                break;
            }
            let line = buffer
                .line_index(index)
                .expect("the loop counts the buffer lines");
            let text = buffer.line_text(line);
            scanned_bytes = scanned_bytes.saturating_add(text.len());
            let haystack: Vec<char> = text.chars().collect();
            if haystack.len() < needle.len() {
                continue;
            }
            for start in 0..=haystack.len() - needle.len() {
                if !equal_at(&haystack[start..], &needle, case) {
                    continue;
                }
                let column = buffer
                    .source_column(line, start)
                    .expect("the window start indexes the line characters");
                found.push(buffer.column_to_char(line, column));
                if found.len() >= SEARCH_MATCHES_MAX {
                    break;
                }
            }
        }

        debug_assert!(
            found.windows(2).all(|pair| pair[0] < pair[1]),
            "the scan reads ascending lines and ascending columns"
        );
        found
    }

    /// Returns the next match after a position, and wraps at the buffer limit.
    ///
    /// Returns `None` when the buffer holds no match.
    #[must_use]
    pub fn find(
        &self,
        buffer: &TextBuffer,
        from: CharPosition,
        direction: SearchDirection,
        settings: &SearchSettings,
    ) -> Option<CharPosition> {
        let matches = self.matches(buffer, settings);
        match direction {
            SearchDirection::Forward => matches
                .iter()
                .copied()
                .find(|position| *position > from)
                .or_else(|| matches.first().copied()),
            SearchDirection::Backward => matches
                .iter()
                .rev()
                .copied()
                .find(|position| *position < from)
                .or_else(|| matches.last().copied()),
        }
    }

    /// Resolves the configured case rule against this query.
    ///
    /// The smart-case rule compares the case as soon as the query holds one
    /// uppercase character. See `docs/settings.md`.
    fn match_case(&self, settings: &SearchSettings) -> MatchCase {
        match settings.case_sensitivity {
            CaseSensitivity::Sensitive => MatchCase::Sensitive,
            CaseSensitivity::Insensitive => MatchCase::Insensitive,
            CaseSensitivity::SmartCase => {
                if self.text.chars().any(char::is_uppercase) {
                    MatchCase::Sensitive
                } else {
                    MatchCase::Insensitive
                }
            }
        }
    }
}

/// Compares the query against the start of a character window.
///
/// The comparison runs over characters, never over bytes, so a multi-byte
/// character stays one position.
fn equal_at(haystack: &[char], needle: &[char], case: MatchCase) -> bool {
    haystack.iter().zip(needle).all(|(left, right)| match case {
        MatchCase::Sensitive => left == right,
        MatchCase::Insensitive => left == right || left.to_lowercase().eq(right.to_lowercase()),
    })
}
