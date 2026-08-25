//! Unit tests for the changed files panel.

use super::*;

use kvim_workspace::{
    BaseRevision, CandidateAuthority, DiffLimit, DiffLine, DiffLineText, DiffOldSide, DiffTarget,
    DiffTruncation, FileMode, FileSide, HeadAuthority, Hunk, HunkId, IndexAuthority, LineEnding,
    NewLine, NewLineRange, OldLine, OldLineRange, TextDiff, WorktreeDiff,
};

const BASE_HEX: &str = "0123456789abcdef0123456789abcdef01234567";

fn path(value: &str) -> WorktreeRelativePath {
    WorktreeRelativePath::new(value).expect("the fixture names one contained path")
}

/// Builds one added file whose new side holds the given lines.
fn added(name: &str, lines: &[&str], truncation: DiffTruncation) -> FileDiff {
    let body: Vec<DiffLine> = lines
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let number = u32::try_from(index).expect("the fixture is short") + 1;
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
        NewLineRange::new(NewLine::new(1).expect("one is one line number"), count)
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

fn review(files: Vec<FileDiff>, index: [u8; 32]) -> ReviewState {
    let authority =
        CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest(index));
    let candidate = WorktreeDiff::new(
        DiffOldSide::Commit(
            BaseRevision::new(BASE_HEX).expect("the fixture names one full identifier"),
        ),
        DiffTarget::Worktree,
        &authority,
        files,
        DiffTruncation::Complete,
    )
    .expect("the fixture candidate is usable");
    ReviewState::new(candidate)
}

#[test]
fn one_entry_counts_the_lines_of_its_own_file() {
    let review = review(
        vec![added(
            "a.txt",
            &["one", "two", "three"],
            DiffTruncation::Complete,
        )],
        [1; 32],
    );
    let entries = entries(&review);

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].added, 3);
    assert_eq!(entries[0].removed, 0);
    assert_eq!(entries[0].mark, 'A');
    assert!(entries[0].label().contains("+3 -0"));
}

#[test]
fn a_file_reads_as_complete_only_after_every_hunk_is_read() {
    let mut review = review(
        vec![added("a.txt", &["one"], DiffTruncation::Complete)],
        [2; 32],
    );

    assert_eq!(entries(&review)[0].unread, 1);
    assert!(!entries(&review)[0].is_complete());

    assert!(review.mark_read());
    assert_eq!(entries(&review)[0].unread, 0);
    assert!(entries(&review)[0].is_complete());
}

#[test]
fn a_truncated_file_never_reads_as_complete() {
    // The candidate holds content that the reader cannot reach, so the row
    // states the bound instead of claiming that the file is finished.
    let mut review = review(
        vec![added(
            "a.txt",
            &["one"],
            DiffTruncation::Truncated(DiffLimit::Hunks),
        )],
        [3; 32],
    );
    assert!(review.mark_read());

    let entries = entries(&review);
    assert_eq!(entries[0].unread, 0);
    assert!(entries[0].truncated);
    assert!(!entries[0].is_complete());
    assert!(entries[0].label().contains('…'));
}

#[test]
fn the_two_sections_hold_their_own_candidates_and_never_merge() {
    let staged = review(
        vec![added("staged.txt", &["one"], DiffTruncation::Complete)],
        [4; 32],
    );
    let unstaged = review(
        vec![added("unstaged.txt", &["two"], DiffTruncation::Complete)],
        [5; 32],
    );

    // The tab strip names the section, so the panel lists one section alone.
    let published = rows(ChangeSection::Staged, Some(&staged));
    let named: Vec<&ChangesRow> = published.iter().map(SidebarRow::id).collect();
    assert_eq!(
        named,
        vec![&ChangesRow::File {
            section: ChangeSection::Staged,
            path: path("staged.txt"),
            depth: 0,
        }]
    );

    let published = rows(ChangeSection::Unstaged, Some(&unstaged));
    let named: Vec<&ChangesRow> = published.iter().map(SidebarRow::id).collect();
    assert_eq!(
        named,
        vec![&ChangesRow::File {
            section: ChangeSection::Unstaged,
            path: path("unstaged.txt"),
            depth: 0,
        }]
    );
}

