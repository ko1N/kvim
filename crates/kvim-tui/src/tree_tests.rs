//! Tests for the file-tree sidebar: its rows, its keys, and its transitions.
//!
//! Every test drives one temporary workspace. The session performs no
//! filesystem work itself, so each test runs the queued workspace requests as
//! the event loop does.

use std::fs;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;

use kvim_input::Mode;
use kvim_settings::{EditorSettings, FileTreeIcons};
use kvim_terminal::{Key, KeyCode, TerminalEvent};
use kvim_workspace::{TREE_PENDING_READS_MAX, temp::TempDir};

use super::session::{FileRequestFailure, Session};
use super::tree::TREE_TITLE_ROWS;

const NOW: Duration = Duration::ZERO;

/// The terminal width of every test session.
const WIDTH: u16 = 80;

/// The terminal height of every test session.
const HEIGHT: u16 = 12;

/// The first column of the sidebar in a terminal of [`WIDTH`] cells.
const SIDEBAR_X: u16 = WIDTH - 40;

/// The largest number of workspace operations that one test drains.
///
/// One reveal or one refresh queues fewer reads than the queue bound of the
/// tree, so the value ends every drain loop.
const WORKSPACE_STEPS_MAX: usize = TREE_PENDING_READS_MAX;

/// Creates one workspace and one session over it.
///
/// The root is the canonical path of the temporary directory, so it matches
/// the path that a loaded buffer holds. The session hides the icons, so a row
/// assertion reads the structure of the tree alone. One test turns them on
/// again.
fn workspace() -> (TempDir, Session) {
    workspace_with_icons(FileTreeIcons::Hidden)
}

/// Creates one workspace and one session with the named icon setting.
fn workspace_with_icons(icons: FileTreeIcons) -> (TempDir, Session) {
    let dir = TempDir::new("tree");
    dir.file("src/main.rs", "fn main() {}\n");
    dir.file("README.md", "kvim\n");
    dir.file(".hidden", "secret\n");
    dir.dir("docs");
    let root = dir.path.clone();
    let mut settings = EditorSettings::default();
    settings.windows.file_tree_icons = icons;
    let mut session = Session::new(Rect::new(0, 0, WIDTH, HEIGHT), settings, root);
    drain(&mut session);
    (dir, session)
}

/// Runs every queued workspace operation, as the event loop does.
fn drain(session: &mut Session) {
    for _ in 0..WORKSPACE_STEPS_MAX {
        let Some(request) = session.take_workspace_request() else {
            return;
        };
        session.apply_workspace_result(request.run());
    }
    panic!("one transition queues fewer reads than the queue bound of the tree");
}

/// Runs the queued file operation, as the event loop does.
fn drain_file(session: &mut Session) {
    if let Some(request) = session.take_file_request() {
        session.apply_file_result(request.run());
    }
}

/// Feeds one plain character key.
fn press(session: &mut Session, value: char) {
    session.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char(value))), NOW);
}

/// Feeds one plain key without a character.
fn press_code(session: &mut Session, code: KeyCode) {
    session.handle_event(TerminalEvent::Key(Key::plain(code)), NOW);
}

/// Feeds one key with the Control chord.
fn press_ctrl(session: &mut Session, value: char) {
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char(value))), NOW);
}

/// Feeds a run of plain character keys.
fn type_keys(session: &mut Session, keys: &str) {
    for value in keys.chars() {
        press(session, value);
    }
}

/// Renders one session and returns the terminal cell buffer.
fn draw(session: &Session) -> CellBuffer {
    let backend = TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = Terminal::new(backend).expect("the test backend never fails");
    terminal
        .draw(|frame| session.render(frame))
        .expect("the test backend never fails");
    terminal.backend().buffer().clone()
}

/// Returns one sidebar row as text, without trailing blanks.
fn sidebar_row(session: &Session, row: u16) -> String {
    let buffer = draw(session);
    let mut text = String::new();
    for x in SIDEBAR_X..WIDTH {
        if let Some(cell) = buffer.cell((x, row)) {
            text.push_str(cell.symbol());
        }
    }
    text.trim_end().to_owned()
}

