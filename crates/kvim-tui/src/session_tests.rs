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
use kvim_input::{CommandLineCommand, Mode};
use kvim_language::LspError;
use kvim_runtime::{ProcessOutput, WatchBatch, WatchEvent, WatchKind};
use kvim_settings::{EditorSettings, WHICH_KEY_DELAY_DEFAULT};
use kvim_terminal::{FocusChange, Key, KeyCode, TerminalEvent};
use kvim_workspace::temp::TempDir;
use kvim_workspace::{Candidate, ExternalChange, PickerRequest, PickerResult, rank_candidates};

use super::clipboard::SessionClipboard;
use super::completion::{CompletionOutcome, LineCompletion};
use super::language::{LanguageRequest, LanguageRequestKind};
use super::log::LOG_ENTRIES_MAX;
use super::session::{
    CONFIRM_ANSWER_CHARS_MAX, ConfirmationRequest, ConfirmedAction, HostProbeFailure, MessageLevel,
    Redraw, RunState, Session,
};
use super::window::{SidebarSide, WindowId};

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

/// Answers the open question with one typed text and `Enter`.
///
/// The question reads the text only when `Enter` closes it, so the returned
/// redraw request belongs to that last key.
fn answer(session: &mut Session, text: &str) -> Redraw {
    type_keys(session, text);
    press_code(session, KeyCode::Enter)
}

/// Returns the open question, or an empty text while none waits.
fn question(session: &Session) -> String {
    session
        .visible()
        .confirmation
        .map_or_else(String::new, |confirmation| confirmation.question.clone())
}

/// Returns the text of the open prompt, or an empty text while none is open.
fn prompt_text(session: &Session) -> String {
    session
        .visible()
        .prompt
        .map_or_else(String::new, |prompt| prompt.text.clone())
}

/// Reports whether the open prompt holds a completion.
fn completing(session: &Session) -> bool {
    session
        .visible()
        .prompt
        .is_some_and(|prompt| prompt.completion.is_some())
}

/// Returns what the open completion of the prompt shows.
///
/// A prompt without a completion reports [`CompletionOutcome::Missed`], because
/// no candidate reached its line.
fn completion_outcome(session: &Session) -> CompletionOutcome {
    session
        .visible()
        .prompt
        .and_then(|prompt| prompt.completion.as_ref())
        .map_or(CompletionOutcome::Missed, LineCompletion::outcome)
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
fn the_focused_file_tree_answers_the_resize_keys() {
    // The sidebar owns its own binding scope, so the resize keys must live in
    // that scope as well as in the Normal scope.
    let mut session = session(120, 20);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('e'))), NOW);
    let width = |session: &Session| {
        session
            .windows()
            .sidebar(SidebarSide::Right)
            .expect("`Ctrl-E` opens the file tree")
            .width_cells()
    };
    let opened = width(&session);

    assert_eq!(
        session.handle_event(TerminalEvent::Key(Key::ctrl_alt(KeyCode::Char('h'))), NOW),
        Redraw::Needed
    );
    assert_eq!(width(&session), opened + 6, "the inner border moves left");

    assert_eq!(
        session.handle_event(TerminalEvent::Key(Key::ctrl_alt(KeyCode::Char('l'))), NOW),
        Redraw::Needed
    );
    assert_eq!(width(&session), opened, "the inner border moves back");
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

    // A buffer without a file name refuses the reload and asks nothing,
    // because no file can replace its text.
    press(&mut session, ':');
    type_keys(&mut session, "e");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        message(&session),
        "the buffer holds no file name; use :e <path> to name one"
    );
    assert_eq!(question(&session), "", "a refusal asks nothing");

    // `:q` asks before it discards the unsaved changes.
    press(&mut session, ':');
    type_keys(&mut session, "q");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Running);
    assert_eq!(
        question(&session),
        "Quit and discard the unsaved changes of [Scratch]"
    );
    answer(&mut session, "n");
    assert_eq!(session.run_state(), RunState::Running);
    assert_eq!(question(&session), "", "the answer closed the question");

    // `:q!` discards them and ends the editor.
    press(&mut session, ':');
    type_keys(&mut session, "q!");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Finished);
}

/// Reports two messages and returns both texts, oldest first.
///
/// The second message replaces the first one on the message line, so the
/// message line alone loses the first text.
fn report_two_messages(session: &mut Session) -> (String, String) {
    // A scratch buffer holds no file name, so `:w` refuses the save.
    press(session, ':');
    type_keys(session, "w");
    press_code(session, KeyCode::Enter);
    let replaced = message(session);
    assert!(!replaced.is_empty(), "the refused save reports its reason");

    press(session, ':');
    type_keys(session, "wqa");
    press_code(session, KeyCode::Enter);
    let newest = message(session);
    assert_ne!(
        newest, replaced,
        "the message line keeps the newest message only"
    );
    (replaced, newest)
}

