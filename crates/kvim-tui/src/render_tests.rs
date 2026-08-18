//! Tests for the rendered frame: buffer text, line numbers, the cursor, every
//! selection kind, search matches, chrome, overlays, and narrow terminals.

use std::path::PathBuf;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use kvim_settings::{EditorSettings, WHICH_KEY_DELAY_DEFAULT};
use kvim_terminal::{Key, KeyCode, TerminalEvent};
use kvim_workspace::temp::TempDir;

use super::buffer_view::WINBAR_ROWS;
use super::session::Session;
use super::window::WindowId;

const NOW: Duration = Duration::ZERO;

/// Returns the workspace root that the file tree of a test session shows.
fn workspace_root() -> PathBuf {
    PathBuf::from("/workspace")
}

/// The which-key delay of the settings that every test session holds.
const WHICH_KEY_DELAY: Duration = WHICH_KEY_DELAY_DEFAULT;

/// The background of a Visual selection in the reference palette.
const SELECTION: Color = Color::Rgb(0x28, 0x34, 0x57);

/// The background of one search match in the reference palette.
const SEARCH: Color = Color::Rgb(0x3d, 0x59, 0xa1);

/// The background of the current search match, and the color of the cursor
/// line number, in the reference palette.
const ACCENT_WARM: Color = Color::Rgb(0xff, 0x9e, 0x64);

/// The muted text color of the reference palette.
const MUTED: Color = Color::Rgb(0x3b, 0x42, 0x61);

/// The title color of the reference palette.
const TITLE: Color = Color::Rgb(0x7a, 0xa2, 0xf7);

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

/// Renders one session and returns one row as text.
fn row(session: &Session, y: u16) -> String {
    row_of(&draw(session), y)
}

/// Renders one session and returns the style of one cell.
fn style_at(session: &Session, x: u16, y: u16) -> Style {
    draw(session)
        .cell((x, y))
        .expect("the test reads a cell inside the terminal")
        .style()
}

/// Renders one session and returns the cell that holds the terminal cursor.
///
/// The editor paints no cursor cell. It reports the cursor position of the
/// focused window, and the terminal draws its own cursor there.
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

/// Reports whether one rendered cell inverts its colors.
fn is_reversed(session: &Session, x: u16, y: u16) -> bool {
    style_at(session, x, y)
        .add_modifier
        .contains(Modifier::REVERSED)
}

/// Creates a session over one terminal size.
fn session(width: u16, height: u16) -> Session {
    Session::new(
        Rect::new(0, 0, width, height),
        EditorSettings::default(),
        workspace_root(),
    )
}

/// Feeds one plain character key.
fn press(session: &mut Session, value: char) {
    session.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char(value))), NOW);
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

/// Opens one path and runs the queued file request, like the event loop.
fn open_file(session: &mut Session, path: PathBuf) {
    session.open_path(path);
    let request = session
        .take_file_request()
        .expect("the open queued one file request");
    session.apply_file_result(request.run());
}

/// Creates a session that holds `lines` numbered lines, with the cursor at the
/// start of the buffer.
fn with_lines(width: u16, height: u16, lines: usize) -> Session {
    let mut session = session(width, height);
    press(&mut session, 'i');
    for index in 0..lines {
        if index > 0 {
            press_code(&mut session, KeyCode::Enter);
        }
        type_keys(&mut session, &format!("line{index}"));
    }
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "gg");
    session
}

#[test]
fn every_window_paints_the_rows_that_its_rectangle_reserves() {
    // The rectangle of a window holds one winbar row and the text rows. The
    // viewport must report the text rows alone, and the renderer must paint
    // every row of the rectangle, so no row of the terminal stays unclaimed.
    for height in 6..14u16 {
        let mut session = with_lines(40, height, 3);
        // The inverse adaptive rule stacks a second window below the first.
        session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('\\'))), NOW);
        let buffer = draw(&session);

        for region in session.windows().layout().regions() {
            let area = region.area;
            let viewport = session
                .windows()
                .viewport(region.id)
                .expect("every editor region owns one viewport");
            assert_eq!(
                u16::from(viewport.height_rows()) + WINBAR_ROWS,
                area.height,
                "the window reserves the text rows of its rectangle at {height} rows"
            );
            for y in area.y + WINBAR_ROWS..area.bottom() {
                assert!(
                    !row_of(&buffer, y).is_empty(),
                    "row {y} of the rectangle at {height} rows carries text or a marker"
                );
            }
        }
    }
}

