use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use kvim_core::TextBuffer;
use kvim_editor::{Cursor, Selection};
use kvim_language::{
    Diagnostic, DiagnosticSeverity, DocumentPosition, HighlightSpan, SourceSpan, SyntaxRole,
};
use kvim_settings::EditorSettings;
use kvim_workspace::ExternalChange;

use super::super::theme::{Theme, ThemeRole};
use super::{
    BracketHighlight, END_OF_BUFFER_GLYPH, RegionFocus, WindowView, render_window, scrollbar_thumb,
    text_surface_geometry,
};

/// The window rectangle of every test, including the winbar row.
const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 40,
    height: 4,
};

/// Renders one buffer with highlight spans and returns the cell buffer.
fn draw(text: &str, highlights: &[HighlightSpan]) -> CellBuffer {
    let buffer = TextBuffer::from_text(text, kvim_core::BufferBytesMax::default())
        .expect("the test text is small");
    let settings = EditorSettings::default();
    let view = WindowView {
        buffer: &buffer,
        name: "test.rs",
        path: None,
        external: None,
        root: Path::new("/workspace"),
        first_line: 0,
        left_column: 0,
        cursor: Cursor::ORIGIN,
        selection: None,
        matches: &[],
        match_chars: 0,
        highlights,
        diagnostics: &[],
        focus: RegionFocus::Unfocused,
        brackets: BracketHighlight::Hidden,
        display: &settings.display,
        tab_width: usize::from(settings.indent.tab_width.get()),
    };
    let mut target = CellBuffer::empty(AREA);
    render_window(&mut target, AREA, Theme::new(), &view);
    target
}

/// The workspace root of every winbar test.
const ROOT: &str = "/workspace";

/// Returns one rendered row as text, without the trailing blanks.
///
/// A wide character owns two cells, and the cell buffer fills the second one
/// with a blank, so the scan skips it. The result then reads as the terminal
/// shows the row.
fn row_of(target: &CellBuffer, y: u16) -> String {
    let area = *target.area();
    let mut text = String::new();
    let mut tail = 0;
    for x in area.x..area.right() {
        let Some(cell) = target.cell((x, y)) else {
            continue;
        };
        if tail > 0 {
            tail -= 1;
            continue;
        }
        tail = super::text_cells(cell.symbol()).saturating_sub(1);
        text.push_str(cell.symbol());
    }
    text.trim_end().to_owned()
}

/// Renders one window over `lines` numbered lines and returns its winbar.
///
/// The window shows the rows of `area` below the winbar row, so the caller
/// selects the scroll position through `lines` and `first_line`.
fn winbar(path: Option<&Path>, lines: usize, first_line: usize, area: Rect) -> String {
    winbar_of(path, None, lines, first_line, area)
}

/// Renders one window whose file may have changed outside the editor.
fn winbar_of(
    path: Option<&Path>,
    external: Option<ExternalChange>,
    lines: usize,
    first_line: usize,
    area: Rect,
) -> String {
    let text: String = (0..lines).map(|index| format!("line{index}\n")).collect();
    let buffer = TextBuffer::from_text(&text, kvim_core::BufferBytesMax::default())
        .expect("the test text is small");
    let settings = EditorSettings::default();
    let view = WindowView {
        buffer: &buffer,
        name: "[Scratch]",
        path,
        external,
        root: Path::new(ROOT),
        first_line,
        left_column: 0,
        cursor: Cursor::ORIGIN,
        selection: None,
        matches: &[],
        match_chars: 0,
        highlights: &[],
        diagnostics: &[],
        focus: RegionFocus::Focused,
        brackets: BracketHighlight::Hidden,
        display: &settings.display,
        tab_width: usize::from(settings.indent.tab_width.get()),
    };
    let mut target = CellBuffer::empty(area);
    render_window(&mut target, area, Theme::new(), &view);
    row_of(&target, area.y)
}

