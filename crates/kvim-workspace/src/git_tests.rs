use std::path::Path;
use std::sync::Arc;

use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{
    ProcessOutput, ProcessRequest, PublicationGate, RequestSlot, Runtime, RuntimeLimits,
    SubmitError,
};

use crate::temp::{TempDir, TempRepository};

use super::{
    GIT_STATUS_DEADLINE, GIT_STATUS_ENTRIES_MAX, GIT_STATUS_OUTPUT_BYTES_MAX, GitStatus,
    GitStatusFailure, GitStatusRead, GitStatusRequest, GitStatusSnapshot, RepositoryPrefix,
};

/// The number of commands that one complete status read runs.
const READ_COMMANDS: usize = 2;

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

/// Reads the status of one workspace root through the bounded process
/// service, exactly as the terminal event loop does.
///
/// The call runs the real `git` command, so it proves the flags of
/// [`GitStatusRequest::command`]. A recorded output can never prove them.
fn read_status(root: &Path) -> Result<GitStatusSnapshot, GitStatusFailure> {
    let root = Arc::new(WorktreeRoot::open(root).expect("the fixture root is one directory"));
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the test host starts one Tokio runtime");
    tokio.block_on(async move {
        let mut request = GitStatusRequest::new(root);
        for _ in 0..READ_COMMANDS {
            let output = run(request.command()).await;
            match request.publish(&output)? {
                GitStatusRead::Pending(next) => request = next,
                GitStatusRead::Published(snapshot) => return Ok(snapshot),
            }
        }
        unreachable!("one status read publishes after {READ_COMMANDS} commands");
    })
}

/// The workspace root of every parser test.
///
/// The parser performs no filesystem work, so one capability over the
/// working directory of the test process names the root of every snapshot.
fn root() -> Arc<WorktreeRoot> {
    Arc::new(
        WorktreeRoot::open(
            std::env::current_dir().expect("the test process holds a working directory"),
        )
        .expect("the working directory is one canonical root"),
    )
}

/// Returns one validated contained path of the test root.
fn relative(path: &str) -> WorktreeRelativePath {
    WorktreeRelativePath::new(path).expect("the fixture path is contained")
}

/// Builds one snapshot whose workspace root is the repository top level.
fn snapshot(output: &str) -> GitStatusSnapshot {
    GitStatusSnapshot::parse(root(), &RepositoryPrefix::default(), output.as_bytes())
}

/// Returns the state of one path below the workspace root.
fn state(snapshot: &GitStatusSnapshot, path: &str) -> Option<GitStatus> {
    snapshot.state(&relative(path))
}

/// One ordinary record, from the recorded output of `git status`.
fn ordinary(field: &str, path: &str) -> String {
    format!(
        "1 {field} N... 100644 100644 100644 \
             78981922613b2afb6025042ff6bd878ac1994e85 \
             78981922613b2afb6025042ff6bd878ac1994e85 {path}\0"
    )
}

#[test]
fn a_clean_tree_reports_no_state_at_all() {
    let snapshot = snapshot("");
    assert_eq!(state(&snapshot, "src/main.rs"), None);
    assert_eq!(state(&snapshot, "src"), None);
}

#[test]
fn each_half_of_the_state_field_names_its_own_state() {
    let output = format!(
        "{}{}{}",
        ordinary(".M", "src/modified.rs"),
        ordinary("M.", "src/staged.rs"),
        ordinary("MM", "src/both.rs"),
    );
    let snapshot = snapshot(&output);
    assert_eq!(
        state(&snapshot, "src/modified.rs"),
        Some(GitStatus::Modified)
    );
    assert_eq!(state(&snapshot, "src/staged.rs"), Some(GitStatus::Staged));
    assert_eq!(
        state(&snapshot, "src/both.rs"),
        Some(GitStatus::StagedAndModified)
    );
}

