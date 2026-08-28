//! Tests for the file sidebar that one embedded host draws.
//!
//! Every test drives one host-owned editor. No test opens a terminal and no
//! test lets the editor reach the filesystem by itself: a directory read leaves
//! the editor as one [`WorkspaceRequest`], and the test runs it exactly where a
//! host runs it, off the event loop. See `docs/embedding.md`.

use std::path::Path;
use std::time::Duration;

use kvim_path::{WorktreeDirectoryPath, WorktreeRelativePath};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use kvim_runtime::ProcessOutput;
use kvim_settings::EditorSettings;
use kvim_ui::{
    ListMotion, RowKind, SIDEBAR_GUIDE_BLANK, SIDEBAR_GUIDE_ELBOW, SIDEBAR_GUIDE_TRUNK, SidebarRow,
    SidebarState,
};
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

/// Paints one published row through the shared standalone and embedded painter.
fn paint_row(row: &FileRow, width: u16) -> Buffer {
    let area = Rect::new(0, 0, width, 1);
    let mut buffer = Buffer::empty(area);
    let mut sidebar = SidebarState::new(1);
    sidebar
        .set_rows(vec![SidebarRow::single(0, RowKind::Selectable)])
        .expect("one row stays inside the sidebar bound");
    sidebar
        .render(&mut buffer, area, |canvas, _placement| {
            draw_file_row(
                canvas,
                row,
                Theme::new(),
                FileTreeIcons::Hidden,
                RegionFocus::Focused,
            );
        })
        .expect("the painter stays inside one row");
    buffer
}

/// Selects the row that carries one label.
fn select(session: &mut Session, label: &str) {
    let row = session
        .file_rows()
        .iter()
        .position(|row| row.label() == label)
        .unwrap_or_else(|| panic!("the sidebar shows a row named {label}"));
    let outcome = session.reduce_file_sidebar(FileSidebarInput::Move(ListMotion::ToRow(row)));
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

/// Creates one workspace whose entries cover every recorded Git state.
fn git_state_workspace(name: &str) -> TempDir {
    let directory = TempDir::new(name);
    directory.file("staged.rs", "");
    directory.file("modified.rs", "");
    directory.file("staged-and-modified.rs", "");
    directory.file("new.rs", "");
    directory.file("notes.log", "");
    directory.file("conflict.rs", "");
    directory
}

/// Returns one ordinary record of `git status --porcelain=v2 -z`.
fn git_record(field: &str, path: &str) -> String {
    format!(
        "1 {field} N... 100644 100644 100644 \
         78981922613b2afb6025042ff6bd878ac1994e85 \
         78981922613b2afb6025042ff6bd878ac1994e85 {path}\0"
    )
}

/// Returns the recorded status output that covers every Git state that
/// [`git_state_workspace`] builds.
fn git_state_output() -> String {
    format!(
        "{}{}{}? new.rs\0! notes.log\0u UU N... 100644 100644 100644 100644 \
         aa bb cc conflict.rs\0",
        git_record("M.", "staged.rs"),
        git_record(".M", "modified.rs"),
        git_record("MM", "staged-and-modified.rs"),
    )
}

/// Answers one queued command of one Git status read, as the event loop does.
fn answer_git(session: &mut Session, stdout: &[u8]) {
    let request = session
        .take_git_request()
        .expect("the sidebar queues one Git status read");
    let output = ProcessOutput {
        status_code: Some(0),
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    };
    let _ = session.apply_git_result(request.publish(&output));
}

/// Publishes one recorded status output, as the event loop does.
///
/// The read takes two commands: the first learns the place of the workspace
/// root inside its repository, and the workspace root of every test is its own
/// repository top level, so the first answer reports an empty prefix.
fn publish_git(session: &mut Session, output: &str) {
    answer_git(session, b"\n");
    answer_git(session, output.as_bytes());
}

#[test]
fn the_first_listing_reaches_the_tree_as_work_and_never_as_a_read_of_the_facade() {
    let directory = workspace("file-sidebar-first-read");
    let mut session = editor(&directory.path);

    // The tree opens with one pending read. Every facade call runs before that
    // read, so a facade that read the directory itself would show rows here.
    assert!(session.file_rows().is_empty());
    assert_eq!(
        session.reduce_file_sidebar(FileSidebarInput::Move(ListMotion::Down(1))),
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
    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Move(ListMotion::LastRow));
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

    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Move(ListMotion::Down(1)));
    let published = session
        .take_event()
        .expect("one sidebar input asks the host for one frame");
    assert_eq!(published.event, EditorEvent::RedrawRequested);
}

