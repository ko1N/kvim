use kvim_path::WorktreeRelativePath;

use super::*;

const BASE_HEX: &str = "0123456789abcdef0123456789abcdef01234567";

fn base() -> BaseRevision {
    BaseRevision::new(BASE_HEX).expect("the fixture names a full SHA-1 identifier")
}

/// The old side of every fixture candidate: the base commit above.
fn old_side() -> DiffOldSide {
    DiffOldSide::Commit(base())
}

fn authority() -> CandidateAuthority {
    CandidateAuthority::new(
        HeadAuthority::Commit(base()),
        IndexAuthority::from_digest([7; DIGEST_BYTES]),
    )
}

fn path(name: &str) -> WorktreeRelativePath {
    WorktreeRelativePath::new(name).expect("the fixture names a contained path")
}

fn side(name: &str, mode: FileMode) -> FileSide {
    FileSide::new(path(name), mode)
}

fn line_text(bytes: &str) -> DiffLineText {
    DiffLineText::new(bytes.as_bytes().to_vec()).expect("the fixture line holds no line feed")
}

fn old_line(number: u32) -> OldLine {
    OldLine::new(number).expect("the fixture names a bounded line")
}

fn new_line(number: u32) -> NewLine {
    NewLine::new(number).expect("the fixture names a bounded line")
}

fn context_line(old: u32, new: u32, bytes: &str) -> DiffLine {
    DiffLine::new(
        LineOrigin::Context {
            old: old_line(old),
            new: new_line(new),
        },
        line_text(bytes),
        LineEnding::Newline,
    )
}

fn removed_line(old: u32, bytes: &str) -> DiffLine {
    DiffLine::new(
        LineOrigin::Removed { old: old_line(old) },
        line_text(bytes),
        LineEnding::Newline,
    )
}

fn added_line(new: u32, bytes: &str) -> DiffLine {
    DiffLine::new(
        LineOrigin::Added { new: new_line(new) },
        line_text(bytes),
        LineEnding::Newline,
    )
}

/// Builds one hunk that only adds the supplied lines to an empty old side.
fn added_hunk(lines: &[&str]) -> Hunk {
    let count = u32::try_from(lines.len()).expect("the fixture holds few lines");
    let lines = lines
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            let number = u32::try_from(index).expect("the fixture holds few lines") + 1;
            added_line(number, bytes)
        })
        .collect();
    Hunk::new(
        HunkId::new(0),
        OldLineRange::new(old_line(1), 0).expect("an empty old range is valid"),
        NewLineRange::new(new_line(1), count).expect("the fixture stays inside the bound"),
        lines,
    )
    .expect("the fixture lines realize both ranges")
}

fn added_file(name: &str, lines: &[&str]) -> FileDiff {
    FileDiff::new(
        DiffChange::Added {
            new: side(name, FileMode::Regular),
        },
        DiffContent::Text(
            TextDiff::new(vec![added_hunk(lines)], DiffTruncation::Complete)
                .expect("one hunk needs no order"),
        ),
    )
    .expect("a regular mode publishes text")
}

fn candidate(files: Vec<FileDiff>) -> WorktreeDiff {
    WorktreeDiff::new(
        old_side(),
        DiffTarget::Worktree,
        &authority(),
        files,
        DiffTruncation::Complete,
    )
    .expect("the fixture files rise by path")
}

// ---------------------------------------------------------------------
// Base revisions
// ---------------------------------------------------------------------

#[test]
fn base_revision_accepts_both_object_formats() {
    let sha1 = BaseRevision::new(&"AB".repeat(SHA1_HEX_CHARS / 2))
        .expect("40 hexadecimal characters name one SHA-1 commit");
    let sha256 = BaseRevision::new(&"ab".repeat(SHA256_HEX_CHARS / 2))
        .expect("64 hexadecimal characters name one SHA-256 commit");

    assert_eq!(sha1.to_hex(), "ab".repeat(SHA1_HEX_CHARS / 2));
    assert_eq!(sha1.as_bytes().len(), SHA1_HEX_CHARS / 2);
    assert_eq!(sha256.as_bytes().len(), SHA256_HEX_CHARS / 2);
    assert_ne!(sha1.as_bytes(), sha256.as_bytes());
}

