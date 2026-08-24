//! The which-key overlay, the language-service float, the notification overlay,
//! and the candidate list of the command line.
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
//!
//! The candidate list takes the last rows of the body band while the completion
//! of the command line offers more than one candidate. It covers neither the
//! statusline nor the message line, so the command line that it describes stays
//! visible. It draws over the notification overlay, because the user cycles it
//! with a key and reads it now. See `docs/windows.md`.

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::Style;

use kvim_input::WhichKeyRow;
use kvim_language::DiagnosticSeverity;
use kvim_settings::FileTreeIcons;
use kvim_ui::{WhichKeyHint, WhichKeyIcon, WhichKeyOverlay, WhichKeyStyles};

use super::cells::{text_cells, truncate_cells_left, wrap_cells};
use super::completion::{CompletionOutcome, LineCompletion};
use super::icons::Icon;
use super::language::{FLOAT_COLUMNS_MAX, FLOAT_ROWS_MAX, Float, FloatContent, FloatRow};
use super::markup::{FloatLine, FloatStyle, markup_lines};
use super::notify::NotificationRow;
use super::theme::{Theme, ThemeRole};

/// The number of rows that the overlay title occupies.
const TITLE_ROWS: u16 = 1;

/// The title of the which-key overlay.
const OVERLAY_TITLE: &str = " Which Key ";

/// The number of cells that a float keeps beside its widest row.
///
/// The text of a row starts one cell inside the float, and one further cell
/// stays free at the right edge, so the surface color frames the text.
const FLOAT_PADDING_CELLS: usize = 2;

/// The text that replaces the last row of an overlay that hides rows.
///
/// The float and the candidate list both bound their height, so a long answer
/// and a long candidate set lose rows. The note reports the loss instead of
/// letting the overlay end without a sign.
const OVERFLOW_NOTE: &str = "...";

/// The largest number of rows that the candidate list shows.
///
/// The list covers the buffer text while the user reads the command line, so a
/// long candidate set never fills the terminal. The list reports the candidates
/// that it hides. See `docs/windows.md`.
const COMPLETION_ROWS_MAX: usize = 8;

/// The largest number of cells that the candidate list occupies.
///
/// A command name is short, and a path candidate is long, so the bound keeps a
/// wide list off the buffer text beside it. A narrower body band bounds the
/// list further.
const COMPLETION_COLUMNS_MAX: u16 = 48;

/// The number of cells that the candidate list keeps beside its text.
///
/// The left cell puts a candidate above the text of the command line, which
/// follows the `:` prefix. The right cell frames the text with the same surface
/// color.
const COMPLETION_PADDING_CELLS: u16 = 1;

