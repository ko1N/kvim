//! Tests for the pure state transitions of the event loop.
//!
//! No test opens a terminal. The session receives normalized events and an
//! elapsed time, so every transition is deterministic.

use std::path::PathBuf;
use std::time::Duration;

use ratatui::layout::Rect;
use tokio_util::sync::CancellationToken;

use kvim_clipboard::{CLIPBOARD_BYTES_MAX, ClipboardFailure};
use kvim_editor::Selection;
use kvim_input::Mode;
use kvim_language::LspError;
use kvim_runtime::{ProcessOutput, WatchBatch, WatchEvent, WatchKind};
use kvim_settings::{EditorSettings, WHICH_KEY_DELAY_DEFAULT};
use kvim_terminal::{FocusChange, Key, KeyCode, TerminalEvent};
use kvim_workspace::ExternalChange;
use kvim_workspace::temp::TempDir;

use super::clipboard::SessionClipboard;
use super::language::{LanguageRequest, LanguageRequestKind};
use super::session::{MessageLevel, Redraw, RunState, Session};
use super::window::WindowId;

const NOW: Duration = Duration::ZERO;

/// Returns the workspace root that the file tree of a test session shows.
///
/// No test reads the directory, because the session hands every read to the
/// bounded worker service.
fn workspace_root() -> PathBuf {
    PathBuf::from("/workspace")
}

/// The which-key delay of the settings that every test session holds.
const WHICH_KEY_DELAY: Duration = WHICH_KEY_DELAY_DEFAULT;

/// Creates a session over one terminal size.
fn session(width: u16, height: u16) -> Session {
    Session::new(
        Rect::new(0, 0, width, height),
        EditorSettings::default(),
        workspace_root(),
    )
}

/// Feeds one plain character key and returns the redraw request.
fn press(session: &mut Session, value: char) -> Redraw {
    session.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char(value))), NOW)
}

/// Feeds one plain key without a character.
fn press_code(session: &mut Session, code: KeyCode) -> Redraw {
    session.handle_event(TerminalEvent::Key(Key::plain(code)), NOW)
}

/// Feeds a run of plain character keys.
fn type_keys(session: &mut Session, keys: &str) {
    for value in keys.chars() {
        press(session, value);
    }
}

/// Returns the message text, or an empty text while the line is empty.
fn message(session: &Session) -> String {
    session
        .message()
        .map_or_else(String::new, |message| message.text().to_owned())
}

/// Creates a session that holds the given lines, with the cursor at the start.
fn with_text(lines: &[&str]) -> Session {
    let mut session = session(60, 20);
    press(&mut session, 'i');
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            press_code(&mut session, KeyCode::Enter);
        }
        type_keys(&mut session, line);
    }
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "gg");
    session
}

/// Returns the selection of the active Visual mode.
fn selection(session: &Session) -> Option<Selection> {
    session.selection()
}

#[test]
fn the_mode_follows_the_mode_commands_and_returns_with_escape() {
    let mut session = session(40, 10);
    assert_eq!(session.mode(), Mode::Normal);
    for (keys, expected) in [
        ("i", Mode::Insert),
        ("v", Mode::Visual),
        ("V", Mode::VisualLine),
    ] {
        type_keys(&mut session, keys);
        assert_eq!(session.mode(), expected, "`{keys}` must reach {expected}");
        press_code(&mut session, KeyCode::Esc);
        assert_eq!(session.mode(), Mode::Normal);
    }
}

#[test]
fn insert_mode_typing_reaches_the_buffer_including_digits() {
    let mut session = session(40, 10);
    press(&mut session, 'i');
    // A digit is buffer text in Insert mode, never a command count.
    type_keys(&mut session, "let x = 42;");
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "y");
    assert_eq!(session.buffer().to_string(), "let x = 42;\ny\n");
    assert!(session.buffer().is_modified());

    // The same digit opens a count again after the mode returns to Normal.
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "2gg");
    assert_eq!(session.buffer().to_string(), "let x = 42;\ny\n");
}

#[test]
fn insert_mode_wires_enter_and_backspace_to_the_editor_entry_points() {
    let mut session = session(40, 10);
    press(&mut session, 'i');
    type_keys(&mut session, "    alpha");

    // `Enter` copies the indent of the previous non-empty line.
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "beta");
    assert_eq!(session.buffer().to_string(), "    alpha\n    beta\n");

    // `Backspace` deletes one character at a time.
    for _ in 0..4 {
        press_code(&mut session, KeyCode::Backspace);
    }
    assert_eq!(session.buffer().to_string(), "    alpha\n    \n");

    // At column zero it joins the cursor line with the line above it.
    for _ in 0..5 {
        press_code(&mut session, KeyCode::Backspace);
    }
    assert_eq!(session.buffer().to_string(), "    alpha\n");
}

#[test]
fn the_tab_key_follows_the_indent_settings() {
    let mut soft = session(40, 10);
    press(&mut soft, 'i');
    press_code(&mut soft, KeyCode::Tab);
    assert_eq!(soft.buffer().to_string(), "    \n");

    let mut settings = EditorSettings::default();
    settings.indent.expand_tab = false;
    let mut hard = Session::new(Rect::new(0, 0, 40, 10), settings, workspace_root());
    press(&mut hard, 'i');
    press_code(&mut hard, KeyCode::Tab);
    assert_eq!(hard.buffer().to_string(), "\t\n");
}

#[test]
fn a_window_command_changes_the_tree_and_the_last_close_ends_the_session() {
    let mut session = session(80, 20);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    assert_eq!(session.windows().window_count(), 2);
    assert_eq!(session.run_state(), RunState::Running);

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('q'))), NOW);
    assert_eq!(session.windows().window_count(), 1);
    assert_eq!(session.run_state(), RunState::Running);

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('q'))), NOW);
    assert_eq!(
        session.run_state(),
        RunState::Finished,
        "closing the last window ends the editor"
    );
}

#[test]
fn only_a_visible_change_requests_a_new_frame() {
    let mut session = session(40, 10);
    // A focus change moves no cursor and shows no new text.
    assert_eq!(
        session.handle_event(TerminalEvent::Focus(FocusChange::Lost), NOW),
        Redraw::Skipped
    );
    // A resize to the same size changes no rectangle.
    assert_eq!(
        session.handle_event(
            TerminalEvent::Resize {
                columns: 40,
                rows: 10
            },
            NOW
        ),
        Redraw::Skipped
    );
    assert_eq!(
        session.handle_event(
            TerminalEvent::Resize {
                columns: 30,
                rows: 8
            },
            NOW
        ),
        Redraw::Needed
    );
    // A pending sequence shows nothing until the which-key delay passes.
    assert_eq!(press(&mut session, 'g'), Redraw::Skipped);
    assert_eq!(press(&mut session, 'g'), Redraw::Needed);
}

