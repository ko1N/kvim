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

fn count(value: u32) -> Option<NonZeroU32> {
    Some(NonZeroU32::new(value).expect("the fixture count is not zero"))
}

/// Builds one modified file that publishes two hunks.
fn two_hunk_file(name: &str) -> FileDiff {
    let hunk = |id: u32, first: u32| {
        let body = vec![DiffLine::new(
            LineOrigin::Added {
                new: NewLine::new(first).expect("the fixture number is one line number"),
            },
            DiffLineText::new(b"line".to_vec()).expect("the fixture text is short"),
            LineEnding::Newline,
        )];
        Hunk::new(
            HunkId::new(id),
            OldLineRange::new(OldLine::new(1).expect("one is one line number"), 0)
                .expect("an empty old range is usable"),
            NewLineRange::new(
                NewLine::new(first).expect("the fixture number is one line number"),
                1,
            )
            .expect("the fixture range is usable"),
            body,
        )
        .expect("the fixture hunk realizes its ranges")
    };
    FileDiff::new(
        DiffChange::Added {
            new: FileSide::new(path(name), FileMode::Regular),
        },
        DiffContent::Text(
            TextDiff::new(vec![hunk(0, 1), hunk(1, 10)], DiffTruncation::Complete)
                .expect("the fixture is usable"),
        ),
    )
    .expect("the fixture file is usable")
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
        review.apply(Command::ToggleReviewView, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.view(), DiffView::Inline);

    assert_eq!(
        review.apply(Command::ToggleReviewView, None),
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
    // The walk moves inside the body of one file, so one file with one hunk
    // holds one header and the walk reaches no second one.
    let mut review = surface(vec![added("a.txt", 1, &["one", "two"])]);

    assert_eq!(
        review.apply(Command::NextHunk, None),
        ReviewOutcome::Unchanged
    );
    assert_eq!(
        review.apply(Command::PreviousHunk, None),
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
        review.apply(Command::NextChangedFile, None),
        ReviewOutcome::Changed
    );
    let cursor = review
        .active()
        .and_then(ReviewState::cursor)
        .expect("the walk placed the cursor");
    assert_eq!(cursor.file.path(), &path("b.txt"));

    // No further file follows, so the walk changes nothing.
    assert_eq!(
        review.apply(Command::NextChangedFile, None),
        ReviewOutcome::Unchanged
    );
}

#[test]
fn the_jump_names_the_file_and_the_first_line_of_the_hunk() {
    let mut review = surface(vec![added("src/main.rs", 42, &["one", "two"])]);

    assert_eq!(
        review.apply(Command::OpenHunkFile, None),
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

    assert_eq!(
        review.apply(Command::MarkHunkRead, None),
        ReviewOutcome::Changed
    );
    assert_eq!(
        review
            .active()
            .expect("the fixture holds one half")
            .unread_total(),
        0
    );
    // A second mark records nothing further.
    assert_eq!(
        review.apply(Command::MarkHunkRead, None),
        ReviewOutcome::Unchanged
    );
}

#[test]
fn a_reload_keeps_the_marks_that_the_later_capture_still_holds() {
    let mut review = surface(vec![added("a.txt", 1, &["one"])]);
    assert_eq!(
        review.apply(Command::MarkHunkRead, None),
        ReviewOutcome::Changed
    );

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
    assert_eq!(
        review.apply(Command::CloseReview, None),
        ReviewOutcome::Close
    );
}

#[test]
fn a_command_of_another_surface_reaches_no_behavior_here() {
    let mut review = surface(vec![added("a.txt", 1, &["one"])]);
    assert_eq!(
        review.apply(Command::InsertBeforeCursor, None),
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

    // Both halves stay reachable by name, and the strip names both sections.
    assert!(review.review(ChangeSection::Staged).is_some());
    assert!(review.review(ChangeSection::Unstaged).is_some());
    assert_eq!(review.sections().len(), 2, "one tab for each section");
    // The panel lists the files of the active section alone.
    assert_eq!(
        review.changes().rows().len(),
        2,
        "the two files of the unstaged half"
    );

    // The panel selection follows the cursor into the next file.
    assert_eq!(
        review.apply(Command::NextChangedFile, None),
        ReviewOutcome::Changed
    );
    assert_eq!(
        review.changes().selected(),
        Some(&ChangesRow::File {
            section: ChangeSection::Unstaged,
            path: path("b.txt"),
            depth: 0,
        })
    );
}

#[test]
fn the_focus_moves_between_the_two_regions() {
    let mut review = surface(vec![added("a.txt", 1, &["one"])]);
    assert_eq!(review.focus(), ReviewFocus::Diff);

    assert_eq!(
        review.apply(Command::FocusWindowRight, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.focus(), ReviewFocus::Panel);
    // The region that already owns the keys changes nothing.
    assert_eq!(
        review.apply(Command::FocusWindowRight, None),
        ReviewOutcome::Unchanged
    );

    assert_eq!(
        review.apply(Command::FocusWindowLeft, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.focus(), ReviewFocus::Diff);
}

#[test]
fn the_body_holds_every_row_of_its_file_and_scrolls_them() {
    let lines: Vec<&str> = vec!["one", "two", "three", "four", "five", "six"];
    let mut review = surface(vec![added("a.txt", 1, &lines)]);
    review.set_height_rows(3);

    // One header and one row for each published line.
    assert_eq!(review.body().len(), lines.len() + 1);
    assert!(review.body()[0].is_header());
    assert_eq!(review.cursor_row(), 0);
    assert_eq!(review.first_row(), 0);

    // `j` moves one row and the viewport follows the cursor.
    assert_eq!(
        review.apply(Command::MoveDown, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.cursor_row(), 1);
    assert_eq!(review.first_row(), 0);

    assert_eq!(
        review.apply(Command::MoveDown, count(3)),
        ReviewOutcome::Changed
    );
    assert_eq!(review.cursor_row(), 4);
    assert_eq!(
        review.first_row(),
        2,
        "the viewport scrolled with the cursor"
    );

    // `G` reaches the last row and `gg` returns to the first.
    assert_eq!(
        review.apply(Command::MoveLastLine, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.cursor_row(), review.body().len() - 1);
    assert_eq!(
        review.apply(Command::MoveFirstLine, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.cursor_row(), 0);
    assert_eq!(review.first_row(), 0);

    // The border changes nothing.
    assert_eq!(
        review.apply(Command::MoveUp, None),
        ReviewOutcome::Unchanged
    );
}

#[test]
fn a_half_page_moves_by_the_height_of_the_region() {
    let lines: Vec<&str> = (0..20).map(|_| "line").collect();
    let mut review = surface(vec![added("a.txt", 1, &lines)]);
    review.set_height_rows(10);

    assert_eq!(
        review.apply(Command::MoveHalfPageDown, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.cursor_row(), 5);

    assert_eq!(
        review.apply(Command::MoveFullPageDown, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.cursor_row(), 14);
}

#[test]
fn a_motion_moves_the_region_that_owns_the_keys_alone() {
    let mut review = surface(vec![
        added("a.txt", 1, &["one", "two"]),
        added("b.txt", 1, &["three"]),
    ]);
    review.set_height_rows(10);
    let selected = review.changes().selected().cloned();

    // The body owns the keys, so `j` scrolls it and the panel selection stays.
    assert_eq!(review.focus(), ReviewFocus::Diff);
    assert_eq!(
        review.apply(Command::MoveDown, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.cursor_row(), 1);
    assert_eq!(review.changes().selected().cloned(), selected);

    // The panel owns the keys, so `j` moves its selection to the next file.
    assert_eq!(
        review.apply(Command::FocusWindowRight, None),
        ReviewOutcome::Changed
    );
    assert_eq!(
        review.apply(Command::MoveDown, None),
        ReviewOutcome::Changed
    );
    assert_eq!(
        review.changes().selected(),
        Some(&ChangesRow::File {
            section: ChangeSection::Unstaged,
            path: path("b.txt"),
            depth: 0,
        }),
    );
}

#[test]
fn selecting_a_file_in_the_panel_shows_that_file() {
    let mut review = surface(vec![
        added("a.txt", 1, &["one"]),
        added("b.txt", 1, &["two", "three"]),
    ]);
    review.set_height_rows(10);

    review.apply(Command::FocusWindowRight, None);
    // The heading takes no selection, so two steps reach the second file.
    review.apply(Command::MoveDown, None);
    review.apply(Command::MoveDown, None);

    let cursor = review
        .active()
        .and_then(ReviewState::cursor)
        .expect("the selection placed the review cursor");
    assert_eq!(cursor.file.path(), &path("b.txt"));
    assert_eq!(review.body().len(), 3, "one header and two published rows");
}

#[test]
fn the_hunk_walk_reaches_the_header_of_every_hunk() {
    let mut review = surface(vec![two_hunk_file("a.txt")]);
    review.set_height_rows(10);

    let headers: Vec<usize> = review
        .body()
        .iter()
        .enumerate()
        .filter(|(_, row)| row.is_header())
        .map(|(index, _)| index)
        .collect();
    assert_eq!(headers.len(), 2);

    assert_eq!(review.cursor_row(), headers[0]);
    assert_eq!(
        review.apply(Command::NextHunk, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.cursor_row(), headers[1]);
    assert_eq!(
        review.apply(Command::PreviousHunk, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.cursor_row(), headers[0]);
}

#[test]
fn the_panel_returns_to_a_file_that_it_already_showed() {
    // Walking down the list and back up must show every file again. The cursor
    // reaches a file in either direction, so no diff is shown only once.
    let mut review = surface(vec![
        added("a.txt", 1, &["one"]),
        added("b.txt", 1, &["two"]),
        added("c.txt", 1, &["three"]),
    ]);
    review.set_height_rows(10);
    review.apply(Command::FocusWindowRight, None);

    let shown = |review: &ReviewSurface| {
        review
            .active()
            .and_then(ReviewState::cursor)
            .map(|cursor| cursor.file.path().clone())
            .expect("the review holds one cursor")
    };

    // Down to the last file.
    review.apply(Command::MoveDown, None);
    review.apply(Command::MoveDown, None);
    review.apply(Command::MoveDown, None);
    assert_eq!(shown(&review), path("c.txt"));

    // Back up, one file at a time.
    review.apply(Command::MoveUp, None);
    assert_eq!(shown(&review), path("b.txt"));
    review.apply(Command::MoveUp, None);
    assert_eq!(shown(&review), path("a.txt"));

    // And down again, which must show the same files once more.
    review.apply(Command::MoveDown, None);
    assert_eq!(shown(&review), path("b.txt"));
}

#[test]
fn a_page_jump_keeps_the_body_on_its_own_file() {
    // A hunk identity restarts in every file, so a jump that crosses hunks must
    // never place the review cursor in another file. The body would then draw
    // rows that it looked up in the wrong one.
    let mut review = surface(vec![two_hunk_file("a.txt"), two_hunk_file("b.txt")]);
    review.set_height_rows(4);
    let rows = review.body().len();

    for _ in 0..8 {
        review.apply(Command::MoveHalfPageDown, None);
        assert_eq!(
            review.body_path(),
            Some(&path("a.txt")),
            "the body stays on the file that it drew"
        );
        assert_eq!(review.body().len(), rows, "the rows stay the rows of a.txt");
        let cursor = review
            .active()
            .and_then(ReviewState::cursor)
            .expect("the review holds one cursor");
        assert_eq!(
            cursor.file.path(),
            &path("a.txt"),
            "the review cursor stays inside the file of the body"
        );
    }

    // The same holds walking back up.
    for _ in 0..8 {
        review.apply(Command::MoveHalfPageUp, None);
        assert_eq!(review.body_path(), Some(&path("a.txt")));
    }
}

#[test]
fn one_key_walks_the_sections_of_the_review() {
    let mut review = ReviewSurface::new(
        Some(candidate(vec![added("staged.txt", 1, &["one"])], [40; 32])),
        Some(candidate(
            vec![added("unstaged.txt", 1, &["two"])],
            [41; 32],
        )),
        DiffSettings::default(),
        20,
    );

    // The review opens on the half that a reader works on.
    assert_eq!(review.section(), ChangeSection::Unstaged);

    assert_eq!(
        review.apply(Command::NextReviewSection, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.section(), ChangeSection::Staged);
    assert_eq!(
        review.body_path(),
        Some(&path("staged.txt")),
        "the body follows the section"
    );

    // The walk cycles, so one key reaches every section.
    assert_eq!(
        review.apply(Command::NextReviewSection, None),
        ReviewOutcome::Changed
    );
    assert_eq!(review.section(), ChangeSection::Unstaged);
}

#[test]
fn a_review_with_one_section_walks_to_nothing_new() {
    let mut review = surface(vec![added("a.txt", 1, &["one"])]);

    assert_eq!(review.sections().len(), 1, "the staged half publishes none");
    assert_eq!(
        review.apply(Command::NextReviewSection, None),
        ReviewOutcome::Unchanged
    );
    assert_eq!(review.section(), ChangeSection::Unstaged);
}