/// Returns the winbar row that one left text and one scroll label produce.
///
/// The label ends one cell before the right edge of the window, and blanks
/// fill the cells between the two parts.
fn expected_row(width: u16, left: &str, label: &str) -> String {
    let blanks = usize::from(width) - 1 - super::text_cells(left) - super::text_cells(label);
    format!("{left}{}{label}", " ".repeat(blanks))
}

/// Returns one absolute path inside the workspace root.
fn inside(relative: &str) -> PathBuf {
    Path::new(ROOT).join(relative)
}

#[test]
fn scrollbar_thumb_covers_only_long_buffers() {
    assert_eq!(scrollbar_thumb(4, 0, 0), None);
    assert_eq!(scrollbar_thumb(4, 1, 0), None);
    assert_eq!(scrollbar_thumb(4, 4, 0), None);
    assert_eq!(scrollbar_thumb(4, 8, 0), Some((0, 2)));
    assert_eq!(scrollbar_thumb(4, 8, 2), Some((1, 2)));
    assert_eq!(scrollbar_thumb(4, 8, 4), Some((2, 2)));
    assert_eq!(scrollbar_thumb(0, 8, 0), None);
}

#[test]
fn text_geometry_reserves_only_a_usable_enabled_scrollbar() {
    let buffer = TextBuffer::from_text("abc\n", kvim_core::BufferBytesMax::default()).unwrap();
    let mut settings = EditorSettings::default();
    let enabled = text_surface_geometry(Rect::new(0, 0, 8, 3), &buffer, &settings.display);
    assert_eq!(enabled.content.width, 7);
    assert_eq!(enabled.scrollbar_x, Some(7));

    settings.display.scrollbar = false;
    let disabled = text_surface_geometry(Rect::new(0, 0, 8, 3), &buffer, &settings.display);
    assert_eq!(disabled.content.width, 8);
    assert_eq!(disabled.scrollbar_x, None);

    settings.display.scrollbar = true;
    let narrow = text_surface_geometry(Rect::new(0, 0, 1, 3), &buffer, &settings.display);
    assert_eq!(narrow.content.width, 1);
    assert_eq!(narrow.scrollbar_x, None);
}

#[test]
fn scrollbar_renders_only_the_track_when_the_buffer_fully_fits() {
    let buffer =
        TextBuffer::from_text("zero\none\ntwo\n", kvim_core::BufferBytesMax::default()).unwrap();
    let settings = EditorSettings::default();
    let view = WindowView {
        buffer: &buffer,
        name: "test.rs",
        path: None,
        external: None,
        root: Path::new("/workspace"),
        first_line: 0,
        left_column: 0,
        cursor: Cursor::ORIGIN,
        selection: None,
        matches: &[],
        match_chars: 0,
        highlights: &[],
        diagnostics: &[],
        focus: RegionFocus::Unfocused,
        brackets: BracketHighlight::Hidden,
        display: &settings.display,
        tab_width: 4,
    };
    let area = Rect::new(0, 0, 10, 5);
    let mut target = CellBuffer::empty(area);
    render_window(&mut target, area, Theme::new(), &view);

    for y in 1..5 {
        assert_eq!(target[(9, y)].symbol(), "│");
    }
}

#[test]
fn scrollbar_renders_track_and_overflow_thumb_without_changing_the_winbar_width() {
    let text = (0..8).map(|line| format!("{line}\n")).collect::<String>();
    let buffer = TextBuffer::from_text(&text, kvim_core::BufferBytesMax::default()).unwrap();
    let settings = EditorSettings::default();
    let view = WindowView {
        buffer: &buffer,
        name: "test.rs",
        path: None,
        external: None,
        root: Path::new("/workspace"),
        first_line: 2,
        left_column: 0,
        cursor: Cursor::ORIGIN,
        selection: None,
        matches: &[],
        match_chars: 0,
        highlights: &[],
        diagnostics: &[],
        focus: RegionFocus::Unfocused,
        brackets: BracketHighlight::Hidden,
        display: &settings.display,
        tab_width: 4,
    };
    let area = Rect::new(0, 0, 10, 5);
    let mut target = CellBuffer::empty(area);
    render_window(&mut target, area, Theme::new(), &view);

    assert_eq!(
        target[(9, 0)].symbol(),
        " ",
        "the winbar keeps its full width"
    );
    assert_eq!(target[(9, 1)].symbol(), "│");
    assert_eq!(target[(9, 2)].symbol(), "┃");
    assert_eq!(target[(9, 3)].symbol(), "┃");
    assert_eq!(target[(9, 4)].symbol(), "│");
}