#[test]
fn base_revision_rejects_an_abbreviation() {
    assert_eq!(
        BaseRevision::new("0123456"),
        Err(BaseRevisionError::Length { actual: 7 })
    );
}

#[test]
fn base_revision_rejects_a_character_that_is_no_digit() {
    let mut hex = BASE_HEX.to_string();
    hex.replace_range(3..4, "z");

    assert_eq!(
        BaseRevision::new(&hex),
        Err(BaseRevisionError::NotHexadecimal { position: 3 })
    );
}

// ---------------------------------------------------------------------
// Modes and targets
// ---------------------------------------------------------------------

#[test]
fn file_mode_keeps_every_published_kind_distinct() {
    assert_eq!(FileMode::from_octal("100644"), Ok(FileMode::Regular));
    assert_eq!(FileMode::from_octal("100755"), Ok(FileMode::Executable));
    assert_eq!(FileMode::from_octal("120000"), Ok(FileMode::SymbolicLink));
    assert_eq!(FileMode::from_octal("160000"), Ok(FileMode::Submodule));

    let other = FileMode::from_octal("040000").expect("six octal digits name one mode");
    assert!(matches!(other, FileMode::Unsupported(_)));
    assert_eq!(other.as_octal(), "040000");
    assert!(!other.stores_text());
    assert!(FileMode::Regular.stores_text());
    assert!(FileMode::Executable.stores_text());
    assert!(!FileMode::SymbolicLink.stores_text());
    assert!(!FileMode::Submodule.stores_text());
}

#[test]
fn file_mode_rejects_a_malformed_value() {
    assert_eq!(
        FileMode::from_octal("10064"),
        Err(FileModeError::Digits { actual: 5 })
    );
    assert_eq!(
        FileMode::from_octal("100648"),
        Err(FileModeError::NotOctal { position: 5 })
    );
}

#[test]
fn one_path_target_matches_either_rename_side() {
    let rename = DiffChange::Renamed {
        old: side("src/old.rs", FileMode::Regular),
        new: side("src/new.rs", FileMode::Regular),
    };

    assert!(DiffTarget::Worktree.selects(&rename));
    assert!(DiffTarget::Path(path("src/old.rs")).selects(&rename));
    assert!(DiffTarget::Path(path("src/new.rs")).selects(&rename));
    assert!(!DiffTarget::Path(path("src/other.rs")).selects(&rename));
}

#[test]
fn a_renamed_file_answers_under_both_paths() {
    let file = FileDiff::new(
        DiffChange::Renamed {
            old: side("src/old.rs", FileMode::Regular),
            new: side("src/new.rs", FileMode::Regular),
        },
        DiffContent::Binary,
    )
    .expect("a regular mode publishes binary content");
    let diff = candidate(vec![file.clone()]);

    assert_eq!(diff.file(&path("src/old.rs")), Some(&file));
    assert_eq!(diff.file(&path("src/new.rs")), Some(&file));
    assert_eq!(diff.file(&path("src/other.rs")), None);
}

// ---------------------------------------------------------------------
// Sides and content
// ---------------------------------------------------------------------

#[test]
fn a_modification_names_one_path_and_a_rename_names_two() {
    assert_eq!(
        FileDiff::new(
            DiffChange::Modified {
                old: side("a.rs", FileMode::Regular),
                new: side("b.rs", FileMode::Regular),
            },
            DiffContent::Binary,
        ),
        Err(FileDiffError::SidePathMismatch)
    );
    assert_eq!(
        FileDiff::new(
            DiffChange::Renamed {
                old: side("a.rs", FileMode::Regular),
                new: side("a.rs", FileMode::Executable),
            },
            DiffContent::Binary,
        ),
        Err(FileDiffError::RenameToSelf)
    );
}

#[test]
fn content_must_match_the_published_modes() {
    let text = DiffContent::Text(
        TextDiff::new(vec![added_hunk(&["a"])], DiffTruncation::Complete)
            .expect("one hunk needs no order"),
    );

    assert_eq!(
        FileDiff::new(
            DiffChange::Added {
                new: side("link", FileMode::SymbolicLink),
            },
            text,
        ),
        Err(FileDiffError::ContentModeMismatch)
    );
    assert_eq!(
        FileDiff::new(
            DiffChange::Added {
                new: side("a.rs", FileMode::Regular),
            },
            DiffContent::Submodule,
        ),
        Err(FileDiffError::ContentModeMismatch)
    );

    for (mode, content) in [
        (FileMode::SymbolicLink, DiffContent::SymbolicLink),
        (FileMode::Submodule, DiffContent::Submodule),
        (
            FileMode::from_octal("040000").expect("six octal digits name one mode"),
            DiffContent::Unsupported,
        ),
        (FileMode::Executable, DiffContent::Binary),
    ] {
        FileDiff::new(
            DiffChange::Added {
                new: side("x", mode),
            },
            content,
        )
        .expect("the published mode explains the content");
    }
}