/// The number of cells that the padding of both sides occupies.
const COMPLETION_PADDING_TOTAL: u16 = COMPLETION_PADDING_CELLS.saturating_mul(2);

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
/// `kvim-ui` owns the bounded overlay, its column layout, and its clipping.
/// This function is the theme adapter: it resolves every row into its final
/// texts, it selects the icon of the command group, and it names the palette
/// colors of the surface, of the title, and of the keys. The one file-tree icon
/// setting also turns these icons off, and the columns stay aligned without
/// them. See `docs/input-actions.md`.
pub(super) fn render_which_key(
    target: &mut CellBuffer,
    body: Rect,
    theme: Theme,
    rows: &[WhichKeyRow],
    icons: FileTreeIcons,
) {
    let texts: Vec<(String, String, Option<Icon>)> = rows
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
    let hints: Vec<WhichKeyHint<'_>> = texts
        .iter()
        .map(|(key, label, icon)| {
            let hint = WhichKeyHint::new(key, label);
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
        title,
        key: title,
    };
    let Ok(overlay) = WhichKeyOverlay::new(OVERLAY_TITLE, &hints, styles) else {
        debug_assert!(
            false,
            "the registry bounds every command label, so one level of hints stays inside the overlay bounds"
        );
        return;
    };
    overlay.render(target, body);
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

/// Renders the candidate list of the command-line completion.
///
/// The list takes the last rows of the body band, so the statusline and the
/// message line below it stay visible and the user still reads the command line
/// that the list describes. It covers the buffer text, so it blanks its
/// rectangle first.
///
/// A completion that offers one candidate needs no choice, so it paints no
/// list. See `docs/windows.md`.
pub(super) fn render_completion(
    target: &mut CellBuffer,
    body: Rect,
    theme: Theme,
    completion: &LineCompletion,
) {
    match completion.outcome() {
        // One candidate answers the line alone, and no candidate changes it, so
        // neither outcome needs a choice from the user.
        CompletionOutcome::Missed | CompletionOutcome::Completed => return,
        CompletionOutcome::Listed => {}
    }
    if body.is_empty() {
        return;
    }
    let candidates = completion.candidates();
    // The row bound applies before the measurement, so a candidate that the
    // list never shows cannot widen it.
    let rows = candidates
        .len()
        .min(usize::from(body.height).min(COMPLETION_ROWS_MAX));
    let hidden = rows < candidates.len();
    // A clipped list spends its last row on the note, so the note never hides a
    // candidate without reporting the loss.
    let shown = if hidden { rows - 1 } else { rows };
    let first = completion_first_row(candidates.len(), completion.selected_row(), shown);
    let Some(painted) = candidates.get(first..first + shown) else {
        debug_assert!(
            false,
            "the window start keeps the shown rows inside the list"
        );
        return;
    };

    let text_cells_max = painted
        .iter()
        .map(|candidate| text_cells(candidate))
        .chain(hidden.then(|| text_cells(OVERFLOW_NOTE)))
        .max()
        .unwrap_or(0);
    let width = u16::try_from(text_cells_max)
        .unwrap_or(u16::MAX)
        .saturating_add(COMPLETION_PADDING_TOTAL)
        .clamp(1, body.width.min(COMPLETION_COLUMNS_MAX));
    let Ok(height) = u16::try_from(rows) else {
        debug_assert!(false, "the row bound keeps the list height small");
        return;
    };
    let area = Rect::new(body.x, body.bottom() - height, width, height);
    // A row that is wider than the list loses its start at this budget. The
    // file name at the end of a path names the file that the user looks for,
    // and every row of one path list starts with the same command name. The
    // budget counts terminal cells, so the clip never splits a wide character.
    // See `docs/windows.md`.
    let budget = usize::from(width.saturating_sub(COMPLETION_PADDING_TOTAL));
    let x = area.x.saturating_add(COMPLETION_PADDING_CELLS);
    let surface = theme.style(ThemeRole::Surface);
    let selected = surface.patch(theme.style(ThemeRole::PopupSelection));
    fill(target, area, " ");
    target.set_style(area, surface);
    for (offset, candidate) in painted.iter().enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            debug_assert!(false, "the row bound keeps the index small");
            break;
        };
        let y = area.y.saturating_add(offset);
        let style = if first + usize::from(offset) == completion.selected_row() {
            // The selected candidate is the text that the command line shows,
            // so its row carries the selection color of a popup list.
            target.set_style(Rect::new(area.x, y, area.width, 1), selected);
            selected
        } else {
            surface
        };
        let row = truncate_cells_left(candidate, budget);
        target.set_stringn(x, y, row, budget, style);
    }
    if !hidden {
        return;
    }
    target.set_stringn(
        x,
        area.bottom().saturating_sub(1),
        OVERFLOW_NOTE,
        budget,
        surface,
    );
}

