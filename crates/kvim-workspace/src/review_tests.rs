use kvim_path::WorktreeRelativePath;

use crate::diff::{
    BaseRevision, CandidateAuthority, DiffChange, DiffContent, DiffLine, DiffLineText, DiffOldSide,
    DiffTruncation, FileMode, FileSide, HeadAuthority, Hunk, HunkId, IndexAuthority, LineEnding,
    LineOrigin, NewLine, NewLineRange, OldLine, OldLineRange, TextDiff,
};

use super::*;

const BASE_HEX: &str = "0123456789abcdef0123456789abcdef01234567";

fn path(value: &str) -> WorktreeRelativePath {
    WorktreeRelativePath::new(value).expect("the fixture names one contained path")
}

/// Builds one added text file whose new side holds the given lines.
fn added(name: &str, lines: &[&str], truncation: DiffTruncation) -> FileDiff {
    let body: Vec<DiffLine> = lines
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let number = u32::try_from(index).expect("the fixture holds few lines") + 1;
            DiffLine::new(
                LineOrigin::Added {
                    new: NewLine::new(number).expect("the fixture stays inside the bound"),
                },
                DiffLineText::new(text.as_bytes()).expect("the fixture holds few bytes"),
                LineEnding::Newline,
            )
        })
        .collect();
    let count = u32::try_from(body.len()).expect("the fixture holds few lines");
    let hunk = Hunk::new(
        HunkId::new(0),
        OldLineRange::new(OldLine::new(1).expect("one is inside the bound"), 0)
            .expect("an empty old range is usable"),
        NewLineRange::new(NewLine::new(1).expect("one is inside the bound"), count)
            .expect("the fixture range is usable"),
        body,
    )
    .expect("the fixture hunk realizes its ranges");
    FileDiff::new(
        DiffChange::Added {
            new: FileSide::new(path(name), FileMode::Regular),
        },
        DiffContent::Text(
            TextDiff::new(vec![hunk], truncation).expect("the fixture text is usable"),
        ),
    )
    .expect("the fixture file is usable")
}