// ---------------------------------------------------------------------
// Line mapping and final-line state
// ---------------------------------------------------------------------

#[test]
fn a_hunk_maps_every_line_onto_both_sides() {
    let hunk = Hunk::new(
        HunkId::new(0),
        OldLineRange::new(old_line(10), 3).expect("the fixture stays inside the bound"),
        NewLineRange::new(new_line(20), 3).expect("the fixture stays inside the bound"),
        vec![
            context_line(10, 20, "keep"),
            removed_line(11, "drop"),
            added_line(21, "insert"),
            context_line(12, 22, "tail"),
        ],
    )
    .expect("the lines realize both ranges");

    let old: Vec<Option<u32>> = hunk
        .side_lines(DiffSide::Old)
        .map(|line| line.number(DiffSide::Old))
        .collect();
    let new: Vec<Option<u32>> = hunk
        .side_lines(DiffSide::New)
        .map(|line| line.number(DiffSide::New))
        .collect();

    assert_eq!(old, vec![Some(10), Some(11), Some(12)]);
    assert_eq!(new, vec![Some(20), Some(21), Some(22)]);
    assert!(!hunk.lines()[1].appears_on(DiffSide::New));
    assert!(!hunk.lines()[2].appears_on(DiffSide::Old));
}

#[test]
fn a_hunk_rejects_a_gap_in_one_side() {
    let error = Hunk::new(
        HunkId::new(0),
        OldLineRange::new(old_line(1), 2).expect("the fixture stays inside the bound"),
        NewLineRange::new(new_line(1), 2).expect("the fixture stays inside the bound"),
        vec![context_line(1, 1, "a"), context_line(3, 2, "b")],
    );

    assert_eq!(
        error,
        Err(HunkError::LineMismatch {
            side: DiffSide::Old,
            expected: 2,
            actual: 3,
        })
    );
}

#[test]
fn a_hunk_rejects_lines_that_do_not_fill_a_range() {
    let error = Hunk::new(
        HunkId::new(0),
        OldLineRange::new(old_line(1), 2).expect("the fixture stays inside the bound"),
        NewLineRange::new(new_line(1), 1).expect("the fixture stays inside the bound"),
        vec![context_line(1, 1, "a")],
    );

    assert_eq!(
        error,
        Err(HunkError::CountMismatch {
            side: DiffSide::Old,
            expected: 2,
            actual: 1,
        })
    );
    assert_eq!(
        Hunk::new(
            HunkId::new(0),
            OldLineRange::new(old_line(1), 0).expect("an empty old range is valid"),
            NewLineRange::new(new_line(1), 0).expect("an empty new range is valid"),
            Vec::new(),
        ),
        Err(HunkError::Empty)
    );
}

#[test]
fn a_hunk_rejects_a_line_after_the_last_line_of_its_side() {
    let final_line = DiffLine::new(
        LineOrigin::Context {
            old: old_line(1),
            new: new_line(1),
        },
        line_text("a"),
        LineEnding::EndOfFile,
    );

    assert_eq!(
        Hunk::new(
            HunkId::new(0),
            OldLineRange::new(old_line(1), 2).expect("the fixture stays inside the bound"),
            NewLineRange::new(new_line(1), 2).expect("the fixture stays inside the bound"),
            vec![final_line, context_line(2, 2, "b")],
        ),
        Err(HunkError::LineAfterFinalLine {
            side: DiffSide::Old
        })
    );
}

