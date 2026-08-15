//! Rendering of one editor window: the winbar, the gutter, and the buffer text.
//!
//! Rendering is a pure function of visible state. It performs no input, no
//! output, and no change to editor state. Every window rectangle comes from the
//! one layout calculation. See `docs/windows.md`.

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::core::{CharPosition, LineIndex, TextBuffer};
use crate::editor::{Cursor, Selection};
use crate::settings::{DisplaySettings, SignColumn};

use super::cells::{RowCell, RowSymbol, layout_row, terminal_column};
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

/// Whether one window holds the input focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WindowFocus {
    /// The window holds the focus.
    Focused,
    /// Another region holds the focus.
    Unfocused,
}

/// Everything that one window rectangle needs to render.
pub(super) struct WindowView<'a> {
    /// The buffer that the window shows.
    pub(super) buffer: &'a TextBuffer,
    /// The name that the winbar shows.
    pub(super) name: &'a str,
    /// The first visible line of the window.
    pub(super) first_line: usize,
    /// The first visible source column of the window.
    pub(super) left_column: usize,
    /// The cursor of the editing state.
    pub(super) cursor: Cursor,
    /// The selection of the active Visual mode.
    pub(super) selection: Option<Selection>,
    /// The matches of the active search query, in ascending order.
    pub(super) matches: &'a [CharPosition],
    /// The number of characters that one match holds.
    pub(super) match_chars: usize,
    /// Whether the window holds the input focus.
    pub(super) focus: WindowFocus,
    /// The visible layout settings of the editor.
    pub(super) display: &'a DisplaySettings,
    /// The number of cells that one tab occupies.
    pub(super) tab_width: usize,
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
/// The automatic rule reserves nothing while no sign source exists. Slice 13
/// adds the diagnostics that produce a sign.
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
    render_winbar(target, area, theme, view);
    let text = Rect {
        y: area.y.saturating_add(WINBAR_ROWS),
        height: area.height.saturating_sub(WINBAR_ROWS),
        ..area
    };
    if text.is_empty() {
        return;
    }
    let painter = RowPainter {
        view,
        theme,
        gutter: gutter_cells(view.buffer, view.display, text.width),
        spans: match_spans(view, text.height),
    };
    let mut scratch = String::new();
    for row in 0..text.height {
        let line = view.first_line.saturating_add(usize::from(row));
        let y = text.y.saturating_add(row);
        if line >= view.buffer.line_count() {
            render_end_of_buffer(target, text, y, theme);
            continue;
        }
        painter.render_gutter(target, text, y, line);
        painter.render_text(target, text, y, line, &mut scratch);
    }
}

/// Renders the winbar band of one window.
fn render_winbar(target: &mut CellBuffer, area: Rect, theme: Theme, view: &WindowView<'_>) {
    let band = Rect {
        height: WINBAR_ROWS,
        ..area
    };
    target.set_style(band, theme.style(ThemeRole::Winbar));
    let title = match view.focus {
        WindowFocus::Focused => ThemeRole::Title,
        WindowFocus::Unfocused => ThemeRole::TitleMuted,
    };
    let marker = if view.buffer.is_modified() {
        MODIFIED_MARKER
    } else {
        ""
    };
    target.set_stringn(
        band.x,
        band.y,
        format!(" {}{marker}", view.name),
        usize::from(band.width),
        theme.style(title),
    );
}

/// Renders one row below the last buffer line.
///
/// The end-of-buffer color equals the editor background, so the glyph marks the
/// row without drawing the reader's eye. The reference configuration does the
/// same.
fn render_end_of_buffer(target: &mut CellBuffer, text: Rect, y: u16, theme: Theme) {
    let row = Rect {
        y,
        height: 1,
        ..text
    };
    let style = theme.style(ThemeRole::EndOfBuffer);
    target.set_style(row, style);
    target.set_stringn(row.x, y, END_OF_BUFFER_GLYPH, usize::from(row.width), style);
}

/// The prepared state that every row of one window shares.
struct RowPainter<'a> {
    view: &'a WindowView<'a>,
    theme: Theme,
    /// The width of the sign column and the number column together, in cells.
    gutter: u16,
    spans: Vec<MatchSpan>,
}

impl RowPainter<'_> {
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
            target.set_style(
                Rect::new(text.x, y, signs, 1),
                self.theme.style(ThemeRole::SignColumn),
            );
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
        let selected = selected_columns(self.view, index, line_len);
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
            target_cell.set_style(self.cell_style(base, selected, line, line_len, *cell));
        }
    }

    /// Returns the style of one rendered cell.
    ///
    /// Each overlay patches the style below it, so the cursor keeps the colors
    /// of a selection or of a match and only inverts them.
    fn cell_style(
        &self,
        base: Style,
        selected: Option<(usize, usize)>,
        line: usize,
        line_len: usize,
        cell: RowCell,
    ) -> Style {
        let mut style = base;
        if let Some((first, last)) = selected
            && cell.column >= first
            && cell.column <= last
        {
            style = style.patch(self.theme.style(ThemeRole::Selection));
        }
        if cell.column < line_len
            && let Some(span) = self.spans.iter().find(|span| {
                span.line == line
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
        let cursor = self.view.cursor;
        if self.view.focus == WindowFocus::Focused
            && line == cursor.line().get()
            && cell.column == cursor.column().get()
            && cell.symbol != RowSymbol::WideTail
        {
            style = style.patch(self.theme.style(ThemeRole::Cursor));
        }
        style
    }
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
fn selected_columns(
    view: &WindowView<'_>,
    line: LineIndex,
    line_len: usize,
) -> Option<(usize, usize)> {
    let selection = view.selection?;
    let buffer = view.buffer;
    match selection {
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
                line_len
            };
            Some((first_column, last_column))
        }
        Selection::Linewise { first, last } => {
            if line < first || line > last {
                return None;
            }
            // Vim highlights the line terminator of a linewise selection, so the
            // highlight reaches one cell past the last character.
            Some((0, line_len))
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
            Some((left.get(), right.get()))
        }
    }
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