/// Returns every sidebar row below the title, without the empty rows.
fn sidebar_rows(session: &Session) -> Vec<String> {
    (1..HEIGHT)
        .map(|row| sidebar_row(session, row))
        .take_while(|text| !text.is_empty())
        .collect()
}

/// Returns the message of the session as text.
///
/// A message that holds a path needs this entry point instead of
/// [`message_line`], because the message line paints one terminal row and drops
/// every character behind it. The length of a temporary path is a property of
/// the host, so a rendered assertion over such a message would report the
/// ambient temporary directory instead of the transition.
fn message(session: &Session) -> String {
    session
        .message()
        .map_or_else(String::new, |message| message.text().to_owned())
}

/// Returns the message line as text.
fn message_line(session: &Session) -> String {
    let buffer = draw(session);
    let mut text = String::new();
    for x in 0..WIDTH {
        if let Some(cell) = buffer.cell((x, HEIGHT - 1)) {
            text.push_str(cell.symbol());
        }
    }
    text.trim_end().to_owned()
}

/// Opens the sidebar over the active file and applies every queued read.
fn reveal(session: &mut Session) {
    press_ctrl(session, 'e');
    drain(session);
}

/// The glyph that marks one row after the last line of a buffer window.
const END_OF_BUFFER_GLYPH: &str = "~";

#[test]
fn the_sidebar_marks_no_row_after_its_last_entry() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);
    let buffer = draw(&session);

    // The buffer window beside the sidebar marks its own empty rows, so the
    // frame proves that the sidebar leaves those rows alone by choice.
    let marked = (1..HEIGHT - 2).any(|row| {
        buffer
            .cell((0, row))
            .is_some_and(|cell| cell.symbol() == END_OF_BUFFER_GLYPH)
    });
    assert!(
        marked,
        "the buffer window marks its rows after the last line"
    );

    // The title row names the workspace root, which may itself hold the glyph
    // as the abbreviation of the home directory, so the scan starts below it.
    for row in TREE_TITLE_ROWS..HEIGHT {
        for column in SIDEBAR_X..WIDTH {
            let symbol = buffer
                .cell((column, row))
                .expect("the test reads a cell inside the terminal")
                .symbol()
                .to_owned();
            assert_ne!(
                symbol, END_OF_BUFFER_GLYPH,
                "the sidebar row {row} shows no end-of-buffer marker"
            );
        }
    }
}

#[test]
fn ctrl_e_opens_the_sidebar_and_shows_the_ordered_rows() {
    let (_dir, mut session) = workspace();
    assert!(
        sidebar_row(&session, 1).is_empty(),
        "the sidebar stays closed until the reveal opens it"
    );

    reveal(&mut session);

    // A directory sorts before a file, and two entries of one kind sort by
    // name. The hidden entry stays out of the rows.
    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▸ docs".to_owned(),
            "▸ src".to_owned(),
            "  README.md".to_owned(),
        ]
    );
}

#[test]
fn a_reveal_expands_the_ancestors_and_selects_the_active_file() {
    let (dir, mut session) = workspace();
    let path = dir.join("src/main.rs");
    session.open_path(path.clone());
    drain_file(&mut session);

    reveal(&mut session);

    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▸ docs".to_owned(),
            "▾ src".to_owned(),
            "    main.rs".to_owned(),
            "  README.md".to_owned(),
        ],
        "the reveal expands every parent and indents the child"
    );
    let selection = draw(&session);
    let row = selection
        .cell((SIDEBAR_X, 3))
        .expect("the sidebar shows the revealed row");
    let plain = selection
        .cell((SIDEBAR_X, 1))
        .expect("the sidebar shows the first row");
    assert_ne!(
        row.style().bg,
        plain.style().bg,
        "the selected row carries the selection style"
    );
}