#[test]
fn an_untracked_and_an_ignored_record_name_their_own_state() {
    let snapshot = snapshot("? src/new.rs\0! target/\0");
    assert_eq!(state(&snapshot, "src/new.rs"), Some(GitStatus::Untracked));
    assert_eq!(state(&snapshot, "target"), Some(GitStatus::Ignored));
}

#[test]
fn a_collapsed_directory_record_covers_every_entry_below_it() {
    // `git status` names one ignored directory instead of every file below
    // it, so the entries of that directory inherit its state.
    let snapshot = snapshot("! target/\0? crates/\0");
    assert_eq!(
        state(&snapshot, "target/debug/build/out.o"),
        Some(GitStatus::Ignored)
    );
    assert_eq!(
        state(&snapshot, "crates/kvim/src/main.rs"),
        Some(GitStatus::Untracked)
    );
}

#[test]
fn the_state_of_one_entry_rolls_up_onto_the_directories_above_it() {
    let snapshot = snapshot(&ordinary(".M", "crates/kvim/src/main.rs"));
    assert_eq!(
        state(&snapshot, "crates/kvim/src"),
        Some(GitStatus::Modified)
    );
    assert_eq!(state(&snapshot, "crates/kvim"), Some(GitStatus::Modified));
    assert_eq!(state(&snapshot, "crates"), Some(GitStatus::Modified));
}

#[test]
fn one_directory_reports_both_halves_of_the_changes_below_it() {
    let output = format!(
        "{}{}",
        ordinary("M.", "src/staged.rs"),
        ordinary(".M", "src/modified.rs"),
    );
    assert_eq!(
        state(&snapshot(&output), "src"),
        Some(GitStatus::StagedAndModified),
        "the roll-up keeps both halves that the entries below it hold"
    );
}

#[test]
fn an_ignored_entry_never_reaches_the_directories_above_it() {
    // An ordinary repository ignores its build directory. That directory
    // must not make the whole workspace read as ignored, so no entry beside
    // it inherits the state.
    let snapshot = snapshot("! target/\0");
    assert_eq!(state(&snapshot, "target"), Some(GitStatus::Ignored));
    assert_eq!(state(&snapshot, "src/main.rs"), None);
}

#[test]
fn a_renamed_record_names_one_entry_and_drops_its_original_path() {
    let output = concat!(
        "2 R. N... 100644 100644 100644 aaaa bbbb R100 docs/two.md\0",
        "docs/one.md\0",
        "? docs/three.md\0",
    );
    let snapshot = snapshot(output);
    assert_eq!(state(&snapshot, "docs/two.md"), Some(GitStatus::Staged));
    assert_eq!(
        state(&snapshot, "docs/three.md"),
        Some(GitStatus::Untracked),
        "the record behind the original path still parses"
    );
}

#[test]
fn an_unmerged_record_names_one_conflict() {
    let output = "u UU N... 100644 100644 100644 100644 aa bb cc src/merge.rs\0";
    assert_eq!(
        state(&snapshot(output), "src/merge.rs"),
        Some(GitStatus::Conflicted)
    );
}

#[test]
fn a_malformed_record_is_dropped_without_a_panic() {
    let output = concat!(
        "\0",
        "x unknown record type\0",
        "1 .M too few fields\0",
        "1 ... N... 100644 100644 100644 aa bb src/wide.rs\0",
        "1 .. N... 100644 100644 100644 aa bb src/quiet.rs\0",
        "? \0",
        "? ../outside.rs\0",
        "? /absolute.rs\0",
        "? src/only-valid.rs\0",
    );
    let snapshot = snapshot(output);
    assert_eq!(
        state(&snapshot, "src/only-valid.rs"),
        Some(GitStatus::Untracked)
    );
    assert_eq!(state(&snapshot, "src/wide.rs"), None);
    assert_eq!(state(&snapshot, "src/quiet.rs"), None);
    assert_eq!(state(&snapshot, "outside.rs"), None);
}