#[test]
fn the_winbar_shows_the_path_relative_to_the_workspace_root() {
    let path = inside("src/main.rs");
    assert_eq!(
        winbar(Some(&path), 2, 0, AREA),
        expected_row(AREA.width, " src/main.rs", "ALL")
    );
}

#[test]
fn a_buffer_without_a_file_shows_its_short_name() {
    assert_eq!(
        winbar(None, 2, 0, AREA),
        expected_row(AREA.width, " [Scratch]", "ALL")
    );
}

#[test]
fn a_file_outside_the_workspace_root_keeps_its_complete_path() {
    // No relative path reaches the file, so the winbar names it in full.
    let path = Path::new("/other/notes.md");
    assert_eq!(
        winbar(Some(path), 2, 0, AREA),
        expected_row(AREA.width, " /other/notes.md", "ALL")
    );
}

#[test]
fn a_path_of_exactly_the_available_width_keeps_every_character() {
    // The winbar keeps one blank left of the path and four cells for the
    // scroll position, so the path region holds 35 cells here.
    let exact = "a".repeat(35);
    assert_eq!(
        winbar(Some(&inside(&exact)), 2, 0, AREA),
        expected_row(AREA.width, &format!(" {exact}"), "ALL")
    );
    // One cell more cuts the start and spends one cell on the marker.
    let long = "a".repeat(36);
    assert_eq!(
        winbar(Some(&inside(&long)), 2, 0, AREA),
        expected_row(AREA.width, &format!(" <{}", "a".repeat(34)), "ALL")
    );
}

#[test]
fn a_long_path_loses_its_start_and_keeps_the_file_name() {
    let path = inside("deep/nested/directory/tree/my-folder/file.md");
    assert_eq!(
        winbar(Some(&path), 2, 0, AREA),
        expected_row(AREA.width, " <d/directory/tree/my-folder/file.md", "ALL")
    );
}

#[test]
fn the_truncated_path_never_splits_a_wide_character() {
    // Eighteen wide characters need 36 cells, and the path region holds 35,
    // so the marker replaces the first character.
    let wide = "漢".repeat(18);
    assert_eq!(
        winbar(Some(&inside(&wide)), 2, 0, AREA),
        expected_row(AREA.width, &format!(" <{}", "漢".repeat(17)), "ALL")
    );
    // A path region of 36 cells still shows 17 wide characters, because the
    // eighteenth would overflow the region by one cell.
    let area = Rect {
        width: AREA.width + 1,
        ..AREA
    };
    assert_eq!(
        winbar(Some(&inside(&"漢".repeat(19))), 2, 0, area),
        expected_row(area.width, &format!(" <{}", "漢".repeat(17)), "ALL")
    );
}

#[test]
fn the_winbar_reports_where_the_view_sits_in_the_buffer() {
    // The rectangle holds one winbar row and three text rows.
    let path = inside("main.rs");
    let cases = [
        (3, 0, "ALL"),
        (10, 0, "TOP"),
        (10, 7, "BOT"),
        (10, 3, "42%"),
        (100, 1, " 1%"),
    ];
    for (lines, first_line, label) in cases {
        assert_eq!(
            winbar(Some(&path), lines, first_line, AREA),
            expected_row(AREA.width, " main.rs", label),
            "{lines} lines from line {first_line} show `{label}`"
        );
    }
}