#[test]
fn an_unreadable_directory_reports_a_notice_row() {
    let (dir, mut session) = workspace();
    reveal(&mut session);

    // The selection starts on the first directory, which the test removes
    // between the expansion and the read.
    press(&mut session, ' ');
    fs::remove_dir_all(dir.join("docs")).expect("the temporary directory is writable");
    drain(&mut session);

    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▾ docs".to_owned(),
            "    … unreadable".to_owned(),
            "▸ src".to_owned(),
            "  README.md".to_owned(),
        ]
    );
}

#[test]
fn the_navigation_keys_move_the_selection() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    press(&mut session, 'j');
    press(&mut session, 'j');
    assert!(
        selected(&session).ends_with("README.md"),
        "two steps down reach the third row"
    );
    press(&mut session, 'k');
    assert!(selected(&session).ends_with("src"));

    // The parent of an entry at the workspace root is the root itself, which
    // holds no row, so the selection stays.
    press_code(&mut session, KeyCode::Backspace);
    assert!(selected(&session).ends_with("src"));
}

/// Returns the selected path of the tree as text.
fn selected(session: &Session) -> String {
    session
        .file_tree()
        .selected()
        .expect("the tree shows one selected row")
        .display()
        .to_string()
}

#[test]
fn space_expands_a_directory_and_enter_opens_a_file() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    press(&mut session, 'j');
    press(&mut session, ' ');
    drain(&mut session);
    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▸ docs".to_owned(),
            "▾ src".to_owned(),
            "    main.rs".to_owned(),
            "  README.md".to_owned(),
        ]
    );

    press(&mut session, 'j');
    press_code(&mut session, KeyCode::Enter);
    drain_file(&mut session);
    assert_eq!(session.buffer().to_string(), "fn main() {}\n");
    // The focus follows the opened file, so the next key edits the buffer.
    press(&mut session, 'i');
    assert_eq!(session.mode(), Mode::Insert);
}

#[test]
fn the_hidden_key_shows_and_hides_the_dotfiles() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    press(&mut session, 'H');
    assert!(
        sidebar_rows(&session).contains(&"  .hidden".to_owned()),
        "the hidden entry appears once"
    );
    press(&mut session, 'H');
    assert!(!sidebar_rows(&session).contains(&"  .hidden".to_owned()));
}

#[test]
fn the_refresh_key_reads_the_workspace_again() {
    let (dir, mut session) = workspace();
    reveal(&mut session);
    dir.file("later.rs", "\n");

    press(&mut session, 'R');
    drain(&mut session);

    assert!(sidebar_rows(&session).contains(&"  later.rs".to_owned()));
}

#[test]
fn the_text_operations_open_the_prompt_of_the_message_line() {
    let cases = [
        ('a', "new file: "),
        ('A', "new directory: "),
        ('r', "rename: "),
        ('/', "filter: "),
    ];
    for (key, prefix) in cases {
        let (_dir, mut session) = workspace();
        reveal(&mut session);

        press(&mut session, key);
        assert_eq!(
            message_line(&session),
            prefix.trim_end(),
            "`{key}` must open its prompt"
        );
        type_keys(&mut session, "x");
        assert_eq!(message_line(&session), format!("{prefix}x"));
    }
}

#[test]
fn a_new_file_reaches_the_workspace_and_the_tree_selects_it() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    // The selected file names the destination directory, which is the root.
    press(&mut session, 'j');
    press(&mut session, 'j');
    press(&mut session, 'a');
    type_keys(&mut session, "added.rs");
    press_code(&mut session, KeyCode::Enter);
    drain(&mut session);

    assert!(sidebar_rows(&session).contains(&"  added.rs".to_owned()));
    assert!(selected(&session).ends_with("added.rs"));
}

#[test]
fn a_rename_applies_the_buffer_path_and_the_tree_as_one_transition() {
    let (dir, mut session) = workspace();
    let path = dir.join("README.md");
    session.open_path(path);
    drain_file(&mut session);
    reveal(&mut session);

    press(&mut session, 'r');
    type_keys(&mut session, "GUIDE.md");
    press_code(&mut session, KeyCode::Enter);
    drain(&mut session);

    assert_eq!(
        session.active_buffer().name(),
        "GUIDE.md",
        "the buffer follows the entry and keeps its identity"
    );
    assert!(sidebar_rows(&session).contains(&"  GUIDE.md".to_owned()));
    assert!(selected(&session).ends_with("GUIDE.md"));
    assert!(
        dir.join("GUIDE.md").exists() && !dir.join("README.md").exists(),
        "the workspace holds the renamed entry alone"
    );
}

