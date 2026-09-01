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
use ratatui::style::{Color, Style};
use tokio_util::sync::CancellationToken;

use kvim_runtime::ProcessOutput;
use kvim_settings::{EditorSettings, FileTreeIcons};
use kvim_terminal::{Key, KeyCode, TerminalEvent};
use kvim_workspace::{PickerResult, PickerSlot, Preview, PreviewKey, PreviewTarget, temp::TempDir};

use crate::cells::text_cells;
use crate::icons::file_icon;
use crate::picker::{
    PREVIEW_WIDTH_PERCENT, PickerFailure, RESULT_COLUMN_GAP_CELLS, RESULT_ICON_CELLS,
    RESULT_MARKER_CELLS, RESULT_SELECTED_MARKER, picker_areas,
};
use crate::session::{Redraw, Session, test_root};
use crate::theme::{Theme, ThemeRole};

const NOW: Duration = Duration::ZERO;

/// The terminal width of every test session.
const WIDTH: u16 = 80;

/// The terminal height of every test session.
const HEIGHT: u16 = 16;

/// The first result row of the picker.
///
/// The title row and its gap, then the query row and its gap, then the header
/// row of the result list sit above the results.
const FIRST_RESULT_ROW: u16 = 5;

/// Returns the style that the theme gives one semantic role.
///
/// The tests read every expected color through this lookup, so the palette
/// stays owned by the theme module alone and a recolor never edits a test.
fn role(role: ThemeRole) -> Style {
    Theme::new().style(role)
}

/// The largest number of picker operations that one test drains.
const PICKER_STEPS_MAX: usize = 8;

/// Creates one workspace and one session over it.
///
/// The root is the canonical path of the temporary directory, so it matches
/// the path that a loaded buffer holds.
fn workspace() -> (TempDir, Session) {
    workspace_sized(WIDTH, HEIGHT)
}

/// Creates one workspace and one session of one terminal size over it.
fn workspace_sized(width: u16, height: u16) -> (TempDir, Session) {
    workspace_with(width, height, EditorSettings::default())
}

