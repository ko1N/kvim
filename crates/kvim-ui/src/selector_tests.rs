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
fn apply_motion_moves_by_a_count_and_stops_at_both_ends() {
    let mut selector = selector(&[
        (Entry::First, "a", ""),
        (Entry::Second, "b", ""),
        (Entry::Third, "c", ""),
    ]);
    selector.apply_motion(ListMotion::Down(1));
    assert_eq!(selector.selected_row(), Some(1));
    selector.apply_motion(ListMotion::Up(1));
    assert_eq!(selector.selected_row(), Some(0));

    // The move stops at each end instead of wrapping, the way `SidebarState`
    // stops instead of wrapping.
    selector.apply_motion(ListMotion::Up(9));
    assert_eq!(selector.selected_row(), Some(0));
    selector.apply_motion(ListMotion::Down(9));
    assert_eq!(selector.selected_row(), Some(2));
}

#[test]
fn apply_motion_to_row_indexes_matches_not_the_candidate_list() {
    let mut selector = selector(&[
        (Entry::First, "zebra", ""),
        (Entry::Second, "main", ""),
        (Entry::Third, "manual", ""),
    ]);
    selector.set_query("ma");
    assert_eq!(
        selector.matches().len(),
        2,
        "the query keeps two candidates"
    );

    // Row 1 of the matched list is `Entry::Third`, not `Entry::Second`, the
    // candidate that row 1 would name in the unfiltered candidate list.
    selector.apply_motion(ListMotion::ToRow(1));
    assert_eq!(
        selector.selected(),
        selector.candidate(selector.matches()[1])
    );
    assert_eq!(
        selector.selected().map(SelectorCandidate::id),
        Some(&Entry::Third)
    );

    selector.apply_motion(ListMotion::ToRow(100));
    assert_eq!(
        selector.selected_row(),
        Some(1),
        "a row past the end clamps to the last matched row"
    );
}

