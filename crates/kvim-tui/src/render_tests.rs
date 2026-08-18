//! Tests for the rendered frame: buffer text, line numbers, the cursor, every
//! selection kind, search matches, chrome, overlays, and narrow terminals.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use kvim_clipboard::ClipboardFailure;
use kvim_language::{
    LanguageEvent, LanguageOutcome, LspError, ProgressPercentage, ProgressReport, ProgressStage,
    ProgressToken, SessionGeneration,
};
use kvim_settings::{EditorSettings, FileTreeIcons, NotificationSettings, WHICH_KEY_DELAY_DEFAULT};
use kvim_terminal::{Key, KeyCode, TerminalEvent};
use kvim_workspace::temp::TempDir;

use super::buffer_view::WINBAR_ROWS;
use super::clipboard::SessionClipboard;
use super::session::{MessageLevel, Redraw, Session};
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

/// The normal text color of the reference palette.
const TEXT: Color = Color::Rgb(0xc0, 0xca, 0xf5);

/// The warning color of the reference palette.
const WARNING: Color = Color::Rgb(0xe0, 0xaf, 0x68);

/// The informational color of the reference palette.
const INFO: Color = Color::Rgb(0x0d, 0xb9, 0xd7);

/// The failure color of the reference palette.
const ERROR: Color = Color::Rgb(0xdb, 0x4b, 0x4b);

/// The title color of the reference palette.
const TITLE: Color = Color::Rgb(0x7a, 0xa2, 0xf7);

/// The editor background of the reference palette.
const BASE: Color = Color::Rgb(0x11, 0x13, 0x17);

/// The first text cell of a window whose gutter holds one sign cell and a
/// three-digit number column with its gap.
const GUTTER: u16 = 5;

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

/// Returns the complete winbar row of one window, with its trailing blank.
///
/// The left text starts at the first cell of the window, and the scroll label
/// ends one cell before the right edge.
fn winbar(width: u16, left: &str, label: &str) -> String {
    let blanks = usize::from(width) - 1 - left.chars().count() - label.chars().count();
    format!("{left}{}{label} ", " ".repeat(blanks))
}

/// Returns the complete statusline row of one terminal, without its trailing
/// blank.
///
/// The mode starts at the first cell, the cursor position ends one cell before
/// the right edge, and the format-on-save state sits left of that position.
fn statusline(width: u16, mode: &str, state: &str, position: &str) -> String {
    let used = mode.chars().count() + state.chars().count() + position.chars().count() + 4;
    let blanks = usize::from(width) - used;
    format!(" {mode} {}{state} {position}", " ".repeat(blanks))
}

/// Creates a session over one terminal size.
fn session(width: u16, height: u16) -> Session {
    Session::new(
        Rect::new(0, 0, width, height),
        EditorSettings::default(),
        workspace_root(),
    )
}

/// Creates a session over one terminal size whose interface paints no icon.
///
/// The one file-tree icon setting also covers the which-key overlay, so a test
/// that reads plain row text turns every glyph off through it.
fn session_without_icons(width: u16, height: u16) -> Session {
    let mut settings = EditorSettings::default();
    settings.windows.file_tree_icons = FileTreeIcons::Hidden;
    Session::new(Rect::new(0, 0, width, height), settings, workspace_root())
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
    let _ = session.apply_file_result(request.run());
}

/// Creates a session that holds one typed text, with the cursor at the start of
/// the buffer.
fn with_text(width: u16, height: u16, text: &str) -> Session {
    let mut session = session(width, height);
    press(&mut session, 'i');
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            press_code(&mut session, KeyCode::Enter);
        }
        type_keys(&mut session, line);
    }
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "gg");
    session
}

/// Creates a session that holds `lines` numbered lines, with the cursor at the
/// start of the buffer.
fn with_lines(width: u16, height: u16, lines: usize) -> Session {
    let text: String = (0..lines).map(|index| format!("line{index}\n")).collect();
    with_text(width, height, &text)
}