/// Opens the editor log and returns the rows of the new buffer.
fn open_log(session: &mut Session, name: &str) -> Vec<String> {
    press(session, ':');
    type_keys(session, name);
    press_code(session, KeyCode::Enter);
    assert_eq!(session.active_buffer().name(), "[Logs]");
    session
        .buffer()
        .to_string()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_log_command_opens_a_snapshot_that_holds_a_replaced_message() {
    let mut session = session(60, 12);
    let (replaced, newest) = report_two_messages(&mut session);

    // `:l` is the declared abbreviation of `:logs`.
    let rows = open_log(&mut session, "l");
    assert_eq!(rows.len(), 2, "the log holds both messages, not {rows:?}");
    assert!(
        rows[0].ends_with(&replaced),
        "the replaced message survives in {:?}",
        rows[0]
    );
    assert!(
        rows[1].ends_with(&newest),
        "the newest message is the last row, not {:?}",
        rows[1]
    );
    assert!(
        rows[0].contains("ERROR MESSAGE"),
        "one entry names its severity and its source in {:?}",
        rows[0]
    );

    // The buffer is an ordinary scratch buffer over generated text.
    assert_eq!(session.active_buffer().path(), None);
    assert!(
        !session.active_buffer().is_modified(),
        "the new buffer holds no unsaved change"
    );
    // The command line clears the message line when it opens, exactly as it
    // does for every other command, and the log command reports nothing.
    assert_eq!(message(&session), "");
}

#[test]
fn the_diagnostics_command_probes_off_the_event_loop_and_opens_the_report() {
    let mut session = session(60, 12);
    // The declared minimum of the name reaches the command, so `:d` runs it.
    run_command(&mut session, "d");

    // The probe reads the executable search path, so the command opens no
    // buffer yet and the message line names the wait.
    assert_eq!(session.active_buffer().name(), "[Scratch]");
    assert_eq!(
        message(&session),
        "the host report is running; its buffer opens when it answers"
    );

    // The event loop hands the request to the bounded worker service, and one
    // command produces exactly one request.
    let request = session
        .take_host_request()
        .expect("the command asked for one probe");
    assert!(
        session.take_host_request().is_none(),
        "one command asks for one probe"
    );

    let report = request.run();
    assert_eq!(session.apply_host_report(&report), Redraw::Needed);
    assert_eq!(session.active_buffer().name(), "[Diagnostics]");
    let text = session.buffer().to_string();
    assert!(text.contains("Language servers ("), "{text}");
    assert!(text.contains("Formatters ("), "{text}");
    assert!(text.contains("rust-analyzer"), "{text}");

    // The probe answered, so the note that named the wait leaves the message
    // line with the buffer that it promised.
    assert_eq!(message(&session), "");

    // The buffer is an ordinary scratch buffer over generated text.
    assert_eq!(session.active_buffer().path(), None);
    assert!(!session.active_buffer().is_modified());
}

#[test]
fn a_second_diagnostics_command_starts_no_second_probe() {
    let mut session = session(60, 12);
    run_command(&mut session, "diagnostics");
    let request = session
        .take_host_request()
        .expect("the first command asked for one probe");

    // The probe already runs, so the second command reports the same state and
    // queues nothing.
    run_command(&mut session, "diagnostics");
    assert!(
        session.take_host_request().is_none(),
        "the running probe answers both commands"
    );

    let report = request.run();
    assert_eq!(session.apply_host_report(&report), Redraw::Needed);
    assert_eq!(
        session.buffers().len(),
        2,
        "the two commands open one buffer"
    );

    // The finished probe leaves the session ready for a fresh report.
    run_command(&mut session, "diagnostics");
    assert!(
        session.take_host_request().is_some(),
        "a later command asks for a fresh probe"
    );
}

#[test]
fn a_failed_host_probe_opens_no_buffer_and_reports_the_outcome() {
    let mut session = session(60, 12);
    run_command(&mut session, "diagnostics");
    let _request = session
        .take_host_request()
        .expect("the command asked for one probe");

    assert_eq!(
        session.abandon_host_request(HostProbeFailure::Timeout),
        Redraw::Needed
    );
    assert_eq!(message(&session), "the host report passed its deadline");
    assert_eq!(session.buffers().len(), 1, "the failure opens no buffer");

    // The abandoned probe leaves the session ready for a fresh report.
    run_command(&mut session, "diagnostics");
    assert!(session.take_host_request().is_some());
}

#[test]
fn an_edit_of_the_log_buffer_changes_no_entry_and_a_second_log_builds_a_new_snapshot() {
    let mut session = session(60, 12);
    let (_, newest) = report_two_messages(&mut session);
    let rows = open_log(&mut session, "logs");
    let edited = session.buffers().ids();

    // The user edits the snapshot like any other buffer.
    press(&mut session, 'i');
    type_keys(&mut session, "note");
    press_code(&mut session, KeyCode::Esc);
    assert!(session.active_buffer().is_modified());

    // The edit changed no entry, so the next snapshot holds the same rows.
    assert_eq!(open_log(&mut session, "logs"), rows);

    // The same report reaches the log again. The log collapses a repeated
    // report, so the next snapshot counts it instead of adding one row.
    press(&mut session, ':');
    type_keys(&mut session, "wqa");
    press_code(&mut session, KeyCode::Enter);
    let grown = open_log(&mut session, "logs");
    assert_eq!(grown.len(), 2, "a repeated report adds no row to {grown:?}");
    assert!(
        grown[1].ends_with(&format!("{newest} (x2)")),
        "the repeat raises the count of the newest entry, not {:?}",
        grown[1]
    );
    assert_ne!(grown, rows, "the command builds the snapshot again");

    // Every earlier snapshot stayed as it was.
    let first = edited
        .last()
        .and_then(|id| session.buffers().get(*id))
        .expect("the first log buffer is still loaded");
    assert!(
        first.text().to_string().starts_with("note"),
        "the first log buffer keeps the edit of the user"
    );
}

#[test]
fn the_command_line_completes_a_command_name_and_wraps_the_cycle() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "q");
    assert!(!completing(&session), "the typed text opens no completion");

    // The completion offers the full name, so `q` reaches `quit`. The text
    // holds no `!`, so `quit` is the whole offer and needs no list.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "quit");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Completed);
    press_code(&mut session, KeyCode::Esc);
    press_code(&mut session, KeyCode::Esc);

    // An empty line names every command, so the first cycle writes the first
    // candidate and opens the list.
    press(&mut session, ':');
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "diagnostics");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);
    for expected in ["edit", "logs", "quit", "wq", "write"] {
        press_code(&mut session, KeyCode::Tab);
        assert_eq!(prompt_text(&session), expected);
    }

    // The candidates stay anchored to the typed text, so the cycle wraps
    // instead of narrowing the list to the written candidate.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "diagnostics");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);
}