#[test]
fn a_path_that_holds_spaces_stays_one_field() {
    let snapshot = snapshot(&ordinary(".M", "docs/two words.md"));
    assert_eq!(
        state(&snapshot, "docs/two words.md"),
        Some(GitStatus::Modified)
    );
}

#[test]
fn the_record_list_stops_at_the_entry_bound() {
    let mut output = String::new();
    for index in 0..GIT_STATUS_ENTRIES_MAX + 8 {
        output.push_str(&format!("? src/file-{index}.rs\0"));
    }
    let snapshot = snapshot(&output);
    assert_eq!(
        state(&snapshot, "src/file-0.rs"),
        Some(GitStatus::Untracked)
    );
    assert_eq!(
        state(&snapshot, &format!("src/file-{GIT_STATUS_ENTRIES_MAX}.rs")),
        None,
        "the entry bound keeps the snapshot finite"
    );
}

#[test]
fn a_record_outside_the_workspace_root_is_dropped() {
    // The workspace root may sit below the top level of the repository.
    // Git reports every path against that top level, and the prefix names
    // the place of the root inside it.
    let prefix = RepositoryPrefix::parse(b"crates/kvim/\n").expect("the prefix is one path");
    let output = concat!(
        "1 .M N... 100644 100644 100644 aa bb crates/kvim/src/main.rs\0",
        "1 .M N... 100644 100644 100644 aa bb docs/other.md\0",
    );
    let snapshot = GitStatusSnapshot::parse(root(), &prefix, output.as_bytes());
    assert_eq!(
        snapshot.state(&relative("src/main.rs")),
        Some(GitStatus::Modified)
    );
    assert_eq!(snapshot.state(&relative("docs/other.md")), None);
}

#[test]
fn a_root_that_is_its_own_top_level_reports_an_empty_prefix() {
    assert_eq!(
        RepositoryPrefix::parse(b"\n"),
        Some(RepositoryPrefix::default())
    );
    assert_eq!(
        RepositoryPrefix::parse(b""),
        Some(RepositoryPrefix::default())
    );
}

#[test]
fn a_prefix_that_leaves_the_repository_is_refused() {
    assert_eq!(RepositoryPrefix::parse(b"../escape/\n"), None);
    assert_eq!(RepositoryPrefix::parse(b"/absolute/\n"), None);
}

#[test]
fn the_command_reads_the_repository_and_never_writes_it() {
    let command = GitStatusRequest::new(root()).command();
    let args: Vec<String> = command
        .args
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    // The format flags need no assertion here. One real read proves them
    // by its result, and a recorded output could never prove them at all.
    assert!(
        args.contains(&"--no-optional-locks".to_owned()),
        "the read gives up every optional lock of the repository"
    );
    assert!(
        args.contains(&"core.hooksPath=/dev/null".to_owned()),
        "the repository cannot start a hook during a read"
    );
    assert!(
        args.contains(&"diff.external=".to_owned()),
        "the repository cannot start an external diff program"
    );
    assert!(command.stdin.is_empty());
    assert_eq!(command.current_dir.as_deref(), Some(root().as_path()));
}

#[test]
fn every_command_drops_the_inherited_helper_variables() {
    let command = GitStatusRequest::new(root()).command();
    let dropped: Vec<String> = command
        .dropped_variables
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    // Each of these either redirects the read to another repository or
    // names a program that Git would start.
    for name in [
        "GIT_DIR",
        "GIT_EXTERNAL_DIFF",
        "GIT_PAGER",
        "GIT_SSH_COMMAND",
    ] {
        assert!(dropped.contains(&name.to_owned()), "{name} stays inherited");
    }
}

