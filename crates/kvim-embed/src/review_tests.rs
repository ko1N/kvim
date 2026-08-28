use super::*;
use ratatui::layout::Rect;
use std::path::Path;

fn candidate(id: &str, line: &str) -> ReviewCandidate {
    let line =
        ReviewLine::new(ReviewLineOrigin::Added { new: 1 }, line).expect("test line is valid");
    let hunk = ReviewHunk::new(1, 0, 1, 1, &[line]).expect("test hunk is valid");
    let file = ReviewFile::new(
        WorktreeRelativePath::new("src/lib.rs").expect("test path is valid"),
        ReviewFileChange::Added,
        &[hunk],
    )
    .expect("test file is valid");
    ReviewCandidate::new(
        ReviewCandidateId::new(id).expect("test identity is bounded"),
        ReviewSection::Unstaged,
        &[file],
    )
    .expect("test candidate is valid")
}

fn surface(id: &str, line: &str) -> ReviewSurface {
    ReviewSurface::from_candidates(
        &[candidate(id, line)],
        ReviewConfig::new(Rect::new(0, 0, 80, 16)),
    )
    .expect("test candidate is valid")
}

#[test]
fn supplied_review_renders_and_publishes_host_events() {
    let mut review = surface("candidate-1", "one");
    let mut cells = Buffer::empty(Rect::new(0, 0, 80, 16));
    let rendered = review.render(&mut cells).expect("geometry fits");
    assert_eq!(
        rendered.cursor(),
        None,
        "review paints selection without a terminal cursor"
    );

    assert_eq!(
        review
            .input(ReviewInput::command(ReviewCommand::MarkRead))
            .expect("event capacity remains"),
        ReviewUpdate::Changed
    );
    assert_eq!(review.event(), Some(ReviewEvent::ReadStateChanged));
    assert_eq!(review.event(), Some(ReviewEvent::Redraw));
    assert_eq!(review.snapshot().anchor_count(), 2);

    assert_eq!(
        review
            .input(ReviewInput::command(ReviewCommand::OpenFile))
            .expect("event capacity remains"),
        ReviewUpdate::Event
    );
    assert!(matches!(
        review.event(),
        Some(ReviewEvent::OpenFile { path, line: 1 }) if path.as_path() == Path::new("src/lib.rs")
    ));

    let body = ReviewCommentBody::new("Please rename this").expect("comment is bounded");
    review
        .input(ReviewInput::command(ReviewCommand::SubmitComment(body)))
        .expect("event capacity remains");
    assert!(matches!(
        review.event(),
        Some(ReviewEvent::CommentSubmitted { anchor, body })
            if anchor.path().as_path() == Path::new("src/lib.rs")
                && body.as_str() == "Please rename this"
    ));
}

#[test]
fn snapshot_restores_and_relocates_against_same_identity() {
    let mut first = surface("candidate-1", "one");
    first
        .input(ReviewInput::command(ReviewCommand::MarkRead))
        .expect("event capacity remains");
    while first.event().is_some() {}
    let snapshot = first.snapshot();

    assert_eq!(
        snapshot.unstaged_id().map(ReviewCandidateId::as_str),
        Some("candidate-1")
    );
    assert_eq!(snapshot.unstaged_read().len(), 1);
    assert!(snapshot.cursor().is_some());

    let reconstructed = ReviewSnapshot::new(
        snapshot.staged_id().cloned(),
        snapshot.unstaged_id().cloned(),
        snapshot.staged_read().to_vec(),
        snapshot.unstaged_read().to_vec(),
        snapshot
            .cursor()
            .map(|(section, anchor)| (section, anchor.clone())),
        snapshot.focus(),
        snapshot.view(),
        snapshot.panel_cells(),
    )
    .expect("exported snapshot satisfies its public bounds");
    assert_eq!(reconstructed, snapshot);

    let mut restored = surface("candidate-1", "one");
    assert_eq!(restored.restore(&snapshot), Ok(ReviewUpdate::Changed));
    assert_eq!(restored.snapshot(), snapshot);
    assert_eq!(restored.event(), Some(ReviewEvent::Redraw));

    let mut changed = surface("candidate-1", "two");
    assert_eq!(changed.restore(&snapshot), Ok(ReviewUpdate::Changed));
    assert_eq!(changed.event(), Some(ReviewEvent::StaleSnapshotAnchor));
    assert_eq!(changed.event(), Some(ReviewEvent::StaleSnapshotAnchor));
    assert_eq!(changed.event(), Some(ReviewEvent::Redraw));
}

#[test]
fn reload_replaces_a_changed_logical_identity_safely() {
    let mut review = surface("candidate-1", "one");
    review
        .input(ReviewInput::command(ReviewCommand::MarkRead))
        .expect("event capacity remains");
    while review.event().is_some() {}

    assert_eq!(
        review.reload(&[candidate("candidate-2", "two")]),
        Ok(ReviewUpdate::Changed)
    );
    assert_eq!(review.event(), Some(ReviewEvent::ReplacedCandidate));
    assert_eq!(review.event(), Some(ReviewEvent::Redraw));
    assert_eq!(review.snapshot().unstaged_read().len(), 0);
}

#[test]
fn public_constructors_reject_values_before_ownership() {
    let oversized_line = "x".repeat(DIFF_LINE_BYTES_MAX + 1);
    assert!(ReviewLine::new(ReviewLineOrigin::Added { new: 1 }, &oversized_line).is_err());
    assert!(ReviewLine::new(ReviewLineOrigin::Added { new: 0 }, "x").is_err());
    assert!(ReviewHunk::new(1, 0, 1, 0, &[]).is_err());

    let too_many_hunks = vec![
        ReviewHunk::new(
            1,
            0,
            1,
            1,
            &[ReviewLine::new(ReviewLineOrigin::Added { new: 1 }, "x").unwrap()],
        )
        .unwrap();
        REVIEW_FILE_HUNKS_MAX + 1
    ];
    assert!(
        ReviewFile::new(
            WorktreeRelativePath::new("a").unwrap(),
            ReviewFileChange::Added,
            &too_many_hunks,
        )
        .is_err()
    );

    let oversized = "x".repeat(REVIEW_ROOT_LABEL_BYTES_MAX + 1);
    assert!(
        ReviewConfig::new(Rect::new(0, 0, 80, 16))
            .with_root_label(&oversized)
            .is_err()
    );

    let snapshot = ReviewSnapshot::new(
        None,
        None,
        Vec::new(),
        Vec::new(),
        None,
        ReviewFocus::Diff,
        DiffView::Inline,
        0,
    );
    assert_eq!(snapshot, Err(ReviewError::SnapshotPanelWidth));
}

#[test]
fn construction_rejects_surface_collection_bounds() {
    let candidates = vec![
        candidate("one", "one"),
        candidate("two", "two"),
        candidate("three", "three"),
    ];
    assert!(matches!(
        ReviewSurface::from_candidates(&candidates, ReviewConfig::new(Rect::new(0, 0, 80, 16))),
        Err(ReviewError::CandidateCapacity)
    ));
}