#[test]
fn published_side_bytes_keep_the_final_line_state_of_each_side() {
    let hunk = Hunk::new(
        HunkId::new(0),
        OldLineRange::new(old_line(1), 2).expect("the fixture stays inside the bound"),
        NewLineRange::new(new_line(1), 2).expect("the fixture stays inside the bound"),
        vec![
            context_line(1, 1, "keep"),
            DiffLine::new(
                LineOrigin::Removed { old: old_line(2) },
                line_text("old"),
                LineEnding::EndOfFile,
            ),
            DiffLine::new(
                LineOrigin::Added { new: new_line(2) },
                line_text("new"),
                LineEnding::Newline,
            ),
        ],
    )
    .expect("the lines realize both ranges");
    let text =
        TextDiff::new(vec![hunk], DiffTruncation::Complete).expect("one hunk needs no order");

    // The old side ends without a line feed, the new side ends with one.
    assert_eq!(text.side_bytes(DiffSide::Old), b"keep\nold");
    assert_eq!(text.side_bytes(DiffSide::New), b"keep\nnew\n");
}

#[test]
fn hunks_rise_without_an_overlap() {
    let first = added_hunk(&["a"]);
    let second = Hunk::new(
        HunkId::new(1),
        OldLineRange::new(old_line(1), 0).expect("an empty old range is valid"),
        NewLineRange::new(new_line(1), 1).expect("the fixture stays inside the bound"),
        vec![added_line(1, "b")],
    )
    .expect("the lines realize both ranges");

    assert_eq!(
        TextDiff::new(vec![first.clone(), second], DiffTruncation::Complete),
        Err(TextDiffError::RangeOverlap {
            side: DiffSide::New,
            first: 1,
            previous_end: 2,
        })
    );
    assert_eq!(
        TextDiff::new(vec![first], DiffTruncation::Complete)
            .expect("one hunk needs no order")
            .hunk(HunkId::new(1)),
        None
    );
}

// ---------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------

#[test]
fn every_constructor_reports_its_own_limit() {
    assert_eq!(
        DiffLineText::new(vec![b'a'; DIFF_LINE_BYTES_MAX + 1]),
        Err(DiffLineTextError::Limit {
            actual: DIFF_LINE_BYTES_MAX + 1,
            max: DIFF_LINE_BYTES_MAX,
        })
    );
    assert_eq!(
        DiffLineText::new(b"a\nb".to_vec()),
        Err(DiffLineTextError::LineFeed { position: 1 })
    );
    assert_eq!(OldLine::new(0), Err(LineNumberError::Zero));
    assert_eq!(
        NewLine::new(DIFF_LINE_NUMBER_MAX + 1),
        Err(LineNumberError::Limit {
            actual: DIFF_LINE_NUMBER_MAX + 1,
            max: DIFF_LINE_NUMBER_MAX,
        })
    );
    let hunk_max = u32::try_from(DIFF_HUNK_LINES_MAX).expect("the bound fits one u32");
    assert_eq!(
        OldLineRange::new(old_line(1), hunk_max + 1),
        Err(LineRangeError::Limit {
            actual: hunk_max + 1,
            max: hunk_max,
        })
    );
    assert_eq!(
        AnchorContext::new(
            vec![line_text("a"); REVIEW_CONTEXT_LINES_MAX + 1],
            Vec::new()
        ),
        Err(AnchorContextError::Limit {
            actual: REVIEW_CONTEXT_LINES_MAX + 1,
            max: REVIEW_CONTEXT_LINES_MAX,
        })
    );
    assert_eq!(CommentBody::new("   \n\t "), Err(CommentBodyError::Empty));
    assert_eq!(
        CommentBody::new("c".repeat(REVIEW_COMMENT_BYTES_MAX + 1)),
        Err(CommentBodyError::Limit {
            actual: REVIEW_COMMENT_BYTES_MAX + 1,
            max: REVIEW_COMMENT_BYTES_MAX,
        })
    );
    assert_eq!(
        CommentBody::new("looks wrong")
            .expect("the fixture holds text")
            .as_str(),
        "looks wrong"
    );
}

