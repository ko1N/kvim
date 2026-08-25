//! The deterministic fuzzy score of one candidate against one query.
//!
//! The crate is pure. It reads no clock, no filesystem, and no process, so one
//! query and one candidate text always produce one score. It names no path, no
//! buffer, and no editor concept, so any caller that ranks a list of its own
//! values can hold it.
//!
//! [`score_candidate`] scores one candidate that carries a name and a directory,
//! which is the shape that a file picker holds. A caller with one text alone
//! passes an empty directory. The kvim picker ranks its rows with these scores;
//! see `docs/files.md`.
//!
//! [`rank`] turns those scores into one ordered list of source indexes. It is
//! the one rule that every caller with a bounded candidate list shares: the
//! workspace picker, the command-line completion, and the domain-neutral
//! selector of `kvim-ui`. No caller keeps a second copy of the ordering rule.
//!
//! # Examples
//!
//! ```
//! use kvim_fuzzy::score_candidate;
//!
//! // A run of characters outranks the same characters spread apart.
//! let dense = score_candidate("main", "main.rs", "").expect("the name answers");
//! let spread = score_candidate("main", "m_a_i_n.rs", "").expect("the name answers");
//! assert!(dense > spread);
//!
//! // A query that the candidate does not hold scores nothing.
//! assert_eq!(score_candidate("xyz", "main.rs", "src"), None);
//!
//! // An empty query keeps the order of the source list.
//! assert_eq!(score_candidate("", "main.rs", "src"), Some(0));
//! ```

use std::cmp::Reverse;

/// The largest number of characters that one scored text holds.
///
/// A longer text keeps its first characters. The bound keeps the cost of one
/// keystroke proportional to the number of candidates alone.
pub const FUZZY_TEXT_CHARS_MAX: usize = 512;

/// The score of one matched character that follows the previous match.
///
/// A run of characters is the strongest signal that one candidate answers the
/// query, so it outranks the start of one word.
const CONSECUTIVE_BONUS: i32 = 16;

/// The score of one matched character at the start of one word.
const WORD_START_BONUS: i32 = 12;

/// The score of one matched character that neither rule above rewards.
const MATCH_SCORE: i32 = 2;

/// The cost of one character between the first and the last matched character.
const SKIP_PENALTY: i32 = 1;

/// The factor that a match inside the filename receives.
///
/// The picker shows the filename first, so a match there must rank above a
/// match inside the directory of another candidate. See `docs/files.md`.
pub const FUZZY_NAME_WEIGHT: i32 = 3;

/// The characters that start one word inside a path.
const WORD_BOUNDARIES: [char; 7] = ['/', '\\', '_', '-', '.', ' ', ':'];

/// Returns the score of one query against one filename and its directory.
///
/// The function matches the filename first, because the picker shows the
/// filename first. A match there receives [`FUZZY_NAME_WEIGHT`]. Only a query that
/// the filename does not hold reaches the complete path.
///
/// An empty query matches every candidate with the score zero, so the picker
/// keeps the order of its source.
///
/// # Examples
///
/// ```
/// use kvim_fuzzy::score_candidate;
///
/// // The filename weighs more than the directory.
/// let name = score_candidate("main", "main.rs", "src").expect("the name holds the query");
/// let directory = score_candidate("main", "lib.rs", "main").expect("the path holds the query");
/// assert!(name > directory);
/// assert_eq!(score_candidate("zz", "main.rs", "src"), None);
/// ```
#[must_use]
pub fn score_candidate(query: &str, name: &str, directory: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    if let Some(score) = score_text(query, name.chars()) {
        return Some(score.saturating_mul(FUZZY_NAME_WEIGHT));
    }
    if directory.is_empty() {
        return None;
    }
    score_text(
        query,
        directory
            .chars()
            .chain(std::iter::once('/'))
            .chain(name.chars()),
    )
}

/// Returns the source indexes that `query` keeps, ranked with the best first.
///
/// The function scores every entry through [`score_candidate`] over its name
/// and its container, and it drops every entry that scores `None`. The order
/// is total, so two equal queries always produce one order:
///
/// 1. the higher score first,
/// 2. then the shorter combined name and container,
/// 3. then the earlier entry of `entries`.
///
/// An empty query keeps every entry and the order of `entries`, because every
/// entry then holds the same score.
///
/// The function takes borrowed entries and clips no query, so a caller ranks
/// a list on every keystroke with no allocation beyond the returned index
/// list. Clip the query to a caller bound before calling this function; the
/// function keeps none of its own.
///
/// # Examples
///
/// ```
/// use kvim_fuzzy::rank;
///
/// let entries = [("session.rs", "src"), ("main.rs", "src")];
/// assert_eq!(rank("main", entries.iter().copied()), [1]);
/// assert_eq!(rank("", entries.iter().copied()), [0, 1]);
/// ```
#[must_use]
pub fn rank<'a, I>(query: &str, entries: I) -> Vec<usize>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let entries: Vec<(&str, &str)> = entries.into_iter().collect();
    let mut scored: Vec<(usize, i32)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, (name, container))| {
            let score = score_candidate(query, name, container)?;
            Some((index, score))
        })
        .collect();
    if !query.is_empty() {
        scored.sort_by_key(|(index, score)| {
            let (name, container) = entries[*index];
            let width = name
                .chars()
                .count()
                .saturating_add(container.chars().count());
            (Reverse(*score), width, *index)
        });
    }
    scored.into_iter().map(|(index, _)| index).collect()
}

/// Returns the score of one query against one text.
///
/// The scan is greedy and left to right: each query character takes the first
/// character of the text that answers it. The rule is deterministic, so the
/// same query and the same text always produce the same score.
fn score_text<I>(query: &str, text: I) -> Option<i32>
where
    I: Iterator<Item = char>,
{
    let mut wanted = query.chars();
    let Some(mut next) = wanted.next() else {
        return Some(0);
    };
    let mut score = 0_i32;
    let mut matched = 0_i32;
    let mut previous: Option<char> = None;
    let mut last_match: Option<usize> = None;
    let mut complete = false;
    for (index, value) in text.take(FUZZY_TEXT_CHARS_MAX).enumerate() {
        if !complete && equal_ignoring_case(value, next) {
            score = score.saturating_add(bonus(previous, value, last_match, index));
            matched = matched.saturating_add(1);
            last_match = Some(index);
            match wanted.next() {
                Some(value) => next = value,
                None => complete = true,
            }
        }
        previous = Some(value);
    }
    if !complete {
        return None;
    }
    let last = last_match.map_or(0, |index| i32::try_from(index).unwrap_or(i32::MAX));
    // The skipped characters lie between the first and the last match, so a
    // dense match ranks above a match that spreads over the whole text.
    let skipped = last.saturating_add(1).saturating_sub(matched);
    Some(score.saturating_sub(skipped.saturating_mul(SKIP_PENALTY)))
}

/// Returns the score of one matched character.
fn bonus(previous: Option<char>, value: char, last_match: Option<usize>, index: usize) -> i32 {
    if last_match.is_some_and(|last| last.saturating_add(1) == index) {
        return CONSECUTIVE_BONUS;
    }
    let word_start = previous.is_none_or(|it| {
        WORD_BOUNDARIES.contains(&it) || (!it.is_uppercase() && value.is_uppercase())
    });
    if word_start {
        WORD_START_BONUS
    } else {
        MATCH_SCORE
    }
}

/// Reports whether two characters match without their case.
fn equal_ignoring_case(left: char, right: char) -> bool {
    left == right || left.to_lowercase().eq(right.to_lowercase())
}

#[cfg(test)]
mod tests;
