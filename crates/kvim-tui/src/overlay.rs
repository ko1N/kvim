//! The which-key overlay, the language-service float, and the notification
//! overlay.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! The which-key overlay lists the keys that may follow the pending key
//! sequence, one level at a time. Every row comes from the realized binding
//! table of the active registry, so the overlay is never a hand-written list.
//! See `docs/input-actions.md`.
//!
//! The float shows one bounded answer of the language services, such as a hover
//! text or the diagnostics at the cursor. It sits beside the cursor cell of the
//! window that asked, so it stands next to the text that it describes. It is
//! decoration: it changes no buffer text, no line mapping, and no cursor
//! position. See `docs/language-services.md`.
//!
//! The notification overlay sits in the bottom right corner of the body band.
//! It shows the work-done progress of every language server, and nothing else.
//! It is decoration as well: it moves no cursor, and it paints its text over the
//! buffer without a background.

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::Style;

use kvim_input::WhichKeyRow;
use kvim_language::DiagnosticSeverity;
use kvim_settings::FileTreeIcons;

use super::cells::{text_cells, wrap_cells};
use super::icons::{ICON_CELLS, Icon};
use super::language::{FLOAT_COLUMNS_MAX, FLOAT_ROWS_MAX, Float, FloatRow};
use super::notify::NotificationRow;
use super::theme::{Theme, ThemeRole};

/// The largest number of binding rows that one overlay column holds.
///
/// The bound keeps the overlay short for a prefix that reaches many commands,
/// even in a tall terminal. The overlay reports the rows that it drops.
const WHICH_KEY_COLUMN_ROWS_MAX: usize = 10;

/// The share of the body band that the overlay may cover.
///
/// The overlay answers a pending key while the reader still needs the text
/// around the cursor, so it never covers more than one part of the body out of
/// this many. The value two therefore keeps at least half of the buffer
/// visible, title row included.
const BODY_SHARE: u16 = 2;

/// The number of rows that the overlay title occupies.
const TITLE_ROWS: u16 = 1;

/// The number of cells between the key column and the label column.
const KEY_GAP_CELLS: usize = 2;

/// The number of cells that the overlay keeps left of its first column.
const LEFT_PAD_CELLS: usize = 1;

/// The number of cells between two overlay columns.
const COLUMN_GAP_CELLS: usize = 2;

/// The title of the overlay.
const OVERLAY_TITLE: &str = " Which Key ";

/// The number of cells that a float keeps beside its widest row.
///
/// The text of a row starts one cell inside the float, and one further cell
/// stays free at the right edge, so the surface color frames the text.
const FLOAT_PADDING_CELLS: usize = 2;

/// The text that replaces the last row of a float that hides rows.
///
/// The float bounds its height, so a long answer loses rows. The note reports
/// the loss instead of letting the answer end without a sign.
const FLOAT_OVERFLOW_NOTE: &str = "...";

/// The number of cells that the notification overlay keeps beside its text.
///
/// The reference configuration keeps the same gap between its text and the
/// right edge of the editor area. The overlay reaches the corner and holds that
/// gap inside its own rectangle, so the text sits one cell in from the edge.
/// The overlay keeps no row above or below its text, because every such row
/// would push the text further from the corner without separating anything.
const NOTIFICATION_PADDING_CELLS: u16 = 1;

/// The number of cells that the padding of both sides occupies.
const NOTIFICATION_PADDING_TOTAL: u16 = NOTIFICATION_PADDING_CELLS.saturating_mul(2);

