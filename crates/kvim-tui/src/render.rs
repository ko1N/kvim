//! The one frame builder of the editor.
//!
//! The builder reads visible state and writes terminal cells. It performs no
//! input, no output, and no state change, so a frame is a pure function of the
//! session. See `docs/responsiveness.md`.

use ratatui::Frame;

use kvim_editor::{Cursor, WindowState};

use super::buffer_view::{WindowFocus, WindowView, cursor_cell, render_window};
use super::chrome::{render_message, render_statusline, shell_areas};
use super::layout::RegionKind;
use super::overlay::{render_float, render_notifications, render_which_key};
use super::picker::render_picker;
use super::session::Visible;
use super::theme::ThemeRole;
use super::tree::render_tree;

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
    // A focused sidebar owns the keys, so its selected row wins the one cursor
    // cell that a frame reports.
    let mut sidebar_cursor = None;
    let matches = view.search.map_or(&[][..], |search| &search.matches);
    let match_chars = view
        .search
        .filter(|_| view.settings.search.highlight_matches)
        .map_or(0, |search| search.query.text().chars().count());
    for region in view.windows.layout().regions() {
        match region.kind {
            RegionKind::Editor => {
                let Some(state) = view.windows.state(region.id) else {
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
                // Every window owns its cursor, so its relative line numbers
                // count from its own cursor line. The mode is global and belongs
                // to the focused window, so only that window paints a selection.
                // See `docs/windows.md`.
                // The active search belongs to the active buffer only.
                let searched = id == view.active && match_chars > 0;
                let window = WindowView {
                    buffer: text,
                    name: file.name(),
                    first_line: state.first_line(),
                    left_column: state.left_column(),
                    cursor: state.cursor(),
                    selection: match focus {
                        WindowFocus::Focused => view.editing.selection(text, &state),
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
            // The file tree is the one sidebar of the first release. It paints
            // its own rows, so no editor window covers its rectangle.
            RegionKind::Sidebar(_) => {
                let focus = if region.id == view.windows.focused_region() {
                    WindowFocus::Focused
                } else {
                    WindowFocus::Unfocused
                };
                let selected = render_tree(
                    target,
                    region.area,
                    theme,
                    view.tree,
                    focus,
                    view.settings.windows.file_tree_icons,
                );
                // The keys reach the sidebar, so the terminal cursor sits on
                // the selected row instead of in an editor window.
                if focus == WindowFocus::Focused {
                    sidebar_cursor = selected;
                }
            }
        }
    }

    // The statusline reports the mode of the editor and the cursor of the
    // focused window, because the keys act there.
    let focused_cursor = view
        .windows
        .state(focused)
        .map_or(Cursor::ORIGIN, WindowState::cursor);
    render_statusline(
        target,
        bands.statusline,
        theme,
        view.editing.mode(),
        focused_cursor,
    );
    render_message(target, bands.message, theme, view.prompt, view.message);
    // The notification overlay reports the background work of the editor, so
    // the two overlays that answer the last key paint over it.
    if !view.notifications.is_empty() {
        render_notifications(target, bands.body, theme, &view.notifications.rows());
    }
    if let Some(float) = view.float {
        render_float(target, bands.body, theme, float);
    }
    if let Some(rows) = view.which_key {
        render_which_key(target, bands.body, theme, rows);
    }
    // The picker covers the complete terminal, so it renders last and owns the
    // one cursor cell that the frame reports. See `docs/files.md`.
    let picker_cursor = view
        .picker
        .and_then(|picker| render_picker(target, view.area, theme, picker));
    if let Some(position) = picker_cursor.or(sidebar_cursor).or(cursor_at) {
        frame.set_cursor_position(position);
    }
}
