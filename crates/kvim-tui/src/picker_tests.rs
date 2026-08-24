//! Tests for the picker overlay: its layout, its rows, its keys, and the
//! obsolete results that it rejects.
//!
//! Every test drives one temporary workspace. The session performs no
//! filesystem work and starts no process itself, so each test runs the queued
//! picker requests as the event loop does.

use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use tokio_util::sync::CancellationToken;

use kvim_runtime::ProcessOutput;
use kvim_settings::EditorSettings;
use kvim_terminal::{Key, KeyCode, TerminalEvent};
use kvim_workspace::{PickerResult, PickerSlot, Preview, PreviewKey, PreviewTarget, temp::TempDir};

use super::picker::{PREVIEW_WIDTH_PERCENT, PickerFailure, picker_areas};
use super::session::{Redraw, Session, test_root};

const NOW: Duration = Duration::ZERO;

/// The terminal width of every test session.
const WIDTH: u16 = 80;

/// The terminal height of every test session.
const HEIGHT: u16 = 16;

/// The first result row of the picker.
const FIRST_RESULT_ROW: u16 = 2;

/// The background of the selected row in the reference palette.
const SELECTION: Color = Color::Rgb(0x34, 0x3a, 0x55);

/// The largest number of picker operations that one test drains.
const PICKER_STEPS_MAX: usize = 8;

/// Creates one workspace and one session over it.
///
/// The root is the canonical path of the temporary directory, so it matches
/// the path that a loaded buffer holds.
fn workspace() -> (TempDir, Session) {
    let dir = TempDir::new("picker");
    dir.file(
        "src/main.rs",
        "fn main() {\n    // one\n    let needle = 1;\n}\n",
    );
    dir.file("src/mode.rs", "pub enum Mode {}\n");
    dir.file("README.md", "kvim\n");
    dir.file(".gitignore", "target/\n");
    dir.file("target/debug/kvim", "binary\n");
    let root = dir.path.clone();
    let session = Session::new(
        Rect::new(0, 0, WIDTH, HEIGHT),
        EditorSettings::default(),
        test_root(root),
    );
    (dir, session)
}

/// Feeds one plain character key.
fn press(session: &mut Session, value: char) {
    session.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char(value))), NOW);
}

/// Feeds one key with the control chord.
fn press_ctrl(session: &mut Session, value: char) {
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char(value))), NOW);
}

/// Feeds one plain key without a character.
fn press_code(session: &mut Session, code: KeyCode) {
    session.handle_event(TerminalEvent::Key(Key::plain(code)), NOW);
}

/// Feeds a run of plain character keys.
fn type_keys(session: &mut Session, keys: &str) {
    for value in keys.chars() {
        press(session, value);
    }
}

/// Opens one picker through its first-release binding.
fn open_picker(session: &mut Session, keys: &str) {
    press(session, ' ');
    type_keys(session, keys);
}

/// Runs every queued worker picker operation, as the event loop does.
///
/// A search reaches the process service, so this helper leaves it for the test
/// that publishes its output.
fn drain(session: &mut Session) {
    for _ in 0..PICKER_STEPS_MAX {
        let Some(request) = session.take_picker_request() else {
            return;
        };
        if request.command().is_some() {
            continue;
        }
        let _ = session.apply_picker_result(request.run(&CancellationToken::new()));
    }
    panic!("one transition queues fewer picker requests than the bound");
}

/// Runs the queued file operation, as the event loop does.
fn drain_file(session: &mut Session) {
    if let Some(request) = session.take_file_request() {
        let _ = session.apply_file_result(request.run());
    }
}

/// Renders one session and returns the terminal cell buffer.
fn draw(session: &Session) -> CellBuffer {
    let area = session.area();
    let backend = TestBackend::new(area.width, area.height);
    let mut terminal = Terminal::new(backend).expect("the test backend never fails");
    terminal
        .draw(|frame| session.render(frame))
        .expect("the test backend never fails");
    terminal.backend().buffer().clone()
}

/// Returns one row of a rendered buffer as text, without trailing blanks.
fn row_of(buffer: &CellBuffer, y: u16) -> String {
    let area = *buffer.area();
    let mut text = String::new();
    for x in area.x..area.right() {
        if let Some(cell) = buffer.cell((x, y)) {
            text.push_str(cell.symbol());
        }
    }
    text.trim_end().to_owned()
}

/// Returns one row of one region as text, without trailing blanks.
fn region_row(buffer: &CellBuffer, area: Rect, y: u16) -> String {
    let mut text = String::new();
    for x in area.x..area.right() {
        if let Some(cell) = buffer.cell((x, y)) {
            text.push_str(cell.symbol());
        }
    }
    text.trim_end().to_owned()
}

/// Returns the visible result rows of the picker.
fn results(session: &Session) -> Vec<String> {
    let buffer = draw(session);
    let area = picker_areas(session.area()).results;
    (area.y..area.bottom())
        .map(|y| region_row(&buffer, area, y))
        .filter(|row| !row.is_empty())
        .collect()
}

