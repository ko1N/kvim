//! The picker overlay: the visible state, its transitions, its layout, and its
//! rows.
//!
//! The overlay owns one [`Picker`], the bounded requests that fill it, and the
//! preview of the selected row. It performs no filesystem work and starts no
//! process. Every candidate list and every preview becomes one
//! [`PickerRequest`] that the event loop hands to the bounded worker or process
//! service, and the typed result returns through one transition. See
//! `docs/files.md` and `docs/responsiveness.md`.
//!
//! The layout follows the reference configuration: the picker covers the
//! complete terminal, the prompt sits at the top, the results ascend from the
//! prompt with the best match first, and the preview receives
//! [`PREVIEW_WIDTH_PERCENT`] of the width. No region carries a divider glyph.
//! One blank row and one blank column separate them. See `docs/windows.md`.

use std::sync::Arc;

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use kvim_input::PromptKind;
use kvim_path::WorktreeRoot;
use kvim_workspace::{
    Acceptance, Candidate, Picker, PickerKind, PickerRequest, PickerResult, PickerSlot, Preview,
    PreviewKey,
};

use super::session::Redraw;
use super::theme::{Theme, ThemeRole};

/// The share of the picker width that the preview receives, in percent.
pub(super) const PREVIEW_WIDTH_PERCENT: u16 = 75;

/// The smallest preview column that the layout keeps, in cells.
///
/// A narrower column shows no readable source line, so the results take the
/// complete width instead.
pub(super) const PREVIEW_MIN_CELLS: u16 = 24;

/// The smallest result column that the layout keeps beside a preview, in cells.
pub(super) const RESULTS_MIN_CELLS: u16 = 16;

/// The number of rows that the prompt occupies.
const PROMPT_ROWS: u16 = 1;

/// The number of rows between the prompt and the first result.
const GAP_ROWS: u16 = 1;

/// The number of cells between the result column and the preview column.
const GAP_CELLS: u16 = 1;

/// The smallest query that starts one search.
const SEARCH_CHARS_MIN: usize = 1;

/// The message that a picker without a matching row shows.
const NO_RESULT_NOTE: &str = "no result";

/// The message that a truncated candidate list shows.
const TRUNCATED_NOTE: &str = "the result list stops at an editor limit";

/// The message that a failed preview shows.
const NO_PREVIEW_NOTE: &str = "no preview";

/// The message that a clipped preview shows.
const PREVIEW_TRUNCATED_NOTE: &str = "preview stops at an editor limit";

/// The message that a missing ripgrep command shows.
pub(super) const RIPGREP_MISSING_NOTE: &str =
    "the `rg` command is not available; the search picker stays empty";

/// The reason that one picker request produced no result.
///
/// The event loop maps every runtime failure onto one of these values, so the
/// picker never reads an error message text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PickerFailure {
    /// The bounded runtime held no free permit or result slot.
    Saturated,
    /// A newer request or the shutdown cancelled this request.
    Cancelled,
    /// The operation passed its deadline.
    Timeout,
    /// The external command could not start.
    CommandMissing,
}

impl PickerFailure {
    /// Returns the message that the picker shows, or `None` for a normal state.
    ///
    /// A cancelled request is normal: a newer query replaces its result, so the
    /// reader needs no message.
    const fn message(self) -> Option<&'static str> {
        match self {
            Self::Saturated => Some("the editor is busy; type again to search"),
            Self::Timeout => Some("the search passed its deadline"),
            Self::CommandMissing => Some(RIPGREP_MISSING_NOTE),
            Self::Cancelled => None,
        }
    }
}

/// The rectangles of one open picker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PickerAreas {
    /// The row that holds the query.
    pub(super) prompt: Rect,
    /// The rows that hold the results, with the best match first.
    pub(super) results: Rect,
    /// The preview column, or `None` in a narrow terminal.
    pub(super) preview: Option<Rect>,
}

/// Splits the terminal into the regions of one picker.
///
/// The picker covers the complete terminal, so it keeps no padding on either
/// axis. A terminal that cannot hold both columns drops the preview and gives
/// the complete width to the results.
pub(super) fn picker_areas(area: Rect) -> PickerAreas {
    let preview_width = area.width.saturating_mul(PREVIEW_WIDTH_PERCENT) / 100;
    let results_width = area.width.saturating_sub(preview_width);
    let wide = preview_width >= PREVIEW_MIN_CELLS
        && results_width >= RESULTS_MIN_CELLS.saturating_add(GAP_CELLS);
    let (results_width, preview) = if wide {
        let column = results_width.saturating_sub(GAP_CELLS);
        (
            column,
            Some(Rect::new(
                area.x.saturating_add(results_width),
                area.y,
                preview_width,
                area.height,
            )),
        )
    } else {
        (area.width, None)
    };
    let prompt = Rect::new(area.x, area.y, results_width, PROMPT_ROWS.min(area.height));
    let used = PROMPT_ROWS.saturating_add(GAP_ROWS);
    let results = Rect::new(
        area.x,
        area.y.saturating_add(used).min(area.bottom()),
        results_width,
        area.height.saturating_sub(used),
    );
    PickerAreas {
        prompt,
        results,
        preview,
    }
}