#[test]
fn no_completion_cycle_writes_a_force_variant_that_the_user_did_not_type() {
    let mut session = modified_session();
    press(&mut session, ':');
    type_keys(&mut session, "q");

    // `quit!` discards the unsaved changes and asks nothing, so no cycle of a
    // text without a `!` writes it.
    for _ in 0..4 {
        press_code(&mut session, KeyCode::Tab);
        assert_eq!(prompt_text(&session), "quit");
    }

    // The completed line runs `:quit`, which still asks before it discards.
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Running);
    assert_eq!(question(&session), QUIT_QUESTION);
    answer(&mut session, "n");
}

#[test]
fn the_command_line_completion_answers_the_size_of_its_candidate_set() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "w");
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "wq");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "write");
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(
        prompt_text(&session),
        "wq",
        "the list holds `wq` and `write` alone"
    );
    press_code(&mut session, KeyCode::Esc);
    press_code(&mut session, KeyCode::Esc);

    // One candidate completes the line and opens no list.
    press(&mut session, ':');
    type_keys(&mut session, "wq");
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Needed);
    assert_eq!(prompt_text(&session), "wq");
    assert_eq!(
        completion_outcome(&session),
        CompletionOutcome::Completed,
        "one candidate needs no list"
    );
    press_code(&mut session, KeyCode::Esc);
    press_code(&mut session, KeyCode::Esc);

    // A text that names no command changes nothing and reports nothing.
    press(&mut session, ':');
    type_keys(&mut session, "x");
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "x");
    assert!(!completing(&session));
    assert_eq!(message(&session), "");

    // A line number is no name, so the digits offer no candidate.
    press_code(&mut session, KeyCode::Backspace);
    type_keys(&mut session, "42");
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "42");
    assert!(!completing(&session));
}

#[test]
fn the_command_line_completion_cycles_backward_and_restores_the_typed_text() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "w");

    // A backward cycle from the typed text wraps to the last candidate.
    press_code(&mut session, KeyCode::BackTab);
    assert_eq!(prompt_text(&session), "write");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);
    press_code(&mut session, KeyCode::BackTab);
    assert_eq!(prompt_text(&session), "wq");
    press_code(&mut session, KeyCode::BackTab);
    assert_eq!(prompt_text(&session), "write");

    // The first cancel restores the typed text and closes the list.
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(prompt_text(&session), "w");
    assert!(!completing(&session));
    assert!(
        session.visible().prompt.is_some(),
        "the first cancel keeps the command line open"
    );

    // The second cancel closes the command line.
    press_code(&mut session, KeyCode::Esc);
    assert!(session.visible().prompt.is_none());
    assert_eq!(session.mode(), Mode::Normal);
}

#[test]
fn one_typed_key_after_a_cycle_closes_the_list_and_reads_the_new_line() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    // An empty line names every command, so the second cycle reaches `edit`
    // and the list stays open.
    press_code(&mut session, KeyCode::Tab);
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "edit");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);

    // The typed key continues from the line as it is shown.
    press(&mut session, '!');
    assert_eq!(prompt_text(&session), "edit!");
    assert!(
        !completing(&session),
        "one insert closes the candidate list"
    );

    // The next completion reads the new line and offers `edit!` alone, so it
    // never reuses the candidates of the closed list.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "edit!");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Completed);

    // One delete closes the list too, and the completion answers `edit` again.
    press_code(&mut session, KeyCode::Backspace);
    assert!(!completing(&session));
    assert_eq!(prompt_text(&session), "edit");
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(
        prompt_text(&session),
        "edit",
        "the new line holds no `!`, so it offers `edit` alone"
    );
    assert_eq!(completion_outcome(&session), CompletionOutcome::Completed);
}

#[test]
fn enter_runs_the_command_that_the_completion_wrote_into_the_line() {
    let mut session = modified_session();
    press(&mut session, ':');
    type_keys(&mut session, "qu!");
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "quit!");

    // The line shows `quit!`, so `Enter` discards the changes and asks nothing.
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(question(&session), "");
    assert_eq!(session.run_state(), RunState::Finished);
}

/// The workspace files that one test walk collects, in walk order.
///
/// The walk returns absolute paths below the workspace root, so the candidates
/// hold the same shape that the file picker receives.
fn walked_files() -> Vec<Candidate> {
    [
        "src/session.rs",
        "src/main.rs",
        "docs/windows.md",
        "src/mode.rs",
    ]
    .into_iter()
    .map(|relative| Candidate::file(&workspace_root(), workspace_root().join(relative)))
    .collect()
}