/// Returns every text cell of one rendered session that marks a bracket pair.
///
/// The scan starts after the gutter, because the cursor line number carries the
/// same accent color and the same bold modifier as the pair highlight.
fn highlighted_brackets(session: &Session) -> Vec<(u16, u16)> {
    let buffer = draw(session);
    let area = *buffer.area();
    let mut found = Vec::new();
    for y in area.y..area.bottom() {
        for x in GUTTER..area.right() {
            let marked = buffer.cell((x, y)).is_some_and(|cell| {
                let style = cell.style();
                style.fg == Some(ACCENT_WARM) && style.add_modifier.contains(Modifier::BOLD)
            });
            if marked {
                found.push((x, y));
            }
        }
    }
    found
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
    assert_eq!(
        row(&session, 0),
        winbar(28, " [Scratch] [+]", "ALL").trim_end()
    );
    assert_eq!(row(&session, 1), " 1   alpha");
    assert_eq!(
        row(&session, 2),
        "~",
        "the rows below the buffer are marked"
    );
    assert_eq!(row(&session, 6), " Insert          fmt:on 1:6");
    assert_eq!(row(&session, 7), "");
}

#[test]
fn the_winbar_marks_a_modified_buffer_only_after_a_change() {
    let mut session = session(28, 6);
    assert_eq!(row(&session, 0), winbar(28, " [Scratch]", "ALL").trim_end());
    press(&mut session, 'i');
    type_keys(&mut session, "x");
    assert_eq!(
        row(&session, 0),
        winbar(28, " [Scratch] [+]", "ALL").trim_end()
    );
}

#[test]
fn the_winbar_names_an_open_file_relative_to_the_directory_that_kvim_started_in() {
    let directory = TempDir::new("render-winbar-path");
    let path = directory.file("src/main.rs", "one\ntwo\nthree\nfour\nfive\nsix\n");
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    // The session starts in the temporary directory, so the winbar strips that
    // prefix from the path of the buffer.
    let mut session = Session::new(Rect::new(0, 0, 30, 6), settings, directory.path.clone());
    open_file(&mut session, path);

    // The window holds three text rows over six buffer lines, so the view sits
    // at the top of the buffer.
    assert_eq!(
        row(&session, 0),
        winbar(30, " src/main.rs", "TOP").trim_end()
    );

    // One typed character adds the changed marker beside the path.
    press(&mut session, 'i');
    type_keys(&mut session, "x");
    assert_eq!(
        row(&session, 0),
        winbar(30, " src/main.rs [+]", "TOP").trim_end()
    );

    // The last line reaches the bottom of the buffer.
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "G");
    assert_eq!(
        row(&session, 0),
        winbar(30, " src/main.rs [+]", "BOT").trim_end()
    );
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
fn the_statusline_shows_the_format_on_save_state_of_the_focused_buffer() {
    let directory = TempDir::new("render-format-state");
    let first = directory.write("first.rs", "fn first() {}\n");
    let second = directory.write("second.rs", "fn second() {}\n");
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    let mut session = Session::new(Rect::new(0, 0, 60, 8), settings, directory.path.clone());
    open_file(&mut session, first);

    // Every new buffer follows the settings default, which enables the format.
    assert_eq!(row(&session, 6), statusline(60, "Normal", "fmt:on", "1:1"));
    // The state stays quiet beside the mode, so it carries the muted color.
    // It ends before the cursor position, which occupies the last four cells.
    assert_eq!(style_at(&session, 60 - 11, 6).fg, Some(MUTED));

    // The second window shows the second buffer, and the toggle changes that
    // buffer alone.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    open_file(&mut session, second);
    type_keys(&mut session, " cf");
    assert_eq!(row(&session, 6), statusline(60, "Normal", "fmt:off", "1:1"));

    // The state follows the focus, not the buffer list, so the left window
    // reports the state of the buffer that it shows.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('h'))), NOW);
    assert_eq!(row(&session, 6), statusline(60, "Normal", "fmt:on", "1:1"));
}

#[test]
fn a_narrow_statusline_drops_the_format_on_save_state_before_the_cursor_position() {
    // The mode label, the state, the cursor position, and one blank between
    // the mode and the state need twenty cells together.
    assert_eq!(row(&session(20, 6), 4), " Normal  fmt:on 1:1");
    assert_eq!(
        row(&session(19, 6), 4),
        " Normal        1:1",
        "the state drops first, because the position moves with every key"
    );
    assert_eq!(row(&session(12, 6), 4), " Normal 1:1");
    assert_eq!(
        row(&session(11, 6), 4),
        " Normal",
        "the mode survives longest, because it decides what the next key does"
    );
}