#[test]
fn the_which_key_deadline_is_the_only_time_driven_change() {
    let mut session = session(60, 20);
    assert_eq!(session.next_deadline(), None, "no sequence is pending");

    press(&mut session, ' ');
    assert_eq!(
        session.next_deadline(),
        Some(WHICH_KEY_DELAY),
        "the loop wakes when the overlay appears"
    );
    assert_eq!(session.tick(WHICH_KEY_DELAY), Redraw::Needed);
    assert_eq!(
        session.next_deadline(),
        None,
        "the overlay is visible, and the sequence itself never expires"
    );
    // The sequence survives every later tick, so the user keeps reading.
    assert_eq!(session.tick(Duration::from_secs(3_600)), Redraw::Skipped);
    press(&mut session, 'q');
    assert_eq!(
        session.run_state(),
        RunState::Finished,
        "the late key still completes `Space q`"
    );
}

#[test]
fn a_cancel_key_hides_the_overlay_and_keeps_the_mode() {
    for cancel in [
        TerminalEvent::Key(Key::plain(KeyCode::Esc)),
        TerminalEvent::Key(Key::ctrl(KeyCode::Char('c'))),
    ] {
        let mut session = session(60, 20);
        press(&mut session, 'v');
        type_keys(&mut session, " ");
        assert_eq!(session.tick(WHICH_KEY_DELAY), Redraw::Needed);
        assert_eq!(session.next_deadline(), None);

        assert_eq!(session.handle_event(cancel, NOW), Redraw::Needed);
        assert_eq!(
            session.mode(),
            Mode::Visual,
            "a cancel of pending input keeps the mode"
        );
        // A second cancel leaves the mode, because no input is pending.
        session.handle_event(cancel, NOW);
        assert_eq!(session.mode(), Mode::Normal);
    }
}

#[test]
fn the_visual_modes_switch_between_each_other_and_keep_the_anchor() {
    let control_v = TerminalEvent::Key(Key::ctrl(KeyCode::Char('v')));
    let cases: [(&str, Mode); 9] = [
        ("v", Mode::Visual),
        ("vV", Mode::VisualLine),
        ("vv", Mode::Normal),
        ("vVv", Mode::Visual),
        ("vVV", Mode::Normal),
        ("V", Mode::VisualLine),
        ("Vv", Mode::Visual),
        ("VV", Mode::Normal),
        ("vVvV", Mode::VisualLine),
    ];
    for (keys, expected) in cases {
        let mut session = with_text(&["alpha beta", "gamma delta"]);
        type_keys(&mut session, "jll");
        type_keys(&mut session, keys);
        assert_eq!(session.mode(), expected, "`{keys}` must reach {expected}");
    }

    // `Ctrl-V` completes the matrix and repeats into Normal mode.
    let mut session = with_text(&["alpha beta", "gamma delta"]);
    type_keys(&mut session, "v");
    session.handle_event(control_v, NOW);
    assert_eq!(session.mode(), Mode::VisualBlock);
    type_keys(&mut session, "V");
    assert_eq!(session.mode(), Mode::VisualLine);
    session.handle_event(control_v, NOW);
    assert_eq!(session.mode(), Mode::VisualBlock);
    session.handle_event(control_v, NOW);
    assert_eq!(session.mode(), Mode::Normal);

    // The anchor survives the switch: the selection still starts where `v` did.
    let mut session = with_text(&["alpha beta", "gamma delta"]);
    type_keys(&mut session, "vlll");
    let before = selection(&session).expect("a Visual mode always holds a selection");
    type_keys(&mut session, "V");
    let after = selection(&session).expect("a Visual mode always holds a selection");
    assert_ne!(
        std::mem::discriminant(&before),
        std::mem::discriminant(&after),
        "only the shape of the selection changes"
    );
    type_keys(&mut session, "v");
    assert_eq!(
        selection(&session),
        Some(before),
        "the anchor and the cursor return the original selection"
    );
}

#[test]
fn the_command_line_runs_the_fixed_command_set_and_rejects_the_rest() {
    let mut session = session(60, 12);
    press(&mut session, 'i');
    type_keys(&mut session, "one");
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "two");
    press_code(&mut session, KeyCode::Esc);

    // `:<number>` moves the cursor to that line.
    press(&mut session, ':');
    type_keys(&mut session, "1");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.mode(), Mode::Normal);
    assert_eq!(message(&session), "");

    // Every unknown line is a typed rejection.
    press(&mut session, ':');
    type_keys(&mut session, "wqa");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );

    // A scratch buffer holds no file name, so `:w` needs one first.
    press(&mut session, ':');
    type_keys(&mut session, "w");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        message(&session),
        "the buffer holds no file name; use :e <path> to name one"
    );

    // `:q` refuses to discard the unsaved changes.
    press(&mut session, ':');
    type_keys(&mut session, "q");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Running);
    assert_eq!(
        message(&session),
        "the buffer holds unsaved changes; use :q! to discard them"
    );

    // `:q!` discards them and ends the editor.
    press(&mut session, ':');
    type_keys(&mut session, "q!");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn a_cancelled_prompt_runs_no_command_and_gives_input_back_to_the_registry() {
    let mut session = session(40, 10);
    press(&mut session, ':');
    // The prompt owns input, so `q` becomes prompt text instead of a command.
    type_keys(&mut session, "q");
    assert_eq!(session.run_state(), RunState::Running);
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        session.run_state(),
        RunState::Running,
        "a cancelled command line never runs its line"
    );
    assert_eq!(session.mode(), Mode::Normal);
    press(&mut session, 'i');
    assert_eq!(
        session.mode(),
        Mode::Insert,
        "the registry owns input again"
    );
}

#[test]
fn a_backspace_on_the_empty_prompt_closes_it() {
    let mut session = session(40, 10);
    press(&mut session, ':');
    type_keys(&mut session, "q");
    press_code(&mut session, KeyCode::Backspace);
    press_code(&mut session, KeyCode::Backspace);
    // The prompt is closed, so the next key reaches the registry again.
    press(&mut session, 'i');
    assert_eq!(session.mode(), Mode::Insert);
}

#[test]
fn a_search_without_a_match_reports_it_and_keeps_the_cursor() {
    let mut session = session(40, 10);
    press(&mut session, 'i');
    type_keys(&mut session, "alpha");
    press_code(&mut session, KeyCode::Esc);

    press(&mut session, '/');
    type_keys(&mut session, "zeta");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(message(&session), "no match");
    assert_eq!(session.buffer().to_string(), "alpha\n");
}