/// Creates one workspace and one session of one terminal size and one settings
/// value over it.
fn workspace_with(width: u16, height: u16, settings: EditorSettings) -> (TempDir, Session) {
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
    let session = Session::new(Rect::new(0, 0, width, height), settings, test_root(root));
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

/// Feeds one key without a character with the control chord.
fn press_ctrl_code(session: &mut Session, code: KeyCode) {
    session.handle_event(TerminalEvent::Key(Key::ctrl(code)), NOW);
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

/// Renders one session and returns the cell that holds the terminal cursor.
///
/// The picker covers the complete terminal and owns the one cursor cell that
/// the frame reports, so this cell is the cursor of the query row.
fn cursor_position(session: &Session) -> (u16, u16) {
    let area = session.area();
    let backend = TestBackend::new(area.width, area.height);
    let mut terminal = Terminal::new(backend).expect("the test backend never fails");
    terminal
        .draw(|frame| session.render(frame))
        .expect("the test backend never fails");
    let position = terminal
        .get_cursor_position()
        .expect("the test backend never fails");
    (position.x, position.y)
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

/// Returns the visible result rows of the picker, without the leading marker
/// column and the icon column, so the assertions read the candidate text alone.
///
/// Every test session keeps the shown icons of the default settings, so the
/// icon column stands between the marker and the filename.
fn results(session: &Session) -> Vec<String> {
    result_rows(session, RESULT_MARKER_CELLS + RESULT_ICON_CELLS)
}

/// Returns the visible result rows of the picker after `skip` leading cells.
fn result_rows(session: &Session, skip: u16) -> Vec<String> {
    let buffer = draw(session);
    let mut area = picker_areas(session.area()).results;
    area.x = area.x.saturating_add(skip);
    area.width = area.width.saturating_sub(skip);
    (area.y..area.bottom())
        .map(|y| region_row(&buffer, area, y))
        .filter(|row| !row.is_empty())
        .collect()
}

/// Returns the complete query row of the picker, match counter included.
fn query_row(session: &Session) -> String {
    let buffer = draw(session);
    let area = picker_areas(session.area()).prompt;
    region_row(&buffer, area, area.y)
}

/// Returns the query row of the picker without its match counter.
///
/// The counter stands at the right edge of the same row, so at least two blanks
/// separate it from the typed query in every test terminal.
fn prompt_row(session: &Session) -> String {
    let row = query_row(session);
    match row.rsplit_once("  ") {
        Some((query, _)) => query.trim_end().to_owned(),
        None => row,
    }
}

/// Returns the glyph that one rendered cell holds.
fn symbol_at(buffer: &CellBuffer, x: u16, y: u16) -> String {
    buffer
        .cell((x, y))
        .expect("the tests read cells of the rendered terminal")
        .symbol()
        .to_owned()
}

/// Returns the foreground color of one rendered cell.
fn foreground_at(buffer: &CellBuffer, x: u16, y: u16) -> Option<Color> {
    buffer
        .cell((x, y))
        .expect("the tests read cells of the rendered terminal")
        .style()
        .fg
}

/// Returns the background color of one rendered cell.
fn background_at(buffer: &CellBuffer, x: u16, y: u16) -> Option<Color> {
    buffer
        .cell((x, y))
        .expect("the tests read cells of the rendered terminal")
        .style()
        .bg
}

/// Splits one painted result row into its filename and its directory.
///
/// The row leaves [`RESULT_COLUMN_GAP_CELLS`] blank cells between the two, so
/// the first run of two blanks names the boundary.
fn split_result_row(row: &str) -> (&str, &str) {
    row.split_once("  ")
        .map_or((row, ""), |(name, rest)| (name, rest.trim_start()))
}

/// Returns the number of blank cells before the first glyph of one row.
fn leading_blanks(row: &str) -> usize {
    row.chars().take_while(|value| *value == ' ').count()
}

/// Returns the offset of the selected result row.
fn selected_row(session: &Session) -> Option<u16> {
    let buffer = draw(session);
    (FIRST_RESULT_ROW..HEIGHT).find_map(|y| {
        let cell = buffer.cell((0, y))?;
        (cell.style().bg == role(ThemeRole::PickerSelection).bg).then_some(y - FIRST_RESULT_ROW)
    })
}

#[test]
fn the_file_picker_lists_the_workspace_with_the_filename_first() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);

    assert_eq!(
        prompt_row(&session),
        "> Search",
        "the empty query shows the placeholder"
    );
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
    assert_eq!(
        prompt_row(&session),
        "> Search",
        "the chord removes the word and the placeholder returns"
    );
    let rows = results(&session);
    assert!(
        rows.iter().any(|row| row.starts_with("main.rs")),
        "the result list follows the query that the chord shortened: {rows:?}"
    );
}

#[test]
fn the_query_row_draws_its_cursor_at_the_position_of_the_prompt() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "fb");
    drain(&mut session);
    let (start, row) = cursor_position(&session);
    let prompt = picker_areas(session.area()).prompt;
    assert_eq!(
        row, prompt.y,
        "the cursor of the frame sits on the query row"
    );
    assert_eq!(
        start,
        prompt.x + 2,
        "the prefix `> ` takes two cells before the first character of the query"
    );

    type_keys(&mut session, "main");
    drain(&mut session);
    assert_eq!(
        cursor_position(&session),
        (start + 4, row),
        "the cursor follows the four typed characters"
    );

    // The picker reads its query through the prompt line, so the query row
    // draws the cursor of that line and follows every motion of it.
    press_code(&mut session, KeyCode::Home);
    assert_eq!(
        cursor_position(&session),
        (start, row),
        "`Home` returns the cursor to the first character of the query"
    );

    press_ctrl_code(&mut session, KeyCode::Right);
    assert_eq!(
        cursor_position(&session),
        (start + 4, row),
        "the word motion reaches the end of the one query word"
    );
    assert_eq!(
        prompt_row(&session),
        "> main",
        "no motion changes the query"
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
fn the_selected_row_paints_its_filename_with_the_row_foreground_not_the_title_color() {
    // The picker normally colors a candidate's filename with the title accent
    // so the reader finds it first, but that accent disappears against the
    // filled selection band, so the selected row must keep the band's own
    // readable foreground across the complete row, filename included.
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);

    let buffer = draw(&session);
    let area = picker_areas(session.area()).results;
    let filename_x = area.x + RESULT_MARKER_CELLS + RESULT_ICON_CELLS;
    let filename_fg = buffer
        .cell((filename_x, area.y))
        .expect("the selected row paints its first result")
        .style()
        .fg;
    assert_eq!(
        filename_fg,
        role(ThemeRole::PickerSelection).fg,
        "the filename keeps the readable foreground of the filled selection band"
    );
    assert_ne!(
        filename_fg,
        role(ThemeRole::Title).fg,
        "the title accent color would vanish against the accent selection band"
    );

    let marker_fg = buffer
        .cell((area.x, area.y))
        .expect("the selected row paints its marker")
        .style()
        .fg;
    assert_eq!(
        marker_fg, filename_fg,
        "the marker and the filename share the one readable foreground"
    );
}

