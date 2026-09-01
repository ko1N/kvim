//! The one frame builder of the editor.
//!
//! The builder reads visible state and writes terminal cells. It performs no
//! input, no output, and no state change, so a frame is a pure function of the
//! session. See `docs/responsiveness.md`.

use ratatui::Frame;
use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect};

use kvim_editor::Cursor;
use kvim_input::{Mode, PromptKind};
use kvim_ui::{DialogStyles, RegionKind};

use super::buffer_view::{BracketHighlight, RegionFocus, WindowView, cursor_cell, render_window};
use super::chrome::{StatuslineParts, render_message, render_statusline, shell_areas};
use super::completion::draw_completion_menu_viewport;
use super::overlay::{render_float, render_notifications, render_which_key};
use super::picker::render_picker;
use super::review::draw_review;
use super::session::Visible;
use super::theme::ThemeRole;
use super::tree::{TreeChrome, render_tree};

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
    let bands = shell_areas(view.area, view.presentation);
    let theme = view.theme;
    target.set_style(view.area, theme.style(ThemeRole::Text));

    // The open review draws the body instead of the window tree. It changes no
    // window, no viewport, and no buffer, so leaving it restores the layout by
    // drawing that tree again. See `docs/diff-view.md`.
    if let Some(review) = view.review {
        draw_review(
            target,
            bands.body,
            theme,
            view.settings.diff,
            &view.tree.root_label(),
            review,
        );
        // The review draws no candidate list, because the early return here
        // runs before the list draws, so the statusline always shows its
        // parts in this branch.
        render_statusline(
            target,
            bands.statusline,
            theme,
            view.editing.mode(),
            Cursor::ORIGIN,
            view.focused_format_on_save(),
            StatuslineParts::Shown,
        );
        render_message(target, bands.message, theme, view.prompt, view.message);
        return None;
    }

    // A region is focused only while it holds the input focus, so a focused
    // sidebar leaves every editor window unfocused. See `docs/windows.md`.
    let focused_region = view.windows.focused_region();
    // The terminal draws its own cursor, so one frame reports at most one
    // cursor cell: the one of the region that holds the focus. Every other
    // region reports none, and the terminal then shows no cursor there. See
    // `docs/windows.md`.
    let mut cursor_at = None;
    // The language float belongs to the window that holds the focus, so it
    // needs that rectangle as well as the cursor cell inside it. See
    // `docs/windows.md`.
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
                let focus = if region.id == focused_region {
                    RegionFocus::Focused
                } else {
                    RegionFocus::Unfocused
                };
                // Every window owns its cursor, so its relative line numbers
                // count from its own cursor line. The mode is global and belongs
                // to the region that holds the focus, so a window paints a
                // selection only while it holds that focus. See
                // `docs/windows.md`.
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
                        RegionFocus::Focused => view.editing.selection(text, &state),
                        RegionFocus::Unfocused => None,
                    },
                    matches: if searched { matches } else { &[] },
                    match_chars: if searched { match_chars } else { 0 },
                    highlights: view.highlights(id),
                    diagnostics: view.diagnostics(id),
                    focus,
                    // The bracket pair answers a Normal-mode `%`, and that key
                    // reaches no window while a sidebar holds the focus, so
                    // only the focused window paints the pair.
                    brackets: if focus == RegionFocus::Focused
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
                if focus == RegionFocus::Focused {
                    focused_area = Some(region.area);
                    cursor_at = cursor_cell(region.area, &window);
                }
            }
            // The file tree is the one sidebar of the first release. It paints
            // its own rows, so no editor window covers its rectangle.
            RegionKind::Sidebar(_) => {
                let focus = if region.id == focused_region {
                    RegionFocus::Focused
                } else {
                    RegionFocus::Unfocused
                };
                let selected = render_tree(
                    target,
                    region.area,
                    theme,
                    view.tree,
                    TreeChrome {
                        focus,
                        icons: view.settings.windows.file_tree_icons,
                        display: &view.settings.display,
                    },
                );
                // The keys reach the sidebar, so the terminal cursor sits on
                // the selected row instead of in an editor window.
                if focus == RegionFocus::Focused {
                    sidebar_cursor = selected;
                }
            }
        }
    }

    // The candidate list ends on the statusline row, and its own width covers
    // only some of that row at some terminal widths. The statusline therefore
    // decides its own visibility as one fact for the whole row instead of
    // leaving a part to survive beside the list at the widths the list does
    // not reach. See `docs/windows.md`.
    let completion = view
        .presentation
        .command_line_embedded()
        .then(|| view.prompt.and_then(|prompt| prompt.completion.as_ref()))
        .flatten();
    let statusline_parts = if completion.is_some() {
        StatuslineParts::Hidden
    } else {
        StatuslineParts::Shown
    };

    let status = view.status();
    render_statusline(
        target,
        bands.statusline,
        theme,
        status.mode,
        status.cursor,
        status.format_on_save(),
        statusline_parts,
    );
    let internal_prompt = view.prompt.filter(|prompt| {
        matches!(
            prompt.kind,
            PromptKind::Search | PromptKind::Tree(_) | PromptKind::Picker
        )
    });
    let internal_message_area = if bands.message.is_empty()
        && view.picker.is_none()
        && internal_prompt.is_some()
        && !bands.body.is_empty()
    {
        Rect::new(bands.body.x, bands.body.bottom() - 1, bands.body.width, 1)
    } else {
        bands.message
    };
    let visible_prompt = if bands.message.is_empty() {
        internal_prompt
    } else {
        view.prompt
    };
    render_message(
        target,
        internal_message_area,
        theme,
        visible_prompt,
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
    // notification overlay, which reports background work instead. It draws
    // into the body and the statusline together, so its last row lands
    // directly above the message line, on the statusline row. The statusline
    // already drew no part there, so the list stands where the mode was. See
    // `docs/windows.md`.
    if let Some(completion) = completion {
        draw_completion_menu_viewport(
            target,
            bands.above_command_line(),
            theme,
            completion,
            view.prompt.map(|prompt| &prompt.completion_viewport),
        );
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
    if let Some(confirmation) = view.confirmation {
        let styles = DialogStyles {
            dim: theme.style(ThemeRole::DialogDim),
            surface: theme.style(ThemeRole::Surface),
            rail: theme.style(ThemeRole::DialogRail),
            icon: theme.style(ThemeRole::DialogIcon),
            body: theme.style(ThemeRole::DialogBody),
            question: theme.style(ThemeRole::DialogQuestion),
            footer: theme.style(ThemeRole::DialogFooter),
            choice: theme.style(ThemeRole::DialogChoice),
            default_choice: theme.style(ThemeRole::DialogDefaultChoice),
            focused_choice: theme.style(ThemeRole::DialogFocusedChoice),
        };
        let _ = confirmation.render(target, bands.body, styles);
        cursor_at = None;
        sidebar_cursor = None;
    }
    // The picker covers the complete terminal, so it renders last and owns the
    // one cursor cell that the frame reports. See `docs/files.md`.
    let picker_cursor = view
        .picker
        .and_then(|picker| render_picker(target, view.area, theme, picker, view.prompt));
    picker_cursor.or(sidebar_cursor).or(cursor_at)
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