/// Builds one binary file, which publishes no hunk and no selectable line.
fn binary(name: &str) -> FileDiff {
    FileDiff::new(
        DiffChange::Added {
            new: FileSide::new(path(name), FileMode::Regular),
        },
        DiffContent::Binary,
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

fn one_file(lines: &[&str]) -> WorktreeDiff {
    candidate(
        vec![added("a.txt", lines, DiffTruncation::Complete)],
        [7; 32],
    )
}

fn body(text: &str) -> CommentBody {
    CommentBody::new(text).expect("the fixture comment holds few bytes")
}

#[test]
fn navigation_skips_every_file_without_a_hunk() {
    let published = candidate(
        vec![
            binary("a.bin"),
            added("b.txt", &["one"], DiffTruncation::Complete),
            binary("c.bin"),
            added("d.txt", &["two"], DiffTruncation::Complete),
        ],
        [1; 32],
    );
    let mut review = ReviewState::new(published);

    assert_eq!(
        review
            .cursor()
            .expect("the candidate publishes a hunk")
            .file
            .path(),
        &path("b.txt")
    );
    assert_eq!(review.next_hunk(), HunkStep::Moved);
    assert_eq!(
        review
            .cursor()
            .expect("the candidate publishes a hunk")
            .file
            .path(),
        &path("d.txt")
    );
    assert_eq!(review.next_hunk(), HunkStep::AtBorder);
    assert_eq!(review.previous_hunk(), HunkStep::Moved);
    assert_eq!(
        review
            .cursor()
            .expect("the candidate publishes a hunk")
            .file
            .path(),
        &path("b.txt")
    );
    assert_eq!(review.previous_hunk(), HunkStep::AtBorder);
}

#[test]
fn a_candidate_without_a_hunk_holds_no_cursor() {
    let mut review = ReviewState::new(candidate(vec![binary("a.bin")], [2; 32]));

    assert!(review.cursor().is_none());
    assert_eq!(review.next_hunk(), HunkStep::AtBorder);
    assert_eq!(
        review.select(DiffSide::New, 1, 1),
        Err(ReviewSelectError::NoHunk)
    );
}

#[test]
fn a_step_drops_the_selection_of_the_earlier_hunk() {
    let published = candidate(
        vec![
            added("a.txt", &["one"], DiffTruncation::Complete),
            added("b.txt", &["two"], DiffTruncation::Complete),
        ],
        [3; 32],
    );
    let mut review = ReviewState::new(published);
    review
        .select(DiffSide::New, 1, 1)
        .expect("the hunk holds the line");

    assert_eq!(review.next_hunk(), HunkStep::Moved);
    assert!(review.selection().is_none());
}

#[test]
fn each_side_keeps_its_own_line_numbers() {
    let published = candidate(vec![modified()], [4; 32]);
    let mut review = ReviewState::new(published);

    let old = review
        .select(DiffSide::Old, 1, 1)
        .expect("the hunk publishes the old line")
        .clone();
    let new = review
        .select(DiffSide::New, 1, 1)
        .expect("the hunk publishes the new line")
        .clone();

    assert_eq!(old.side(), DiffSide::Old);
    assert_eq!(new.side(), DiffSide::New);
    assert_ne!(old.selection(), new.selection());
}

/// Builds one modified file whose two sides hold different bytes.
fn modified() -> FileDiff {
    let side = FileSide::new(path("a.txt"), FileMode::Regular);
    let hunk = Hunk::new(
        HunkId::new(0),
        OldLineRange::new(OldLine::new(1).expect("one is inside the bound"), 1)
            .expect("the fixture range is usable"),
        NewLineRange::new(NewLine::new(1).expect("one is inside the bound"), 1)
            .expect("the fixture range is usable"),
        vec![
            DiffLine::new(
                LineOrigin::Removed {
                    old: OldLine::new(1).expect("one is inside the bound"),
                },
                DiffLineText::new(*b"old").expect("the fixture holds few bytes"),
                LineEnding::Newline,
            ),
            DiffLine::new(
                LineOrigin::Added {
                    new: NewLine::new(1).expect("one is inside the bound"),
                },
                DiffLineText::new(*b"new").expect("the fixture holds few bytes"),
                LineEnding::Newline,
            ),
        ],
    )
    .expect("the fixture hunk realizes its ranges");
    FileDiff::new(
        DiffChange::Modified {
            old: side.clone(),
            new: side,
        },
        DiffContent::Text(
            TextDiff::new(vec![hunk], DiffTruncation::Complete)
                .expect("the fixture text is usable"),
        ),
    )
    .expect("the fixture file is usable")
}

#[test]
fn omitted_content_is_visible_and_cannot_be_selected() {
    let published = candidate(
        vec![added(
            "a.txt",
            &["one"],
            DiffTruncation::Truncated(DiffLimit::Lines),
        )],
        [5; 32],
    );
    let mut review = ReviewState::new(published);

    assert!(review.rows().any(|row| row
        == ReviewRow::Truncated {
            limit: DiffLimit::Lines
        }));
    assert_eq!(
        review
            .rows()
            .filter(|row| matches!(row, ReviewRow::Line { .. }))
            .count(),
        1
    );
    assert_eq!(
        review.select(DiffSide::New, 2, 1),
        Err(ReviewSelectError::Anchor(ReviewAnchorError::LinesMissing))
    );
}

#[test]
fn a_reload_relocates_the_selection_into_the_later_candidate() {
    let mut review = ReviewState::new(one_file(&["one", "two"]));
    review
        .select(DiffSide::New, 2, 1)
        .expect("the hunk publishes the line");

    let later = one_file(&["added", "one", "two"]);
    let outcome = review.reload(later.clone());

    assert!(matches!(outcome, Some(Relocation::Relocated { .. })));
    let anchor = review
        .selection()
        .expect("the later candidate holds the lines");
    assert_eq!(anchor.location().first(), 3);
    assert_eq!(anchor.candidate(), later.revision());
    assert_eq!(review.candidate(), &later);
}

#[test]
fn a_reload_drops_a_selection_that_the_later_candidate_lost() {
    let mut review = ReviewState::new(one_file(&["one", "two"]));
    review
        .select(DiffSide::New, 2, 1)
        .expect("the hunk publishes the line");

    let outcome = review.reload(one_file(&["one", "three"]));

    assert_eq!(outcome, Some(Relocation::Missing));
    assert!(review.selection().is_none());
}

#[test]
fn one_submission_publishes_the_anchor_and_the_body() {
    let published = one_file(&["one", "two"]);
    let captured = TargetAuthority::of(&published);
    let mut review = ReviewState::new(published);
    review
        .select(DiffSide::New, 2, 1)
        .expect("the hunk publishes the line");

    review
        .submit_comment(body("the name hides the unit"), &captured)
        .expect("the fresh capture holds the location");

    let Some(ReviewEvent::CommentSubmitted { anchor, body }) = review.take_event() else {
        unreachable!("one accepted submission queues one event")
    };
    assert_eq!(anchor.location().first(), 2);
    assert_eq!(body.as_str(), "the name hides the unit");
    assert_eq!(review.queued_events(), 0);
}

#[test]
fn a_submission_without_a_selection_publishes_nothing() {
    let published = one_file(&["one"]);
    let captured = TargetAuthority::of(&published);
    let mut review = ReviewState::new(published);

    assert_eq!(
        review.submit_comment(body("no place"), &captured),
        Err(SubmitCommentError::NoSelection)
    );
    assert_eq!(review.queued_events(), 0);
}

#[test]
fn changed_content_publishes_no_comment() {
    let mut review = ReviewState::new(one_file(&["one", "two"]));
    review
        .select(DiffSide::New, 2, 1)
        .expect("the hunk publishes the line");
    let captured = TargetAuthority::of(&one_file(&["one", "TWO"]));

    assert_eq!(
        review.submit_comment(body("late"), &captured),
        Err(SubmitCommentError::Stale(StaleLocation::Content))
    );
    assert_eq!(review.queued_events(), 0);
}

#[test]
fn a_changed_revision_publishes_no_comment() {
    let mut review = ReviewState::new(one_file(&["one", "two"]));
    review
        .select(DiffSide::New, 2, 1)
        .expect("the hunk publishes the line");

    // The staged authority changed while every published byte stayed, so
    // only the revision separates the two captures.
    let staged = candidate(
        vec![added("a.txt", &["one", "two"], DiffTruncation::Complete)],
        [9; 32],
    );
    let captured = TargetAuthority::of(&staged);

    assert_eq!(
        review.submit_comment(body("late"), &captured),
        Err(SubmitCommentError::Stale(StaleLocation::Revision))
    );
    assert_eq!(review.queued_events(), 0);
}

#[test]
fn a_capture_of_another_target_publishes_no_comment() {
    let mut review = ReviewState::new(one_file(&["one", "two"]));
    review
        .select(DiffSide::New, 2, 1)
        .expect("the hunk publishes the line");

    let authority =
        CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest([7; 32]));
    let other = WorktreeDiff::new(
        DiffOldSide::Commit(
            BaseRevision::new(BASE_HEX).expect("the fixture names one full identifier"),
        ),
        DiffTarget::Path(path("a.txt")),
        &authority,
        vec![added("a.txt", &["one", "two"], DiffTruncation::Complete)],
        DiffTruncation::Complete,
    )
    .expect("the fixture candidate is usable");

    assert_eq!(
        review.submit_comment(body("late"), &TargetAuthority::of(&other)),
        Err(SubmitCommentError::Stale(StaleLocation::Target))
    );
    assert_eq!(review.queued_events(), 0);
}

#[test]
fn a_full_queue_answers_before_the_submission_starts() {
    let published = one_file(&["one", "two"]);
    let captured = TargetAuthority::of(&published);
    let mut review = ReviewState::new(published);
    review
        .select(DiffSide::New, 2, 1)
        .expect("the hunk publishes the line");

    for _ in 0..REVIEW_EVENTS_MAX {
        review
            .submit_comment(body("queued"), &captured)
            .expect("the queue holds one free slot");
    }
    assert_eq!(review.queued_events(), REVIEW_EVENTS_MAX);

    // The stale authority would refuse the submission on its own. The full
    // queue answers first, so the host learns that it must drain before it
    // captures the target again.
    let stale = TargetAuthority::of(&one_file(&["one", "TWO"]));
    assert_eq!(
        review.submit_comment(body("dropped"), &stale),
        Err(SubmitCommentError::Saturated)
    );

    // The queue kept every earlier comment, so none disappeared.
    assert_eq!(review.queued_events(), REVIEW_EVENTS_MAX);
    assert!(review.take_event().is_some());
    review
        .submit_comment(body("accepted"), &captured)
        .expect("the drain freed one slot");
    assert_eq!(review.queued_events(), REVIEW_EVENTS_MAX);
}