/// Renders the notification overlay in the bottom right corner of the body.
///
/// The overlay paints text alone, as the reference configuration does. It
/// blanks no cell and carries no background and no border, so the buffer text
/// and the end-of-buffer markers stay visible between and around its rows. See
/// `docs/language-services.md`.
pub(super) fn render_notifications(
    target: &mut CellBuffer,
    body: Rect,
    theme: Theme,
    rows: &[NotificationRow<'_>],
) {
    if body.is_empty() || rows.is_empty() {
        return;
    }
    let painted: Vec<Vec<(String, ThemeRole)>> = rows.iter().map(segments).collect();
    // A terminal that cannot hold every row keeps the newest reports, because
    // the older ones already left the message line.
    let shown = painted.len().min(usize::from(body.height));
    let painted = &painted[painted.len() - shown..];
    let Ok(height) = u16::try_from(shown) else {
        debug_assert!(false, "the row bound of the board keeps the height small");
        return;
    };
    let text_width = painted.iter().map(|row| row_width(row)).max().unwrap_or(0);
    let width = text_width
        .saturating_add(NOTIFICATION_PADDING_TOTAL)
        .clamp(1, body.width);
    let area = Rect::new(body.right() - width, body.bottom() - height, width, height);
    let content_right = area.right().saturating_sub(NOTIFICATION_PADDING_CELLS);
    for (index, row) in painted.iter().enumerate() {
        let Ok(offset) = u16::try_from(index) else {
            debug_assert!(false, "the row bound of the board keeps the index small");
            break;
        };
        let y = area.y + offset;
        // Every row is right-aligned, so it starts one row width left of the
        // padded right edge. A row that is wider than the panel starts at the
        // left edge instead and clips, so no cell reaches outside the panel.
        let mut x = content_right.saturating_sub(row_width(row)).max(area.x);
        for (text, role) in row {
            if x >= content_right {
                break;
            }
            // Every notification role carries a foreground color alone, so the
            // painted cell keeps the background of the buffer behind it.
            let style = theme.style(*role);
            let remaining = usize::from(content_right - x);
            target.set_stringn(x, y, text, remaining, style);
            let painted_cells = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
            x = x.saturating_add(painted_cells);
        }
    }
}

/// Returns the painted segments of one notification row.
fn segments(row: &NotificationRow<'_>) -> Vec<(String, ThemeRole)> {
    match row {
        NotificationRow::Item {
            state,
            message,
            percentage,
        } => {
            let mut painted = vec![
                (format!("{} ", state.label()), state.role()),
                ((*message).to_owned(), ThemeRole::NotificationMessage),
            ];
            if let Some(percentage) = percentage {
                painted.push((format!(" {}%", percentage.get()), state.role()));
            }
            painted
        }
        NotificationRow::Group { title, spinner } => {
            let mut painted = vec![((*title).to_owned(), ThemeRole::NotificationGroup)];
            if let Some(spinner) = spinner {
                painted.push((format!(" {spinner}"), ThemeRole::NotificationGroup));
            }
            painted
        }
    }
}

/// Returns the number of cells that one painted row occupies.
fn row_width(row: &[(String, ThemeRole)]) -> u16 {
    let cells: usize = row.iter().map(|(text, _)| text.chars().count()).sum();
    u16::try_from(cells).unwrap_or(u16::MAX)
}

/// How the overlay spreads its rows over columns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ColumnLayout {
    /// The number of columns that the overlay paints.
    columns: usize,
    /// The number of rows that each column holds.
    ///
    /// Every column except the last one is full, because the overlay fills one
    /// column from top to bottom before it starts the next one.
    rows_per_column: usize,
}

impl ColumnLayout {
    /// The layout that paints nothing.
    const EMPTY: Self = Self {
        columns: 0,
        rows_per_column: 0,
    };

    /// Returns the number of rows that the layout shows out of `rows`.
    const fn shown(self, rows: usize) -> usize {
        let capacity = self.columns.saturating_mul(self.rows_per_column);
        if capacity < rows { capacity } else { rows }
    }
}

/// Returns the column layout of one which-key overlay.
///
/// The function is pure: `rows` counts the generated rows, `column_cells` is the
/// width of one column with its gap, `cells` is the width that the overlay may
/// use, and `rows_max` is the height bound of one column.
///
/// The overlay fills the width: it takes as many columns as `cells` holds, and
/// it then spreads the rows evenly over them, so no column stays empty. A
/// terminal that is narrower than one column still shows one column, which
/// clips at the body edge, because a single column is the readable minimum.
fn column_layout(rows: usize, column_cells: usize, cells: usize, rows_max: usize) -> ColumnLayout {
    debug_assert!(
        column_cells >= 1,
        "one column holds at least the key of one row"
    );
    if rows == 0 || rows_max == 0 || cells == 0 || column_cells == 0 {
        return ColumnLayout::EMPTY;
    }
    let fitting = (cells / column_cells).max(1);
    let columns = fitting.min(rows);
    let rows_per_column = rows.div_ceil(columns).min(rows_max);
    debug_assert!(rows_per_column >= 1, "a non-empty overlay holds one row");
    // An even spread can leave the last columns empty, for example four rows in
    // three fitting columns. The recount drops those columns.
    let columns = rows.div_ceil(rows_per_column).min(columns);
    ColumnLayout {
        columns,
        rows_per_column,
    }
}