#[test]
fn one_window_shows_the_winbar_the_text_and_the_chrome() {
    let mut session = session(28, 8);
    press(&mut session, 'i');
    type_keys(&mut session, "alpha");

    // Row 0 is the winbar, rows 1 to 5 hold the text, row 6 is the statusline,
    // and row 7 is the message line.
    assert_eq!(row(&session, 0), " [Scratch] [+]");
    assert_eq!(row(&session, 1), " 1   alpha");
    assert_eq!(
        row(&session, 2),
        "~",
        "the rows below the buffer are marked"
    );
    assert_eq!(row(&session, 6), " Insert                 1:6");
    assert_eq!(row(&session, 7), "");
}

#[test]
fn the_winbar_marks_a_modified_buffer_only_after_a_change() {
    let mut session = session(28, 6);
    assert_eq!(row(&session, 0), " [Scratch]");
    press(&mut session, 'i');
    type_keys(&mut session, "x");
    assert_eq!(row(&session, 0), " [Scratch] [+]");
}

#[test]
fn every_mode_reaches_the_statusline() {
    for (keys, expected) in [
        ("", " Normal"),
        ("i", " Insert"),
        ("v", " Visual"),
        ("V", " Visual Line"),
    ] {
        let mut session = session(40, 6);
        type_keys(&mut session, keys);
        assert!(
            row(&session, 4).starts_with(expected),
            "`{keys}` must show `{expected}`"
        );
    }
    // `Ctrl-V` carries a chord, so it needs its own key value.
    let mut session = session(40, 6);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('v'))), NOW);
    assert!(row(&session, 4).starts_with(" Visual Block"));
}

#[test]
fn the_number_column_shows_absolute_and_relative_numbers() {
    let mut session = with_lines(30, 10, 6);
    type_keys(&mut session, "jj");

    // The cursor line shows its absolute number, left-aligned. Every other line
    // shows its distance from the cursor line, right-aligned.
    assert_eq!(row(&session, 1), "   2 line0");
    assert_eq!(row(&session, 2), "   1 line1");
    assert_eq!(row(&session, 3), " 3   line2");
    assert_eq!(row(&session, 4), "   1 line3");
    assert_eq!(row(&session, 5), "   2 line4");
}

#[test]
fn the_cursor_line_number_carries_its_role_and_the_terminal_cursor_marks_the_cell() {
    let session = with_lines(30, 8, 3);
    // The sign column holds cell zero, so the number column starts at cell one.
    let number = style_at(&session, 1, 1);
    assert_eq!(number.fg, Some(ACCENT_WARM));
    assert!(number.add_modifier.contains(Modifier::BOLD));
    assert_eq!(
        style_at(&session, 1, 2).fg,
        Some(MUTED),
        "a line that is not the cursor line uses the muted number color"
    );
    // The terminal draws the cursor on the first text cell, and the frame
    // inverts no cell of its own.
    assert_eq!(cursor_position(&session), (5, 1));
    assert!(!is_reversed(&session, 5, 1));
    assert!(!is_reversed(&session, 6, 1));
}

#[test]
fn the_cursor_follows_the_gutter_and_the_horizontal_scroll() {
    let mut session = with_lines(30, 8, 1);
    assert_eq!(
        cursor_position(&session),
        (5, 1),
        "the gutter holds 5 cells"
    );

    // `$` moves the cursor onto the last character of `line0`.
    type_keys(&mut session, "$");
    assert_eq!(cursor_position(&session), (9, 1));

    // A long line scrolls horizontally, and the cursor stays inside the window.
    press(&mut session, 'i');
    type_keys(&mut session, &"x".repeat(60));
    let (x, y) = cursor_position(&session);
    assert_eq!(y, 1);
    assert!(
        (5..30).contains(&x),
        "the cursor stays inside the text cells of the window"
    );
}