#[test]
fn a_new_command_clears_the_previous_message() {
    let mut session = session(60, 10);
    press(&mut session, 'u');
    assert_eq!(message(&session), "no further change");
    press(&mut session, 'j');
    assert_eq!(message(&session), "");
}

#[test]
fn an_exhausted_history_reports_instead_of_changing_the_buffer() {
    let mut session = session(40, 10);
    press(&mut session, 'u');
    assert_eq!(message(&session), "no further change");
    assert_eq!(session.buffer().to_string(), "\n");
}

#[test]
fn a_terminal_resize_keeps_every_window_identity() {
    let mut session = session(80, 24);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let before = session.windows().window_ids();

    session.handle_event(
        TerminalEvent::Resize {
            columns: 50,
            rows: 12,
        },
        NOW,
    );
    assert_eq!(session.windows().window_ids(), before);
    assert_eq!(session.area(), Rect::new(0, 0, 50, 12));
}

#[test]
fn the_viewport_follows_the_text_area_instead_of_the_window_rectangle() {
    let mut session = session(40, 12);
    press(&mut session, 'i');
    for index in 0..40 {
        if index > 0 {
            press_code(&mut session, KeyCode::Enter);
        }
        type_keys(&mut session, &format!("line{index}"));
    }
    press_code(&mut session, KeyCode::Esc);

    // The terminal holds twelve rows: one winbar, nine text rows, one
    // statusline, and one message line. The viewport must report the nine text
    // rows, so the scroll margin applies to the cells that the reader sees.
    let window = session.windows().focused_window();
    let viewport = session
        .windows()
        .viewport(window)
        .expect("the focused window is always visible");
    assert_eq!(viewport.height_rows().get(), 9);
    assert_eq!(
        viewport.width_cells().get(),
        35,
        "the gutter takes five of the forty cells"
    );
    // The cursor sits on the last line, so the view keeps it visible.
    assert!(viewport.first_line() + 9 > 39);
}

/// Creates a session that keeps no persistent undo file.
///
/// The tests below save real files. The undo file would reach the editor state
/// directory of the user, so these sessions keep it off.
fn file_session() -> Session {
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    Session::new(Rect::new(0, 0, 80, 24), settings, workspace_root())
}

/// Refuses every queued language request, like an editor without a server.
///
/// The event loop performs the same step, so a save that waits for a formatter
/// continues instead of stalling. See `docs/language-services.md`.
fn refuse_language_requests(session: &mut Session) {
    asks_a_question(session);
}

/// Refuses every queued request and reports whether one asked a question.
///
/// A save that formats first asks its language server before it writes, so the
/// answer distinguishes a formatting save from a plain save.
fn asks_a_question(session: &mut Session) -> bool {
    let mut asked = false;
    while let Some(request) = session.take_language_request() {
        let kind = request.kind();
        asked |= kind == LanguageRequestKind::Query;
        let _ = session.apply_language_dispatch(kind, Err(LspError::NoServerDeclared));
    }
    asked
}

/// Runs the queued file request, like the event loop and the worker service.
fn run_file_request(session: &mut Session) {
    refuse_language_requests(session);
    let request = session
        .take_file_request()
        .expect("the transition queued one file request");
    let _ = session.apply_file_result(request.run());
}

#[test]
fn a_path_opens_one_buffer_and_ctrl_s_writes_it() {
    let directory = TempDir::new("session-save");
    let path = directory.write("main.rs", "fn main() {}\n");
    let mut session = file_session();

    session.open_path(path.clone());
    run_file_request(&mut session);
    assert_eq!(
        session.buffers().len(),
        2,
        "the file joins the scratch buffer"
    );
    assert_eq!(session.buffer().to_string(), "fn main() {}\n");
    assert_eq!(session.active_buffer().name(), "main.rs");
    assert!(!session.buffer().is_modified());

    press(&mut session, 'i');
    type_keys(&mut session, "// note");
    press_code(&mut session, KeyCode::Enter);
    assert!(session.buffer().is_modified());

    // `Ctrl-S` saves from every mode and forces no mode transition.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert_eq!(session.mode(), Mode::Insert);
    run_file_request(&mut session);
    assert_eq!(session.mode(), Mode::Insert);
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file exists"),
        "// note\nfn main() {}\n"
    );
    assert!(
        !session.buffer().is_modified(),
        "a successful save clears the dirty state"
    );

    // The saved buffer leaves the editor without a refusal.
    press(&mut session, ':');
    type_keys(&mut session, "q");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn one_file_reaches_one_buffer_however_the_user_spells_its_path() {
    let directory = TempDir::new("session-duplicate");
    let path = directory.write("main.rs", "one\n");
    let nested = directory.dir("nested");
    let mut session = file_session();

    session.open_path(path);
    run_file_request(&mut session);
    let first = session.active();
    let loaded_path = session
        .active_buffer()
        .path()
        .expect("the buffer holds the file")
        .to_path_buf();

    // The recorded path needs no file read at all.
    session.open_path(loaded_path);
    assert!(session.take_file_request().is_none());
    assert_eq!(session.active(), first);

    // Another spelling of the same file reaches the same buffer after the load.
    // The parent step keeps the two paths distinct on every host, because the
    // comparison of two paths drops a `.` component but keeps a `..` component.
    session.open_path(nested.join("..").join("main.rs"));
    run_file_request(&mut session);
    assert_eq!(session.active(), first);
    assert_eq!(session.buffers().len(), 2);
}

#[test]
fn a_conflict_keeps_the_buffer_dirty_and_usable() {
    let directory = TempDir::new("session-conflict");
    let path = directory.write("main.rs", "one\n");
    let mut session = file_session();

    session.open_path(path.clone());
    run_file_request(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "two");
    press_code(&mut session, KeyCode::Esc);

    std::fs::write(&path, "another program wrote this\n").expect("the file is writable");
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);

    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file exists"),
        "another program wrote this\n",
        "a conflict never overwrites the file"
    );
    assert!(session.buffer().is_modified());

    // The buffer stays usable after the refused save.
    press(&mut session, 'o');
    type_keys(&mut session, "three");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "twoone\nthree\n");
}

#[test]
fn a_failed_save_keeps_the_buffer_usable() {
    let directory = TempDir::new("session-failure");
    let mut session = file_session();

    // The path holds no file yet, so the open starts a new empty buffer. Its
    // directory is missing, so no write can succeed.
    session.open_path(directory.join("missing").join("main.rs"));
    run_file_request(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "text");
    press_code(&mut session, KeyCode::Esc);

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );
    assert!(session.buffer().is_modified());
    assert_eq!(session.buffer().to_string(), "text\n");
}