#[test]
fn a_file_row_paints_the_marker_column_the_icon_the_name_and_the_dim_directory() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);
    // Both files of `src` match the query, and the selection moves to the
    // second of them, so the first row shows the colors of a plain row.
    type_keys(&mut session, "src");
    drain(&mut session);
    press_ctrl(&mut session, 'j');
    drain(&mut session);

    let buffer = draw(&session);
    let area = picker_areas(session.area()).results;
    let y = area.y;
    for x in area.x..area.x + RESULT_MARKER_CELLS {
        assert_eq!(
            symbol_at(&buffer, x, y),
            " ",
            "a row that is not selected keeps its marker column blank"
        );
    }

    let mut text_area = area;
    let text_x = RESULT_MARKER_CELLS + RESULT_ICON_CELLS;
    text_area.x += text_x;
    text_area.width -= text_x;
    let text = region_row(&buffer, text_area, y);
    let (name, directory) = split_result_row(&text);
    assert_eq!(directory, "src", "the row names the directory of the file");

    let icon = file_icon(name);
    let icon_x = area.x + RESULT_MARKER_CELLS;
    assert_eq!(
        symbol_at(&buffer, icon_x, y),
        icon.glyph,
        "the icon column shows the one file icon of `{name}`"
    );
    assert_eq!(
        foreground_at(&buffer, icon_x, y),
        role(ThemeRole::Icon(icon.role)).fg,
        "the icon carries the color of its own role"
    );
    assert_eq!(
        foreground_at(&buffer, text_area.x, y),
        role(ThemeRole::Title).fg,
        "the filename carries the title color, so the reader finds it first"
    );
    let directory_x = text_area.x
        + u16::try_from(text_cells(name)).expect("the filename stays inside the terminal")
        + RESULT_COLUMN_GAP_CELLS;
    assert_eq!(
        foreground_at(&buffer, directory_x, y),
        role(ThemeRole::PickerMuted).fg,
        "the directory follows the filename in a dim color"
    );
}

#[test]
fn a_search_row_names_its_line_its_directory_and_its_matched_text_after_the_icon() {
    // The result column of the default test terminal is too narrow for the
    // matched text, so this session takes a wider one.
    let (_dir, mut session) = workspace_sized(160, HEIGHT);
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

    assert_eq!(
        results(&session),
        vec!["main.rs:3  src  let needle = 1;".to_owned()],
        "the matched row keeps the filename, the line, the directory, and the text"
    );
    let buffer = draw(&session);
    let area = picker_areas(session.area()).results;
    assert_eq!(
        symbol_at(&buffer, area.x + RESULT_MARKER_CELLS, area.y),
        file_icon("main.rs").glyph,
        "a search row carries the icon of the file that holds the match"
    );
}

#[test]
fn a_buffer_row_without_a_directory_ends_after_its_name() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "o");

    assert_eq!(
        results(&session),
        vec!["[Scratch]".to_owned()],
        "a buffer without a file shows its name alone"
    );
    let buffer = draw(&session);
    let area = picker_areas(session.area()).results;
    assert_eq!(
        symbol_at(&buffer, area.x + RESULT_MARKER_CELLS, area.y),
        file_icon("[Scratch]").glyph,
        "a buffer name without an extension takes the default file icon"
    );
}

#[test]
fn hidden_icons_reserve_no_cell_and_keep_the_result_columns_aligned() {
    let mut settings = EditorSettings::default();
    settings.windows.file_tree_icons = FileTreeIcons::Hidden;
    let (_dir, mut session) = workspace_with(WIDTH, HEIGHT, settings);
    open_picker(&mut session, "ff");
    drain(&mut session);

    let rows = result_rows(&session, RESULT_MARKER_CELLS);
    assert!(
        rows.iter().any(|row| row.starts_with("main.rs  src")),
        "the hidden icon column moves the name to the marker column: {rows:?}"
    );
    let buffer = draw(&session);
    let area = picker_areas(session.area()).results;
    for (offset, row) in rows.iter().enumerate() {
        let y = area.y
            + u16::try_from(offset).expect("the visible rows stay inside the terminal height");
        assert_eq!(
            symbol_at(&buffer, area.x + RESULT_MARKER_CELLS, y),
            row.chars()
                .next()
                .expect("every listed row holds one glyph")
                .to_string(),
            "every name starts on the same column when the icons stay hidden"
        );
    }
}

