//! The which-key overlay.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! The overlay lists the commands that the pending key sequence can still
//! reach. Every row comes from the realized binding table of the active
//! registry, so the overlay is never a hand-written list. See
//! `docs/input-actions.md`.

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;

use crate::input::WhichKeyRow;

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
        .map(|row| row.keys.to_string())
        .collect();
    let key_width = keys.iter().map(String::len).max().unwrap_or(0);
    for (index, row) in rows.iter().take(shown).enumerate() {
        let Ok(offset) = u16::try_from(index) else {
            debug_assert!(false, "the row bound keeps the index small");
            break;
        };
        let y = area.y + TITLE_ROWS + offset;
        let gap = " ".repeat(KEY_GAP_CELLS);
        let line = format!(" {:<key_width$}{gap}{}", keys[index], row.label());
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
