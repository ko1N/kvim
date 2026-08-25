//! Unit tests for the cluster boundary helpers.

use super::*;

use kvim_settings::FileSettings;

/// `e` and a combining acute, then `x`, then `a` and two combining marks.
const MARKED: &str = "e\u{301}xa\u{300}\u{308}\n";

fn buffer(text: &str) -> TextBuffer {
    TextBuffer::from_text(text, &FileSettings::default()).expect("the test text is small")
}

fn first_line(buffer: &TextBuffer) -> LineIndex {
    buffer.line_index(0).expect("the first line exists")
}

#[test]
fn an_ascii_line_keeps_every_column() {
    let buffer = buffer("alpha\n");
    let line = first_line(&buffer);
    for column in 0..=5 {
        assert_eq!(snapped_column(&buffer, line, column), column);
    }
    assert_eq!(column_left(&buffer, line, 3, 2), 1);
    assert_eq!(column_right(&buffer, line, 3, 2), 5);
}

#[test]
fn a_column_inside_a_cluster_moves_to_the_cluster_start() {
    let buffer = buffer(MARKED);
    let line = first_line(&buffer);
    // The clusters start at 0, 2, and 3, and the line holds six characters.
    assert_eq!(snapped_column(&buffer, line, 0), 0);
    assert_eq!(snapped_column(&buffer, line, 1), 0);
    assert_eq!(snapped_column(&buffer, line, 2), 2);
    assert_eq!(snapped_column(&buffer, line, 3), 3);
    assert_eq!(snapped_column(&buffer, line, 4), 3);
    assert_eq!(snapped_column(&buffer, line, 5), 3);
    assert_eq!(snapped_column(&buffer, line, 6), 6);
}

#[test]
fn one_step_passes_one_whole_cluster() {
    let buffer = buffer(MARKED);
    let line = first_line(&buffer);
    assert_eq!(column_right(&buffer, line, 0, 1), 2);
    assert_eq!(column_right(&buffer, line, 2, 1), 3);
    assert_eq!(column_left(&buffer, line, 3, 1), 2);
    assert_eq!(column_left(&buffer, line, 2, 1), 0);
}

#[test]
fn a_step_stops_at_the_line_limits() {
    let buffer = buffer(MARKED);
    let line = first_line(&buffer);
    assert_eq!(column_left(&buffer, line, 3, 9), 0);
    assert_eq!(column_right(&buffer, line, 0, 9), 6);
}

#[test]
fn a_count_names_clusters_and_not_characters() {
    let buffer = buffer(MARKED);
    let line = first_line(&buffer);
    // Three clusters hold six characters, so a count of two reaches the third.
    assert_eq!(column_right(&buffer, line, 0, 2), 3);
    assert_eq!(column_left(&buffer, line, 3, 2), 0);
}

#[test]
fn a_step_left_stops_at_the_first_column_of_an_empty_line() {
    let buffer = buffer("\n");
    let line = first_line(&buffer);
    assert_eq!(snapped_column(&buffer, line, 0), 0);
    assert_eq!(column_left(&buffer, line, 0, 3), 0);
}
