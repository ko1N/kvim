//! Unit tests for the open review of one captured diff.

use super::*;

use kvim_workspace::{
    BaseRevision, CandidateAuthority, DiffChange, DiffContent, DiffLine, DiffLineText, DiffOldSide,
    DiffTarget, DiffTruncation, FileDiff, FileMode, FileSide, HeadAuthority, Hunk, HunkId,
    IndexAuthority, LineEnding, LineOrigin, NewLine, NewLineRange, OldLine, OldLineRange, TextDiff,
};

const BASE_HEX: &str = "0123456789abcdef0123456789abcdef01234567";

fn path(value: &str) -> WorktreeRelativePath {
    WorktreeRelativePath::new(value).expect("the fixture names one contained path")
}

/// Builds one added file whose new side starts at the named line.
fn added(name: &str, first: u32, lines: &[&str]) -> FileDiff {
    let body: Vec<DiffLine> = lines
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let number = first + u32::try_from(index).expect("the fixture is short");
            DiffLine::new(
                LineOrigin::Added {
                    new: NewLine::new(number).expect("the fixture number is one line number"),
                },
                DiffLineText::new(text.as_bytes()).expect("the fixture text is short"),
                LineEnding::Newline,
            )
        })
        .collect();
    let count = u32::try_from(body.len()).expect("the fixture is short");
    let hunk = Hunk::new(
        HunkId::new(0),
        OldLineRange::new(OldLine::new(1).expect("one is one line number"), 0)
            .expect("an empty old range is usable"),
        NewLineRange::new(
            NewLine::new(first).expect("the fixture number is one line number"),
            count,
        )
        .expect("the fixture range is usable"),
        body,
    )
    .expect("the fixture hunk realizes its ranges");
    FileDiff::new(
        DiffChange::Added {
            new: FileSide::new(path(name), FileMode::Regular),
        },
        DiffContent::Text(
            TextDiff::new(vec![hunk], DiffTruncation::Complete).expect("the fixture is usable"),
        ),
    )
    .expect("the fixture file is usable")
}

fn candidate(files: Vec<FileDiff>, index: [u8; 32]) -> WorktreeDiff {
    let authority =
        CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest(index));
    WorktreeDiff::new(
        DiffOldSide::Commit(
            BaseRevision::new(BASE_HEX).expect("the fixture names one full identifier"),
        ),
        DiffTarget::Worktree,
        &authority,
        files,
        DiffTruncation::Complete,
    )
    .expect("the fixture candidate is usable")
}

fn surface(unstaged: Vec<FileDiff>) -> ReviewSurface {
    ReviewSurface::new(
        None,
        Some(candidate(unstaged, [1; 32])),
        DiffSettings::default(),
        20,
    )
}

#[test]
fn the_view_key_switches_the_two_views_and_returns() {
    let mut review = surface(vec![added("a.txt", 1, &["one"])]);
    assert_eq!(review.view(), DiffView::SideBySide);

    assert_eq!(
        review.apply(Command::ToggleReviewView),
        ReviewOutcome::Changed
    );
    assert_eq!(review.view(), DiffView::Inline);

    assert_eq!(
        review.apply(Command::ToggleReviewView),
        ReviewOutcome::Changed
    );
    assert_eq!(review.view(), DiffView::SideBySide);
}

#[test]
fn the_cursor_starts_in_the_half_that_a_reader_works_on() {
    // The unstaged half publishes a change, so the cursor starts there.
    let both = ReviewSurface::new(
        Some(candidate(vec![added("staged.txt", 1, &["one"])], [2; 32])),
        Some(candidate(vec![added("unstaged.txt", 1, &["two"])], [3; 32])),
        DiffSettings::default(),
        20,
    );
    assert_eq!(both.section(), ChangeSection::Unstaged);

    // A workspace with staged work alone starts there instead.
    let staged_only = ReviewSurface::new(
        Some(candidate(vec![added("staged.txt", 1, &["one"])], [4; 32])),
        Some(candidate(Vec::new(), [5; 32])),
        DiffSettings::default(),
        20,
    );
    assert_eq!(staged_only.section(), ChangeSection::Staged);
}

