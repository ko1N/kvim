//! Tests for the file-tree sidebar: its rows, its keys, and its transitions.
//!
//! Every test drives one temporary workspace. The session performs no
//! filesystem work itself, so each test runs the queued workspace requests as
//! the event loop does.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use kvim_input::{BindingScope, Mode};
use kvim_language::LspError;
use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{
    FileWatcher, ProcessOutput, WATCH_COALESCE_WINDOW, WatchBatch, WatchCoverage, WatchEvent,
    WatchFidelity, WatchKind,
};
use kvim_settings::{EditorSettings, FileTreeIcons};
use kvim_terminal::{Key, KeyCode, TerminalEvent};
use kvim_ui::{
    SIDEBAR_GUIDE_BLANK, SIDEBAR_GUIDE_ELBOW, SIDEBAR_GUIDE_INDENT_CELLS, SIDEBAR_GUIDE_TRUNK,
};
use kvim_workspace::{
    EntryKind, GIT_PROGRAM, GitStatus, GitStatusFailure, GitStatusRead, GitStatusRequest, LinkKind,
    Notice, RowContent, TREE_PENDING_READS_MAX, TakenDestination, TransferMode, TreeRow,
    temp::TempDir,
};

use crate::file_sidebar::FileRowGit;
use crate::session::{FileRequestFailure, Redraw, Session, test_root, watch_coverage_note};
use crate::theme::{Theme, ThemeRole};
use crate::tree::{
    GENERATED_NAMES, MARK_CELLS, RowState, TREE_TITLE_ROWS, delete_question, overwrite_question,
    root_label,
};

const NOW: Duration = Duration::ZERO;

/// The terminal width of every test session.
const WIDTH: u16 = 80;

/// The terminal height of every test session.
const HEIGHT: u16 = 12;

/// The first column of the sidebar in a terminal of [`WIDTH`] cells.
const SIDEBAR_X: u16 = WIDTH - 40;

/// The mark that the sidebar paints at the left edge of the selected row.
const ROW_MARK: &str = "▌";

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
    let mut session = Session::new(Rect::new(0, 0, WIDTH, HEIGHT), settings, test_root(root));
    drain(&mut session);
    (dir, session)
}

/// Runs every queued workspace operation, as the event loop does.
fn drain(session: &mut Session) {
    for _ in 0..WORKSPACE_STEPS_MAX {
        let Some(request) = session.take_workspace_request() else {
            return;
        };
        let _ = session.apply_workspace_result(request.run());
    }
    panic!("one transition queues fewer reads than the queue bound of the tree");
}