#[test]
fn the_status_stage_carries_the_bounds_of_one_status_read() {
    let request = GitStatusRequest::new(root());
    let prefix = ProcessOutput {
        status_code: Some(0),
        stdout: b"\n".to_vec(),
        stderr: Vec::new(),
    };
    let GitStatusRead::Pending(request) = request
        .publish(&prefix)
        .expect("the prefix command answered")
    else {
        panic!("the first stage never publishes one snapshot");
    };
    let command = request.command();
    let args: Vec<String> = command
        .args
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args.last().map(String::as_str),
        Some("."),
        "the pathspec follows the separator, so a root cannot become a flag"
    );
    assert_eq!(command.output_bytes_max, GIT_STATUS_OUTPUT_BYTES_MAX);
    assert_eq!(command.deadline, GIT_STATUS_DEADLINE);
}

#[test]
fn a_refused_command_reports_no_status() {
    // Git reports a directory outside a repository through its exit code.
    let output = ProcessOutput {
        status_code: Some(128),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    assert_eq!(
        GitStatusRequest::new(root()).publish(&output),
        Err(GitStatusFailure::Unavailable)
    );
}

#[test]
fn the_status_flags_report_every_state_of_one_real_repository() {
    // The recorded records above prove the parser. This test proves the
    // flags: only a real invocation shows that `--ignored=traditional`
    // still names one directory instead of every file below it, and that
    // `--porcelain=v2` still writes the format that the parser reads.
    let repository = TempRepository::new("git-states");
    repository.file(".gitignore", "build/\n");
    repository.file("src/modified.rs", "one\n");
    repository.file("src/staged.rs", "one\n");
    repository.file("docs/clean.md", "one\n");
    repository.commit("record the first state");

    repository.file("src/modified.rs", "one\ntwo\n");
    repository.file("src/staged.rs", "one\ntwo\n");
    repository.git(&["add", "src/staged.rs"]);
    repository.file("src/untracked.rs", "one\n");
    repository.file("build/output.o", "one\n");

    let snapshot = read_status(repository.path()).expect("the directory is one repository");
    let state = |name: &str| snapshot.state(&relative(name));

    assert_eq!(state("src/modified.rs"), Some(GitStatus::Modified));
    assert_eq!(state("src/staged.rs"), Some(GitStatus::Staged));
    assert_eq!(state("src/untracked.rs"), Some(GitStatus::Untracked));
    assert_eq!(state("build"), Some(GitStatus::Ignored));
    assert_eq!(
        state("build/output.o"),
        Some(GitStatus::Ignored),
        "one collapsed directory record covers every file below it"
    );
    assert_eq!(
        state("src"),
        Some(GitStatus::StagedAndModified),
        "the directory reports both halves of the changes below it"
    );
    assert_eq!(state("docs/clean.md"), None);
    assert_eq!(state("docs"), None, "a clean subtree reports nothing");
}

#[test]
fn a_workspace_below_the_repository_reads_its_own_subtree() {
    let repository = TempRepository::new("git-nested");
    repository.file("crates/kvim/src/main.rs", "one\n");
    repository.file("docs/outside.md", "one\n");
    repository.commit("record the first state");

    repository.file("crates/kvim/src/main.rs", "one\ntwo\n");
    repository.file("docs/outside.md", "one\ntwo\n");

    let workspace = repository.join("crates/kvim");
    let snapshot = read_status(&workspace).expect("the directory sits inside one repository");

    assert_eq!(snapshot.root().as_path(), workspace.as_path());
    assert_eq!(
        snapshot.state(&relative("src/main.rs")),
        Some(GitStatus::Modified),
        "Git reports the path against the top level, and the prefix subtracts it"
    );
    assert_eq!(
        snapshot.state(&relative("docs/outside.md")),
        None,
        "the pathspec keeps the report inside the workspace root"
    );
}

#[test]
fn a_directory_outside_a_repository_reports_no_status() {
    let dir = TempDir::new("git-plain");
    assert_eq!(
        read_status(&dir.path),
        Err(GitStatusFailure::Unavailable),
        "the refusal is a normal state, never an error of the editor"
    );
}
