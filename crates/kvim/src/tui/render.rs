//! The one frame builder of the editor.
//!
//! The builder reads visible state and writes terminal cells. It performs no
//! input, no output, and no state change, so a frame is a pure function of the
//! session. See `docs/responsiveness.md`.

use ratatui::Frame;

use super::buffer_view::{WindowFocus, WindowView, render_window};
use super::chrome::{render_message, render_statusline, shell_areas};
use super::layout::RegionKind;
use super::overlay::render_which_key;
use super::session::Visible;
use super::theme::ThemeRole;

/// Renders one complete frame.
pub(super) fn frame(frame: &mut Frame<'_>, view: &Visible<'_>) {
    let bands = shell_areas(view.area);
    let theme = view.theme;
    let target = frame.buffer_mut();
    target.set_style(view.area, theme.style(ThemeRole::Text));

    let focused = view.windows.focused_window();
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
                let window = WindowView {
                    buffer: view.buffer,
                    name: view.name,
                    first_line: viewport.first_line(),
                    left_column: viewport.left_column(),
                    cursor: view.editing.cursor(),
                    selection: view.editing.selection(view.buffer),
                    matches: if match_chars == 0 { &[] } else { matches },
                    match_chars,
                    // Slice 14 connects the accepted analysis of the session.
                    // An empty list renders plain text, which every unsupported,
                    // cancelled, or rejected analysis must also do.
                    highlights: &[],
                    focus: if region.id == focused {
                        WindowFocus::Focused
                    } else {
                        WindowFocus::Unfocused
                    },
                    display: &view.settings.display,
                    tab_width: usize::from(view.settings.indent.tab_width.get()),
                };
                render_window(target, region.area, theme, &window);
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
    if let Some(rows) = view.which_key {
        render_which_key(target, bands.body, theme, rows);
    }
}