/// Runs the queued file operation, as the event loop does.
fn drain_file(session: &mut Session) {
    if let Some(request) = session.take_file_request() {
        let _ = session.apply_file_result(request.run());
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

/// Answers the open question with one typed text and `Enter`.
///
/// The question reads the text only when `Enter` closes it, so one keypress
/// performs no action. See `docs/input-actions.md`.
fn answer(session: &mut Session, text: &str) {
    type_keys(session, text);
    press_code(session, KeyCode::Enter);
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
            "▌  ▸ docs".to_owned(),
            "   ▸ src".to_owned(),
            "     README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
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
            "   ▸ docs".to_owned(),
            "   ▾ src".to_owned(),
            "▌  └   main.rs".to_owned(),
            "     README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
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
    press(&mut session, 'l');
    fs::remove_dir_all(dir.join("docs")).expect("the temporary directory is writable");
    drain(&mut session);

    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▌  ▾ docs".to_owned(),
            "   └   … unreadable".to_owned(),
            "   ▸ src".to_owned(),
            "     README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
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
    press(&mut session, 'l');
    drain(&mut session);
    assert_eq!(
        sidebar_rows(&session),
        vec![
            "   ▸ docs".to_owned(),
            "▌  ▾ src".to_owned(),
            "   └   main.rs".to_owned(),
            "     README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
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
        sidebar_rows(&session).contains(&"     .hidden".to_owned()),
        "the hidden entry appears once"
    );
    press(&mut session, 'H');
    assert!(!sidebar_rows(&session).contains(&"     .hidden".to_owned()));
}

#[test]
fn the_refresh_key_reads_the_workspace_again() {
    let (dir, mut session) = workspace();
    reveal(&mut session);
    dir.file("later.rs", "\n");

    press(&mut session, 'R');
    drain(&mut session);

    assert!(sidebar_rows(&session).contains(&"     later.rs".to_owned()));
}

#[test]
fn the_text_operations_open_the_prompt_of_the_message_line() {
    let cases = [
        ('a', "new file: "),
        ('A', "new directory: "),
        ('r', "rename: "),
        ('/', "search: "),
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

    assert!(sidebar_rows(&session).contains(&"▌    added.rs".to_owned()));
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
    assert!(sidebar_rows(&session).contains(&"▌    GUIDE.md".to_owned()));
    assert!(selected(&session).ends_with("GUIDE.md"));
    assert!(
        dir.join("GUIDE.md").exists() && !dir.join("README.md").exists(),
        "the workspace holds the renamed entry alone"
    );
    assert_eq!(
        message_line(&session),
        "",
        "a free destination destroys nothing, so it asks nothing"
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

/// Reports whether the sidebar still shows a row of `README.md`.
///
/// The selection mark sits at the left edge of the selected row, so the test
/// reads the end of each row instead of the complete row.
fn shows_readme(session: &Session) -> bool {
    sidebar_rows(session)
        .iter()
        .any(|row| row.ends_with("README.md"))
}

/// Selects `README.md` and presses the delete key, which opens the question.
fn ask_to_delete_readme(session: &mut Session) {
    press(session, 'j');
    press(session, 'j');
    press(session, 'd');
    assert_eq!(
        message_line(session),
        "Delete README.md? [y/N]:",
        "the question names the entry"
    );
}

#[test]
fn a_confirmed_delete_removes_the_selected_entry() {
    let (dir, mut session) = workspace();
    reveal(&mut session);
    ask_to_delete_readme(&mut session);
    assert!(
        session.take_workspace_request().is_none(),
        "the open question reaches no worker before the answer"
    );

    // A lone `y` reaches the answer, so it removes nothing.
    press(&mut session, 'y');
    drain(&mut session);
    assert!(
        dir.join("README.md").exists(),
        "one keypress removes no entry"
    );
    assert_eq!(
        message_line(&session),
        "Delete README.md? [y/N]:y",
        "the row shows the typed answer"
    );

    press_code(&mut session, KeyCode::Enter);
    drain(&mut session);

    assert!(!dir.join("README.md").exists());
    assert!(!shows_readme(&session));
    // The answer returns the keys to the sidebar, so `i` opens no insert mode.
    press(&mut session, 'i');
    assert_eq!(session.mode(), Mode::Normal);
}

#[test]
fn a_cancelled_delete_leaves_every_entry_in_place() {
    // `n` names the default of the question, the empty text names it as well,
    // and `no` and `ya` stand for every remaining answer.
    for value in ["n", "", "no", "ya"] {
        let (dir, mut session) = workspace();
        reveal(&mut session);
        ask_to_delete_readme(&mut session);

        answer(&mut session, value);

        assert!(dir.join("README.md").exists(), "{value:?} removes no entry");
        assert!(
            session.take_workspace_request().is_none(),
            "{value:?} reaches no worker"
        );
        assert_eq!(message_line(&session), "", "{value:?} leaves no trace");
        assert!(shows_readme(&session), "{value:?} keeps the row");
    }

    let (dir, mut session) = workspace();
    reveal(&mut session);
    ask_to_delete_readme(&mut session);
    // `Esc` cancels at any time, so it cancels the typed `y` as well.
    press(&mut session, 'y');
    press_code(&mut session, KeyCode::Esc);
    assert!(dir.join("README.md").exists(), "Esc removes no entry");
    assert_eq!(message_line(&session), "", "Esc leaves no trace");
    assert!(session.take_workspace_request().is_none());
}

#[test]
fn a_delete_without_a_selection_reports_the_refusal_and_asks_nothing() {
    let dir = TempDir::new("tree-empty");
    let root = dir.path.clone();
    let mut settings = EditorSettings::default();
    settings.windows.file_tree_icons = FileTreeIcons::Hidden;
    let mut session = Session::new(Rect::new(0, 0, WIDTH, HEIGHT), settings, test_root(root));
    drain(&mut session);
    reveal(&mut session);

    press(&mut session, 'd');

    assert_eq!(
        message_line(&session),
        "the file tree shows no selected entry",
        "a refused delete reports the refusal instead of a question"
    );
    assert!(session.take_workspace_request().is_none());
}

#[test]
fn a_delete_of_an_entry_that_disappeared_reports_it_and_removes_nothing() {
    let (dir, mut session) = workspace();
    reveal(&mut session);
    ask_to_delete_readme(&mut session);

    // Another program removes the entry while the question waits. The tree
    // follows the change, which proves that the editor stays usable.
    let removed = dir.join("README.md");
    fs::remove_file(&removed).expect("the temporary directory is writable");
    let _ = session.apply_watch_batch(&watch_batch(&dir.path, &removed, WatchKind::Removed));
    drain(&mut session);
    assert!(!shows_readme(&session), "the tree followed the removal");

    answer(&mut session, "y");

    assert_eq!(
        message_line(&session),
        "the file tree no longer shows the entry",
        "the answer stages the removal again and refuses the lost entry"
    );
    assert!(
        session.take_workspace_request().is_none(),
        "the lost entry reaches no worker"
    );
    assert!(
        dir.join("src/main.rs").exists(),
        "the answer removes no other entry"
    );
}

#[test]
fn a_delete_question_names_one_entry_or_the_count_of_several() {
    let one = [PathBuf::from("/workspace/docs/README.md")];
    assert_eq!(delete_question(&one), "Delete README.md");

    let several = [
        PathBuf::from("/workspace/docs/README.md"),
        PathBuf::from("/workspace/src/main.rs"),
        PathBuf::from("/workspace/docs"),
    ];
    assert_eq!(delete_question(&several), "Delete 3 entries");
}

#[test]
fn an_overwrite_question_names_one_entry_or_the_count_of_several() {
    let one = [taken("docs/README.md")];
    assert_eq!(overwrite_question(&one), "Overwrite README.md");

    let several = [
        taken("docs/README.md"),
        taken("src/main.rs"),
        taken("notes.txt"),
    ];
    assert_eq!(overwrite_question(&several), "Overwrite 3 entries");
}

/// Returns one taken destination that holds a file.
fn taken(path: &str) -> TakenDestination {
    TakenDestination {
        path: WorktreeRelativePath::new(path).expect("the fixture path is contained"),
        kind: EntryKind::File,
    }
}

/// Adds one file to the workspace and reads the tree again.
fn add_file(dir: &TempDir, session: &mut Session, name: &str, text: &str) -> PathBuf {
    let path = dir.file(name, text);
    press(session, 'R');
    drain(session);
    path
}

/// Selects `NOTES.md`, which is the third row of the workspace root.
///
/// The first move reaches the first row, because a reveal over an open file
/// selects that file instead of the first entry.
fn select_notes(session: &mut Session) {
    type_keys(session, "gg");
    press(session, 'j');
    press(session, 'j');
    assert!(
        selected(session).ends_with("NOTES.md"),
        "the sidebar selects the source of the rename"
    );
}

/// Renames `NOTES.md` onto `README.md`, which opens the overwrite question.
///
/// The worker refuses the taken destination first, so the question follows the
/// result of the mutation that the user asked for.
fn ask_to_overwrite_readme(session: &mut Session) {
    select_notes(session);
    press(session, 'r');
    type_keys(session, "README.md");
    press_code(session, KeyCode::Enter);
    drain(session);
    assert_eq!(
        message_line(session),
        "Overwrite README.md? [y/N]:",
        "the question names the destination"
    );
}

#[test]
fn a_confirmed_overwrite_replaces_the_destination() {
    let (dir, mut session) = workspace();
    reveal(&mut session);
    let notes = add_file(&dir, &mut session, "NOTES.md", "notes\n");
    ask_to_overwrite_readme(&mut session);

    // The question changed nothing yet.
    assert_eq!(
        fs::read_to_string(dir.join("README.md")).expect("the file exists"),
        "kvim\n"
    );

    answer(&mut session, "y");
    drain(&mut session);

    let content = fs::read_to_string(dir.join("README.md")).expect("the file exists");
    assert_eq!(content, "notes\n", "the answer replaced the destination");
    assert!(!notes.exists(), "the rename leaves no source");
}

#[test]
fn a_cancelled_overwrite_leaves_both_entries_unchanged() {
    // `n` names the default of the question, the empty text names it as well,
    // and `no` and `ya` stand for every remaining answer.
    for value in ["n", "", "no", "ya"] {
        let (dir, mut session) = workspace();
        reveal(&mut session);
        let notes = add_file(&dir, &mut session, "NOTES.md", "notes\n");
        ask_to_overwrite_readme(&mut session);

        answer(&mut session, value);

        assert_eq!(
            fs::read_to_string(dir.join("README.md")).expect("the file exists"),
            "kvim\n",
            "{value:?} keeps the destination"
        );
        assert_eq!(
            fs::read_to_string(&notes).expect("the file exists"),
            "notes\n",
            "{value:?} keeps the source"
        );
        assert!(
            session.take_workspace_request().is_none(),
            "{value:?} reaches no worker"
        );
        assert_eq!(message_line(&session), "", "{value:?} leaves no trace");
    }

    let (dir, mut session) = workspace();
    reveal(&mut session);
    let notes = add_file(&dir, &mut session, "NOTES.md", "notes\n");
    ask_to_overwrite_readme(&mut session);
    press_code(&mut session, KeyCode::Esc);
    assert!(notes.is_file(), "Esc keeps the source");
    assert_eq!(
        fs::read_to_string(dir.join("README.md")).expect("the file exists"),
        "kvim\n"
    );
    assert!(session.take_workspace_request().is_none());
}

#[test]
fn a_confirmed_paste_replaces_the_entry_of_the_destination() {
    let (dir, mut session) = workspace();
    reveal(&mut session);
    add_file(&dir, &mut session, "docs/README.md", "old\n");

    // Hold the file of the workspace root, then paste it into `docs`, which
    // holds one entry of that name already.
    type_keys(&mut session, "gg");
    press(&mut session, 'j');
    press(&mut session, 'j');
    press(&mut session, 'y');
    type_keys(&mut session, "gg");
    press(&mut session, 'p');
    drain(&mut session);

    assert_eq!(
        message_line(&session),
        "Overwrite README.md? [y/N]:",
        "a transfer onto a taken name asks before it replaces the entry"
    );
    answer(&mut session, "y");
    drain(&mut session);

    let content = fs::read_to_string(dir.join("docs/README.md")).expect("the file exists");
    assert_eq!(content, "kvim\n");
    assert!(
        dir.join("README.md").is_file(),
        "a copy leaves the source in place"
    );
}

#[test]
fn an_overwrite_of_a_destination_that_changed_reports_the_refusal() {
    let (dir, mut session) = workspace();
    reveal(&mut session);
    add_file(&dir, &mut session, "NOTES.md", "notes\n");
    ask_to_overwrite_readme(&mut session);

    // A directory took the place of the file while the question waited, so the
    // answer names another entry than the question did.
    let destination = dir.join("README.md");
    fs::remove_file(&destination).expect("the file exists");
    fs::create_dir(&destination).expect("the name is free");
    fs::write(destination.join("inner.md"), "inner").expect("the directory is writable");

    answer(&mut session, "y");
    drain(&mut session);

    let report = message(&session);
    assert_eq!(
        report,
        format!(
            "{} changed while the question waited",
            destination.display()
        )
    );
    assert_eq!(
        fs::read_to_string(destination.join("inner.md")).expect("the directory survived"),
        "inner",
        "the answer destroys no entry that the question never named"
    );
    assert!(dir.join("NOTES.md").is_file(), "the source stays in place");
}

#[test]
fn an_overwrite_of_a_buffer_with_unsaved_changes_asks_nothing() {
    let (dir, mut session) = workspace();
    session.open_path(dir.join("README.md"));
    drain_file(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);

    reveal(&mut session);
    add_file(&dir, &mut session, "NOTES.md", "notes\n");
    select_notes(&mut session);
    press(&mut session, 'r');
    type_keys(&mut session, "README.md");
    press_code(&mut session, KeyCode::Enter);
    drain(&mut session);

    let report = message(&session);
    assert_eq!(
        report,
        format!("{} holds unsaved changes", dir.join("README.md").display()),
        "a destination with unsaved changes refuses the mutation without a question"
    );
    assert_eq!(
        fs::read_to_string(dir.join("README.md")).expect("the file exists"),
        "kvim\n"
    );
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
        "src exists already",
        "a destination collision reports the typed rejection, against the workspace root"
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
        "x\n",
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

/// Creates one workspace whose names make every search match unambiguous.
///
/// The root holds one directory that a test opens itself, one directory that
/// stays closed until a search opens it, and one file that matches no query.
fn search_workspace() -> (TempDir, Session) {
    let dir = TempDir::new("tree-search");
    dir.file("open/target_one.rs", "");
    dir.file("closed/target_two.rs", "");
    dir.file("plain.txt", "");
    let root = dir.path.clone();
    let mut settings = EditorSettings::default();
    settings.windows.file_tree_icons = FileTreeIcons::Hidden;
    let mut session = Session::new(Rect::new(0, 0, WIDTH, HEIGHT), settings, test_root(root));
    drain(&mut session);
    reveal(&mut session);
    // `closed` is the first row, so one step down reaches `open`.
    press(&mut session, 'j');
    press(&mut session, 'l');
    drain(&mut session);
    (dir, session)
}

/// Runs one search over the file tree and applies every queued read.
fn search(session: &mut Session, query: &str) {
    press(session, '/');
    type_keys(session, query);
    press_code(session, KeyCode::Enter);
    drain(session);
}

/// Returns the sidebar columns of one row that carry a search-match color.
///
/// The renderer paints the matched characters with the match role of the theme,
/// so the background of a cell reports the mark.
fn match_columns(session: &Session, row: u16) -> Vec<u16> {
    let theme = Theme::new();
    let marked = theme.style(ThemeRole::SearchMatch).bg;
    let current = theme.style(ThemeRole::CurrentSearchMatch).bg;
    let buffer = draw(session);
    (SIDEBAR_X..WIDTH)
        .filter(|x| {
            buffer
                .cell((*x, row))
                .is_some_and(|cell| cell.style().bg == marked || cell.style().bg == current)
        })
        .map(|x| x - SIDEBAR_X)
        .collect()
}

/// Returns the entry name of one rendered sidebar row.
fn entry_name(row: &str) -> &str {
    row.trim_start_matches([
        ' ', '\u{258c}', '\u{2502}', '\u{2514}', '\u{25be}', '\u{25b8}',
    ])
}

/// The rows of the search workspace before one search runs.
fn rows_before_the_search() -> Vec<String> {
    vec![
        "   ▸ closed".to_owned(),
        "▌  ▾ open".to_owned(),
        "   └   target_one.rs".to_owned(),
        "     plain.txt".to_owned(),
    ]
}

/// The rows of the search workspace while the search shows both matches.
fn rows_during_the_search() -> Vec<String> {
    vec![
        "   ▾ closed".to_owned(),
        "   └   target_two.rs".to_owned(),
        "▌  ▾ open".to_owned(),
        "   └   target_one.rs".to_owned(),
        "     plain.txt".to_owned(),
    ]
}

#[test]
fn a_search_keeps_every_row_and_marks_every_match() {
    let (_dir, mut session) = search_workspace();
    assert_eq!(sidebar_rows(&session), rows_before_the_search());

    search(&mut session, "target");

    // Every entry that the reader saw before the search stays visible. Only
    // the expansion marker of the opened directory changes.
    let rows = sidebar_rows(&session);
    let names: Vec<&str> = rows.iter().map(|row| entry_name(row)).collect();
    for row in rows_before_the_search() {
        let name = entry_name(&row);
        assert!(names.contains(&name), "the search keeps the entry `{name}`");
    }
    assert_eq!(rows, rows_during_the_search());

    // The name of a match starts behind the mark cell, the indent guides, and
    // the glyph cells, so the mark covers the six characters of `target` alone.
    let marked: Vec<u16> = (7..13).collect();
    assert_eq!(match_columns(&session, 2), marked, "the revealed match");
    assert_eq!(match_columns(&session, 4), marked, "the open match");
    assert!(
        match_columns(&session, 5).is_empty(),
        "a row without a match carries no mark"
    );
}

#[test]
fn n_and_capital_n_move_between_the_matches_in_row_order() {
    let (_dir, mut session) = search_workspace();
    search(&mut session, "target");
    assert_eq!(selected_name(&session), "open");

    // `n` takes the next match below the selection and wraps at the last one.
    press(&mut session, 'n');
    assert_eq!(selected_name(&session), "target_one.rs");
    press(&mut session, 'n');
    assert_eq!(selected_name(&session), "target_two.rs");

    // `N` takes the previous match and wraps at the first one.
    press(&mut session, 'N');
    assert_eq!(selected_name(&session), "target_one.rs");
    press(&mut session, 'N');
    assert_eq!(selected_name(&session), "target_two.rs");
}

#[test]
fn esc_and_ctrl_c_both_end_the_search_and_restore_the_expansion() {
    for end in ["esc", "ctrl-c"] {
        let (_dir, mut session) = search_workspace();
        search(&mut session, "target");
        assert_eq!(sidebar_rows(&session), rows_during_the_search());

        if end == "esc" {
            press_code(&mut session, KeyCode::Esc);
        } else {
            press_ctrl(&mut session, 'c');
        }

        assert_eq!(
            sidebar_rows(&session),
            rows_before_the_search(),
            "`{end}` closes the directory that the search opened and keeps the other"
        );
        assert!(
            match_columns(&session, 3).is_empty(),
            "`{end}` removes every mark"
        );
    }
}

#[test]
fn a_query_without_a_match_changes_no_row_and_reports_the_missing_match() {
    let (_dir, mut session) = search_workspace();

    search(&mut session, "zzzz");

    assert_eq!(sidebar_rows(&session), rows_before_the_search());
    assert!(match_columns(&session, 3).is_empty());

    press(&mut session, 'n');
    assert_eq!(message(&session), "no match");
    assert_eq!(selected_name(&session), "open");
}

#[test]
fn an_end_without_an_active_search_changes_nothing() {
    let (_dir, mut session) = search_workspace();
    let before = sidebar_rows(&session);
    let selected = selected_name(&session);

    press_code(&mut session, KeyCode::Esc);
    press_ctrl(&mut session, 'c');

    assert_eq!(sidebar_rows(&session), before);
    assert_eq!(selected_name(&session), selected);
}

#[test]
fn the_sidebar_scrolls_so_the_selected_row_stays_visible() {
    // The workspace holds more files than the sidebar shows at once.
    let (_dir, mut session) = flat_workspace(TALL_ENTRIES);

    for _ in 0..TALL_ENTRIES - 1 {
        press(&mut session, 'j');
    }

    let rows = sidebar_rows(&session);
    assert!(
        rows.contains(&selected_entry_row(TALL_ENTRIES - 1)),
        "the selected row stays inside the sidebar"
    );
    assert!(
        !rows.contains(&entry_row(0)),
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
    let _ = session.abandon_workspace_request(crate::session::FileRequestFailure::Timeout);

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
    assert_eq!(sidebar_row(&session, 1), "▌  ▾ docs");
    press(&mut session, 'l');
    drain(&mut session);
    assert_eq!(
        sidebar_row(&session, 1),
        "▌  ▾ docs",
        "`l` on an open directory keeps it open"
    );

    // `h` closes an open directory.
    press(&mut session, 'h');
    drain(&mut session);
    assert_eq!(sidebar_row(&session, 1), "▌  ▸ docs");
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
            "   ▸ docs".to_owned(),
            "▌  ▾ src".to_owned(),
            "   └   main.rs".to_owned(),
            "     README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
        ]
    );
    press(&mut session, 'j');
    assert_eq!(selected_name(&session), "main.rs");
    press(&mut session, 'h');
    assert_eq!(selected_name(&session), "src");
    press(&mut session, 'h');
    drain(&mut session);
    assert_eq!(sidebar_row(&session, 2), "▌  ▸ src");
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
fn opening_a_tree_entry_records_one_jump() {
    let (_dir, mut session) = workspace();
    let under = session.active_buffer().name().to_owned();
    reveal(&mut session);

    // The third row holds `README.md`, and the first read selected the first.
    type_keys(&mut session, "jj");
    assert_eq!(selected_name(&session), "README.md");
    press_code(&mut session, KeyCode::Enter);
    drain_file(&mut session);
    assert_eq!(session.active_buffer().name(), "README.md");

    // The opened entry replaced the buffer of the editor window, which Vim
    // records as a jump.
    press_ctrl(&mut session, 'o');
    assert_eq!(
        session.active_buffer().name(),
        under,
        "`Ctrl-O` returns to the buffer that the opened entry replaced"
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
            "▌  \u{f07b} docs".to_owned(),
            "   \u{f07b} src".to_owned(),
            "   \u{f48a} README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
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
            "   \u{f07b} docs".to_owned(),
            "▌  \u{f07c} src".to_owned(),
            "   └ \u{e7a8} main.rs".to_owned(),
            "   \u{f48a} README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
        ]
    );

    // The same tree without icons keeps the names aligned.
    let (_other_dir, mut plain) = workspace_with_icons(FileTreeIcons::Hidden);
    reveal(&mut plain);
    assert_eq!(
        sidebar_rows(&plain),
        vec![
            "▌  ▸ docs".to_owned(),
            "   ▸ src".to_owned(),
            "     README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
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
        let _ = session.apply_workspace_result(request.run());
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
    let _ = session.apply_workspace_result(request.run());
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
    let _ = session.abandon_workspace_request(FileRequestFailure::Saturated);
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
            "▌  ▸ docs".to_owned(),
            "   ▸ src".to_owned(),
            "     README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
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
    let _ = session.abandon_workspace_request(FileRequestFailure::Cancelled);
    let _ = session.apply_workspace_result(result);
    assert!(
        drain_counted(&mut session) < WORKSPACE_STEPS_MAX,
        "the queue still empties after an obsolete result"
    );
    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▌  ▸ docs".to_owned(),
            "   ▸ src".to_owned(),
            "     README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
        ],
        "the tree keeps its rows"
    );
}

/// The number of tree rows that the sidebar shows in a terminal of [`HEIGHT`]
/// rows.
///
/// The statusline and the message line take the last two terminal rows, and the
/// sidebar title takes the first row of the region.
const SIDEBAR_ROWS: usize = (HEIGHT - 2 - TREE_TITLE_ROWS) as usize;

/// The number of rows that `Ctrl-D` and `Ctrl-U` move.
///
/// The sidebar reads the buffer rule, which is half of the visible rows.
const HALF_PAGE_ROWS: usize = SIDEBAR_ROWS / 2;

/// The number of rows that `Ctrl-F` and `Ctrl-B` move.
///
/// The sidebar reads the buffer rule, which is the visible rows less the
/// two-row overlap that keeps the reader oriented.
const FULL_PAGE_ROWS: usize = SIDEBAR_ROWS - 2;

/// The number of files in the workspace of the navigation tests.
///
/// The count passes the visible rows twice over, so a page move stays inside
/// the tree and a clamp at the last row is a real transition.
const TALL_ENTRIES: usize = 20;

/// Returns one plain character key.
fn plain(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

/// Returns one character key with the Control chord.
fn ctrl(value: char) -> Key {
    Key::ctrl(KeyCode::Char(value))
}

/// Feeds a run of keys.
fn press_keys(session: &mut Session, keys: &[Key]) {
    for &key in keys {
        session.handle_event(TerminalEvent::Key(key), NOW);
    }
}

/// Creates one flat workspace of `entries` files and opens its sidebar.
///
/// Every name sorts in order, so row `n` holds `entry-n`. A move is therefore
/// one row index, and the test needs no path.
fn flat_workspace(entries: usize) -> (TempDir, Session) {
    let dir = TempDir::new("tree-motion");
    for index in 0..entries {
        dir.file(&format!("entry-{index:02}"), "\n");
    }
    let root = dir.path.clone();
    let mut settings = EditorSettings::default();
    settings.windows.file_tree_icons = FileTreeIcons::Hidden;
    let mut session = Session::new(Rect::new(0, 0, WIDTH, HEIGHT), settings, test_root(root));
    drain(&mut session);
    reveal(&mut session);
    (dir, session)
}

/// Returns the row index of the selection, or `None` while nothing is selected.
fn selected_index(session: &Session) -> Option<usize> {
    let tree = session.file_tree();
    let selected = tree.selected()?;
    tree.rows()
        .iter()
        .position(|row| row.is_selectable() && row.path == selected)
}

/// Returns the row index of the selection and reports an empty tree.
fn selected_row_index(session: &Session) -> usize {
    selected_index(session).expect("the tree of the test holds a selection")
}

/// Returns the entry rows of the sidebar, without the statusline below them.
///
/// [`sidebar_rows`] stops at the first empty row, so it reaches the statusline
/// once the tree fills the sidebar. This entry point reads the region of the
/// entries alone, which lets a test assert the first and the last visible row.
fn visible_entries(session: &Session) -> Vec<String> {
    let rows = u16::try_from(SIDEBAR_ROWS).expect("the test terminal is small");
    (TREE_TITLE_ROWS..TREE_TITLE_ROWS + rows)
        .map(|row| sidebar_row(session, row).replacen(ROW_MARK, " ", 1))
        .collect()
}

/// Returns the row text of one unselected entry of the flat workspace.
fn entry_row(index: usize) -> String {
    format!("     entry-{index:02}")
}

/// Returns the row text of one selected entry of the flat workspace.
fn selected_entry_row(index: usize) -> String {
    entry_row(index).replacen(' ', ROW_MARK, 1)
}

#[test]
fn the_navigation_keys_move_the_tree_selection_like_a_buffer() {
    let cases: [(&str, usize, Vec<Key>, usize); 16] = [
        ("j moves one row down", 0, vec![plain('j')], 1),
        ("k moves one row up", 5, vec![plain('k')], 4),
        ("k stops at the first row", 0, vec![plain('k')], 0),
        (
            "Ctrl-D moves half a page down",
            0,
            vec![ctrl('d')],
            HALF_PAGE_ROWS,
        ),
        (
            "Ctrl-U moves half a page up",
            HALF_PAGE_ROWS * 2,
            vec![ctrl('u')],
            HALF_PAGE_ROWS,
        ),
        ("Ctrl-U stops at the first row", 0, vec![ctrl('u')], 0),
        (
            "Ctrl-F moves a full page down",
            0,
            vec![ctrl('f')],
            FULL_PAGE_ROWS,
        ),
        (
            "Ctrl-B moves a full page up",
            FULL_PAGE_ROWS + 1,
            vec![ctrl('b')],
            1,
        ),
        (
            "G reaches the last row",
            0,
            vec![plain('G')],
            TALL_ENTRIES - 1,
        ),
        (
            "gg reaches the first row",
            6,
            vec![plain('g'), plain('g')],
            0,
        ),
        (
            "a page move stops at the last row",
            TALL_ENTRIES - 2,
            vec![ctrl('d'), ctrl('d')],
            TALL_ENTRIES - 1,
        ),
        ("a count repeats j", 0, vec![plain('5'), plain('j')], 5),
        (
            "a count repeats Ctrl-D",
            0,
            vec![plain('2'), ctrl('d')],
            HALF_PAGE_ROWS * 2,
        ),
        (
            "a count names the row of G",
            0,
            vec![plain('1'), plain('2'), plain('G')],
            11,
        ),
        (
            "a count names the row of gg",
            9,
            vec![plain('3'), plain('g'), plain('g')],
            2,
        ),
        (
            "a count above the row count clamps",
            0,
            vec![plain('9'), plain('9'), plain('G')],
            TALL_ENTRIES - 1,
        ),
    ];

    for (name, start, keys, expected) in cases {
        let (_dir, mut session) = flat_workspace(TALL_ENTRIES);
        for _ in 0..start {
            press(&mut session, 'j');
        }
        assert_eq!(selected_row_index(&session), start, "{name}: the start row");

        press_keys(&mut session, &keys);

        assert_eq!(selected_row_index(&session), expected, "{name}");
    }
}

#[test]
fn a_navigation_key_leaves_an_empty_tree_unselected() {
    let (_dir, mut session) = flat_workspace(0);
    assert!(
        session.file_tree().rows().is_empty(),
        "the workspace of this test holds no entry"
    );

    // Every move reads the row count first, so none of them may reach a row
    // index of an empty tree.
    for key in [
        plain('j'),
        plain('k'),
        ctrl('d'),
        ctrl('u'),
        ctrl('f'),
        ctrl('b'),
        plain('G'),
    ] {
        press_keys(&mut session, &[key]);
        assert_eq!(selected_index(&session), None, "{key:?} selects no row");
    }
    press_keys(&mut session, &[plain('g'), plain('g')]);
    assert_eq!(selected_index(&session), None, "gg selects no row");
}

#[test]
fn every_navigation_key_keeps_the_one_row_of_a_one_row_tree() {
    let (_dir, mut session) = flat_workspace(1);

    for key in [
        plain('j'),
        plain('k'),
        ctrl('d'),
        ctrl('u'),
        ctrl('f'),
        ctrl('b'),
        plain('G'),
    ] {
        press_keys(&mut session, &[key]);
        assert_eq!(selected_row_index(&session), 0, "{key:?} keeps the one row");
    }
    press_keys(&mut session, &[plain('g'), plain('g')]);
    assert_eq!(selected_row_index(&session), 0, "gg keeps the one row");
}

#[test]
fn the_sidebar_keeps_the_scroll_margin_around_the_selected_row() {
    let margin = usize::from(EditorSettings::default().display.scrolloff_rows);
    assert!(
        margin > 0 && margin * 2 < SIDEBAR_ROWS,
        "the sidebar of this test is taller than twice the scroll margin"
    );

    let (_dir, mut session) = flat_workspace(TALL_ENTRIES);
    assert_eq!(
        visible_entries(&session).first(),
        Some(&entry_row(0)),
        "the sidebar starts at the first row"
    );

    // The first half page leaves room for the margin below the selection, so
    // the visible rows stay where they are.
    press_keys(&mut session, &[ctrl('d')]);
    assert_eq!(
        visible_entries(&session).first(),
        Some(&entry_row(0)),
        "a selection inside the margin moves no row"
    );

    // The second half page puts the selection close to the last visible row, so
    // the sidebar scrolls until the margin fits below it again.
    press_keys(&mut session, &[ctrl('d')]);
    let selected = selected_row_index(&session);
    assert!(
        selected + margin >= SIDEBAR_ROWS,
        "the second half page passes the margin of the visible rows"
    );
    assert_eq!(
        visible_entries(&session).first(),
        Some(&entry_row(selected + margin + 1 - SIDEBAR_ROWS)),
        "the margin below the selection decides the first visible row"
    );

    // `G` reaches the last row, where no content can fill the margin below, so
    // the sidebar stops at its own end instead of scrolling past it.
    press_keys(&mut session, &[plain('G')]);
    let rows = visible_entries(&session);
    assert_eq!(
        rows.last(),
        Some(&entry_row(TALL_ENTRIES - 1)),
        "the last row of the tree is the last visible row"
    );
    assert_eq!(
        rows.first(),
        Some(&entry_row(TALL_ENTRIES - SIDEBAR_ROWS)),
        "the sidebar shows a full region of rows at its end"
    );
}

/// Creates one workspace whose entries cover the appearance of the tree.
///
/// The root holds one nested directory, one generated directory, and one file
/// of each icon kind, so a rendered row proves the guides, the icons, and the
/// dimmed style together.
fn appearance_workspace(icons: FileTreeIcons) -> (TempDir, Session) {
    let dir = TempDir::new("tree-look");
    dir.file("alpha/inner/deep.rs", "");
    dir.file("alpha/beta.rs", "");
    dir.dir("target");
    dir.file("LICENSE", "");
    dir.file("README.md", "");
    dir.file("flake.nix", "");
    dir.file("root.rs", "");
    let root = dir.path.clone();
    let mut settings = EditorSettings::default();
    settings.windows.file_tree_icons = icons;
    let mut session = Session::new(Rect::new(0, 0, WIDTH, HEIGHT), settings, test_root(root));
    drain(&mut session);
    reveal(&mut session);
    (dir, session)
}

/// Returns the style of one sidebar cell.
fn sidebar_style(session: &Session, column: u16, row: u16) -> Style {
    draw(session)
        .cell((SIDEBAR_X + column, row))
        .expect("the test reads a cell inside the sidebar")
        .style()
}

/// Returns the theme of every test session.
fn theme() -> Theme {
    Theme::new()
}

#[test]
fn the_header_row_shortens_the_home_directory_to_one_character() {
    let home = Path::new("/home/reader");
    assert_eq!(root_label(Path::new("/home/reader"), Some(home)), "~");
    assert_eq!(
        root_label(Path::new("/home/reader/work/kvim"), Some(home)),
        "~/work/kvim"
    );
    assert_eq!(
        root_label(Path::new("/srv/kvim"), Some(home)),
        "/srv/kvim",
        "a root outside the home directory keeps its complete path"
    );
    assert_eq!(
        root_label(Path::new("/srv/kvim"), None),
        "/srv/kvim",
        "a session without a home directory keeps the complete path"
    );
}

#[test]
fn the_header_row_names_the_root_behind_an_open_directory_icon() {
    let (dir, session) = appearance_workspace(FileTreeIcons::Shown);
    let header = sidebar_row(&session, 0);
    assert!(
        header.starts_with(" \u{f07c} "),
        "the header holds the open-directory icon: `{header}`"
    );
    // The sidebar is narrower than a temporary path, so the header holds the
    // first cells of the root path.
    let root = dir.path.display().to_string();
    assert!(
        root.starts_with(header.trim_start_matches([' ', '\u{f07c}'])),
        "the header names the workspace root: `{header}`"
    );

    // Without a patched font the header keeps the same cells and marks the
    // root as one open directory.
    let (_plain_dir, plain) = appearance_workspace(FileTreeIcons::Hidden);
    assert!(sidebar_row(&plain, 0).starts_with(" ▾ "));
}

#[test]
fn the_indent_guides_draw_a_trunk_and_close_the_last_child_with_an_elbow() {
    let (_dir, mut session) = appearance_workspace(FileTreeIcons::Hidden);

    // Open `alpha` and then `alpha/inner`, so the rows cover three levels.
    press(&mut session, 'l');
    drain(&mut session);
    press(&mut session, 'j');
    press(&mut session, 'l');
    drain(&mut session);

    // The rows fill the sidebar, so the assertion reads the entry rows alone.
    let rows = sidebar_rows(&session);
    assert_eq!(
        rows[..9],
        [
            "   ▾ alpha".to_owned(),
            "▌  │ ▾ inner".to_owned(),
            "   │ └   deep.rs".to_owned(),
            "   └   beta.rs".to_owned(),
            "   ▸ target".to_owned(),
            "     LICENSE".to_owned(),
            "     README.md".to_owned(),
            "     flake.nix".to_owned(),
            "     root.rs".to_owned(),
        ],
        "a level that holds a further entry draws a trunk, and its last child \
         closes it with an elbow"
    );

    // The guides carry their own color, so they never read as one name.
    let guide = theme().style(ThemeRole::TreeIndentGuide);
    assert_eq!(sidebar_style(&session, 3, 3).fg, guide.fg);
}

#[test]
fn the_leading_blank_and_the_shared_rule_together_cost_one_guide_per_level() {
    // `tree.rs` prepends one `SIDEBAR_GUIDE_BLANK` before the shared
    // `kvim_ui::sidebar_guides` result, because the workspace-root header row
    // above the entries carries no sibling that a guide could ever close.
    // This test pins the exact guide string at three depths, and the total
    // cell width that the leading blank and the shared rule together cost, so
    // a later reader who drops the leading blank fails here first.
    let (_dir, mut session) = appearance_workspace(FileTreeIcons::Hidden);
    press(&mut session, 'l');
    drain(&mut session);
    press(&mut session, 'j');
    press(&mut session, 'l');
    drain(&mut session);

    let rows = sidebar_rows(&session);
    // The guide of one row sits behind the mark cell and ends where the
    // directory or the file glyph starts, at `(depth + 1)` guide levels.
    let guide_of = |row: &str, depth: usize| -> String {
        row.chars()
            .skip(MARK_CELLS)
            .take(SIDEBAR_GUIDE_INDENT_CELLS * (depth + 1))
            .collect::<String>()
    };

    // Depth 0: `alpha` carries the leading blank alone.
    assert_eq!(guide_of(&rows[0], 0), SIDEBAR_GUIDE_BLANK);
    // Depth 1: `inner` sits below `alpha`, which holds the further sibling
    // `beta.rs`, so the first level draws a trunk behind the leading blank.
    assert_eq!(
        guide_of(&rows[1], 1),
        format!("{SIDEBAR_GUIDE_BLANK}{SIDEBAR_GUIDE_TRUNK}"),
    );
    // Depth 2: `deep.rs` is the only, and therefore the last, child of
    // `inner`, so its own level closes with an elbow behind the same trunk.
    assert_eq!(
        guide_of(&rows[2], 2),
        format!("{SIDEBAR_GUIDE_BLANK}{SIDEBAR_GUIDE_TRUNK}{SIDEBAR_GUIDE_ELBOW}"),
    );

    for (row, depth) in [(&rows[0], 0), (&rows[1], 1), (&rows[2], 2)] {
        assert_eq!(
            guide_of(row, depth).chars().count(),
            (depth + 1) * SIDEBAR_GUIDE_INDENT_CELLS,
            "the row at depth {depth} costs (depth + 1) guide levels",
        );
    }
}

#[test]
fn every_row_selects_its_icon_by_extension_and_by_well_known_name() {
    let (_dir, mut session) = appearance_workspace(FileTreeIcons::Shown);
    press(&mut session, 'l');
    drain(&mut session);

    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▌  \u{f07c} alpha".to_owned(),
            "   │ \u{f07b} inner".to_owned(),
            "   └ \u{e7a8} beta.rs".to_owned(),
            "   \u{f07b} target".to_owned(),
            "   \u{f0e3} LICENSE".to_owned(),
            "   \u{f48a} README.md".to_owned(),
            "   \u{f313} flake.nix".to_owned(),
            "   \u{e7a8} root.rs".to_owned(),
        ],
        "the well-known name wins over the extension, and every other file \
         reads its extension"
    );
}

#[test]
fn a_generated_entry_reads_as_dimmed_beside_an_ordinary_one() {
    let (_dir, session) = appearance_workspace(FileTreeIcons::Hidden);

    // `target` is the second row, and `LICENSE` the third one.
    let generated = sidebar_style(&session, 5, 2);
    let ordinary = sidebar_style(&session, 5, 3);
    assert_eq!(generated.fg, theme().style(ThemeRole::TreeMuted).fg);
    assert_ne!(
        generated.fg, ordinary.fg,
        "a generated entry separates from an ordinary one"
    );
}

#[test]
fn the_hidden_count_row_names_its_number_and_reads_as_one_quiet_report() {
    let (dir, mut session) = workspace();
    reveal(&mut session);

    // The workspace hides one entry, so the count closes the rows of the root.
    assert_eq!(sidebar_row(&session, 4), "     (1 hidden item)");
    let counted = sidebar_style(&session, 6, 4);
    assert_eq!(counted.fg, theme().style(ThemeRole::TreeNotice).fg);
    assert!(
        counted.add_modifier.contains(Modifier::ITALIC),
        "a report row separates from an entry by its italic style"
    );

    dir.file(".second", "");
    press(&mut session, 'R');
    drain(&mut session);
    assert_eq!(sidebar_row(&session, 4), "     (2 hidden items)");
}

#[test]
fn a_failed_read_warns_while_a_hidden_count_stays_quiet() {
    let (dir, mut session) = workspace();
    reveal(&mut session);
    let counted = sidebar_style(&session, 6, 4);

    press(&mut session, 'l');
    fs::remove_dir_all(dir.join("docs")).expect("the temporary directory is writable");
    drain(&mut session);

    // The report of the failed read sits on the second row.
    let report = sidebar_style(&session, 8, 2);
    assert_eq!(report.fg, theme().style(ThemeRole::TreeIncomplete).fg);
    assert!(report.add_modifier.contains(Modifier::ITALIC));
    assert_ne!(
        report.fg, counted.fg,
        "an incomplete read must not read like a count of hidden entries"
    );
}

#[test]
fn the_selected_row_carries_one_band_and_one_mark_at_its_left_edge() {
    let (_dir, session) = appearance_workspace(FileTreeIcons::Hidden);
    let buffer = draw(&session);
    let band = theme().style(ThemeRole::PopupSelection).bg;

    for column in SIDEBAR_X..WIDTH {
        let cell = buffer
            .cell((column, 1))
            .expect("the test reads a cell inside the sidebar");
        assert_eq!(cell.style().bg, band, "the band covers the column {column}");
    }
    assert_eq!(
        buffer
            .cell((SIDEBAR_X, 1))
            .expect("the sidebar holds its first column")
            .symbol(),
        ROW_MARK
    );
    assert_eq!(
        sidebar_style(&session, 0, 1).fg,
        theme().style(ThemeRole::TreeSelectionMark).fg
    );

    // Every other row keeps the column blank, so one mark never moves a name.
    assert_eq!(
        buffer
            .cell((SIDEBAR_X, 2))
            .expect("the sidebar holds its first column")
            .symbol(),
        " "
    );
}

#[test]
fn every_row_below_the_last_entry_stays_empty() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    // The workspace holds three entries and one hidden count, so the rows below
    // the fifth one hold no text at all. The statusline and the message line
    // take the last two terminal rows.
    for row in 5..HEIGHT - 2 {
        assert_eq!(
            sidebar_row(&session, row),
            "",
            "the sidebar row {row} stays empty"
        );
    }
}

#[test]
fn the_file_clipboard_marks_a_held_entry_until_the_operation_ends() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);
    type_keys(&mut session, "jj");
    assert_eq!(selected_name(&session), "README.md");

    press(&mut session, 'x');
    assert_eq!(sidebar_row(&session, 3), "▌    README.md (cut)");
    assert_eq!(
        sidebar_style(&session, 5, 3).fg,
        theme().style(ThemeRole::TreeMuted).fg,
        "the name of a held entry dims with its suffix"
    );

    press(&mut session, 'y');
    assert_eq!(sidebar_row(&session, 3), "▌    README.md (copied)");

    // `Esc` cancels the pending operation and clears the marker.
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(sidebar_row(&session, 3), "▌    README.md");

    // One completed paste clears the marker as well.
    press(&mut session, 'y');
    assert_eq!(sidebar_row(&session, 3), "▌    README.md (copied)");
    type_keys(&mut session, "kk");
    press(&mut session, 'p');
    drain(&mut session);
    assert!(
        sidebar_rows(&session)
            .iter()
            .all(|row| !row.contains("(copied)") && !row.contains("(cut)")),
        "the completed paste released the held entry"
    );
}

#[test]
fn a_narrow_sidebar_clips_every_row_at_its_own_edge() {
    let dir = TempDir::new("tree-narrow");
    dir.file("alpha/inner/a-very-long-file-name.rs", "");
    let root = dir.path.clone();
    let mut settings = EditorSettings::default();
    settings.windows.file_tree_icons = FileTreeIcons::Hidden;
    settings.windows.file_tree_width_cells = 10;
    let mut session = Session::new(Rect::new(0, 0, WIDTH, HEIGHT), settings, test_root(root));
    drain(&mut session);
    reveal(&mut session);
    press(&mut session, 'l');
    drain(&mut session);
    press(&mut session, 'j');
    press(&mut session, 'l');
    drain(&mut session);

    let narrow = WIDTH - 10;
    let buffer = draw(&session);
    let rows: Vec<String> = (0..4)
        .map(|row| {
            let mut text = String::new();
            for column in narrow..WIDTH {
                text.push_str(
                    buffer
                        .cell((column, row))
                        .expect("the test reads a cell inside the sidebar")
                        .symbol(),
                );
            }
            text.trim_end().to_owned()
        })
        .collect();
    assert_eq!(
        rows[2..],
        ["▌  └ ▾ in".to_owned(), "     └".to_owned()],
        "every row stops at the right edge of the sidebar"
    );
}

/// Creates one workspace whose entries cover every recorded Git state.
///
/// The sidebar shows one ignored directory, one collapsed directory, and four
/// files, so one snapshot covers every mark and the roll-up together. The
/// opening status read stays queued, so each test decides when it answers.
fn git_workspace() -> (TempDir, Session) {
    let dir = TempDir::new("tree-git");
    dir.file("src/main.rs", "");
    dir.dir("build");
    dir.file("modified.rs", "");
    dir.file("new.rs", "");
    dir.file("notes.log", "");
    dir.file("staged.rs", "");
    let root = dir.path.clone();
    let mut settings = EditorSettings::default();
    settings.windows.file_tree_icons = FileTreeIcons::Hidden;
    let mut session = Session::new(Rect::new(0, 0, WIDTH, HEIGHT), settings, test_root(root));
    drain(&mut session);
    reveal(&mut session);
    (dir, session)
}

/// One ordinary record of `git status --porcelain=v2 -z`.
fn git_record(field: &str, path: &str) -> String {
    format!(
        "1 {field} N... 100644 100644 100644 \
         78981922613b2afb6025042ff6bd878ac1994e85 \
         78981922613b2afb6025042ff6bd878ac1994e85 {path}\0"
    )
}

/// The recorded status of the workspace that [`git_workspace`] builds.
fn git_output() -> String {
    format!(
        "! build/\0{}{}{}? new.rs\0! notes.log\0",
        git_record(".M", "src/main.rs"),
        git_record(".M", "modified.rs"),
        git_record("M.", "staged.rs"),
    )
}

/// Answers one command of the status read with a recorded output.
///
/// The read takes one prefix command and one status command, so the session
/// receives one answer for each and publishes after the second.
fn answer_git(session: &mut Session, root: &Path, stdout: &[u8]) {
    let request = session
        .take_git_request()
        .expect("the sidebar asks for one status read");
    assert_eq!(
        request.root().as_path(),
        root,
        "the read names the workspace root"
    );
    let command = request.command();
    assert_eq!(
        command.program, GIT_PROGRAM,
        "the read leaves the session as one external command"
    );
    assert_eq!(
        command.current_dir.as_deref(),
        Some(root),
        "the child receives the canonical root as its working directory"
    );
    let output = ProcessOutput {
        status_code: Some(0),
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    };
    let _ = session.apply_git_result(request.publish(&output));
}

/// Publishes one recorded status output, as the event loop does.
fn publish_git(session: &mut Session, root: &Path, output: &str) {
    // The workspace root of every test is its own repository top level, so the
    // prefix command reports an empty prefix.
    answer_git(session, root, b"\n");
    answer_git(session, root, output.as_bytes());
}

/// Returns the Git mark at the right edge of one sidebar row.
fn git_mark(session: &Session, row: u16) -> String {
    draw(session)
        .cell((WIDTH - 1, row))
        .expect("the test reads the last cell of the sidebar")
        .symbol()
        .to_owned()
}

#[test]
fn the_sidebar_marks_every_recorded_git_state_and_rolls_a_directory_up() {
    let (dir, mut session) = git_workspace();
    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▌  ▸ build".to_owned(),
            "   ▸ src".to_owned(),
            "     modified.rs".to_owned(),
            "     new.rs".to_owned(),
            "     notes.log".to_owned(),
            "     staged.rs".to_owned(),
        ]
    );
    assert!(
        (1..=6).all(|row| git_mark(&session, row) == " "),
        "the sidebar shows no mark before the first status read"
    );

    publish_git(&mut session, &dir.path, &git_output());

    assert_eq!(git_mark(&session, 1), "☑", "an ignored directory");
    assert_eq!(
        git_mark(&session, 2),
        "●",
        "a collapsed directory reports the change of the entry below it"
    );
    assert_eq!(git_mark(&session, 3), "●", "a modified entry");
    assert_eq!(git_mark(&session, 4), "□", "an untracked entry");
    assert_eq!(git_mark(&session, 5), "☑", "an ignored entry");
    assert_eq!(git_mark(&session, 6), "■", "a staged entry");
}