/// Returns the query row of the picker.
fn prompt_row(session: &Session) -> String {
    let buffer = draw(session);
    let area = picker_areas(session.area()).prompt;
    region_row(&buffer, area, area.y)
}

/// Returns the offset of the selected result row.
fn selected_row(session: &Session) -> Option<u16> {
    let buffer = draw(session);
    (FIRST_RESULT_ROW..HEIGHT).find_map(|y| {
        let cell = buffer.cell((0, y))?;
        (cell.style().bg == Some(SELECTION)).then_some(y - FIRST_RESULT_ROW)
    })
}

#[test]
fn the_file_picker_lists_the_workspace_with_the_filename_first() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);

    assert_eq!(prompt_row(&session), ">", "the prompt sits at the top");
    let rows = results(&session);
    assert!(
        rows.iter().any(|row| row.starts_with("main.rs  src")),
        "the row shows the filename before its directory: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| !row.contains("target")),
        "the walk honours the ignore rules: {rows:?}"
    );
}

#[test]
fn the_query_ranks_the_best_match_next_to_the_prompt() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);
    type_keys(&mut session, "main");
    drain(&mut session);

    assert_eq!(prompt_row(&session), "> main");
    let rows = results(&session);
    assert!(
        rows.first().is_some_and(|row| row.starts_with("main.rs")),
        "the best match sits at the top: {rows:?}"
    );
}

#[test]
fn the_control_w_chord_removes_one_word_from_the_query() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);
    type_keys(&mut session, "mode");
    drain(&mut session);
    assert_eq!(prompt_row(&session), "> mode");
    assert!(
        results(&session)
            .iter()
            .all(|row| !row.starts_with("main.rs")),
        "the query holds the result list to one file"
    );

    press_ctrl(&mut session, 'w');
    drain(&mut session);
    assert_eq!(prompt_row(&session), ">", "the chord removes the word");
    let rows = results(&session);
    assert!(
        rows.iter().any(|row| row.starts_with("main.rs")),
        "the result list follows the query that the chord shortened: {rows:?}"
    );
}

#[test]
fn the_control_keys_move_the_selection_inside_the_picker() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);
    assert_eq!(selected_row(&session), Some(0));

    press_ctrl(&mut session, 'j');
    drain(&mut session);
    assert_eq!(selected_row(&session), Some(1));

    press_ctrl(&mut session, 'k');
    drain(&mut session);
    assert_eq!(selected_row(&session), Some(0));
}

#[test]
fn the_wide_layout_gives_the_preview_three_quarters_of_the_width() {
    let areas = picker_areas(Rect::new(0, 0, 120, 40));
    let preview = areas.preview.expect("a wide terminal shows the preview");
    assert_eq!(preview.width, 120 * PREVIEW_WIDTH_PERCENT / 100);
    assert_eq!(
        preview.x,
        areas.results.width + 1,
        "one column separates them"
    );
    assert_eq!(areas.prompt.y, 0, "the prompt sits at the top");
    assert_eq!(
        areas.results.y,
        areas.prompt.y + 2,
        "one row separates them"
    );
}

#[test]
fn a_narrow_terminal_drops_the_preview() {
    let areas = picker_areas(Rect::new(0, 0, 40, 12));
    assert_eq!(areas.preview, None);
    assert_eq!(areas.results.width, 40, "the results take the full width");
    assert_eq!(areas.prompt.width, 40);
}

#[test]
fn the_picker_shows_the_preview_of_the_selected_row() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);
    type_keys(&mut session, "main");
    drain(&mut session);

    let buffer = draw(&session);
    let preview = picker_areas(session.area())
        .preview
        .expect("the test terminal is wide");
    let rows: Vec<String> = (preview.y..HEIGHT).map(|y| row_of(&buffer, y)).collect();
    assert!(
        rows.iter().any(|row| row.contains("fn main()")),
        "the preview shows the file: {rows:?}"
    );
}

#[test]
fn a_stale_preview_never_reaches_the_screen() {
    let (dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);

    let stale = PickerResult::Preview {
        key: PreviewKey::new(
            test_root(dir.path.clone()),
            kvim_path::WorktreeRelativePath::new("src/mode.rs").expect("the preview path is valid"),
            PreviewTarget::Match { line: 400 },
        ),
        outcome: Ok(Preview {
            first_line: 400,
            lines: vec!["stale preview line".to_owned()],
            truncated: false,
        }),
    };
    assert_eq!(session.apply_picker_result(stale), Redraw::Skipped);
    let buffer = draw(&session);
    let rows: Vec<String> = (0..HEIGHT).map(|y| row_of(&buffer, y)).collect();
    assert!(
        rows.iter().all(|row| !row.contains("stale preview line")),
        "the reader already moved past that row: {rows:?}"
    );
}