/// Answers the workspace walk that the open command line asked for.
///
/// The session performs no filesystem work, so the test plays the part of the
/// bounded worker service and hands the collected files back.
fn answer_completion_walk(session: &mut Session, files: Vec<Candidate>) {
    let request = session
        .take_completion_request()
        .expect("the open command line asks for one walk");
    assert!(
        matches!(&request, PickerRequest::Files { root } if root == &workspace_root()),
        "the walk starts at the workspace root, so no candidate leaves it"
    );
    apply_completion_walk(session, files);
}

/// Hands the collected files of one taken walk back to the session.
fn apply_completion_walk(session: &mut Session, files: Vec<Candidate>) {
    assert_eq!(
        session.apply_completion_result(PickerResult::Candidates {
            query: String::new(),
            candidates: files,
            truncated: false,
        }),
        Redraw::Skipped,
        "the list opens on the next completion key, so the frame is unchanged"
    );
}

#[test]
fn the_command_line_completes_a_path_with_the_ranking_of_the_picker() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e src/m");
    answer_completion_walk(&mut session, walked_files());

    // The query reaches the directory of a file, so the completion matches the
    // complete path as the picker does. The two names hold the same score and
    // the same width, so the source order decides between them.
    assert_eq!(
        rank_candidates("src/m", &walked_files()),
        [1, 3],
        "the ranking of the completion is the ranking of the picker"
    );

    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "e src/main.rs");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "e src/mode.rs");

    // The completed line keeps the command name that the user typed, and the
    // parser accepts it.
    assert_eq!(
        CommandLineCommand::parse(&prompt_text(&session)),
        Ok(CommandLineCommand::Edit(PathBuf::from("src/mode.rs")))
    );

    // The candidates stay anchored to the typed text, so the cycle wraps.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "e src/main.rs");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(prompt_text(&session), "e src/m");
}

#[test]
fn the_command_line_offers_no_path_before_the_walk_answers() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e src/ma");

    // The walk still waits for the worker service, so the key changes nothing
    // and reports nothing. The event loop never waits for the result.
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "e src/ma");
    assert!(!completing(&session));
    assert_eq!(message(&session), "");

    // The same key offers the files after the result arrives.
    answer_completion_walk(&mut session, walked_files());
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Needed);
    assert_eq!(prompt_text(&session), "e src/main.rs");
}

#[test]
fn a_path_without_a_match_and_an_empty_walk_open_no_list() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e zz");
    answer_completion_walk(&mut session, walked_files());
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "e zz");
    assert!(!completing(&session));
    assert_eq!(message(&session), "");
    press_code(&mut session, KeyCode::Esc);

    // A walk that collected no file leaves the command line in the same state.
    press(&mut session, ':');
    type_keys(&mut session, "e src");
    answer_completion_walk(&mut session, Vec::new());
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "e src");
    assert!(!completing(&session));
    assert_eq!(message(&session), "");
}

#[test]
fn only_the_path_argument_of_edit_reads_the_workspace_files() {
    let mut session = session(60, 12);
    press(&mut session, ':');

    // A line without a blank still names a command, so the name source answers
    // and no walk of the workspace starts.
    type_keys(&mut session, "e");
    assert!(session.take_completion_request().is_none());
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "edit");
    press_code(&mut session, KeyCode::Esc);
    press_code(&mut session, KeyCode::Esc);

    // `:e!` reloads the buffer, and `:w` saves it, so neither takes a path.
    for line in ["e! src", "w src"] {
        press(&mut session, ':');
        type_keys(&mut session, line);
        assert!(
            session.take_completion_request().is_none(),
            "`:{line}` takes no path, so it asks for no walk"
        );
        assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
        assert_eq!(prompt_text(&session), line);
        assert!(!completing(&session));
        press_code(&mut session, KeyCode::Esc);
    }
}

/// Types one command line and counts the walks that it asked for.
///
/// The test plays the part of the event loop and takes the request after every
/// key, exactly as `submit_completion_work` does.
fn walks_asked(session: &mut Session, line: &str) -> usize {
    let mut asked = 0;
    for value in line.chars() {
        press(session, value);
        if session.take_completion_request().is_some() {
            asked += 1;
        }
    }
    asked
}

#[test]
fn only_a_line_that_holds_a_path_argument_asks_for_the_workspace_walk() {
    // Most command lines take no path, so they walk no directory at all.
    for line in [
        "w", "q", "wq", "q!", "42", "e", "e!", "w src", "e! src", "wq foo",
    ] {
        let mut session = session(60, 12);
        press(&mut session, ':');
        assert_eq!(
            walks_asked(&mut session, line),
            0,
            "`:{line}` holds no path argument, so it asks for no walk"
        );
    }

    // The line asks once, when it first holds a path argument. Every later
    // character of that line asks for no second walk.
    for line in ["e ", "e src/ma", "edit src/main.rs", "e  x"] {
        let mut session = session(60, 12);
        press(&mut session, ':');
        assert_eq!(
            walks_asked(&mut session, line),
            1,
            "`:{line}` holds a path argument, so it asks for exactly one walk"
        );
    }
}

#[test]
fn one_open_command_line_asks_for_one_walk_and_the_next_line_asks_again() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e src/ma");
    assert!(
        session.take_completion_request().is_some(),
        "the path argument asks for one walk"
    );
    // The event loop already took the request, so the rest of the line asks for
    // nothing more.
    type_keys(&mut session, "in");
    assert!(session.take_completion_request().is_none());

    // The walk that the line asked for still answers it.
    apply_completion_walk(&mut session, walked_files());
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "e src/main.rs");

    // The closed line drops its files, so the next line asks for its own walk.
    press_code(&mut session, KeyCode::Esc);
    press_code(&mut session, KeyCode::Esc);
    press(&mut session, ':');
    type_keys(&mut session, "e src/ma");
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert!(session.take_completion_request().is_some());
}