#[test]
fn a_candidate_bounds_its_files_and_orders_its_paths() {
    let files: Vec<FileDiff> = (0..=DIFF_FILES_MAX)
        .map(|index| added_file(&format!("f{index:06}.rs"), &["a"]))
        .collect();

    assert_eq!(
        WorktreeDiff::new(
            old_side(),
            DiffTarget::Worktree,
            &authority(),
            files,
            DiffTruncation::Complete,
        ),
        Err(WorktreeDiffError::FilesLimit {
            actual: DIFF_FILES_MAX + 1,
            max: DIFF_FILES_MAX,
        })
    );

    let unordered = vec![added_file("b.rs", &["a"]), added_file("a.rs", &["a"])];
    assert!(matches!(
        WorktreeDiff::new(
            old_side(),
            DiffTarget::Worktree,
            &authority(),
            unordered,
            DiffTruncation::Complete,
        ),
        Err(WorktreeDiffError::PathOrder { .. })
    ));

    assert!(matches!(
        WorktreeDiff::new(
            old_side(),
            DiffTarget::Path(path("a.rs")),
            &authority(),
            vec![added_file("b.rs", &["a"])],
            DiffTruncation::Complete,
        ),
        Err(WorktreeDiffError::OutsideTarget { .. })
    ));
}

#[test]
fn truncation_stays_visible_on_every_level() {
    let text = TextDiff::new(
        vec![added_hunk(&["a"])],
        DiffTruncation::Truncated(DiffLimit::Lines),
    )
    .expect("one hunk needs no order");
    let file = FileDiff::new(
        DiffChange::Added {
            new: side("a.rs", FileMode::Regular),
        },
        DiffContent::Text(text),
    )
    .expect("a regular mode publishes text");
    let diff = WorktreeDiff::new(
        old_side(),
        DiffTarget::Worktree,
        &authority(),
        vec![file],
        DiffTruncation::Truncated(DiffLimit::Files),
    )
    .expect("one file needs no order");

    assert_eq!(
        diff.truncation(),
        DiffTruncation::Truncated(DiffLimit::Files)
    );
    assert!(diff.truncation().is_truncated());
    assert!(!DiffTruncation::Complete.is_truncated());
    let DiffContent::Text(published) = diff.files()[0].content() else {
        unreachable!("the fixture publishes text")
    };
    assert_eq!(
        published.truncation(),
        DiffTruncation::Truncated(DiffLimit::Lines)
    );
}

// ---------------------------------------------------------------------
// Revisions
// ---------------------------------------------------------------------

#[test]
fn the_revision_covers_the_complete_published_authority() {
    let files = vec![added_file("a.rs", &["one", "two"])];
    let reference = candidate(files.clone()).revision();

    let other_authority = WorktreeDiff::new(
        old_side(),
        DiffTarget::Worktree,
        &CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest([7; 32])),
        files.clone(),
        DiffTruncation::Complete,
    )
    .expect("one file needs no order");
    let other_index = WorktreeDiff::new(
        old_side(),
        DiffTarget::Worktree,
        &CandidateAuthority::new(
            HeadAuthority::Commit(base()),
            IndexAuthority::from_digest([9; 32]),
        ),
        files,
        DiffTruncation::Complete,
    )
    .expect("one file needs no order");
    let other_bytes = candidate(vec![added_file("a.rs", &["one", "three"])]);
    let other_path = candidate(vec![added_file("b.rs", &["one", "two"])]);
    let other_truncation = WorktreeDiff::new(
        old_side(),
        DiffTarget::Worktree,
        &authority(),
        vec![added_file("a.rs", &["one", "two"])],
        DiffTruncation::Truncated(DiffLimit::Files),
    )
    .expect("one file needs no order");

    for other in [
        other_authority,
        other_index,
        other_bytes,
        other_path,
        other_truncation,
    ] {
        assert_ne!(reference, other.revision());
    }
    assert_eq!(
        reference,
        candidate(vec![added_file("a.rs", &["one", "two"])]).revision()
    );
    assert_eq!(reference.to_hex().len(), DIGEST_BYTES * 2);
}

// ---------------------------------------------------------------------
// Review anchors and relocation
// ---------------------------------------------------------------------

fn anchor_on(diff: &WorktreeDiff, name: &str, first: u32, count: u32) -> ReviewAnchor {
    let location = AnchorLocation::New {
        range: NewLineRange::new(new_line(first), count)
            .expect("the fixture stays inside the bound"),
    };
    ReviewAnchor::select(diff, &path(name), HunkId::new(0), location)
        .expect("the fixture selects published lines")
}

