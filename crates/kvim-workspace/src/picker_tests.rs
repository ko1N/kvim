use kvim_path::{WorktreeRelativePath, WorktreeRoot};

use super::{Acceptance, BufferId, Candidate, Picker, PickerKind, PreviewTarget};
use crate::temp::TempDir;

fn root(label: &str) -> (TempDir, WorktreeRoot) {
    let directory = TempDir::new(label);
    let root = WorktreeRoot::open(&directory.path).expect("the fixture root exists");
    (directory, root)
}

fn file(root: &WorktreeRoot, path: &str) -> Candidate {
    Candidate::file(
        root,
        WorktreeRelativePath::new(path).expect("the fixture path is valid"),
    )
}

fn picker(names: &[&str]) -> Picker {
    let (_directory, root) = root("picker");
    let mut picker = Picker::new(PickerKind::Files, root.as_path().to_path_buf());
    let candidates = names.iter().map(|name| file(&root, name)).collect();
    picker.set_candidates(candidates, false);
    picker
}

fn rows(picker: &Picker) -> Vec<String> {
    picker
        .matches()
        .iter()
        .filter_map(|index| picker.candidate(*index))
        .map(Candidate::row)
        .collect()
}

#[test]
fn the_row_shows_the_filename_before_its_directory() {
    let (_directory, root) = root("picker-row");
    let candidate = file(&root, "src/tui/picker.rs");
    assert_eq!(candidate.row(), "picker.rs  src/tui");
    assert_eq!(candidate.name(), "picker.rs");
    assert_eq!(candidate.directory(), "src/tui");
}

#[test]
fn a_matched_row_shows_the_line_and_its_text() {
    let (_directory, root) = root("picker-match");
    let candidate = Candidate::matched(
        &root,
        WorktreeRelativePath::new("src/main.rs").expect("the fixture path is valid"),
        41,
        4,
        "  fn main()",
    );
    assert_eq!(candidate.row(), "main.rs:42  src  fn main()");
    assert_eq!(
        candidate.acceptance(),
        Acceptance::OpenFile {
            path: root.as_path().join("src/main.rs"),
            line: 41,
            byte_column: 4,
        }
    );
}

#[test]
fn an_empty_query_keeps_the_order_of_the_source() {
    let picker = picker(&["src/zebra.rs", "alpha.rs", "src/main.rs"]);
    assert_eq!(
        rows(&picker),
        vec!["zebra.rs  src", "alpha.rs", "main.rs  src"]
    );
}

#[test]
fn the_best_match_sits_at_the_top_of_the_list() {
    let mut picker = picker(&["src/domain.rs", "src/main.rs", "docs/manual.md"]);
    picker.set_query("main");
    assert_eq!(
        rows(&picker).first().map(String::as_str),
        Some("main.rs  src")
    );
}

#[test]
fn two_equal_scores_keep_one_deterministic_order() {
    // Both names hold the query at the same positions, so the shorter row
    // wins, and the earlier candidate wins an equal width.
    let mut picker = picker(&["ab_long_name.rs", "ab.rs", "ab2.rs"]);
    picker.set_query("ab");
    let first = rows(&picker);
    picker.set_query("");
    picker.set_query("ab");
    assert_eq!(first, rows(&picker), "one query always produces one order");
    assert_eq!(first.first().map(String::as_str), Some("ab.rs"));
}

#[test]
fn the_selection_follows_its_candidate_across_one_refiltering() {
    let mut picker = picker(&["src/main.rs", "src/mode.rs", "src/motion.rs"]);
    picker.set_query("mo");
    picker.select_next();
    let selected = picker
        .selected()
        .expect("the query keeps three rows")
        .name()
        .to_owned();
    picker.set_query("mot");
    assert_eq!(
        picker.selected().map(Candidate::name),
        Some(selected.as_str()),
        "the selected row still matches, so it stays selected"
    );
}

#[test]
fn a_selection_that_the_query_drops_returns_to_the_best_row() {
    let mut picker = picker(&["src/main.rs", "src/mode.rs"]);
    picker.set_query("m");
    picker.select_next();
    assert_eq!(picker.selected().map(Candidate::name), Some("mode.rs"));
    picker.set_query("main");
    assert_eq!(picker.selected().map(Candidate::name), Some("main.rs"));
}

#[test]
fn the_selection_stops_at_both_ends_of_the_list() {
    let mut picker = picker(&["a.rs", "b.rs"]);
    picker.select_previous();
    assert_eq!(picker.selected_row(), Some(0));
    picker.select_next();
    picker.select_next();
    picker.select_next();
    assert_eq!(picker.selected_row(), Some(1));
}

#[test]
fn a_candidate_list_above_the_bound_reports_the_truncation() {
    let (_directory, root) = root("picker-bound");
    let mut picker = Picker::new(PickerKind::Files, root.as_path().to_path_buf());
    let candidates = (0..super::PICKER_CANDIDATES_MAX + 8)
        .map(|index| file(&root, &format!("file{index}.rs")))
        .collect();
    picker.set_candidates(candidates, false);
    assert!(picker.is_truncated());
    assert_eq!(picker.matches().len(), super::PICKER_CANDIDATES_MAX);
}

#[test]
fn the_query_stops_at_the_character_bound() {
    let mut picker = picker(&["a.rs"]);
    picker.set_query(&"x".repeat(super::PICKER_QUERY_CHARS_MAX + 32));
    assert_eq!(
        picker.query().chars().count(),
        super::PICKER_QUERY_CHARS_MAX
    );
}

#[test]
fn a_file_candidate_previews_the_start_and_marks_no_line() {
    let (_directory, root) = root("picker-preview");
    let candidate = file(&root, "a.rs");
    let (_, path, target) = candidate.preview().expect("a file row shows a preview");
    assert_eq!(path, root.as_path().join("a.rs"));
    assert_eq!(target, PreviewTarget::Start);
    assert!(!target.marks(0), "a file row marks no line");

    let matched = Candidate::matched(
        &root,
        WorktreeRelativePath::new("a.rs").expect("the fixture path is valid"),
        4,
        0,
        "text",
    );
    let (_, _, target) = matched.preview().expect("a match row shows a preview");
    assert!(target.marks(4), "a match row marks its own line");
}

#[test]
fn a_buffer_candidate_needs_no_file_read() {
    let (_directory, root) = root("picker-buffer");
    let candidate = Candidate::buffer(
        root.as_path(),
        BufferId::new(3),
        Some(&root.as_path().join("src/main.rs")),
        "main.rs",
    );
    assert_eq!(candidate.preview(), None);
    assert_eq!(candidate.row(), "main.rs  src");
}
