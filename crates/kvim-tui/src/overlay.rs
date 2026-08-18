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

use kvim_input::WhichKeyRow;
use kvim_language::DiagnosticSeverity;

use super::cells::{text_cells, wrap_cells};
use super::language::{FLOAT_COLUMNS_MAX, FLOAT_ROWS_MAX, Float, FloatRow};
use super::notify::NotificationRow;
use super::theme::{Theme, ThemeRole};

/// The largest number of binding rows that the overlay shows.
///
/// The bound keeps the overlay inside one screen for a prefix that reaches many
/// commands. The overlay reports the rows that it drops.
pub(super) const WHICH_KEY_ROWS_MAX: usize = 16;

/// The number of rows that the overlay title occupies.
const TITLE_ROWS: u16 = 1;

/// The number of cells between the key column and the label column.
const KEY_GAP_CELLS: usize = 2;

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

/// Renders the which-key overlay at the bottom of the body band.
///
/// The overlay covers the buffer text, so it blanks its rectangle first.
pub(super) fn render_which_key(
    target: &mut CellBuffer,
    body: Rect,
    theme: Theme,
    rows: &[WhichKeyRow],
) {
    if body.is_empty() || rows.is_empty() {
        return;
    }
    let available = usize::from(body.height.saturating_sub(TITLE_ROWS));
    let shown = rows.len().min(WHICH_KEY_ROWS_MAX).min(available);
    if shown == 0 {
        return;
    }
    let Ok(height) = u16::try_from(shown) else {
        debug_assert!(false, "the row bound keeps the overlay height small");
        return;
    };
    let height = height.saturating_add(TITLE_ROWS);
    let area = Rect::new(body.x, body.bottom() - height, body.width, height);
    let surface = theme.style(ThemeRole::Surface);
    fill(target, area, " ");
    target.set_style(area, surface);
    target.set_stringn(
        area.x,
        area.y,
        OVERLAY_TITLE,
        usize::from(area.width),
        theme.style(ThemeRole::Title),
    );

    let keys: Vec<String> = rows
        .iter()
        .take(shown)
        .map(|row| row.key_label().to_string())
        .collect();
    let key_width = keys.iter().map(String::len).max().unwrap_or(0);
    for (index, row) in rows.iter().take(shown).enumerate() {
        let Ok(offset) = u16::try_from(index) else {
            debug_assert!(false, "the row bound keeps the index small");
            break;
        };
        let y = area.y + TITLE_ROWS + offset;
        let gap = " ".repeat(KEY_GAP_CELLS);
        let line = format!(" {:<key_width$}{gap}{}", keys[index], row.target);
        target.set_stringn(area.x, y, &line, usize::from(area.width), surface);
        // The keys carry the title color, so a reader finds the next key first.
        target.set_stringn(
            area.x + 1,
            y,
            &keys[index],
            key_width,
            theme.style(ThemeRole::Title),
        );
    }
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

    use super::float_area;

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
}