/// One painted which-key row: its icon, its key, and its label.
struct PaintedRow {
    /// The icon of the command group, or `None` while the icons are hidden.
    icon: Option<Icon>,
    /// The key in its help form.
    key: String,
    /// The label of the command, or the group marker.
    label: String,
}

/// Renders the which-key overlay at the bottom of the body band.
///
/// The overlay covers the buffer text, so it blanks its rectangle first. It lays
/// the rows out in columns that fill the body width, and it keeps one icon for
/// each row, which the command group of that row selects. The one file-tree icon
/// setting also turns these icons off, and the columns stay aligned without
/// them. See `docs/input-actions.md`.
pub(super) fn render_which_key(
    target: &mut CellBuffer,
    body: Rect,
    theme: Theme,
    rows: &[WhichKeyRow],
    icons: FileTreeIcons,
) {
    if body.is_empty() || rows.is_empty() {
        return;
    }
    let painted: Vec<PaintedRow> = rows
        .iter()
        .map(|row| PaintedRow {
            icon: (icons == FileTreeIcons::Shown).then(|| Icon::of_group(row.group)),
            key: row.key_label().to_string(),
            label: row.target.to_string(),
        })
        .collect();
    // Every column keeps the width of the widest row, so the keys and the labels
    // of all columns align. The hidden icons reserve no cell, which keeps that
    // alignment without a patched font.
    let icon_cells = if icons == FileTreeIcons::Shown {
        ICON_CELLS
    } else {
        0
    };
    let key_cells = painted
        .iter()
        .map(|row| text_cells(&row.key))
        .max()
        .unwrap_or(0);
    let label_cells = painted
        .iter()
        .map(|row| text_cells(&row.label))
        .max()
        .unwrap_or(0);
    let content_cells = icon_cells + key_cells + KEY_GAP_CELLS + label_cells;
    let column_cells = content_cells.saturating_add(COLUMN_GAP_CELLS);

    // The height bound keeps the overlay over one part of the body only, so the
    // buffer text around the cursor stays visible while the reader chooses.
    let rows_max = usize::from((body.height / BODY_SHARE).saturating_sub(TITLE_ROWS))
        .min(WHICH_KEY_COLUMN_ROWS_MAX);
    let cells = usize::from(body.width).saturating_sub(LEFT_PAD_CELLS);
    let layout = column_layout(painted.len(), column_cells, cells, rows_max);
    let shown = layout.shown(painted.len());
    if shown == 0 {
        return;
    }
    let Ok(height) = u16::try_from(layout.rows_per_column) else {
        debug_assert!(false, "the row bound keeps the overlay height small");
        return;
    };
    let height = height.saturating_add(TITLE_ROWS);
    let area = Rect::new(body.x, body.bottom() - height, body.width, height);
    let surface = theme.style(ThemeRole::Surface);
    fill(target, area, " ");
    target.set_style(area, surface);
    render_which_key_title(target, area, theme, painted.len() - shown);

    let title = theme.style(ThemeRole::Title);
    for (index, row) in painted.iter().take(shown).enumerate() {
        let column = index / layout.rows_per_column;
        let offset = index % layout.rows_per_column;
        let Some(x) = column_start(area, column, column_cells) else {
            // A column that starts outside the body paints nothing, which the
            // one-column minimum only reaches on a terminal narrower than one
            // column.
            continue;
        };
        let Ok(offset) = u16::try_from(offset) else {
            debug_assert!(false, "the row bound keeps the index small");
            break;
        };
        let y = area.y + TITLE_ROWS + offset;
        let mut cursor = x;
        if let Some(icon) = row.icon {
            write_cells(
                target,
                area,
                &mut cursor,
                y,
                icon.glyph,
                surface.patch(theme.style(ThemeRole::Icon(icon.role))),
            );
            write_cells(target, area, &mut cursor, y, " ", surface);
        }
        // The keys carry the title color, so a reader finds the next key first.
        write_cells(target, area, &mut cursor, y, &row.key, title);
        let padding = " ".repeat(key_cells - text_cells(&row.key) + KEY_GAP_CELLS);
        write_cells(target, area, &mut cursor, y, &padding, surface);
        write_cells(target, area, &mut cursor, y, &row.label, surface);
    }
}