#[test]
fn a_changed_name_takes_its_state_color_and_an_ignored_name_dims() {
    let (dir, mut session) = git_workspace();
    publish_git(&mut session, &dir.path, &git_output());

    // The name of an entry starts behind the mark cell, the indent guides, and
    // the glyph cells.
    let modified = sidebar_style(&session, 5, 3);
    let untracked = sidebar_style(&session, 5, 4);
    let ignored = sidebar_style(&session, 5, 5);
    assert_eq!(
        modified.fg,
        theme().style(ThemeRole::TreeGit(FileRowGit::Modified)).fg
    );
    assert_eq!(
        untracked.fg,
        theme().style(ThemeRole::TreeGit(FileRowGit::Untracked)).fg
    );
    assert_eq!(
        ignored.fg,
        theme().style(ThemeRole::TreeMuted).fg,
        "an ignored entry dims like one generated name"
    );
    assert_ne!(
        modified.fg,
        theme().style(ThemeRole::Text).fg,
        "a changed name separates from an unchanged one"
    );
    assert_eq!(
        sidebar_style(&session, 5, 2).fg,
        theme().style(ThemeRole::TreeDirectory).fg,
        "a directory keeps the title color, because its state rolls up"
    );
}

#[test]
fn the_refresh_command_asks_for_the_repository_state_again() {
    let (dir, mut session) = git_workspace();
    publish_git(&mut session, &dir.path, &git_output());
    assert!(
        session.take_git_request().is_none(),
        "the sidebar runs one status read at a time"
    );

    press(&mut session, 'R');
    drain(&mut session);
    assert_eq!(
        session
            .take_git_request()
            .map(|request| request.root().as_path().to_path_buf()),
        Some(dir.path.clone())
    );
}