#[test]
fn a_copy_and_a_paste_move_the_entry_into_the_selected_directory() {
    let (dir, mut session) = workspace();
    reveal(&mut session);

    // Hold the file of the workspace root, then paste it into `docs`.
    press(&mut session, 'j');
    press(&mut session, 'j');
    press(&mut session, 'y');
    assert_eq!(
        message(&session),
        format!(
            "{} is copied for the next paste",
            dir.join("README.md").display()
        )
    );
    press(&mut session, 'k');
    press(&mut session, 'k');
    press(&mut session, 'p');
    drain(&mut session);

    assert!(dir.join("docs/README.md").exists());
    assert!(
        dir.join("README.md").exists(),
        "a copy leaves the source in place"
    );
}

#[test]
fn a_delete_removes_the_selected_entry() {
    let (dir, mut session) = workspace();
    reveal(&mut session);

    press(&mut session, 'j');
    press(&mut session, 'j');
    press(&mut session, 'd');
    drain(&mut session);

    assert!(!dir.join("README.md").exists());
    assert!(!sidebar_rows(&session).contains(&"  README.md".to_owned()));
}

#[test]
fn a_refused_mutation_reports_it_and_changes_nothing() {
    let (dir, mut session) = workspace();
    reveal(&mut session);

    press(&mut session, 'j');
    press(&mut session, 'j');
    press(&mut session, 'a');
    type_keys(&mut session, "src");
    press_code(&mut session, KeyCode::Enter);
    drain(&mut session);

    assert_eq!(
        message(&session),
        format!("{} exists already", dir.join("src").display()),
        "a destination collision reports the typed rejection"
    );
    assert!(dir.join("src").is_dir());
}

#[test]
fn a_name_that_holds_a_path_is_refused_before_the_worker_runs() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    press(&mut session, 'a');
    type_keys(&mut session, "nested/file.rs");
    press_code(&mut session, KeyCode::Enter);

    assert_eq!(
        message_line(&session),
        "the name must hold one entry name, not a path"
    );
    assert!(
        session.take_workspace_request().is_none(),
        "a refused name reaches no worker"
    );
}

#[test]
fn closing_the_sidebar_returns_the_focus_to_the_editor() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);
    assert!(!sidebar_row(&session, 1).is_empty());

    press(&mut session, 'q');
    assert!(
        sidebar_row(&session, 1).is_empty(),
        "the closed sidebar shows no row"
    );
    press(&mut session, 'i');
    type_keys(&mut session, "x");
    assert_eq!(
        session.buffer().to_string(),
        "x",
        "the editor owns the keys again"
    );
}

#[test]
fn a_second_ctrl_e_closes_the_sidebar() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    press_ctrl(&mut session, 'e');

    assert!(sidebar_row(&session, 1).is_empty());
    press(&mut session, 'i');
    assert_eq!(session.mode(), Mode::Insert);
}

#[test]
fn a_filter_narrows_the_rows_to_the_matching_names() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    press(&mut session, '/');
    type_keys(&mut session, "read");
    press_code(&mut session, KeyCode::Enter);

    assert_eq!(sidebar_rows(&session), vec!["  README.md".to_owned()]);
}

