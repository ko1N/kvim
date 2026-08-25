use std::path::Path;
use std::sync::Arc;

use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{
    ProcessOutput, ProcessRequest, PublicationGate, RequestSlot, Runtime, RuntimeLimits,
    SubmitError,
};

use crate::diff::{
    DiffChange, DiffComparison, DiffContent, DiffOldSide, DiffSide, DiffTarget, FileMode,
    LineOrigin, WorktreeDiff,
};
use crate::temp::TempRepository;

use super::{
    BaseRevision, DIFF_CAPTURE_ATTEMPTS_MAX, RawStatus, Section, WorktreeDiffFailure,
    WorktreeDiffRead, WorktreeDiffRequest, collect_files, parse_raw_records, split_sections,
};

/// The number of commands that one complete pass of the capture runs.
const PASS_COMMANDS: usize = 6;

/// The largest number of commands that one exhausted capture runs.
///
/// The base check runs once. The first attempt adds one authority pass, and
/// every attempt adds one collection pass and one authority pass.
const CAPTURE_COMMANDS_MAX: usize =
    1 + PASS_COMMANDS + DIFF_CAPTURE_ATTEMPTS_MAX * 2 * PASS_COMMANDS;

/// The identifier that names no object of any fixture repository.
const ABSENT_BASE: &str = "0123456789abcdef0123456789abcdef01234567";

/// Runs one bounded command through the process service of the editor.
async fn run(command: ProcessRequest) -> ProcessOutput {
    let limits = RuntimeLimits::new(1, 1, 1).expect("every capacity is nonzero");
    let (runtime, mut events) = Runtime::<ProcessOutput>::with_limits(limits);
    let handle =
        PublicationGate::default().begin(RequestSlot::new(1), &runtime.cancellation_root());
    let submitted: Result<(), SubmitError> =
        runtime.submit_process(handle, command, |output| output);
    submitted.expect("the isolated runtime holds one free permit");
    let event = events
        .recv()
        .await
        .expect("every accepted request produces one result");
    let output = event
        .result
        .expect("the development shell and the build sandbox both provide git");
    runtime.shutdown().await;
    output
}

/// Captures one worktree diff through the bounded process service.
fn capture(
    root: &Path,
    base: &str,
    target: DiffTarget,
) -> Result<WorktreeDiff, WorktreeDiffFailure> {
    capture_with(root, base, target, |_| {})
}

/// Captures one named comparison through the bounded process service.
fn capture_comparison(
    root: &Path,
    comparison: DiffComparison,
    target: DiffTarget,
) -> Result<WorktreeDiff, WorktreeDiffFailure> {
    let root = Arc::new(WorktreeRoot::open(root).expect("the fixture root is one directory"));
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the test host starts one Tokio runtime");
    tokio.block_on(async move {
        let mut request = WorktreeDiffRequest::new(root, comparison, target);
        for _ in 0..CAPTURE_COMMANDS_MAX {
            let output = run(request.command()).await;
            match request.publish(&output)? {
                WorktreeDiffRead::Pending(next) => request = *next,
                WorktreeDiffRead::Published(diff) => return Ok(*diff),
            }
        }
        panic!("one capture finishes inside its command bound")
    })
}

/// Returns the revision of one fixture commit.
fn revision(hex: &str) -> BaseRevision {
    BaseRevision::new(hex).expect("the fixture names one full identifier")
}