/// Renders the title row of the which-key overlay.
///
/// A prefix that reaches more rows than the bounded overlay holds loses the last
/// ones. The title row names how many rows the overlay dropped, so a reader
/// never believes an incomplete list, and the reader reaches those commands by
/// typing the next key instead.
fn render_which_key_title(target: &mut CellBuffer, area: Rect, theme: Theme, dropped: usize) {
    let title = theme.style(ThemeRole::Title);
    target.set_stringn(
        area.x,
        area.y,
        OVERLAY_TITLE,
        usize::from(area.width),
        title,
    );
    if dropped == 0 {
        return;
    }
    let note = format!("+{dropped} more ");
    let width = usize::from(area.width);
    // The note never covers the title, so a narrow overlay keeps its name and
    // drops the count instead.
    if text_cells(OVERLAY_TITLE) + text_cells(&note) > width {
        return;
    }
    let Ok(offset) = u16::try_from(width - text_cells(&note)) else {
        debug_assert!(false, "the terminal width fits into a u16");
        return;
    };
    target.set_stringn(
        area.x.saturating_add(offset),
        area.y,
        &note,
        text_cells(&note),
        title,
    );
}

/// Returns the first cell of one overlay column, or `None` outside the body.
fn column_start(area: Rect, column: usize, column_cells: usize) -> Option<u16> {
    let offset = LEFT_PAD_CELLS.checked_add(column.checked_mul(column_cells)?)?;
    let offset = u16::try_from(offset).ok()?;
    let x = area.x.checked_add(offset)?;
    (x < area.right()).then_some(x)
}

/// Writes one text at `cursor` and moves the cursor past it.
///
/// The text clips at the right edge of the overlay, so no cell ever reaches
/// outside the body band. One column never reaches into the next one, because
/// every column carries the width of the widest row.
fn write_cells(
    target: &mut CellBuffer,
    area: Rect,
    cursor: &mut u16,
    y: u16,
    text: &str,
    style: Style,
) {
    if *cursor >= area.right() {
        return;
    }
    let remaining = usize::from(area.right() - *cursor);
    target.set_stringn(*cursor, y, text, remaining, style);
    let written = u16::try_from(text_cells(text).min(remaining)).unwrap_or(u16::MAX);
    *cursor = cursor.saturating_add(written);
}

/// Returns the rectangle that one float of `desired` size occupies.
///
/// The float belongs to one window, so it never reaches outside `window`. It
/// prefers the row below the cursor, and it flips above the cursor line when
/// the space below cannot hold it. Neither side ever covers the cursor line
/// itself, because that line holds the text that the float describes. A side
/// that holds too few rows clips the height instead of moving the float, so the
/// float stays anchored. Horizontally the float starts at the cursor column and
/// moves left until its right edge sits inside the window.
///
/// The function is pure: it reads the cursor cell, the window rectangle, and
/// the size, and it returns the rectangle.
pub(super) fn float_area(window: Rect, cursor: Position, desired: Size) -> Rect {
    debug_assert!(
        window.contains(cursor),
        "the renderer anchors a float to a cursor cell of that window"
    );
    let below = window.bottom().saturating_sub(cursor.y.saturating_add(1));
    let above = cursor.y.saturating_sub(window.y);
    let (y, height) = if desired.height <= below {
        (cursor.y.saturating_add(1), desired.height)
    } else if desired.height <= above {
        (cursor.y - desired.height, desired.height)
    } else if below >= above {
        (cursor.y.saturating_add(1), below)
    } else {
        (window.y, above)
    };
    let width = desired.width.min(window.width);
    let x = cursor
        .x
        .min(window.right().saturating_sub(width))
        .max(window.x);
    Rect::new(x, y, width, height)
}