/// The open picker of one editor.
#[derive(Debug)]
pub(super) struct PickerState {
    root: Arc<WorktreeRoot>,
    picker: Picker,
    /// The candidate request that waits for the event loop.
    source_outbox: Option<PickerRequest>,
    /// The preview request that waits for the event loop.
    preview_outbox: Option<PickerRequest>,
    /// The preview that the picker shows.
    preview: Option<(PreviewKey, Preview)>,
    /// The selection that the running preview describes.
    preview_pending: Option<PreviewKey>,
    /// The first visible result row.
    first_row: usize,
    /// The state that the picker reports between the prompt and the results.
    notice: Option<&'static str>,
}

impl PickerState {
    /// Opens one picker over one workspace root.
    ///
    /// The buffer picker receives its candidates at once, because the loaded
    /// buffer list needs no filesystem work. The file picker asks for one
    /// workspace walk, and the search picker waits for the first query.
    pub(super) fn open(kind: PickerKind, root: Arc<WorktreeRoot>, buffers: Vec<Candidate>) -> Self {
        let root_path = root.as_path().to_path_buf();
        let mut state = Self {
            root: Arc::clone(&root),
            picker: Picker::new(kind, root_path),
            source_outbox: None,
            preview_outbox: None,
            preview: None,
            preview_pending: None,
            first_row: 0,
            notice: None,
        };
        match kind {
            PickerKind::Files => state.source_outbox = Some(PickerRequest::Files { root }),
            PickerKind::Buffers => state.picker.set_candidates(buffers, false),
            // A search without a query would list the complete workspace.
            PickerKind::Search => {}
        }
        state.refresh_preview();
        state
    }

    /// Returns the picker model.
    pub(super) const fn picker(&self) -> &Picker {
        &self.picker
    }

    /// Returns the first visible result row.
    pub(super) const fn first_row(&self) -> usize {
        self.first_row
    }

    /// Returns the preview of the selected row.
    pub(super) const fn preview(&self) -> Option<&(PreviewKey, Preview)> {
        self.preview.as_ref()
    }

    /// Returns the picker request that the event loop must submit.
    ///
    /// The candidates come before the preview, because a preview without a
    /// selected row shows nothing.
    pub(super) fn take_request(&mut self) -> Option<PickerRequest> {
        self.source_outbox
            .take()
            .or_else(|| self.preview_outbox.take())
    }

    /// Replaces the query and starts the work that the new query needs.
    pub(super) fn set_query(&mut self, query: &str) {
        self.picker.set_query(query);
        if self.picker.kind() == PickerKind::Search {
            self.start_search();
        }
        self.publish_notice();
        self.refresh_preview();
    }

    /// Moves the selection one row away from the prompt.
    pub(super) fn select_next(&mut self) {
        self.picker.select_next();
        self.refresh_preview();
    }

    /// Moves the selection one row toward the prompt.
    pub(super) fn select_previous(&mut self) {
        self.picker.select_previous();
        self.refresh_preview();
    }

    /// Returns what the editor does with the selected row.
    pub(super) fn accept(&self) -> Option<Acceptance> {
        self.picker.accept()
    }

    /// Applies one completed picker operation as one state transition.
    ///
    /// The publication gate of the runtime rejects the result of a superseded
    /// request. This transition rejects the same result again from the visible
    /// state, so a search of an older query and a preview of an older selection
    /// never reach the screen.
    pub(super) fn apply_result(&mut self, result: PickerResult) -> Redraw {
        match result {
            PickerResult::Candidates {
                query,
                candidates,
                truncated,
            } => {
                if self.picker.kind() == PickerKind::Search && query != self.picker.query() {
                    return Redraw::Skipped;
                }
                self.picker.set_candidates(candidates, truncated);
                self.publish_notice();
                self.refresh_preview();
                Redraw::Needed
            }
            PickerResult::Preview { key, outcome } => {
                if self.preview_pending.as_ref() != Some(&key) {
                    // The reader already moved past this row.
                    return Redraw::Skipped;
                }
                self.preview_pending = None;
                match outcome {
                    Ok(preview) => {
                        let truncated = preview.truncated;
                        self.preview = Some((key, preview));
                        self.publish_notice();
                        if truncated && self.notice.is_none() {
                            self.notice = Some(PREVIEW_TRUNCATED_NOTE);
                        }
                    }
                    Err(_) => {
                        self.preview = None;
                        self.notice = Some(NO_PREVIEW_NOTE);
                    }
                }
                Redraw::Needed
            }
        }
    }

