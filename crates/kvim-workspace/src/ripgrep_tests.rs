use kvim_path::WorktreeRoot;

use crate::temp::TempDir;

use super::{RIPGREP_MATCHES_MAX, parse_matches, ripgrep_command};

fn rows(output: &[u8]) -> Vec<String> {
    let directory = TempDir::new("ripgrep-parse");
    directory.file("src/main.rs", "");
    directory.file("a.rs", "");
    let root = WorktreeRoot::open(&directory.path).expect("the fixture root exists");
    parse_matches(&root, output)
        .0
        .iter()
        .map(super::Candidate::row)
        .collect()
}

#[test]
fn one_result_line_becomes_one_candidate() {
    assert_eq!(
        rows(b"./src/main.rs:12:5:let value = 1;\n"),
        vec!["main.rs:12  src  let value = 1;"]
    );
}

#[test]
fn a_malformed_line_is_dropped_without_a_panic() {
    let output = concat!(
        "no-colons-at-all\n",
        "./a.rs:not-a-number:1:text\n",
        "./a.rs:3:not-a-number:text\n",
        "./a.rs:0:1:a line number starts at one\n",
        "./a.rs:1:0:a column starts at one\n",
        ":4:1:the path is empty\n",
        "./a.rs:5:1\n",
        "\n",
        "./a.rs:6:1:the only valid line\n",
    );
    assert_eq!(
        rows(output.as_bytes()),
        vec!["a.rs:6  the only valid line"],
        "every malformed line is dropped"
    );
}

#[test]
fn a_truncated_last_line_is_dropped() {
    // The output limit can stop inside one line, so the last record may
    // hold no column and no text.
    assert_eq!(
        rows(b"./a.rs:1:1:complete\n./b.rs:2:"),
        vec!["a.rs:1  complete"]
    );
}

#[test]
fn a_text_that_holds_colons_stays_one_field() {
    assert_eq!(
        rows(b"./a.rs:1:1:let map: Map<K, V> = Map::new();\n"),
        vec!["a.rs:1  let map: Map<K, V> = Map::new();"]
    );
}

#[test]
fn the_match_list_stops_at_the_result_bound() {
    let mut output = String::new();
    for index in 1..=RIPGREP_MATCHES_MAX + 8 {
        output.push_str(&format!("./a.rs:{index}:1:line\n"));
    }
    let directory = TempDir::new("ripgrep-bound");
    directory.file("a.rs", "");
    let root = WorktreeRoot::open(&directory.path).expect("the fixture root exists");
    let (candidates, truncated) = parse_matches(&root, output.as_bytes());
    assert_eq!(candidates.len(), RIPGREP_MATCHES_MAX);
    assert!(truncated);
}

#[test]
fn the_command_never_reads_standard_input() {
    let directory = TempDir::new("ripgrep-command");
    let root = WorktreeRoot::open(&directory.path).expect("the fixture root exists");
    let command = ripgrep_command(&root, "-needle");
    let args: Vec<String> = command
        .args
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    assert!(args.contains(&".".to_owned()), "the search names its path");
    let pattern = args
        .iter()
        .position(|value| value == "--regexp")
        .map(|index| args[index + 1].clone());
    assert_eq!(
        pattern,
        Some("-needle".to_owned()),
        "a query that starts with a hyphen stays a pattern"
    );
    for flag in [
        "--no-config",
        "--no-follow",
        "--no-ignore-parent",
        "--no-ignore-global",
    ] {
        assert!(
            args.iter().any(|argument| argument == flag),
            "missing {flag}"
        );
    }
    assert!(command.stdin.is_empty());
}

#[cfg(unix)]
#[test]
fn escaping_and_dangling_records_are_omitted_as_truncated() {
    let directory = TempDir::new("ripgrep-confined");
    let outside = TempDir::new("ripgrep-confined-outside");
    outside.file("outside.rs", "");
    std::os::unix::fs::symlink(outside.join("outside.rs"), directory.join("escape.rs"))
        .expect("the temporary directory supports links");
    std::os::unix::fs::symlink("missing.rs", directory.join("dangling.rs"))
        .expect("the temporary directory supports links");
    let root = WorktreeRoot::open(&directory.path).expect("the fixture root exists");

    let (candidates, truncated) = parse_matches(
        &root,
        b"./escape.rs:1:1:outside\n./dangling.rs:1:1:missing\n",
    );

    assert!(candidates.is_empty());
    assert!(truncated);
}

#[cfg(unix)]
#[test]
fn a_contained_link_record_publishes_its_resolved_identity() {
    let directory = TempDir::new("ripgrep-contained-link");
    directory.file("real.rs", "");
    std::os::unix::fs::symlink("real.rs", directory.join("alias.rs"))
        .expect("the temporary directory supports links");
    let root = WorktreeRoot::open(&directory.path).expect("the fixture root exists");

    let (candidates, truncated) = parse_matches(&root, b"./alias.rs:1:1:match\n");

    assert!(!truncated);
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].acceptance(),
        crate::Acceptance::OpenFile {
            path: directory.join("real.rs"),
            line: 0,
            byte_column: 0,
        }
    );
}

#[test]
fn stdout_at_the_capture_bound_is_conservatively_truncated() {
    let directory = TempDir::new("ripgrep-output-bound");
    let root = WorktreeRoot::open(&directory.path).expect("the fixture root exists");
    let output = vec![b'x'; super::RIPGREP_OUTPUT_BYTES_MAX];

    let (_, truncated) = parse_matches(&root, &output);

    assert!(truncated);
}