#[test]
fn the_message_line_marks_only_a_warning_and_a_failure() {
    let mut session = session(60, 6);

    // An ordinary report reads like buffer text.
    type_keys(&mut session, " cf");
    assert_eq!(row(&session, 5), "format-on-save is off for this buffer");
    assert_eq!(style_at(&session, 0, 5).fg, Some(TEXT));

    // The empty scratch buffer holds no match, so the search warns.
    press(&mut session, '/');
    press(&mut session, 'x');
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(row(&session, 5), "no match");
    assert_eq!(style_at(&session, 0, 5).fg, Some(WARNING));

    // The scratch buffer holds no file name, so the save fails.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert_eq!(
        row(&session, 5),
        "the buffer holds no file name; use :e <path> to name one"
    );
    assert_eq!(style_at(&session, 0, 5).fg, Some(ERROR));
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
fn the_bracket_under_the_cursor_and_its_partner_carry_the_pair_highlight() {
    // The cursor stands on the open bracket, so both ends of the pair mark it.
    let session = with_text(30, 8, "(alpha)\n");
    assert_eq!(row(&session, 1), " 1   (alpha)");
    assert_eq!(
        highlighted_brackets(&session),
        vec![(GUTTER, 1), (GUTTER + 6, 1)]
    );

    // `%` moves the cursor to the close bracket, and the pair stays marked.
    let mut jumped = with_text(30, 8, "(alpha)\n");
    press(&mut jumped, '%');
    assert_eq!(
        highlighted_brackets(&jumped),
        vec![(GUTTER, 1), (GUTTER + 6, 1)]
    );
}

#[test]
fn a_bracket_beside_the_cursor_or_without_a_partner_marks_no_pair() {
    // `%` also jumps to a pair that follows the cursor on the same line. The
    // highlight marks the bracket under the cursor alone, so this line stays
    // plain.
    let beside = with_text(30, 8, "x(alpha)\n");
    assert!(highlighted_brackets(&beside).is_empty());

    // An open bracket without a partner marks nothing.
    let unmatched = with_text(30, 8, "(alpha\n");
    assert!(highlighted_brackets(&unmatched).is_empty());

    // A close bracket without a partner marks nothing either.
    let mut closing = with_text(30, 8, "alpha)\n");
    press(&mut closing, '$');
    assert!(highlighted_brackets(&closing).is_empty());
}

#[test]
fn a_partner_outside_the_viewport_paints_no_cell() {
    // The pair spans more lines than the window shows, so only the end that the
    // viewport holds carries the highlight.
    let mut text = String::from("(\n");
    for _ in 0..7 {
        text.push_str("x\n");
    }
    text.push_str(")\n");
    let mut session = with_text(30, 8, &text);
    assert_eq!(highlighted_brackets(&session), vec![(GUTTER, 1)]);

    // `G` scrolls to the close bracket and leaves the open one above the view.
    press(&mut session, 'G');
    let marked = highlighted_brackets(&session);
    assert_eq!(marked.len(), 1, "the open bracket left the viewport");
    assert_eq!(marked[0].0, GUTTER);
    assert_eq!(row(&session, marked[0].1), " 9   )");
}

#[test]
fn a_selected_bracket_reads_as_selected_and_paints_no_pair() {
    let mut session = with_text(30, 8, "(alpha)\n");
    press(&mut session, 'v');

    // Visual mode owns the keys, so the pair steps aside and the selection band
    // is what the reader sees on the bracket.
    assert!(highlighted_brackets(&session).is_empty());
    assert_eq!(style_at(&session, GUTTER, 1).bg, Some(SELECTION));
}

#[test]
fn a_bracket_that_is_also_a_search_match_reads_as_the_match() {
    let mut session = with_text(30, 8, "(alpha)\n");
    type_keys(&mut session, "/)");
    press_code(&mut session, KeyCode::Enter);

    // The pair sits below the search match, so the searched bracket keeps the
    // colors of the current match while its partner still marks the pair.
    assert_eq!(style_at(&session, GUTTER + 6, 1).bg, Some(ACCENT_WARM));
    assert_eq!(highlighted_brackets(&session), vec![(GUTTER, 1)]);
}

#[test]
fn a_mode_other_than_normal_paints_no_bracket_pair() {
    let mut session = with_text(30, 8, "(alpha)\n");
    assert!(!highlighted_brackets(&session).is_empty());

    press(&mut session, 'i');
    assert!(highlighted_brackets(&session).is_empty());
}