/// Refuses every queued language request, like an editor without a server.
///
/// A save waits for the formatter, so the request must be answered before the
/// file request reaches the worker. See `docs/language-services.md`.
fn refuse_language_requests(session: &mut Session) {
    while let Some(request) = session.take_language_request() {
        let _ = session.apply_language_dispatch(&request, Err(LspError::NoServerDeclared));
    }
}

#[test]
fn a_save_asks_for_the_repository_state_again() {
    let dir = TempDir::new("tree-git-save");
    let path = dir.write("main.rs", "one\n");
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    settings.windows.file_tree_icons = FileTreeIcons::Hidden;
    let mut session = Session::new(
        Rect::new(0, 0, WIDTH, HEIGHT),
        settings,
        test_root(dir.path.clone()),
    );
    drain(&mut session);
    let _opening_read = session.take_git_request();

    session.open_path(path);
    refuse_language_requests(&mut session);
    drain_file(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "two ");
    press_code(&mut session, KeyCode::Esc);
    press(&mut session, ':');
    type_keys(&mut session, "w");
    press_code(&mut session, KeyCode::Enter);
    assert!(
        session.take_git_request().is_none(),
        "the tree waits for the save before it reads the repository again"
    );

    refuse_language_requests(&mut session);
    drain_file(&mut session);
    assert!(
        !session.buffer().is_modified(),
        "the save reached the workspace"
    );
    assert!(
        session.take_git_request().is_some(),
        "the saved file changed the working tree"
    );
}

