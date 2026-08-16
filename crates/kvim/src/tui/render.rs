//! The one frame builder of the editor.
//!
//! The builder reads visible state and writes terminal cells. It performs no
//! input, no output, and no state change, so a frame is a pure function of the
//! session. See `docs/responsiveness.md`.

use ratatui::Frame;

use crate::editor::{ColumnLimit, Cursor};

use super::buffer_view::{WindowFocus, WindowView, cursor_cell, render_window};
use super::chrome::{render_message, render_statusline, shell_areas};
use super::layout::RegionKind;
use super::overlay::{render_float, render_which_key};
use super::session::Visible;
use super::theme::ThemeRole;

/// Renders one complete frame.
pub(super) fn frame(frame: &mut Frame<'_>, view: &Visible<'_>) {
    let bands = shell_areas(view.area);
    let theme = view.theme;
    let target = frame.buffer_mut();
    target.set_style(view.area, theme.style(ThemeRole::Text));

    let focused = view.windows.focused_window();
    // The terminal draws its own cursor, so one frame reports at most one
    // cursor cell: the one of the focused window. An unfocused window reports
    // none, and the terminal then shows no cursor there. See `docs/windows.md`.
    let mut cursor_at = None;
    let matches = view.search.map_or(&[][..], |search| &search.matches);
    let match_chars = view
        .search
        .filter(|_| view.settings.search.highlight_matches)
        .map_or(0, |search| search.query.text().chars().count());
    for region in view.windows.layout().regions() {
        match region.kind {
            RegionKind::Editor => {
                let Some(viewport) = view.windows.viewport(region.id) else {
                    debug_assert!(false, "every editor region belongs to one leaf window");
                    continue;
                };
                let Some(id) = view.windows.buffer(region.id) else {
                    debug_assert!(false, "every editor region belongs to one leaf window");
                    continue;
                };
                let Some(file) = view.buffers.get(id) else {
                    debug_assert!(false, "every window points at one loaded buffer");
                    continue;
                };
                let text = file.text();
                let focus = if region.id == focused {
                    WindowFocus::Focused
                } else {
                    WindowFocus::Unfocused
                };
                // The editing state follows the focused window alone, so an
                // unfocused window shows no cursor and no selection. Its gutter
                // counts from the start of its own buffer, because no per-window
                // cursor exists yet.
                let cursor = match focus {
                    WindowFocus::Focused => view.editing.cursor(),
                    WindowFocus::Unfocused => {
                        Cursor::at_buffer_start(text, ColumnLimit::LastCharacter)
                    }
                };
                // The active search belongs to the active buffer only.
                let searched = id == view.active && match_chars > 0;
                let window = WindowView {
                    buffer: text,
                    name: file.name(),
                    first_line: viewport.first_line(),
                    left_column: viewport.left_column(),
                    cursor,
                    selection: match focus {
                        WindowFocus::Focused => view.editing.selection(text),
                        WindowFocus::Unfocused => None,
                    },
                    matches: if searched { matches } else { &[] },
                    match_chars: if searched { match_chars } else { 0 },
                    highlights: view.highlights(id),
                    diagnostics: view.diagnostics(id),
                    focus,
                    display: &view.settings.display,
                    tab_width: usize::from(view.settings.indent.tab_width.get()),
                };
                render_window(target, region.area, theme, &window);
                if focus == WindowFocus::Focused {
                    cursor_at = cursor_cell(region.area, &window);
                }
            }
            // Slice 10 adds the file tree. The band keeps the surface color, so
            // the reserved width already reads as chrome.
            RegionKind::Sidebar(_) => {
                target.set_style(region.area, theme.style(ThemeRole::Surface));
            }
        }
    }

    render_statusline(
        target,
        bands.statusline,
        theme,
        view.editing.mode(),
        view.editing.cursor(),
    );
    render_message(target, bands.message, theme, view.prompt, view.message);
    if let Some(float) = view.float {
        render_float(target, bands.body, theme, float);
    }
    if let Some(rows) = view.which_key {
        render_which_key(target, bands.body, theme, rows);
    }
    if let Some(position) = cursor_at {
        frame.set_cursor_position(position);
    }
}