#[test]
fn a_walk_that_answers_a_closed_command_line_fills_no_list() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e ");
    assert!(
        session.take_completion_request().is_some(),
        "the path argument of the open command line asks for one walk"
    );
    press_code(&mut session, KeyCode::Esc);

    // The line that asked for the walk is gone, so its result fills no cache.
    assert_eq!(
        session.apply_completion_result(PickerResult::Candidates {
            query: String::new(),
            candidates: walked_files(),
            truncated: false,
        }),
        Redraw::Skipped
    );

    // The next command line asks for its own walk and offers no path until it
    // answers.
    press(&mut session, ':');
    type_keys(&mut session, "e src/ma");
    assert!(session.take_completion_request().is_some());
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "e src/ma");
}

/// The question that `:q` asks over the modified scratch buffer.
const QUIT_QUESTION: &str = "Quit and discard the unsaved changes of [Scratch]";

/// Returns a session whose scratch buffer holds unsaved changes.
fn modified_session() -> Session {
    let mut session = session(60, 12);
    press(&mut session, 'i');
    type_keys(&mut session, "one");
    press_code(&mut session, KeyCode::Esc);
    assert!(session.buffer().is_modified());
    session
}

/// Runs one command line and returns nothing, like a typed command.
fn run_command(session: &mut Session, line: &str) {
    press(session, ':');
    type_keys(session, line);
    press_code(session, KeyCode::Enter);
}

#[test]
fn the_quit_command_asks_and_a_confirmed_answer_ends_the_editor() {
    let mut session = modified_session();

    run_command(&mut session, "q");
    assert_eq!(
        question(&session),
        QUIT_QUESTION,
        "the question names the buffer"
    );
    assert_eq!(
        session.run_state(),
        RunState::Running,
        "the open question ends no editor"
    );

    // A lone `y` reaches the answer alone, so the editor keeps running.
    press(&mut session, 'y');
    assert_eq!(
        session.run_state(),
        RunState::Running,
        "one keypress ends no editor"
    );
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn a_cancelled_quit_keeps_the_buffer_and_the_window() {
    // `n` names the default of the question, the empty text names it as well,
    // and `no` and `ya` stand for every remaining answer.
    for value in ["n", "", "no", "ya"] {
        let mut session = modified_session();
        run_command(&mut session, "q");
        answer(&mut session, value);

        assert_eq!(
            session.run_state(),
            RunState::Running,
            "{value:?} keeps the editor running"
        );
        assert_eq!(
            session.buffer().to_string(),
            "one\n",
            "{value:?} keeps the text"
        );
        assert!(
            session.buffer().is_modified(),
            "{value:?} keeps the changes"
        );
        assert_eq!(message(&session), "", "{value:?} leaves no trace");
        assert_eq!(question(&session), "", "{value:?} closes the question");
        press(&mut session, 'i');
        assert_eq!(session.mode(), Mode::Insert, "{value:?} returns the keys");
    }

    let mut session = modified_session();
    run_command(&mut session, "q");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        session.run_state(),
        RunState::Running,
        "Esc keeps the window"
    );
    assert!(session.buffer().is_modified());
}

