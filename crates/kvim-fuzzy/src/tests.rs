use super::{rank, score_candidate};

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

#[test]
fn an_empty_query_keeps_the_order_of_the_source() {
    let entries = [("zebra.rs", "src"), ("alpha.rs", ""), ("main.rs", "src")];
    assert_eq!(rank("", entries.iter().copied()), [0, 1, 2]);
}

#[test]
fn the_best_match_sits_at_the_top_of_the_list() {
    // "domain.rs" holds "main" as a spread subsequence too, so it stays in
    // the list behind the dense match of "main.rs".
    let entries = [
        ("domain.rs", "src"),
        ("main.rs", "src"),
        ("manual.md", "docs"),
    ];
    assert_eq!(rank("main", entries.iter().copied()), [1, 0]);
}

#[test]
fn a_dropped_entry_keeps_no_index() {
    let entries = [("main.rs", "src"), ("mode.rs", "src")];
    assert_eq!(rank("zz", entries.iter().copied()), Vec::<usize>::new());
}

#[test]
fn two_equal_scores_keep_one_deterministic_order() {
    // Both names hold the query at the same positions, so the shorter
    // combined name and container wins, and the earlier entry wins an equal
    // width.
    let entries = [("ab_long_name.rs", ""), ("ab.rs", ""), ("ab2.rs", "")];
    assert_eq!(rank("ab", entries.iter().copied()), [1, 2, 0]);
}