#[test]
fn apply_motion_reconciles_the_window_after_a_direct_jump() {
    let mut selector = long_selector(LONG_LIST_ROWS);
    selector.set_height_rows(5);
    let total = u32::try_from(LONG_LIST_ROWS).expect("forty fits u32");

    selector.apply_motion(ListMotion::LastRow);
    assert_eq!(selector.selected_row(), Some(LONG_LIST_ROWS - 1));
    assert!(shows(&selector, LONG_LIST_ROWS - 1));
    assert_eq!(
        selector.first_line(),
        total - 5,
        "a direct jump to the last row still stops the window at the end"
    );

    selector.apply_motion(ListMotion::ToRow(0));
    assert_eq!(selector.selected_row(), Some(0));
    assert!(shows(&selector, 0));
    assert_eq!(
        selector.first_line(),
        0,
        "a direct jump to the first row scrolls the window back to the top"
    );
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

/// The number of rows of the long uniform list that the sweeps walk.
const LONG_LIST_ROWS: usize = 40;

/// Returns one selector of `count` candidates, in source order under an
/// empty query.
fn long_selector(count: usize) -> Selector<usize> {
    let mut selector = Selector::default();
    selector.set_candidates(
        (0..count)
            .map(|index| SelectorCandidate::new(index, format!("row{index:02}"), ""))
            .collect(),
        false,
    );
    selector
}

/// Reports whether the window shows the row of the named position.
fn shows(selector: &Selector<usize>, row: usize) -> bool {
    selector
        .placements()
        .iter()
        .any(|placement| placement.index() == row)
}

/// Returns the number of terminal rows that the placements cover.
fn covered_rows(selector: &Selector<usize>) -> u32 {
    selector
        .placements()
        .iter()
        .map(|placement| u32::from(placement.lines()))
        .sum()
}

/// Asserts that the current selection sits inside the published window, with
/// no gap and no row past the end of the list.
fn assert_selection_inside_window(selector: &Selector<usize>, last_start: u32, height: u16) {
    let row = selector
        .selected_row()
        .expect("the sweep keeps one row selected");
    assert!(
        shows(selector, row),
        "height {height}, row {row} left the window"
    );
    assert!(
        selector.first_line() <= last_start,
        "height {height}, row {row} scrolled past the end"
    );
    assert_eq!(
        covered_rows(selector),
        u32::from(height),
        "height {height}, row {row} left a gap"
    );
}

#[test]
fn the_window_shows_the_selection_at_every_row_of_a_long_list_at_several_heights() {
    for height in [1_u16, 3, 8, 20, 40] {
        for margin in [0_u16, 1, 3] {
            let mut selector = long_selector(LONG_LIST_ROWS);
            selector.set_height_rows(height);
            selector.set_scroll_margin(margin);
            let last_start =
                u32::try_from(LONG_LIST_ROWS).expect("forty fits u32") - u32::from(height);

            // The sweep walks down to the last row, then back up to the
            // first, because the clamp at the end and the clamp at the start
            // meet different arms of the same offset rule.
            for _ in 0..LONG_LIST_ROWS {
                assert_selection_inside_window(&selector, last_start, height);
                selector.select_next();
            }
            for _ in 0..LONG_LIST_ROWS {
                assert_selection_inside_window(&selector, last_start, height);
                selector.select_previous();
            }
        }
    }
}

#[test]
fn the_last_row_stops_the_window_at_the_end_of_the_list_instead_of_centring() {
    for height in [1_u16, 2, 5, 12, 40] {
        let mut selector = long_selector(LONG_LIST_ROWS);
        selector.set_height_rows(height);
        for _ in 0..LONG_LIST_ROWS {
            selector.select_next();
        }
        let total = u32::try_from(LONG_LIST_ROWS).expect("forty fits u32");
        assert_eq!(
            selector.first_line(),
            total.saturating_sub(u32::from(height)),
            "the window of {height} rows stops at the end of the list"
        );
        assert!(
            shows(&selector, LONG_LIST_ROWS - 1),
            "the last row stays visible"
        );
    }
}

#[test]
fn a_scroll_margin_stops_at_the_end_of_the_list_instead_of_scrolling_past_it() {
    let mut selector = long_selector(12);
    selector.set_height_rows(6);
    selector.set_scroll_margin(3);

    // The margin holds two rows below the selection, because it stops at
    // half the window.
    for _ in 0..8 {
        selector.select_next();
    }
    assert_eq!(selector.selected_row(), Some(8));
    assert_eq!(selector.first_line(), 5);

    // The last row cannot hold a margin below itself, so the window stops at
    // the last line instead.
    selector.select_next();
    selector.select_next();
    selector.select_next();
    assert_eq!(selector.selected_row(), Some(11));
    assert_eq!(selector.first_line(), 6);
    assert!(shows(&selector, 11));
}

#[test]
fn a_scroll_margin_stops_at_the_start_of_the_list_instead_of_scrolling_past_it() {
    let mut selector = long_selector(12);
    selector.set_height_rows(6);
    selector.set_scroll_margin(3);
    for _ in 0..8 {
        selector.select_next();
    }
    assert_eq!(selector.first_line(), 5);

    // The first row cannot hold a margin above itself, so the window stops
    // at the first line instead of centring the margin.
    for _ in 0..8 {
        selector.select_previous();
    }
    assert_eq!(selector.selected_row(), Some(0));
    assert_eq!(selector.first_line(), 0);
    assert!(shows(&selector, 0));
}

#[test]
fn a_window_of_zero_rows_places_nothing() {
    let mut selector = long_selector(4);
    selector.set_height_rows(0);
    assert_eq!(selector.first_line(), 0);
    assert!(selector.placements().is_empty());
}

#[test]
fn an_empty_match_list_places_nothing() {
    let mut selector = long_selector(4);
    selector.set_height_rows(3);
    selector.set_query("zzz");
    assert!(selector.matches().is_empty());
    assert_eq!(selector.first_line(), 0);
    assert!(selector.placements().is_empty());
}

#[test]
fn the_candidate_length_tells_an_empty_list_from_a_query_that_keeps_nothing() {
    let empty = Selector::<Entry>::default();
    assert_eq!(empty.candidates_len(), 0, "no candidate at all");
    assert!(empty.matches().is_empty());

    let mut narrowed = selector(&[(Entry::First, "main", "")]);
    narrowed.set_query("zzz");
    assert_eq!(
        narrowed.candidates_len(),
        1,
        "one candidate exists, so the empty match list names a lost query"
    );
    assert!(narrowed.matches().is_empty());
}

#[test]
fn a_placement_names_the_row_position_and_the_matched_candidate() {
    let mut selector = long_selector(4);
    selector.set_height_rows(4);

    for placement in selector.placements() {
        let expected_candidate = selector.matches()[placement.index()];
        assert_eq!(placement.candidate_index(), expected_candidate);
        let candidate = selector
            .candidate(placement.candidate_index())
            .expect("the placement names one held candidate");
        assert_eq!(*candidate.id(), expected_candidate);
    }
}