#[test]
fn the_marker_and_the_selection_band_mark_the_selected_row_alone() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);
    press_ctrl(&mut session, 'j');
    drain(&mut session);

    let buffer = draw(&session);
    let area = picker_areas(session.area()).results;
    let selected = area.y + 1;
    let marker = Rect::new(area.x, selected, RESULT_MARKER_CELLS, 1);
    assert_eq!(
        region_row(&buffer, marker, selected),
        RESULT_SELECTED_MARKER.trim_end(),
        "the marker names the selected row"
    );
    let band = role(ThemeRole::PickerSelection).bg;
    for x in area.x..area.right() {
        assert_eq!(
            background_at(&buffer, x, selected),
            band,
            "the band covers the complete selected row"
        );
    }
    assert_eq!(
        region_row(
            &buffer,
            Rect::new(area.x, area.y, RESULT_MARKER_CELLS, 1),
            area.y
        ),
        "",
        "no other row shows the marker"
    );
    assert_ne!(
        background_at(&buffer, area.x, area.y),
        band,
        "no other row carries the band"
    );
}

#[test]
fn a_narrow_results_column_clips_every_result_row_inside_its_rectangle() {
    // The width keeps the preview beside the smallest result column that the
    // layout allows, so every row of the search picker runs past its edge.
    let (_dir, mut session) = workspace_sized(68, HEIGHT);
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

    let areas = picker_areas(session.area());
    let column = areas.results;
    let preview = areas.preview.expect("the width keeps one preview");
    assert!(
        usize::from(column.width) < text_cells("main.rs:3  src  let needle = 1;"),
        "the narrow column cannot hold the complete row"
    );
    assert_eq!(
        results(&session),
        vec!["main.rs:3  s".to_owned()],
        "the row stops at the right edge of the result column"
    );

    let buffer = draw(&session);
    for y in column.y..column.bottom() {
        for x in column.right()..preview.x {
            assert_eq!(
                symbol_at(&buffer, x, y),
                " ",
                "no row writes a cell outside the result column"
            );
        }
    }
    assert_eq!(
        background_at(&buffer, column.right() - 1, column.y),
        role(ThemeRole::PickerSelection).bg,
        "the band of the selected row reaches the right edge of the column"
    );
    assert_ne!(
        background_at(&buffer, column.right(), column.y),
        role(ThemeRole::PickerSelection).bg,
        "the band stops at that edge"
    );
}

#[test]
fn each_picker_kind_centers_its_own_title_beside_the_close_hint() {
    for (keys, title) in [("ff", "Files"), ("f/", "Search"), ("o", "Buffers")] {
        let (_dir, mut session) = workspace();
        open_picker(&mut session, keys);
        drain(&mut session);

        let area = picker_areas(session.area())
            .title
            .expect("the test terminal shows the title row");
        let row = region_row(&draw(&session), area, area.y);
        assert!(
            row.ends_with("esc"),
            "the close hint keeps the right edge of the title row: {row}"
        );
        assert_eq!(
            row.trim_end_matches("esc").trim(),
            title,
            "the title row names this picker kind: {row}"
        );
        assert_eq!(
            leading_blanks(&row),
            (usize::from(area.width) - title.len()) / 2,
            "the title centers over the results column: {row}"
        );
    }
}

#[test]
fn a_narrow_results_column_keeps_the_centered_title_clear_of_the_close_hint() {
    // A narrow column cannot center `Buffers` and still clear the `esc` hint,
    // so the title moves left and then clips. The hint always survives.
    for width in [10, 12] {
        let (_dir, mut session) = workspace_sized(width, HEIGHT);
        open_picker(&mut session, "o");
        drain(&mut session);

        let area = picker_areas(session.area())
            .title
            .expect("the test terminal shows the title row");
        let row = region_row(&draw(&session), area, area.y);
        assert!(
            row.ends_with("esc"),
            "the close hint survives the narrow column: {row}"
        );
        let title = row.trim_end_matches("esc");
        assert!(
            title.ends_with(' '),
            "one blank stands between the title and the hint: {row}"
        );
        let title = title.trim();
        assert!(
            !title.is_empty() && "Buffers".starts_with(title),
            "the title clips instead of printing over the hint: {row}"
        );
    }
}