#[test]
fn an_anchor_names_every_part_of_its_location() {
    let diff = candidate(vec![added_file("a.rs", &["one", "two", "three"])]);
    let anchor = anchor_on(&diff, "a.rs", 2, 1);

    assert_eq!(anchor.old_side(), old_side());
    assert_eq!(anchor.candidate(), diff.revision());
    assert_eq!(anchor.path(), &path("a.rs"));
    assert_eq!(anchor.hunk(), HunkId::new(0));
    assert_eq!(anchor.side(), DiffSide::New);
    assert_eq!(anchor.location().first(), 2);
    assert_eq!(anchor.location().count(), 1);
    assert_eq!(anchor.context().before(), [line_text("one")].as_slice());
    assert_eq!(anchor.context().after(), [line_text("three")].as_slice());
    assert_eq!(
        anchor.selection(),
        SelectionDigest::of(diff.files()[0..1].iter().flat_map(|file| {
            let DiffContent::Text(text) = file.content() else {
                unreachable!("the fixture publishes text")
            };
            text.hunks()[0].lines()[1..2].iter()
        }))
    );
}

#[test]
fn selection_rejects_a_place_that_the_candidate_does_not_publish() {
    let diff = candidate(vec![
        added_file("a.rs", &["one"]),
        FileDiff::new(
            DiffChange::Added {
                new: side("b.bin", FileMode::Regular),
            },
            DiffContent::Binary,
        )
        .expect("a regular mode publishes binary content"),
    ]);
    let location = AnchorLocation::New {
        range: NewLineRange::new(new_line(1), 1).expect("the fixture stays inside the bound"),
    };

    assert_eq!(
        ReviewAnchor::select(&diff, &path("missing.rs"), HunkId::new(0), location),
        Err(ReviewAnchorError::FileMissing)
    );
    assert_eq!(
        ReviewAnchor::select(&diff, &path("b.bin"), HunkId::new(0), location),
        Err(ReviewAnchorError::NoText)
    );
    assert_eq!(
        ReviewAnchor::select(&diff, &path("a.rs"), HunkId::new(1), location),
        Err(ReviewAnchorError::HunkMissing)
    );
    assert_eq!(
        ReviewAnchor::select(
            &diff,
            &path("a.rs"),
            HunkId::new(0),
            AnchorLocation::New {
                range: NewLineRange::new(new_line(1), 0).expect("an empty range is valid"),
            },
        ),
        Err(ReviewAnchorError::EmptySelection)
    );
    assert_eq!(
        ReviewAnchor::select(
            &diff,
            &path("a.rs"),
            HunkId::new(0),
            AnchorLocation::New {
                range: NewLineRange::new(new_line(5), 1)
                    .expect("the fixture stays inside the bound"),
            },
        ),
        Err(ReviewAnchorError::LinesMissing)
    );
    assert_eq!(
        ReviewAnchor::select(
            &diff,
            &path("a.rs"),
            HunkId::new(0),
            AnchorLocation::Old {
                range: OldLineRange::new(old_line(1), 1)
                    .expect("the fixture stays inside the bound"),
            },
        ),
        Err(ReviewAnchorError::LinesMissing)
    );
}

#[test]
fn an_unchanged_candidate_relocates_exactly() {
    let diff = candidate(vec![added_file("a.rs", &["one", "two", "three"])]);
    let anchor = anchor_on(&diff, "a.rs", 2, 1);

    let Relocation::Exact { anchor: found } = relocate(&anchor, &diff) else {
        panic!("the same candidate holds the selection at the same place")
    };
    assert_eq!(found, anchor);
}

#[test]
fn an_inserted_line_relocates_the_anchor_to_the_later_candidate() {
    let earlier = candidate(vec![added_file("a.rs", &["one", "two", "three"])]);
    let anchor = anchor_on(&earlier, "a.rs", 2, 1);
    let later = candidate(vec![added_file("a.rs", &["zero", "one", "two", "three"])]);

    let Relocation::Relocated { anchor: found } = relocate(&anchor, &later) else {
        panic!("the later candidate holds the selection one line below")
    };
    assert_eq!(found.location().first(), 3);
    assert_eq!(found.location().count(), 1);
    assert_eq!(found.candidate(), later.revision());
    assert_ne!(found.candidate(), anchor.candidate());
    assert_eq!(found.selection(), anchor.selection());
}

#[test]
fn a_removed_selection_is_missing() {
    let earlier = candidate(vec![added_file("a.rs", &["one", "two", "three"])]);
    let anchor = anchor_on(&earlier, "a.rs", 2, 1);

    assert_eq!(
        relocate(
            &anchor,
            &candidate(vec![added_file("a.rs", &["one", "three"])])
        ),
        Relocation::Missing
    );
    assert_eq!(
        relocate(&anchor, &candidate(vec![added_file("b.rs", &["two"])])),
        Relocation::Missing
    );
}