#[test]
fn a_section_without_a_change_publishes_no_row() {
    let empty = review(Vec::new(), [7; 32]);
    assert!(rows(ChangeSection::Staged, Some(&empty)).is_empty());
    assert!(rows(ChangeSection::Staged, None).is_empty());
}

#[test]
fn a_file_row_takes_the_selection() {
    let staged = review(
        vec![added("a.txt", &["one"], DiffTruncation::Complete)],
        [8; 32],
    );
    let published = rows(ChangeSection::Staged, Some(&staged));

    assert_eq!(published[0].kind(), RowKind::Selectable);
}

#[test]
fn a_refresh_installs_the_rows_of_one_section_into_one_sidebar() {
    let staged = review(
        vec![added("staged.txt", &["one"], DiffTruncation::Complete)],
        [9; 32],
    );
    let unstaged = review(
        vec![added("unstaged.txt", &["two"], DiffTruncation::Complete)],
        [10; 32],
    );
    let mut sidebar: SidebarState<ChangesRow> = SidebarState::new(20);

    refresh(&mut sidebar, ChangeSection::Staged, Some(&staged));

    assert_eq!(sidebar.rows().len(), 1);
    assert_eq!(
        sidebar.rows()[0].id(),
        &ChangesRow::File {
            section: ChangeSection::Staged,
            path: path("staged.txt"),
            depth: 0,
        }
    );

    // A later refresh replaces the rows instead of adding to them.
    refresh(&mut sidebar, ChangeSection::Unstaged, Some(&unstaged));
    assert_eq!(sidebar.rows().len(), 1);
    assert_eq!(
        sidebar.rows()[0].id(),
        &ChangesRow::File {
            section: ChangeSection::Unstaged,
            path: path("unstaged.txt"),
            depth: 0,
        }
    );
}

#[test]
fn the_panel_groups_the_files_by_directory() {
    // The panel reads like the file tree, so a nested file sits below its
    // directories and each directory opens once.
    let review = review(
        vec![
            // The candidate publishes its files in path order.
            added("notes.md", &["three"], DiffTruncation::Complete),
            added("src/main.rs", &["one"], DiffTruncation::Complete),
            added("src/parser/mod.rs", &["two"], DiffTruncation::Complete),
        ],
        [30; 32],
    );
    let published = rows(ChangeSection::Unstaged, Some(&review));

    let shape: Vec<String> = published
        .iter()
        .map(|row| match row.id() {
            ChangesRow::Directory { path, depth, .. } => {
                format!("d{depth} {}", path.display())
            }
            ChangesRow::File { path, depth, .. } => {
                format!("f{depth} {}", path.as_path().display())
            }
        })
        .collect();

    assert_eq!(
        shape,
        vec![
            "f0 notes.md".to_owned(),
            "d0 src".to_owned(),
            "f1 src/main.rs".to_owned(),
            "d1 src/parser".to_owned(),
            "f2 src/parser/mod.rs".to_owned(),
        ]
    );
}

#[test]
fn a_directory_row_takes_no_selection() {
    let review = review(
        vec![added("src/main.rs", &["one"], DiffTruncation::Complete)],
        [31; 32],
    );
    let published = rows(ChangeSection::Unstaged, Some(&review));

    let directory = published
        .iter()
        .find(|row| matches!(row.id(), ChangesRow::Directory { .. }))
        .expect("the panel published one directory row");
    assert_eq!(directory.kind(), RowKind::Inert);
}

#[test]
fn one_row_names_the_file_and_not_its_whole_path() {
    // The directory rows above the file carry the rest of the path.
    let review = review(
        vec![added(
            "src/parser/mod.rs",
            &["one"],
            DiffTruncation::Complete,
        )],
        [32; 32],
    );
    let label = entries(&review)[0].label();

    assert!(
        label.starts_with("mod.rs"),
        "the row names the file: {label}"
    );
    assert!(
        !label.contains("src/"),
        "the row repeats no directory: {label}"
    );
}
