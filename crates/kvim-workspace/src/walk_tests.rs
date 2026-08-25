use std::path::Path;
use std::sync::Arc;

use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use tokio_util::sync::CancellationToken;

use crate::TREE_DIRECTORY_ENTRIES_MAX;
use crate::temp::TempDir;

use super::{Pattern, glob_matches, walk_files};

/// Returns the walked files, relative to the root and in ascending order.
fn walked(dir: &TempDir) -> Vec<String> {
    let root = Arc::new(WorktreeRoot::open(&dir.path).expect("the fixture root exists"));
    let outcome = walk_files(root, &CancellationToken::new());
    let mut files: Vec<String> = outcome
        .files
        .iter()
        .map(|path| path.as_path().to_string_lossy().replace('\\', "/"))
        .collect();
    files.sort();
    files
}

#[test]
fn the_walk_collects_every_file_below_the_root() {
    let dir = TempDir::new("walk-plain");
    dir.file("src/main.rs", "fn main() {}\n");
    dir.file("src/tui/mod.rs", "\n");
    dir.file("README.md", "\n");
    assert_eq!(
        walked(&dir),
        vec!["README.md", "src/main.rs", "src/tui/mod.rs"]
    );
}

#[test]
fn the_walk_drops_the_ignored_files_and_the_git_directory() {
    let dir = TempDir::new("walk-ignore");
    dir.file(".gitignore", "target/\n*.tmp\n!keep.tmp\n");
    dir.file("src/main.rs", "\n");
    dir.file("target/debug/kvim", "\n");
    dir.file("scratch.tmp", "\n");
    dir.file("keep.tmp", "\n");
    dir.file(".git/config", "\n");
    assert_eq!(
        walked(&dir),
        vec![".gitignore", "keep.tmp", "src/main.rs"],
        "the ignore file drops target/ and *.tmp, and keeps the negated name"
    );
}

#[test]
fn one_ignore_file_applies_below_its_own_directory() {
    let dir = TempDir::new("walk-nested");
    dir.file("src/.gitignore", "generated.rs\n");
    dir.file("src/generated.rs", "\n");
    dir.file("src/main.rs", "\n");
    dir.file("generated.rs", "\n");
    assert_eq!(
        walked(&dir),
        vec!["generated.rs", "src/.gitignore", "src/main.rs"],
        "the ignore file of `src` names no file of the root"
    );
}

#[test]
fn an_anchored_pattern_names_the_directory_of_its_ignore_file() {
    let dir = TempDir::new("walk-anchored");
    dir.file(".gitignore", "/build\n");
    dir.file("build/output", "\n");
    dir.file("src/build/output", "\n");
    assert_eq!(walked(&dir), vec![".gitignore", "src/build/output"]);
}

#[test]
fn a_cancelled_walk_returns_a_truncated_outcome() {
    let dir = TempDir::new("walk-cancelled");
    dir.file("src/main.rs", "\n");
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let root = Arc::new(WorktreeRoot::open(&dir.path).expect("the fixture root exists"));
    let outcome = walk_files(root, &cancellation);
    assert!(outcome.truncated);
    assert!(outcome.files.is_empty());
}

#[test]
fn a_walk_above_one_bound_reports_the_truncation() {
    // The shared directory reader stops at its own entry bound, so a
    // directory above that bound truncates the walk as well.
    let dir = TempDir::new("walk-bound");
    for index in 0..TREE_DIRECTORY_ENTRIES_MAX + 4 {
        dir.file(&format!("f{index}"), "\n");
    }
    let root = Arc::new(WorktreeRoot::open(&dir.path).expect("the fixture root exists"));
    let outcome = walk_files(root, &CancellationToken::new());
    assert!(outcome.truncated);
    assert_eq!(outcome.files.len(), TREE_DIRECTORY_ENTRIES_MAX);
    assert!(outcome.files.len() <= super::WALK_FILES_MAX);
}

#[cfg(unix)]
#[test]
fn contained_directory_aliases_repeat_no_files() {
    let dir = TempDir::new("walk-alias");
    dir.file("real/inner.rs", "");
    std::os::unix::fs::symlink("real", dir.join("alias"))
        .expect("the temporary directory supports links");

    let files = walked(&dir);
    assert_eq!(files.len(), 1, "one resolved directory is walked once");
    assert!(
        files == ["alias/inner.rs"] || files == ["real/inner.rs"],
        "source order decides which contained spelling owns the subtree: {files:?}"
    );
}

#[cfg(unix)]
#[test]
fn root_directory_aliases_repeat_no_files() {
    let dir = TempDir::new("walk-root-alias");
    dir.file("main.rs", "");
    std::os::unix::fs::symlink(".", dir.join("alias"))
        .expect("the temporary directory supports links");

    let root = Arc::new(WorktreeRoot::open(&dir.path).expect("the fixture root exists"));
    let outcome = walk_files(root, &CancellationToken::new());
    assert_eq!(
        outcome
            .files
            .iter()
            .map(WorktreeRelativePath::as_path)
            .collect::<Vec<_>>(),
        [Path::new("main.rs")]
    );
    assert!(outcome.truncated);
}