    /// Reports that one picker request produced no result.
    ///
    /// The picker keeps the candidates and the preview that it already holds,
    /// so the reader can type again.
    pub(super) fn abandon(&mut self, slot: PickerSlot, failure: PickerFailure) -> Redraw {
        match slot {
            PickerSlot::Candidates => self.source_outbox = None,
            PickerSlot::Preview => {
                self.preview_outbox = None;
                self.preview_pending = None;
            }
        }
        let Some(message) = failure.message() else {
            return Redraw::Skipped;
        };
        self.notice = Some(message);
        Redraw::Needed
    }

    /// Moves the visible rows so the selected row stays inside the picker.
    pub(super) fn reconcile(&mut self, rows_visible: usize) {
        let rows = self.picker.matches().len();
        if rows_visible == 0 || rows == 0 {
            self.first_row = 0;
            return;
        }
        let selected = self.picker.selected_row().unwrap_or(0);
        let last_start = rows.saturating_sub(rows_visible);
        self.first_row = self
            .first_row
            .min(selected)
            .max(selected.saturating_sub(rows_visible.saturating_sub(1)))
            .min(last_start);
    }

    /// Returns the state that the picker reports below its prompt.
    pub(super) fn notice(&self) -> Option<&str> {
        self.notice
    }

    /// Queues one search for the current query.
    ///
    /// A newer search makes the older search obsolete. The publication gate
    /// cancels it, which stops the running `rg` process.
    fn start_search(&mut self) {
        let query = self.picker.query().to_owned();
        if query.chars().count() < SEARCH_CHARS_MIN {
            self.picker.set_candidates(Vec::new(), false);
            self.source_outbox = None;
            return;
        }
        self.source_outbox = Some(PickerRequest::Search {
            root: Arc::clone(&self.root),
            query,
        });
    }

    /// Asks for the preview of the selected row.
    ///
    /// A selection that already holds its preview, and a selection whose
    /// preview already runs, need no further request.
    fn refresh_preview(&mut self) {
        let key =
            self.picker
                .selected()
                .and_then(Candidate::preview)
                .map(|(relative, _, target)| {
                    PreviewKey::new(Arc::clone(&self.root), relative.clone(), target)
                });
        let Some(key) = key else {
            self.preview = None;
            self.preview_pending = None;
            self.preview_outbox = None;
            return;
        };
        if self
            .preview
            .as_ref()
            .is_some_and(|(shown, _)| *shown == key)
            || self.preview_pending.as_ref() == Some(&key)
        {
            return;
        }
        self.preview = None;
        self.preview_pending = Some(key.clone());
        self.preview_outbox = Some(PickerRequest::Preview(key));
    }

    /// Reports the state of the candidate list below the prompt.
    fn publish_notice(&mut self) {
        self.notice = if self.picker.is_truncated() {
            Some(TRUNCATED_NOTE)
        } else if self.picker.matches().is_empty() {
            Some(NO_RESULT_NOTE)
        } else {
            None
        };
    }
}

/// Renders one open picker over the complete terminal.
///
/// The function returns the cell of the prompt cursor, so the terminal draws
/// its own cursor where the reader types.
pub(super) fn render_picker(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    state: &PickerState,
) -> Option<Position> {
    if area.is_empty() {
        return None;
    }
    let areas = picker_areas(area);
    let text = theme.style(ThemeRole::Text);
    let surface = theme.style(ThemeRole::Surface);
    // The picker covers every window below it, so it clears its rectangle
    // first. The blank cells also separate its regions, because no region
    // carries a divider glyph.
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = target.cell_mut((x, y)) {
                cell.reset();
            }
        }
    }
    target.set_style(area, text);

    let cursor = render_prompt(target, areas.prompt, theme, state);
    render_notice(target, &areas, theme, state);
    render_results(target, areas.results, theme, state);
    if let Some(preview) = areas.preview {
        target.set_style(Rect::new(preview.x, preview.y, preview.width, 1), surface);
        render_preview(target, preview, theme, state);
    }
    cursor
}