#[test]
fn every_selection_kind_reaches_its_cells() {
    // Characterwise: `v` then `l` selects two characters of the cursor line.
    let mut characterwise = with_lines(30, 8, 3);
    type_keys(&mut characterwise, "vl");
    assert_eq!(style_at(&characterwise, 5, 1).bg, Some(SELECTION));
    assert_eq!(style_at(&characterwise, 6, 1).bg, Some(SELECTION));
    assert_ne!(style_at(&characterwise, 7, 1).bg, Some(SELECTION));

    // Characterwise across a line break: the second line is selected from its
    // first column.
    let mut across = with_lines(30, 8, 3);
    type_keys(&mut across, "vj");
    assert_eq!(style_at(&across, 9, 1).bg, Some(SELECTION));
    assert_eq!(style_at(&across, 5, 2).bg, Some(SELECTION));
    assert_ne!(style_at(&across, 6, 2).bg, Some(SELECTION));

    // Linewise: `V` selects every character of the cursor line and stops at the
    // last one. `line0` holds five characters, so cells five to nine.
    let mut linewise = with_lines(30, 8, 3);
    type_keys(&mut linewise, "V");
    assert_eq!(style_at(&linewise, 5, 1).bg, Some(SELECTION));
    assert_eq!(style_at(&linewise, 9, 1).bg, Some(SELECTION));
    assert_ne!(
        style_at(&linewise, 10, 1).bg,
        Some(SELECTION),
        "the selection paints no cell behind the last character"
    );
    assert_ne!(style_at(&linewise, 5, 2).bg, Some(SELECTION));

    // Block: `Ctrl-V`, one line down, and one column right select a rectangle
    // of two columns over two lines.
    let mut block = with_lines(30, 8, 3);
    block.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('v'))), NOW);
    type_keys(&mut block, "jl");
    assert_eq!(style_at(&block, 5, 1).bg, Some(SELECTION));
    assert_eq!(style_at(&block, 6, 1).bg, Some(SELECTION));
    assert_ne!(style_at(&block, 7, 1).bg, Some(SELECTION));
    assert_eq!(style_at(&block, 6, 2).bg, Some(SELECTION));
    assert_ne!(style_at(&block, 5, 3).bg, Some(SELECTION));
}

#[test]
fn a_search_highlights_every_match_and_marks_the_current_one() {
    let mut session = with_lines(30, 8, 3);
    type_keys(&mut session, "/line");
    press_code(&mut session, KeyCode::Enter);

    // The search moved the cursor to the match on the second line, so that
    // match becomes the current one and the others stay ordinary matches.
    assert_eq!(row(&session, 2), " 2   line1");
    assert_eq!(style_at(&session, 5, 2).bg, Some(ACCENT_WARM));
    assert_eq!(style_at(&session, 8, 2).bg, Some(ACCENT_WARM));
    assert_ne!(style_at(&session, 9, 2).bg, Some(ACCENT_WARM));
    assert_eq!(style_at(&session, 5, 1).bg, Some(SEARCH));
    assert_eq!(style_at(&session, 5, 3).bg, Some(SEARCH));
    assert_ne!(style_at(&session, 9, 1).bg, Some(SEARCH));
}

#[test]
fn esc_and_ctrl_c_both_end_the_buffer_search() {
    for end in [Key::plain(KeyCode::Esc), Key::ctrl(KeyCode::Char('c'))] {
        let mut session = with_lines(30, 8, 3);
        type_keys(&mut session, "/line");
        press_code(&mut session, KeyCode::Enter);
        assert_eq!(style_at(&session, 5, 1).bg, Some(SEARCH));
        assert_eq!(style_at(&session, 5, 2).bg, Some(ACCENT_WARM));

        session.handle_event(TerminalEvent::Key(end), NOW);

        assert_ne!(
            style_at(&session, 5, 1).bg,
            Some(SEARCH),
            "`{end:?}` ends the search and the highlight with it"
        );
        assert_ne!(style_at(&session, 5, 2).bg, Some(ACCENT_WARM));
    }
}

#[test]
fn an_edit_moves_the_search_matches_with_the_text() {
    let mut session = with_lines(30, 8, 3);
    type_keys(&mut session, "/line");
    press_code(&mut session, KeyCode::Enter);
    // Deleting the first line moves every remaining match one row up.
    type_keys(&mut session, "ggdd");
    assert_eq!(row(&session, 1), " 1   line1");
    assert_eq!(style_at(&session, 5, 1).bg, Some(ACCENT_WARM));
    assert_eq!(style_at(&session, 5, 2).bg, Some(SEARCH));
}

#[test]
fn a_long_line_scrolls_horizontally_and_clips_at_the_window_edge() {
    let mut session = session(20, 6);
    press(&mut session, 'i');
    type_keys(&mut session, "abcdefghijklmnopqrstuvwxyz");
    press_code(&mut session, KeyCode::Esc);

    // Wrapping stays disabled. The window holds fifteen text cells, so the view
    // follows the cursor and clips the rest of the line.
    assert_eq!(row(&session, 1), " 1   mnopqrstuvwxyz");
    type_keys(&mut session, "0");
    assert_eq!(row(&session, 1), " 1   abcdefghijklmno");
}