/// Returns the first candidate that the bounded list shows.
///
/// The function is pure: `candidates` counts the candidates of the completion,
/// `selected` names the candidate that the command line shows, and `shown`
/// counts the rows that the list spends on candidates.
///
/// The shown candidates always hold the selected one, so a cycle past the last
/// shown row moves the window instead of hiding the selection. The window stays
/// at the end of the list once it reaches it, so the last rows never repeat a
/// candidate.
fn completion_first_row(candidates: usize, selected: usize, shown: usize) -> usize {
    debug_assert!(
        selected < candidates || candidates == 0,
        "the completion keeps its selection inside its candidate list"
    );
    let Some(last_start) = candidates.checked_sub(shown) else {
        return 0;
    };
    let Some(first) = selected.checked_sub(shown.saturating_sub(1)) else {
        return 0;
    };
    first.min(last_start)
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
    use ratatui::buffer::Buffer as CellBuffer;
    use ratatui::layout::{Position, Rect, Size};

    use crate::completion::{CompletionCycle, LineCompletion};
    use crate::theme::{Theme, ThemeRole};

    use super::{
        COMPLETION_ROWS_MAX, OVERFLOW_NOTE, completion_first_row, float_area, render_completion,
        text_cells,
    };

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

    /// The character bound of a prompt that accepts every test candidate.
    const CHARS_MAX: usize = 32;

    /// The candidates of a completion that is longer than the row bound.
    const MANY: [&str; 12] = [
        "c00", "c01", "c02", "c03", "c04", "c05", "c06", "c07", "c08", "c09", "c10", "c11",
    ];

    /// Opens one completion over `candidates`, with the first one selected.
    fn completion(candidates: &[&str]) -> LineCompletion {
        let candidates = candidates.iter().map(|text| (*text).to_owned()).collect();
        LineCompletion::open("c", candidates, CHARS_MAX, CompletionCycle::Next)
            .expect("the test offers at least one candidate")
    }

    /// Renders one candidate list over the body band `body`.
    fn draw_completion(body: Rect, completion: &LineCompletion) -> CellBuffer {
        let mut target = CellBuffer::empty(body);
        render_completion(&mut target, body, Theme::new(), completion);
        target
    }

    /// Returns one rendered row as text, without the trailing blanks.
    ///
    /// A wide character owns two cells, and the cell buffer fills the second one
    /// with a blank, so the scan skips it. The result then reads as the terminal
    /// shows the row.
    fn row_of(target: &CellBuffer, y: u16) -> String {
        let area = *target.area();
        let mut text = String::new();
        let mut tail = 0;
        for x in area.x..area.right() {
            let Some(cell) = target.cell((x, y)) else {
                continue;
            };
            if tail > 0 {
                tail -= 1;
                continue;
            }
            tail = text_cells(cell.symbol()).saturating_sub(1);
            text.push_str(cell.symbol());
        }
        text.trim_end().to_owned()
    }

    /// Returns the row of the list that carries the selection color.
    fn selected_row_of(target: &CellBuffer) -> Option<u16> {
        let area = *target.area();
        let selected = Theme::new().style(ThemeRole::PopupSelection).bg;
        (area.y..area.bottom()).find(|y| {
            target
                .cell((area.x, *y))
                .is_some_and(|cell| Some(cell.bg) == selected)
        })
    }

    #[test]
    fn the_candidate_list_bounds_its_rows_and_reports_the_hidden_candidates() {
        let body = Rect::new(0, 0, 20, 20);
        let target = draw_completion(body, &completion(&MANY));

        // The list ends at the last body row, so the statusline and the message
        // line below the body stay visible.
        let first = body.bottom() - u16::try_from(COMPLETION_ROWS_MAX).expect("the bound is small");
        let rows: Vec<String> = (first..body.bottom()).map(|y| row_of(&target, y)).collect();
        assert_eq!(rows.len(), COMPLETION_ROWS_MAX);
        // The last row reports the candidates that the bound hides, so no
        // candidate disappears without a note.
        assert_eq!(rows[COMPLETION_ROWS_MAX - 1], format!(" {OVERFLOW_NOTE}"));
        for (offset, row) in rows[..COMPLETION_ROWS_MAX - 1].iter().enumerate() {
            assert_eq!(
                row,
                &format!(" {}", MANY[offset]),
                "row {offset} of the list"
            );
        }
        // The row above the list keeps the text below it, so the list covers the
        // last rows of the body alone.
        assert_eq!(row_of(&target, first - 1), "");
    }

    #[test]
    fn the_candidate_list_moves_its_rows_with_the_selection() {
        let body = Rect::new(0, 0, 20, 20);
        let mut open = completion(&MANY);
        let first = body.bottom() - u16::try_from(COMPLETION_ROWS_MAX).expect("the bound is small");
        assert_eq!(selected_row_of(&draw_completion(body, &open)), Some(first));

        // Seven rows hold candidates, and the eighth holds the note, so the
        // seventh cycle still reaches the last of those rows.
        for _ in 0..6 {
            open.cycle(CompletionCycle::Next);
        }
        let target = draw_completion(body, &open);
        assert_eq!(selected_row_of(&target), Some(first + 6));
        assert_eq!(row_of(&target, first), format!(" {}", MANY[0]));

        // The next cycle leaves no row for the selection, so the shown
        // candidates move instead of hiding it.
        open.cycle(CompletionCycle::Next);
        let target = draw_completion(body, &open);
        assert_eq!(selected_row_of(&target), Some(first + 6));
        assert_eq!(row_of(&target, first), format!(" {}", MANY[1]));
        assert_eq!(row_of(&target, first + 6), format!(" {}", MANY[7]));
        assert_eq!(row_of(&target, first + 7), format!(" {OVERFLOW_NOTE}"));
    }

    #[test]
    fn the_candidate_list_clips_the_start_of_a_wide_candidate_without_splitting_it() {
        // The list keeps one cell beside its text, so a body of seven cells
        // leaves five for the candidate. The marker takes one of those cells,
        // and the wide character no longer fits in the four that remain.
        let body = Rect::new(0, 0, 7, 4);
        let target = draw_completion(body, &completion(&["\u{6e2c}\u{8a66}abc", "ab"]));
        let row = row_of(&target, body.bottom() - 2);
        // The end of the candidate always survives, because the file name at
        // the end of a path names the file that the user looks for.
        assert_eq!(row, " <abc");
        // The cell that the wide character would have split stays blank.
        let cell = target
            .cell((5, body.bottom() - 2))
            .expect("the cell is inside");
        assert_eq!(cell.symbol(), " ");
    }

    #[test]
    fn one_candidate_opens_no_list() {
        let body = Rect::new(0, 0, 20, 20);
        let target = draw_completion(body, &completion(&["only"]));
        for y in body.y..body.bottom() {
            assert_eq!(row_of(&target, y), "", "row {y} stays empty");
        }
    }

    #[test]
    fn the_shown_candidates_always_hold_the_selection() {
        // The window start is the pure rule behind the moving rows above, so it
        // answers every selection of one bounded list.
        for shown in 1..=4usize {
            for selected in 0..12usize {
                let first = completion_first_row(12, selected, shown);
                assert!(
                    first <= selected && selected < first + shown,
                    "the window [{first}, {}) holds {selected}",
                    first + shown
                );
                assert!(first + shown <= 12, "the window stays inside the list");
            }
        }
    }
}
