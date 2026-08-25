//! The two views of one captured diff.
//!
//! Both views read the aligned rows that `kvim-workspace` publishes, so neither
//! owns a row model of its own and neither can disagree with the other about
//! what one hunk holds. The two-column view draws each side in its own column.
//! The inline view draws one column and marks the origin of every line. See
//! `docs/diff-view.md`.
//!
//! The module is pure. It builds the drawable rows and paints them into cells.
//! It reads no clock, no filesystem, and no process.

use kvim_settings::{DiffSettings, DiffView};
use kvim_workspace::{AlignedRow, DiffLine, DiffLineText, DiffSide, Hunk, align_hunk};

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::cells::clip_cells;
use crate::theme::{Theme, ThemeRole};

/// The text that stands for one line that holds no valid text.
///
/// A diff publishes exact bytes, and not every byte sequence is text. The row
/// states what the line holds instead of guessing characters for it.
const NO_TEXT_MARKER: &str = "<no text>";

/// The number of cells that one line-number column occupies.
const NUMBER_CELLS: u16 = 5;

/// The number of cells between the two columns of the side-by-side view.
const COLUMN_GAP_CELLS: u16 = 1;

/// The marker that the inline view draws before one line.
const CONTEXT_MARKER: char = ' ';
const ADDED_MARKER: char = '+';
const REMOVED_MARKER: char = '-';

/// The band that one drawn row carries.
///
/// The cursor row of the focused region takes the selection band over its whole
/// width, so it reads like a Visual-line selection instead of a mark at one
/// edge. Every other row keeps the colors of its own change.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum RowBand {
    /// The row keeps the colors of its own change.
    #[default]
    Plain,
    /// The row carries the selection band across its whole width.
    Selected,
}

impl RowBand {
    /// Returns the style of one role under this band.
    ///
    /// A selected row keeps the foreground of its role, so an added line still
    /// reads as added, and takes the background of the selection.
    pub(super) fn apply(self, theme: Theme, role: ThemeRole) -> Style {
        match self {
            Self::Plain => theme.style(role),
            Self::Selected => {
                let selection = theme.style(ThemeRole::PopupSelection);
                match selection.bg {
                    Some(background) => theme.style(role).bg(background),
                    None => selection,
                }
            }
        }
    }
}

/// One column of one drawn diff row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DiffCell {
    /// The line number of the side, or `None` for a gap.
    pub(super) number: Option<u32>,
    /// The text of the line, or the marker of a line that holds none.
    pub(super) text: String,
    /// The role that paints the cell.
    pub(super) role: ThemeRole,
}

impl DiffCell {
    /// Returns the cell that one published line draws on one side.
    fn of(line: &DiffLine, side: DiffSide, role: ThemeRole) -> Self {
        Self {
            number: line.number(side),
            text: line_text(line.text()),
            role,
        }
    }

    /// Returns the cell that draws no line.
    fn gap() -> Self {
        Self {
            number: None,
            text: String::new(),
            role: ThemeRole::DiffGap,
        }
    }
}

/// One drawn row of the side-by-side view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SideRow {
    /// The column that draws the old side.
    pub(super) old: DiffCell,
    /// The column that draws the new side.
    pub(super) new: DiffCell,
}

/// One drawn row of the inline view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct InlineRow {
    /// The character that names the origin of the line.
    pub(super) marker: char,
    /// The one column that the view draws.
    pub(super) cell: DiffCell,
}

/// Returns the side-by-side rows of one hunk.
///
/// A context line draws in both columns. A replacement draws the removed line
/// beside the added line that took its place. A surplus on either side draws
/// against a gap.
pub(super) fn side_rows(hunk: &Hunk) -> Vec<SideRow> {
    align_hunk(hunk).iter().map(side_row).collect()
}

/// Returns the inline rows of one hunk.
///
/// The order follows the published lines: a removed line comes before the added
/// line that replaced it, which is the order that every unified diff writes.
pub(super) fn inline_rows(hunk: &Hunk) -> Vec<InlineRow> {
    let mut rows = Vec::with_capacity(hunk.lines().len());
    for row in align_hunk(hunk) {
        if row.is_context() {
            let line = row
                .old_line()
                .expect("a context row holds the same line on both sides");
            rows.push(InlineRow {
                marker: CONTEXT_MARKER,
                cell: DiffCell::of(line, DiffSide::Old, ThemeRole::DiffContext),
            });
            continue;
        }
        if let Some(line) = row.old_line() {
            rows.push(InlineRow {
                marker: REMOVED_MARKER,
                cell: DiffCell::of(line, DiffSide::Old, ThemeRole::DiffRemoved),
            });
        }
        if let Some(line) = row.new_line() {
            rows.push(InlineRow {
                marker: ADDED_MARKER,
                cell: DiffCell::of(line, DiffSide::New, ThemeRole::DiffAdded),
            });
        }
    }
    rows
}