/// Captures one worktree diff and lets the caller change the repository.
///
/// The callback runs before every command, so a test can place one change
/// exactly between two passes of the capture.
fn capture_with<F>(
    root: &Path,
    base: &str,
    target: DiffTarget,
    mut before: F,
) -> Result<WorktreeDiff, WorktreeDiffFailure>
where
    F: FnMut(usize),
{
    let root = Arc::new(WorktreeRoot::open(root).expect("the fixture root is one directory"));
    let base = BaseRevision::new(base).expect("the fixture names one full identifier");
    // Every fixture below compares the base commit against the worktree, which
    // is the comparison that the capture published before it named its pair.
    let comparison = DiffComparison::CommitToWorktree(base);
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the test host starts one Tokio runtime");
    tokio.block_on(async move {
        let mut request = WorktreeDiffRequest::new(root, comparison, target);
        for step in 0..CAPTURE_COMMANDS_MAX {
            before(step);
            let output = run(request.command()).await;
            match request.publish(&output)? {
                WorktreeDiffRead::Pending(next) => request = *next,
                WorktreeDiffRead::Published(diff) => return Ok(*diff),
            }
        }
        unreachable!("one capture publishes or fails inside {CAPTURE_COMMANDS_MAX} commands");
    })
}

/// The first command of one pass, counted from the base check.
const fn pass_start(pass: usize) -> usize {
    1 + pass * PASS_COMMANDS
}

fn path(value: &str) -> WorktreeRelativePath {
    WorktreeRelativePath::new(value).expect("the fixture names one contained path")
}

/// Returns the exact new-side bytes that one published file holds.
fn new_bytes(diff: &WorktreeDiff, name: &str) -> Vec<u8> {
    let file = diff
        .file(&path(name))
        .unwrap_or_else(|| panic!("the candidate holds {name}"));
    match file.content() {
        DiffContent::Text(text) => text.side_bytes(DiffSide::New),
        other => panic!("{name} publishes {other:?} instead of text"),
    }
}

#[test]
fn publishes_commits_after_the_base_and_dirty_changes() {
    let repository = TempRepository::new("diff-committed-and-dirty");
    repository.file("committed.txt", "one\ntwo\n");
    repository.file("dirty.txt", "three\nfour\n");
    repository.commit("base");
    let base = repository.head();

    repository.file("committed.txt", "one\nTWO\n");
    repository.commit("after the base");
    repository.file("dirty.txt", "three\nFOUR\n");

    let diff = capture(repository.path(), &base, DiffTarget::Worktree)
        .expect("the fixture holds one reachable base");

    assert_eq!(diff.files().len(), 2);
    assert_eq!(new_bytes(&diff, "committed.txt"), b"one\nTWO\n");
    assert_eq!(new_bytes(&diff, "dirty.txt"), b"three\nFOUR\n");
}

#[test]
fn publishes_a_clean_commit_after_the_base() {
    let repository = TempRepository::new("diff-clean-commit");
    repository.file("only.txt", "first\n");
    repository.commit("base");
    let base = repository.head();

    repository.file("only.txt", "second\n");
    repository.commit("after the base");

    let diff = capture(repository.path(), &base, DiffTarget::Worktree)
        .expect("a clean worktree above the base stays reviewable");

    assert_eq!(diff.files().len(), 1);
    assert_eq!(new_bytes(&diff, "only.txt"), b"second\n");
}

#[test]
fn publishes_a_rename_pair_under_either_name() {
    let repository = TempRepository::new("diff-rename");
    repository.file("old.txt", "stable content of the renamed file\n");
    repository.commit("base");
    let base = repository.head();

    repository.git(&["mv", "old.txt", "new.txt"]);
    repository.commit("rename");

    let diff = capture(repository.path(), &base, DiffTarget::Path(path("old.txt")))
        .expect("the one-path target names the old side of the rename");

    assert_eq!(diff.files().len(), 1);
    let file = diff.files().first().expect("the candidate holds the pair");
    let DiffChange::Renamed { old, new } = file.change() else {
        panic!(
            "the capture published {:?} instead of a rename",
            file.change()
        );
    };
    assert_eq!(old.path(), &path("old.txt"));
    assert_eq!(new.path(), &path("new.txt"));
    assert!(diff.file(&path("new.txt")).is_some());
}

