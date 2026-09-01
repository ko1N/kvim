use super::*;
use kvim_input::{BindingOverride, BindingScope, Command, Key, KeyCode, ReviewBindingProfile};
use kvim_workspace::{DiffLimit, DiffTruncation};
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

fn truncated_surface() -> ReviewSurface {
    let line =
        ReviewLine::new(ReviewLineOrigin::Added { new: 1 }, "one").expect("test line is valid");
    let hunk = ReviewHunk::new(1, 0, 1, 1, &[line]).expect("test hunk is valid");
    let file = ReviewFile::with_truncation(
        WorktreeRelativePath::new("src/lib.rs").expect("test path is valid"),
        ReviewFileChange::Added,
        &[hunk],
        DiffTruncation::Truncated(DiffLimit::Lines),
    )
    .expect("test file is valid");
    let candidate = ReviewCandidate::new(
        ReviewCandidateId::new("candidate-1").expect("test identity is bounded"),
        ReviewSection::Unstaged,
        &[file],
    )
    .expect("test candidate is valid");
    ReviewSurface::from_candidates(&[candidate], ReviewConfig::new(Rect::new(0, 0, 80, 16)))
        .expect("test candidate is valid")
}

#[test]
fn review_bindings_are_independent_from_editor_entry_and_navigation() {
    let standalone = surface("candidate-1", "one");
    assert!(standalone.bindings().entries().iter().any(|entry| {
        entry.command() == Command::NextReviewSection
            && entry.sequence().keys() == [Key::plain(KeyCode::Tab)]
    }));

    let config = ReviewConfig::new(Rect::new(0, 0, 80, 16))
        .binding_profile(ReviewBindingProfile::HostResolved);
    let mut host = ReviewSurface::from_candidates(&[candidate("candidate-1", "one")], config)
        .expect("host review bindings are valid");
    assert!(
        !host
            .bindings()
            .entries()
            .iter()
            .any(|entry| { entry.sequence().keys() == [Key::plain(KeyCode::Tab)] })
    );
    assert!(host.bindings().entries().iter().any(|entry| {
        entry.command() == Command::NextReviewSection
            && entry.sequence().keys()
                == [
                    Key::plain(KeyCode::Char(']')),
                    Key::plain(KeyCode::Char('s')),
                ]
    }));
    assert_eq!(
        host.input(ReviewInput::command(ReviewCommand::NextSection)),
        Ok(ReviewUpdate::Unchanged),
        "semantic navigation remains callable without a physical editor entry binding"
    );
}

#[test]
fn review_binding_overrides_reject_editor_scopes() {
    let replacement = kvim_input::BindingReplacement::new(
        BindingScope::Mode(kvim_input::Mode::Normal),
        &[Key::plain(KeyCode::Char('x'))],
        Command::NextHunk,
    )
    .expect("the sequence is bounded");
    let result = ReviewSurface::from_candidates(
        &[candidate("candidate-1", "one")],
        ReviewConfig::new(Rect::new(0, 0, 80, 16))
            .binding_overrides(&[BindingOverride::Replace(replacement)])
            .expect("the override count is bounded"),
    );
    assert!(matches!(result, Err(ReviewError::Bindings(_))));
}

#[test]
fn review_config_rejects_override_count_before_copying() {
    let overrides = vec![BindingOverride::Disable(Command::NextHunk); BINDING_OVERRIDES_MAX + 1];
    let result = ReviewConfig::new(Rect::new(0, 0, 80, 16)).binding_overrides(&overrides);
    assert!(matches!(
        result,
        Err(ReviewError::Bindings(
            BindingProfileError::TooManyOverrides { overrides }
        )) if overrides == BINDING_OVERRIDES_MAX + 1
    ));
}

#[test]
fn review_binding_overrides_reject_editor_commands() {
    let config = ReviewConfig::new(Rect::new(0, 0, 80, 16))
        .binding_overrides(&[BindingOverride::Enable(Command::OpenReview)])
        .expect("the override count is bounded");
    let result = ReviewSurface::from_candidates(&[candidate("candidate-1", "one")], config);
    assert!(matches!(
        result,
        Err(ReviewError::Bindings(
            BindingProfileError::InconsistentCommand {
                command: Command::OpenReview
            }
        ))
    ));
}

#[test]
fn supplied_truncation_survives_conversion_and_prevents_panel_dimming() {
    let mut review = truncated_surface();
    let before = review.panel_snapshot();
    let before_file = before
        .rows()
        .iter()
        .find(|row| !row.is_directory())
        .expect("the panel contains the supplied file");
    assert!(before_file.is_truncated());
    assert_eq!(before_file.truncation(), Some(DiffLimit::Lines));
    assert!(!before_file.is_complete());

    assert_eq!(
        review
            .input(ReviewInput::command(ReviewCommand::MarkRead))
            .expect("event capacity remains"),
        ReviewUpdate::Changed
    );
    let after = review.panel_snapshot();
    let after_file = after
        .rows()
        .iter()
        .find(|row| !row.is_directory())
        .expect("the panel retains the supplied file");
    assert!(after_file.is_truncated());
    assert_eq!(after_file.truncation(), Some(DiffLimit::Lines));
    assert!(
        !after_file.is_complete(),
        "a truncated file cannot dim after every published hunk is read"
    );
}

