//! Rendering of one editor window: the winbar, the gutter, and the buffer text.
//!
//! Rendering is a pure function of visible state. It performs no input, no
//! output, and no change to editor state. Every window rectangle comes from the
//! one layout calculation. See `docs/windows.md`.

use std::borrow::Cow;
use std::iter::Peekable;
use std::path::Path;
use std::str::CharIndices;

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};

use kvim_core::{CharPosition, LineIndex, TextBuffer};
use kvim_editor::{Cursor, Delimiter, DelimiterShape, Selection, matching_bracket};
use kvim_language::{Diagnostic, DiagnosticSeverity, HighlightSpan, SyntaxRole};
use kvim_settings::{DisplaySettings, SignColumn};
use kvim_ui::{BandRank, BandSegment, ChromeBand};
use kvim_workspace::ExternalChange;

use super::cells::{RowCell, layout_row, terminal_column, text_cells, truncate_cells_left};
use super::chrome::draw_band;
use super::theme::{Theme, ThemeRole};

/// The number of rows that the winbar of one window occupies.
pub(super) const WINBAR_ROWS: u16 = 1;

/// The number of cells that the sign column occupies while it is reserved.
const SIGN_COLUMN_CELLS: u16 = 1;

/// The smallest number of digits that the number column shows.
///
/// The value plus [`NUMBER_GAP_CELLS`] matches the reference Neovim
/// `numberwidth` of four cells.
const NUMBER_DIGITS_MIN: u32 = 3;

/// The number of cells between the number column and the buffer text.
const NUMBER_GAP_CELLS: u16 = 1;

/// The glyph that marks one row below the last buffer line.
const END_OF_BUFFER_GLYPH: &str = "~";

/// The marker that follows the name of a modified buffer.
const MODIFIED_MARKER: &str = " [+]";

/// The marker that follows the name of a buffer whose file changed or is gone.
///
/// The state hides the modified marker, because it is the state that the reader
/// must act on: kvim could not make the buffer current. See `docs/files.md`.
const EXTERNAL_MARKER: &str = " [!]";

/// The number of cells that the scroll position of the winbar occupies.
///
/// Every label occupies three cells, and one blank separates the label from the
/// right edge of the window, so the right edge never moves while a window
/// scrolls.
const POSITION_CELLS: u16 = 4;

/// The number of cells that the blank left of the path occupies.
const PATH_INDENT_CELLS: usize = 1;

/// The smallest path region that the winbar keeps, in cells.
///
/// The region holds the blank, the truncation marker, and four cells of the
/// file name. A winbar that cannot spare this much drops the scroll position,
/// and then the changed marker, because the path names the file.
const PATH_CELLS_MIN: usize = 6;

/// How long the path survives a narrow winbar.
///
/// The path always survives, because it names the file.
const PATH_RANK: BandRank = BandRank::new(2);

/// How long the changed marker survives a narrow winbar.
const MARKER_RANK: BandRank = BandRank::new(1);

/// How long the scroll position survives a narrow winbar.
///
/// The scroll position sheds first, because it reports where the view sits and
/// not which file it shows.
const POSITION_RANK: BandRank = BandRank::new(0);

/// Whether one window paints the bracket pair that its cursor stands on.
///
/// The pair answers a Normal-mode `%`, and the mode belongs to the focused
/// window, so every other window and every other mode paints none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BracketHighlight {
    /// The window paints the pair under its cursor.
    Shown,
    /// The window paints no pair.
    Hidden,
}

/// Whether one region holds the input focus.
///
/// A region is one editor window or one sidebar, so both surfaces name the
/// same fact with this value. The published row painter
/// [`draw_file_row`](super::file_sidebar::draw_file_row) takes it, because a
/// host owns the focus of the sidebar that it draws.
///
/// # Examples
///
/// ```
/// use kvim_tui::RegionFocus;
///
/// assert_ne!(RegionFocus::Focused, RegionFocus::Unfocused);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionFocus {
    /// The region holds the focus.
    Focused,
    /// Another region holds the focus.
    Unfocused,
}