#[test]
fn publishes_the_exact_content_of_an_untracked_file() {
    let repository = TempRepository::new("diff-untracked");
    repository.file("tracked.txt", "tracked\n");
    repository.commit("base");
    let base = repository.head();

    repository.file("fresh.txt", "alpha\nbeta");

    let diff = capture(repository.path(), &base, DiffTarget::Worktree)
        .expect("an untracked file needs no commit");

    assert_eq!(diff.files().len(), 1);
    // The file holds no final line feed, and the candidate keeps that.
    assert_eq!(new_bytes(&diff, "fresh.txt"), b"alpha\nbeta");
    let file = diff
        .file(&path("fresh.txt"))
        .expect("the path is published");
    assert!(matches!(file.change(), DiffChange::Added { .. }));
}

#[test]
fn rejects_a_base_that_names_no_commit() {
    let repository = TempRepository::new("diff-absent-base");
    repository.file("only.txt", "one\n");
    repository.commit("base");
    let tree = repository.head();

    assert_eq!(
        capture(repository.path(), ABSENT_BASE, DiffTarget::Worktree),
        Err(WorktreeDiffFailure::BaseUnavailable)
    );
    // The identifier of one commit stays available, so the fixture proves
    // that only the absent object is refused.
    assert!(capture(repository.path(), &tree, DiffTarget::Worktree).is_ok());
}

#[test]
fn records_every_file_side_without_guessing() {
    let repository = TempRepository::new("diff-sides");
    repository.file("modified.txt", "one\n");
    repository.file("deleted.txt", "gone\n");
    repository.file("link.bin", "\u{0}\u{1}\u{2}before\n");
    repository.commit("base");
    let base = repository.head();

    repository.file("modified.txt", "ONE\n");
    std::fs::remove_file(repository.join("deleted.txt")).expect("the fixture holds the file");
    repository.file("link.bin", "\u{0}\u{1}\u{2}after\n");
    repository.file("added.txt", "added\n");
    repository.git(&["add", "added.txt"]);
    std::os::unix::fs::symlink("modified.txt", repository.join("link.txt"))
        .expect("the fixture host supports one symbolic link");

    let diff = capture(repository.path(), &base, DiffTarget::Worktree)
        .expect("the fixture holds one reachable base");

    let kinds: Vec<(&str, &DiffContent)> = diff
        .files()
        .iter()
        .map(|file| {
            (
                file.path().as_path().to_str().expect("the fixture is text"),
                file.content(),
            )
        })
        .collect();
    assert_eq!(kinds.len(), 5, "the candidate holds {kinds:?}");
    assert!(matches!(
        diff.file(&path("added.txt")).map(FileDiffKind::of),
        Some(FileDiffKind::Added)
    ));
    assert!(matches!(
        diff.file(&path("deleted.txt")).map(FileDiffKind::of),
        Some(FileDiffKind::Deleted)
    ));
    assert!(matches!(
        diff.file(&path("link.bin")).map(|file| file.content()),
        Some(DiffContent::Binary)
    ));
    assert!(matches!(
        diff.file(&path("link.txt")).map(|file| file.content()),
        Some(DiffContent::SymbolicLink)
    ));
    assert_eq!(new_bytes(&diff, "modified.txt"), b"ONE\n");
}

/// The kind of one published change, for a test that names it directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileDiffKind {
    Added,
    Deleted,
    Modified,
    Renamed,
}

impl FileDiffKind {
    fn of(file: &crate::diff::FileDiff) -> Self {
        match file.change() {
            DiffChange::Added { .. } => Self::Added,
            DiffChange::Deleted { .. } => Self::Deleted,
            DiffChange::Modified { .. } => Self::Modified,
            DiffChange::Renamed { .. } => Self::Renamed,
        }
    }
}

#[test]
fn refuses_a_repository_that_changes_through_every_attempt() {
    let repository = TempRepository::new("diff-changing");
    repository.file("busy.txt", "0\n");
    repository.commit("base");
    let base = repository.head();

    let outcome = capture_with(repository.path(), &base, DiffTarget::Worktree, |step| {
        repository.file("busy.txt", &format!("{step}\n"));
    });

    assert_eq!(outcome, Err(WorktreeDiffFailure::ChangedDuringCapture));
}

