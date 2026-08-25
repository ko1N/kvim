//! The terminal-wide chrome: the shell bands, the statusline, and the message
//! line.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! kvim shows one statusline and one message line for the whole terminal, and
//! one winbar for each window. Regions carry no divider glyph: the surface band
//! of the winbar and of the statusline separates them by color, as ReviewGraph
//! does. See `docs/windows.md`.

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;

use kvim_editor::Cursor;
use kvim_input::Mode;

use super::cells::text_cells;
use super::language::FormatOnSave;
use super::session::{Confirmation, Message, MessageLevel, PromptLine};
use super::theme::{Theme, ThemeRole};

/// The number of rows that the statusline occupies.
const STATUSLINE_ROWS: u16 = 1;

/// The number of rows that the message line occupies.
const MESSAGE_ROWS: u16 = 1;

/// The number of rows that both chrome bands occupy together.
const CHROME_ROWS: u16 = STATUSLINE_ROWS + MESSAGE_ROWS;

/// The number of blank cells that separate the mode from the format-on-save
/// state.
///
/// The mode label and the state label each end with one blank, so this gap
/// keeps a wide mode name apart from the state on a narrow band.
const STATUSLINE_GAP_CELLS: usize = 1;

/// The three bands of the terminal.
///
/// The window tree receives the body band only. A terminal that cannot hold
/// every band drops the bands in a deterministic order: the body first, then
/// the statusline. The message line survives longest, because it reports why
/// the terminal is too small.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ShellAreas {
    /// The rectangle that holds the window tree.
    pub(super) body: Rect,
    /// The statusline band.
    pub(super) statusline: Rect,
    /// The message line, which the command line and the search prompt share.
    pub(super) message: Rect,
}

/// Splits the terminal into the three chrome bands.
pub(super) fn shell_areas(area: Rect) -> ShellAreas {
    let empty = Rect::new(area.x, area.y, area.width, 0);
    let row = |offset: u16| Rect::new(area.x, area.y + offset, area.width, 1);
    match area.height {
        0 => ShellAreas {
            body: empty,
            statusline: empty,
            message: empty,
        },
        1 => ShellAreas {
            body: empty,
            statusline: empty,
            message: row(0),
        },
        2 => ShellAreas {
            body: empty,
            statusline: row(0),
            message: row(1),
        },
        height => ShellAreas {
            body: Rect::new(area.x, area.y, area.width, height - CHROME_ROWS),
            statusline: row(height - CHROME_ROWS),
            message: row(height - MESSAGE_ROWS),
        },
    }
}

/// Renders the mode, the format-on-save state, and the cursor position into
/// the statusline band.
///
/// A band that cannot hold every part drops them in a fixed order: the
/// format-on-save state first, then the cursor position. The mode always
/// survives, because the mode decides what the next key does.
///
/// A buffer that no formatter can format reports no state at all, so `format`
/// is `None` there and the band shows the mode and the position alone.
pub(super) fn render_statusline(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    mode: Mode,
    cursor: Cursor,
    format: Option<FormatOnSave>,
) {
    if area.is_empty() {
        return;
    }
    let style = theme.style(ThemeRole::Statusline);
    target.set_style(area, style);
    let mode_text = format!(" {mode} ");
    // The title role already carries the surface band, so the mode reads as
    // chrome on the same background as the rest of the statusline.
    target.set_stringn(
        area.x,
        area.y,
        &mode_text,
        usize::from(area.width),
        theme.style(ThemeRole::Title),
    );

    let position = format!("{}:{} ", cursor.line().get() + 1, cursor.column().get() + 1);
    let width = usize::from(area.width);
    let mode_cells = text_cells(&mode_text);
    let position_cells = text_cells(&position);
    if width < mode_cells + position_cells {
        return;
    }
    let Ok(position_offset) = u16::try_from(position_cells) else {
        debug_assert!(false, "one cursor position never fills a terminal row");
        return;
    };
    target.set_stringn(
        area.right() - position_offset,
        area.y,
        &position,
        position_cells,
        style,
    );

    let Some(state) = format.map(|state| format!("{} ", state.label())) else {
        return;
    };
    let state_cells = text_cells(&state);
    if width < mode_cells + state_cells + position_cells + STATUSLINE_GAP_CELLS {
        return;
    }
    let Ok(state_offset) = u16::try_from(state_cells + position_cells) else {
        debug_assert!(false, "the checked width bounds both labels by the band");
        return;
    };
    // The state answers a question the reader asks once, so it stays quiet
    // beside the mode. See `docs/windows.md`.
    target.set_stringn(
        area.right() - state_offset,
        area.y,
        &state,
        state_cells,
        theme.style(ThemeRole::StatuslineMuted),
    );
}

/// Renders the open confirmation, the open prompt, or the last message, into
/// the message line.
///
/// The confirmation owns the keys, so its question covers both other entries.
/// The question, its hint, and the typed answer share the row.
pub(super) fn render_message(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    confirmation: Option<&Confirmation>,
    prompt: Option<&PromptLine>,
    message: Option<&Message>,
) {
    if area.is_empty() {
        return;
    }
    let base = theme.style(ThemeRole::Text);
    target.set_style(area, base);
    if let Some(confirmation) = confirmation {
        // The user types the answer after the hint, so the row draws its own
        // cursor behind that answer, exactly as a prompt does. See
        // `docs/windows.md`.
        let line = format!("{}? [y/N]:{}", confirmation.question, confirmation.answer);
        let (x, _) = target.set_stringn(area.x, area.y, &line, usize::from(area.width), base);
        if let Some(cell) = target.cell_mut((x, area.y)) {
            cell.set_style(base.patch(theme.style(ThemeRole::Cursor)));
        }
        return;
    }
    if let Some(prompt) = prompt {
        let line = format!("{}{}", prompt.kind.prefix(), prompt.text);
        let (x, _) = target.set_stringn(area.x, area.y, &line, usize::from(area.width), base);
        // The terminal cursor marks the cell of the focused window, so the
        // prompt draws its own cursor at the end of the line.
        if let Some(cell) = target.cell_mut((x, area.y)) {
            cell.set_style(base.patch(theme.style(ThemeRole::Cursor)));
        }
        return;
    }
    let Some(message) = message else {
        return;
    };
    // An ordinary report reads like buffer text, so only a warning and a
    // failure stand out on the message line.
    let style = match message.level {
        MessageLevel::Error => base.patch(theme.style(ThemeRole::Error)),
        MessageLevel::Warning => base.patch(theme.style(ThemeRole::Warning)),
        MessageLevel::Info => base,
    };
    target.set_stringn(
        area.x,
        area.y,
        &message.text,
        usize::from(area.width),
        style,
    );
}

#[cfg(test)]
#[path = "chrome_tests.rs"]
mod tests;