/// Returns the view that one width and one setting select.
///
/// Two columns of a handful of cells each show nothing useful, so a window that
/// cannot hold two full columns draws inline whatever the setting asks for.
pub(super) fn view_of(settings: DiffSettings, width: u16) -> DiffView {
    if settings.view == DiffView::Inline {
        return DiffView::Inline;
    }
    if width >= two_column_cells_min(settings) {
        DiffView::SideBySide
    } else {
        DiffView::Inline
    }
}

/// Returns the smallest width that the two-column view needs.
pub(super) const fn two_column_cells_min(settings: DiffSettings) -> u16 {
    let column = settings.side_column_cells_min.saturating_add(NUMBER_CELLS);
    column.saturating_mul(2).saturating_add(COLUMN_GAP_CELLS)
}

/// Paints one run of side-by-side rows into one rectangle.
///
/// The caller supplies the rows that the viewport shows, so this function
/// scrolls nothing and reads no state. Each column takes half of the width that
/// stays after the gap, and a text that passes its column clips.
pub(super) fn draw_side_rows(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    rows: &[SideRow],
    band: RowBand,
) {
    let columns = column_width(area.width);
    for (offset, row) in rows.iter().take(usize::from(area.height)).enumerate() {
        let y = area.y + u16::try_from(offset).unwrap_or(u16::MAX);
        // The gap between the two columns belongs to the row, so the band
        // covers it as well and the selection holds no hole.
        if band == RowBand::Selected {
            target.set_style(
                Rect::new(area.x, y, area.width, 1),
                band.apply(theme, ThemeRole::DiffContext),
            );
        }
        draw_cell(
            target,
            Rect::new(area.x, y, columns, 1),
            theme,
            &row.old,
            None,
            band,
        );
        let right = area
            .x
            .saturating_add(columns)
            .saturating_add(COLUMN_GAP_CELLS);
        draw_cell(
            target,
            Rect::new(right, y, columns, 1),
            theme,
            &row.new,
            None,
            band,
        );
    }
}

/// Paints one run of inline rows into one rectangle.
pub(super) fn draw_inline_rows(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    rows: &[InlineRow],
    band: RowBand,
) {
    for (offset, row) in rows.iter().take(usize::from(area.height)).enumerate() {
        let y = area.y + u16::try_from(offset).unwrap_or(u16::MAX);
        draw_cell(
            target,
            Rect::new(area.x, y, area.width, 1),
            theme,
            &row.cell,
            Some(row.marker),
            band,
        );
    }
}

/// Returns the width of one column of the two-column view.
fn column_width(width: u16) -> u16 {
    width.saturating_sub(COLUMN_GAP_CELLS) / 2
}

/// Paints one cell of one row: its number, its marker, and its text.
fn draw_cell(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    cell: &DiffCell,
    marker: Option<char>,
    band: RowBand,
) {
    if area.width == 0 {
        return;
    }
    let style = |role: ThemeRole| band.apply(theme, role);
    target.set_style(area, style(cell.role));

    let number_cells = NUMBER_CELLS.min(area.width);
    if number_cells > 0 {
        let text = cell
            .number
            .map_or_else(String::new, |number| format!("{number:>4} "));
        target.set_stringn(
            area.x,
            area.y,
            &text,
            usize::from(number_cells),
            style(ThemeRole::DiffLineNumber),
        );
    }

    let mut x = area.x.saturating_add(number_cells);
    let mut left = area.width.saturating_sub(number_cells);
    if let Some(marker) = marker {
        if left == 0 {
            return;
        }
        let mut scratch = [0_u8; 4];
        target.set_stringn(
            x,
            area.y,
            &*marker.encode_utf8(&mut scratch),
            1,
            style(cell.role),
        );
        x = x.saturating_add(1);
        left = left.saturating_sub(1);
    }
    if left == 0 {
        return;
    }
    target.set_stringn(
        x,
        area.y,
        clip_cells(&cell.text, usize::from(left)),
        usize::from(left),
        style(cell.role),
    );
}

/// Returns the side-by-side row of one aligned row.
fn side_row(row: &AlignedRow<'_>) -> SideRow {
    if row.is_context() {
        let line = row
            .old_line()
            .expect("a context row holds the same line on both sides");
        return SideRow {
            old: DiffCell::of(line, DiffSide::Old, ThemeRole::DiffContext),
            new: DiffCell::of(line, DiffSide::New, ThemeRole::DiffContext),
        };
    }
    SideRow {
        old: row.old_line().map_or_else(DiffCell::gap, |line| {
            DiffCell::of(line, DiffSide::Old, ThemeRole::DiffRemoved)
        }),
        new: row.new_line().map_or_else(DiffCell::gap, |line| {
            DiffCell::of(line, DiffSide::New, ThemeRole::DiffAdded)
        }),
    }
}

/// Returns the drawable text of one published line.
///
/// A line that holds no valid text draws its state instead of guessed
/// characters, because the capture published exact bytes and the view never
/// invents any.
fn line_text(text: &DiffLineText) -> String {
    text.as_str().map_or_else(
        || NO_TEXT_MARKER.to_owned(),
        |value| value.trim_end_matches(['\n', '\r']).to_owned(),
    )
}

#[cfg(test)]
#[path = "diff_view_tests.rs"]
mod tests;
