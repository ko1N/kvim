//! The terminal-wide chrome: the shell bands, the statusline, and the message
//! line.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! kvim shows one statusline and one message line for the whole terminal, and
//! one winbar for each window. Regions carry no divider glyph: the surface band
//! of the winbar and of the statusline separates them by color, as ReviewGraph
//! does. See `docs/windows.md`.

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect};

use kvim_editor::Cursor;
use kvim_input::Mode;
use kvim_ui::{BandRank, BandSegment, ChromeBand, LineInput, LineInputStyles};

use super::embed::EditorPresentation;
use super::language::FormatOnSave;
use super::session::{Message, MessageLevel, PromptLine};
use super::theme::{Theme, ThemeRole};

/// How long the mode survives a narrow statusline.
///
/// The mode always survives, because it decides what the next key does.
const MODE_RANK: BandRank = BandRank::new(2);

/// How long the cursor position survives a narrow statusline.
const POSITION_RANK: BandRank = BandRank::new(1);

/// How long the format-on-save state survives a narrow statusline.
///
/// The state sheds first, because the position moves with every key.
const STATE_RANK: BandRank = BandRank::new(0);

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

impl ShellAreas {
    /// Returns the rectangle that a popup above the command line may draw
    /// into.
    ///
    /// The body and the statusline are contiguous for every realized
    /// presentation, including a zero-height host-owned statusline.
    pub(super) fn above_command_line(&self) -> Rect {
        debug_assert_eq!(
            self.body.bottom(),
            self.statusline.y,
            "shell_areas keeps the body and the statusline contiguous"
        );
        Rect::new(
            self.body.x,
            self.body.y,
            self.body.width,
            self.body.height + self.statusline.height,
        )
    }
}

/// Splits the accepted rectangle according to fixed presentation ownership.
pub(super) fn shell_areas(area: Rect, presentation: EditorPresentation) -> ShellAreas {
    let statusline_rows = u16::from(presentation.statusline_embedded());
    let message_rows = u16::from(presentation.command_line_embedded());
    let chrome_rows = statusline_rows + message_rows;
    let body_height = area.height.saturating_sub(chrome_rows);
    let body = Rect::new(area.x, area.y, area.width, body_height);
    let statusline = if statusline_rows == 0 || area.height <= message_rows {
        Rect::new(area.x, body.bottom(), area.width, 0)
    } else {
        Rect::new(area.x, body.bottom(), area.width, 1)
    };
    let message = if message_rows == 0 || area.height == 0 {
        Rect::new(area.x, statusline.bottom(), area.width, 0)
    } else {
        Rect::new(area.x, area.bottom() - 1, area.width, 1)
    };
    ShellAreas {
        body,
        statusline,
        message,
    }
}