#[test]
fn a_truncated_preview_shows_its_own_notice() {
    let (dir, mut session) = workspace();
    std::fs::write(
        dir.join("src/main.rs"),
        "x".repeat(kvim_workspace::PREVIEW_LINE_CHARS_MAX + 1),
    )
    .expect("the temporary directory is writable");
    open_picker(&mut session, "ff");
    drain(&mut session);
    type_keys(&mut session, "main");
    drain(&mut session);

    let buffer = draw(&session);
    let rows: Vec<String> = (0..HEIGHT).map(|y| row_of(&buffer, y)).collect();
    assert!(
        rows.iter().any(|row| row.contains("preview stops")),
        "the picker reports preview clipping: {rows:?}"
    );
}

#[test]
fn a_superseded_search_never_reaches_the_screen() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "f/");
    type_keys(&mut session, "need");
    let request = session
        .take_picker_request()
        .expect("the query starts one search");
    assert!(request.command().is_some(), "a search runs one command");

    // The next key makes the running search obsolete.
    press(&mut session, 'l');
    let obsolete = request.publish(&ProcessOutput {
        status_code: Some(0),
        stdout: b"./src/main.rs:3:9:let needle = 1;\n".to_vec(),
        stderr: Vec::new(),
    });
    assert_eq!(session.apply_picker_result(obsolete), Redraw::Skipped);
    assert!(
        results(&session)
            .iter()
            .all(|row| !row.contains("main.rs:3")),
        "the result of the older query never reaches the screen"
    );
}

#[test]
fn accepting_one_search_row_opens_the_file_at_the_matched_line() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "f/");
    type_keys(&mut session, "needle");
    let request = session
        .take_picker_request()
        .expect("the query starts one search");
    let result = request.publish(&ProcessOutput {
        status_code: Some(0),
        stdout: b"./src/main.rs:3:9:    let needle = 1;\n".to_vec(),
        stderr: Vec::new(),
    });
    let _ = session.apply_picker_result(result);
    let rows = results(&session);
    assert!(
        rows.iter().any(|row| row.starts_with("main.rs:3  src")),
        "the matched line names its file, its line, and its directory: {rows:?}"
    );

    press_code(&mut session, KeyCode::Enter);
    drain_file(&mut session);
    assert_eq!(session.active_buffer().name(), "main.rs");
    let buffer = draw(&session);
    let statusline = row_of(&buffer, HEIGHT - 2);
    assert!(
        statusline.ends_with("3:9"),
        "the cursor sits at the matched position: {statusline}"
    );
}

#[test]
fn cancelling_the_picker_restores_the_previous_view() {
    let (_dir, mut session) = workspace();
    let before = draw(&session);

    open_picker(&mut session, "ff");
    drain(&mut session);
    type_keys(&mut session, "ma");
    drain(&mut session);
    press_ctrl(&mut session, 'j');
    assert_ne!(draw(&session), before, "the picker covers the terminal");

    press_code(&mut session, KeyCode::Esc);
    assert_eq!(draw(&session), before, "the previous view returns exactly");
}

#[test]
fn a_missing_search_command_is_reported_once_and_keeps_the_editor_usable() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "f/");
    type_keys(&mut session, "x");
    session
        .take_picker_request()
        .expect("the query starts one search");

    let _ = session.abandon_picker_request(PickerSlot::Candidates, PickerFailure::CommandMissing);
    let reported = session
        .message()
        .map(|message| message.text().to_owned())
        .unwrap_or_default();
    assert!(
        reported.contains("`rg`"),
        "the editor names the missing command: {reported}"
    );

    // The next key clears the message line, and the second failure adds no
    // second report.
    press_ctrl(&mut session, 'j');
    let _ = session.abandon_picker_request(PickerSlot::Candidates, PickerFailure::CommandMissing);
    assert_eq!(session.message().map(|message| message.text()), None);

    // The editor stays fully usable without the search picker.
    press_code(&mut session, KeyCode::Esc);
    press(&mut session, 'i');
    type_keys(&mut session, "ok");
    assert_eq!(session.buffer().to_string(), "ok\n");
}

#[test]
fn the_buffer_picker_lists_the_loaded_buffers() {
    let (dir, mut session) = workspace();
    session.open_path(dir.join("src/main.rs"));
    drain_file(&mut session);

    open_picker(&mut session, "o");
    let rows = results(&session);
    assert!(
        rows.iter().any(|row| row.starts_with("main.rs  src")),
        "the loaded buffer is one row: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.starts_with("[Scratch]")),
        "the scratch buffer is one row: {rows:?}"
    );
}

#[test]
fn an_accepted_picker_row_records_the_position_that_it_left() {
    let (_dir, mut session) = workspace();
    assert_eq!(session.active_buffer().name(), "[Scratch]");

    open_picker(&mut session, "ff");
    drain(&mut session);
    type_keys(&mut session, "main");
    drain(&mut session);
    press_code(&mut session, KeyCode::Enter);
    drain_file(&mut session);
    assert_eq!(session.active_buffer().name(), "main.rs");

    press_ctrl(&mut session, 'o');
    assert_eq!(
        session.active_buffer().name(),
        "[Scratch]",
        "`Ctrl-O` returns to the buffer that the accepted row left"
    );

    // A terminal reports `Ctrl-I` as `Tab`.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(
        session.active_buffer().name(),
        "main.rs",
        "`Tab` returns to the accepted row"
    );
}