/// Everything that one window rectangle needs to render.
pub(super) struct WindowView<'a> {
    /// The buffer that the window shows.
    pub(super) buffer: &'a TextBuffer,
    /// The short name of the buffer, which the winbar shows for a buffer that
    /// holds no file.
    pub(super) name: &'a str,
    /// The file of the buffer, or `None` for a buffer that holds no file.
    pub(super) path: Option<&'a Path>,
    /// What another program did to the file that kvim could not follow.
    ///
    /// The value is `None` while the buffer and its file agree, which every
    /// buffer that reloaded or saved does. See `docs/files.md`.
    pub(super) external: Option<ExternalChange>,
    /// The directory that kvim started in.
    ///
    /// The winbar strips this prefix from the path of the buffer, so a window
    /// names the file the way the user opened it. See `docs/windows.md`.
    pub(super) root: &'a Path,
    /// The first visible line of the window.
    pub(super) first_line: usize,
    /// The first visible source column of the window.
    pub(super) left_column: usize,
    /// The cursor of the window.
    pub(super) cursor: Cursor,
    /// The selection of the active Visual mode.
    pub(super) selection: Option<Selection>,
    /// The matches of the active search query, in ascending order.
    pub(super) matches: &'a [CharPosition],
    /// The number of characters that one match holds.
    pub(super) match_chars: usize,
    /// The highlight spans of the newest accepted analysis, in ascending order.
    ///
    /// The list is empty while no language adapter serves the buffer, and while
    /// analysis is unavailable, cancelled, or rejected. The window then renders
    /// plain text.
    pub(super) highlights: &'a [HighlightSpan],
    /// The published diagnostics of the buffer, in ascending position order.
    ///
    /// Diagnostics are decoration: they change no buffer text, no line mapping,
    /// and no cursor position. The list is empty while no language server
    /// published one. See `docs/language-services.md`.
    pub(super) diagnostics: &'a [Diagnostic],
    /// Whether the window holds the input focus.
    pub(super) focus: RegionFocus,
    /// Whether the window paints the bracket pair under its cursor.
    pub(super) brackets: BracketHighlight,
    /// The visible layout settings of the editor.
    pub(super) display: &'a DisplaySettings,
    /// The number of cells that one tab occupies.
    pub(super) tab_width: usize,
}

/// What the sign cell of one window row shows.
///
/// One row shows one sign at most. A diagnostic names a buffer line, and a row
/// after the last buffer line holds no buffer line, so the two can never
/// compete for the cell: the row decides which value applies. A row without a
/// diagnostic and with a buffer line shows no sign at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowSign {
    /// The strictest severity that marks the buffer line of the row.
    Diagnostic(DiagnosticSeverity),
    /// The row sits after the last line of the buffer.
    EndOfBuffer,
}

impl RowSign {
    /// Returns the glyph of the sign.
    ///
    /// The glyphs follow the reference configuration, which shows `E` for an
    /// error and `H` for a warning. Every glyph occupies exactly one terminal
    /// cell, so the sign column never shifts the buffer text sideways.
    const fn glyph(self) -> &'static str {
        match self {
            Self::Diagnostic(DiagnosticSeverity::Error) => "E",
            Self::Diagnostic(DiagnosticSeverity::Warning) => "H",
            Self::Diagnostic(DiagnosticSeverity::Information) => "I",
            Self::Diagnostic(DiagnosticSeverity::Hint) => "H",
            Self::EndOfBuffer => END_OF_BUFFER_GLYPH,
        }
    }

    /// Returns the theme role that colors the sign.
    const fn role(self) -> ThemeRole {
        match self {
            Self::Diagnostic(severity) => severity_role(severity),
            Self::EndOfBuffer => ThemeRole::EndOfBuffer,
        }
    }
}

/// One syntax role over inclusive source columns of one visible line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ColumnRole {
    first_column: usize,
    last_column: usize,
    role: SyntaxRole,
}

/// One diagnostic severity over inclusive source columns of one visible line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ColumnSeverity {
    first_column: usize,
    last_column: usize,
    severity: DiagnosticSeverity,
}

/// Everything that one visible line contributes to a cell style.
struct LineOverlays {
    /// The index of the line inside the buffer.
    line: usize,
    /// The number of characters of the line.
    line_len: usize,
    /// The inclusive selected columns of the line, when it holds a selection.
    selected: Option<(usize, usize)>,
    /// The syntax roles of the line, in ascending column order.
    roles: Vec<ColumnRole>,
    /// The diagnostic severities of the line, in ascending column order.
    marked: Vec<ColumnSeverity>,
}

