//! Unit tests for the screen rows of one aligned hunk.

use super::*;

use crate::diff::{DiffLineText, HunkId, LineEnding, NewLine, NewLineRange, OldLine, OldLineRange};

/// Returns one published line of the named origin.
fn line(origin: LineOrigin, text: &str) -> DiffLine {
    DiffLine::new(
        origin,
        DiffLineText::new(text.as_bytes().to_vec()).expect("the fixture text is short"),
        LineEnding::Newline,
    )
}

fn context(old: u32, new: u32, text: &str) -> DiffLine {
    line(
        LineOrigin::Context {
            old: OldLine::new(old).expect("the fixture number is one line number"),
            new: NewLine::new(new).expect("the fixture number is one line number"),
        },
        text,
    )
}

fn removed(old: u32, text: &str) -> DiffLine {
    line(
        LineOrigin::Removed {
            old: OldLine::new(old).expect("the fixture number is one line number"),
        },
        text,
    )
}

fn added(new: u32, text: &str) -> DiffLine {
    line(
        LineOrigin::Added {
            new: NewLine::new(new).expect("the fixture number is one line number"),
        },
        text,
    )
}

/// Returns one hunk over the published lines, with ranges that they realize.
fn hunk(lines: Vec<DiffLine>) -> Hunk {
    let old_count = lines
        .iter()
        .filter(|line| line.number(DiffSide::Old).is_some())
        .count();
    let new_count = lines
        .iter()
        .filter(|line| line.number(DiffSide::New).is_some())
        .count();
    Hunk::new(
        HunkId::new(1),
        OldLineRange::new(
            OldLine::new(1).expect("one is one line number"),
            u32::try_from(old_count).expect("the fixture is short"),
        )
        .expect("the fixture range is valid"),
        NewLineRange::new(
            NewLine::new(1).expect("one is one line number"),
            u32::try_from(new_count).expect("the fixture is short"),
        )
        .expect("the fixture range is valid"),
        lines,
    )
    .expect("the fixture lines realize both ranges")
}

/// Returns the text of both sides of one row, or a gap marker.
fn texts(row: &AlignedRow<'_>) -> (String, String) {
    let side = |line: Option<&DiffLine>| {
        line.map_or_else(
            || "-".to_owned(),
            |line| String::from_utf8_lossy(line.text().as_bytes()).into_owned(),
        )
    };
    (side(row.old()), side(row.new()))
}

#[test]
fn a_context_line_stands_on_both_sides_of_one_row() {
    let hunk = hunk(vec![context(1, 1, "same")]);
    let rows = align_hunk(&hunk);

    assert_eq!(rows.len(), 1);
    assert_eq!(texts(&rows[0]), ("same".to_owned(), "same".to_owned()));
    assert!(rows[0].is_context());
}

#[test]
fn a_replaced_run_pairs_one_for_one() {
    let hunk = hunk(vec![
        removed(1, "old one"),
        removed(2, "old two"),
        added(1, "new one"),
        added(2, "new two"),
    ]);
    let rows = align_hunk(&hunk);

    assert_eq!(rows.len(), 2);
    assert_eq!(
        texts(&rows[0]),
        ("old one".to_owned(), "new one".to_owned())
    );
    assert_eq!(
        texts(&rows[1]),
        ("old two".to_owned(), "new two".to_owned())
    );
    assert!(!rows[0].is_context());
}

#[test]
fn a_surplus_on_either_side_draws_against_one_gap() {
    // Two removals answered by three additions leave one addition alone.
    let longer = hunk(vec![
        removed(1, "old one"),
        removed(2, "old two"),
        added(1, "new one"),
        added(2, "new two"),
        added(3, "new three"),
    ]);
    let rows = align_hunk(&longer);

    assert_eq!(rows.len(), 3);
    assert_eq!(texts(&rows[2]), ("-".to_owned(), "new three".to_owned()));

    // The other direction leaves one removal alone.
    let shorter = hunk(vec![
        removed(1, "gone"),
        removed(2, "also gone"),
        added(1, "kept"),
    ]);
    let rows = align_hunk(&shorter);

    assert_eq!(rows.len(), 2);
    assert_eq!(texts(&rows[1]), ("also gone".to_owned(), "-".to_owned()));
}

#[test]
fn an_addition_without_a_removal_holds_an_empty_old_side() {
    let hunk = hunk(vec![context(1, 1, "before"), added(2, "fresh")]);
    let rows = align_hunk(&hunk);

    assert_eq!(rows.len(), 2);
    assert_eq!(texts(&rows[1]), ("-".to_owned(), "fresh".to_owned()));
    assert!(rows[1].old().is_none());
}

#[test]
fn the_row_count_never_passes_the_longer_published_side() {
    let lines = vec![
        context(1, 1, "one"),
        removed(2, "two"),
        added(2, "TWO"),
        added(3, "extra"),
        context(3, 4, "four"),
    ];
    let old_lines = lines
        .iter()
        .filter(|line| line.number(DiffSide::Old).is_some())
        .count();
    let new_lines = lines
        .iter()
        .filter(|line| line.number(DiffSide::New).is_some())
        .count();
    let hunk = hunk(lines);
    let rows = align_hunk(&hunk);

    assert_eq!(rows.len(), old_lines.max(new_lines));
}

#[test]
fn every_published_line_reaches_exactly_one_row() {
    let hunk = hunk(vec![
        context(1, 1, "one"),
        removed(2, "two"),
        removed(3, "three"),
        added(2, "TWO"),
        context(4, 3, "four"),
    ]);
    let rows = align_hunk(&hunk);

    let drawn = rows
        .iter()
        .filter_map(|row| row.old())
        .chain(rows.iter().filter_map(|row| row.new()))
        .count();
    // Every context line draws twice, once in each column.
    let context_lines = 2;
    assert_eq!(drawn, hunk.lines().len() + context_lines);
}