#[test]
fn a_wide_character_occupies_two_cells() {
    let mut session = session(30, 6);
    press(&mut session, 'i');
    type_keys(&mut session, "漢字x");

    let buffer = draw(&session);
    // The text starts after the five-cell gutter, and each wide character
    // advances the row by two cells.
    assert_eq!(buffer.cell((5, 1)).map(|cell| cell.symbol()), Some("漢"));
    assert_eq!(buffer.cell((7, 1)).map(|cell| cell.symbol()), Some("字"));
    assert_eq!(buffer.cell((9, 1)).map(|cell| cell.symbol()), Some("x"));
}

#[test]
fn a_tab_expands_to_the_configured_tab_stop() {
    let mut settings = EditorSettings::default();
    settings.indent.expand_tab = false;
    let mut session = Session::new(Rect::new(0, 0, 30, 6), settings, workspace_root());
    press(&mut session, 'i');
    type_keys(&mut session, "ab");
    press_code(&mut session, KeyCode::Tab);
    type_keys(&mut session, "c");

    // One hard tab after two characters reaches the tab stop at cell four, so
    // it occupies two cells instead of four.
    assert_eq!(row(&session, 1), " 1   ab  c");
}

#[test]
fn several_splits_each_render_their_own_winbar_and_focus_style() {
    let mut session = session(60, 10);
    press(&mut session, 'i');
    type_keys(&mut session, "alpha");
    press_code(&mut session, KeyCode::Esc);
    // `Ctrl-Enter` splits with the adaptive rule, which always selects a
    // vertical split while the terminal holds one window.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    assert_eq!(session.windows().window_count(), 2);

    let buffer = draw(&session);
    assert_eq!(
        row_of(&buffer, 0),
        " [Scratch] [+]                 [Scratch] [+]"
    );
    assert_eq!(
        row_of(&buffer, 1),
        " 1   alpha                     1   alpha"
    );

    // The focused window carries the title color, and the other window carries
    // the muted color. No divider glyph separates them.
    let focused = buffer
        .cell((31, 0))
        .expect("the right window shows its winbar")
        .style();
    let unfocused = buffer
        .cell((1, 0))
        .expect("the left window shows its winbar")
        .style();
    assert_eq!(focused.fg, Some(TITLE));
    assert_eq!(unfocused.fg, Some(MUTED));

    // Only the focused window holds the cursor. `Esc` left it on the last
    // character of the line, which is the fifth text cell of each window.
    assert_eq!(cursor_position(&session), (39, 1));
    assert!(
        !is_reversed(&session, 9, 1),
        "an unfocused window shows no cursor"
    );
}

#[test]
fn two_splits_paint_two_different_buffers() {
    let directory = TempDir::new("render-splits");
    let first = directory.write("first.rs", "fn first() {}\n");
    let second = directory.write("second.rs", "fn second() {}\n");
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    let mut session = Session::new(Rect::new(0, 0, 60, 8), settings, workspace_root());

    open_file(&mut session, first);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    open_file(&mut session, second);

    let buffer = draw(&session);
    assert_eq!(
        row_of(&buffer, 0),
        " first.rs                      second.rs"
    );
    assert_eq!(
        row_of(&buffer, 1),
        " 1   fn first() {}             1   fn second() {}",
        "each window paints the buffer of its own leaf"
    );
    // Only the focused window holds the cursor.
    assert_eq!(cursor_position(&session), (35, 1));
    assert!(
        !is_reversed(&session, 5, 1),
        "an unfocused window shows no cursor"
    );
}

#[test]
fn the_command_line_and_the_search_prompt_share_the_message_line() {
    let mut session = session(40, 6);
    press(&mut session, ':');
    assert_eq!(row(&session, 5), ":");
    type_keys(&mut session, "wq");
    assert_eq!(row(&session, 5), ":wq");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(row(&session, 5), "", "Esc cancels the command line");

    press(&mut session, '/');
    type_keys(&mut session, "abc");
    assert_eq!(row(&session, 5), "/abc");
}