/// One bracket of the highlighted pair, inside one visible line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BracketCell {
    line: usize,
    column: usize,
}

/// One search match inside one visible line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MatchSpan {
    line: usize,
    first_column: usize,
    last_column: usize,
    /// Whether the match holds the cursor.
    current: bool,
}

/// Returns the width of the gutter of one window, in cells.
///
/// The gutter holds the sign column and the number column, and both follow
/// [`DisplaySettings`]. The gutter never takes the complete window width, so at
/// least one text cell stays visible.
pub(super) fn gutter_cells(buffer: &TextBuffer, display: &DisplaySettings, width: u16) -> u16 {
    let numbers = if display.number || display.relative_number {
        let digits = digit_count(buffer.line_count()).max(NUMBER_DIGITS_MIN);
        u16::try_from(digits)
            .unwrap_or(u16::MAX)
            .saturating_add(NUMBER_GAP_CELLS)
    } else {
        0
    };
    sign_cells(display)
        .saturating_add(numbers)
        .min(width.saturating_sub(1))
}

/// Returns the reserved width of the sign column, in cells.
///
/// The default rule reserves the column at all times, so an arriving or a
/// leaving diagnostic never moves the buffer text sideways. The automatic rule
/// reserves nothing, because a stable text position is worth more than one
/// saved cell.
const fn sign_cells(display: &DisplaySettings) -> u16 {
    match display.signcolumn {
        SignColumn::Always => SIGN_COLUMN_CELLS,
        SignColumn::Auto | SignColumn::Never => 0,
    }
}

/// Returns the number of decimal digits of one line count.
fn digit_count(value: usize) -> u32 {
    let mut digits = 1;
    let mut rest = value / 10;
    while rest > 0 {
        digits += 1;
        rest /= 10;
    }
    digits
}

/// Renders one editor window into its rectangle.
pub(super) fn render_window(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    view: &WindowView<'_>,
) {
    if area.is_empty() {
        return;
    }
    let text = Rect {
        y: area.y.saturating_add(WINBAR_ROWS),
        height: area.height.saturating_sub(WINBAR_ROWS),
        ..area
    };
    render_winbar(target, area, theme, view, text.height);
    if text.is_empty() {
        return;
    }
    let painter = RowPainter {
        view,
        theme,
        gutter: gutter_cells(view.buffer, view.display, text.width),
        spans: match_spans(view, text.height),
        brackets: bracket_cells(view, text.height),
    };
    let mut scratch = String::new();
    for row in 0..text.height {
        let line = view.first_line.saturating_add(usize::from(row));
        let y = text.y.saturating_add(row);
        if line >= view.buffer.line_count() {
            painter.render_end_of_buffer(target, text, y);
            continue;
        }
        painter.render_gutter(target, text, y, line);
        painter.render_text(target, text, y, line, &mut scratch);
    }
}

/// Where the visible rows of one window sit inside its buffer.
///
/// kvim follows the Vim convention: the percentage reports the share of the
/// buffer above the first visible line, and the three named outcomes take
/// precedence over a number. See `docs/windows.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScrollPosition {
    /// Every line of the buffer is visible.
    All,
    /// The first line of the buffer is visible, and later lines are not.
    Top,
    /// The last line of the buffer is visible, and earlier lines are not.
    Bottom,
    /// The share of the buffer above the first visible line, in percent.
    Percent(u8),
}

impl ScrollPosition {
    /// Returns the position of one window over its buffer.
    fn measure(first_line: usize, rows: usize, lines: usize) -> Self {
        let above = first_line;
        let below = lines.saturating_sub(first_line.saturating_add(rows));
        match (above, below) {
            (0, 0) => Self::All,
            (_, 0) => Self::Bottom,
            (0, _) => Self::Top,
            (above, below) => {
                let total = above.saturating_add(below);
                let percent = above.saturating_mul(100) / total;
                debug_assert!(
                    percent < 100,
                    "one line below the view keeps the share under the whole buffer"
                );
                Self::Percent(u8::try_from(percent).unwrap_or(99))
            }
        }
    }

    /// Returns the label that the winbar shows.
    ///
    /// Every label occupies exactly three cells, so the right edge of the
    /// winbar never moves while a window scrolls.
    fn label(self) -> Cow<'static, str> {
        match self {
            Self::All => Cow::Borrowed("ALL"),
            Self::Top => Cow::Borrowed("TOP"),
            Self::Bottom => Cow::Borrowed("BOT"),
            Self::Percent(percent) => Cow::Owned(format!("{percent:>2}%")),
        }
    }
}