#[test]
fn a_label_longer_than_the_bound_reaches_the_host_clipped() {
    let name = "n".repeat(FILE_SIDEBAR_LABEL_CHARS_MAX + 8);
    let row = FileRow::new(
        FileRowIdentity::Entry(WorktreeRelativePath::new("long").unwrap()),
        WorktreeRelativePath::new("long").ok(),
        name,
        SIDEBAR_GUIDE_BLANK.to_owned(),
        0,
        FileRowKind::File,
        RowState::File,
    );
    assert_eq!(row.label().chars().count(), FILE_SIDEBAR_LABEL_CHARS_MAX);
}

#[test]
fn clipped_row_text_fades_before_the_fixed_git_mark() {
    let row = FileRow::new(
        FileRowIdentity::Entry(WorktreeRelativePath::new("a-very-long-name.rs").unwrap()),
        WorktreeRelativePath::new("a-very-long-name.rs").ok(),
        "a-very-long-name.rs".to_owned(),
        SIDEBAR_GUIDE_BLANK.to_owned(),
        0,
        FileRowKind::File,
        RowState::File,
        false,
    )
    .with_git(Some(FileRowGit::Modified));
    let buffer = paint_row(&row, 12);
    let theme = Theme::new();
    let normal = theme
        .style(ThemeRole::Text)
        .patch(theme.style(ThemeRole::TreeGit(FileRowGit::Modified)));

    assert_eq!(buffer.cell((11, 0)).map(|cell| cell.symbol()), Some("●"));
    assert_eq!(buffer.cell((11, 0)).map(|cell| cell.fg), normal.fg);
    let faded: Vec<Color> = (8..11)
        .map(|column| {
            buffer
                .cell((column, 0))
                .expect("the fade cell is inside the row")
                .fg
        })
        .collect();
    assert_ne!(faded[0], normal.fg.expect("text has a foreground"));
    assert_ne!(faded[0], faded[1]);
    assert_ne!(faded[1], faded[2]);
}

#[test]
fn clipped_wide_text_fades_both_cells_without_moving_the_git_mark() {
    let row = FileRow::new(
        FileRowIdentity::Entry(WorktreeRelativePath::new("ab漢tail").unwrap()),
        WorktreeRelativePath::new("ab漢tail").ok(),
        "ab漢tail".to_owned(),
        SIDEBAR_GUIDE_BLANK.to_owned(),
        0,
        FileRowKind::File,
        RowState::File,
        false,
    )
    .with_git(Some(FileRowGit::Staged));
    let buffer = paint_row(&row, 11);

    assert_eq!(buffer.cell((7, 0)).map(|cell| cell.symbol()), Some("漢"));
    assert_eq!(buffer.cell((8, 0)).map(|cell| cell.symbol()), Some(" "));
    assert_eq!(
        buffer.cell((7, 0)).map(|cell| cell.fg),
        buffer.cell((8, 0)).map(|cell| cell.fg)
    );
    assert_eq!(buffer.cell((10, 0)).map(|cell| cell.symbol()), Some("■"));
}

#[test]
fn short_row_text_keeps_its_style_and_a_selected_fade_keeps_its_background() {
    let plain = FileRow::new(
        FileRowIdentity::Entry(WorktreeRelativePath::new("short.rs").unwrap()),
        WorktreeRelativePath::new("short.rs").ok(),
        "short.rs".to_owned(),
        SIDEBAR_GUIDE_BLANK.to_owned(),
        0,
        FileRowKind::File,
        RowState::File,
        false,
    );
    let plain_buffer = paint_row(&plain, 20);
    let plain_style = Theme::new().style(ThemeRole::Text);
    assert_eq!(
        plain_buffer.cell((7, 0)).map(|cell| cell.fg),
        plain_style.fg
    );

    let selected = FileRow::new(
        FileRowIdentity::Entry(WorktreeRelativePath::new("a-very-long-name.rs").unwrap()),
        WorktreeRelativePath::new("a-very-long-name.rs").ok(),
        "a-very-long-name.rs".to_owned(),
        SIDEBAR_GUIDE_BLANK.to_owned(),
        0,
        FileRowKind::File,
        RowState::File,
        true,
    );
    let selected_buffer = paint_row(&selected, 12);
    let selected_style = plain_style.patch(Theme::new().style(ThemeRole::PopupSelection));
    for column in 8..11 {
        assert_eq!(
            selected_buffer.cell((column, 0)).map(|cell| cell.bg),
            selected_style.bg
        );
    }
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
    // A note row names no entry, so it carries no Git state, no link, and no
    // icon role.
    assert_eq!(note.git(), None);
    assert!(!note.is_symlink());
    assert_eq!(note.icon_role(), None);
    assert_eq!(
        note.identity(),
        &FileRowIdentity::Notice {
            parent: None,
            kind: FileRowNoticeKind::Hidden,
        }
    );

    // The last row is the note, so the move stops on the entry above it.
    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Move(ListMotion::LastRow));
    assert!(row_of(&session, "kept.rs").is_selected());
}