#[test]
fn a_workspace_mutation_asks_for_the_repository_state_again() {
    let (dir, mut session) = git_workspace();
    publish_git(&mut session, &dir.path, &git_output());

    press(&mut session, 'j');
    press(&mut session, 'j');
    press(&mut session, 'r');
    type_keys(&mut session, "renamed.rs");
    press_code(&mut session, KeyCode::Enter);
    drain(&mut session);

    assert!(dir.join("renamed.rs").exists());
    assert!(
        session.take_git_request().is_some(),
        "the rename changed the entries of the workspace"
    );
}

#[test]
fn a_snapshot_of_another_workspace_never_reaches_the_rows() {
    let (dir, mut session) = git_workspace();
    let elsewhere = TempDir::new("git-elsewhere");
    let root = Arc::new(WorktreeRoot::open(&elsewhere.path).expect("the fixture root exists"));
    let answered = |stdout: &[u8]| ProcessOutput {
        status_code: Some(0),
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    };
    let GitStatusRead::Pending(records) = GitStatusRequest::new(root)
        .publish(&answered(b"\n"))
        .expect("the prefix command answered")
    else {
        panic!("the first stage never publishes one snapshot");
    };
    let published = records
        .publish(&answered(b"? new.rs\0"))
        .expect("the status command answered");
    let _ = session.apply_git_result(Ok(published));

    assert_eq!(
        git_mark(&session, 4),
        " ",
        "a snapshot of another root names no entry of this one"
    );
    publish_git(&mut session, &dir.path, &git_output());
    assert_eq!(git_mark(&session, 4), "□");
}