/// Renders the query row and returns the cell of its cursor.
fn render_prompt(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    state: &PickerState,
) -> Option<Position> {
    if area.is_empty() {
        return None;
    }
    let surface = theme.style(ThemeRole::Surface);
    target.set_style(area, surface);
    let prefix = PromptKind::Picker.prefix();
    let line = format!("{prefix}{}", state.picker().query());
    target.set_stringn(area.x, area.y, &line, usize::from(area.width), surface);
    target.set_stringn(
        area.x,
        area.y,
        prefix,
        usize::from(area.width),
        theme.style(ThemeRole::Title),
    );
    let column = u16::try_from(line.chars().count()).unwrap_or(u16::MAX);
    Some(Position::new(
        area.x
            .saturating_add(column)
            .min(area.right().saturating_sub(1)),
        area.y,
    ))
}

/// Renders the state of the result list between the prompt and the results.
fn render_notice(target: &mut CellBuffer, areas: &PickerAreas, theme: Theme, state: &PickerState) {
    let row = areas.prompt.bottom();
    if row >= areas.results.y {
        return;
    }
    let Some(notice) = state.notice() else {
        return;
    };
    target.set_stringn(
        areas.results.x,
        row,
        notice,
        usize::from(areas.results.width),
        theme.style(ThemeRole::Warning),
    );
}

/// Renders the matched rows, with the best match next to the prompt.
fn render_results(target: &mut CellBuffer, area: Rect, theme: Theme, state: &PickerState) {
    if area.is_empty() {
        return;
    }
    let selected = state.picker().selected_row();
    let width = usize::from(area.width);
    for (offset, (row, index)) in state
        .picker
        .matches()
        .iter()
        .enumerate()
        .skip(state.first_row())
        .take(usize::from(area.height))
        .enumerate()
    {
        let index = *index;
        let Ok(offset) = u16::try_from(offset) else {
            debug_assert!(false, "the visible rows never pass the terminal height");
            return;
        };
        let Some(candidate) = state.picker().candidate(index) else {
            debug_assert!(false, "every matched index names one candidate");
            continue;
        };
        let y = area.y.saturating_add(offset);
        let style = if selected == Some(row) {
            theme
                .style(ThemeRole::Text)
                .patch(theme.style(ThemeRole::PopupSelection))
        } else {
            theme.style(ThemeRole::Text)
        };
        target.set_style(Rect::new(area.x, y, area.width, 1), style);
        target.set_stringn(area.x, y, candidate.row(), width, style);
        // The filename stands before its directory, so it carries the title
        // color and the reader finds it first. It keeps the background of its
        // row, so the selection covers the complete row.
        let name = Style {
            bg: style.bg,
            ..theme.style(ThemeRole::Title)
        };
        target.set_stringn(area.x, y, candidate.name(), width, name);
    }
}

/// Renders the preview of the selected row.
fn render_preview(target: &mut CellBuffer, area: Rect, theme: Theme, state: &PickerState) {
    let Some((key, preview)) = state.preview() else {
        return;
    };
    let name = key.path().file_name().map_or_else(
        || key.path().display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let title = format!(" {name} ");
    target.set_stringn(
        area.x,
        area.y,
        &title,
        usize::from(area.width),
        theme.style(ThemeRole::Title),
    );
    let body = Rect::new(
        area.x,
        area.y.saturating_add(PROMPT_ROWS.saturating_add(GAP_ROWS)),
        area.width,
        area.height
            .saturating_sub(PROMPT_ROWS.saturating_add(GAP_ROWS)),
    );
    for (offset, line) in preview
        .lines
        .iter()
        .take(usize::from(body.height))
        .enumerate()
    {
        let Ok(offset) = u16::try_from(offset) else {
            debug_assert!(false, "the preview holds fewer lines than the terminal");
            return;
        };
        let number = preview.first_line.saturating_add(usize::from(offset));
        // Only a search row marks one line, so a file preview shows plain text.
        let style = if key.target().marks(number) {
            theme
                .style(ThemeRole::Text)
                .patch(theme.style(ThemeRole::CurrentSearchMatch))
        } else {
            theme.style(ThemeRole::Text)
        };
        let y = body.y.saturating_add(offset);
        target.set_style(Rect::new(body.x, y, body.width, 1), style);
        target.set_stringn(
            body.x.saturating_add(1),
            y,
            line,
            usize::from(body.width.saturating_sub(1)),
            style,
        );
    }
}