#[test]
fn the_sidebar_scrolls_so_the_selected_row_stays_visible() {
    let dir = TempDir::new("tree-scroll");
    // The workspace holds more files than the sidebar shows at once.
    for index in 0..20 {
        dir.file(&format!("file{index:02}.rs"), "\n");
    }
    let root = dir.path.clone();
    let mut settings = EditorSettings::default();
    settings.windows.file_tree_icons = FileTreeIcons::Hidden;
    let mut session = Session::new(Rect::new(0, 0, WIDTH, HEIGHT), settings, root);
    drain(&mut session);
    reveal(&mut session);

    for _ in 0..19 {
        press(&mut session, 'j');
    }

    let rows = sidebar_rows(&session);
    assert!(
        rows.contains(&"  file19.rs".to_owned()),
        "the selected row stays inside the sidebar"
    );
    assert!(
        !rows.contains(&"  file00.rs".to_owned()),
        "the rows above the visible window scroll away"
    );
}

#[test]
fn a_workspace_failure_keeps_the_tree_usable() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);
    let before = sidebar_rows(&session);

    press(&mut session, 'R');
    let _request = session
        .take_workspace_request()
        .expect("the refresh queues one read");
    session.abandon_workspace_request(super::session::FileRequestFailure::Timeout);

    assert_eq!(sidebar_rows(&session), before);
    assert_eq!(
        message_line(&session),
        "the file operation passed its deadline"
    );
}

#[test]
fn the_tree_reads_no_directory_on_the_event_loop() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    // Every read reaches the worker as one request, so a fresh refresh leaves
    // one queued request behind instead of a completed listing.
    press(&mut session, 'R');
    let request = session.take_workspace_request();
    assert!(
        matches!(
            request,
            Some(kvim_workspace::WorkspaceRequest::ReadDirectory { .. })
        ),
        "the refresh hands the read to the bounded worker service"
    );
}

/// Returns the name of the selected entry, or an empty text while none is
/// selected.
fn selected_name(session: &Session) -> String {
    session
        .file_tree()
        .selected()
        .map_or_else(String::new, |path| {
            path.file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
        })
}

#[test]
fn l_expands_a_directory_and_h_collapses_it_or_selects_the_parent() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    // The first read selects the first row, which is the `docs` directory.
    assert_eq!(selected_name(&session), "docs");

    // `l` opens the selected directory, and it never closes one.
    press(&mut session, 'l');
    drain(&mut session);
    assert_eq!(sidebar_row(&session, 1), "▾ docs");
    press(&mut session, 'l');
    drain(&mut session);
    assert_eq!(
        sidebar_row(&session, 1),
        "▾ docs",
        "`l` on an open directory keeps it open"
    );

    // `h` closes an open directory.
    press(&mut session, 'h');
    drain(&mut session);
    assert_eq!(sidebar_row(&session, 1), "▸ docs");
    assert_eq!(selected_name(&session), "docs");

    // `h` on a closed directory leaves for the parent. The workspace root holds
    // no row, so the selection stays.
    press(&mut session, 'h');
    assert_eq!(selected_name(&session), "docs");

    // `h` from a file selects the directory that holds it, and the next `h`
    // closes that directory.
    press(&mut session, 'j');
    assert_eq!(selected_name(&session), "src");
    press(&mut session, 'l');
    drain(&mut session);
    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▸ docs".to_owned(),
            "▾ src".to_owned(),
            "    main.rs".to_owned(),
            "  README.md".to_owned(),
        ]
    );
    press(&mut session, 'j');
    assert_eq!(selected_name(&session), "main.rs");
    press(&mut session, 'h');
    assert_eq!(selected_name(&session), "src");
    press(&mut session, 'h');
    drain(&mut session);
    assert_eq!(sidebar_row(&session, 2), "▸ src");
}

#[test]
fn l_on_a_file_opens_it_in_the_editor_window() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    // The third row holds `README.md`, and the first read selected the first.
    type_keys(&mut session, "jj");
    assert_eq!(selected_name(&session), "README.md");
    press(&mut session, 'l');
    drain_file(&mut session);

    assert_eq!(session.active_buffer().name(), "README.md");
    assert_eq!(
        session.mode(),
        Mode::Normal,
        "the focus leaves the sidebar for the editor window"
    );
}