#[test]
fn refresh_queues_each_expanded_directory_and_one_git_read() {
    let directory = workspace("file-sidebar-refresh");
    let mut session = editor(&directory.path);
    read_directories(&mut session);
    let _opening_git = session.take_git_request();

    select(&mut session, "src");
    let _ = session.reduce_file_sidebar(FileSidebarInput::Open);
    read_directories(&mut session);
    assert!(session.take_workspace_request().is_none());

    let _ = session.reduce_file_sidebar(FileSidebarInput::Refresh);
    let mut refreshed = Vec::new();
    for _ in 0..READS_MAX {
        let Some(request) = session.take_workspace_request() else {
            break;
        };
        let WorkspaceRequest::ReadDirectory { path, .. } = &request else {
            panic!("refresh queues directory reads alone");
        };
        refreshed.push(path.clone());
        let _ = session.apply_workspace_result(request.run());
    }
    assert_eq!(
        refreshed,
        vec![
            WorktreeDirectoryPath::Root,
            WorktreeDirectoryPath::Relative(WorktreeRelativePath::new("src").unwrap()),
        ]
    );
    assert!(session.take_git_request().is_some());
    assert!(session.take_git_request().is_none());
}

#[test]
fn a_row_names_every_recorded_git_state() {
    let directory = git_state_workspace("file-sidebar-git-state");
    let mut session = editor(&directory.path);
    read_directories(&mut session);
    publish_git(&mut session, &git_state_output());

    assert_eq!(
        row_of(&session, "staged.rs").git(),
        Some(FileRowGit::Staged)
    );
    assert_eq!(
        row_of(&session, "modified.rs").git(),
        Some(FileRowGit::Modified)
    );
    assert_eq!(
        row_of(&session, "staged-and-modified.rs").git(),
        Some(FileRowGit::StagedAndModified)
    );
    assert_eq!(
        row_of(&session, "new.rs").git(),
        Some(FileRowGit::Untracked)
    );
    assert_eq!(
        row_of(&session, "notes.log").git(),
        Some(FileRowGit::Ignored)
    );
    assert_eq!(
        row_of(&session, "conflict.rs").git(),
        Some(FileRowGit::Conflicted)
    );
}

#[test]
fn a_row_names_no_git_state_before_a_read_publishes_one() {
    let directory = workspace("file-sidebar-git-none");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    assert_eq!(row_of(&session, "readme.md").git(), None);
}

#[test]
fn a_symbolic_link_row_names_its_link() {
    use std::os::unix::fs::symlink;

    let directory = workspace("file-sidebar-symlink");
    symlink(
        directory.join("readme.md"),
        directory.join("readme-link.md"),
    )
    .expect("the temporary directory supports links");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    assert!(row_of(&session, "readme-link.md").is_symlink());
    assert!(!row_of(&session, "readme.md").is_symlink());
}

#[test]
fn a_directory_row_and_a_file_row_name_their_icon_roles() {
    let directory = workspace("file-sidebar-icon-role");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    assert_eq!(
        row_of(&session, "src").icon_role(),
        Some(IconRole::Directory)
    );
    assert_eq!(
        row_of(&session, "readme.md").icon_role(),
        Some(IconRole::Document)
    );
}

#[test]
fn the_facade_reports_no_deadline_of_its_own() {
    let directory = workspace("file-sidebar-deadline");
    let mut session = editor(&directory.path);
    read_directories(&mut session);

    // The sidebar reads no clock, so its inputs arm no timer.
    let before = session.next_deadline();
    let _outcome = session.reduce_file_sidebar(FileSidebarInput::Move(ListMotion::Down(1)));
    assert_eq!(session.next_deadline(), before);
    let _tick = session.tick(Duration::ZERO);
}

#[test]
fn a_host_reserving_the_published_mark_width_reaches_the_tree_label_offset() {
    // A host that reserves `FILE_SIDEBAR_MARK_CELLS` for its own mark column,
    // then one guide of `SIDEBAR_GUIDE_INDENT_CELLS` per depth level, and
    // `FILE_SIDEBAR_ICON_CELLS` for its own icon column, starts its label at
    // the column that `label_offset_cells` names, because the file tree of
    // kvim reserves the same three widths in the same order.
    for depth in 0..4_usize {
        let host_offset = FILE_SIDEBAR_MARK_CELLS
            + SIDEBAR_GUIDE_INDENT_CELLS * (depth + 1)
            + FILE_SIDEBAR_ICON_CELLS;
        assert_eq!(host_offset, label_offset_cells(depth));
    }
}