#[test]
fn a_wide_character_before_a_bracket_keeps_the_pair_on_its_own_cells() {
    let mut session = with_text(30, 8, "漢(x)\n");
    // The wide character is no bracket, so the line stays plain.
    assert!(highlighted_brackets(&session).is_empty());

    // One step right reaches the open bracket, which starts after the two cells
    // of the wide character.
    press(&mut session, 'l');
    assert_eq!(
        highlighted_brackets(&session),
        vec![(GUTTER + 2, 1), (GUTTER + 4, 1)]
    );
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
    let bar = winbar(30, " [Scratch] [+]", "ALL");
    assert_eq!(row_of(&buffer, 0), format!("{bar}{bar}").trim_end());
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
    // The session starts in the directory that holds both files, so each winbar
    // names its buffer relative to that directory.
    let mut session = Session::new(Rect::new(0, 0, 60, 8), settings, directory.path.clone());

    open_file(&mut session, first);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    open_file(&mut session, second);

    let buffer = draw(&session);
    assert_eq!(
        row_of(&buffer, 0),
        format!(
            "{}{}",
            winbar(30, " first.rs", "ALL"),
            winbar(30, " second.rs", "ALL")
        )
        .trim_end()
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
    let mut session = session_without_icons(60, 20);
    press(&mut session, ' ');
    assert_eq!(row(&session, 5), "~", "the overlay waits for the delay");

    session.tick(WHICH_KEY_DELAY);
    let buffer = draw(&session);
    assert_eq!(row_of(&buffer, 10), " /      Toggle the comment");
    assert_eq!(
        row_of(&buffer, 14),
        " f      +3 commands",
        "a key that reaches several commands shows a group marker"
    );
    assert_eq!(
        buffer
            .cell((1, 10))
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
fn the_which_key_overlay_fills_a_wide_terminal_with_columns() {
    let mut session = session_without_icons(120, 30);
    type_keys(&mut session, " f");
    session.tick(WHICH_KEY_DELAY);
    let buffer = draw(&session);
    // Three columns of thirty-five cells fit, so one row holds every mapping.
    assert_eq!(row_of(&buffer, 26), " Which Key");
    assert_eq!(
        row_of(&buffer, 27),
        format!(
            " {:<35}{:<35}{}",
            "/  Open the ripgrep search picker",
            "b  Open the buffer picker",
            "f  Open the file search picker"
        )
        .trim_end()
    );
    assert_eq!(
        row_of(&buffer, 28).trim_start().split(' ').next(),
        Some("Normal"),
        "the overlay ends above the statusline"
    );
}

#[test]
fn a_narrow_terminal_keeps_the_which_key_overlay_in_one_column() {
    let mut session = session_without_icons(40, 20);
    type_keys(&mut session, " f");
    session.tick(WHICH_KEY_DELAY);
    let buffer = draw(&session);
    assert_eq!(row_of(&buffer, 14), " Which Key");
    assert_eq!(row_of(&buffer, 15), " /  Open the ripgrep search picker");
    assert_eq!(row_of(&buffer, 16), " b  Open the buffer picker");
    assert_eq!(row_of(&buffer, 17), " f  Open the file search picker");
}

#[test]
fn the_which_key_overlay_bounds_its_height_and_reports_the_dropped_rows() {
    let mut session = session_without_icons(60, 20);
    press(&mut session, ' ');
    session.tick(WHICH_KEY_DELAY);
    let buffer = draw(&session);
    // The body band holds eighteen rows, so the overlay keeps nine of them,
    // and eight of those show a mapping.
    assert_eq!(
        row_of(&buffer, 9),
        format!(" Which Key{}+2 more", " ".repeat(42)),
        "the title row reports the mappings that no column holds"
    );
    assert_eq!(row_of(&buffer, 8), "~", "the buffer stays visible above");
    assert_eq!(row_of(&buffer, 17), " q      Close the focused window");

    // A body band that cannot hold the title and one mapping over its own half
    // shows no overlay at all, so the buffer never disappears behind it.
    let mut small = session_without_icons(60, 4);
    press(&mut small, ' ');
    small.tick(WHICH_KEY_DELAY);
    let painted = draw(&small);
    assert!(
        (0..4).all(|y| !row_of(&painted, y).contains("Which Key")),
        "a body band of two rows shows no overlay at all"
    );
}

#[test]
fn the_which_key_overlay_shows_the_icon_of_the_command_group() {
    let mut session = session(80, 24);
    press(&mut session, ' ');
    session.tick(WHICH_KEY_DELAY);
    let buffer = draw(&session);
    let icon = |y: u16| {
        let cell = buffer
            .cell((1, y))
            .expect("the overlay paints an icon cell");
        (cell.symbol().to_owned(), cell.style().fg)
    };
    // `/` toggles the comment, which is a language service. `\` splits the
    // window. `f` opens the pickers, which all reach a file or a buffer.
    let (code, code_color) = icon(12);
    let (window, window_color) = icon(13);
    let (files, files_color) = icon(16);
    assert_eq!(code_color, Some(ACCENT_WARM));
    assert_eq!(window_color, Some(INFO));
    assert_eq!(files_color, Some(TEXT));
    assert_eq!(
        [code.as_str(), window.as_str(), files.as_str()]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        3,
        "each group carries its own glyph"
    );
    assert_eq!(
        row_of(&buffer, 12),
        format!(" {code} /      Toggle the comment"),
        "the icon sits left of the key and the label"
    );
}

#[test]
fn one_setting_turns_every_overlay_icon_off_and_keeps_the_columns_aligned() {
    // Every picker behind `Space f` opens a file or a buffer, so the three rows
    // carry one group icon.
    let files = "\u{f0f6}";
    let mut painted = session(120, 30);
    type_keys(&mut painted, " f");
    painted.tick(WHICH_KEY_DELAY);
    // With icons every column keeps two further cells, and the columns stay
    // evenly spaced.
    let ripgrep = format!("{files} /  Open the ripgrep search picker");
    let buffers = format!("{files} b  Open the buffer picker");
    let files_picker = format!("{files} f  Open the file search picker");
    assert_eq!(
        row_of(&draw(&painted), 27),
        format!(" {ripgrep:<37}{buffers:<37}{files_picker}")
    );

    let mut plain = session_without_icons(120, 30);
    type_keys(&mut plain, " f");
    plain.tick(WHICH_KEY_DELAY);
    assert_eq!(
        row_of(&draw(&plain), 27),
        format!(
            " {:<35}{:<35}{}",
            "/  Open the ripgrep search picker",
            "b  Open the buffer picker",
            "f  Open the file search picker"
        ),
        "every column loses the same two cells, so the columns stay aligned"
    );
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

/// The notification settings that every test session holds.
const NOTIFICATIONS: NotificationSettings = NotificationSettings {
    rows_max: 16,
    spinner_period: Duration::from_secs(1),
    done_ttl: Duration::from_secs(2),
};

/// The time between two spinner frames of those settings.
const SPINNER_FRAME: Duration = Duration::from_millis(100);

/// The server name that titles the notification group of the tests.
const SERVER: &str = "rust-analyzer";

/// Builds one progress event of one session attempt.
fn progress(generation: SessionGeneration, token: &str, stage: ProgressStage) -> LanguageEvent {
    LanguageEvent {
        adapter: "rust",
        outcome: LanguageOutcome::Progress(ProgressReport {
            generation,
            server: SERVER,
            token: ProgressToken::new(token.to_owned()).expect("the test token is short"),
            stage,
        }),
    }
}

/// Applies one progress report of one attempt at one elapsed time.
fn report_of(
    session: &mut Session,
    generation: SessionGeneration,
    now: Duration,
    token: &str,
    stage: ProgressStage,
) -> Redraw {
    session.advance_clock(now);
    session.apply_language_event(progress(generation, token, stage))
}

/// Applies one progress report of the first session attempt.
fn report(session: &mut Session, now: Duration, token: &str, stage: ProgressStage) -> Redraw {
    report_of(session, SessionGeneration::FIRST, now, token, stage)
}

/// Starts one running item with the message of the reference screenshot.
fn start_indexing(session: &mut Session, now: Duration, token: &str, message: &str) {
    report(
        session,
        now,
        token,
        ProgressStage::Begin {
            title: "Indexing".to_owned(),
            message: Some(message.to_owned()),
            percentage: None,
        },
    );
}

#[test]
fn the_notification_overlay_anchors_to_the_bottom_right_corner() {
    let mut session = session(80, 24);
    let before = cursor_position(&session);
    start_indexing(&mut session, NOW, "index", "Building compile-time-deps");

    // The body band ends above the statusline and the message line, so the
    // group title takes the last body row and the item takes the row above it.
    assert!(row(&session, 20).ends_with("In progress... Building compile-time-deps"));
    assert!(row(&session, 21).ends_with("rust-analyzer ⠋"));
    // One cell of padding keeps the text off the right edge, and the overlay
    // paints no background: every cell of it keeps the editor background.
    let buffer = draw(&session);
    assert_eq!(
        buffer.cell((78, 20)).expect("the cell is inside").symbol(),
        "s",
        "the widest row ends one cell in from the right edge"
    );
    for y in 20..=21 {
        for x in 36..80 {
            let style = buffer.cell((x, y)).expect("the cell is inside").style();
            assert_eq!(style.bg, Some(BASE), "cell ({x}, {y}) paints no panel");
        }
    }
    // The overlay is decoration, so it never moves the terminal cursor.
    assert_eq!(cursor_position(&session), before);
}

#[test]
fn the_group_title_carries_the_server_name_and_the_reported_percentage() {
    let mut session = session(80, 24);
    report(
        &mut session,
        NOW,
        "index",
        ProgressStage::Begin {
            title: "Indexing".to_owned(),
            message: None,
            percentage: ProgressPercentage::new(42),
        },
    );

    // A `begin` without a message shows the title of the operation.
    assert!(row(&session, 20).ends_with("In progress... Indexing 42%"));
    assert!(row(&session, 21).ends_with("rust-analyzer ⠋"));
    let title = style_at(&session, 79 - "rust-analyzer ⠋".chars().count() as u16, 21);
    assert_eq!(title.fg, Some(TITLE));
    assert!(title.add_modifier.contains(Modifier::BOLD));
    assert!(title.add_modifier.contains(Modifier::ITALIC));
    // The painted text carries a foreground color alone, so the cell keeps the
    // background of the row behind it.
    assert_eq!(title.bg, Some(BASE));
}

#[test]
fn the_spinner_advances_one_frame_for_each_reported_deadline() {
    let mut session = session(80, 24);
    start_indexing(&mut session, NOW, "index", "Indexing");

    // The elapsed time alone drives the animation, so the session reports the
    // moment of the next frame and the loop waits for it.
    assert_eq!(session.next_deadline(), Some(SPINNER_FRAME));
    assert_eq!(session.tick(SPINNER_FRAME), Redraw::Needed);
    assert!(row(&session, 21).ends_with("rust-analyzer ⠙"));
    // The transition leaves a later deadline behind, so the loop never repeats
    // the same catch-up step.
    assert_eq!(session.next_deadline(), Some(SPINNER_FRAME * 2));
    session.tick(SPINNER_FRAME * 2);
    assert!(row(&session, 21).ends_with("rust-analyzer ⠹"));
}

#[test]
fn a_finished_item_shows_the_done_icon_and_leaves_after_its_lifetime() {
    let mut session = session(80, 24);
    start_indexing(&mut session, NOW, "index", "Indexing");
    report(
        &mut session,
        NOW,
        "index",
        ProgressStage::End {
            message: Some("Indexed".to_owned()),
        },
    );

    assert!(row(&session, 20).ends_with("✓ Indexed"));
    // No item runs, so the group shows no spinner and only the removal remains.
    assert!(row(&session, 21).ends_with("rust-analyzer"));
    assert_eq!(session.next_deadline(), Some(NOTIFICATIONS.done_ttl));

    assert_eq!(session.tick(NOTIFICATIONS.done_ttl), Redraw::Needed);
    assert!(!row(&session, 21).contains("rust-analyzer"));
    assert_eq!(session.next_deadline(), None);
}

#[test]
fn the_overlay_drops_its_oldest_row_above_the_row_bound() {
    let mut session = session(80, 40);
    let items = NOTIFICATIONS.rows_max + 4;
    for index in 0..items {
        start_indexing(
            &mut session,
            NOW,
            &format!("token-{index}"),
            &format!("crate-{index}"),
        );
    }

    // The group keeps its own title row, so the bound leaves one row less for
    // the items.
    let body_bottom = 40 - 2 - 1;
    let title = body_bottom;
    let top = title + 1 - u16::try_from(NOTIFICATIONS.rows_max).expect("the bound is small");
    assert!(row(&session, top).ends_with(&format!("crate-{}", items - NOTIFICATIONS.rows_max + 1)));
    assert!(!row(&session, top - 1).contains("crate-"));
    assert!(row(&session, title).ends_with("rust-analyzer ⠋"));
    assert!(row(&session, title - 1).ends_with(&format!("crate-{}", items - 1)));
}

#[test]
fn the_overlay_stays_inside_a_narrow_terminal() {
    let mut session = session(20, 8);
    start_indexing(&mut session, NOW, "index", "Building compile-time-deps");

    // The panel never grows past the body band, so the row is clipped instead.
    let body_bottom = 8 - 2 - 1;
    let title = row(&session, body_bottom);
    assert!(
        title.chars().count() <= 20,
        "the row {title:?} fits the terminal"
    );
    assert!(title.contains("rust-analyzer"));
    // A clipped row still paints no background of its own.
    for x in 0..20 {
        assert_eq!(style_at(&session, x, body_bottom).bg, Some(BASE));
    }
}

#[test]
fn one_editor_message_stays_off_the_notification_overlay() {
    let mut session = session(80, 24);
    // The empty scratch buffer holds no match, so the search reports one.
    press(&mut session, '/');
    press(&mut session, 'x');
    press_code(&mut session, KeyCode::Enter);

    // The message line owns every ordinary editor report.
    let message = session
        .message()
        .expect("the search reports the missed query");
    assert_eq!(message.text(), "no match");
    assert_eq!(message.level(), MessageLevel::Warning);
    assert!(row(&session, 23).starts_with("no match"));

    // The overlay carries language server progress alone, so the message
    // reaches no second surface and leaves the board without a deadline.
    assert!(!row(&session, 20).contains("no match"));
    assert!(!row(&session, 21).contains("no match"));
    assert!(!row(&session, 21).contains("editor"));
    assert_eq!(session.next_deadline(), None);
}

#[test]
fn the_overlay_paints_no_background_over_the_buffer_text() {
    // Every text row of the body must reach under the overlay, so the test can
    // read the buffer behind it.
    let mut session = session(80, 24);
    press(&mut session, 'i');
    for index in 0..21 {
        if index > 0 {
            press_code(&mut session, KeyCode::Enter);
        }
        // The line fills every text column, so the overlay covers buffer text
        // rather than blank cells.
        type_keys(&mut session, &"-".repeat(75));
    }
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "gg");
    start_indexing(&mut session, NOW, "index", "Building compile-time-deps");

    // The item row is the widest row, so it sets the left edge of the overlay.
    // The group title is shorter, which leaves the buffer visible inside the
    // same bounding box.
    let buffer = draw(&session);
    let title_start =
        79 - u16::try_from("rust-analyzer ⠋".chars().count()).expect("the row is short");
    for x in 37..title_start {
        let cell = buffer
            .cell((x, 21))
            .expect("the test reads a cell inside the terminal");
        assert_eq!(
            cell.symbol(),
            "-",
            "column {x} of the overlay box shows the buffer text behind it"
        );
        assert_eq!(cell.style().bg, Some(BASE));
    }
    // The padding cell beside the widest row keeps the buffer as well.
    let padding = buffer
        .cell((79, 20))
        .expect("the test reads a cell inside the terminal");
    assert_eq!(padding.symbol(), "-");
}

#[test]
fn an_obsolete_session_and_an_unknown_token_never_change_the_overlay() {
    let mut session = session(80, 24);
    let restarted = SessionGeneration::FIRST.next();
    report_of(
        &mut session,
        restarted,
        NOW,
        "index",
        ProgressStage::Begin {
            title: "Indexing".to_owned(),
            message: Some("after the restart".to_owned()),
            percentage: None,
        },
    );
    let shown = row(&session, 20);

    // The attempt that failed reports its own tokens, so its report reaches no
    // item of the session that replaced it.
    assert_eq!(
        report(
            &mut session,
            NOW,
            "index",
            ProgressStage::End {
                message: Some("from the old server".to_owned()),
            },
        ),
        Redraw::Skipped
    );
    assert_eq!(row(&session, 20), shown);

    // A token that no `begin` created addresses no item at all.
    assert_eq!(
        report_of(
            &mut session,
            restarted,
            NOW,
            "unknown",
            ProgressStage::Report {
                message: Some("never opened".to_owned()),
                percentage: None,
            },
        ),
        Redraw::Skipped
    );
    assert_eq!(row(&session, 20), shown);
    assert!(!row(&session, 19).contains("never opened"));
}

#[test]
fn a_later_attempt_clears_the_rows_of_the_attempt_that_failed() {
    let mut session = session(80, 24);
    start_indexing(&mut session, NOW, "index", "before the restart");
    assert!(row(&session, 20).ends_with("before the restart"));

    // The first report of the new attempt addresses no item, because the new
    // server assigns its own tokens. It still drops every row of the attempt
    // that failed, which is a visible change and needs a frame.
    assert_eq!(
        report_of(
            &mut session,
            SessionGeneration::FIRST.next(),
            NOW,
            "index",
            ProgressStage::End {
                message: Some("late".to_owned()),
            },
        ),
        Redraw::Needed
    );
    assert!(!row(&session, 20).contains("before the restart"));
    assert!(!row(&session, 21).contains("rust-analyzer"));
}

/// Creates a session that writes no undo file, over one terminal size.
///
/// A save must reach the file alone, so the test reads the one write that it
/// asked for.
fn save_session(width: u16, height: u16) -> Session {
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    Session::new(Rect::new(0, 0, width, height), settings, workspace_root())
}

/// Refuses every queued language request, like an editor without a server.
///
/// The event loop performs the same step, so a save that waits for a formatter
/// continues instead of stalling.
fn refuse_language_requests(session: &mut Session) {
    while let Some(request) = session.take_language_request() {
        let kind = request.kind();
        let _ = session.apply_language_dispatch(kind, Err(LspError::NoServerDeclared));
    }
}

/// Saves the active buffer and publishes the completed write.
///
/// The steps are the ones that the event loop runs, and no terminal event
/// follows them, so the frame that a test draws afterwards is the frame that
/// the completed save must produce.
fn save_and_publish(session: &mut Session) -> Redraw {
    let _ = session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    refuse_language_requests(session);
    let request = session
        .take_file_request()
        .expect("the save queued one file request");
    session.apply_file_result(request.run())
}

#[test]
fn a_completed_save_paints_its_report_and_clears_the_changed_marker() {
    let directory = TempDir::new("render-save");
    let path = directory.write("main.rs", "one\ntwo\n");
    // The report names the absolute path of the temporary directory, so the
    // terminal must be wide enough to paint the complete message.
    let mut session = save_session(200, 10);
    open_file(&mut session, path.clone());

    // One typed character leaves the buffer with an unsaved change.
    press(&mut session, 'i');
    type_keys(&mut session, "// ");
    press_code(&mut session, KeyCode::Esc);
    assert!(
        row(&session, 0).contains("[+]"),
        "the winbar marks the changed buffer: {}",
        row(&session, 0)
    );

    assert_eq!(
        save_and_publish(&mut session),
        Redraw::Needed,
        "a completed save changes the marker and the message line"
    );
    assert!(
        !row(&session, 0).contains("[+]"),
        "the completed save clears the marker in the same frame: {}",
        row(&session, 0)
    );
    assert!(
        row(&session, 9).contains("written"),
        "the completed save names the write on the message line: {}",
        row(&session, 9)
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file exists"),
        "// one\ntwo\n"
    );
}

#[test]
fn a_failed_save_paints_its_failure_and_keeps_the_changed_marker() {
    let directory = TempDir::new("render-save-failure");
    let mut session = save_session(60, 10);
    // The path holds no file yet, so the open starts a new empty buffer. Its
    // directory is missing, so no write can succeed.
    open_file(&mut session, directory.join("missing").join("main.rs"));
    press(&mut session, 'i');
    type_keys(&mut session, "text");
    press_code(&mut session, KeyCode::Esc);

    assert_eq!(
        save_and_publish(&mut session),
        Redraw::Needed,
        "a failed save changes the message line"
    );
    assert!(
        row(&session, 0).contains("[+]"),
        "the failed save keeps the marker: {}",
        row(&session, 0)
    );
    assert!(
        row(&session, 9).contains("cannot save"),
        "the failed save names the failure on the message line: {}",
        row(&session, 9)
    );
}

#[test]
fn a_failed_clipboard_write_paints_its_report_without_a_key_event() {
    let mut session = save_session(90, 10).with_clipboard(SessionClipboard::deferred());
    press(&mut session, 'i');
    type_keys(&mut session, "alpha");
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "yy");
    let _ = session
        .take_clipboard_request()
        .expect("the yank queued one clipboard command");

    assert_eq!(
        session.apply_clipboard_result(Err(ClipboardFailure::Failed)),
        Redraw::Needed,
        "a failed clipboard write changes the message line"
    );
    assert!(
        row(&session, 9).contains("register still holds the value"),
        "the failed write names the state on the message line: {}",
        row(&session, 9)
    );
}