#[test]
fn the_hunk_walk_stops_at_the_border_and_changes_nothing_there() {
    let mut review = surface(vec![
        added("a.txt", 1, &["one"]),
        added("b.txt", 1, &["two"]),
    ]);

    assert_eq!(review.apply(Command::NextHunk), ReviewOutcome::Changed);
    assert_eq!(review.apply(Command::NextHunk), ReviewOutcome::Unchanged);
    assert_eq!(review.apply(Command::PreviousHunk), ReviewOutcome::Changed);
    assert_eq!(
        review.apply(Command::PreviousHunk),
        ReviewOutcome::Unchanged
    );
}

#[test]
fn the_file_walk_passes_every_hunk_of_the_file_that_it_leaves() {
    let mut review = surface(vec![
        added("a.txt", 1, &["one"]),
        added("b.txt", 1, &["two"]),
    ]);

    assert_eq!(
        review.apply(Command::NextChangedFile),
        ReviewOutcome::Changed
    );
    let cursor = review
        .active()
        .and_then(ReviewState::cursor)
        .expect("the walk placed the cursor");
    assert_eq!(cursor.file.path(), &path("b.txt"));

    // No further file follows, so the walk changes nothing.
    assert_eq!(
        review.apply(Command::NextChangedFile),
        ReviewOutcome::Unchanged
    );
}

#[test]
fn the_jump_names_the_file_and_the_first_line_of_the_hunk() {
    let mut review = surface(vec![added("src/main.rs", 42, &["one", "two"])]);

    assert_eq!(
        review.apply(Command::OpenHunkFile),
        ReviewOutcome::OpenFile {
            path: path("src/main.rs"),
            line: 42,
        }
    );
}

#[test]
fn a_read_mark_reaches_the_review_and_the_panel() {
    let mut review = surface(vec![added("a.txt", 1, &["one"])]);
    assert_eq!(
        review
            .active()
            .expect("the fixture holds one half")
            .unread_total(),
        1
    );

    assert_eq!(review.apply(Command::MarkHunkRead), ReviewOutcome::Changed);
    assert_eq!(
        review
            .active()
            .expect("the fixture holds one half")
            .unread_total(),
        0
    );
    // A second mark records nothing further.
    assert_eq!(
        review.apply(Command::MarkHunkRead),
        ReviewOutcome::Unchanged
    );
}

#[test]
fn a_reload_keeps_the_marks_that_the_later_capture_still_holds() {
    let mut review = surface(vec![added("a.txt", 1, &["one"])]);
    assert_eq!(review.apply(Command::MarkHunkRead), ReviewOutcome::Changed);

    review.reload(
        ChangeSection::Unstaged,
        candidate(vec![added("a.txt", 1, &["one"])], [9; 32]),
    );

    assert_eq!(
        review
            .active()
            .expect("the fixture holds one half")
            .unread_total(),
        0,
        "the content is unchanged, so the mark stays"
    );
}

#[test]
fn the_close_key_asks_the_session_to_restore_its_layout() {
    let mut review = surface(vec![added("a.txt", 1, &["one"])]);
    assert_eq!(review.apply(Command::CloseReview), ReviewOutcome::Close);
}

#[test]
fn a_command_of_another_surface_reaches_no_behavior_here() {
    let mut review = surface(vec![added("a.txt", 1, &["one"])]);
    assert_eq!(
        review.apply(Command::InsertBeforeCursor),
        ReviewOutcome::Unhandled
    );
}

#[test]
fn the_panel_follows_the_cursor_and_names_both_halves() {
    let mut review = ReviewSurface::new(
        Some(candidate(vec![added("staged.txt", 1, &["one"])], [20; 32])),
        Some(candidate(
            vec![added("a.txt", 1, &["one"]), added("b.txt", 1, &["two"])],
            [21; 32],
        )),
        DiffSettings::default(),
        20,
    );

    // Both halves stay reachable by name, and the panel lists them together.
    assert!(review.review(ChangeSection::Staged).is_some());
    assert!(review.review(ChangeSection::Unstaged).is_some());
    assert_eq!(
        review.changes().rows().len(),
        5,
        "two headings, three files"
    );

    // The panel selection follows the cursor into the next file.
    assert_eq!(
        review.apply(Command::NextChangedFile),
        ReviewOutcome::Changed
    );
    assert_eq!(
        review.changes().selected(),
        Some(&ChangesRow::File {
            section: ChangeSection::Unstaged,
            path: path("b.txt"),
        })
    );
}