/// Draws one band of ranked parts, each in the theme role that the caller
/// named.
///
/// The band answers which parts a narrow row keeps and where every kept part
/// sits, so no caller repeats the shedding rule. The caller owns the text and
/// the color, because the band names neither. See `docs/windows.md`.
pub(super) fn draw_band(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    parts: &[(ThemeRole, BandSegment<'_>)],
) {
    debug_assert!(
        parts
            .iter()
            .enumerate()
            .all(|(index, (_, segment))| !parts[..index]
                .iter()
                .any(|(_, earlier)| earlier == segment)),
        "each part carries its own rank, so no two parts of one band are equal"
    );
    let Ok(band) = ChromeBand::new(parts.iter().map(|(_, segment)| *segment).collect()) else {
        debug_assert!(
            false,
            "every band of the editor lists fewer parts than the bound"
        );
        return;
    };
    for placement in band.placements(area) {
        // The rank orders the shed and never names a part, so the role comes
        // from the list that the caller wrote beside the segments.
        let Some((role, _)) = parts
            .iter()
            .find(|(_, segment)| *segment == placement.segment)
        else {
            debug_assert!(false, "every placement repeats one listed segment");
            continue;
        };
        target.set_stringn(
            placement.area.x,
            placement.area.y,
            placement.segment.text,
            usize::from(placement.area.width),
            theme.style(*role),
        );
    }
}

/// Whether the statusline shows its parts or its background alone.
///
/// The command-line candidate list ends on the statusline row and covers only
/// the cells that its own width reaches, so a part beside it would survive at
/// some terminal widths and not at others. The statusline therefore decides
/// its own visibility as one fact for the whole row: every part hides while a
/// list is open, at every width, and the row keeps its background so it never
/// reads as a gap. See `docs/windows.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StatuslineParts {
    /// The statusline shows the mode, the format-on-save state, and the
    /// cursor position.
    Shown,
    /// The statusline shows its background alone, because a candidate list
    /// covers the row.
    Hidden,
}

/// Renders the mode, the format-on-save state, and the cursor position into
/// the statusline band.
///
/// A band that cannot hold every part drops them in a fixed order: the
/// format-on-save state first, then the cursor position. The mode always
/// survives, because the mode decides what the next key does. The band of
/// `kvim-ui` holds that rule, so a host that ranks its own parts sheds them
/// the same way.
///
/// A buffer that no formatter can format reports no state at all, so `format`
/// is `None` there and the band shows the mode and the position alone.
///
/// `parts` names whether the row shows those parts at all. The statusline
/// always paints its own background first, so a caller that passes
/// [`StatuslineParts::Hidden`] still gets a row that reads as chrome, not as a
/// gap.
pub(super) fn render_statusline(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    mode: Mode,
    cursor: Cursor,
    format: Option<FormatOnSave>,
    parts: StatuslineParts,
) {
    if area.is_empty() {
        return;
    }
    target.set_style(area, theme.style(ThemeRole::Statusline));
    if parts == StatuslineParts::Hidden {
        return;
    }

    let mode_text = format!(" {mode} ");
    let position = format!("{}:{} ", cursor.line().get() + 1, cursor.column().get() + 1);
    // The mode label and the state label each end with one blank, so the
    // leading blank of the state keeps a wide mode name apart from it.
    let state = format.map(|state| format!(" {} ", state.label()));

    // The title role already carries the surface band, so the mode reads as
    // chrome on the same background as the rest of the statusline. The state
    // answers a question the reader asks once, so it stays quiet beside the
    // mode. See `docs/windows.md`.
    let mut parts = vec![(ThemeRole::Title, BandSegment::left(&mode_text, MODE_RANK))];
    if let Some(state) = &state {
        parts.push((
            ThemeRole::StatuslineMuted,
            BandSegment::right(state, STATE_RANK),
        ));
    }
    parts.push((
        ThemeRole::Statusline,
        BandSegment::right(&position, POSITION_RANK),
    ));
    draw_band(target, area, theme, &parts);
}

/// Renders the open prompt or the last message into the message line.
///
/// A modal confirmation is a body overlay, so it does not replace either
/// message-line entry.
///
/// The message line holds no shedding rule and therefore no band. It shows one
/// entry and clips it to the row, so it never chooses between parts. A band
/// here would invent a rule that kvim does not have. See `docs/windows.md`.
pub(super) fn render_message(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    prompt: Option<&PromptLine>,
    message: Option<&Message>,
) -> Option<Position> {
    if area.is_empty() {
        return None;
    }
    let base = theme.style(ThemeRole::Text);
    if let Some(prompt) = prompt {
        let result = LineInput::render(
            target,
            area,
            prompt.kind.prefix(),
            prompt.line.text(),
            prompt.line.cursor_offset(),
            LineInputStyles {
                field: base,
                prefix: base,
                text: base,
            },
        );
        debug_assert!(
            result.is_ok(),
            "session prompt bounds and shell geometry satisfy LineInput"
        );
        return result.ok();
    }
    target.set_style(area, base);
    let Some(message) = message else {
        return None;
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
    None
}

#[cfg(test)]
#[path = "chrome_tests.rs"]
mod tests;