#[test]
fn the_forced_quit_command_asks_nothing_and_ends_the_editor() {
    let mut session = modified_session();

    run_command(&mut session, "q!");

    assert_eq!(question(&session), "", "`:q!` asks nothing");
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn a_quit_of_a_buffer_without_unsaved_changes_asks_nothing() {
    let mut session = session(60, 12);
    assert!(!session.buffer().is_modified());

    run_command(&mut session, "q");

    assert_eq!(
        question(&session),
        "",
        "a quit that destroys nothing asks nothing"
    );
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

/// The message that the test action of a confirmation reports.
const CONFIRMED: &str = "the confirmation reached its action";

/// Returns the typed answer of the open question, or an empty text.
fn typed_answer(session: &Session) -> String {
    session
        .visible()
        .confirmation
        .map_or_else(String::new, |confirmation| confirmation.answer.clone())
}

#[test]
fn a_confirmed_question_performs_its_action_and_returns_the_keys() {
    // Both accepted words perform the action, in every letter case.
    for value in ["y", "Y", "yes", "Yes", "YES", "yEs"] {
        let mut session = session(40, 10);
        assert_eq!(
            session.open_confirmation("Delete one entry", ConfirmedAction::Report),
            ConfirmationRequest::Opened
        );
        assert_eq!(answer(&mut session, value), Redraw::Needed);
        assert_eq!(message(&session), CONFIRMED, "{value} performs the action");
        assert_eq!(question(&session), "", "{value} closes the question");
        // The answer closes the question, so the mode below owns the keys again.
        press(&mut session, 'i');
        assert_eq!(session.mode(), Mode::Insert, "{value} returns the keys");
    }
}

#[test]
fn one_keypress_performs_no_action() {
    let mut session = session(40, 10);
    session.open_confirmation("Delete one entry", ConfirmedAction::Report);

    // The whole word reaches the answer, and no key of it performs the action.
    for value in ['y', 'e', 's'] {
        assert_eq!(press(&mut session, value), Redraw::Needed);
        assert_eq!(message(&session), "", "{value} alone performs no action");
        assert_eq!(
            question(&session),
            "Delete one entry",
            "{value} alone closes no question"
        );
    }
    assert_eq!(
        typed_answer(&session),
        "yes",
        "every key reached the answer"
    );

    // Only `Enter` reads the answer.
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(message(&session), CONFIRMED);
}

#[test]
fn every_other_answer_cancels_a_question_and_leaves_no_trace() {
    // `n` names the default of the question, the empty text names it as well,
    // and `no` and `ya` stand for every remaining answer.
    for value in ["n", "", "no", "ya"] {
        let mut session = session(40, 10);
        session.open_confirmation("Delete one entry", ConfirmedAction::Report);
        assert_eq!(answer(&mut session, value), Redraw::Needed);
        assert_eq!(message(&session), "", "{value:?} performs no action");
        assert_eq!(question(&session), "", "{value:?} closes the question");
        assert_eq!(
            session.buffer().to_string(),
            "\n",
            "{value:?} changes no text"
        );
        press(&mut session, 'i');
        assert_eq!(session.mode(), Mode::Insert, "{value:?} returns the keys");
    }
}

#[test]
fn a_cancel_key_closes_a_question_at_any_time() {
    // `Esc` and `Ctrl-C` cancel, and they cancel the typed `y` as well.
    let escape = Key::plain(KeyCode::Esc);
    let interrupt = Key::ctrl(KeyCode::Char('c'));
    for typed in ["", "y"] {
        for key in [escape, interrupt] {
            let mut editor = session(40, 10);
            editor.open_confirmation("Delete one entry", ConfirmedAction::Report);
            type_keys(&mut editor, typed);
            assert_eq!(
                editor.handle_event(TerminalEvent::Key(key), NOW),
                Redraw::Needed
            );
            assert_eq!(
                message(&editor),
                "",
                "{key:?} performs no action after {typed:?}"
            );
            assert_eq!(question(&editor), "", "{key:?} closes the question");
            assert_eq!(editor.mode(), Mode::Normal);
        }
    }
}

#[test]
fn a_question_completes_nothing_and_keeps_its_answer() {
    let mut session = session(40, 10);
    session.open_confirmation("Delete one entry", ConfirmedAction::Report);

    // `Tab` and `Shift-Tab` complete nothing, so they add no character.
    for code in [KeyCode::Tab, KeyCode::BackTab] {
        assert_eq!(press_code(&mut session, code), Redraw::Skipped);
        assert_eq!(typed_answer(&session), "", "{code:?} adds no character");
        assert_eq!(question(&session), "Delete one entry");
    }

    // A `Backspace` removes the character before the cursor, and one on the
    // empty answer keeps the question open.
    assert_eq!(
        press_code(&mut session, KeyCode::Backspace),
        Redraw::Skipped
    );
    assert_eq!(question(&session), "Delete one entry");
    type_keys(&mut session, "ye");
    press_code(&mut session, KeyCode::Backspace);
    assert_eq!(typed_answer(&session), "y");

    // `Tab` between the characters still completes nothing.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(typed_answer(&session), "y", "Tab writes no candidate");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(message(&session), CONFIRMED);
}

#[test]
fn the_answer_of_a_question_holds_a_bounded_number_of_characters() {
    let mut session = session(40, 10);
    session.open_confirmation("Delete one entry", ConfirmedAction::Report);

    // The bound keeps the question and its answer inside one row.
    type_keys(&mut session, &"n".repeat(CONFIRM_ANSWER_CHARS_MAX + 4));
    assert_eq!(
        typed_answer(&session).chars().count(),
        CONFIRM_ANSWER_CHARS_MAX,
        "the answer drops the characters above the bound"
    );

    press_code(&mut session, KeyCode::Enter);
    assert_eq!(message(&session), "", "a long answer performs no action");
}

#[test]
fn an_open_question_owns_every_key_over_insert_mode() {
    // The overwrite question follows a worker result, so a question can open
    // over Insert mode. A key that the question does not read must reach no
    // buffer.
    let mut session = session(40, 10);
    press(&mut session, 'i');
    assert_eq!(session.mode(), Mode::Insert);
    session.open_confirmation("Overwrite one entry", ConfirmedAction::Report);

    for code in [KeyCode::Tab, KeyCode::BackTab] {
        assert_eq!(press_code(&mut session, code), Redraw::Skipped);
        assert_eq!(
            session.buffer().to_string(),
            "\n",
            "{code:?} inserts no buffer text"
        );
        assert_eq!(typed_answer(&session), "", "{code:?} adds no character");
    }
    assert_eq!(question(&session), "Overwrite one entry");

    // The answer still reaches the question, and Insert mode regains the keys.
    answer(&mut session, "y");
    assert_eq!(message(&session), CONFIRMED);
    assert_eq!(session.mode(), Mode::Insert);
}

#[test]
fn a_second_question_is_refused_while_one_waits() {
    let mut session = session(40, 10);
    session.open_confirmation("Delete one entry", ConfirmedAction::Report);
    assert_eq!(
        session.open_confirmation("Delete two entries", ConfirmedAction::Report),
        ConfirmationRequest::Refused
    );
    answer(&mut session, "y");
    assert_eq!(message(&session), CONFIRMED);
    // Only one question waited, so the next `y` reaches the yank operator.
    press(&mut session, 'y');
    assert_eq!(
        message(&session),
        "",
        "the refused question never reached the message line"
    );
}

#[test]
fn no_key_reaches_a_closed_question() {
    let mut session = session(40, 10);
    // Without an open question `y` reaches the yank operator instead.
    press(&mut session, 'y');
    press(&mut session, 'y');
    assert_ne!(
        message(&session),
        CONFIRMED,
        "`y` answers no question while none is open"
    );

    session.open_confirmation("Delete one entry", ConfirmedAction::Report);
    answer(&mut session, "n");
    press(&mut session, 'y');
    press(&mut session, 'y');
    assert_ne!(
        message(&session),
        CONFIRMED,
        "the answered question takes no further key"
    );
    press(&mut session, 'i');
    assert_eq!(session.mode(), Mode::Insert);
}

#[test]
fn a_question_over_a_prompt_returns_the_keys_to_that_prompt() {
    // A question can open while a prompt reads a line, because the overwrite
    // question follows a worker result instead of a key.
    for value in ["y", "n"] {
        let mut session = session(40, 10);
        press(&mut session, '/');
        type_keys(&mut session, "al");
        session.open_confirmation("Overwrite one entry", ConfirmedAction::Report);
        assert_eq!(
            prompt_text(&session),
            "al",
            "the question keeps the text of the prompt"
        );

        // The question owns the keys, so its own characters reach no prompt.
        type_keys(&mut session, value);
        assert_eq!(
            prompt_text(&session),
            "al",
            "{value} reaches the answer, not the prompt"
        );
        assert_eq!(typed_answer(&session), value);

        // The `Enter` of the answer closes the question alone. The prompt keeps
        // its text and runs nothing.
        press_code(&mut session, KeyCode::Enter);
        assert_eq!(question(&session), "", "{value} closes the question");
        assert_eq!(
            prompt_text(&session),
            "al",
            "one Enter reaches the question alone, so the prompt stays open"
        );
        type_keys(&mut session, "pha");
        assert_eq!(
            prompt_text(&session),
            "alpha",
            "the prompt reads the keys again after {value}"
        );

        press_code(&mut session, KeyCode::Esc);
        press(&mut session, 'i');
        assert_eq!(
            session.mode(),
            Mode::Insert,
            "the closed prompt returns the keys to the mode after {value}"
        );
    }
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
        asked |= request.kind() == LanguageRequestKind::Query;
        let _ = session.apply_language_dispatch(&request, Err(LspError::NoServerDeclared));
    }
    asked
}

/// Refuses every queued language request with one typed language state.
///
/// The editor reports each normal state once, so a test that proves the report
/// hands the same state to every queued request.
fn refuse_language_requests_with(session: &mut Session, state: impl Fn() -> LspError) {
    while let Some(request) = session.take_language_request() {
        let _ = session.apply_language_dispatch(&request, Err(state()));
    }
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
        question(&session),
        "",
        "the save keeps every change, so `:wq` asks nothing"
    );
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
fn the_edit_command_reloads_a_clean_buffer_and_asks_before_a_dirty_one() {
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

    // A buffer with unsaved changes asks before the file replaces its text.
    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    press(&mut session, ':');
    type_keys(&mut session, "e");
    press_code(&mut session, KeyCode::Enter);
    assert!(
        session.take_file_request().is_none(),
        "the open question reads no file"
    );
    assert_eq!(
        question(&session),
        "Reload main.rs and discard the unsaved changes",
        "the question names the buffer"
    );
    assert_eq!(session.buffer().to_string(), "typed one\ntwo\n");

    // A lone `y` reads no file, because it performs no action.
    press(&mut session, 'y');
    assert!(
        session.take_file_request().is_none(),
        "one keypress reloads nothing"
    );
    press_code(&mut session, KeyCode::Enter);
    run_file_request(&mut session);
    assert_eq!(session.buffer().to_string(), "one\ntwo\n");
    assert!(!session.buffer().is_modified());
}

#[test]
fn a_cancelled_reload_keeps_the_buffer_and_its_unsaved_text() {
    // `n` names the default of the question, the empty text names it as well,
    // and `no` and `ya` stand for every remaining answer.
    for value in ["n", "", "no", "ya"] {
        let (_directory, path, mut session) =
            opened_file("session-reload-cancel", "main.rs", "one\n");
        std::fs::write(&path, "one\ntwo\n").expect("the file is writable");
        press(&mut session, 'i');
        type_keys(&mut session, "typed ");
        press_code(&mut session, KeyCode::Esc);

        press(&mut session, ':');
        type_keys(&mut session, "e");
        press_code(&mut session, KeyCode::Enter);
        answer(&mut session, value);

        assert!(
            session.take_file_request().is_none(),
            "{value:?} reads no file"
        );
        assert_eq!(
            session.buffer().to_string(),
            "typed one\n",
            "{value:?} keeps the unsaved text"
        );
        assert!(
            session.buffer().is_modified(),
            "{value:?} keeps the changes"
        );
        assert_eq!(question(&session), "", "{value:?} closes the question");
        assert_eq!(message(&session), "", "{value:?} leaves no trace");
    }
}

#[test]
fn a_confirmed_quit_keeps_the_editor_running_after_another_buffer_became_active() {
    let (directory, _path, mut session) = opened_file("session-quit-moved", "main.rs", "one\n");
    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    let asked = session.active();

    press(&mut session, ':');
    type_keys(&mut session, "q");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        question(&session),
        "Quit and discard the unsaved changes of main.rs"
    );

    // One open completes while the question waits, so another buffer becomes
    // active. The user approved no loss of that buffer.
    session.open_path(directory.write("other.rs", "other\n"));
    run_file_request(&mut session);
    assert_ne!(session.active(), asked, "the open moved the focus");

    answer(&mut session, "y");

    assert_eq!(
        session.run_state(),
        RunState::Running,
        "the answer quits only while the named buffer holds the focus"
    );
    assert_eq!(
        message(&session),
        "the focused window shows another buffer now, so the editor kept running"
    );
}