/// Renders one language-service float beside the cursor of one window.
///
/// The float covers the buffer text, so it blanks its rectangle first. A cursor
/// that scrolled out of the visible rows anchors the float to the last window
/// row, so the float still belongs to the window that asked.
pub(super) fn render_float(
    target: &mut CellBuffer,
    window: Rect,
    cursor: Option<Position>,
    theme: Theme,
    float: &Float,
) {
    if window.is_empty() || float.rows.is_empty() {
        return;
    }
    let cursor =
        cursor.unwrap_or_else(|| Position::new(window.x, window.bottom().saturating_sub(1)));
    // The text starts one cell inside the float, so the padding covers one cell
    // on each side. A window that leaves no cell for text shows no float.
    let budget = usize::from(window.width)
        .saturating_sub(FLOAT_PADDING_CELLS)
        .min(FLOAT_COLUMNS_MAX);
    if budget == 0 {
        return;
    }
    let mut lines = wrap_float(float, budget);
    // The row bound applies before the measurement, so a row that the float
    // never shows cannot widen it.
    fit(&mut lines, FLOAT_ROWS_MAX);
    let text_width = lines
        .iter()
        .map(|row| text_cells(&row.text))
        .max()
        .unwrap_or(0);
    let width = text_width
        .saturating_add(FLOAT_PADDING_CELLS)
        .max(text_cells(float.title));
    let width = u16::try_from(width)
        .unwrap_or(u16::MAX)
        .clamp(1, window.width);
    let Ok(height) = u16::try_from(lines.len()) else {
        debug_assert!(false, "the row bound keeps the float height small");
        return;
    };
    let area = float_area(
        window,
        cursor,
        Size::new(width, height.saturating_add(TITLE_ROWS)),
    );
    let shown = usize::from(area.height.saturating_sub(TITLE_ROWS));
    if shown == 0 {
        return;
    }
    fit(&mut lines, shown);

    let surface = theme.style(ThemeRole::Surface);
    fill(target, area, " ");
    target.set_style(area, surface);
    target.set_stringn(
        area.x,
        area.y,
        float.title,
        usize::from(area.width),
        theme.style(ThemeRole::Title),
    );
    for (index, row) in lines.iter().enumerate() {
        let Ok(offset) = u16::try_from(index) else {
            debug_assert!(false, "the row bound keeps the index small");
            break;
        };
        let style = match row.severity {
            Some(severity) => surface.patch(theme.style(severity_role(severity))),
            None => surface,
        };
        target.set_stringn(
            area.x.saturating_add(1),
            area.y + TITLE_ROWS + offset,
            &row.text,
            usize::from(area.width.saturating_sub(1)),
            style,
        );
    }
}

/// Wraps every source row of one float into rows of at most `cells` cells.
///
/// The result holds at most one row beyond [`FLOAT_ROWS_MAX`], because [`fit`]
/// needs to see only that further rows exist.
fn wrap_float(float: &Float, cells: usize) -> Vec<FloatRow> {
    let mut lines: Vec<FloatRow> = Vec::new();
    for row in &float.rows {
        for text in wrap_cells(&row.text, cells) {
            if lines.len() > FLOAT_ROWS_MAX {
                return lines;
            }
            lines.push(FloatRow {
                text,
                severity: row.severity,
            });
        }
    }
    lines
}

