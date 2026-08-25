//! Tests for the file sidebar that one embedded host draws.
//!
//! Every test drives one host-owned editor. No test opens a terminal and no
//! test lets the editor reach the filesystem by itself: a directory read leaves
//! the editor as one [`WorkspaceRequest`], and the test runs it exactly where a
//! host runs it, off the event loop. See `docs/embedding.md`.

use std::path::Path;
use std::time::Duration;

use ratatui::layout::Rect;

use kvim_settings::EditorSettings;
use kvim_ui::{SIDEBAR_GUIDE_BLANK, SIDEBAR_GUIDE_ELBOW, SIDEBAR_GUIDE_TRUNK, SidebarMotion};
use kvim_workspace::WorkspaceRequest;
use kvim_workspace::temp::TempDir;

use super::*;
use crate::embed::EditorEvent;
use crate::session::{Session, test_root};

/// The rectangle that every test gives the editor.
const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

/// The largest number of directory reads that one test runs.
const READS_MAX: usize = 32;

/// Creates one editor over one temporary workspace.
fn editor(root: &Path) -> Session {
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    Session::new(AREA, settings, test_root(root.to_path_buf()))
}

/// Runs every queued directory read, exactly where a host runs it.
///
/// The read blocks, so a host hands it to its bounded worker service. The test
/// runs it here and applies the typed result as one transition.
fn read_directories(session: &mut Session) {
    for _ in 0..READS_MAX {
        let Some(request) = session.take_workspace_request() else {
            return;
        };
        let _redraw = session.apply_workspace_result(request.run());
    }
    panic!("one transition queues fewer reads than the bound of this test");
}

/// Returns the label of every published row.
fn labels(session: &Session) -> Vec<String> {
    session
        .file_rows()
        .iter()
        .map(|row| row.label().to_owned())
        .collect()
}

/// Returns the row that carries one label.
fn row_of(session: &Session, label: &str) -> FileRow {
    session
        .file_rows()
        .into_iter()
        .find(|row| row.label() == label)
        .unwrap_or_else(|| {
            panic!(
                "the sidebar shows a row named {label}: {:?}",
                labels(session)
            )
        })
}

/// Selects the row that carries one label.
fn select(session: &mut Session, label: &str) {
    let row = session
        .file_rows()
        .iter()
        .position(|row| row.label() == label)
        .unwrap_or_else(|| panic!("the sidebar shows a row named {label}"));
    let outcome = session.reduce_file_sidebar(FileSidebarInput::Move(SidebarMotion::ToRow(row)));
    assert_eq!(outcome, FileSidebarOutcome::Applied);
    assert!(row_of(session, label).is_selected());
}

/// Creates the workspace that most tests read.
fn workspace(name: &str) -> TempDir {
    let directory = TempDir::new(name);
    directory.file("src/main.rs", "fn main() {}\n");
    directory.file("src/deep/inner.rs", "pub const ONE: u32 = 1;\n");
    directory.file("readme.md", "one line\n");
    directory
}

#[test]
fn the_first_listing_reaches_the_tree_as_work_and_never_as_a_read_of_the_facade() {
    let directory = workspace("file-sidebar-first-read");
    let mut session = editor(&directory.path);

    // The tree opens with one pending read. Every facade call runs before that
    // read, so a facade that read the directory itself would show rows here.
    assert!(session.file_rows().is_empty());
    assert_eq!(
        session.reduce_file_sidebar(FileSidebarInput::Move(SidebarMotion::Down(1))),
        FileSidebarOutcome::Applied
    );
    assert_eq!(
        session.reduce_file_sidebar(FileSidebarInput::Open),
        FileSidebarOutcome::Applied
    );
    assert_eq!(
        session.reduce_file_sidebar(FileSidebarInput::Activate),
        FileSidebarOutcome::Applied
    );
    assert!(session.file_rows().is_empty());

    // The read surfaced as work instead.
    let request = session
        .take_workspace_request()
        .expect("the tree hands its first read to the host");
    assert!(matches!(request, WorkspaceRequest::ReadDirectory { .. }));

    let _redraw = session.apply_workspace_result(request.run());
    assert_eq!(labels(&session), vec!["src", "readme.md"]);
}

#[test]
fn an_expansion_leaves_the_listing_as_work_and_shows_no_entry_before_it_returns() {
    let directory = workspace("file-sidebar-expansion-work");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    select(&mut session, "src");
    assert_eq!(row_of(&session, "src").kind(), FileRowKind::ClosedDirectory);

    // The expansion changes the tree and reads nothing. The directory holds
    // `main.rs` and `deep` on disk, and neither one reaches the rows yet.
    assert_eq!(
        session.reduce_file_sidebar(FileSidebarInput::Open),
        FileSidebarOutcome::Applied
    );
    assert_eq!(
        row_of(&session, "src").kind(),
        FileRowKind::LoadingDirectory
    );
    assert_eq!(labels(&session), vec!["src", "readme.md"]);

    // Moving and drawing over the waiting directory reads nothing either.
    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Move(SidebarMotion::LastRow));
    assert_eq!(labels(&session), vec!["src", "readme.md"]);

    read_directories(&mut session);
    assert_eq!(
        labels(&session),
        vec!["src", "deep", "main.rs", "readme.md"]
    );
    assert_eq!(row_of(&session, "src").kind(), FileRowKind::OpenDirectory);
    assert_eq!(row_of(&session, "deep").depth(), 1);
}

