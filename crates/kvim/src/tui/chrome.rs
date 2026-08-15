//! The terminal-wide chrome: the shell bands, the statusline, and the message
//! line.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! Kvim shows one statusline and one message line for the whole terminal, and
//! one winbar for each window. Regions carry no divider glyph: the surface band
//! of the winbar and of the statusline separates them by color, as ReviewGraph
//! does. See `docs/windows.md`.

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;

use crate::editor::Cursor;
use crate::input::{Mode, PromptKind};

use super::session::{Message, MessageLevel, PromptLine};
use super::theme::{Theme, ThemeRole};

/// The number of rows that the statusline occupies.
const STATUSLINE_ROWS: u16 = 1;

/// The number of rows that the message line occupies.
const MESSAGE_ROWS: u16 = 1;

/// The number of rows that both chrome bands occupy together.
const CHROME_ROWS: u16 = STATUSLINE_ROWS + MESSAGE_ROWS;

/// The prompt character of the command line.
const COMMAND_LINE_PREFIX: char = ':';

/// The prompt character of the search prompt.
const SEARCH_PREFIX: char = '/';

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

/// Renders the mode and the cursor position into the statusline band.
///
/// A band that cannot hold both parts keeps the mode, because the mode decides
/// what the next key does.
pub(super) fn render_statusline(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    mode: Mode,
    cursor: Cursor,
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
    let Ok(width) = u16::try_from(position.len()) else {
        debug_assert!(false, "one cursor position never fills a terminal row");
        return;
    };
    let Ok(used) = u16::try_from(mode_text.len()) else {
        debug_assert!(false, "one mode label never fills a terminal row");
        return;
    };
    if area.width < used.saturating_add(width) {
        return;
    }
    target.set_stringn(
        area.right() - width,
        area.y,
        &position,
        usize::from(width),
        style,
    );
}

/// Renders the open prompt, or the last message, into the message line.
pub(super) fn render_message(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    prompt: Option<&PromptLine>,
    message: Option<&Message>,
) {
    if area.is_empty() {
        return;
    }
    let base = theme.style(ThemeRole::Text);
    target.set_style(area, base);
    if let Some(prompt) = prompt {
        let prefix = match prompt.kind {
            PromptKind::CommandLine => COMMAND_LINE_PREFIX,
            PromptKind::Search => SEARCH_PREFIX,
        };
        let line = format!("{prefix}{}", prompt.text);
        let (x, _) = target.set_stringn(area.x, area.y, &line, usize::from(area.width), base);
        // The terminal cursor stays hidden, so the prompt draws its own.
        if let Some(cell) = target.cell_mut((x, area.y)) {
            cell.set_style(base.patch(theme.style(ThemeRole::Cursor)));
        }
        return;
    }
    let Some(message) = message else {
        return;
    };
    let role = match message.level {
        MessageLevel::Error => ThemeRole::Error,
        MessageLevel::Warning => ThemeRole::Warning,
        MessageLevel::Info => ThemeRole::Info,
    };
    target.set_stringn(
        area.x,
        area.y,
        &message.text,
        usize::from(area.width),
        base.patch(theme.style(role)),
    );
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::shell_areas;

    #[test]
    fn the_bands_cover_the_terminal_without_a_gap() {
        for height in 0..=8u16 {
            let terminal = Rect::new(0, 0, 40, height);
            let areas = shell_areas(terminal);
            let covered = areas.body.height + areas.statusline.height + areas.message.height;
            assert_eq!(
                covered, height,
                "a terminal of {height} rows keeps every row"
            );
            assert_eq!(areas.body.y, terminal.y);
            if height > 0 {
                assert_eq!(
                    areas.message.bottom(),
                    terminal.bottom(),
                    "the message line always ends the terminal"
                );
            }
        }
    }

    #[test]
    fn a_short_terminal_drops_the_body_before_the_message_line() {
        let one = shell_areas(Rect::new(0, 0, 40, 1));
        assert_eq!(one.body.height, 0);
        assert_eq!(one.statusline.height, 0);
        assert_eq!(one.message.height, 1);
        let two = shell_areas(Rect::new(0, 0, 40, 2));
        assert_eq!(two.body.height, 0);
        assert_eq!(two.statusline.height, 1);
        let three = shell_areas(Rect::new(0, 0, 40, 3));
        assert_eq!(three.body.height, 1);
    }
}