#[test]
fn write_quit_saves_the_buffer_and_then_ends_the_editor() {
    let directory = TempDir::new("session-write-quit");
    let path = directory.write("main.rs", "one\n");
    let mut session = file_session();

    session.open_path(path.clone());
    run_file_request(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "two ");
    press_code(&mut session, KeyCode::Esc);

    press(&mut session, ':');
    type_keys(&mut session, "wq");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        session.run_state(),
        RunState::Running,
        "the editor waits for the save result"
    );
    run_file_request(&mut session);

    assert_eq!(
        std::fs::read_to_string(&path).expect("the file exists"),
        "two one\n"
    );
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn space_x_unloads_a_clean_buffer_and_refuses_a_dirty_buffer() {
    let directory = TempDir::new("session-unload");
    let path = directory.write("main.rs", "one\n");
    let mut session = file_session();

    session.open_path(path);
    run_file_request(&mut session);
    let loaded = session.active();

    // Insert mode records one transaction for each key, so one undo reverses
    // one character.
    press(&mut session, 'i');
    type_keys(&mut session, "z");
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, " x");
    assert_eq!(session.active(), loaded, "a dirty buffer stays loaded");
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );

    press(&mut session, 'u');
    assert!(!session.buffer().is_modified());
    type_keys(&mut session, " x");
    assert_ne!(session.active(), loaded);
    assert_eq!(session.buffers().len(), 1);
    assert_eq!(
        session.windows().buffer(session.windows().focused_window()),
        Some(session.active()),
        "every window follows the unload"
    );
}

/// Reports one workspace change, like the coalesced burst of the watcher.
///
/// A content change names no path at all, so one burst asks the session to
/// check every loaded buffer against its file.
fn report_watch_change(session: &mut Session) -> Redraw {
    let mut batch = WatchBatch::default();
    batch.push(&WatchEvent {
        path: workspace_root().join("changed"),
        kind: WatchKind::Modified,
    });
    session.apply_watch_batch(&batch)
}

/// Runs the reload check that one workspace change queued.
fn run_watch_reload(session: &mut Session) {
    let _ = report_watch_change(session);
    let request = session
        .take_file_request()
        .expect("the burst queued one reload check");
    let _ = session.apply_file_result(request.run());
}

/// Returns the external-change marker of the active buffer.
fn external(session: &Session) -> Option<ExternalChange> {
    session.active_buffer().external_change()
}

/// Opens one file in a session that keeps no persistent undo file.
fn opened_file(label: &str, name: &str, text: &str) -> (TempDir, PathBuf, Session) {
    let directory = TempDir::new(label);
    let path = directory.write(name, text);
    let mut session = file_session();
    session.open_path(path.clone());
    run_file_request(&mut session);
    (directory, path, session)
}

#[test]
fn a_dirty_buffer_never_reloads_and_reports_the_external_change_once() {
    let (_directory, path, mut session) = opened_file("session-reload-dirty", "main.rs", "one\n");

    press(&mut session, 'i');
    type_keys(&mut session, "edited ");
    press_code(&mut session, KeyCode::Esc);
    assert!(session.buffer().is_modified());

    std::fs::write(&path, "another program wrote a much longer line\n")
        .expect("the file is writable");
    run_watch_reload(&mut session);

    assert_eq!(
        session.buffer().to_string(),
        "edited one\n",
        "a buffer with unsaved changes never reloads"
    );
    assert!(session.buffer().is_modified());
    assert_eq!(external(&session), Some(ExternalChange::Changed));
    assert_eq!(
        message(&session),
        "main.rs changed on disk; the buffer keeps its unsaved changes"
    );

    // The editor reports one external change once, so a workspace that changes
    // often never fills the message line.
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(message(&session), "");
    run_watch_reload(&mut session);
    assert_eq!(message(&session), "");
    assert_eq!(session.buffer().to_string(), "edited one\n");
}

#[test]
fn a_clean_buffer_reloads_after_an_external_change() {
    let (_directory, path, mut session) = opened_file("session-reload-clean", "main.rs", "one\n");

    // A file that keeps its length reports no change, so the test changes it.
    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(message(&session), "", "the open message is cleared");
    run_watch_reload(&mut session);

    assert_eq!(session.buffer().to_string(), "one\ntwo\n");
    assert!(!session.buffer().is_modified());
    assert_eq!(external(&session), None);
    assert_eq!(message(&session), "", "a background reload reports nothing");

    // The reload recorded the new file state, so the next save is no conflict.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    assert!(!session.buffer().is_modified());
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file exists"),
        "one\ntwo\n"
    );
}

#[test]
fn a_buffer_that_no_window_shows_reloads_in_the_background() {
    let directory = TempDir::new("session-reload-background");
    let first = directory.write("first.rs", "first\n");
    let second = directory.write("second.rs", "second\n");
    let mut session = file_session();

    session.open_path(second.clone());
    run_file_request(&mut session);
    let background = session.active();
    session.open_path(first);
    run_file_request(&mut session);
    assert_ne!(session.active(), background);

    std::fs::write(&second, "second, and changed\n").expect("the file is writable");
    run_watch_reload(&mut session);

    let reloaded = session
        .buffers()
        .get(background)
        .expect("the background buffer stays loaded");
    assert_eq!(reloaded.text().to_string(), "second, and changed\n");
    assert!(!reloaded.text().is_modified());
}

#[test]
fn a_reload_keeps_the_cursor_and_clamps_it_into_a_shorter_file() {
    let (_directory, path, mut session) = opened_file(
        "session-reload-cursor",
        "main.rs",
        "one\ntwo\nthree\nfour\nfive\n",
    );

    type_keys(&mut session, "jj");
    assert_eq!(session.cursor().line().get(), 2);

    // A file that keeps the cursor line keeps the cursor.
    std::fs::write(&path, "one\ntwo\nthree, longer\nfour\nfive\n").expect("the file is writable");
    run_watch_reload(&mut session);
    assert_eq!(session.cursor().line().get(), 2);

    // A file that became shorter clamps the cursor and the viewport.
    std::fs::write(&path, "one\n").expect("the file is writable");
    run_watch_reload(&mut session);
    assert_eq!(session.buffer().to_string(), "one\n");
    assert_eq!(session.cursor().line().get(), 0);
    assert_eq!(
        session
            .windows()
            .state(session.windows().focused_window())
            .expect("the focused window is a leaf")
            .first_line(),
        0
    );
}