#[test]
fn the_which_key_overlay_lists_one_level_of_next_keys() {
    let mut session = session(60, 20);
    press(&mut session, ' ');
    assert_eq!(row(&session, 5), "~", "the overlay waits for the delay");

    session.tick(WHICH_KEY_DELAY);
    let buffer = draw(&session);
    assert_eq!(row_of(&buffer, 7), " Which Key");
    assert_eq!(row_of(&buffer, 8), " /      Toggle the comment");
    assert_eq!(
        row_of(&buffer, 12),
        " f      +3 commands",
        "a key that reaches several commands shows a group marker"
    );
    assert_eq!(
        row_of(&buffer, 17),
        " Enter  Split the window with the adaptive rule"
    );
    assert_eq!(
        buffer
            .cell((1, 8))
            .expect("the overlay shows its first key")
            .style()
            .fg,
        Some(TITLE),
        "the overlay keys carry the title color"
    );

    // The overlay stays open until the user acts, and `f` opens the next level.
    press(&mut session, 'f');
    session.tick(WHICH_KEY_DELAY * 2);
    let buffer = draw(&session);
    assert_eq!(row_of(&buffer, 14), " Which Key");
    assert_eq!(row_of(&buffer, 15), " /  Open the ripgrep search picker");
    assert_eq!(row_of(&buffer, 16), " b  Open the buffer picker");
    assert_eq!(row_of(&buffer, 17), " f  Open the file search picker");

    // `Esc` dismisses the overlay from any depth.
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(row(&session, 15), "~");
}

#[test]
fn a_narrow_terminal_keeps_the_message_line_and_writes_no_cell_outside() {
    for height in 0..=3u16 {
        let mut session = session(6, height);
        press(&mut session, ':');
        let buffer = draw(&session);
        assert_eq!(buffer.area().height, height);
        if height >= 1 {
            assert_eq!(
                row_of(&buffer, height - 1),
                ":",
                "a terminal of {height} rows still shows the command line"
            );
        }
    }
    // A narrow window keeps one text cell beside the gutter.
    let session = with_lines(6, 6, 3);
    assert_eq!(row(&session, 1), " 1   l");
}

#[test]
fn a_resize_recomputes_the_layout_and_keeps_the_buffer() {
    let mut session = with_lines(40, 10, 4);
    session.handle_event(
        TerminalEvent::Resize {
            columns: 24,
            rows: 6,
        },
        NOW,
    );
    assert_eq!(session.area(), Rect::new(0, 0, 24, 6));
    assert_eq!(row(&session, 1), " 1   line0");
    assert_eq!(row(&session, 3), "   2 line2");
}

#[test]
fn rendering_the_same_session_twice_produces_the_same_frame() {
    let mut session = with_lines(40, 10, 4);
    type_keys(&mut session, "vjl");
    assert_eq!(draw(&session), draw(&session));
}

/// Returns one text row of one window, without trailing blanks.
///
/// The offset counts from the winbar row of the window, so offset one is the
/// first text row.
fn window_row(session: &Session, window: WindowId, offset: u16) -> String {
    let area = session
        .windows()
        .layout()
        .area(window)
        .expect("the window is visible");
    let buffer = draw(session);
    let mut text = String::new();
    for x in area.x..area.right() {
        if let Some(cell) = buffer.cell((x, area.y + offset)) {
            text.push_str(cell.symbol());
        }
    }
    text.trim_end().to_owned()
}

#[test]
fn each_window_counts_its_relative_numbers_from_its_own_cursor() {
    let mut session = with_lines(80, 10, 6);
    let left = session.windows().focused_window();
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let right = session.windows().focused_window();
    assert_ne!(left, right, "the split opened a second window");

    // Only the focused window moves, so the two windows hold two cursor lines.
    type_keys(&mut session, "jj");

    // The unfocused window counts from its own cursor line, which is line one.
    assert_eq!(window_row(&session, left, 1), " 1   line0");
    assert_eq!(window_row(&session, left, 2), "   1 line1");
    assert_eq!(window_row(&session, left, 3), "   2 line2");

    // The focused window counts from line three.
    assert_eq!(window_row(&session, right, 1), "   2 line0");
    assert_eq!(window_row(&session, right, 2), "   1 line1");
    assert_eq!(window_row(&session, right, 3), " 3   line2");

    // The terminal cursor marks the focused window alone.
    let area = session
        .windows()
        .layout()
        .area(right)
        .expect("the window is visible");
    assert_eq!(cursor_position(&session), (area.x + 5, area.y + 3));
}