#[test]
fn changed_file_panel_snapshot_matches_the_current_painter_model() {
    let review = surface("candidate-1", "one");
    let panel = review.panel_snapshot();

    assert_eq!(panel.root_label(), "Review");
    assert_eq!(panel.focus(), ReviewFocus::Diff);
    assert_eq!(panel.rows().len(), 2);
    assert_eq!(panel.selected(), Some(panel.rows()[1].id()));
    assert!(panel.rows()[0].is_directory());
    assert_eq!(panel.rows()[0].label(), "src");
    assert_eq!(panel.rows()[0].icon(), "\u{f07c}");
    assert_eq!(panel.rows()[1].label(), "lib.rs");
    assert_eq!(panel.rows()[1].icon(), "\u{f15b}");
    assert_eq!(panel.rows()[1].git(), Some(ReviewPanelGitState::Modified));
    assert_eq!(panel.placements().len(), 2);
    assert_eq!(panel.placements()[1].row(), 1);
    assert_eq!(panel.placements()[0].area().x, panel.rows_area().x);
    assert_eq!(panel.placements()[0].area().y, panel.rows_area().y);
    assert!(panel.placements()[0].area().right() <= panel.rows_area().right());
    assert_eq!(panel.headings().len(), 1);
    assert_eq!(panel.headings()[0].section(), ReviewPanelSection::Unstaged);
    assert!(panel.headings()[0].is_active());

    let mut painted = Buffer::empty(Rect::new(0, 0, 80, 16));
    review.render(&mut painted).expect("snapshot geometry fits");
    for placement in panel.placements() {
        let row = &panel.rows()[placement.row()];
        let drawn: String = (placement.area().x..placement.area().right())
            .filter_map(|x| painted.cell((x, placement.area().y)))
            .map(|cell| cell.symbol())
            .collect();
        let expected: String = row.text().chars().skip(1).collect();
        let drawn_without_selection_mark: String = drawn.chars().skip(1).collect();
        assert!(drawn_without_selection_mark.starts_with(&expected));
    }
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

#[cfg(feature = "worktree")]
fn git(root: &Path, arguments: &[&str]) {
    let status = std::process::Command::new("git")
        .args(arguments)
        .current_dir(root)
        .status()
        .expect("test git command starts");
    assert!(status.success(), "test git command succeeds");
}

#[cfg(feature = "worktree")]
fn worktree_surface(name: &str) -> (std::path::PathBuf, ReviewSurface) {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("test clock follows epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("kvim-review-{name}-{unique}"));
    std::fs::create_dir(&root).expect("test root is created");
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "review@example.invalid"]);
    git(&root, &["config", "user.name", "Review Test"]);
    std::fs::write(root.join("notes.txt"), "one\n").expect("test file is written");
    git(&root, &["add", "notes.txt"]);
    git(&root, &["commit", "-qm", "initial"]);
    let surface = ReviewSurface::for_worktree(&root, ReviewConfig::new(Rect::new(0, 0, 80, 16)))
        .expect("worktree review opens");
    (root, surface)
}

#[cfg(feature = "worktree")]
async fn capture_pair(review: &mut ReviewSurface) {
    loop {
        let _ = review.dispatch().expect("capture dispatch has capacity");
        let completion = review.ready().await.expect("capture completion arrives");
        let _ = review.apply(completion).expect("completion is current");
        let mut finished = false;
        while let Some(event) = review.event() {
            match event {
                ReviewEvent::CaptureFinished { .. } => finished = true,
                ReviewEvent::Redraw if finished => return,
                _ => {}
            }
        }
    }
}

#[cfg(feature = "worktree")]
async fn shutdown(review: ReviewSurface) {
    match review
        .shutdown(std::time::Duration::from_secs(2))
        .await
        .expect("shutdown is supported")
    {
        ReviewShutdown::Finished { .. } => {}
        ReviewShutdown::Draining(drain) => {
            let _ = drain.complete().await;
        }
    }
}

