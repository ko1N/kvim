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

use std::fmt::Write as _;

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::Style;

use kvim_input::{Key, WhichKeyRow};
use kvim_language::DiagnosticSeverity;
use kvim_settings::FileTreeIcons;
use kvim_ui::{
    WhichKeyFooter, WhichKeyIcon, WhichKeyLegendEntry, WhichKeyOverlay, WhichKeyOverlayRow,
    WhichKeyStyles,
};

use super::cells::{text_cells, wrap_cells};
use super::icons::Icon;
use super::language::{FLOAT_COLUMNS_MAX, FLOAT_ROWS_MAX, Float, FloatContent, FloatRow};
use super::markup::{FloatLine, FloatStyle, markup_lines};
use super::notify::NotificationRow;
use super::theme::{Theme, ThemeRole};

/// The number of rows that the overlay title occupies.
const TITLE_ROWS: u16 = 1;

/// The marker between two keys of the which-key breadcrumb.
///
/// The breadcrumb reads as the path that the reader walked, so the marker
/// points from one key to the next.
const BREADCRUMB_MARKER: &str = " » ";

/// The keys that navigate the which-key overlay itself.
///
/// The two entries name the keys that leave the overlay, not the keys that
/// reach a command, so they stand apart from the hint rows above them. Both
/// glyphs occupy one terminal cell, so the footer needs no patched font.
const WHICH_KEY_LEGEND: [WhichKeyLegendEntry<'static>; 2] = [
    WhichKeyLegendEntry {
        key: "ESC",
        action: "close",
    },
    WhichKeyLegendEntry {
        key: "⌫",
        action: "back",
    },
];

/// The number of cells that a float keeps beside its widest row.
///
/// The text of a row starts one cell inside the float, and one further cell
/// stays free at the right edge, so the surface color frames the text.
const FLOAT_PADDING_CELLS: usize = 2;

/// The text that replaces the last row of an overlay that hides rows.
///
/// The float and the candidate menu both bound their height, so a long answer
/// and a long candidate set lose rows. The note reports the loss instead of
/// letting the overlay end without a sign.
pub(super) const OVERFLOW_NOTE: &str = "...";

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

/// The visible state of one open which-key overlay.
///
/// The overlay answers one pending key sequence, so its hint rows and the keys
/// that reached them are one fact. The two travel together, and an overlay
/// that is absent carries neither.
#[derive(Clone, Copy, Debug)]
pub(super) struct WhichKeyView<'a> {
    /// The keys that may follow the pending sequence, one level at a time.
    pub(super) rows: &'a [WhichKeyRow],
    /// The keys that the reader already pressed, in press order.
    pub(super) pending: &'a [Key],
}

/// Returns the breadcrumb of one pending key sequence.
///
/// Every key writes its help form, and one marker separates two keys, so the
/// footer reads as the path that the reader walked. [`Key::label`] is the one
/// key-label rule of the workspace, so a breadcrumb key and a hint key can
/// never disagree. An empty sequence writes an empty text.
///
/// The result is bounded without a bound of its own: the settings reject a
/// pending-key limit above `PENDING_KEYS_MAX`, and the resolver holds no more
/// keys than that limit, so the text stays far inside the text bound of the
/// widget.
fn breadcrumb(pending: &[Key]) -> String {
    let mut text = String::new();
    for key in pending {
        if !text.is_empty() {
            text.push_str(BREADCRUMB_MARKER);
        }
        // Writing into a `String` never fails, so the result carries no case
        // that this render could answer.
        let _ = write!(text, "{}", key.label());
    }
    text
}

