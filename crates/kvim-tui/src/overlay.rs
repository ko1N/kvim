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
//! text or the diagnostics at the cursor. It is decoration: it changes no
//! buffer text, no line mapping, and no cursor position. See
//! `docs/language-services.md`.
//!
//! The notification overlay sits in the bottom right corner of the body band.
//! It shows the work-done progress of every language server, and nothing else.
//! It is decoration as well: it moves no cursor, and it paints its text over the
//! buffer without a background.

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;

use kvim_input::WhichKeyRow;
use kvim_language::DiagnosticSeverity;

use super::language::Float;
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
const FLOAT_PADDING_CELLS: usize = 2;

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

/// Renders one language-service float at the bottom of the body band.
///
/// The float covers the buffer text, so it blanks its rectangle first. It sits
/// where the which-key overlay sits, because both answer the key that the user
/// pressed last and never appear together.
pub(super) fn render_float(target: &mut CellBuffer, body: Rect, theme: Theme, float: &Float) {
    if body.is_empty() || float.rows.is_empty() {
        return;
    }
    let available = usize::from(body.height.saturating_sub(TITLE_ROWS));
    let shown = float.rows.len().min(available);
    if shown == 0 {
        return;
    }
    let Ok(height) = u16::try_from(shown) else {
        debug_assert!(false, "the row bound keeps the float height small");
        return;
    };
    let height = height.saturating_add(TITLE_ROWS);
    let width = float
        .rows
        .iter()
        .take(shown)
        .map(|row| row.text.chars().count().saturating_add(FLOAT_PADDING_CELLS))
        .chain(std::iter::once(float.title.chars().count()))
        .max()
        .unwrap_or(0);
    let width = u16::try_from(width)
        .unwrap_or(u16::MAX)
        .clamp(1, body.width);
    let area = Rect::new(body.x, body.bottom() - height, width, height);
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
    for (index, row) in float.rows.iter().take(shown).enumerate() {
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