#[test]
fn a_deleted_file_keeps_its_buffer_editable_and_reports_it() {
    let (_directory, path, mut session) = opened_file("session-reload-deleted", "main.rs", "one\n");

    std::fs::remove_file(&path).expect("the file exists");
    run_watch_reload(&mut session);

    assert_eq!(
        session.buffer().to_string(),
        "one\n",
        "the buffer holds the only remaining copy"
    );
    assert_eq!(external(&session), Some(ExternalChange::Missing));
    assert_eq!(
        message(&session),
        "main.rs is gone from disk; the buffer keeps the only copy"
    );

    // The buffer stays editable, and a save writes the file again.
    press(&mut session, 'i');
    type_keys(&mut session, "kept ");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "kept one\n");
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    assert_eq!(
        std::fs::read_to_string(&path).expect("the save wrote the file again"),
        "kept one\n"
    );
    assert_eq!(external(&session), None, "the save cleared the marker");
}

#[test]
fn a_renamed_file_reaches_the_same_missing_state() {
    let (directory, path, mut session) = opened_file("session-reload-renamed", "main.rs", "one\n");

    std::fs::rename(&path, directory.join("other.rs")).expect("the file exists");
    run_watch_reload(&mut session);

    assert_eq!(session.buffer().to_string(), "one\n");
    assert_eq!(external(&session), Some(ExternalChange::Missing));
    assert!(!session.buffer().is_modified());
}

#[test]
fn a_reload_reaches_the_language_server_with_the_reloaded_text() {
    let (_directory, path, mut session) =
        opened_file("session-reload-language", "main.rs", "fn main() {}\n");

    std::fs::write(&path, "fn main() { println!(); }\n").expect("the file is writable");
    let _ = report_watch_change(&mut session);
    let request = session
        .take_file_request()
        .expect("the burst queued one reload check");
    refuse_language_requests(&mut session);
    let _ = session.apply_file_result(request.run());

    let synchronization = session
        .take_language_request()
        .expect("the reload synchronizes the document");
    match synchronization {
        LanguageRequest::Open { version, text, .. } => {
            assert_eq!(&*text, "fn main() { println!(); }\n");
            assert_eq!(
                version,
                session.buffer().version(),
                "the server copy carries the version of the reloaded text"
            );
        }
        other => panic!("a reload opens the document again, not {other:?}"),
    }
}

#[test]
fn an_obsolete_reload_result_never_replaces_the_buffer() {
    let (_directory, path, mut session) =
        opened_file("session-reload-obsolete", "main.rs", "one\n");

    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");
    let _ = report_watch_change(&mut session);
    let request = session
        .take_file_request()
        .expect("the burst queued one reload check");
    let result = request.run();

    // The user edits the buffer while the check runs, so its outcome describes
    // a buffer state that the editor already left.
    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    let _ = session.apply_file_result(result);

    assert_eq!(session.buffer().to_string(), "typed one\n");
    assert!(session.buffer().is_modified());
}

#[test]
fn a_file_that_grew_past_the_size_limit_keeps_its_buffer() {
    let directory = TempDir::new("session-reload-limit");
    let path = directory.write("main.rs", "one\n");
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    settings.files.max_file_bytes = 8;
    let mut session = Session::new(Rect::new(0, 0, 80, 24), settings, workspace_root());

    session.open_path(path.clone());
    run_file_request(&mut session);
    assert_eq!(session.buffer().to_string(), "one\n");

    std::fs::write(&path, "far above the limit\n").expect("the file is writable");
    run_watch_reload(&mut session);

    assert_eq!(session.buffer().to_string(), "one\n");
    assert_eq!(external(&session), Some(ExternalChange::Changed));
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );
}

#[test]
fn the_edit_command_reloads_a_clean_buffer_and_refuses_a_dirty_one() {
    let (_directory, path, mut session) = opened_file("session-reload-command", "main.rs", "one\n");

    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");
    press(&mut session, ':');
    type_keys(&mut session, "e");
    press_code(&mut session, KeyCode::Enter);
    run_file_request(&mut session);
    assert_eq!(session.buffer().to_string(), "one\ntwo\n");
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Info)
    );

    // A buffer with unsaved changes refuses the reload and names the form that
    // discards them.
    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    press(&mut session, ':');
    type_keys(&mut session, "e");
    press_code(&mut session, KeyCode::Enter);
    assert!(
        session.take_file_request().is_none(),
        "a refused reload reads no file"
    );
    assert_eq!(
        message(&session),
        "the buffer holds unsaved changes; use :e! to discard them and reload the file"
    );
    assert_eq!(session.buffer().to_string(), "typed one\ntwo\n");
}

#[test]
fn the_forced_edit_command_discards_the_unsaved_changes_and_reloads() {
    let (_directory, path, mut session) = opened_file("session-reload-forced", "main.rs", "one\n");

    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");

    press(&mut session, ':');
    type_keys(&mut session, "e!");
    press_code(&mut session, KeyCode::Enter);
    run_file_request(&mut session);

    assert_eq!(session.buffer().to_string(), "one\ntwo\n");
    assert!(!session.buffer().is_modified());
    assert_eq!(external(&session), None);
}

/// Returns the highlight spans that the frame reads for the active buffer.
fn highlights(session: &Session) -> usize {
    session.visible().highlights(session.active()).len()
}

/// Opens one file in a session that keeps no persistent undo file.
fn opened(name: &str, text: &str) -> (TempDir, Session) {
    let directory = TempDir::new("session-language");
    let path = directory.write(name, text);
    let mut session = file_session();
    session.open_path(path);
    run_file_request(&mut session);
    (directory, session)
}

/// Runs the queued analysis job, like the event loop and the worker service.
fn run_analysis(session: &mut Session) -> Redraw {
    let request = session
        .take_analysis_request()
        .expect("the buffer needs one analysis");
    let result = request.run(&CancellationToken::new());
    session.apply_analysis_result(result)
}

#[test]
fn an_accepted_analysis_reaches_the_view_and_an_obsolete_one_is_rejected() {
    let (_directory, mut session) = opened("main.rs", "fn main() {}\n");
    assert_eq!(highlights(&session), 0, "no result is accepted yet");

    let request = session
        .take_analysis_request()
        .expect("a Rust buffer needs one analysis");
    assert_eq!(request.buffer(), session.active());
    assert!(
        session.take_analysis_request().is_none(),
        "one analysis runs at a time"
    );
    let obsolete = request.run(&CancellationToken::new());

    // One edit moves the buffer past the version that the job read.
    press(&mut session, 'o');
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        session.apply_analysis_result(obsolete),
        Redraw::Skipped,
        "an obsolete buffer version changes nothing"
    );
    assert_eq!(
        highlights(&session),
        0,
        "an obsolete result enters no cache"
    );

    // The next job reads the current version, so its spans reach the view.
    assert_eq!(run_analysis(&mut session), Redraw::Needed);
    assert!(highlights(&session) > 0);
    assert!(
        session.take_analysis_request().is_none(),
        "the accepted result already describes the current version"
    );
}