/// Returns the path that the winbar shows for one window.
///
/// A file inside the workspace root shows its path relative to that root, so
/// the winbar names the file the way the user opened it. A file outside the
/// root keeps its complete path, because no relative path reaches it. A buffer
/// that holds no file shows its short name.
fn window_path<'a>(view: &WindowView<'a>) -> Cow<'a, str> {
    let Some(path) = view.path else {
        return Cow::Borrowed(view.name);
    };
    path.strip_prefix(view.root)
        .unwrap_or(path)
        .to_string_lossy()
}

/// Returns the cells that the winbar leaves to the path of the buffer.
///
/// The path fills whatever the other parts leave, so it joins the shed at the
/// smallest region that still names a file, and it takes back every cell that a
/// shed part frees. A band narrower than that region gives the path every cell
/// it holds. See `docs/windows.md`.
fn path_region(band: Rect, marker: &str, position: &str) -> usize {
    let width = usize::from(band.width);
    let reserve_cells = PATH_CELLS_MIN.min(width);
    let reserved = " ".repeat(reserve_cells);
    let Ok(plan) = ChromeBand::new(vec![
        BandSegment::left(&reserved, PATH_RANK),
        BandSegment::left(marker, MARKER_RANK),
        BandSegment::right(position, POSITION_RANK),
    ]) else {
        debug_assert!(false, "the winbar lists three parts");
        return width.saturating_sub(PATH_INDENT_CELLS);
    };
    let kept: usize = plan
        .placements(band)
        .iter()
        .map(|placement| placement.segment.cells())
        .sum();
    // The reserve stands for the path itself, so every other kept cell is a
    // cell that the path cannot use.
    let taken = kept.saturating_sub(reserve_cells);
    width.saturating_sub(taken.saturating_add(PATH_INDENT_CELLS))
}

/// Renders the winbar band of one window.
///
/// The band shows, from the left, one blank, the path of the buffer, and the
/// changed marker. It shows the scroll position at the right edge. A band that
/// cannot hold every part drops the scroll position first and the changed
/// marker second, because the path names the file. The band of `kvim-ui` holds
/// that rule, so a host that ranks its own parts sheds them the same way.
fn render_winbar(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    view: &WindowView<'_>,
    rows: u16,
) {
    let band = Rect {
        height: WINBAR_ROWS,
        ..area
    };
    target.set_style(band, theme.style(ThemeRole::Winbar));
    if band.width == 0 {
        return;
    }
    let title = match view.focus {
        RegionFocus::Focused => ThemeRole::Title,
        RegionFocus::Unfocused => ThemeRole::TitleMuted,
    };
    let marker = match (view.external, view.buffer.is_modified()) {
        (Some(_), _) => EXTERNAL_MARKER,
        (None, true) => MODIFIED_MARKER,
        (None, false) => "",
    };
    let position =
        ScrollPosition::measure(view.first_line, usize::from(rows), view.buffer.line_count());
    let position = format!("{} ", position.label());
    debug_assert_eq!(
        text_cells(&position),
        usize::from(POSITION_CELLS),
        "every scroll label holds three cells and one blank"
    );
    let path = window_path(view);
    let name = format!(
        " {}",
        truncate_cells_left(&path, path_region(band, marker, &position))
    );
    // The marker follows the path directly, so both carry the title color of
    // the window and the position reads as band chrome.
    draw_band(
        target,
        band,
        theme,
        &[
            (title, BandSegment::left(&name, PATH_RANK)),
            (title, BandSegment::left(marker, MARKER_RANK)),
            (
                ThemeRole::Winbar,
                BandSegment::right(&position, POSITION_RANK),
            ),
        ],
    );
}

/// The prepared state that every row of one window shares.
struct RowPainter<'a> {
    view: &'a WindowView<'a>,
    theme: Theme,
    /// The width of the sign column and the number column together, in cells.
    gutter: u16,
    spans: Vec<MatchSpan>,
    brackets: Vec<BracketCell>,
}