/// Renders the which-key overlay at the bottom of the body band.
///
/// `kvim-ui` owns the bounded overlay, its column layout, and its clipping.
/// This function is the theme adapter: it resolves every row into its final
/// texts, it selects the icon of the command group, it builds the breadcrumb of
/// the pressed keys and the legend of the navigation keys, and it names the
/// palette colors of the surface, of the footer, and of the keys. The one
/// file-tree icon setting also turns these icons off, and the columns stay
/// aligned without them. See `docs/input-actions.md`.
pub(super) fn render_which_key(
    target: &mut CellBuffer,
    body: Rect,
    theme: Theme,
    view: WhichKeyView<'_>,
    icons: FileTreeIcons,
) {
    let texts: Vec<(String, String, Option<Icon>)> = view
        .rows
        .iter()
        .map(|row| {
            (
                row.key_label().to_string(),
                row.target.to_string(),
                (icons == FileTreeIcons::Shown).then(|| Icon::of_group(row.group)),
            )
        })
        .collect();
    let surface = theme.style(ThemeRole::Surface);
    let hints: Vec<WhichKeyOverlayRow<'_>> = texts
        .iter()
        .map(|(key, label, icon)| {
            let hint = WhichKeyOverlayRow::new(key, label);
            match icon {
                // The icon keeps the surface background, so only its foreground
                // color separates one command group from the next.
                Some(icon) => hint.with_icon(WhichKeyIcon {
                    glyph: icon.glyph,
                    style: surface.patch(theme.style(ThemeRole::Icon(icon.role))),
                }),
                None => hint,
            }
        })
        .collect();
    // The keys carry the title color, so a reader finds the next key first.
    let title = theme.style(ThemeRole::Title);
    let styles = WhichKeyStyles {
        surface,
        key: title,
        note: title,
        breadcrumb: theme.style(ThemeRole::WhichKeyBreadcrumb),
        legend_key: theme.style(ThemeRole::WhichKeyLegendKey),
        legend_action: theme.style(ThemeRole::WhichKeyLegendAction),
    };
    let breadcrumb = breadcrumb(view.pending);
    let footer = WhichKeyFooter {
        breadcrumb: &breadcrumb,
        legend: &WHICH_KEY_LEGEND,
    };
    let Ok(overlay) = WhichKeyOverlay::new(footer, &hints, styles) else {
        debug_assert!(
            false,
            "the registry bounds every command label, so one level of hints stays inside the overlay bounds"
        );
        return;
    };
    if overlay.render(target, body).is_err() {
        debug_assert!(
            false,
            "the body band of one frame names cells of the cell buffer of that frame"
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
    if window.is_empty() || float.is_empty() {
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
    let mut lines = float_lines(float, budget);
    // The row bound applies before the measurement, so a row that the float
    // never shows cannot widen it.
    fit(&mut lines, FLOAT_ROWS_MAX);
    // No row reaches past the budget, so the float keeps its column bound even
    // while one row of it loses its end.
    let text_width = lines
        .iter()
        .map(FloatLine::cells)
        .max()
        .unwrap_or(0)
        .min(budget);
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
        let mut column = area.x.saturating_add(1);
        for span in &row.spans {
            let Some(available) = area.right().checked_sub(column).filter(|cells| *cells > 0)
            else {
                break;
            };
            target.set_stringn(
                column,
                area.y + TITLE_ROWS + offset,
                &span.text,
                usize::from(available),
                span_style(theme, surface, span.style),
            );
            let Ok(cells) = u16::try_from(text_cells(&span.text)) else {
                debug_assert!(false, "the column bound keeps one row short");
                break;
            };
            column = column.saturating_add(cells);
        }
    }
}

/// Returns the style of one piece of one float row.
///
/// A piece without a role of its own paints in the surface style of the float.
/// Every other role decorates that style, so the surface band stays behind the
/// text. See `docs/windows.md`.
fn span_style(theme: Theme, surface: Style, style: FloatStyle) -> Style {
    match style {
        FloatStyle::Plain => surface,
        FloatStyle::Severity(severity) => surface.patch(theme.style(severity_role(severity))),
        FloatStyle::Markup(role) => surface.patch(theme.style(ThemeRole::Markup(role))),
        FloatStyle::Syntax(role) => surface.patch(theme.style(ThemeRole::Syntax(role))),
        FloatStyle::Structure => surface.patch(theme.style(ThemeRole::MarkupStructure)),
    }
}

/// Returns the rows that one float paints at one width.
///
/// The result holds at most one row beyond [`FLOAT_ROWS_MAX`], because [`fit`]
/// needs to see only that further rows exist. A float that already lost content
/// carries one further row, so [`fit`] always ends it with the note.
pub(super) fn float_lines(float: &Float, cells: usize) -> Vec<FloatLine> {
    let mut lines = match &float.content {
        FloatContent::Text(rows) => wrap_text_rows(rows, cells),
        FloatContent::Markup(document) => markup_lines(document, cells),
    };
    if float.is_clipped() {
        lines.push(FloatLine::new(OVERFLOW_NOTE, FloatStyle::Plain));
    }
    lines
}

/// Wraps every source row of one plain float into rows of at most `cells`
/// cells.
fn wrap_text_rows(rows: &[FloatRow], cells: usize) -> Vec<FloatLine> {
    let mut lines: Vec<FloatLine> = Vec::new();
    for row in rows {
        for text in wrap_cells(&row.text, cells) {
            if lines.len() > FLOAT_ROWS_MAX {
                return lines;
            }
            lines.push(FloatLine::new(text, FloatStyle::of_severity(row.severity)));
        }
    }
    lines
}

/// Shortens the rows of one float to `shown` rows.
///
/// A float that holds more rows than fit keeps its first rows and reports the
/// missing ones in the last one, so no row disappears without a note.
fn fit(lines: &mut Vec<FloatLine>, shown: usize) {
    debug_assert!(shown >= 1, "the caller returns before an empty float");
    if lines.len() <= shown {
        return;
    }
    lines.truncate(shown);
    let Some(last) = lines.last_mut() else {
        debug_assert!(false, "the truncation keeps at least one row");
        return;
    };
    *last = FloatLine::new(OVERFLOW_NOTE, FloatStyle::Plain);
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
pub(super) fn fill(target: &mut CellBuffer, area: Rect, symbol: &str) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = target.cell_mut((x, y)) {
                cell.set_symbol(symbol);
            }
        }
    }
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