#[test]
fn a_buffer_without_an_adapter_needs_no_analysis_and_stays_editable() {
    let (_directory, mut session) = opened("notes.txt", "plain text\n");
    assert!(session.take_analysis_request().is_none());
    assert_eq!(highlights(&session), 0);

    press(&mut session, 'i');
    type_keys(&mut session, "more ");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "more plain text\n");
}

#[test]
fn space_slash_toggles_the_line_comment_of_the_language_adapter() {
    let (_directory, mut session) = opened("main.rs", "fn main() {}\n");
    type_keys(&mut session, " /");
    assert_eq!(session.buffer().to_string(), "// fn main() {}\n");

    // The toggle is one transaction, so one undo reverses it.
    press(&mut session, 'u');
    assert_eq!(session.buffer().to_string(), "fn main() {}\n");

    // A Visual Line selection toggles every selected line.
    let (_directory, mut session) = opened("pair.rs", "let a = 1;\nlet b = 2;\n");
    type_keys(&mut session, "Vj /");
    assert_eq!(
        session.buffer().to_string(),
        "// let a = 1;\n// let b = 2;\n"
    );
}

#[test]
fn a_comment_toggle_without_an_adapter_changes_nothing_and_reports_why() {
    let (_directory, mut session) = opened("notes.txt", "plain text\n");
    type_keys(&mut session, " /");
    assert_eq!(session.buffer().to_string(), "plain text\n");
    assert_eq!(
        message(&session),
        "no language adapter provides a line-comment token for this buffer"
    );
}

#[test]
fn the_syntax_indent_opens_a_line_one_level_deeper_inside_a_block() {
    let (_directory, mut session) = opened("block.rs", "fn main() {\n}\n");

    // Without a parse result the previous-line rule keeps column zero.
    press(&mut session, 'o');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "fn main() {\nx\n}\n");
    press(&mut session, 'u');
    press(&mut session, 'u');
    assert_eq!(session.buffer().to_string(), "fn main() {\n}\n");

    // With the accepted analysis the new line follows the syntax tree.
    run_analysis(&mut session);
    type_keys(&mut session, "gg");
    press(&mut session, 'o');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "fn main() {\n    x\n}\n");

    // `Enter` reads the same rule, and a closing delimiter loses one level.
    run_analysis(&mut session);
    type_keys(&mut session, "A");
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "y");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        session.buffer().to_string(),
        "fn main() {\n    x\n    y\n}\n"
    );
}

#[test]
fn every_window_paints_its_own_buffer_and_only_the_focused_one_holds_the_cursor() {
    let directory = TempDir::new("session-splits");
    let first = directory.write("first.rs", "fn first() {}\n");
    let second = directory.write("second.rs", "fn second() {}\n");
    let mut session = file_session();

    session.open_path(first);
    run_file_request(&mut session);
    let left = session.windows().focused_window();
    let left_buffer = session.active();

    // `Ctrl-Enter` splits with the adaptive rule and focuses the new window.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let right = session.windows().focused_window();
    assert_ne!(left, right);

    session.open_path(second);
    run_file_request(&mut session);
    let right_buffer = session.active();
    assert_ne!(left_buffer, right_buffer);
    assert_eq!(session.windows().buffer(left), Some(left_buffer));
    assert_eq!(session.windows().buffer(right), Some(right_buffer));

    // The focus moves back, and the editing state follows the focused window.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('h'))), NOW);
    assert_eq!(session.windows().focused_window(), left);
    assert_eq!(
        session.active(),
        left_buffer,
        "a key must change the buffer that the focused window shows"
    );
    press(&mut session, 'i');
    type_keys(&mut session, "// ");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "// fn first() {}\n");
}

#[test]
fn an_unsupported_target_is_rejected_and_leaves_the_editor_usable() {
    let directory = TempDir::new("session-reject");
    let mut session = file_session();

    session.open_path(directory.path.clone());
    run_file_request(&mut session);
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );
    assert_eq!(session.buffers().len(), 1, "no buffer holds a directory");

    press(&mut session, 'i');
    type_keys(&mut session, "text");
    assert_eq!(session.buffer().to_string(), "text\n");
}

#[test]
fn a_missing_language_server_is_reported_once_and_editing_continues() {
    let directory = TempDir::new("session-missing-server");
    let path = directory.write("main.rs", "fn main() {}\n");
    let mut session = file_session();

    session.open_path(path);
    run_file_request(&mut session);
    // The load queues one open, which reaches no server on this system.
    refuse_language_requests(&mut session);
    assert_eq!(
        message(&session),
        "no language server serves this buffer",
        "a missing server is a normal state, not a failure"
    );

    // Every later question finds the state already reported. `Space e` reads
    // the published diagnostics instead, so it asks no server at all.
    for keys in [" k", "gd"] {
        type_keys(&mut session, keys);
        refuse_language_requests(&mut session);
        assert_eq!(
            message(&session),
            "",
            "`{keys}` must not repeat the report of a missing server"
        );
    }

    // The editor stays fully usable without a language server.
    press(&mut session, 'i');
    type_keys(&mut session, "// ");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "// fn main() {}\n");
}

#[test]
fn the_format_on_save_toggle_changes_the_active_buffer_alone() {
    let directory = TempDir::new("session-format-toggle");
    let first = directory.write("first.rs", "one\n");
    let second = directory.write("second.rs", "two\n");
    let mut session = file_session();

    session.open_path(first.clone());
    run_file_request(&mut session);
    session.open_path(second);
    run_file_request(&mut session);

    // Every new buffer follows the settings default, so its save formats first.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert!(
        asks_a_question(&mut session),
        "format-on-save asks the language server before the write"
    );
    run_file_request(&mut session);

    type_keys(&mut session, " cf");
    assert_eq!(message(&session), "format-on-save is off for this buffer");
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert!(
        !asks_a_question(&mut session),
        "the toggled buffer saves its content as it is"
    );
    run_file_request(&mut session);

    // The toggle is per buffer, so no other buffer and no default changed. The
    // first file is loaded already, so its path reaches its buffer without a
    // new read.
    session.open_path(first);
    assert!(session.take_file_request().is_none());
    assert_eq!(session.active_buffer().name(), "first.rs");
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert!(
        asks_a_question(&mut session),
        "the toggle of one buffer never changes another buffer"
    );
    run_file_request(&mut session);

    type_keys(&mut session, " cf");
    assert_eq!(message(&session), "format-on-save is off for this buffer");
    type_keys(&mut session, " cf");
    assert_eq!(message(&session), "format-on-save is on for this buffer");
}

