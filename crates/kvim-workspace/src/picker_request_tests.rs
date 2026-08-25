use kvim_path::{WorktreeConfinementError, WorktreeRelativePath, WorktreeRoot};

use super::{PREVIEW_CONTEXT_LINES, PREVIEW_LINES_MAX, PreviewError, read_preview};
use crate::temp::TempDir;

fn preview(dir: &TempDir, path: &str, line: usize) -> Result<super::Preview, PreviewError> {
    let root = WorktreeRoot::open(&dir.path).expect("the fixture root exists");
    let path = WorktreeRelativePath::new(path).expect("the fixture path is valid");
    read_preview(&root, &path, line)
}

#[test]
fn the_preview_shows_the_region_around_the_line() {
    let dir = TempDir::new("preview-region");
    let text: String = (0..64).map(|index| format!("line {index}\n")).collect();
    dir.file("src/main.rs", &text);
    let preview = preview(&dir, "src/main.rs", 40).expect("the file holds text");
    assert_eq!(preview.first_line, 40 - PREVIEW_CONTEXT_LINES);
    assert_eq!(preview.lines.first().map(String::as_str), Some("line 32"));
    assert!(preview.lines.len() <= PREVIEW_LINES_MAX);
    assert!(!preview.truncated);
}

#[test]
fn a_line_above_the_file_end_shows_the_last_region() {
    let dir = TempDir::new("preview-end");
    dir.file("a.rs", "one\ntwo\n");
    let preview = preview(&dir, "a.rs", 900).expect("the file holds text");
    assert!(preview.lines.is_empty(), "the region starts after the text");
}

#[test]
fn a_binary_file_reports_an_unsupported_preview() {
    let dir = TempDir::new("preview-binary");
    let path = dir.join("binary");
    std::fs::write(&path, [0_u8, 1, 2, 3]).expect("the temporary directory is writable");
    assert!(matches!(
        preview(&dir, "binary", 0),
        Err(PreviewError::Unsupported)
    ));
}

#[test]
fn a_missing_file_reports_a_read_failure() {
    let dir = TempDir::new("preview-missing");
    assert!(matches!(
        preview(&dir, "absent", 0),
        Err(PreviewError::Read(_))
    ));
}

#[test]
fn the_preview_stops_at_the_line_and_character_bounds() {
    let dir = TempDir::new("preview-bounds");
    let long: String = "x".repeat(super::PREVIEW_LINE_CHARS_MAX + 64);
    let text: String = (0..PREVIEW_LINES_MAX + 32)
        .map(|_| format!("{long}\n"))
        .collect();
    dir.file("long.rs", &text);
    let preview = preview(&dir, "long.rs", 0).expect("the file holds text");
    assert_eq!(preview.lines.len(), PREVIEW_LINES_MAX);
    assert!(
        preview
            .lines
            .iter()
            .all(|line| line.chars().count() <= super::PREVIEW_LINE_CHARS_MAX)
    );
    assert!(preview.truncated);
}

#[test]
fn the_preview_reports_byte_clipping() {
    let dir = TempDir::new("preview-byte-bound");
    dir.file("large.rs", &"x".repeat(super::PREVIEW_BYTES_MAX + 8));

    let preview = preview(&dir, "large.rs", 0).expect("the file holds text");

    assert!(preview.truncated);
}

#[test]
fn the_preview_reports_line_clipping() {
    let dir = TempDir::new("preview-line-bound");
    let text = "line\n".repeat(PREVIEW_LINES_MAX + 1);
    dir.file("lines.rs", &text);

    let preview = preview(&dir, "lines.rs", 0).expect("the file holds text");

    assert_eq!(preview.lines.len(), PREVIEW_LINES_MAX);
    assert!(preview.truncated);
}

#[test]
fn the_preview_reports_character_clipping() {
    let dir = TempDir::new("preview-character-bound");
    dir.file("line.rs", &"x".repeat(super::PREVIEW_LINE_CHARS_MAX + 1));

    let preview = preview(&dir, "line.rs", 0).expect("the file holds text");

    assert_eq!(
        preview.lines[0].chars().count(),
        super::PREVIEW_LINE_CHARS_MAX
    );
    assert!(preview.truncated);
}

#[cfg(unix)]
#[test]
fn escaping_dangling_and_looping_preview_links_are_rejected() {
    let dir = TempDir::new("preview-link-failures");
    let outside = TempDir::new("preview-link-failures-outside");
    outside.file("outside.rs", "outside\n");
    std::os::unix::fs::symlink(outside.join("outside.rs"), dir.join("escape.rs"))
        .expect("the temporary directory supports links");
    std::os::unix::fs::symlink("missing.rs", dir.join("dangling.rs"))
        .expect("the temporary directory supports links");
    std::os::unix::fs::symlink("loop-b.rs", dir.join("loop-a.rs"))
        .expect("the temporary directory supports links");
    std::os::unix::fs::symlink("loop-a.rs", dir.join("loop-b.rs"))
        .expect("the temporary directory supports links");

    assert!(matches!(
        preview(&dir, "escape.rs", 0),
        Err(PreviewError::Confinement(WorktreeConfinementError::Escape))
    ));
    assert!(matches!(
        preview(&dir, "dangling.rs", 0),
        Err(PreviewError::Confinement(
            WorktreeConfinementError::DanglingLink
        ))
    ));
    assert!(matches!(
        preview(&dir, "loop-a.rs", 0),
        Err(PreviewError::Confinement(
            WorktreeConfinementError::LinkLoop
        ))
    ));
}