#[test]
fn the_tree_paints_one_icon_for_each_entry_and_hides_them_on_request() {
    assert_eq!(
        EditorSettings::default().windows.file_tree_icons,
        FileTreeIcons::Shown,
        "the reference configuration installs a patched font"
    );

    let (_dir, mut session) = workspace_with_icons(FileTreeIcons::Shown);
    reveal(&mut session);
    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▸ \u{f07b} docs".to_owned(),
            "▸ \u{f07b} src".to_owned(),
            "  \u{f48a} README.md".to_owned(),
        ],
        "a closed directory and a known extension each carry their icon"
    );

    // An open directory carries its own icon, and every name of one depth still
    // starts at one column.
    press(&mut session, 'j');
    press(&mut session, 'l');
    drain(&mut session);
    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▸ \u{f07b} docs".to_owned(),
            "▾ \u{f07c} src".to_owned(),
            "    \u{e7a8} main.rs".to_owned(),
            "  \u{f48a} README.md".to_owned(),
        ]
    );

    // The same tree without icons keeps the names aligned.
    let (_other_dir, mut plain) = workspace_with_icons(FileTreeIcons::Hidden);
    reveal(&mut plain);
    assert_eq!(
        sidebar_rows(&plain),
        vec![
            "▸ docs".to_owned(),
            "▸ src".to_owned(),
            "  README.md".to_owned(),
        ]
    );
}

/// Runs the queued workspace operations and returns the number of steps.
///
/// The loop is bounded, as the event loop is. It reports the steps, so a test
/// can assert that the queue makes progress instead of offering one read
/// forever.
fn drain_counted(session: &mut Session) -> usize {
    for step in 0..WORKSPACE_STEPS_MAX {
        let Some(request) = session.take_workspace_request() else {
            return step;
        };
        session.apply_workspace_result(request.run());
    }
    panic!("the workspace queue offered a read at every one of the bounded steps");
}

#[test]
fn the_workspace_queue_terminates_after_a_failed_read() {
    let (dir, mut session) = workspace();
    reveal(&mut session);

    // The directory disappears between the expansion and the read, so the read
    // fails. The queue must still empty.
    press(&mut session, 'l');
    let request = session
        .take_workspace_request()
        .expect("the expansion queued one directory read");
    fs::remove_dir_all(dir.join("docs")).expect("the test workspace holds the directory");
    session.apply_workspace_result(request.run());
    assert_eq!(
        drain_counted(&mut session),
        0,
        "a failed read queues no further read"
    );
    press(&mut session, 'R');
    assert!(
        drain_counted(&mut session) > 0,
        "a refresh still reads the remaining directories"
    );
}

#[test]
fn a_refused_submission_leaves_the_tree_usable() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    press(&mut session, 'R');
    let refused = session
        .take_workspace_request()
        .expect("the refresh queued one read");
    drop(refused);
    // The bounded worker service refused the request, so the tree clears its
    // pending state instead of waiting for a result that never arrives.
    session.abandon_workspace_request(FileRequestFailure::Saturated);
    assert_eq!(
        drain_counted(&mut session),
        0,
        "the refused read left no operation behind"
    );

    // The tree accepts the next operation, so one refusal never blocks it.
    press(&mut session, 'R');
    assert!(
        drain_counted(&mut session) > 0,
        "a later refresh still reads the workspace"
    );
    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▸ docs".to_owned(),
            "▸ src".to_owned(),
            "  README.md".to_owned(),
        ]
    );
}

#[test]
fn an_obsolete_result_leaves_the_queue_usable() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    // A result that reaches no pending read is obsolete. The publication gate
    // drops it in the event loop, and the tree must stay usable either way.
    press(&mut session, 'R');
    let request = session
        .take_workspace_request()
        .expect("the refresh queued one read");
    let result = request.run();
    session.abandon_workspace_request(FileRequestFailure::Cancelled);
    session.apply_workspace_result(result);
    assert!(
        drain_counted(&mut session) < WORKSPACE_STEPS_MAX,
        "the queue still empties after an obsolete result"
    );
    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▸ docs".to_owned(),
            "▸ src".to_owned(),
            "  README.md".to_owned(),
        ],
        "the tree keeps its rows"
    );
}