#[test]
fn a_failed_status_read_keeps_the_tree_and_its_marks() {
    let (dir, mut session) = git_workspace();
    publish_git(&mut session, &dir.path, &git_output());

    press(&mut session, 'R');
    drain(&mut session);
    let _refused_read = session.take_git_request();
    assert_eq!(
        session.apply_git_result(Err(GitStatusFailure::Unavailable)),
        Redraw::Skipped
    );

    assert_eq!(
        sidebar_rows(&session).len(),
        6,
        "a failed read keeps every row of the tree"
    );
    assert_eq!(
        git_mark(&session, 3),
        "●",
        "a failed read keeps the marks of the last successful one"
    );
    assert!(
        !message(&session).contains("git"),
        "a failed read reports nothing"
    );
}

#[test]
fn a_missing_git_command_reaches_the_message_line_once() {
    let (_dir, mut session) = git_workspace();
    let _opening_read = session.take_git_request();
    assert_eq!(
        session.apply_git_result(Err(GitStatusFailure::CommandMissing)),
        Redraw::Needed
    );
    assert!(
        message(&session).contains("git"),
        "a missing command is a normal state that the editor names"
    );

    press(&mut session, 'R');
    drain(&mut session);
    let _second_read = session.take_git_request();
    assert_eq!(
        session.apply_git_result(Err(GitStatusFailure::CommandMissing)),
        Redraw::Skipped,
        "the editor names the missing command once for each session"
    );
    assert_eq!(
        sidebar_rows(&session).len(),
        6,
        "the tree stays fully usable without the repository state"
    );
}