#[test]
fn a_confirmed_reload_reads_the_file_of_the_buffer_that_the_question_named() {
    let (directory, path, mut session) = opened_file("session-reload-moved", "main.rs", "one\n");
    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    let asked = session.active();
    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");

    press(&mut session, ':');
    type_keys(&mut session, "e");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        question(&session),
        "Reload main.rs and discard the unsaved changes"
    );

    // One open completes while the question waits, so another buffer becomes
    // active. The answer still reads the file of the named buffer.
    session.open_path(directory.write("other.rs", "other\n"));
    run_file_request(&mut session);
    assert_ne!(session.active(), asked, "the open moved the focus");

    answer(&mut session, "y");
    run_file_request(&mut session);

    let reloaded = session
        .buffers()
        .get(asked)
        .expect("the named buffer stays loaded");
    assert_eq!(reloaded.text().to_string(), "one\ntwo\n");
    assert!(!reloaded.is_modified());
    assert_eq!(
        session.buffer().to_string(),
        "other\n",
        "the reload replaced no other buffer"
    );
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
    assert_eq!(question(&session), "", "`:e!` asks nothing");
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
fn a_burst_of_obsolete_analyses_costs_one_log_entry_and_keeps_an_earlier_report() {
    let (_directory, mut session) = opened("main.rs", "fn main() {}\n");

    // One report reaches the message line before the burst starts.
    press(&mut session, ':');
    type_keys(&mut session, "nosuchcommand");
    press_code(&mut session, KeyCode::Enter);
    let earlier = message(&session);
    assert!(!earlier.is_empty(), "the command line rejected the command");

    // The user types while every analysis runs, so every result is obsolete.
    let burst = LOG_ENTRIES_MAX + 16;
    for _ in 0..burst {
        let request = session
            .take_analysis_request()
            .expect("the changed buffer needs one analysis");
        let obsolete = request.run(&CancellationToken::new());
        press(&mut session, 'o');
        press_code(&mut session, KeyCode::Esc);
        assert_eq!(
            session.apply_analysis_result(obsolete),
            Redraw::Skipped,
            "an obsolete buffer version changes nothing"
        );
    }

    let rows = open_log(&mut session, "logs");
    let jobs: Vec<&String> = rows.iter().filter(|row| row.contains(" JOB ")).collect();
    assert_eq!(
        jobs.len(),
        1,
        "the whole burst costs one entry, but the log holds {jobs:?}"
    );
    assert!(
        jobs[0].ends_with(&format!("analysis rejected: the buffer changed (x{burst})")),
        "the entry names its job, its outcome, and its count, not {:?}",
        jobs[0]
    );
    assert!(
        rows.iter().any(|row| row.ends_with(&earlier)),
        "the report from before the burst is still in {rows:?}"
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
fn a_server_that_the_workspace_does_not_use_is_reported_once_and_editing_continues() {
    let directory = TempDir::new("session-unused-server");
    let path = directory.write("main.rs", "fn main() {}\n");
    let mut session = file_session();

    session.open_path(path);
    run_file_request(&mut session);
    // The load queues one open. This workspace uses no declared server of the
    // buffer, which is a normal state and not a failure.
    refuse_language_requests_with(&mut session, || LspError::UnusedInWorkspace);
    assert_eq!(
        message(&session),
        "this workspace uses no language server for this buffer; editing continues",
        "an unused server names its own state, not a missing installation"
    );

    // The state reaches the message line once, so a later question repeats it
    // never.
    type_keys(&mut session, " k");
    refuse_language_requests_with(&mut session, || LspError::UnusedInWorkspace);
    assert_eq!(message(&session), "");

    // The editor stays fully usable without the server.
    press(&mut session, 'i');
    type_keys(&mut session, "// ");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "// fn main() {}\n");
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
fn a_refusal_opens_no_document_again_without_a_lost_copy() {
    let directory = TempDir::new("session-refused-request");
    let path = directory.write("main.rs", "fn main() {}\n");
    let mut session = file_session();

    session.open_path(path);
    run_file_request(&mut session);
    refuse_language_requests(&mut session);

    // The edit queues one incremental change of the document.
    press(&mut session, 'i');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);
    let change = session
        .take_language_request()
        .expect("the edit queued one synchronization");
    assert_eq!(change.kind(), LanguageRequestKind::Synchronization);

    // No running session took the change, so no copy of that document exists
    // and no fresh open can repair one.
    let _ = session.apply_language_dispatch(&change, Err(LspError::NoServerDeclared));
    assert!(
        session.take_language_request().is_none(),
        "a refusal that names no running session opens no document again"
    );

    // A question carries no text, so its refusal leaves no copy behind. The
    // editor releases the question and opens no document again.
    type_keys(&mut session, "gd");
    let query = session
        .take_language_request()
        .expect("the keys asked one question");
    assert_eq!(query.kind(), LanguageRequestKind::Query);
    let _ = session.apply_language_dispatch(&query, Err(LspError::Saturated));
    assert!(
        session.take_language_request().is_none(),
        "a refused question opens no document again"
    );

    // The editor stays fully usable after both refusals.
    press(&mut session, 'i');
    type_keys(&mut session, "y");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "xyfn main() {}\n");
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
    // The clipboard still holds the text that kvim wrote, so the recorded
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
        "text that kvim never wrote is characterwise"
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
