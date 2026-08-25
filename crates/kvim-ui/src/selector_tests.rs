//! Unit tests for the domain-neutral selector.

use super::*;

/// The host identity that these tests own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Entry {
    First,
    Second,
    Third,
}

fn selector(names: &[(Entry, &str, &str)]) -> Selector<Entry> {
    let mut selector = Selector::default();
    let candidates = names
        .iter()
        .map(|(id, name, container)| SelectorCandidate::new(*id, *name, *container))
        .collect();
    selector.set_candidates(candidates, false);
    selector
}

fn names(selector: &Selector<Entry>) -> Vec<String> {
    selector
        .matches()
        .iter()
        .filter_map(|index| selector.candidate(*index))
        .map(|candidate| candidate.name().to_owned())
        .collect()
}

#[test]
fn an_empty_query_keeps_the_order_of_the_source() {
    let selector = selector(&[
        (Entry::First, "zebra", "one"),
        (Entry::Second, "alpha", ""),
        (Entry::Third, "main", "one"),
    ]);
    assert_eq!(names(&selector), vec!["zebra", "alpha", "main"]);
}

#[test]
fn the_best_match_sits_at_the_top_of_the_list() {
    let mut selector = selector(&[
        (Entry::First, "domain", "one"),
        (Entry::Second, "main", "one"),
        (Entry::Third, "manual", "two"),
    ]);
    selector.set_query("main");
    assert_eq!(names(&selector).first().map(String::as_str), Some("main"));
}

#[test]
fn two_equal_scores_keep_one_deterministic_order() {
    // Both names hold the query at the same positions, so the shorter
    // candidate wins, and the earlier candidate wins an equal width.
    let mut selector = selector(&[
        (Entry::First, "ab_long_name", ""),
        (Entry::Second, "ab", ""),
        (Entry::Third, "ab2", ""),
    ]);
    selector.set_query("ab");
    let first = names(&selector);
    selector.set_query("");
    selector.set_query("ab");
    assert_eq!(
        first,
        names(&selector),
        "one query always produces one order"
    );
    assert_eq!(first.first().map(String::as_str), Some("ab"));
}

#[test]
fn the_ranking_breaks_a_tie_over_the_name_and_the_container_together() {
    // The container adds to the width that breaks a tie, so a candidate with
    // a longer container loses the tie even with an equal name.
    let mut selector = selector(&[
        (Entry::First, "ab", "long_container"),
        (Entry::Second, "ab", ""),
    ]);
    selector.set_query("ab");
    assert_eq!(names(&selector), vec!["ab", "ab"]);
    assert_eq!(selector.matches().to_vec(), vec![1_usize, 0]);
}

#[test]
fn the_selection_follows_its_candidate_across_one_refiltering() {
    let mut selector = selector(&[
        (Entry::First, "main", "one"),
        (Entry::Second, "mode", "one"),
        (Entry::Third, "motion", "one"),
    ]);
    selector.set_query("mo");
    selector.select_next();
    let selected = selector
        .selected()
        .expect("the query \"mo\" keeps at least one candidate")
        .name()
        .to_owned();
    selector.set_query("mot");
    assert_eq!(
        selector.selected().map(SelectorCandidate::name),
        Some(selected.as_str()),
        "the selected row still matches, so it stays selected"
    );
}

#[test]
fn a_selection_that_the_query_drops_returns_to_the_best_row() {
    let mut selector = selector(&[
        (Entry::First, "main", "one"),
        (Entry::Second, "mode", "one"),
    ]);
    selector.set_query("m");
    selector.select_next();
    assert_eq!(
        selector.selected().map(SelectorCandidate::name),
        Some("mode")
    );
    selector.set_query("main");
    assert_eq!(
        selector.selected().map(SelectorCandidate::name),
        Some("main")
    );
}

#[test]
fn the_selection_stops_at_both_ends_of_the_list() {
    let mut selector = selector(&[(Entry::First, "a", ""), (Entry::Second, "b", "")]);
    selector.select_previous();
    assert_eq!(selector.selected_row(), Some(0));
    selector.select_next();
    selector.select_next();
    selector.select_next();
    assert_eq!(selector.selected_row(), Some(1));
}

#[test]
fn a_candidate_list_above_the_bound_reports_the_truncation() {
    let mut selector = Selector::default();
    let candidates = (0..SELECTOR_CANDIDATES_MAX + 8)
        .map(|index| SelectorCandidate::new(Entry::First, format!("entry{index}"), ""))
        .collect();
    selector.set_candidates(candidates, false);
    assert!(selector.is_truncated());
    assert_eq!(selector.matches().len(), SELECTOR_CANDIDATES_MAX);
}

#[test]
fn the_query_stops_at_the_character_bound() {
    let mut selector = selector(&[(Entry::First, "a", "")]);
    selector.set_query(&"x".repeat(SELECTOR_QUERY_CHARS_MAX + 32));
    assert_eq!(selector.query().chars().count(), SELECTOR_QUERY_CHARS_MAX);
}