impl RowPainter<'_> {
    /// Renders one row after the last buffer line.
    ///
    /// The row holds no text and no number, so the marker takes the sign cell
    /// at the left edge of the window. A window without a reserved sign column
    /// still marks that cell, because no number and no character claims it.
    fn render_end_of_buffer(&self, target: &mut CellBuffer, text: Rect, y: u16) {
        let row = Rect {
            y,
            height: 1,
            ..text
        };
        target.set_style(row, self.theme.style(ThemeRole::Text));
        target.set_stringn(
            row.x,
            y,
            RowSign::EndOfBuffer.glyph(),
            usize::from(row.width.min(SIGN_COLUMN_CELLS)),
            self.theme.style(RowSign::EndOfBuffer.role()),
        );
    }

    /// Renders the sign column and the number column of one row.
    ///
    /// The cursor line shows its absolute number. Every other line shows its
    /// distance from the cursor line. See `docs/windows.md`.
    fn render_gutter(&self, target: &mut CellBuffer, text: Rect, y: u16, line: usize) {
        if self.gutter == 0 {
            return;
        }
        let signs = self.gutter.min(sign_cells(self.view.display));
        if signs > 0 {
            let area = Rect::new(text.x, y, signs, 1);
            target.set_style(area, self.theme.style(ThemeRole::SignColumn));
            // The strictest severity of one line owns its sign, so a warning
            // never hides an error on the same line.
            if let Some(severity) = line_severity(self.view.diagnostics, line) {
                let sign = RowSign::Diagnostic(severity);
                target.set_stringn(
                    area.x,
                    y,
                    sign.glyph(),
                    usize::from(signs),
                    self.theme.style(sign.role()),
                );
            }
        }
        let numbers = self.gutter - signs;
        if numbers == 0 {
            return;
        }
        let cursor_line = self.view.cursor.line().get();
        let on_cursor_line = line == cursor_line;
        let style = if on_cursor_line {
            self.theme.style(ThemeRole::CursorLineNumber)
        } else {
            self.theme.style(ThemeRole::LineNumber)
        };
        let area = Rect::new(text.x.saturating_add(signs), y, numbers, 1);
        target.set_style(area, style);

        let display = self.view.display;
        let Some(label) = number_label(display, line, cursor_line) else {
            return;
        };
        let digits = usize::from(numbers.saturating_sub(NUMBER_GAP_CELLS));
        // Neovim left-aligns the absolute number of the cursor line while both
        // number settings are on, and right-aligns every other number.
        let x = if on_cursor_line && display.number && display.relative_number {
            area.x
        } else {
            let pad = u16::try_from(digits.saturating_sub(label.len())).unwrap_or(0);
            area.x.saturating_add(pad)
        };
        target.set_stringn(x, y, &label, digits, style);
    }

    /// Renders the text of one buffer line with every overlay applied.
    fn render_text(
        &self,
        target: &mut CellBuffer,
        text: Rect,
        y: u16,
        line: usize,
        scratch: &mut String,
    ) {
        let width = text.width.saturating_sub(self.gutter);
        if width == 0 {
            return;
        }
        let Ok(index) = self.view.buffer.line_index(line) else {
            debug_assert!(false, "the caller checked the line against the buffer");
            return;
        };
        let content = self.view.buffer.line_text(index);
        let tab_width = self.view.tab_width;
        let first_cell = terminal_column(&content, tab_width, self.view.left_column);
        let row = layout_row(&content, tab_width, first_cell, usize::from(width));
        let line_len = self.view.buffer.line_len_chars(index);
        let overlays = LineOverlays {
            line,
            line_len,
            selected: selected_columns(self.view, index, line_len),
            roles: line_roles(self.view.highlights, line, &content),
            marked: line_severities(self.view.diagnostics, line, &content),
        };
        let base = self.theme.style(ThemeRole::Text);

        for (offset, cell) in row.iter().enumerate() {
            let Ok(offset) = u16::try_from(offset) else {
                debug_assert!(false, "one row holds at most one cell for each column");
                break;
            };
            let x = text.x.saturating_add(self.gutter).saturating_add(offset);
            let Some(target_cell) = target.cell_mut((x, y)) else {
                continue;
            };
            target_cell.set_symbol(cell.symbol.as_str(scratch));
            target_cell.set_style(self.cell_style(base, &overlays, *cell));
        }
    }

    /// Returns the style of one rendered cell.
    ///
    /// Each overlay patches the style below it, so a match keeps the colors of
    /// a selection. The syntax role sits directly above the text style, so
    /// every overlay still wins over it. A diagnostic underlines the syntax
    /// color instead of replacing it, so decoration never hides the code. The
    /// bracket pair sits below the selection and below the search match, so a
    /// selected bracket still reads as selected and a matched bracket still
    /// reads as a match. No overlay marks the cursor cell: the terminal draws
    /// its own cursor there. See `docs/windows.md`.
    fn cell_style(&self, base: Style, overlays: &LineOverlays, cell: RowCell) -> Style {
        let mut style = base;
        if let Some(found) = overlays
            .roles
            .iter()
            .find(|found| cell.column >= found.first_column && cell.column <= found.last_column)
        {
            style = style.patch(self.theme.style(ThemeRole::Syntax(found.role)));
        }
        if let Some(found) = overlays
            .marked
            .iter()
            .find(|found| cell.column >= found.first_column && cell.column <= found.last_column)
        {
            style = style
                .patch(self.theme.style(severity_role(found.severity)))
                .add_modifier(Modifier::UNDERLINED);
        }
        if self
            .brackets
            .iter()
            .any(|bracket| bracket.line == overlays.line && bracket.column == cell.column)
        {
            style = style.patch(self.theme.style(ThemeRole::MatchingBracket));
        }
        if let Some((first, last)) = overlays.selected
            && cell.column >= first
            && cell.column <= last
        {
            style = style.patch(self.theme.style(ThemeRole::Selection));
        }
        if cell.column < overlays.line_len
            && let Some(span) = self.spans.iter().find(|span| {
                span.line == overlays.line
                    && cell.column >= span.first_column
                    && cell.column <= span.last_column
            })
        {
            let role = if span.current {
                ThemeRole::CurrentSearchMatch
            } else {
                ThemeRole::SearchMatch
            };
            style = style.patch(self.theme.style(role));
        }
        style
    }
}