#[test]
fn rejects_a_candidate_of_a_file_that_returns_to_its_first_content() {
    let repository = TempRepository::new("diff-file-race");
    repository.file("race.txt", "A\n");
    repository.commit("base commit");
    // The base holds the original content, so every state below is a change.
    repository.file("race.txt", "first\n");
    let base = repository.head();

    let diff = capture_with(repository.path(), &base, DiffTarget::Worktree, |step| {
        // The collection pass reads B, and both authority passes read A.
        if step == pass_start(1) {
            repository.file("race.txt", "second\n");
        }
        if step == pass_start(2) {
            repository.file("race.txt", "first\n");
        }
    })
    .expect("the retry collects one consistent candidate");

    assert_eq!(new_bytes(&diff, "race.txt"), b"first\n");
}

#[test]
fn rejects_a_candidate_of_an_index_that_returns_to_its_first_state() {
    let repository = TempRepository::new("diff-index-race");
    repository.file("tracked.txt", "one\n");
    repository.commit("base");
    let base = repository.head();
    repository.file("staged.txt", "staged content\n");

    let diff = capture_with(repository.path(), &base, DiffTarget::Worktree, |step| {
        // The worktree never changes. Only the index leaves and returns to
        // its first state, so only the index digest rejects the candidate.
        if step == pass_start(1) {
            repository.git(&["add", "staged.txt"]);
        }
        if step == pass_start(2) {
            repository.git(&["rm", "--cached", "staged.txt"]);
        }
    })
    .expect("the retry collects one consistent candidate");

    // The worktree content proves nothing here, because it never changed.
    // The revision covers the index authority, so only a candidate that the
    // capture collected against the settled index carries this identity.
    let settled = capture(repository.path(), &base, DiffTarget::Worktree)
        .expect("the settled repository captures once more");
    assert_eq!(new_bytes(&diff, "staged.txt"), b"staged content\n");
    assert_eq!(diff.revision(), settled.revision());
}

#[test]
fn starts_no_program_that_the_repository_names() {
    let repository = TempRepository::new("diff-hostile");
    let marker = repository.join("marker");
    let program = repository.join("hostile.sh");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\ntouch {}\nexit 0\n",
            marker.to_str().expect("the fixture path is text")
        ),
    )
    .expect("the fixture writes one script");
    set_executable(&program);
    let named = program.to_str().expect("the fixture path is text");

    repository.file("source.txt", "one\n");
    repository.file(".gitattributes", "* diff=hostile\n");
    repository.commit("base");
    let base = repository.head();
    repository.file("source.txt", "two\n");

    for (name, value) in [
        ("core.askPass", named),
        ("core.editor", named),
        ("core.fsmonitor", named),
        ("core.pager", named),
        ("credential.helper", named),
        ("diff.external", named),
        ("diff.hostile.textconv", named),
    ] {
        repository.git(&["config", name, value]);
    }
    for hook in ["post-index-change", "pre-auto-gc", "reference-transaction"] {
        let installed = repository.join(&format!(".git/hooks/{hook}"));
        std::fs::copy(&program, &installed).expect("the fixture installs one hook");
        set_executable(&installed);
    }

    let diff = capture(repository.path(), &base, DiffTarget::Worktree)
        .expect("the policy keeps the read usable");

    assert_eq!(new_bytes(&diff, "source.txt"), b"two\n");
    assert!(
        !marker.exists(),
        "the policy must start no program that the repository names"
    );
}

/// Gives one fixture script the permission that Git needs to start it.
fn set_executable(program: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let mut permissions = std::fs::metadata(program)
        .expect("the fixture wrote the script")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(program, permissions).expect("the fixture owns the script");
}

// -----------------------------------------------------------------------
// The pure listing and patch parsers
// -----------------------------------------------------------------------

fn sections(patch: &[u8]) -> Vec<Section<'_>> {
    split_sections(patch).expect("the fixture holds one well-formed patch")
}