#[cfg(feature = "worktree")]
#[test]
fn worktree_review_rejects_wrong_instance_and_stale_completion_atomically() {
    let (first_root, mut first) = worktree_surface("routing-first");
    let (second_root, mut second) = worktree_surface("routing-second");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime starts");
    runtime.block_on(async {
        first.dispatch().expect("pair dispatches");
        let completion = first.ready().await.expect("completion arrives");
        let first_snapshot = first.snapshot();
        let second_snapshot = second.snapshot();
        let error = second.apply(completion).expect_err("instance is checked");
        assert!(matches!(
            error.kind(),
            ReviewApplyErrorKind::WrongInstance { .. }
        ));
        assert_eq!(first.snapshot(), first_snapshot);
        assert_eq!(second.snapshot(), second_snapshot);

        let completion = error.into_completion();
        first.request_reload().expect("reload advances identity");
        let error = first.apply(completion).expect_err("old pair is stale");
        assert!(matches!(
            error.kind(),
            ReviewApplyErrorKind::StaleRequest { .. }
        ));
        assert_eq!(first.snapshot(), first_snapshot);
        assert_eq!(first.event(), None);

        shutdown(first).await;
        shutdown(second).await;
    });
    std::fs::remove_dir_all(first_root).expect("first test root is removed");
    std::fs::remove_dir_all(second_root).expect("second test root is removed");
}

#[cfg(feature = "worktree")]
#[test]
fn supplied_review_rejects_worktree_completion_without_panicking() {
    let (root, mut worktree) = worktree_surface("not-worktree");
    let mut supplied = surface("supplied", "line");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime starts");
    runtime.block_on(async {
        worktree.dispatch().expect("pair dispatches");
        let completion = worktree.ready().await.expect("completion arrives");
        let before = supplied.snapshot();
        let error = supplied
            .apply(completion)
            .expect_err("supplied surface rejects capture completion");
        assert_eq!(error.kind(), ReviewApplyErrorKind::NotWorktree);
        assert_eq!(supplied.snapshot(), before);
        assert_eq!(supplied.event(), None);
        shutdown(worktree).await;
    });
    std::fs::remove_dir_all(root).expect("test root is removed");
}

#[cfg(feature = "worktree")]
#[test]
fn worktree_review_supports_empty_staged_only_and_unstaged_only_pairs() {
    let (root, mut review) = worktree_surface("pair-shapes");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime starts");
    runtime.block_on(async {
        capture_pair(&mut review).await;
        assert!(review.snapshot().staged_id().is_some());
        assert!(review.snapshot().unstaged_id().is_some());

        std::fs::write(root.join("staged.txt"), "staged\n").expect("staged file is written");
        git(&root, &["add", "staged.txt"]);
        review.request_reload().expect("staged reload starts");
        capture_pair(&mut review).await;
        assert!(review.snapshot().staged_id().is_some());
        assert!(review.snapshot().unstaged_id().is_some());

        git(&root, &["reset", "-q", "HEAD", "staged.txt"]);
        review.request_reload().expect("unstaged reload starts");
        capture_pair(&mut review).await;
        assert!(review.snapshot().staged_id().is_some());
        assert!(review.snapshot().unstaged_id().is_some());

        shutdown(review).await;
    });
    std::fs::remove_dir_all(root).expect("test root is removed");
}

#[cfg(feature = "worktree")]
#[test]
fn worktree_review_captures_pair_reloads_and_shuts_down() {
    let (root, mut review) = worktree_surface("lifecycle");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime starts");
    runtime.block_on(async {
        std::fs::write(root.join("notes.txt"), "one\ntwo\n").expect("test worktree changes");
        capture_pair(&mut review).await;
        assert!(review.snapshot().staged_id().is_some());
        assert!(review.snapshot().unstaged_id().is_some());

        review
            .input(ReviewInput::command(ReviewCommand::MarkRead))
            .expect("mark read has event capacity");
        while review.event().is_some() {}
        let read = review.snapshot().unstaged_read().len();
        assert_eq!(read, 1);

        std::fs::write(root.join("notes.txt"), "one\ntwo\nthree\n").expect("test worktree changes");
        review.request_reload().expect("reload starts");
        capture_pair(&mut review).await;
        assert!(review.snapshot().unstaged_read().len() <= read);

        let shutdown = review
            .shutdown(std::time::Duration::from_secs(2))
            .await
            .expect("shutdown is supported");
        assert!(matches!(shutdown, ReviewShutdown::Finished { .. }));
    });
    std::fs::remove_dir_all(root).expect("test root is removed");
}

#[cfg(feature = "worktree")]
#[test]
fn worktree_review_cancels_without_publishing_partial_pair() {
    let (root, mut review) = worktree_surface("cancel");
    let request = review.cancel_capture().expect("capture can be cancelled");
    assert_eq!(
        review.event(),
        Some(ReviewEvent::CaptureCancelled { request })
    );
    assert!(review.snapshot().staged_id().is_none());
    assert!(review.snapshot().unstaged_id().is_none());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime starts");
    runtime.block_on(async {
        let shutdown = review.shutdown(std::time::Duration::from_secs(2)).await;
        assert!(matches!(shutdown, Ok(ReviewShutdown::Finished { .. })));
    });
    std::fs::remove_dir_all(root).expect("test root is removed");
}