/// Returns the first visible line of one window.
fn first_line(session: &Session, window: WindowId) -> usize {
    session
        .windows()
        .state(window)
        .expect("the window exists")
        .first_line()
}

/// Returns the cursor line of one window.
fn cursor_line(session: &Session, window: WindowId) -> usize {
    session
        .windows()
        .state(window)
        .expect("the window exists")
        .cursor()
        .line()
        .get()
}

/// Creates a session with one long buffer and one vertical split.
///
/// The function returns the left window and the right window, and the right
/// window holds the focus, as a new split always does.
fn split_session(lines: usize) -> (Session, WindowId, WindowId) {
    let mut session = session(80, 24);
    press(&mut session, 'i');
    for index in 0..lines {
        if index > 0 {
            press_code(&mut session, KeyCode::Enter);
        }
        type_keys(&mut session, "line");
    }
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "gg");

    let left = session.windows().focused_window();
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let right = session.windows().focused_window();
    assert_ne!(left, right, "the split opened a second window");
    (session, left, right)
}

#[test]
fn two_windows_on_one_buffer_scroll_independently() {
    let (mut session, left, right) = split_session(200);
    assert_eq!(first_line(&session, left), 0);
    assert_eq!(first_line(&session, right), 0);

    // The focused window scrolls to the buffer end.
    press(&mut session, 'G');
    let scrolled = first_line(&session, right);
    assert!(scrolled > 0, "the focused window followed its cursor");
    assert_eq!(
        first_line(&session, left),
        0,
        "the untouched window keeps its first visible line"
    );
    assert_eq!(
        cursor_line(&session, left),
        0,
        "the untouched window keeps its cursor"
    );

    // The focus returns to the left window, and both windows stay where they
    // were.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('h'))), NOW);
    assert_eq!(session.windows().focused_window(), left);
    assert_eq!(first_line(&session, left), 0);
    assert_eq!(first_line(&session, right), scrolled);
    assert_eq!(cursor_line(&session, right), 199);

    // A move in the left window moves no other window.
    type_keys(&mut session, "10j");
    assert_eq!(cursor_line(&session, left), 10);
    assert_eq!(cursor_line(&session, right), 199);
    assert_eq!(first_line(&session, right), scrolled);
}

#[test]
fn two_windows_on_two_buffers_scroll_independently() {
    let directory = TempDir::new("session-window-cursors");
    let first = directory.write("first.rs", &"first\n".repeat(200));
    let second = directory.write("second.rs", &"second\n".repeat(200));
    let mut session = file_session();

    session.open_path(first);
    run_file_request(&mut session);
    let left = session.windows().focused_window();

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let right = session.windows().focused_window();
    session.open_path(second);
    run_file_request(&mut session);
    assert_ne!(
        session.windows().buffer(left),
        session.windows().buffer(right),
        "the two windows show two buffers"
    );

    press(&mut session, 'G');
    let scrolled = first_line(&session, right);
    assert!(scrolled > 0);
    assert_eq!(
        first_line(&session, left),
        0,
        "the window of the other buffer did not scroll"
    );

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('h'))), NOW);
    assert_eq!(
        session.active_buffer().name(),
        "first.rs",
        "the focus move follows the buffer of its window"
    );
    assert_eq!(cursor_line(&session, left), 0);
    assert_eq!(first_line(&session, right), scrolled);
}

#[test]
fn a_new_split_copies_the_cursor_and_the_viewport_of_its_source() {
    let (mut session, _, right) = split_session(200);
    press(&mut session, 'G');
    let line = cursor_line(&session, right);
    assert!(first_line(&session, right) > 0);

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let created = session.windows().focused_window();
    assert_ne!(created, right);
    assert_eq!(
        cursor_line(&session, created),
        line,
        "the new window opens at the cursor of its source"
    );
    // The split halves the height of the source window, so both windows
    // reconcile to the same smaller view.
    assert_eq!(
        first_line(&session, created),
        first_line(&session, right),
        "the new window opens at the view of its source"
    );
    assert!(
        first_line(&session, created) > 0,
        "the new window did not return to the buffer start"
    );
}

#[test]
fn closing_a_window_discards_its_cursor() {
    let (mut session, left, right) = split_session(200);
    press(&mut session, 'G');
    assert!(first_line(&session, right) > 0);

    type_keys(&mut session, " q");
    assert_eq!(session.windows().window_count(), 1);
    assert_eq!(session.windows().focused_window(), left);
    assert_eq!(
        first_line(&session, left),
        0,
        "the surviving window keeps its own view"
    );
    assert!(
        session.windows().state(right).is_none(),
        "the closed window discarded its view"
    );
}

#[test]
fn a_reported_deadline_always_reaches_a_transition_that_clears_it() {
    // The event loop runs one catch-up transition for a deadline that already
    // passed. A deadline that no transition can clear would keep the loop out of
    // its wait, and the editor would stop serving input. Every reported deadline
    // must therefore disappear after one tick.
    for keys in ["5", "12", " ", "5 ", "g", "5g", "z"] {
        let mut session = session(60, 20);
        type_keys(&mut session, keys);
        let Some(deadline) = session.next_deadline() else {
            continue;
        };
        session.tick(deadline);
        assert_eq!(
            session.next_deadline(),
            None,
            "the tick after the deadline of `{keys}` must clear it"
        );
    }
}

#[test]
fn a_pending_count_reports_no_deadline_at_all() {
    let mut session = session(60, 20);
    press(&mut session, '5');
    assert_eq!(
        session.next_deadline(),
        None,
        "a pending count shows no overlay, so the loop waits for the next key"
    );
    // The count still reaches the command that follows it.
    press(&mut session, 'j');
    assert_eq!(
        session.cursor().line().get(),
        0,
        "the buffer holds one line"
    );
    assert_eq!(session.mode(), Mode::Normal);
}

/// Creates a session whose clipboard reaches its value through one command.
///
/// The command never runs. Each test returns its output through
/// [`Session::apply_clipboard_result`], exactly as the event loop does.
fn clipboard_session(lines: &[&str]) -> Session {
    with_text(lines).with_clipboard(SessionClipboard::deferred())
}

/// Returns the standard input of the clipboard command that waits.
fn clipboard_text(session: &mut Session) -> String {
    let request = session
        .take_clipboard_request()
        .expect("the transition queued one clipboard command");
    String::from_utf8(request.stdin).expect("the editor writes UTF-8 text")
}