/// Builds one coalesced burst that names the change of a single path.
fn watch_batch(root: &Path, path: &Path, kind: WatchKind) -> WatchBatch {
    let mut batch = WatchBatch::default();
    batch.push(
        &WatchEvent::new(test_root(root.to_path_buf()), path.to_path_buf(), kind)
            .expect("the fixture event lies below its worktree root"),
    );
    batch
}

#[test]
fn a_watched_change_reads_only_the_directory_that_it_named() {
    let (dir, mut session) = workspace();
    reveal(&mut session);
    press(&mut session, 'j');
    press(&mut session, 'l');
    drain(&mut session);
    press(&mut session, 'j');

    let selected = session
        .file_tree()
        .selected()
        .expect("the sidebar selects one row")
        .to_path_buf();
    assert_eq!(selected, dir.join("src/main.rs"));

    // Another program adds one file inside the expanded directory.
    let created = dir.file("src/added.rs", "fn added() {}\n");
    // The `docs` directory stays closed, so its listing must not appear.
    dir.file("docs/guide.md", "guide\n");

    let redraw = session.apply_watch_batch(&watch_batch(&dir.path, &created, WatchKind::Created));
    assert_eq!(
        redraw,
        Redraw::Skipped,
        "the burst names a read; the rows change when the read returns"
    );
    drain(&mut session);

    assert_eq!(
        sidebar_rows(&session),
        vec![
            "   ▸ docs".to_owned(),
            "   ▾ src".to_owned(),
            "   │   added.rs".to_owned(),
            "▌  └   main.rs".to_owned(),
            "     README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
        ],
        "the refresh keeps the expansion and the selection, and reads no closed directory"
    );
    assert_eq!(
        session.file_tree().selected(),
        Some(selected.as_path()),
        "the selection stays on the same entry"
    );
}

#[test]
fn a_watched_removal_drops_the_entry_and_keeps_the_expansion() {
    let (dir, mut session) = workspace();
    reveal(&mut session);
    press(&mut session, 'j');
    press(&mut session, 'l');
    drain(&mut session);

    let removed = dir.join("src/main.rs");
    fs::remove_file(&removed).expect("the temporary directory is writable");

    let _ = session.apply_watch_batch(&watch_batch(&dir.path, &removed, WatchKind::Removed));
    drain(&mut session);

    assert_eq!(
        sidebar_rows(&session),
        vec![
            "   ▸ docs".to_owned(),
            "▌  ▾ src".to_owned(),
            "     README.md".to_owned(),
            "     (1 hidden item)".to_owned(),
        ],
        "the removed entry leaves the rows and the directory stays open"
    );
}

#[test]
fn a_watched_burst_that_lost_events_reads_every_expanded_directory() {
    let (dir, mut session) = workspace();
    reveal(&mut session);

    // The burst names one directory that holds no change, and it reports that a
    // bound dropped events, so the sidebar must not trust the named set.
    let elsewhere = dir.file("src/added.rs", "fn added() {}\n");
    dir.file("late.txt", "late\n");
    let mut batch = watch_batch(&dir.path, &elsewhere, WatchKind::Created);
    batch.drop_events();

    let _ = session.apply_watch_batch(&batch);
    drain(&mut session);

    assert_eq!(
        sidebar_rows(&session),
        vec![
            "▌  ▸ docs".to_owned(),
            "   ▸ src".to_owned(),
            "     README.md".to_owned(),
            "     late.txt".to_owned(),
            "     (1 hidden item)".to_owned(),
        ],
        "a dropped burst reads every expanded directory again"
    );
}

#[test]
fn a_watched_content_change_reads_no_directory() {
    let (dir, mut session) = workspace();
    reveal(&mut session);

    // Another program rewrites one file. Its directory keeps the same entries,
    // so the burst asks for no read at all.
    let modified = dir.file("README.md", "changed\n");
    let _ = session.apply_watch_batch(&watch_batch(&dir.path, &modified, WatchKind::Modified));

    assert!(
        session.take_workspace_request().is_none(),
        "a content change asks for no directory read"
    );
    assert!(
        session.take_git_request().is_some(),
        "a content change asks for the repository state again"
    );
}

#[test]
fn a_refused_workspace_watch_reports_once() {
    let (_dir, mut session) = workspace();

    assert_eq!(session.report_watch_unavailable(), Redraw::Needed);
    assert_eq!(
        message(&session),
        "the workspace watcher could not start; the file tree updates on a refresh"
    );

    assert_eq!(
        session.report_watch_unavailable(),
        Redraw::Skipped,
        "the editor names the refused watch once for each session"
    );
}

/// Builds one burst that carries the named coverage and no change.
fn coverage_batch(coverage: WatchCoverage) -> WatchBatch {
    let mut batch = WatchBatch::default();
    batch.set_coverage(coverage);
    batch
}

#[test]
fn a_complete_workspace_watch_reports_nothing() {
    let (_dir, mut session) = workspace();

    assert_eq!(
        session.apply_watch_batch(&coverage_batch(WatchCoverage::default())),
        Redraw::Skipped
    );
    assert_eq!(
        message(&session),
        "",
        "a registration that watches every directory names no gap"
    );
}