/// Returns the terminal cell that holds the cursor of one window.
///
/// The result accounts for the winbar row, the gutter, and the horizontal
/// scroll, so the terminal draws its own cursor over the character that the
/// editor edits. Returns `None` while the cursor sits outside the visible
/// cells, which keeps the cursor inside its window rectangle at all times.
pub(super) fn cursor_cell(area: Rect, view: &WindowView<'_>) -> Option<Position> {
    let text = Rect {
        y: area.y.saturating_add(WINBAR_ROWS),
        height: area.height.saturating_sub(WINBAR_ROWS),
        ..area
    };
    if text.is_empty() {
        return None;
    }
    let gutter = gutter_cells(view.buffer, view.display, text.width);
    let width = text.width.saturating_sub(gutter);
    if width == 0 {
        return None;
    }
    let line = view.cursor.line().get();
    let row = u16::try_from(line.checked_sub(view.first_line)?).ok()?;
    if row >= text.height {
        return None;
    }
    let index = view.buffer.line_index(line).ok()?;
    let content = view.buffer.line_text(index);
    let first_cell = terminal_column(&content, view.tab_width, view.left_column).get();
    let cursor_cell = terminal_column(&content, view.tab_width, view.cursor.column().get()).get();
    let column = u16::try_from(cursor_cell.checked_sub(first_cell)?).ok()?;
    if column >= width {
        return None;
    }
    Some(Position {
        x: text.x.saturating_add(gutter).saturating_add(column),
        y: text.y.saturating_add(row),
    })
}

/// Converts ascending byte offsets of one line into source columns.
///
/// The walk moves forward only, so one pass over the line converts every
/// boundary of that line.
struct ColumnCursor<'a> {
    characters: Peekable<CharIndices<'a>>,
    column: usize,
}

impl ColumnCursor<'_> {
    /// Returns the source column of the character that holds one byte offset.
    ///
    /// An offset behind the last character returns the column after the line,
    /// so a malformed span cannot place a role inside a character.
    fn column_at(&mut self, byte: usize) -> usize {
        while self
            .characters
            .peek()
            .is_some_and(|(offset, _)| *offset < byte)
        {
            self.characters.next();
            self.column += 1;
        }
        self.column
    }
}