#[test]
fn publishes_a_submodule_side_without_lines() {
    let listing = b":160000 160000 1111111 2222222 M\0nested\0".as_slice();
    let patch = b"diff --git a/nested b/nested\nindex 1111111..2222222 160000\n--- a/nested\n+++ b/nested\n@@ -1 +1 @@\n-Subproject commit 1111111\n+Subproject commit 2222222\n";
    let records = parse_raw_records(listing).expect("the listing is well formed");

    let files = collect_files(&records, &sections(patch)).expect("every section is owned");

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].content(), &DiffContent::Submodule);
    assert_eq!(
        files[0].change().new_side().map(|side| side.mode()),
        Some(FileMode::Submodule)
    );
}

#[test]
fn publishes_an_unmerged_side_as_unsupported() {
    // Git names no mode for an unresolved entry, so neither side stores
    // reviewable content.
    let listing = b":000000 000000 0000000 0000000 U\0conflict.txt\0".as_slice();
    let patch = b"diff --git a/conflict.txt b/conflict.txt\n";
    let records = parse_raw_records(listing).expect("the listing is well formed");
    assert_eq!(records[0].status, RawStatus::Other);

    let files = collect_files(&records, &sections(patch)).expect("every section is owned");

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].content(), &DiffContent::Unsupported);
}

#[test]
fn owns_two_sections_for_one_type_change() {
    let listing = b":100644 120000 1111111 2222222 T\0target.txt\0".as_slice();
    let patch = b"diff --git a/target.txt b/target.txt\ndeleted file mode 100644\n--- a/target.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-plain\ndiff --git a/target.txt b/target.txt\nnew file mode 120000\n--- /dev/null\n+++ b/target.txt\n@@ -0,0 +1 @@\n+other.txt\n\\ No newline at end of file\n";
    let records = parse_raw_records(listing).expect("the listing is well formed");
    assert_eq!(sections(patch).len(), 2);

    let files = collect_files(&records, &sections(patch)).expect("every section is owned");

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].content(), &DiffContent::SymbolicLink);
}

#[test]
fn refuses_a_listing_that_leaves_one_section_unowned() {
    let listing = b":100644 100644 1111111 2222222 M\0one.txt\0".as_slice();
    let patch = b"diff --git a/one.txt b/one.txt\n--- a/one.txt\n+++ b/one.txt\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/two.txt b/two.txt\n--- a/two.txt\n+++ b/two.txt\n@@ -1 +1 @@\n-c\n+d\n";
    let records = parse_raw_records(listing).expect("the listing is well formed");

    assert!(collect_files(&records, &sections(patch)).is_none());
}

#[test]
fn keeps_a_final_line_that_holds_no_line_feed() {
    let listing = b":100644 100644 1111111 2222222 M\0tail.txt\0".as_slice();
    let patch = b"diff --git a/tail.txt b/tail.txt\n--- a/tail.txt\n+++ b/tail.txt\n@@ -1,2 +1,2 @@\n keep\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n";
    let records = parse_raw_records(listing).expect("the listing is well formed");

    let files = collect_files(&records, &sections(patch)).expect("every section is owned");

    let DiffContent::Text(text) = files[0].content() else {
        panic!("a regular mode publishes text");
    };
    assert_eq!(text.side_bytes(DiffSide::Old), b"keep\nold");
    assert_eq!(text.side_bytes(DiffSide::New), b"keep\nnew");
    let lines = text.hunks()[0].lines();
    assert!(matches!(lines[0].origin(), LineOrigin::Context { .. }));
}

#[test]
fn publishes_the_staged_half_alone() {
    // The staged half compares the commit against the index, so the later
    // unstaged edit of the same file never reaches it.
    let repository = TempRepository::new("diff-staged-half");
    repository.file("notes.txt", "one\n");
    repository.commit("base");
    let base = repository.head();

    repository.file("notes.txt", "one\nstaged\n");
    repository.git(&["add", "notes.txt"]);
    repository.file("notes.txt", "one\nstaged\nunstaged\n");

    let diff = capture_comparison(
        repository.path(),
        DiffComparison::CommitToIndex(revision(&base)),
        DiffTarget::Worktree,
    )
    .expect("the fixture holds one reachable base");

    assert_eq!(diff.files().len(), 1);
    assert_eq!(new_bytes(&diff, "notes.txt"), b"one\nstaged\n");
    assert_eq!(diff.old_side(), DiffOldSide::Commit(revision(&base)));
}