/// Returns the output of one clipboard command that succeeded.
fn clipboard_output(stdout: &str) -> ProcessOutput {
    ProcessOutput {
        status_code: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

#[test]
fn a_yank_sends_the_register_value_to_the_system_clipboard() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    assert_eq!(
        clipboard_text(&mut session),
        "alpha\n",
        "a linewise yank carries its line ending across the boundary"
    );
    assert_eq!(
        session.apply_clipboard_result(Ok(clipboard_output(""))),
        Redraw::Skipped,
        "a clipboard write that succeeded reports nothing"
    );
    assert_eq!(message(&session), "");
}

#[test]
fn a_failed_clipboard_write_keeps_the_register_value() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Err(ClipboardFailure::Failed));
    assert!(
        message(&session).contains("register still holds the value"),
        "the yank succeeded, so the report names the clipboard alone: {}",
        message(&session)
    );

    // The register survived the failure, so a paste still returns the value.
    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Err(ClipboardFailure::Failed));
    assert_eq!(session.buffer().to_string(), "alpha\nalpha\nbeta\n");
}

#[test]
fn a_clipboard_write_that_reported_no_outcome_reports_nothing() {
    // `wl-copy` and `xclip` own the selection through a background process that
    // inherits the captured output streams, so a write that succeeded holds
    // those streams open and reaches the process deadline. The write worked, so
    // the message line must stay empty. See `docs/clipboard.md`.
    for failure in [ClipboardFailure::Timeout, ClipboardFailure::Cancelled] {
        let mut session = clipboard_session(&["alpha", "beta"]);
        type_keys(&mut session, "yy");
        assert_eq!(clipboard_text(&mut session), "alpha\n");
        assert_eq!(
            session.apply_clipboard_result(Err(failure)),
            Redraw::Skipped,
            "{failure} proves no clipboard failure, so nothing changes"
        );
        assert_eq!(message(&session), "", "{failure} reports nothing");

        // The register kept the value on this path as well.
        type_keys(&mut session, "p");
        let _ = clipboard_text(&mut session);
        let _ = session.apply_clipboard_result(Err(failure));
        assert_eq!(session.buffer().to_string(), "alpha\nalpha\nbeta\n");
        assert_eq!(message(&session), "");
    }
}

#[test]
fn a_clipboard_write_that_a_signal_ended_reports_the_failure() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    // A signal leaves no exit status, so the command reported no success.
    let signalled = ProcessOutput {
        status_code: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    assert_eq!(
        session.apply_clipboard_result(Ok(signalled)),
        Redraw::Needed
    );
    assert!(
        message(&session).contains("register still holds the value"),
        "a proven failure still reaches the message line: {}",
        message(&session)
    );
}

#[test]
fn a_write_that_a_newer_write_displaced_reports_nothing() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    // The newer yank owns the clipboard, and the displaced write resolves from
    // internal state alone, so neither yank reports anything.
    type_keys(&mut session, "jyy");
    assert_eq!(message(&session), "");
    assert_eq!(clipboard_text(&mut session), "beta\n");
    assert_eq!(
        session.apply_clipboard_result(Ok(clipboard_output(""))),
        Redraw::Skipped
    );
    assert_eq!(message(&session), "");
}

#[test]
fn a_failed_clipboard_read_falls_back_to_the_internal_register() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Ok(clipboard_output("alpha\n")));

    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    // A refused submission is the same expected runtime state as a failed
    // command, so the paste still applies the internal register.
    let _ = session.apply_clipboard_result(Err(ClipboardFailure::Refused));
    assert_eq!(session.buffer().to_string(), "alpha\nalpha\nbeta\n");
}

#[test]
fn a_kvim_yank_pastes_with_the_shape_that_it_recorded() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    assert_eq!(clipboard_text(&mut session), "alpha\n");
    let _ = session.apply_clipboard_result(Ok(clipboard_output("")));

    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    // The clipboard still holds the text that Kvim wrote, so the recorded
    // linewise shape applies. See `docs/clipboard.md`.
    let _ = session.apply_clipboard_result(Ok(clipboard_output("alpha\n")));
    assert_eq!(session.buffer().to_string(), "alpha\nalpha\nbeta\n");
}

#[test]
fn an_external_copy_pastes_characterwise() {
    let mut session = clipboard_session(&["alpha"]);
    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Ok(clipboard_output("gamma")));
    assert_eq!(
        session.buffer().to_string(),
        "agammalpha\n",
        "text that Kvim never wrote is characterwise"
    );
}

#[test]
fn an_external_copy_that_ends_with_a_line_ending_pastes_linewise() {
    let mut session = clipboard_session(&["alpha"]);
    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Ok(clipboard_output("gamma\n")));
    assert_eq!(session.buffer().to_string(), "alpha\ngamma\n");
}

#[test]
fn an_oversized_clipboard_value_never_reaches_the_register() {
    let mut session = clipboard_session(&["alpha"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Ok(clipboard_output("")));

    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    let oversized = "b".repeat(CLIPBOARD_BYTES_MAX + 1);
    let _ = session.apply_clipboard_result(Ok(clipboard_output(&oversized)));
    assert!(
        message(&session).contains("clipboard bound"),
        "the report names the bound: {}",
        message(&session)
    );
    assert_eq!(
        session.buffer().to_string(),
        "alpha\nalpha\n",
        "the paste falls back to the internal register"
    );
}

#[test]
fn a_missing_clipboard_command_is_reported_once_for_each_session() {
    // A session without an injected clipboard reaches no command at all, which
    // is the supported state of a host without a clipboard tool.
    let mut session = with_text(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    assert!(
        message(&session).contains("no system clipboard command"),
        "the first operation names the missing command: {}",
        message(&session)
    );
    assert!(
        session.take_clipboard_request().is_none(),
        "a host without a command runs none"
    );

    type_keys(&mut session, "yy");
    assert_eq!(
        message(&session),
        "",
        "the missing command is reported once for each session"
    );
}

#[test]
fn a_clipboard_output_without_a_pending_operation_changes_nothing() {
    let mut session = clipboard_session(&["alpha"]);
    assert_eq!(
        session.apply_clipboard_result(Ok(clipboard_output("gamma"))),
        Redraw::Skipped,
        "an output that no operation waits for is obsolete"
    );
    assert_eq!(session.buffer().to_string(), "alpha\n");
    assert_eq!(message(&session), "");
}

#[test]
fn a_newer_clipboard_operation_never_leaves_a_paste_waiting() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Ok(clipboard_output("")));

    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    // A yank displaces the read that the paste waits for. The paste must then
    // apply the internal register instead of waiting forever.
    type_keys(&mut session, "yy");
    assert_eq!(session.buffer().to_string(), "alpha\nalpha\nbeta\n");
    assert_eq!(
        clipboard_text(&mut session),
        "alpha\n",
        "the displacing yank owns the clipboard command"
    );
}