#[test]
fn a_narrow_winbar_drops_the_scroll_position_before_the_path() {
    let path = inside("src/main.rs");
    let narrow = |width: u16| winbar(Some(&path), 2, 0, Rect { width, ..AREA });
    // Ten cells still hold the position and the smallest path region.
    assert_eq!(narrow(10), expected_row(10, " <n.rs", "ALL"));
    // Nine cells give every cell to the path.
    assert_eq!(narrow(9), " <main.rs");
    assert_eq!(narrow(3), " <s");
    // One cell holds the blank alone, so the row carries no text.
    assert_eq!(narrow(1), "");
}

#[test]
fn a_narrow_winbar_drops_the_scroll_position_before_the_changed_marker() {
    // The file changed outside the editor, so the winbar shows the marker that
    // the reader must act on. See `docs/files.md`.
    let path = inside("a.rs");
    let narrow = |width: u16| {
        winbar_of(
            Some(&path),
            Some(ExternalChange::Changed),
            2,
            0,
            Rect { width, ..AREA },
        )
    };
    // Thirteen cells hold the path, the marker, and the scroll position.
    assert_eq!(narrow(13), expected_row(13, " a.rs [!]", "ALL"));
    assert_eq!(
        narrow(12),
        " a.rs [!]",
        "the scroll position sheds before the marker"
    );
    assert_eq!(narrow(9), " a.rs [!]");
    assert_eq!(
        narrow(8),
        " a.rs",
        "the marker sheds second, because the path names the file"
    );
}

/// Returns the foreground color of one cell of the first text row.
fn foreground(target: &CellBuffer, x: u16) -> Option<Color> {
    target
        .cell((x, super::WINBAR_ROWS))
        .and_then(|cell| cell.fg.into())
}

fn syntax_color(role: SyntaxRole) -> Option<Color> {
    Theme::new().style(ThemeRole::Syntax(role)).fg
}

/// Renders one buffer with diagnostics into a window of a chosen width.
fn draw_marked(text: &str, diagnostics: &[Diagnostic], width: u16) -> CellBuffer {
    let buffer = TextBuffer::from_text(text, kvim_core::BufferBytesMax::default())
        .expect("the test text is small");
    let settings = EditorSettings::default();
    let view = WindowView {
        buffer: &buffer,
        name: "test.rs",
        path: None,
        external: None,
        root: Path::new("/workspace"),
        first_line: 0,
        left_column: 0,
        cursor: Cursor::ORIGIN,
        selection: None,
        matches: &[],
        match_chars: 0,
        highlights: &[],
        diagnostics,
        focus: RegionFocus::Unfocused,
        brackets: BracketHighlight::Hidden,
        display: &settings.display,
        tab_width: usize::from(settings.indent.tab_width.get()),
    };
    let area = Rect { width, ..AREA };
    let mut target = CellBuffer::empty(area);
    render_window(&mut target, area, Theme::new(), &view);
    target
}

/// Returns one diagnostic that marks the first character of one line.
fn diagnostic(line: u32, severity: DiagnosticSeverity) -> Diagnostic {
    Diagnostic {
        span: SourceSpan::new(
            DocumentPosition::new(line, 0),
            DocumentPosition::new(line, 1),
        ),
        severity,
        message: "the test message".to_owned(),
        source: "test-server".to_owned(),
    }
}

/// Returns the symbol and the style of one cell.
fn cell_at(target: &CellBuffer, x: u16, y: u16) -> (String, Style) {
    let cell = target
        .cell((x, y))
        .expect("the test reads a cell inside the window");
    (cell.symbol().to_owned(), cell.style())
}