#[test]
fn the_query_row_reports_the_matched_count_beside_the_candidate_count() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);

    // The fixture holds four files that the ignore rules keep.
    assert!(
        query_row(&session).ends_with("4 / 4"),
        "the empty query matches every candidate: {}",
        query_row(&session)
    );

    type_keys(&mut session, "main");
    drain(&mut session);
    assert!(
        query_row(&session).ends_with("1 / 4"),
        "the query narrows the matched count alone: {}",
        query_row(&session)
    );
    assert_eq!(
        prompt_row(&session),
        "> main",
        "the counter never prints over the typed query"
    );
}

#[test]
fn the_results_header_and_the_preview_title_center_over_their_columns() {
    let (_dir, mut session) = workspace();
    open_picker(&mut session, "ff");
    drain(&mut session);
    type_keys(&mut session, "main");
    drain(&mut session);

    let areas = picker_areas(session.area());
    let buffer = draw(&session);
    let header = areas
        .results_header
        .expect("the test terminal shows the header row");
    let row = region_row(&buffer, header, header.y);
    assert_eq!(row.trim(), "Results");
    assert_eq!(
        leading_blanks(&row),
        (usize::from(header.width) - "Results".len()) / 2,
        "the header centers over the results column: {row}"
    );

    let preview = areas.preview.expect("the test terminal is wide");
    let title = region_row(&buffer, preview, preview.y);
    assert_eq!(title.trim(), "main.rs");
    assert_eq!(
        leading_blanks(&title),
        (usize::from(preview.width) - " main.rs ".len()) / 2 + 1,
        "the preview title centers over the preview column: {title}"
    );
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
    let title = areas.title.expect("a tall terminal shows the title row");
    assert_eq!(title.y, 0, "the title sits at the top");
    assert_eq!(
        areas.prompt.y,
        title.y + 2,
        "the title row and its gap sit above the prompt"
    );
    let header = areas
        .results_header
        .expect("a tall terminal shows the header row");
    assert_eq!(
        header.y,
        areas.prompt.y + 2,
        "one row separates the prompt from the header of the result list"
    );
    assert_eq!(
        areas.results.y,
        header.y + 1,
        "the results start directly below their header"
    );
    let hint = areas.hint.expect("a tall terminal shows the hint row");
    assert_eq!(
        hint.y,
        areas.results.bottom(),
        "the hint row sits directly below the results"
    );
}

#[test]
fn a_terminal_that_just_affords_the_complete_chrome_keeps_one_result_row() {
    let areas = picker_areas(Rect::new(0, 0, 80, 8));
    assert!(
        areas.title.is_some() && areas.results_header.is_some() && areas.hint.is_some(),
        "eight rows are the shortest terminal that affords the complete chrome"
    );
    assert_eq!(areas.results.height, 1, "one result row remains");
}

#[test]
fn a_short_terminal_drops_the_title_row_and_the_hint_row_but_keeps_one_result_row() {
    let areas = picker_areas(Rect::new(0, 0, 80, 3));
    assert_eq!(
        areas.title, None,
        "three rows cannot afford the title row and one result row too"
    );
    assert_eq!(
        areas.hint, None,
        "the hint row drops together with the title row"
    );
    assert_eq!(areas.prompt.height, 1, "the prompt stays mandatory");
    assert_eq!(
        areas.results.height, 1,
        "the picker always shows at least one match"
    );
}

#[test]
fn a_terminal_too_short_for_the_header_row_drops_it_and_keeps_the_result_rows() {
    // Seven rows afford the complete chrome of every row except the header of
    // the result list, so the chrome drops together rather than eating the one
    // row that the results need.
    let areas = picker_areas(Rect::new(0, 0, 80, 7));
    assert_eq!(
        areas.results_header, None,
        "the header row drops with the rest of the optional chrome"
    );
    assert_eq!(areas.title, None, "the title row drops with the header row");
    assert_eq!(areas.hint, None, "the hint row drops with the header row");
    assert_eq!(
        areas.results.y,
        areas.prompt.y + 2,
        "without the header the results follow the notice row directly"
    );
    assert!(
        areas.results.height >= 1,
        "a short terminal still shows a result row"
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
