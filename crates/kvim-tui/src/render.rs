//! The one frame builder of the editor.
//!
//! The builder reads visible state and writes terminal cells. It performs no
//! input, no output, and no state change, so a frame is a pure function of the
//! session. See `docs/responsiveness.md`.

use ratatui::Frame;
use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Position;

use kvim_editor::{Cursor, WindowState};
use kvim_input::Mode;
use kvim_ui::RegionKind;

use super::buffer_view::{BracketHighlight, WindowFocus, WindowView, cursor_cell, render_window};
use super::chrome::{render_message, render_statusline, shell_areas};
use super::overlay::{render_completion, render_float, render_notifications, render_which_key};
use super::picker::render_picker;
use super::session::Visible;
use super::theme::ThemeRole;
use super::tree::render_tree;

/// Renders one complete frame and applies the cursor of that frame.
///
/// The terminal owns its cursor, so the standalone adapter applies the request
/// that [`draw`] returns.
pub(super) fn frame(frame: &mut Frame<'_>, view: &Visible<'_>) {
    if let Some(position) = draw(frame.buffer_mut(), view) {
        frame.set_cursor_position(position);
    }
}

/// Renders one complete frame into the supplied cells.
///
/// The caller validated that every band of `view.area` fits inside the buffer,
/// so this function writes no cell outside that rectangle. It returns the one
/// cursor cell of the frame instead of moving a terminal cursor.
pub(super) fn draw(target: &mut CellBuffer, view: &Visible<'_>) -> Option<Position> {
    let bands = shell_areas(view.area);
    let theme = view.theme;
    target.set_style(view.area, theme.style(ThemeRole::Text));

    let focused = view.windows.focused_window();
    // The terminal draws its own cursor, so one frame reports at most one
    // cursor cell: the one of the focused window. An unfocused window reports
    // none, and the terminal then shows no cursor there. See `docs/windows.md`.
    let mut cursor_at = None;
    // The language float belongs to the focused window, so it needs that
    // rectangle as well as the cursor cell inside it. See `docs/windows.md`.
    let mut focused_area = None;
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
            RegionKind::Surface => {
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
                    path: file.path(),
                    external: file.external_change(),
                    root: view.tree.tree().root(),
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
                    // The bracket pair answers a Normal-mode `%`, and the mode
                    // belongs to the focused window, so only that window paints
                    // the pair under its cursor.
                    brackets: if focus == WindowFocus::Focused
                        && view.editing.mode() == Mode::Normal
                    {
                        BracketHighlight::Shown
                    } else {
                        BracketHighlight::Hidden
                    },
                    display: &view.settings.display,
                    tab_width: usize::from(view.settings.indent.tab_width.get()),
                };
                render_window(target, region.area, theme, &window);
                if focus == WindowFocus::Focused {
                    focused_area = Some(region.area);
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

    // The statusline reports the mode of the editor, and the cursor and the
    // format-on-save state of the focused window, because the keys act there.
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
        view.focused_format_on_save(),
    );
    render_message(
        target,
        bands.message,
        theme,
        view.confirmation,
        view.prompt,
        view.message,
    );
    // The notification overlay reports the background work of the editor, so
    // the two overlays that answer the last key paint over it.
    if !view.notifications.is_empty() {
        render_notifications(target, bands.body, theme, &view.notifications.rows());
    }
    if let Some(float) = view.float {
        // The float answers a question about the cursor, so it sits beside that
        // cursor inside its own window instead of at the bottom of the body.
        render_float(
            target,
            focused_area.unwrap_or(bands.body),
            cursor_at,
            theme,
            float,
        );
    }
    // The list answers the last key of the user, so it paints over the
    // notification overlay, which reports background work instead. See
    // `docs/windows.md`.
    if let Some(completion) = view.prompt.and_then(|prompt| prompt.completion.as_ref()) {
        render_completion(target, bands.body, theme, completion);
    }
    if let Some(rows) = view.which_key {
        // The overlay reads the one icon setting of the file tree, so a
        // terminal without a patched font turns every glyph off together.
        render_which_key(
            target,
            bands.body,
            theme,
            rows,
            view.settings.windows.file_tree_icons,
        );
    }
    // The picker covers the complete terminal, so it renders last and owns the
    // one cursor cell that the frame reports. See `docs/files.md`.
    let picker_cursor = view
        .picker
        .and_then(|picker| render_picker(target, view.area, theme, picker));
    picker_cursor.or(sidebar_cursor).or(cursor_at)
}
