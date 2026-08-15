//! Tests for the pure state transitions of the event loop.
//!
//! No test opens a terminal. The session receives normalized events and an
//! elapsed time, so every transition is deterministic.

use std::time::Duration;

use ratatui::layout::Rect;

use crate::input::Mode;
use crate::settings::EditorSettings;
use crate::terminal::{FocusChange, Key, KeyCode, TerminalEvent};

use super::session::{MessageLevel, Redraw, RunState, Session};

const NOW: Duration = Duration::ZERO;

/// Creates a session over one terminal size.
fn session(width: u16, height: u16) -> Session {
    Session::new(Rect::new(0, 0, width, height), EditorSettings::default())
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
    assert_eq!(session.buffer().to_string(), "let x = 42;\ny");
    assert!(session.buffer().is_modified());

    // The same digit opens a count again after the mode returns to Normal.
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "2gg");
    assert_eq!(session.buffer().to_string(), "let x = 42;\ny");
}

#[test]
fn insert_mode_wires_enter_and_backspace_to_the_editor_entry_points() {
    let mut session = session(40, 10);
    press(&mut session, 'i');
    type_keys(&mut session, "    alpha");

    // `Enter` copies the indent of the previous non-empty line.
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "beta");
    assert_eq!(session.buffer().to_string(), "    alpha\n    beta");

    // `Backspace` deletes one character at a time.
    for _ in 0..4 {
        press_code(&mut session, KeyCode::Backspace);
    }
    assert_eq!(session.buffer().to_string(), "    alpha\n    ");

    // At column zero it joins the cursor line with the line above it.
    for _ in 0..5 {
        press_code(&mut session, KeyCode::Backspace);
    }
    assert_eq!(session.buffer().to_string(), "    alpha");
}

#[test]
fn the_tab_key_follows_the_indent_settings() {
    let mut soft = session(40, 10);
    press(&mut soft, 'i');
    press_code(&mut soft, KeyCode::Tab);
    assert_eq!(soft.buffer().to_string(), "    ");

    let mut settings = EditorSettings::default();
    settings.indent.expand_tab = false;
    let mut hard = Session::new(Rect::new(0, 0, 40, 10), settings);
    press(&mut hard, 'i');
    press_code(&mut hard, KeyCode::Tab);
    assert_eq!(hard.buffer().to_string(), "\t");
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
fn the_which_key_deadline_wakes_the_loop_and_the_sequence_deadline_resets_it() {
    let settings = EditorSettings::default();
    let mut session = session(60, 20);
    assert_eq!(session.next_deadline(), None, "no sequence is pending");

    press(&mut session, ' ');
    assert_eq!(
        session.next_deadline(),
        Some(settings.input.which_key_delay),
        "the loop wakes when the overlay appears"
    );
    assert_eq!(session.tick(settings.input.which_key_delay), Redraw::Needed);
    assert_eq!(
        session.next_deadline(),
        Some(settings.input.sequence_timeout),
        "the overlay is visible, so only the sequence deadline remains"
    );
    assert_eq!(
        session.tick(settings.input.sequence_timeout),
        Redraw::Needed,
        "the expired sequence hides the overlay"
    );
    assert_eq!(session.next_deadline(), None);
    assert_eq!(
        session.tick(settings.input.sequence_timeout),
        Redraw::Skipped
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

    // Saving and opening arrive in a later release.
    press(&mut session, ':');
    type_keys(&mut session, "w");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(message(&session), "saving arrives in a later release");

    // `:q` closes the last window and ends the editor.
    press(&mut session, ':');
    type_keys(&mut session, "q");
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
    assert_eq!(session.buffer().to_string(), "alpha");
}

#[test]
fn a_new_command_clears_the_previous_message() {
    let mut session = session(60, 10);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert_eq!(message(&session), "saving arrives in a later release");
    press(&mut session, 'j');
    assert_eq!(message(&session), "");
}

#[test]
fn an_exhausted_history_reports_instead_of_changing_the_buffer() {
    let mut session = session(40, 10);
    press(&mut session, 'u');
    assert_eq!(message(&session), "no further change");
    assert_eq!(session.buffer().to_string(), "");
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