/// Shortens the rows of one float to `shown` rows.
///
/// A float that holds more rows than fit keeps its first rows and reports the
/// missing ones in the last one, so no row disappears without a note.
fn fit(lines: &mut Vec<FloatRow>, shown: usize) {
    debug_assert!(shown >= 1, "the caller returns before an empty float");
    if lines.len() <= shown {
        return;
    }
    lines.truncate(shown);
    let Some(last) = lines.last_mut() else {
        debug_assert!(false, "the truncation keeps at least one row");
        return;
    };
    last.text = FLOAT_OVERFLOW_NOTE.to_owned();
    last.severity = None;
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

/// Writes one symbol into every cell of a rectangle.
///
/// An overlay covers text that the renderer already wrote, so it clears its
/// rectangle before it draws.
fn fill(target: &mut CellBuffer, area: Rect, symbol: &str) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = target.cell_mut((x, y)) {
                cell.set_symbol(symbol);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::{Position, Rect, Size};

    use super::{ColumnLayout, column_layout, float_area};

    /// One editor window that starts at the top left corner.
    const WINDOW: Rect = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 20,
    };

    /// The right window of one vertical split, which starts inside the body.
    const SPLIT: Rect = Rect {
        x: 20,
        y: 3,
        width: 20,
        height: 10,
    };

    #[test]
    fn a_float_sits_below_the_cursor_line() {
        let area = float_area(WINDOW, Position::new(5, 2), Size::new(12, 4));
        assert_eq!(
            area,
            Rect::new(5, 3, 12, 4),
            "the float starts one row down"
        );
    }

    #[test]
    fn a_float_flips_above_the_cursor_line_when_the_space_below_is_too_small() {
        // Three rows follow the cursor line, and the float needs four.
        let area = float_area(WINDOW, Position::new(5, 16), Size::new(12, 4));
        assert_eq!(area, Rect::new(5, 12, 12, 4));
        assert_eq!(area.bottom(), 16, "the float never covers the cursor line");
    }

    #[test]
    fn a_float_takes_the_larger_side_and_clips_when_neither_side_holds_it() {
        let narrow = Rect::new(0, 0, 40, 7);
        // Two rows sit above the cursor line and four below it.
        let area = float_area(narrow, Position::new(5, 2), Size::new(12, 9));
        assert_eq!(area, Rect::new(5, 3, 12, 4));
        // Four rows sit above the cursor line and two below it.
        let area = float_area(narrow, Position::new(5, 4), Size::new(12, 9));
        assert_eq!(area, Rect::new(5, 0, 12, 4));
    }

    #[test]
    fn a_float_moves_left_until_its_right_edge_sits_inside_the_window() {
        let area = float_area(WINDOW, Position::new(36, 2), Size::new(12, 4));
        assert_eq!(area, Rect::new(28, 3, 12, 4));
        assert_eq!(area.right(), WINDOW.right());
    }

    #[test]
    fn a_float_that_is_wider_than_the_window_starts_at_the_window_edge() {
        let area = float_area(WINDOW, Position::new(36, 2), Size::new(60, 4));
        assert_eq!(area, Rect::new(0, 3, 40, 4));
    }

    #[test]
    fn a_float_of_a_split_stays_inside_that_window() {
        // The cursor sits near the right edge and near the bottom of the split,
        // so both rules act, and both keep the float inside the split.
        let area = float_area(SPLIT, Position::new(38, 11), Size::new(14, 5));
        assert_eq!(area, Rect::new(26, 6, 14, 5));
        assert!(
            SPLIT.contains(Position::new(area.x, area.y))
                && area.right() <= SPLIT.right()
                && area.bottom() <= SPLIT.bottom(),
            "the float of a split never reaches outside that window"
        );
    }

    #[test]
    fn a_window_of_one_row_leaves_no_space_beside_the_cursor_line() {
        let single = Rect::new(0, 0, 40, 1);
        let area = float_area(single, Position::new(0, 0), Size::new(12, 4));
        assert_eq!(area.height, 0, "no row remains beside the cursor line");
    }

    #[test]
    fn a_wide_terminal_spreads_the_rows_over_columns() {
        // Five columns of twenty cells fit into one hundred cells, so ten rows
        // need two rows in each column.
        let layout = column_layout(10, 20, 100, 10);
        assert_eq!(
            layout,
            ColumnLayout {
                columns: 5,
                rows_per_column: 2,
            }
        );
        assert_eq!(layout.shown(10), 10, "every row fits");
    }

    #[test]
    fn a_narrow_terminal_keeps_one_column() {
        let layout = column_layout(6, 40, 30, 10);
        assert_eq!(
            layout,
            ColumnLayout {
                columns: 1,
                rows_per_column: 6,
            }
        );
        // A terminal narrower than the widest row still shows that one column,
        // which clips at the body edge.
        assert_eq!(column_layout(3, 40, 5, 10).columns, 1);
    }

    #[test]
    fn the_height_bound_drops_the_rows_that_no_column_holds() {
        let layout = column_layout(30, 20, 100, 4);
        assert_eq!(
            layout,
            ColumnLayout {
                columns: 5,
                rows_per_column: 4,
            }
        );
        assert_eq!(
            layout.shown(30),
            20,
            "ten rows stay out of the bounded overlay"
        );
    }

    #[test]
    fn no_column_of_the_overlay_stays_empty() {
        // Three columns fit, but four rows spread over three columns would
        // leave the third one empty, so two columns of two rows remain.
        let layout = column_layout(4, 20, 70, 10);
        assert_eq!(
            layout,
            ColumnLayout {
                columns: 2,
                rows_per_column: 2,
            }
        );
    }

    #[test]
    fn an_overlay_without_rows_or_without_space_paints_nothing() {
        assert_eq!(column_layout(0, 20, 100, 10).shown(0), 0);
        assert_eq!(column_layout(5, 20, 100, 0).shown(5), 0, "no row fits");
        assert_eq!(column_layout(5, 20, 0, 10).shown(5), 0, "no cell is free");
    }
}