#[test]
fn a_kept_directory_below_the_depth_bound_reports_truncation() {
    let dir = TempDir::new("walk-depth");
    let mut path = String::new();
    for depth in 0..=super::WALK_DEPTH_MAX {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(&format!("d{depth}"));
    }
    dir.file(&format!("{path}/hidden.rs"), "");
    let root = Arc::new(WorktreeRoot::open(&dir.path).expect("the fixture root exists"));

    let outcome = walk_files(root, &CancellationToken::new());

    assert!(outcome.truncated);
    assert!(outcome.files.is_empty());
}

#[cfg(unix)]
#[test]
fn escaping_and_looping_links_are_omitted_visibly() {
    let dir = TempDir::new("walk-link-failures");
    let outside = TempDir::new("walk-link-failures-outside");
    outside.file("outside.rs", "");
    std::os::unix::fs::symlink(outside.join("outside.rs"), dir.join("escape.rs"))
        .expect("the temporary directory supports links");
    std::os::unix::fs::symlink("loop-b.rs", dir.join("loop-a.rs"))
        .expect("the temporary directory supports links");
    std::os::unix::fs::symlink("loop-a.rs", dir.join("loop-b.rs"))
        .expect("the temporary directory supports links");

    let root = Arc::new(WorktreeRoot::open(&dir.path).expect("the fixture root exists"));
    let outcome = walk_files(root, &CancellationToken::new());
    assert!(outcome.files.is_empty());
    assert!(outcome.truncated, "the picker reports the rejected links");
}

#[cfg(unix)]
#[test]
fn an_escaping_ignore_file_applies_no_outside_rules_and_reports_truncation() {
    let dir = TempDir::new("walk-ignore-escape");
    let outside = TempDir::new("walk-ignore-escape-outside");
    outside.file("rules", "*.rs\n");
    dir.file("main.rs", "");
    std::os::unix::fs::symlink(outside.join("rules"), dir.join(".gitignore"))
        .expect("the temporary directory supports links");

    let root = Arc::new(WorktreeRoot::open(&dir.path).expect("the fixture root exists"));
    let outcome = walk_files(root, &CancellationToken::new());
    assert_eq!(
        outcome
            .files
            .iter()
            .map(WorktreeRelativePath::as_path)
            .collect::<Vec<_>>(),
        [Path::new("main.rs")]
    );
    assert!(outcome.truncated);
}

#[test]
fn a_non_utf8_existing_ignore_file_reports_truncation() {
    let dir = TempDir::new("walk-ignore-utf8");
    dir.file("main.rs", "");
    std::fs::write(dir.join(".gitignore"), [0xff]).expect("the temporary directory is writable");
    let root = Arc::new(WorktreeRoot::open(&dir.path).expect("the fixture root exists"));

    let outcome = walk_files(root, &CancellationToken::new());

    assert!(outcome.truncated);
    assert_eq!(
        outcome
            .files
            .iter()
            .map(WorktreeRelativePath::as_path)
            .collect::<Vec<_>>(),
        [Path::new(".gitignore"), Path::new("main.rs")]
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_queued_directory_reports_truncation() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = TempDir::new("walk-unreadable");
    dir.file("locked/main.rs", "");
    let locked = dir.join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
        .expect("the temporary directory is writable");
    let readable = std::fs::read_dir(&locked).is_ok();
    let root = Arc::new(WorktreeRoot::open(&dir.path).expect("the fixture root exists"));
    let outcome = walk_files(root, &CancellationToken::new());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
        .expect("the temporary directory is writable");

    if !readable {
        assert!(outcome.truncated);
        assert!(outcome.files.is_empty());
    }
}

#[test]
fn the_glob_matches_the_supported_pattern_subset() {
    let cases = [
        ("*.rs", "main.rs", true),
        ("*.rs", "main.txt", false),
        ("*.rs", "src/main.rs", false),
        ("**/main.rs", "src/tui/main.rs", true),
        ("**/main.rs", "main.rs", true),
        ("src/*.rs", "src/main.rs", true),
        ("src/*.rs", "src/tui/main.rs", false),
        ("ma?n.rs", "main.rs", true),
        ("build", "build", true),
    ];
    for (glob, text, expected) in cases {
        assert_eq!(
            glob_matches(glob, text),
            expected,
            "`{glob}` against `{text}`"
        );
    }
}

#[test]
fn a_comment_and_an_empty_line_name_no_pattern() {
    assert_eq!(Pattern::parse("# comment"), None);
    assert_eq!(Pattern::parse("   "), None);
    assert_eq!(Pattern::parse(""), None);
    assert_eq!(Pattern::parse("/"), None);
}