/// Returns the syntax roles of one visible line, in inclusive source columns.
///
/// A language adapter reports byte ranges inside a line, but the renderer
/// styles source columns. A multi-byte character makes the two differ, so the
/// conversion happens here, at the boundary that owns cells.
fn line_roles(spans: &[HighlightSpan], line: usize, content: &str) -> Vec<ColumnRole> {
    let Ok(line) = u32::try_from(line) else {
        debug_assert!(false, "a buffer holds fewer lines than u32 counts");
        return Vec::new();
    };
    let first = spans.partition_point(|span| span.line < line);
    let mut cursor = ColumnCursor {
        characters: content.char_indices().peekable(),
        column: 0,
    };
    let mut roles = Vec::new();
    for span in spans[first..]
        .iter()
        .take_while(|span| span.line == line)
        .filter(|span| span.start_byte < span.end_byte)
    {
        let first_column = cursor.column_at(span.start_byte as usize);
        let last_column = cursor.column_at(span.end_byte as usize).saturating_sub(1);
        if last_column >= first_column {
            roles.push(ColumnRole {
                first_column,
                last_column,
                role: span.role,
            });
        }
    }
    roles
}

/// Returns the theme role of one diagnostic severity.
const fn severity_role(severity: DiagnosticSeverity) -> ThemeRole {
    match severity {
        DiagnosticSeverity::Error => ThemeRole::Error,
        DiagnosticSeverity::Warning => ThemeRole::Warning,
        DiagnosticSeverity::Information => ThemeRole::Info,
        DiagnosticSeverity::Hint => ThemeRole::Hint,
    }
}

/// Returns the strictest severity that marks one line, if any.
fn line_severity(diagnostics: &[Diagnostic], line: usize) -> Option<DiagnosticSeverity> {
    let line = u32::try_from(line).ok()?;
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.span.start.line <= line && line <= diagnostic.span.end.line)
        .map(|diagnostic| diagnostic.severity)
        .min()
}

/// Returns the diagnostic severities of one visible line, in source columns.
///
/// A language server reports byte columns, and the renderer styles source
/// columns, so the conversion happens here, at the boundary that owns cells. A
/// diagnostic that spans several lines marks the complete text of every line
/// between its ends.
fn line_severities(diagnostics: &[Diagnostic], line: usize, content: &str) -> Vec<ColumnSeverity> {
    let Ok(line) = u32::try_from(line) else {
        debug_assert!(false, "a buffer holds fewer lines than u32 counts");
        return Vec::new();
    };
    let mut marked = Vec::new();
    for diagnostic in diagnostics {
        let span = diagnostic.span;
        if line < span.start.line || line > span.end.line {
            continue;
        }
        let start_byte = if span.start.line == line {
            usize::try_from(span.start.byte_column).unwrap_or(usize::MAX)
        } else {
            0
        };
        let end_byte = if span.end.line == line {
            usize::try_from(span.end.byte_column).unwrap_or(usize::MAX)
        } else {
            content.len()
        };
        let first_column = source_column(content, start_byte);
        // An empty range marks the character that it points at, so a marker
        // without width stays visible.
        let last_byte = end_byte.max(start_byte.saturating_add(1));
        let last_column = source_column(content, last_byte).saturating_sub(1);
        if last_column >= first_column {
            marked.push(ColumnSeverity {
                first_column,
                last_column,
                severity: diagnostic.severity,
            });
        }
    }
    // The cell style takes the first entry that covers its column, so the
    // strictest severity must come first and win over a wider weaker range.
    marked.sort_by_key(|entry| (entry.severity, entry.first_column));
    marked
}

/// Returns the source column of the character that holds one byte offset.
///
/// An offset behind the last character returns the column after the line, so a
/// malformed span cannot place decoration inside a character.
fn source_column(content: &str, byte: usize) -> usize {
    content
        .char_indices()
        .take_while(|(offset, _)| *offset < byte)
        .count()
}

/// Returns the number that one row shows, or `None` while both settings are off.
fn number_label(display: &DisplaySettings, line: usize, cursor_line: usize) -> Option<String> {
    match (display.number, display.relative_number) {
        (false, false) => None,
        (true, false) => Some((line + 1).to_string()),
        (true, true) if line == cursor_line => Some((line + 1).to_string()),
        (_, true) => Some(line.abs_diff(cursor_line).to_string()),
    }
}