#[test]
fn publishes_the_unstaged_half_and_names_the_index() {
    // The unstaged half compares the index against the worktree, so it holds
    // the later edit alone. The index is no commit, so the candidate names the
    // index digest instead of a revision.
    let repository = TempRepository::new("diff-unstaged-half");
    repository.file("notes.txt", "one\n");
    repository.commit("base");

    repository.file("notes.txt", "one\nstaged\n");
    repository.git(&["add", "notes.txt"]);
    repository.file("notes.txt", "one\nstaged\nunstaged\n");

    let diff = capture_comparison(
        repository.path(),
        DiffComparison::IndexToWorktree,
        DiffTarget::Worktree,
    )
    .expect("the unstaged half names no commit and proves none");

    assert_eq!(diff.files().len(), 1);
    assert_eq!(new_bytes(&diff, "notes.txt"), b"one\nstaged\nunstaged\n");
    assert!(diff.old_side().commit().is_none());
    assert!(matches!(diff.old_side(), DiffOldSide::Index(_)));
}

#[test]
fn publishes_one_commit_against_another_and_ignores_the_worktree() {
    // A commit pair is immutable, so a dirty worktree changes nothing about
    // what the capture publishes.
    let repository = TempRepository::new("diff-commit-pair");
    repository.file("notes.txt", "one\n");
    repository.commit("base");
    let base = repository.head();

    repository.file("notes.txt", "one\ntwo\n");
    repository.commit("second");
    let second = repository.head();

    repository.file("notes.txt", "one\ntwo\nthree\n");

    let diff = capture_comparison(
        repository.path(),
        DiffComparison::CommitToCommit {
            old: revision(&base),
            new: revision(&second),
        },
        DiffTarget::Worktree,
    )
    .expect("the fixture holds both commits");

    assert_eq!(diff.files().len(), 1);
    assert_eq!(new_bytes(&diff, "notes.txt"), b"one\ntwo\n");
    assert_eq!(diff.old_side(), DiffOldSide::Commit(revision(&base)));
}

#[test]
fn the_staged_half_resolves_its_own_head() {
    // The neogit screen compares `HEAD` against the index, and Git resolves
    // `HEAD` itself, so the caller names no revision.
    let repository = TempRepository::new("diff-head-to-index");
    repository.file("notes.txt", "one\n");
    repository.commit("base");
    let head = repository.head();

    repository.file("notes.txt", "one\nstaged\n");
    repository.git(&["add", "notes.txt"]);
    repository.file("notes.txt", "one\nstaged\nunstaged\n");

    let diff = capture_comparison(
        repository.path(),
        DiffComparison::HeadToIndex,
        DiffTarget::Worktree,
    )
    .expect("the fixture holds one commit");

    assert_eq!(new_bytes(&diff, "notes.txt"), b"one\nstaged\n");
    assert_eq!(diff.old_side(), DiffOldSide::Commit(revision(&head)));
}

#[test]
fn a_repository_without_a_commit_publishes_no_staged_half() {
    // An unborn `HEAD` names no commit, so the staged half has nothing to
    // compare against and answers the typed outcome instead of guessing.
    let repository = TempRepository::new("diff-head-unborn");
    repository.file("notes.txt", "one\n");
    repository.git(&["add", "notes.txt"]);

    let failure = capture_comparison(
        repository.path(),
        DiffComparison::HeadToIndex,
        DiffTarget::Worktree,
    )
    .expect_err("an unborn head compares against no commit");

    assert_eq!(failure, WorktreeDiffFailure::BaseUnavailable);
}