#[test]
fn a_row_after_the_last_line_marks_the_sign_column_in_a_readable_color() {
    // The line ending terminates the one line of the buffer, so every row
    // behind the first text row marks the end of the buffer. The marker
    // sits left of the number column.
    let target = draw_marked("only\n", &[], AREA.width);
    let theme = Theme::new();
    let marker = theme.style(ThemeRole::EndOfBuffer);

    for y in super::WINBAR_ROWS + 1..AREA.height {
        let (symbol, style) = cell_at(&target, AREA.x, y);
        assert_eq!(symbol, END_OF_BUFFER_GLYPH, "row {y} marks absent text");
        assert_eq!(style.fg, marker.fg, "row {y} keeps the marker color");
        assert_ne!(style.fg, style.bg, "row {y} keeps the marker readable");
    }
    // The number column starts after the marker and stays empty.
    assert_eq!(
        cell_at(&target, AREA.x + 1, AREA.height - 1).0,
        " ",
        "no number follows the marker"
    );
}

#[test]
fn a_warning_and_an_error_take_the_sign_column_in_their_own_colors() {
    let theme = Theme::new();
    let cases = [
        (DiagnosticSeverity::Warning, "H", ThemeRole::Warning),
        (DiagnosticSeverity::Error, "E", ThemeRole::Error),
    ];
    for (severity, glyph, role) in cases {
        let target = draw_marked("one\ntwo\n", &[diagnostic(1, severity)], AREA.width);
        // The sign marks the second buffer line, which is the second text
        // row of the window.
        let (symbol, style) = cell_at(&target, AREA.x, super::WINBAR_ROWS + 1);
        assert_eq!(symbol, glyph, "{severity:?} owns its glyph");
        assert_eq!(
            style.fg,
            theme.style(role).fg,
            "{severity:?} owns its color"
        );
        // The unmarked line keeps its sign cell empty.
        assert_eq!(cell_at(&target, AREA.x, super::WINBAR_ROWS).0, " ");
    }
}

#[test]
fn a_row_after_the_last_line_shows_the_marker_and_never_a_diagnostic_sign() {
    // The server names a range that reaches past the last buffer line. The
    // row holds no buffer line, so the marker wins the sign cell.
    let span = SourceSpan::new(DocumentPosition::new(0, 0), DocumentPosition::new(40, 0));
    let stale = Diagnostic {
        span,
        severity: DiagnosticSeverity::Error,
        message: "the test message".to_owned(),
        source: "test-server".to_owned(),
    };
    let target = draw_marked("one\n", &[stale], AREA.width);

    // The first text row holds the marked line and shows the error sign.
    assert_eq!(cell_at(&target, AREA.x, super::WINBAR_ROWS).0, "E");
    for y in super::WINBAR_ROWS + 1..AREA.height {
        assert_eq!(
            cell_at(&target, AREA.x, y).0,
            END_OF_BUFFER_GLYPH,
            "row {y} holds no buffer line, so it shows the marker"
        );
    }
}

#[test]
fn a_narrow_window_keeps_the_marker_and_one_text_cell() {
    // The gutter never takes the complete width, so one text cell survives.
    let narrow = 6;
    let target = draw_marked("one\n", &[], narrow);

    assert_eq!(
        cell_at(&target, AREA.x, AREA.height - 1).0,
        END_OF_BUFFER_GLYPH,
        "the narrow window still marks absent text"
    );
    let gutter = super::gutter_cells(
        &TextBuffer::from_text("one\n", kvim_core::BufferBytesMax::default()).unwrap(),
        &EditorSettings::default().display,
        narrow,
    );
    assert!(gutter < narrow, "one text cell stays visible");
}

#[test]
fn a_highlight_span_styles_the_columns_of_its_role() {
    let span = HighlightSpan {
        line: 0,
        start_byte: 0,
        end_byte: 3,
        role: SyntaxRole::Keyword,
    };
    let target = draw("let value = 1;\n", &[span]);
    let gutter = super::gutter_cells(
        &TextBuffer::from_text("let value = 1;\n", kvim_core::BufferBytesMax::default()).unwrap(),
        &EditorSettings::default().display,
        AREA.width,
    );

    for offset in 0..3 {
        assert_eq!(
            foreground(&target, gutter + offset),
            syntax_color(SyntaxRole::Keyword),
            "column {offset} carries the keyword role"
        );
    }
    assert_eq!(
        foreground(&target, gutter + 3),
        Theme::new().style(ThemeRole::Text).fg,
        "the span ends at its last column"
    );
}