#[test]
fn a_close_hides_every_entry_of_the_directory_again() {
    let directory = workspace("file-sidebar-close");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    select(&mut session, "src");
    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Open);
    read_directories(&mut session);
    assert_eq!(row_of(&session, "src").kind(), FileRowKind::OpenDirectory);

    select(&mut session, "src");
    assert_eq!(
        session.reduce_file_sidebar(FileSidebarInput::Close),
        FileSidebarOutcome::Applied
    );
    assert_eq!(labels(&session), vec!["src", "readme.md"]);
    assert_eq!(row_of(&session, "src").kind(), FileRowKind::ClosedDirectory);
}

#[test]
fn a_close_below_a_file_selects_the_directory_that_holds_it() {
    let directory = workspace("file-sidebar-close-parent");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    select(&mut session, "src");
    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Open);
    read_directories(&mut session);

    select(&mut session, "main.rs");
    assert_eq!(
        session.reduce_file_sidebar(FileSidebarInput::Close),
        FileSidebarOutcome::Applied
    );
    assert!(row_of(&session, "src").is_selected());
}

#[test]
fn an_activated_file_returns_its_contained_path_and_opens_no_buffer() {
    let directory = workspace("file-sidebar-activation");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    select(&mut session, "readme.md");
    let outcome = session.reduce_file_sidebar(FileSidebarInput::Activate);
    let FileSidebarOutcome::Activated { path } = outcome.clone() else {
        panic!("the reader activated one file: {outcome:?}");
    };
    assert_eq!(path.as_path(), Path::new("readme.md"));
    assert_eq!(
        outcome.event(),
        Some(EditorEvent::FileActivated { path: path.clone() })
    );

    // The sidebar opened no buffer, so the editor still shows no file.
    assert_eq!(session.active_buffer().path(), None);

    // The host decides the effect. Opening the file is its own call.
    let _redraw = session.open(path);
}

#[test]
fn the_open_input_activates_a_file_and_only_ever_opens_a_directory() {
    let directory = workspace("file-sidebar-open-input");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    select(&mut session, "src");
    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Open);
    read_directories(&mut session);
    // A second open on an open directory keeps it open.
    select(&mut session, "src");
    assert_eq!(
        session.reduce_file_sidebar(FileSidebarInput::Open),
        FileSidebarOutcome::Applied
    );
    assert_eq!(row_of(&session, "src").kind(), FileRowKind::OpenDirectory);

    select(&mut session, "main.rs");
    let outcome = session.reduce_file_sidebar(FileSidebarInput::Open);
    assert_eq!(
        outcome
            .activated()
            .map(kvim_path::WorktreeRelativePath::as_path),
        Some(Path::new("src/main.rs"))
    );
}

#[test]
fn every_row_carries_the_leading_blank_of_the_workspace_header() {
    let directory = workspace("file-sidebar-guides");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    select(&mut session, "src");
    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Open);
    read_directories(&mut session);

    // The header row of the workspace root is no sibling of the first entries,
    // so the shared rule draws no guide for them and the leading blank of the
    // file tree is the whole indent of a top-level row.
    assert_eq!(row_of(&session, "src").guides(), SIDEBAR_GUIDE_BLANK);
    assert_eq!(row_of(&session, "readme.md").guides(), SIDEBAR_GUIDE_BLANK);
    // Inside `src`, `deep` still has `main.rs` below it, and `main.rs` is the
    // last entry of that level, so it closes the guide.
    assert_eq!(
        row_of(&session, "deep").guides(),
        format!("{SIDEBAR_GUIDE_BLANK}{SIDEBAR_GUIDE_TRUNK}")
    );
    assert_eq!(
        row_of(&session, "main.rs").guides(),
        format!("{SIDEBAR_GUIDE_BLANK}{SIDEBAR_GUIDE_ELBOW}")
    );
}

#[test]
fn one_sidebar_input_latches_the_redraw_request_of_the_host() {
    let directory = workspace("file-sidebar-redraw");
    let mut session = editor(&directory.path);
    read_directories(&mut session);
    while session.take_event().is_some() {}

    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Move(SidebarMotion::Down(1)));
    let published = session
        .take_event()
        .expect("one sidebar input asks the host for one frame");
    assert_eq!(published.event, EditorEvent::RedrawRequested);
}

#[test]
fn a_label_longer_than_the_bound_reaches_the_host_clipped() {
    let name = "n".repeat(FILE_SIDEBAR_LABEL_CHARS_MAX + 8);
    let row = FileRow::new(
        name,
        SIDEBAR_GUIDE_BLANK.to_owned(),
        0,
        FileRowKind::File,
        false,
    );
    assert_eq!(row.label().chars().count(), FILE_SIDEBAR_LABEL_CHARS_MAX);
}

#[test]
fn a_note_row_reports_its_directory_and_takes_no_selection() {
    let directory = TempDir::new("file-sidebar-note");
    directory.file(".hidden", "one line\n");
    directory.file("kept.rs", "fn main() {}\n");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    let note = session
        .file_rows()
        .into_iter()
        .find(|row| row.kind() == FileRowKind::Note)
        .expect("the hidden-entry policy keeps one entry out of the rows");
    assert_eq!(note.label(), "(1 hidden item)");
    assert!(!note.is_selected());
    assert!(!note.kind().is_selectable());

    // The last row is the note, so the move stops on the entry above it.
    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Move(SidebarMotion::LastRow));
    assert!(row_of(&session, "kept.rs").is_selected());
}

#[test]
fn the_facade_reports_no_deadline_of_its_own() {
    let directory = workspace("file-sidebar-deadline");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    // The sidebar reads no clock, so its inputs arm no timer.
    let before = session.next_deadline();
    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Move(SidebarMotion::Down(1)));
    assert_eq!(session.next_deadline(), before);
    let _tick = session.tick(Duration::ZERO);
}