/// Returns the inclusive source columns that the selection covers on one line.
///
/// Every result ends at the last character of the line at the latest, so the
/// selection never paints a cell behind the text. A line without a selected
/// character returns `None`. See `docs/windows.md`.
fn selected_columns(
    view: &WindowView<'_>,
    line: LineIndex,
    line_len: usize,
) -> Option<(usize, usize)> {
    let selection = view.selection?;
    let buffer = view.buffer;
    // An empty line holds no character, so no shape selects a cell on it.
    let last_character = line_len.checked_sub(1)?;
    let (first_column, last_column) = match selection {
        Selection::Characterwise(range) => {
            let first_line = buffer.char_to_line(range.start());
            // The range ends after the last selected character, so the last
            // selected position is one character back.
            let last = buffer
                .char_position(range.end().get().saturating_sub(1))
                .ok()?;
            let last_line = buffer.char_to_line(last);
            if line < first_line || line > last_line {
                return None;
            }
            let first_column = if line == first_line {
                buffer.char_to_column(range.start()).get()
            } else {
                0
            };
            let last_column = if line == last_line {
                buffer.char_to_column(last).get()
            } else {
                last_character
            };
            (first_column, last_column)
        }
        Selection::Linewise { first, last } => {
            if line < first || line > last {
                return None;
            }
            (0, last_character)
        }
        Selection::Block {
            first_line,
            last_line,
            left,
            right,
        } => {
            if line < first_line || line > last_line {
                return None;
            }
            (left.get(), right.get())
        }
    };
    let last_column = last_column.min(last_character);
    (first_column <= last_column).then_some((first_column, last_column))
}

/// Collects the search matches that fall inside the visible lines.
///
/// The scan reads the bounded match list once for each window, so its cost
/// stays independent of the buffer length.
fn match_spans(view: &WindowView<'_>, rows: u16) -> Vec<MatchSpan> {
    if view.match_chars == 0 || view.matches.is_empty() {
        return Vec::new();
    }
    let last_line = view.first_line.saturating_add(usize::from(rows));
    let cursor = view.cursor.position(view.buffer).get();
    let mut spans = Vec::new();
    for &position in view.matches {
        let line = view.buffer.char_to_line(position).get();
        if line < view.first_line || line >= last_line {
            continue;
        }
        let first_column = view.buffer.char_to_column(position).get();
        let end = position.get() + view.match_chars;
        spans.push(MatchSpan {
            line,
            first_column,
            last_column: first_column + view.match_chars - 1,
            current: cursor >= position.get() && cursor < end,
        });
    }
    spans
}

/// Returns whether one character opens or closes a bracket pair.
///
/// `Delimiter::MATCH_PAIRS` decides, which is the same table that the `%`
/// motion reads, so the highlight covers exactly the characters that a jump
/// pairs. See `docs/input-actions.md`.
fn is_match_pair(character: char) -> bool {
    Delimiter::MATCH_PAIRS.iter().any(|delimiter| {
        matches!(
            delimiter.shape(),
            DelimiterShape::Balanced { open, close } if character == open || character == close
        )
    })
}

/// Collects the visible cells of the bracket pair under the cursor.
///
/// The partner comes from [`matching_bracket`], the one search that the `%`
/// motion uses, so the highlight always marks the bracket that a jump reaches.
/// That search also answers for a bracket after the cursor, because `%` reads
/// its line forward. The highlight marks the bracket under the cursor alone, so
/// the character at the cursor decides whether a pair exists at all.
///
/// A bracket outside the visible lines contributes no cell, so a partner far
/// from the viewport costs no paint work.
fn bracket_cells(view: &WindowView<'_>, rows: u16) -> Vec<BracketCell> {
    if view.brackets == BracketHighlight::Hidden {
        return Vec::new();
    }
    let buffer = view.buffer;
    let cursor = view.cursor.position(buffer);
    let line = buffer.char_to_line(cursor);
    let column = buffer.char_to_column(cursor).get();
    if !buffer
        .line_text(line)
        .chars()
        .nth(column)
        .is_some_and(is_match_pair)
    {
        return Vec::new();
    }
    let Some(matched) = matching_bracket(buffer, cursor) else {
        return Vec::new();
    };
    let last_line = view.first_line.saturating_add(usize::from(rows));
    [cursor, matched]
        .into_iter()
        .filter_map(|position| {
            let line = buffer.char_to_line(position).get();
            (line >= view.first_line && line < last_line).then(|| BracketCell {
                line,
                column: buffer.char_to_column(position).get(),
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "buffer_view_tests.rs"]
mod tests;