#[test]
fn a_span_over_multibyte_and_wide_characters_keeps_its_cells() {
    // The string literal starts at byte 4 and holds three wide characters,
    // so the span covers two cells for each of them.
    let text = "let s = \"日本語\";\n";
    let start = text.find('"').expect("the test text holds a string") as u32;
    let end = text.rfind('"').expect("the test text holds a string") as u32 + 1;
    let span = HighlightSpan {
        line: 0,
        start_byte: start,
        end_byte: end,
        role: SyntaxRole::String,
    };
    let target = draw(text, &[span]);
    let buffer = TextBuffer::from_text(text, kvim_core::BufferBytesMax::default()).unwrap();
    let gutter = super::gutter_cells(&buffer, &EditorSettings::default().display, AREA.width);

    // The quote and the three wide characters occupy 1 + 6 + 1 cells.
    for offset in 8..16 {
        assert_eq!(
            foreground(&target, gutter + offset),
            syntax_color(SyntaxRole::String),
            "cell {offset} belongs to the string span"
        );
    }
    assert_eq!(
        foreground(&target, gutter + 16),
        Theme::new().style(ThemeRole::Text).fg,
        "the semicolon stays plain text"
    );
}

#[test]
fn a_selection_ends_at_the_last_character_of_every_line() {
    // `alpha` holds five characters, the second line holds none, and `beta`
    // holds four.
    let text = "alpha\n\nbeta\n";
    let buffer = TextBuffer::from_text(text, kvim_core::BufferBytesMax::default())
        .expect("the test text is small");
    let settings = EditorSettings::default();
    let line = |index: usize| buffer.line_index(index).expect("the test line exists");
    let column = |index: usize| {
        buffer
            .source_column(line(0), index)
            .expect("the column exists")
    };
    let view = |selection| WindowView {
        buffer: &buffer,
        name: "test.rs",
        path: None,
        external: None,
        root: Path::new("/workspace"),
        first_line: 0,
        left_column: 0,
        cursor: Cursor::ORIGIN,
        selection: Some(selection),
        matches: &[],
        match_chars: 0,
        highlights: &[],
        diagnostics: &[],
        focus: RegionFocus::Unfocused,
        brackets: BracketHighlight::Hidden,
        display: &settings.display,
        tab_width: usize::from(settings.indent.tab_width.get()),
    };

    let linewise = Selection::Linewise {
        first: line(0),
        last: line(2),
    };
    let cases = [(0, 5, Some((0, 4))), (1, 0, None), (2, 4, Some((0, 3)))];
    for (index, len, expected) in cases {
        assert_eq!(
            super::selected_columns(&view(linewise), line(index), len),
            expected,
            "the linewise selection of line {index} covers only its characters"
        );
    }

    // A block rectangle stops at the last character of a shorter line.
    let block = Selection::Block {
        first_line: line(0),
        last_line: line(2),
        left: column(1),
        right: column(4),
    };
    let cases = [(0, 5, Some((1, 4))), (1, 0, None), (2, 4, Some((1, 3)))];
    for (index, len, expected) in cases {
        assert_eq!(
            super::selected_columns(&view(block), line(index), len),
            expected,
            "the block selection of line {index} stops at its last character"
        );
    }
}

#[test]
fn an_empty_span_list_renders_plain_text() {
    let target = draw("let value = 1;\n", &[]);
    let plain = Theme::new().style(ThemeRole::Text).fg;

    // The last column of the surface is the reserved scrollbar column, which
    // carries the color of its own track and no text color.
    for x in 0..AREA.width - 1 {
        let color = foreground(&target, x);
        assert!(
            color == plain || color.is_none() || x < 5,
            "column {x} keeps the plain text color"
        );
    }
}
