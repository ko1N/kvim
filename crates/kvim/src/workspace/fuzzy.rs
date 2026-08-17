//! The deterministic fuzzy score of one picker candidate.
//!
//! The module is pure. It reads no clock, no filesystem, and no process, so one
//! query and one candidate text always produce one score. The picker ranks its
//! rows with these scores. See `docs/files.md`.

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
/// use kvim::workspace::score_candidate;
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
mod tests {
    use super::score_candidate;

    #[test]
    fn a_query_that_the_candidate_does_not_hold_scores_nothing() {
        assert_eq!(score_candidate("xyz", "main.rs", "src"), None);
        // The order of the characters matters, because the match is a
        // subsequence and not a set.
        assert_eq!(score_candidate("nima", "main.rs", ""), None);
    }

    #[test]
    fn an_empty_query_matches_every_candidate_with_one_score() {
        assert_eq!(score_candidate("", "main.rs", "src"), Some(0));
        assert_eq!(score_candidate("", "lib.rs", ""), Some(0));
    }

    #[test]
    fn a_consecutive_match_ranks_above_a_spread_match() {
        let dense = score_candidate("main", "main.rs", "").expect("the name holds the query");
        let spread = score_candidate("main", "m_a_i_n.rs", "").expect("the name holds the query");
        assert!(dense > spread, "{dense} must rank above {spread}");
    }

    #[test]
    fn a_word_start_ranks_above_a_match_inside_one_word() {
        let start = score_candidate("s", "session.rs", "").expect("the name holds the query");
        let inside = score_candidate("s", "parse.rs", "").expect("the name holds the query");
        assert!(start > inside, "{start} must rank above {inside}");
    }

    #[test]
    fn the_filename_weighs_more_than_the_directory() {
        let name =
            score_candidate("picker", "picker.rs", "workspace").expect("the name holds the query");
        let directory =
            score_candidate("picker", "mod.rs", "picker").expect("the path holds the query");
        assert!(name > directory, "{name} must rank above {directory}");
    }

    #[test]
    fn the_match_ignores_the_case_of_both_sides() {
        assert!(score_candidate("MAIN", "main.rs", "").is_some());
        assert!(score_candidate("main", "MAIN.RS", "").is_some());
    }
}