#[test]
fn a_workspace_watch_that_the_host_refused_reports_its_cause_once() {
    let (_dir, mut session) = workspace();

    let refused = coverage_batch(WatchCoverage {
        refused: 812,
        at_limit: true,
        truncated: false,
    });
    assert_eq!(session.apply_watch_batch(&refused), Redraw::Needed);
    assert!(
        message(&session).starts_with("the host refused 812 workspace watches"),
        "the report names the directories that carry no watch: `{}`",
        message(&session)
    );

    assert_eq!(
        session.apply_watch_batch(&refused),
        Redraw::Skipped,
        "the editor names the state of the watch once for each session"
    );
    assert_eq!(
        session.report_watch_unavailable(),
        Redraw::Skipped,
        "one flag holds every report of the watch, so a later report adds no noise"
    );

    reveal(&mut session);
    assert!(
        sidebar_rows(&session).len() > 1,
        "the tree stays fully usable while the host watches a part of the workspace"
    );
}

#[test]
fn a_workspace_watch_that_the_bounds_truncated_names_the_editor() {
    let (_dir, mut session) = workspace();

    let truncated = coverage_batch(WatchCoverage {
        refused: 0,
        at_limit: false,
        truncated: true,
    });
    assert_eq!(session.apply_watch_batch(&truncated), Redraw::Needed);
    assert_eq!(
        message(&session),
        "the workspace passes the watch bounds of the editor; the file tree updates on a refresh",
        "a bound of the editor is no refusal of the host, so the report names another action"
    );
}

#[test]
fn the_report_of_a_refused_watch_names_the_setting_that_holds_the_limit() {
    let at_limit = WatchCoverage {
        refused: 3,
        at_limit: true,
        truncated: false,
    };

    assert_eq!(
        watch_coverage_note(at_limit, Some("fs.inotify.max_user_watches")),
        "the host refused 3 workspace watches; raise `fs.inotify.max_user_watches`",
        "the user raises the limit of the host, so the report names it"
    );
    assert_eq!(
        watch_coverage_note(at_limit, None),
        "the host refused 3 workspace watches; raise the watch limit of the host",
        "a platform that publishes no name still reports the cause"
    );
    assert_eq!(
        watch_coverage_note(
            WatchCoverage {
                refused: 3,
                at_limit: false,
                truncated: true,
            },
            None
        ),
        "the host refused 3 workspace watches and the workspace passes the watch bounds of the \
         editor; the file tree updates on a refresh",
        "one report names both causes"
    );
}

#[test]
fn a_created_file_reaches_the_editor_through_the_workspace_watcher() {
    let dir = TempDir::new("watch");
    dir.file("src/main.rs", "fn main() {}\n");
    let root = dir.path.clone();
    let created = dir.join("src/added.rs");

    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the test host provides a Tokio runtime");
    let batch = tokio.block_on(async {
        let mut watcher = FileWatcher::start(test_root(root), &GENERATED_NAMES)
            .expect("the root is a readable directory");
        // The registration runs after the start, so the first burst reports the
        // window that no watch covered. The write follows that burst, so every
        // watch of the workspace already stands.
        let opening = tokio::time::timeout(WATCH_COALESCE_WINDOW * 20, watcher.recv())
            .await
            .expect("the registration finishes inside the bound")
            .expect("the task opens the stream with one burst");
        assert_eq!(opening.fidelity(), WatchFidelity::Dropped);
        fs::write(&created, "fn added() {}\n").expect("the temporary directory is writable");
        // The window of one burst plus a generous margin, so a slow host still
        // reports before the bound ends the wait.
        let batch = tokio::time::timeout(WATCH_COALESCE_WINDOW * 20, watcher.recv()).await;
        let batch = batch.expect("the watcher reports inside the bound");
        watcher.shutdown().await;
        batch
    });

    let batch = batch.expect("the coalescing task publishes the burst");
    assert!(
        batch.directories().contains(
            &created
                .parent()
                .expect("the file lies in a directory")
                .to_path_buf()
        ),
        "the burst names the directory that changed"
    );
}

/// Returns one notice row of the workspace root.
fn notice_row(notice: Notice) -> TreeRow {
    TreeRow {
        path: PathBuf::from("/workspace"),
        depth: 0,
        content: RowContent::Notice(notice),
        matched: None,
    }
}

#[test]
fn a_truncated_listing_never_reads_like_a_count_of_hidden_entries() {
    // A truncated listing keeps entries out that the reader expects, so it
    // warns. A hidden count reports a choice of the reader instead. The
    // entry bound of the tree is far above a practical test workspace, so
    // the row builder answers this question directly.
    let truncated = RowState::of(
        &notice_row(Notice::Truncated {
            shown: 8192,
            total: 9000,
        }),
        None,
        None,
    );
    let counted = RowState::of(&notice_row(Notice::Hidden { count: 5 }), None, None);
    assert_eq!(truncated.role(), ThemeRole::TreeIncomplete);
    assert_eq!(counted.role(), ThemeRole::TreeNotice);
    assert_eq!(
        RowState::of(&notice_row(Notice::Unreadable), None, None).role(),
        ThemeRole::TreeIncomplete,
        "a failed read warns like a truncated one"
    );
}

/// Returns one file row of the workspace root.
fn file_row(name: &str) -> TreeRow {
    TreeRow {
        path: PathBuf::from("/workspace").join(name),
        depth: 0,
        content: RowContent::File {
            name: name.to_owned(),
            link: LinkKind::Direct,
        },
        matched: None,
    }
}

#[test]
fn an_ignored_entry_dims_like_one_generated_name() {
    // The fixed generated list and the Git ignore rules must not disagree
    // about one row, so both reach the same state. See `docs/git.md`.
    let ignored = RowState::of(&file_row("kvim.log"), None, Some(GitStatus::Ignored));
    let generated = RowState::of(&file_row("node_modules"), None, None);
    assert_eq!(ignored, generated);
    assert_eq!(ignored.role(), ThemeRole::TreeMuted);
}

#[test]
fn a_notice_row_never_borrows_the_state_of_its_directory() {
    let row = notice_row(Notice::Hidden { count: 5 });
    assert!(!row.is_selectable());
    assert_eq!(
        RowState::of(&row, None, Some(GitStatus::Ignored)).role(),
        ThemeRole::TreeNotice
    );
}

#[test]
fn a_held_entry_keeps_its_pending_operation_over_the_git_state() {
    // The pending file operation is the report that the reader waits for,
    // so it wins over the recorded state of the repository.
    let held = RowState::of(
        &file_row("main.rs"),
        Some(TransferMode::Copy),
        Some(GitStatus::Modified),
    );
    assert_eq!(held, RowState::Held(TransferMode::Copy));
    assert_eq!(held.suffix(), " (copied)");
}

#[test]
fn the_leader_belongs_to_the_leader_in_the_sidebar() {
    let (_dir, mut session) = workspace();
    reveal(&mut session);

    // Space opens the leader sequence instead of acting on the selected row.
    // `l` and `h` already open and close an entry.
    let before = sidebar_row(&session, 1);
    press(&mut session, ' ');
    assert_eq!(
        sidebar_row(&session, 1),
        before,
        "the leader key changes no row of the sidebar"
    );

    // The sequence reaches its command from the sidebar, as it does from a
    // buffer.
    press(&mut session, 'g');
    press(&mut session, 'g');
    assert_eq!(
        session.input_context().scope,
        BindingScope::Review,
        "`<leader>gg` opens the review from the sidebar"
    );
}

#[test]
fn a_watched_change_captures_the_open_review_again() {
    let (dir, mut session) = workspace();
    let changed = dir.join("docs");

    // A change while no review is open queues no capture.
    let _ = session.apply_watch_batch(&watch_batch(&dir.path, &changed, WatchKind::Modified));
    assert!(
        session.take_diff_request().is_none(),
        "a closed review captures nothing"
    );

    // The review opens and asks for both halves.
    type_keys(&mut session, " gg");
    let _staged = session.take_diff_request().expect("the staged half");
    let _unstaged = session.take_diff_request().expect("the unstaged half");
    assert!(session.take_diff_request().is_none());

    // A change of the worktree captures it again, so an agent that writes
    // files updates the diff without a key.
    let _ = session.apply_watch_batch(&watch_batch(&dir.path, &changed, WatchKind::Modified));
    assert!(
        session.take_diff_request().is_some(),
        "the open review captures the change"
    );
    let _ = session.take_diff_request();

    // A second change while captures wait queues nothing further, so a burst
    // never grows the outbox.
    type_keys(&mut session, " gg");
    let _ = session.apply_watch_batch(&watch_batch(&dir.path, &changed, WatchKind::Modified));
    let mut queued = 0;
    while session.take_diff_request().is_some() {
        queued += 1;
        assert!(queued < 8, "the outbox stays bounded");
    }
}

#[test]
fn the_refresh_key_captures_the_review_again() {
    // The file watch skips the Git directory, so a staged change reaches no
    // burst. The reader asks for the capture instead.
    let (_dir, mut session) = workspace();
    type_keys(&mut session, " gg");
    while session.take_diff_request().is_some() {}

    press(&mut session, 'R');

    let mut queued = 0;
    while session.take_diff_request().is_some() {
        queued += 1;
    }
    assert_eq!(queued, 2, "the refresh asks for both halves");
}