#[test]
fn duplicate_content_never_guesses_one_place() {
    let earlier = candidate(vec![added_file("a.rs", &["two", "x"])]);
    let anchor = anchor_on(&earlier, "a.rs", 1, 1);
    let later = candidate(vec![added_file("a.rs", &["two", "x", "two", "x"])]);

    assert_eq!(
        relocate(&anchor, &later),
        Relocation::Ambiguous(AmbiguityReason::MultipleMatches)
    );
}

#[test]
fn the_search_bound_reports_an_ambiguity_instead_of_a_place() {
    let lines: Vec<String> = (0..DIFF_HUNK_LINES_MAX)
        .map(|index| format!("line {index}"))
        .collect();
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let file = added_file("a.rs", &borrowed);
    let earlier = candidate(vec![file.clone()]);
    let anchor = anchor_on(&earlier, "a.rs", 1, 1);

    // Every hunk of the fixture holds the published maximum of windows, so
    // a candidate with enough hunks passes the relocation bound.
    let hunks_needed = RELOCATION_WINDOWS_MAX / DIFF_HUNK_LINES_MAX + 1;
    let mut hunks = Vec::with_capacity(hunks_needed);
    for position in 0..hunks_needed {
        let offset = u32::try_from(position * DIFF_HUNK_LINES_MAX)
            .expect("the fixture stays inside the bound");
        let id = u32::try_from(position).expect("the fixture holds few hunks");
        let lines = (0..DIFF_HUNK_LINES_MAX)
            .map(|index| {
                let number =
                    offset + u32::try_from(index).expect("the fixture holds few lines") + 1;
                added_line(number, "filler")
            })
            .collect();
        hunks.push(
            Hunk::new(
                HunkId::new(id),
                OldLineRange::new(old_line(1), 0).expect("an empty old range is valid"),
                NewLineRange::new(
                    new_line(offset + 1),
                    u32::try_from(DIFF_HUNK_LINES_MAX).expect("the bound fits one u32"),
                )
                .expect("the fixture stays inside the bound"),
                lines,
            )
            .expect("the lines realize both ranges"),
        );
    }
    let wide = candidate(vec![
        FileDiff::new(
            DiffChange::Added {
                new: side("a.rs", FileMode::Regular),
            },
            DiffContent::Text(
                TextDiff::new(hunks, DiffTruncation::Complete)
                    .expect("the fixture hunks rise without an overlap"),
            ),
        )
        .expect("a regular mode publishes text"),
    ]);

    assert_eq!(
        relocate(&anchor, &wide),
        Relocation::Ambiguous(AmbiguityReason::SearchLimit)
    );
}

#[test]
fn an_old_side_anchor_relocates_on_the_old_side() {
    let hunk = Hunk::new(
        HunkId::new(0),
        OldLineRange::new(old_line(1), 3).expect("the fixture stays inside the bound"),
        NewLineRange::new(new_line(1), 1).expect("the fixture stays inside the bound"),
        vec![
            context_line(1, 1, "keep"),
            removed_line(2, "gone"),
            removed_line(3, "also gone"),
        ],
    )
    .expect("the lines realize both ranges");
    let file = FileDiff::new(
        DiffChange::Modified {
            old: side("a.rs", FileMode::Regular),
            new: side("a.rs", FileMode::Regular),
        },
        DiffContent::Text(
            TextDiff::new(vec![hunk], DiffTruncation::Complete).expect("one hunk needs no order"),
        ),
    )
    .expect("a regular mode publishes text");
    let diff = candidate(vec![file]);

    let anchor = ReviewAnchor::select(
        &diff,
        &path("a.rs"),
        HunkId::new(0),
        AnchorLocation::Old {
            range: OldLineRange::new(old_line(2), 2).expect("the fixture stays inside the bound"),
        },
    )
    .expect("the old side publishes both lines");

    assert_eq!(anchor.side(), DiffSide::Old);
    assert_eq!(anchor.context().before(), [line_text("keep")].as_slice());
    assert!(anchor.context().after().is_empty());
    assert!(matches!(relocate(&anchor, &diff), Relocation::Exact { .. }));
}
