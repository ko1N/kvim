//! Tests for the pure state transitions of the event loop.
//!
//! No test opens a terminal. The session receives normalized events and an
//! elapsed time, so every transition is deterministic.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use tokio_util::sync::CancellationToken;

use kvim_clipboard::{CLIPBOARD_BYTES_MAX, ClipboardFailure};
use kvim_core::BufferRevision;
use kvim_editor::Selection;
use kvim_input::{BindingScope, CommandLineCommand, EditedLine, Mode, PromptKind, TreePrompt};
use kvim_language::{Diagnostic, DiagnosticSeverity, DocumentPosition, LspError, SourceSpan};
use kvim_path::WorktreeRelativePath;
use kvim_runtime::{ProcessOutput, WatchBatch, WatchEvent, WatchKind};
use kvim_settings::{EditorSettings, WHICH_KEY_DELAY_DEFAULT};
use kvim_terminal::{
    CellPosition, Key, KeyCode, PasteText, PointerAction, PointerButton, PointerEvent,
    PointerModifiers, PointerWheel, PointerWheelDirection, TerminalEvent,
};
use kvim_workspace::temp::TempDir;
use kvim_workspace::{
    BUFFERS_MAX, BufferPathUpdate, Candidate, DirectoryIdentity, DirectoryListing, DurableOutcome,
    ExternalChange, FileRequest, FileResult, LinkKind, MutationOutcome, PickerKind, PickerRequest,
    PickerResult, RecoveryBaseline, RecoveryRecord, SaveError, TreeEntry, Truncation,
    WorkspaceResult, recovery_record_path, write_recovery_record,
};

use crate::buffer_view::{WINBAR_ROWS, gutter_cells};
use crate::clipboard::SessionClipboard;
use crate::completion::{CompletionOutcome, LineCompletion};
use crate::embed::EditorInstanceId;
use crate::language::Float;
use crate::language::{LanguageRequest, LanguageRequestKind};
use crate::log::LOG_ENTRIES_MAX;
use crate::picker::{PickerState, picker_areas};
use crate::review::ReviewSurface;
use crate::session::{
    CONFIRM_ANSWER_CHARS_MAX, ConfirmationRequest, ConfirmedAction, HostProbeFailure, MessageLevel,
    PromptSeed, RecoveryDecision, RecoveryDecisionError, RecoveryOperation,
    RecoverySubmissionFailure, Redraw, RunState, Session, test_root,
};
use crate::tree::TREE_TITLE_ROWS;
use kvim_ui::{SidebarSide, WindowId};

const NOW: Duration = Duration::ZERO;

fn click(column: u16, row: u16) -> TerminalEvent {
    pointer_button(column, row, PointerButton::Left)
}

fn pointer_button(column: u16, row: u16, button: PointerButton) -> TerminalEvent {
    TerminalEvent::Pointer(PointerEvent::new(
        CellPosition::new(column, row),
        PointerModifiers::default(),
        PointerAction::Press(button),
    ))
}

fn drag(column: u16, row: u16) -> TerminalEvent {
    TerminalEvent::Pointer(PointerEvent::new(
        CellPosition::new(column, row),
        PointerModifiers::default(),
        PointerAction::Drag(PointerButton::Left),
    ))
}

fn release(column: u16, row: u16) -> TerminalEvent {
    TerminalEvent::Pointer(PointerEvent::new(
        CellPosition::new(column, row),
        PointerModifiers::default(),
        PointerAction::Release(PointerButton::Left),
    ))
}

fn release_for(event: &TerminalEvent) -> TerminalEvent {
    let TerminalEvent::Pointer(pointer) = event else {
        panic!("a click helper requires one pointer event");
    };
    release(pointer.position().column(), pointer.position().row())
}

fn motion(column: u16, row: u16) -> TerminalEvent {
    TerminalEvent::Pointer(PointerEvent::new(
        CellPosition::new(column, row),
        PointerModifiers::default(),
        PointerAction::Motion,
    ))
}

/// Creates one bounded vertical wheel event at a terminal cell.
fn wheel(column: u16, row: u16, direction: PointerWheelDirection) -> TerminalEvent {
    TerminalEvent::Pointer(PointerEvent::new(
        CellPosition::new(column, row),
        PointerModifiers::default(),
        PointerAction::Wheel(
            PointerWheel::new(direction, 1).expect("one tick is within the event bound"),
        ),
    ))
}

fn draw(session: &Session) -> CellBuffer {
    let area = session.area();
    let backend = TestBackend::new(area.width, area.height);
    let mut terminal = Terminal::new(backend).expect("the test backend never fails");
    terminal
        .draw(|frame| session.render(frame))
        .expect("the test backend never fails");
    terminal.backend().buffer().clone()
}

#[test]
fn buffer_drag_selects_forward_and_reverse_and_release_keeps_visual_mode() {
    let mut session = with_text(&["alpha beta"]);
    let area = session
        .windows
        .layout()
        .area(session.windows.focused_window())
        .expect("the window is visible");
    let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
    let x = area.x.saturating_add(gutter);
    let y = area.y.saturating_add(WINBAR_ROWS);

    let _ = session.handle_event(click(x.saturating_add(1), y), NOW);
    assert_eq!(
        session.handle_event(drag(x.saturating_add(4), y), NOW),
        Redraw::Needed
    );
    assert_eq!(session.mode(), Mode::Visual);
    assert_eq!(character_selection(&session), (1, 5));
    assert_eq!(
        session.handle_event(release(x.saturating_add(4), y), NOW),
        Redraw::Skipped
    );
    assert_eq!(session.mode(), Mode::Visual);
    assert_eq!(character_selection(&session), (1, 5));

    press_code(&mut session, KeyCode::Esc);
    let _ = session.handle_event(click(x.saturating_add(4), y), NOW);
    let _ = session.handle_event(drag(x.saturating_add(1), y), NOW);
    assert_eq!(character_selection(&session), (1, 5));
}

#[test]
fn plain_buffer_click_cancels_visual_and_a_later_drag_starts_a_fresh_selection() {
    let mut session = with_text(&["alpha beta"]);
    let area = session
        .windows
        .layout()
        .area(session.windows.focused_window())
        .expect("the window is visible");
    let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
    let x = area.x.saturating_add(gutter);
    let y = area.y.saturating_add(WINBAR_ROWS);

    let _ = session.handle_event(click(x, y), NOW);
    let _ = session.handle_event(drag(x.saturating_add(3), y), NOW);
    assert_eq!(session.mode(), Mode::Visual);

    let _ = session.handle_event(click(x.saturating_add(5), y), NOW);
    assert_eq!(session.mode(), Mode::Normal);
    assert!(selection(&session).is_none());

    let _ = session.handle_event(drag(x.saturating_add(7), y), NOW);
    assert_eq!(session.mode(), Mode::Visual);
    assert_eq!(character_selection(&session), (5, 8));
}

#[test]
fn escape_and_control_c_exit_visual_immediately_after_pointer_drag() {
    for cancel in [Key::plain(KeyCode::Esc), Key::ctrl(KeyCode::Char('c'))] {
        let mut session = with_text(&["alpha beta"]);
        let area = session
            .windows
            .layout()
            .area(session.windows.focused_window())
            .expect("the window is visible");
        let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
        let x = area.x.saturating_add(gutter);
        let y = area.y.saturating_add(WINBAR_ROWS);

        let _ = session.handle_event(click(x, y), NOW);
        let _ = session.handle_event(drag(x.saturating_add(3), y), NOW);
        let _ = session.handle_event(release(x.saturating_add(3), y), NOW);
        assert_eq!(session.mode(), Mode::Visual);
        assert_eq!(
            session.input_context().scope,
            BindingScope::Mode(Mode::Visual)
        );

        let _ = session.handle_event(TerminalEvent::Key(cancel), NOW);
        assert_eq!(session.mode(), Mode::Normal);
        assert!(selection(&session).is_none());
    }
}

#[test]
fn buffer_drag_maps_both_wide_glyph_cells_and_clamps_outside_text() {
    let mut session = with_text(&["a界z"]);
    let area = session
        .windows
        .layout()
        .area(session.windows.focused_window())
        .expect("the window is visible");
    let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
    let x = area.x.saturating_add(gutter);
    let y = area.y.saturating_add(WINBAR_ROWS);

    let _ = session.handle_event(click(x, y), NOW);
    let _ = session.handle_event(drag(x.saturating_add(1), y), NOW);
    assert_eq!(session.cursor().column().get(), 1);
    let _ = session.handle_event(drag(x.saturating_add(2), y), NOW);
    assert_eq!(session.cursor().column().get(), 1);
    let _ = session.handle_event(drag(u16::MAX, y), NOW);
    assert_eq!(session.cursor().column().get(), 2);
}

#[test]
fn edge_drag_scrolls_once_by_the_configured_bound() {
    let mut session = with_text(&(0..40).map(|_| "line").collect::<Vec<_>>());
    let window = session.windows.focused_window();
    let area = session
        .windows
        .layout()
        .area(window)
        .expect("the window is visible");
    let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
    let x = area.x.saturating_add(gutter);
    let y = area.y.saturating_add(WINBAR_ROWS);
    let _ = session.handle_event(click(x, y), NOW);

    let before = first_line(&session, window);
    assert_eq!(
        session.handle_event(drag(x, area.bottom()), NOW),
        Redraw::Needed
    );
    assert_eq!(
        first_line(&session, window).saturating_sub(before),
        usize::from(session.settings.mouse.scroll_rows)
    );
}

#[test]
fn buffer_drag_updates_only_its_captured_split() {
    let (mut session, left, right) = split_session(20);
    let right_before = session
        .windows
        .state(right)
        .expect("the right split has state");
    let area = session
        .windows
        .layout()
        .area(left)
        .expect("the left split is visible");
    let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
    let x = area.x.saturating_add(gutter);
    let y = area.y.saturating_add(WINBAR_ROWS);
    let _ = session.handle_event(click(x, y), NOW);
    let _ = session.handle_event(drag(x, y.saturating_add(3)), NOW);

    assert_eq!(session.windows.state(right), Some(right_before));
    assert_eq!(cursor_line(&session, left), 3);
}

#[test]
fn drag_capture_cancels_on_non_pointer_input_wheel_resize_and_overlay_change() {
    let cancellation_events = [
        TerminalEvent::Key(Key::plain(KeyCode::Char('j'))),
        TerminalEvent::Paste(PasteText::new("x").expect("the paste is bounded")),
        TerminalEvent::Unsupported,
        wheel(0, 0, PointerWheelDirection::Down),
        TerminalEvent::Resize {
            columns: 61,
            rows: 20,
        },
    ];
    for event in cancellation_events {
        let mut session = with_text(&["alpha", "beta"]);
        let area = session
            .windows
            .layout()
            .area(session.windows.focused_window())
            .expect("the window is visible");
        let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
        let x = area.x.saturating_add(gutter);
        let y = area.y.saturating_add(WINBAR_ROWS);
        let _ = session.handle_event(click(x, y), NOW);
        let _ = session.handle_event(event, NOW);
        let before = session.cursor();
        let _ = session.handle_event(drag(x, y.saturating_add(1)), NOW);
        assert_eq!(session.cursor(), before);
    }

    let mut session = with_text(&["alpha", "beta"]);
    let area = session
        .windows
        .layout()
        .area(session.windows.focused_window())
        .expect("the window is visible");
    let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
    let x = area.x.saturating_add(gutter);
    let y = area.y.saturating_add(WINBAR_ROWS);
    let _ = session.handle_event(click(x, y), NOW);
    session.picker = Some(PickerState::open(
        PickerKind::Buffers,
        test_root(workspace_root()),
        Vec::new(),
    ));
    let before = session.cursor();
    let _ = session.handle_event(drag(x, y.saturating_add(1)), NOW);
    assert_eq!(session.cursor(), before);

    let mut session = with_text(&["alpha", "beta"]);
    let area = session
        .windows
        .layout()
        .area(session.windows.focused_window())
        .expect("the window is visible");
    let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
    let x = area.x.saturating_add(gutter);
    let y = area.y.saturating_add(WINBAR_ROWS);
    let _ = session.handle_event(click(x, y), NOW);
    let _ = session.open_prompt(PromptKind::CommandLine);
    let completion = LineCompletion::open(
        "",
        vec!["first".to_owned(), "second".to_owned()],
        64,
        crate::completion::CompletionCycle::Next,
    )
    .expect("the fixture opens a completion");
    session.prompt.as_mut().unwrap().completion = Some(completion);
    session.reconcile_completion();
    let before = session.cursor();
    assert_eq!(
        session.handle_event(drag(x, y.saturating_add(1)), NOW),
        Redraw::Skipped
    );
    assert_eq!(session.cursor(), before);
}

#[test]
fn drag_capture_cancels_when_its_window_is_lost() {
    let (mut session, left, _) = split_session(20);
    let area = session
        .windows
        .layout()
        .area(left)
        .expect("the left split is visible");
    let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
    let x = area.x.saturating_add(gutter);
    let y = area.y.saturating_add(WINBAR_ROWS);
    let _ = session.handle_event(click(x, y), NOW);
    assert!(matches!(
        session.windows.close_focused(),
        kvim_ui::CloseOutcome::Closed(_)
    ));
    let before = session.cursor();

    assert_eq!(
        session.handle_event(drag(x, y.saturating_add(1)), NOW),
        Redraw::Skipped
    );
    assert_eq!(session.cursor(), before);
}

#[test]
fn drag_without_press_and_release_without_press_are_inert() {
    let mut session = with_text(&["alpha", "beta"]);
    let before = session.cursor();
    assert_eq!(session.handle_event(drag(10, 3), NOW), Redraw::Skipped);
    assert_eq!(session.handle_event(release(10, 3), NOW), Redraw::Skipped);
    assert_eq!(session.cursor(), before);
    assert_eq!(session.mode(), Mode::Normal);
}

/// Returns the published vertical border of one split session.
fn vertical_border(session: &Session) -> kvim_ui::BorderPlacement {
    *session
        .windows
        .layout()
        .borders()
        .iter()
        .find(|placement| placement.orientation() == kvim_ui::Orientation::Vertical)
        .expect("the split publishes one vertical border")
}

#[test]
fn a_border_drag_moves_the_edge_and_keeps_the_focus_and_the_cursor() {
    let (mut session, left, right) = split_session(20);
    let focused = session.windows.focused_region();
    let cursor = session.cursor();
    let border = vertical_border(&session);
    let column = border.area().x;
    let row = border.area().y + 5;

    // The press holds the border, so it selects nothing and focuses nothing.
    assert_eq!(
        session.handle_event(click(column, row), NOW),
        Redraw::Skipped
    );
    assert_eq!(session.windows.focused_region(), focused);
    assert_eq!(session.cursor(), cursor);

    // The border follows the pointer, and the panes across it give up the
    // cells.
    assert_eq!(
        session.handle_event(drag(column + 6, row), NOW),
        Redraw::Needed
    );
    let left_width = session
        .windows
        .layout()
        .area(left)
        .expect("the split is visible")
        .width;
    let right_width = session
        .windows
        .layout()
        .area(right)
        .expect("the split is visible")
        .width;
    assert_eq!(left_width, 46);
    assert_eq!(right_width, 34);
    assert_eq!(session.windows.focused_region(), focused);
    assert_eq!(session.cursor(), cursor);
    assert_eq!(
        session.mode(),
        Mode::Normal,
        "a border drag starts no selection"
    );

    // The border keeps following the pointer back, without a drift.
    assert_eq!(session.handle_event(drag(column, row), NOW), Redraw::Needed);
    assert_eq!(
        session
            .windows
            .layout()
            .area(left)
            .expect("the split is visible")
            .width,
        40
    );
}

#[test]
fn a_border_drag_ends_at_the_release_and_a_later_drag_is_inert() {
    let (mut session, left, _right) = split_session(20);
    let border = vertical_border(&session);
    let column = border.area().x;
    let row = border.area().y + 5;

    session.handle_event(click(column, row), NOW);
    session.handle_event(drag(column + 4, row), NOW);
    assert_eq!(
        session.handle_event(release(column + 4, row), NOW),
        Redraw::Skipped
    );
    let width = session
        .windows
        .layout()
        .area(left)
        .expect("the split is visible")
        .width;

    assert_eq!(
        session.handle_event(drag(column + 12, row), NOW),
        Redraw::Skipped
    );
    assert_eq!(
        session
            .windows
            .layout()
            .area(left)
            .expect("the split is visible")
            .width,
        width,
        "a drag without a press moves no border"
    );
}

#[test]
fn a_key_a_wheel_and_a_terminal_resize_each_cancel_a_border_drag() {
    for cancel in [
        TerminalEvent::Key(Key::plain(KeyCode::Esc)),
        wheel(2, 2, PointerWheelDirection::Down),
        TerminalEvent::Resize {
            columns: 80,
            rows: 24,
        },
    ] {
        let (mut session, left, _right) = split_session(20);
        let border = vertical_border(&session);
        let column = border.area().x;
        let row = border.area().y + 5;
        session.handle_event(click(column, row), NOW);
        session.handle_event(cancel.clone(), NOW);
        let width = session
            .windows
            .layout()
            .area(left)
            .expect("the split is visible")
            .width;

        assert_eq!(
            session.handle_event(drag(column + 8, row), NOW),
            Redraw::Skipped
        );
        assert_eq!(
            session
                .windows
                .layout()
                .area(left)
                .expect("the split is visible")
                .width,
            width,
            "the cancelled capture moves no border"
        );
    }
}

#[test]
fn a_drag_on_the_sidebar_border_resizes_the_sidebar() {
    let mut session = sidebar_session(&["alpha.rs", "beta.rs"]);
    let sidebar = session.tree_region.expect("the sidebar is visible");
    let before = session
        .windows
        .layout()
        .area(sidebar)
        .expect("the sidebar is visible");
    let selected = session.tree.selected_entry_name();
    // The border is the last column of the pane left of the sidebar.
    let column = before.x - 1;
    let row = before.y + 2;

    session.handle_event(click(column, row), NOW);
    assert_eq!(
        session.handle_event(drag(column - 5, row), NOW),
        Redraw::Needed
    );
    let after = session
        .windows
        .layout()
        .area(sidebar)
        .expect("the sidebar is visible");
    assert_eq!(after.width, before.width + 5);
    assert_eq!(after.x, before.x - 5);
    assert_eq!(
        session.tree.selected_entry_name(),
        selected,
        "a border drag selects no entry"
    );
}

#[test]
fn a_press_on_a_border_intersection_follows_the_first_movement() {
    // The left pane holds a horizontal split, so its border row crosses the
    // vertical border column of the outer split.
    let (mut session, left, right) = split_session(20);
    press_ctrl(&mut session, 'h');
    assert_eq!(session.windows.focused_window(), left);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let lower = session.windows.focused_window();

    let vertical = vertical_border(&session);
    let horizontal = *session
        .windows
        .layout()
        .borders()
        .iter()
        .find(|placement| placement.orientation() == kvim_ui::Orientation::Horizontal)
        .expect("the second split publishes one horizontal border");
    let column = vertical.area().x;
    let row = horizontal.area().y;
    assert!(
        kvim_ui::contains_cell(horizontal.area(), kvim_ui::Cell::new(column, row)),
        "the two borders cross at one cell"
    );

    // A movement along the columns names the vertical border, so the row edge
    // of the lower pane stays where it is.
    let lower_height = session
        .windows
        .layout()
        .area(lower)
        .expect("the split is visible")
        .height;
    session.handle_event(click(column, row), NOW);
    assert_eq!(
        session.handle_event(drag(column + 5, row + 1), NOW),
        Redraw::Needed
    );
    assert_eq!(
        session
            .windows
            .layout()
            .area(right)
            .expect("the split is visible")
            .width,
        35
    );
    assert_eq!(
        session
            .windows
            .layout()
            .area(lower)
            .expect("the split is visible")
            .height,
        lower_height,
        "the movement chose the vertical border alone"
    );
}

#[test]
fn left_click_focuses_only_the_target_split_and_places_its_cursor() {
    let (mut session, left, right) = split_session(20);
    let area = session
        .windows
        .layout()
        .area(left)
        .expect("the split is visible");
    let row = area.y.saturating_add(WINBAR_ROWS).saturating_add(3);
    assert_eq!(
        session.handle_event(click(area.x, row), NOW),
        Redraw::Needed
    );
    assert_eq!(session.windows.focused_window(), left);
    assert_eq!(cursor_line(&session, left), 3);
    assert_eq!(cursor_line(&session, right), 0);
}

#[test]
fn buffer_click_maps_gutter_tabs_wide_glyphs_horizontal_offset_and_line_end() {
    let mut session = with_text(&[
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        "\t界x",
    ]);
    let window = session.windows.focused_window();
    type_keys(&mut session, "55l");
    let area = session
        .windows
        .layout()
        .area(window)
        .expect("the window is visible");
    assert!(
        session.windows.state(window).unwrap().left_column() > 0,
        "the long first line establishes a horizontal viewport offset"
    );
    let gutter = gutter_cells(session.buffer(), &session.settings.display, area.width);
    let text_x = area.x.saturating_add(gutter);
    let first_row = area.y.saturating_add(WINBAR_ROWS);
    let _ = session.handle_event(click(text_x, first_row), NOW);
    assert!(session.cursor().column().get() > 0);

    let row = first_row.saturating_add(1);
    let _ = session.handle_event(click(area.x, row), NOW);

    for (column, expected) in [
        (area.x, 0),
        (text_x.saturating_add(2), 0),
        (text_x.saturating_add(4), 1),
        (text_x.saturating_add(5), 1),
        (text_x.saturating_add(20), 2),
    ] {
        let _ = session.handle_event(click(column, row), NOW);
        assert_eq!(session.cursor().column().get(), expected, "cell {column}");
    }
}

#[test]
fn sidebar_click_selects_once_and_activates_the_same_row_twice() {
    let mut session = session(80, 10);
    let root = workspace_root();
    let _ = session.apply_workspace_result(WorkspaceResult::Directory {
        path: root.clone(),
        outcome: Ok(DirectoryListing {
            path: root,
            identity: DirectoryIdentity::Root,
            entries: vec![TreeEntry {
                name: "clicked.rs".to_owned(),
                kind: kvim_workspace::EntryKind::File,
                link: LinkKind::Direct,
            }],
            truncation: Truncation::Complete,
        }),
    });
    press_ctrl(&mut session, 'e');
    press_ctrl(&mut session, 'h');
    let sidebar = session.tree_region.expect("the sidebar is visible");
    let area = session
        .windows
        .layout()
        .area(sidebar)
        .expect("the sidebar is visible");
    let event = click(area.x, area.y.saturating_add(TREE_TITLE_ROWS));

    assert_eq!(session.handle_event(event.clone(), NOW), Redraw::Needed);
    assert_eq!(
        session.handle_event(release_for(&event), Duration::from_millis(50)),
        Redraw::Skipped
    );
    let TerminalEvent::Pointer(pointer) = &event else {
        panic!("the sidebar click is one pointer event");
    };
    let _ = session.handle_event(
        motion(pointer.position().column(), pointer.position().row()),
        Duration::from_millis(75),
    );
    assert_eq!(session.windows.focused_region(), sidebar);
    assert_eq!(
        session.tree.selected_entry_name().as_deref(),
        Some("clicked.rs")
    );
    assert!(
        session.take_file_request().is_none(),
        "one click does not activate"
    );

    assert_eq!(
        session.handle_event(event.clone(), Duration::from_millis(100)),
        Redraw::Needed
    );
    let _ = session.handle_event(release_for(&event), Duration::from_millis(150));
    assert_ne!(session.windows.focused_region(), sidebar);
    assert!(
        session.take_file_request().is_some(),
        "the second click opens the file"
    );
}

#[test]
fn sidebar_double_click_toggles_a_directory_and_ignores_its_stale_read() {
    let mut session = session(80, 10);
    let root = workspace_root();
    let directory = root.join("docs");
    let _ = session.apply_workspace_result(WorkspaceResult::Directory {
        path: root.clone(),
        outcome: Ok(DirectoryListing {
            path: root,
            identity: DirectoryIdentity::Root,
            entries: vec![TreeEntry {
                name: "docs".to_owned(),
                kind: kvim_workspace::EntryKind::Directory,
                link: LinkKind::Direct,
            }],
            truncation: Truncation::Complete,
        }),
    });
    press_ctrl(&mut session, 'e');
    press_ctrl(&mut session, 'h');
    let event = sidebar_first_entry_click(&session);

    let _ = session.handle_event(event.clone(), NOW);
    let _ = session.handle_event(release_for(&event), Duration::from_millis(50));
    let _ = session.handle_event(event.clone(), Duration::from_millis(100));
    let _ = session.handle_event(release_for(&event), Duration::from_millis(150));
    assert!(matches!(
        session
            .tree
            .tree()
            .selected_row()
            .and_then(|row| row.kind()),
        Some(kvim_workspace::EntryKind::Directory)
    ));
    assert!(session.take_workspace_request().is_some());

    let _ = session.handle_event(event.clone(), Duration::from_millis(200));
    let _ = session.handle_event(release_for(&event), Duration::from_millis(250));
    let _ = session.handle_event(event.clone(), Duration::from_millis(300));
    let _ = session.handle_event(release_for(&event), Duration::from_millis(350));
    let _ = session.apply_workspace_result(WorkspaceResult::Directory {
        path: directory.clone(),
        outcome: Ok(DirectoryListing {
            path: directory.clone(),
            identity: DirectoryIdentity::Relative(
                WorktreeRelativePath::new("docs").expect("the fixture path is valid"),
            ),
            entries: vec![TreeEntry {
                name: "guide.md".to_owned(),
                kind: kvim_workspace::EntryKind::File,
                link: LinkKind::Direct,
            }],
            truncation: Truncation::Complete,
        }),
    });

    assert_eq!(session.tree.tree().rows().len(), 1);
    assert!(
        session.take_workspace_request().is_none(),
        "collapsing a pending directory queues no duplicate read"
    );

    let event = sidebar_first_entry_click(&session);
    let _ = session.handle_event(event.clone(), Duration::from_millis(400));
    let _ = session.handle_event(release_for(&event), Duration::from_millis(450));
    let _ = session.handle_event(event.clone(), Duration::from_millis(500));
    let _ = session.handle_event(release_for(&event), Duration::from_millis(550));
    assert!(session.take_workspace_request().is_some());
    let _ = session.apply_workspace_result(WorkspaceResult::Directory {
        path: directory.clone(),
        outcome: Ok(DirectoryListing {
            path: directory,
            identity: DirectoryIdentity::Relative(
                WorktreeRelativePath::new("docs").expect("the fixture path is valid"),
            ),
            entries: vec![TreeEntry {
                name: "guide.md".to_owned(),
                kind: kvim_workspace::EntryKind::File,
                link: LinkKind::Direct,
            }],
            truncation: Truncation::Complete,
        }),
    });
    assert_eq!(session.tree.tree().rows().len(), 2);

    let _ = session.handle_event(event.clone(), Duration::from_millis(600));
    let _ = session.handle_event(release_for(&event), Duration::from_millis(650));
    let _ = session.handle_event(event.clone(), Duration::from_millis(700));
    let _ = session.handle_event(release_for(&event), Duration::from_millis(750));
    assert_eq!(session.tree.tree().rows().len(), 1);
}

#[test]
fn sidebar_double_click_requires_consecutive_left_clicks() {
    for intervening in [
        TerminalEvent::Key(Key::plain(KeyCode::Char('j'))),
        wheel(0, 0, PointerWheelDirection::Down),
        drag(0, 0),
    ] {
        let mut session = sidebar_session(&["clicked.rs"]);
        let event = sidebar_first_entry_click(&session);

        assert_eq!(session.handle_event(event.clone(), NOW), Redraw::Needed);
        let _ = session.handle_event(release_for(&event), Duration::from_millis(25));
        let _ = session.handle_event(intervening, Duration::from_millis(50));
        assert_eq!(
            session.handle_event(event, Duration::from_millis(100)),
            Redraw::Needed
        );
        assert!(
            session.take_file_request().is_none(),
            "an intervening input cancels the pending sidebar click"
        );
    }
}

#[test]
fn sidebar_release_over_a_different_cell_cancels_activation() {
    let mut session = sidebar_session(&["clicked.rs"]);
    let event = sidebar_first_entry_click(&session);
    let TerminalEvent::Pointer(pointer) = &event else {
        panic!("the sidebar click is one pointer event");
    };

    assert_eq!(session.handle_event(event.clone(), NOW), Redraw::Needed);
    let _ = session.handle_event(
        release(
            pointer.position().column().saturating_add(1),
            pointer.position().row(),
        ),
        Duration::from_millis(50),
    );
    assert_eq!(
        session.handle_event(event, Duration::from_millis(100)),
        Redraw::Needed
    );
    assert!(session.take_file_request().is_none());
}

#[test]
fn sidebar_double_click_requires_a_release_between_presses() {
    let mut session = sidebar_session(&["clicked.rs"]);
    let event = sidebar_first_entry_click(&session);

    assert_eq!(session.handle_event(event.clone(), NOW), Redraw::Needed);
    assert_eq!(
        session.handle_event(event, Duration::from_millis(100)),
        Redraw::Needed
    );
    assert!(
        session.take_file_request().is_none(),
        "two press reports are not one physical double-click"
    );
}

#[test]
fn sidebar_double_click_requires_the_same_stable_entry() {
    let mut session = sidebar_session(&["alpha.rs", "beta.rs"]);
    let event = sidebar_first_entry_click(&session);
    assert_eq!(session.handle_event(event.clone(), NOW), Redraw::Needed);
    let _ = session.handle_event(release_for(&event), Duration::from_millis(50));

    let root = workspace_root();
    let _ = session.apply_workspace_result(WorkspaceResult::Directory {
        path: root.clone(),
        outcome: Ok(DirectoryListing {
            path: root,
            identity: DirectoryIdentity::Root,
            entries: vec![TreeEntry {
                name: "beta.rs".to_owned(),
                kind: kvim_workspace::EntryKind::File,
                link: LinkKind::Direct,
            }],
            truncation: Truncation::Complete,
        }),
    });

    assert_eq!(
        session.handle_event(event, Duration::from_millis(100)),
        Redraw::Needed
    );
    assert_eq!(
        session.tree.selected_entry_name().as_deref(),
        Some("beta.rs")
    );
    assert!(
        session.take_file_request().is_none(),
        "a changed entry at the rendered row cannot activate the first entry"
    );
}

#[test]
fn sidebar_click_after_the_interval_selects_without_activation() {
    let mut session = sidebar_session(&["clicked.rs"]);
    let event = sidebar_first_entry_click(&session);
    assert_eq!(session.handle_event(event.clone(), NOW), Redraw::Needed);
    let _ = session.handle_event(release_for(&event), Duration::from_millis(1));
    let after_interval = session.settings.mouse.double_click_interval + Duration::from_millis(1);

    assert_eq!(session.handle_event(event, after_interval), Redraw::Needed);
    assert!(session.take_file_request().is_none());
    assert_eq!(
        session.tree.selected_entry_name().as_deref(),
        Some("clicked.rs")
    );
}

#[test]
fn buffer_click_maps_the_visible_row_through_the_scrolled_viewport() {
    let text = (0..40)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut session = session(80, 10);
    press(&mut session, 'i');
    let _ = session.handle_event(
        TerminalEvent::Paste(PasteText::new(&text).expect("the fixture text is bounded")),
        NOW,
    );
    press_code(&mut session, KeyCode::Esc);
    let window = session.windows.focused_window();
    let area = session
        .windows
        .layout()
        .area(window)
        .expect("the window is visible");
    let text_row = area.y.saturating_add(WINBAR_ROWS);
    let _ = session.handle_event(wheel(area.x, text_row, PointerWheelDirection::Down), NOW);
    let first = first_line(&session, window);
    assert!(
        first > 0,
        "the wheel establishes a vertically scrolled viewport"
    );

    assert_eq!(
        session.handle_event(click(area.x, text_row), Duration::from_millis(1)),
        Redraw::Needed
    );
    assert_eq!(session.cursor().line().get(), first);
}

#[test]
fn completion_click_selects_its_row_and_does_not_reach_the_buffer() {
    let mut session = with_text(&["alpha", "beta", "gamma"]);
    press(&mut session, ':');
    let completion = LineCompletion::open(
        "",
        vec!["first".to_owned(), "second".to_owned()],
        64,
        crate::completion::CompletionCycle::Next,
    )
    .expect("the fixture opens a completion");
    session.prompt.as_mut().unwrap().completion = Some(completion);
    session.reconcile_completion();
    let before = session.cursor();
    let layout = session
        .completion_layout()
        .expect("the completion is visible");

    assert_eq!(
        session.handle_event(click(layout.area.x, layout.area.y.saturating_add(1)), NOW),
        Redraw::Needed
    );
    assert_eq!(prompt_text(&session), "second");
    assert_eq!(session.cursor(), before);
}

#[test]
fn completion_click_on_the_overflow_note_is_ignored() {
    let mut session = session(80, 10);
    press(&mut session, ':');
    let completion = LineCompletion::open(
        "",
        (0..12)
            .map(|index| format!("candidate-{index:02}"))
            .collect(),
        64,
        crate::completion::CompletionCycle::Next,
    )
    .expect("the fixture opens a completion");
    session.prompt.as_mut().unwrap().completion = Some(completion);
    session.reconcile_completion();
    let layout = session
        .completion_layout()
        .expect("the completion is visible");
    assert!(layout.hidden, "the final row is the overflow note");
    let selected = session
        .prompt
        .as_ref()
        .unwrap()
        .completion
        .as_ref()
        .unwrap()
        .selected_row();
    let text = prompt_text(&session);

    assert_eq!(
        session.handle_event(click(layout.area.x, layout.area.bottom() - 1), NOW),
        Redraw::Skipped
    );
    assert_eq!(
        session
            .prompt
            .as_ref()
            .unwrap()
            .completion
            .as_ref()
            .unwrap()
            .selected_row(),
        selected
    );
    assert_eq!(prompt_text(&session), text);
}

#[test]
fn picker_click_on_an_empty_result_row_is_ignored() {
    let mut session = with_text(&["alpha", "beta"]);
    let root = test_root(workspace_root());
    let candidates = (0..2)
        .map(|index| {
            Candidate::file(
                &root,
                WorktreeRelativePath::new(format!("file-{index}.rs")).unwrap(),
            )
        })
        .collect();
    session.picker = Some(PickerState::open(PickerKind::Buffers, root, candidates));
    let _ = session.open_prompt(PromptKind::Picker);
    session.reconcile_picker();
    let area = picker_areas(session.area).results;
    let selected = session.picker.as_ref().unwrap().picker().selected_row();

    assert_eq!(
        session.handle_event(click(area.x, area.y.saturating_add(3)), NOW),
        Redraw::Skipped
    );
    assert_eq!(
        session.picker.as_ref().unwrap().picker().selected_row(),
        selected
    );
}

#[test]
fn picker_click_selects_the_clicked_result_without_reaching_the_buffer() {
    let mut session = with_text(&["alpha", "beta"]);
    let root = test_root(workspace_root());
    let candidates = (0..4)
        .map(|index| {
            Candidate::file(
                &root,
                WorktreeRelativePath::new(format!("file-{index}.rs"))
                    .expect("the fixture path is valid"),
            )
        })
        .collect();
    session.picker = Some(PickerState::open(PickerKind::Buffers, root, candidates));
    let _ = session.open_prompt(PromptKind::Picker);
    session.reconcile_picker();
    let before = session.cursor();
    let area = picker_areas(session.area).results;

    assert_eq!(
        session.handle_event(click(area.x, area.y.saturating_add(2)), NOW),
        Redraw::Needed
    );
    assert_eq!(
        session
            .picker
            .as_ref()
            .and_then(|picker| picker.picker().selected_row()),
        Some(2)
    );
    assert_eq!(session.cursor(), before);
}

#[test]
fn decorative_float_passes_click_through_to_the_buffer() {
    let mut session = with_text(&["alpha", "beta", "gamma"]);
    session.float = Some(Float::text("note", "decoration"));
    assert_eq!(session.handle_event(click(10, 4), NOW), Redraw::Needed);
    assert_eq!(
        session.cursor().line().get(),
        3.min(session.buffer().line_count() - 1)
    );
    assert!(
        session.float.is_some(),
        "pointer input keeps decoration open"
    );
}

#[test]
fn chrome_and_unsupported_pointer_buttons_do_nothing() {
    let mut session = with_text(&["alpha"]);
    let before = session.cursor();
    assert_eq!(
        session.handle_event(click(1, session.area.bottom().saturating_sub(1)), NOW),
        Redraw::Skipped
    );
    assert_eq!(
        session.handle_event(pointer_button(10, 3, PointerButton::Right), NOW),
        Redraw::Skipped
    );
    assert_eq!(session.cursor(), before);
}

#[test]
fn wheel_scrolls_the_hovered_buffer_without_resolving_pending_keys() {
    let mut session = session(80, 12);
    press(&mut session, 'i');
    let text = "line\n".repeat(100);
    session.handle_event(
        TerminalEvent::Paste(PasteText::new(&text).expect("the test paste is bounded")),
        NOW,
    );
    press_code(&mut session, KeyCode::Esc);
    press(&mut session, 'g');
    press(&mut session, 'g');
    press(&mut session, 'g');
    let pending = session.resolver.which_key(NOW);
    let wheel = wheel(10, 2, PointerWheelDirection::Down);
    assert_eq!(session.handle_event(wheel, NOW), Redraw::Needed);
    assert_eq!(session.resolver.which_key(NOW), pending);
    let window = session.windows.focused_window();
    assert_eq!(
        session
            .windows
            .viewport(window)
            .map(|view| view.first_line()),
        Some(3)
    );
}

#[test]
fn wheel_over_file_sidebar_scrolls_without_focus_or_selection_change() {
    let mut session = session(80, 10);
    let root = workspace_root();
    let entries = (0..20)
        .map(|index| TreeEntry {
            name: format!("file-{index:02}.rs"),
            kind: kvim_workspace::EntryKind::File,
            link: LinkKind::Direct,
        })
        .collect();
    let _ = session.apply_workspace_result(WorkspaceResult::Directory {
        path: root.clone(),
        outcome: Ok(DirectoryListing {
            path: root,
            identity: DirectoryIdentity::Root,
            entries,
            truncation: Truncation::Complete,
        }),
    });
    press_ctrl(&mut session, 'e');
    let focused = session.windows.focused_region();
    let selected = session.tree.selected_entry_name();
    let before = session.tree.view().first_line();
    let frame_before = draw(&session);
    let area = session
        .tree_region
        .and_then(|id| session.windows.layout().area(id))
        .expect("the file sidebar is visible");

    assert_eq!(
        session.handle_event(
            wheel(
                area.x,
                area.y.saturating_add(1),
                PointerWheelDirection::Down
            ),
            NOW,
        ),
        Redraw::Needed
    );
    assert_eq!(session.windows.focused_region(), focused);
    assert_eq!(session.tree.selected_entry_name(), selected);
    assert!(session.tree.view().first_line() > before);
    let frame_after = draw(&session);
    assert_ne!(
        frame_after, frame_before,
        "the first frame after the wheel must show the new sidebar window"
    );
}

#[test]
fn sidebar_wheel_stays_at_bottom_through_time_only_frames_then_selection_reconciles() {
    let names: Vec<String> = (0..32).map(|index| format!("file-{index:02}.rs")).collect();
    let entry_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut session = sidebar_session(&entry_refs);
    session.float = Some(Float::text("note", "decorative overlay"));
    let area = session
        .tree_region
        .and_then(|id| session.windows.layout().area(id))
        .expect("the file sidebar is visible");
    let height = u32::from(area.height.saturating_sub(TREE_TITLE_ROWS));
    let expected_bottom = session.tree.view().total_lines().saturating_sub(height);

    for _ in 0..32 {
        let _ = session.handle_event(
            wheel(
                area.x,
                area.y.saturating_add(TREE_TITLE_ROWS),
                PointerWheelDirection::Down,
            ),
            NOW,
        );
    }
    assert_eq!(session.tree.view().first_line(), expected_bottom);

    for frame in 1..=4 {
        let first_line = session.tree.view().first_line();
        let _ = session.tick(Duration::from_millis(frame));
        assert_eq!(
            session.tree.view().first_line(),
            first_line,
            "an unrelated animated frame must not reclaim the directly scrolled viewport"
        );
    }

    session
        .tree
        .move_selection(crate::tree::TreeMotion::Down(1));
    let _ = session.tick(Duration::from_millis(5));
    let first_line = session.tree.view().first_line();
    let selected = session
        .tree
        .view()
        .selected_index()
        .expect("the tree selects a row");
    assert!(selected >= usize::try_from(first_line).expect("viewport lines fit usize"));
    assert!(
        selected
            < usize::try_from(first_line.saturating_add(height)).expect("viewport lines fit usize"),
        "keyboard selection returns ownership to the scroll-margin reconciliation"
    );
}
#[test]
fn wheel_inside_picker_results_scrolls_without_changing_selection() {
    let mut session = session(80, 10);
    let root = test_root(workspace_root());
    let candidates = (0..20)
        .map(|index| {
            Candidate::file(
                &root,
                WorktreeRelativePath::new(format!("file-{index:02}.rs"))
                    .expect("the fixture path is valid"),
            )
        })
        .collect();
    session.picker = Some(PickerState::open(PickerKind::Buffers, root, candidates));
    let _ = session.open_prompt(PromptKind::Picker);
    session.reconcile_picker();
    let selected = session
        .picker
        .as_ref()
        .and_then(|picker| picker.picker().selected_row());
    let prompt = prompt_text(&session);
    let area = picker_areas(session.area).results;

    assert_eq!(
        session.handle_event(wheel(area.x, area.y, PointerWheelDirection::Down), NOW),
        Redraw::Needed
    );
    let picker = session.picker.as_ref().expect("the picker stays open");
    assert_eq!(picker.picker().selected_row(), selected);
    assert_eq!(prompt_text(&session), prompt);
    assert!(picker.first_row() > 0);
}

#[test]
fn wheel_inside_completion_scrolls_without_changing_candidate_or_prompt() {
    let mut session = session(80, 12);
    press(&mut session, ':');
    let candidates = (0..12)
        .map(|index| format!("candidate-{index:02}"))
        .collect();
    let completion =
        LineCompletion::open("", candidates, 64, crate::completion::CompletionCycle::Next)
            .expect("the fixture opens a completion");
    let prompt = session.prompt.as_mut().expect("the command line is open");
    let _ = prompt.line.write(completion.selected().to_owned());
    prompt.completion = Some(completion);
    session.reconcile_completion();
    let selected = session
        .prompt
        .as_ref()
        .unwrap()
        .completion
        .as_ref()
        .unwrap()
        .selected_row();
    let text = prompt_text(&session);
    let layout = session
        .completion_layout()
        .expect("the completion is visible");

    assert_eq!(
        session.handle_event(
            wheel(layout.area.x, layout.area.y, PointerWheelDirection::Down),
            NOW
        ),
        Redraw::Needed
    );
    let prompt = session.prompt.as_ref().expect("the prompt stays open");
    assert_eq!(prompt.completion.as_ref().unwrap().selected_row(), selected);
    assert_eq!(prompt.line.text(), text);
    assert!(prompt.completion_viewport.first_line() > 0);

    press_code(&mut session, KeyCode::Tab);
    let prompt = session.prompt.as_ref().expect("the prompt stays open");
    let selected = prompt.completion.as_ref().unwrap().selected_row();
    let first = usize::try_from(prompt.completion_viewport.first_line()).unwrap();
    assert!(
        first <= selected
            && selected
                < first.saturating_add(usize::from(prompt.completion_viewport.height_rows())),
        "keyboard cycling reconciles the selection into the candidate viewport"
    );
}

#[test]
fn completion_overlay_intercepts_wheel_before_the_buffer() {
    let mut session = with_text(&(0..40).map(|_| "line").collect::<Vec<_>>());
    type_keys(&mut session, "gg");
    press(&mut session, ':');
    let candidates = (0..12)
        .map(|index| format!("candidate-{index:02}"))
        .collect();
    let completion =
        LineCompletion::open("", candidates, 64, crate::completion::CompletionCycle::Next)
            .expect("the fixture opens a completion");
    session.prompt.as_mut().unwrap().completion = Some(completion);
    session.reconcile_completion();
    let window = session.windows.focused_window();
    let before = first_line(&session, window);
    let layout = session
        .completion_layout()
        .expect("the completion is visible");

    let _ = session.handle_event(
        wheel(layout.area.x, layout.area.y, PointerWheelDirection::Down),
        NOW,
    );
    assert_eq!(first_line(&session, window), before);
}

#[test]
fn decorative_float_passes_wheel_through_to_the_buffer() {
    let mut session = with_text(&(0..40).map(|_| "line").collect::<Vec<_>>());
    type_keys(&mut session, "gg");
    session.float = Some(Float::text("note", "decorative overlay"));
    let window = session.windows.focused_window();
    let before = first_line(&session, window);

    assert_eq!(
        session.handle_event(wheel(10, 3, PointerWheelDirection::Down), NOW),
        Redraw::Needed
    );
    assert!(first_line(&session, window) > before);
    assert!(
        session.float.is_some(),
        "pointer input does not close decoration"
    );
}
///
/// No test reads the directory, because the session hands every read to the
/// bounded worker service.
fn workspace_root() -> PathBuf {
    std::env::current_dir().expect("the test process holds a working directory")
}

/// The which-key delay of the settings that every test session holds.
const WHICH_KEY_DELAY: Duration = WHICH_KEY_DELAY_DEFAULT;

/// Creates one sidebar session with a completed root listing.
fn sidebar_session(entries: &[&str]) -> Session {
    let mut session = session(80, 10);
    let root = workspace_root();
    let _ = session.apply_workspace_result(WorkspaceResult::Directory {
        path: root.clone(),
        outcome: Ok(DirectoryListing {
            path: root,
            identity: DirectoryIdentity::Root,
            entries: entries
                .iter()
                .map(|name| TreeEntry {
                    name: (*name).to_owned(),
                    kind: kvim_workspace::EntryKind::File,
                    link: LinkKind::Direct,
                })
                .collect(),
            truncation: Truncation::Complete,
        }),
    });
    press_ctrl(&mut session, 'e');
    press_ctrl(&mut session, 'h');
    session
}

#[test]
fn the_sidebar_scrollbar_column_scrolls_on_a_wheel_and_selects_on_no_press() {
    let names: Vec<String> = (0..32).map(|index| format!("file-{index:02}.rs")).collect();
    let entry_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut session = sidebar_session(&entry_refs);
    let area = session
        .tree_region
        .and_then(|id| session.windows.layout().area(id))
        .expect("the file sidebar is visible");
    // The body reserves its last column for the scrollbar.
    let column = area.right().saturating_sub(1);
    let row = area.y.saturating_add(TREE_TITLE_ROWS);
    let selected = session.tree.selected_entry_name();

    // A press on the track carries no entry, so it selects and focuses nothing.
    let focused = session.windows.focused_region();
    assert_eq!(
        session.handle_event(click(column, row), NOW),
        Redraw::Skipped
    );
    assert_eq!(session.tree.selected_entry_name(), selected);
    assert_eq!(session.windows.focused_region(), focused);

    // A wheel over the same column reaches the sidebar under it.
    let before = session.tree.view().first_line();
    assert_eq!(
        session.handle_event(wheel(column, row, PointerWheelDirection::Down), NOW),
        Redraw::Needed
    );
    assert!(session.tree.view().first_line() > before);
    assert_eq!(session.tree.selected_entry_name(), selected);
}

/// Returns a left click at the first visible file-sidebar entry.
fn sidebar_first_entry_click(session: &Session) -> TerminalEvent {
    let sidebar = session.tree_region.expect("the sidebar is visible");
    let area = session
        .windows
        .layout()
        .area(sidebar)
        .expect("the sidebar is visible");
    click(area.x, area.y.saturating_add(TREE_TITLE_ROWS))
}

/// Creates a session over one terminal size.
fn session(width: u16, height: u16) -> Session {
    Session::new(
        Rect::new(0, 0, width, height),
        EditorSettings::default(),
        test_root(workspace_root()),
    )
}

/// Creates a session with host-owned command and status rows.
fn integrated_session(width: u16, height: u16) -> Session {
    Session::new_with_registry_and_presentation(
        Rect::new(0, 0, width, height),
        EditorSettings::default(),
        test_root(workspace_root()),
        kvim_input::Registry::first_release(),
        crate::embed::EditorPresentation::new(false, false, false, false),
    )
}

/// Feeds one plain character key and returns the redraw request.
fn press(session: &mut Session, value: char) -> Redraw {
    session.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char(value))), NOW)
}

/// Feeds one plain key without a character.
fn press_code(session: &mut Session, code: KeyCode) -> Redraw {
    session.handle_event(TerminalEvent::Key(Key::plain(code)), NOW)
}

/// Feeds one character key with the control chord.
fn press_ctrl(session: &mut Session, value: char) -> Redraw {
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char(value))), NOW)
}

/// Feeds one key without a character with the control chord.
fn press_ctrl_code(session: &mut Session, code: KeyCode) -> Redraw {
    session.handle_event(TerminalEvent::Key(Key::ctrl(code)), NOW)
}

/// Feeds a run of plain character keys.
fn type_keys(session: &mut Session, keys: &str) {
    for value in keys.chars() {
        press(session, value);
    }
}

/// Answers the open question with one typed text and `Enter`.
///
/// The question reads the text only when `Enter` closes it, so the returned
/// redraw request belongs to that last key.
fn answer(session: &mut Session, text: &str) -> Redraw {
    type_keys(session, text);
    press_code(session, KeyCode::Enter)
}

/// Returns the open question, or an empty text while none waits.
fn question(session: &Session) -> String {
    session
        .visible()
        .confirmation
        .map_or_else(String::new, |confirmation| confirmation.question.clone())
}

/// Returns the text of the open prompt, or an empty text while none is open.
fn prompt_text(session: &Session) -> String {
    session
        .visible()
        .prompt
        .map_or_else(String::new, |prompt| prompt.line.text().to_owned())
}

/// Returns the cursor of the open prompt, counted in characters.
fn prompt_cursor(session: &Session) -> usize {
    session
        .visible()
        .prompt
        .map_or(0, |prompt| prompt.line.cursor())
}

/// Places the cursor of the open prompt at one character position.
///
/// The helper places the position directly, exactly as the seed of a prompt
/// does, so a test of one edit needs no run of motion keys before it. A test of
/// a motion presses the motion key instead.
fn place_prompt_cursor(session: &mut Session, cursor: usize) {
    let prompt = session.prompt.as_mut().expect("the test opened a prompt");
    assert!(
        cursor <= prompt.line.text().chars().count(),
        "the test places the cursor inside the line"
    );
    prompt.line = EditedLine::opened_at(
        prompt.line.text().to_owned(),
        cursor,
        prompt.line.chars_max(),
    )
    .expect("the existing prompt text meets its existing limit");
}

/// Reports whether the open prompt holds a completion.
fn completing(session: &Session) -> bool {
    session
        .visible()
        .prompt
        .is_some_and(|prompt| prompt.completion.is_some())
}

/// Returns what the open completion of the prompt shows.
///
/// A prompt without a completion reports [`CompletionOutcome::Missed`], because
/// no candidate reached its line.
fn completion_outcome(session: &Session) -> CompletionOutcome {
    session
        .visible()
        .prompt
        .and_then(|prompt| prompt.completion.as_ref())
        .map_or(CompletionOutcome::Missed, LineCompletion::outcome)
}

/// Returns the message text, or an empty text while the line is empty.
fn message(session: &Session) -> String {
    session
        .message()
        .map_or_else(String::new, |message| message.text().to_owned())
}

/// Creates a session that holds the given lines, with the cursor at the start.
fn with_text(lines: &[&str]) -> Session {
    let mut session = session(60, 20);
    press(&mut session, 'i');
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            press_code(&mut session, KeyCode::Enter);
        }
        type_keys(&mut session, line);
    }
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "gg");
    session
}

/// Returns the selection of the active Visual mode.
fn selection(session: &Session) -> Option<Selection> {
    session.selection()
}

fn character_selection(session: &Session) -> (usize, usize) {
    let Some(Selection::Characterwise(range)) = selection(session) else {
        panic!("the drag must create a characterwise selection");
    };
    (range.start().get(), range.end().get())
}

#[test]
fn the_mode_follows_the_mode_commands_and_returns_with_escape() {
    let mut session = session(40, 10);
    assert_eq!(session.mode(), Mode::Normal);
    for (keys, expected) in [
        ("i", Mode::Insert),
        ("v", Mode::Visual),
        ("V", Mode::VisualLine),
    ] {
        type_keys(&mut session, keys);
        assert_eq!(session.mode(), expected, "`{keys}` must reach {expected}");
        press_code(&mut session, KeyCode::Esc);
        assert_eq!(session.mode(), Mode::Normal);
    }
}

#[test]
fn insert_mode_typing_reaches_the_buffer_including_digits() {
    let mut session = session(40, 10);
    press(&mut session, 'i');
    // A digit is buffer text in Insert mode, never a command count.
    type_keys(&mut session, "let x = 42;");
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "y");
    assert_eq!(session.buffer().to_string(), "let x = 42;\ny\n");
    assert!(session.buffer().is_modified());

    // The same digit opens a count again after the mode returns to Normal.
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "2gg");
    assert_eq!(session.buffer().to_string(), "let x = 42;\ny\n");
}

#[test]
fn insert_mode_wires_enter_and_backspace_to_the_editor_entry_points() {
    let mut session = session(40, 10);
    press(&mut session, 'i');
    type_keys(&mut session, "    alpha");

    // `Enter` copies the indent of the previous non-empty line.
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "beta");
    assert_eq!(session.buffer().to_string(), "    alpha\n    beta\n");

    // `Backspace` deletes one character at a time.
    for _ in 0..4 {
        press_code(&mut session, KeyCode::Backspace);
    }
    assert_eq!(session.buffer().to_string(), "    alpha\n    \n");

    // At column zero it joins the cursor line with the line above it.
    for _ in 0..5 {
        press_code(&mut session, KeyCode::Backspace);
    }
    assert_eq!(session.buffer().to_string(), "    alpha\n");
}

#[test]
fn insert_mode_wires_the_control_w_chord_to_the_word_delete() {
    let mut session = session(40, 10);
    press(&mut session, 'i');
    type_keys(&mut session, "alpha beta");
    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Needed);
    assert_eq!(
        session.buffer().to_string(),
        "alpha \n",
        "`Ctrl-W` reaches the word delete instead of no binding"
    );
}

#[test]
fn the_tab_key_follows_the_indent_settings() {
    let mut soft = session(40, 10);
    press(&mut soft, 'i');
    press_code(&mut soft, KeyCode::Tab);
    assert_eq!(soft.buffer().to_string(), "    \n");

    let mut settings = EditorSettings::default();
    settings.indent.expand_tab = false;
    let mut hard = Session::new(
        Rect::new(0, 0, 40, 10),
        settings,
        test_root(workspace_root()),
    );
    press(&mut hard, 'i');
    press_code(&mut hard, KeyCode::Tab);
    assert_eq!(hard.buffer().to_string(), "\t\n");
}

#[test]
fn a_window_command_changes_the_tree_and_the_last_close_ends_the_session() {
    let mut session = session(80, 20);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    assert_eq!(session.windows().window_count(), 2);
    assert_eq!(session.run_state(), RunState::Running);

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('q'))), NOW);
    assert_eq!(session.windows().window_count(), 1);
    assert_eq!(session.run_state(), RunState::Running);

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('q'))), NOW);
    assert_eq!(
        session.run_state(),
        RunState::Finished,
        "closing the last window ends the editor"
    );
}

#[test]
fn the_focused_file_tree_answers_the_resize_keys() {
    // The sidebar owns its own binding scope, so the resize keys must live in
    // that scope as well as in the Normal scope.
    let mut session = session(120, 20);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('e'))), NOW);
    let width = |session: &Session| {
        session
            .windows()
            .sidebar(SidebarSide::Right)
            .expect("`Ctrl-E` opens the file tree")
            .width_cells()
    };
    let opened = width(&session);

    assert_eq!(
        session.handle_event(TerminalEvent::Key(Key::ctrl_alt(KeyCode::Char('h'))), NOW),
        Redraw::Needed
    );
    assert_eq!(width(&session), opened + 6, "the inner border moves left");

    assert_eq!(
        session.handle_event(TerminalEvent::Key(Key::ctrl_alt(KeyCode::Char('l'))), NOW),
        Redraw::Needed
    );
    assert_eq!(width(&session), opened, "the inner border moves back");
}

#[test]
fn only_a_visible_change_requests_a_new_frame() {
    let mut session = session(40, 10);
    // A resize to the same size changes no rectangle.
    assert_eq!(
        session.handle_event(
            TerminalEvent::Resize {
                columns: 40,
                rows: 10
            },
            NOW
        ),
        Redraw::Skipped
    );
    assert_eq!(
        session.handle_event(
            TerminalEvent::Resize {
                columns: 30,
                rows: 8
            },
            NOW
        ),
        Redraw::Needed
    );
    // A pending sequence shows nothing until the which-key delay passes.
    assert_eq!(press(&mut session, 'g'), Redraw::Skipped);
    assert_eq!(press(&mut session, 'g'), Redraw::Needed);
}

#[test]
fn the_which_key_deadline_is_the_only_time_driven_change() {
    let mut session = session(60, 20);
    assert_eq!(session.next_deadline(), None, "no sequence is pending");

    press(&mut session, ' ');
    assert_eq!(
        session.next_deadline(),
        Some(WHICH_KEY_DELAY),
        "the loop wakes when the overlay appears"
    );
    assert_eq!(session.tick(WHICH_KEY_DELAY), Redraw::Needed);
    assert_eq!(
        session.next_deadline(),
        None,
        "the overlay is visible, and the sequence itself never expires"
    );
    // The sequence survives every later tick, so the user keeps reading.
    assert_eq!(session.tick(Duration::from_secs(3_600)), Redraw::Skipped);
    press(&mut session, 'q');
    assert_eq!(
        session.run_state(),
        RunState::Finished,
        "the late key still completes `Space q`"
    );
}

#[test]
fn host_owned_which_key_has_no_internal_deadline_or_rows() {
    let mut session = session(60, 20).with_embedded_which_key(false);

    press(&mut session, ' ');
    assert_eq!(session.next_deadline(), None);
    assert_eq!(session.tick(WHICH_KEY_DELAY), Redraw::Skipped);
    assert!(session.visible().which_key.is_none());
    press(&mut session, 'q');
    assert_eq!(session.run_state(), RunState::Finished);
}

/// Feeds one bounded bracketed-paste block.
fn paste(session: &mut Session, text: &str) -> Redraw {
    let block = PasteText::new(text).expect("the block is bounded");
    session.handle_event(TerminalEvent::Paste(block), NOW)
}

#[test]
fn one_paste_block_inserts_as_one_undo_unit() {
    // The terminal reports one bracketed paste as one event, so the editor
    // applies it as one edit transaction. A run of key presses would need one
    // undo for every character. See `docs/input-actions.md`.
    let mut session = with_text(&["alpha"]);
    press(&mut session, 'i');

    assert_eq!(paste(&mut session, "one two"), Redraw::Needed);
    assert_eq!(session.buffer().to_string(), "one twoalpha\n");

    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "u");
    assert_eq!(session.buffer().to_string(), "alpha\n");
}

#[test]
fn a_paste_block_outside_insert_mode_changes_no_text() {
    // Normal mode owns no text fallback, so a paste block reaches no buffer.
    let mut session = with_text(&["alpha"]);

    assert_eq!(paste(&mut session, "one two"), Redraw::Skipped);
    assert_eq!(session.buffer().to_string(), "alpha\n");
}

#[test]
fn a_paste_block_reaches_the_open_prompt_line() {
    let mut session = session(60, 20);
    press(&mut session, ':');

    let _ = paste(&mut session, "write");

    assert_eq!(prompt_text(&session), "write");
}

#[test]
fn unsupported_input_resets_every_pending_grammar_phase() {
    // A rejected chord must never run the binding of its unmodified key, so
    // the pending count and the pending operator both end here.
    let mut session = with_text(&["alpha beta", "gamma delta", "epsilon zeta"]);
    type_keys(&mut session, "2d");

    let _ = session.handle_event(TerminalEvent::Unsupported, NOW);

    // The operator is gone, so `d` opens a new one and `d` completes it. One
    // line leaves the buffer, not the two that the abandoned count named.
    type_keys(&mut session, "dd");
    assert_eq!(session.buffer().to_string(), "gamma delta\nepsilon zeta\n");
}

#[test]
fn unsupported_input_changes_no_text_and_no_mode() {
    let mut session = with_text(&["alpha"]);
    press(&mut session, 'i');

    assert_eq!(
        session.handle_event(TerminalEvent::Unsupported, NOW),
        Redraw::Skipped
    );
    assert_eq!(session.mode(), Mode::Insert);
    assert_eq!(session.buffer().to_string(), "alpha\n");
}

#[test]
fn a_cancel_key_hides_the_overlay_and_keeps_the_mode() {
    for cancel in [
        TerminalEvent::Key(Key::plain(KeyCode::Esc)),
        TerminalEvent::Key(Key::ctrl(KeyCode::Char('c'))),
    ] {
        let mut session = session(60, 20);
        press(&mut session, 'v');
        type_keys(&mut session, " ");
        assert_eq!(session.tick(WHICH_KEY_DELAY), Redraw::Needed);
        assert_eq!(session.next_deadline(), None);

        assert_eq!(session.handle_event(cancel.clone(), NOW), Redraw::Needed);
        assert_eq!(
            session.mode(),
            Mode::Visual,
            "a cancel of pending input keeps the mode"
        );
        // A second cancel leaves the mode, because no input is pending.
        session.handle_event(cancel, NOW);
        assert_eq!(session.mode(), Mode::Normal);
    }
}

#[test]
fn the_visual_modes_switch_between_each_other_and_keep_the_anchor() {
    let control_v = TerminalEvent::Key(Key::ctrl(KeyCode::Char('v')));
    let cases: [(&str, Mode); 9] = [
        ("v", Mode::Visual),
        ("vV", Mode::VisualLine),
        ("vv", Mode::Normal),
        ("vVv", Mode::Visual),
        ("vVV", Mode::Normal),
        ("V", Mode::VisualLine),
        ("Vv", Mode::Visual),
        ("VV", Mode::Normal),
        ("vVvV", Mode::VisualLine),
    ];
    for (keys, expected) in cases {
        let mut session = with_text(&["alpha beta", "gamma delta"]);
        type_keys(&mut session, "jll");
        type_keys(&mut session, keys);
        assert_eq!(session.mode(), expected, "`{keys}` must reach {expected}");
    }

    // `Ctrl-V` completes the matrix and repeats into Normal mode.
    let mut session = with_text(&["alpha beta", "gamma delta"]);
    type_keys(&mut session, "v");
    session.handle_event(control_v.clone(), NOW);
    assert_eq!(session.mode(), Mode::VisualBlock);
    type_keys(&mut session, "V");
    assert_eq!(session.mode(), Mode::VisualLine);
    session.handle_event(control_v.clone(), NOW);
    assert_eq!(session.mode(), Mode::VisualBlock);
    session.handle_event(control_v, NOW);
    assert_eq!(session.mode(), Mode::Normal);

    // The anchor survives the switch: the selection still starts where `v` did.
    let mut session = with_text(&["alpha beta", "gamma delta"]);
    type_keys(&mut session, "vlll");
    let before = selection(&session).expect("a Visual mode always holds a selection");
    type_keys(&mut session, "V");
    let after = selection(&session).expect("a Visual mode always holds a selection");
    assert_ne!(
        std::mem::discriminant(&before),
        std::mem::discriminant(&after),
        "only the shape of the selection changes"
    );
    type_keys(&mut session, "v");
    assert_eq!(
        selection(&session),
        Some(before),
        "the anchor and the cursor return the original selection"
    );
}

#[test]
fn the_command_line_runs_the_fixed_command_set_and_rejects_the_rest() {
    let mut session = session(60, 12);
    press(&mut session, 'i');
    type_keys(&mut session, "one");
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "two");
    press_code(&mut session, KeyCode::Esc);

    // `:<number>` moves the cursor to that line.
    press(&mut session, ':');
    type_keys(&mut session, "1");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.mode(), Mode::Normal);
    assert_eq!(message(&session), "");

    // Every unknown line is a typed rejection.
    press(&mut session, ':');
    type_keys(&mut session, "wqa");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );

    // A scratch buffer holds no file name, so `:w` needs one first.
    press(&mut session, ':');
    type_keys(&mut session, "w");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        message(&session),
        "the buffer holds no file name; use :e <path> to name one"
    );

    // A buffer without a file name refuses the reload and asks nothing,
    // because no file can replace its text.
    press(&mut session, ':');
    type_keys(&mut session, "e");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        message(&session),
        "the buffer holds no file name; use :e <path> to name one"
    );
    assert_eq!(question(&session), "", "a refusal asks nothing");

    // `:q` asks before it discards the unsaved changes.
    press(&mut session, ':');
    type_keys(&mut session, "q");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Running);
    assert_eq!(
        question(&session),
        "Quit and discard the unsaved changes of [Scratch]"
    );
    answer(&mut session, "n");
    assert_eq!(session.run_state(), RunState::Running);
    assert_eq!(question(&session), "", "the answer closed the question");

    // `:q!` discards them and ends the editor.
    press(&mut session, ':');
    type_keys(&mut session, "q!");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Finished);
}

/// Reports two messages and returns both texts, oldest first.
///
/// The second message replaces the first one on the message line, so the
/// message line alone loses the first text.
fn report_two_messages(session: &mut Session) -> (String, String) {
    // A scratch buffer holds no file name, so `:w` refuses the save.
    press(session, ':');
    type_keys(session, "w");
    press_code(session, KeyCode::Enter);
    let replaced = message(session);
    assert!(!replaced.is_empty(), "the refused save reports its reason");

    press(session, ':');
    type_keys(session, "wqa");
    press_code(session, KeyCode::Enter);
    let newest = message(session);
    assert_ne!(
        newest, replaced,
        "the message line keeps the newest message only"
    );
    (replaced, newest)
}

/// Opens the editor log and returns the rows of the new buffer.
fn open_log(session: &mut Session, name: &str) -> Vec<String> {
    press(session, ':');
    type_keys(session, name);
    press_code(session, KeyCode::Enter);
    assert_eq!(session.active_buffer().name(), "[Logs]");
    session
        .buffer()
        .to_string()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_log_command_opens_a_snapshot_that_holds_a_replaced_message() {
    let mut session = session(60, 12);
    let (replaced, newest) = report_two_messages(&mut session);

    // `:l` is the declared abbreviation of `:logs`.
    let rows = open_log(&mut session, "l");
    assert_eq!(rows.len(), 2, "the log holds both messages, not {rows:?}");
    assert!(
        rows[0].ends_with(&replaced),
        "the replaced message survives in {:?}",
        rows[0]
    );
    assert!(
        rows[1].ends_with(&newest),
        "the newest message is the last row, not {:?}",
        rows[1]
    );
    assert!(
        rows[0].contains("ERROR MESSAGE"),
        "one entry names its severity and its source in {:?}",
        rows[0]
    );

    // The buffer is an ordinary scratch buffer over generated text.
    assert_eq!(session.active_buffer().path(), None);
    assert!(
        !session.active_buffer().is_modified(),
        "the new buffer holds no unsaved change"
    );
    // The command line clears the message line when it opens, exactly as it
    // does for every other command, and the log command reports nothing.
    assert_eq!(message(&session), "");
}

#[test]
fn the_diagnostics_command_probes_off_the_event_loop_and_opens_the_report() {
    let mut session = session(60, 12);
    // The declared minimum of the name reaches the command, so `:d` runs it.
    run_command(&mut session, "d");

    // The probe reads the executable search path, so the command opens no
    // buffer yet and the message line names the wait.
    assert_eq!(session.active_buffer().name(), "[Scratch]");
    assert_eq!(
        message(&session),
        "the host report is running; its buffer opens when it answers"
    );

    // The event loop hands the request to the bounded worker service, and one
    // command produces exactly one request.
    let request = session
        .take_host_request()
        .expect("the command asked for one probe");
    assert!(
        session.take_host_request().is_none(),
        "one command asks for one probe"
    );

    let report = request.run();
    assert_eq!(session.apply_host_report(&report), Redraw::Needed);
    assert_eq!(session.active_buffer().name(), "[Diagnostics]");
    let text = session.buffer().to_string();
    assert!(text.contains("Language servers ("), "{text}");
    assert!(text.contains("Formatters ("), "{text}");
    assert!(text.contains("rust-analyzer"), "{text}");

    // The probe answered, so the note that named the wait leaves the message
    // line with the buffer that it promised.
    assert_eq!(message(&session), "");

    // The buffer is an ordinary scratch buffer over generated text.
    assert_eq!(session.active_buffer().path(), None);
    assert!(!session.active_buffer().is_modified());
}

#[test]
fn a_second_diagnostics_command_starts_no_second_probe() {
    let mut session = session(60, 12);
    run_command(&mut session, "diagnostics");
    let request = session
        .take_host_request()
        .expect("the first command asked for one probe");

    // The probe already runs, so the second command reports the same state and
    // queues nothing.
    run_command(&mut session, "diagnostics");
    assert!(
        session.take_host_request().is_none(),
        "the running probe answers both commands"
    );

    let report = request.run();
    assert_eq!(session.apply_host_report(&report), Redraw::Needed);
    assert_eq!(
        session.buffers().len(),
        2,
        "the two commands open one buffer"
    );

    // The finished probe leaves the session ready for a fresh report.
    run_command(&mut session, "diagnostics");
    assert!(
        session.take_host_request().is_some(),
        "a later command asks for a fresh probe"
    );
}

#[test]
fn a_failed_host_probe_opens_no_buffer_and_reports_the_outcome() {
    let mut session = session(60, 12);
    run_command(&mut session, "diagnostics");
    let _request = session
        .take_host_request()
        .expect("the command asked for one probe");

    assert_eq!(
        session.abandon_host_request(HostProbeFailure::Timeout),
        Redraw::Needed
    );
    assert_eq!(message(&session), "the host report passed its deadline");
    assert_eq!(session.buffers().len(), 1, "the failure opens no buffer");

    // The abandoned probe leaves the session ready for a fresh report.
    run_command(&mut session, "diagnostics");
    assert!(session.take_host_request().is_some());
}

#[test]
fn an_edit_of_the_log_buffer_changes_no_entry_and_a_second_log_builds_a_new_snapshot() {
    let mut session = session(60, 12);
    let (_, newest) = report_two_messages(&mut session);
    let rows = open_log(&mut session, "logs");
    let edited = session.buffers().ids();

    // The user edits the snapshot like any other buffer.
    press(&mut session, 'i');
    type_keys(&mut session, "note");
    press_code(&mut session, KeyCode::Esc);
    assert!(session.active_buffer().is_modified());

    // The edit changed no entry, so the next snapshot holds the same rows.
    assert_eq!(open_log(&mut session, "logs"), rows);

    // The same report reaches the log again. The log collapses a repeated
    // report, so the next snapshot counts it instead of adding one row.
    press(&mut session, ':');
    type_keys(&mut session, "wqa");
    press_code(&mut session, KeyCode::Enter);
    let grown = open_log(&mut session, "logs");
    assert_eq!(grown.len(), 2, "a repeated report adds no row to {grown:?}");
    assert!(
        grown[1].ends_with(&format!("{newest} (x2)")),
        "the repeat raises the count of the newest entry, not {:?}",
        grown[1]
    );
    assert_ne!(grown, rows, "the command builds the snapshot again");

    // Every earlier snapshot stayed as it was.
    let first = edited
        .last()
        .and_then(|id| session.buffers().get(*id))
        .expect("the first log buffer is still loaded");
    assert!(
        first.text().to_string().starts_with("note"),
        "the first log buffer keeps the edit of the user"
    );
}

#[test]
fn the_command_line_completes_a_command_name_and_wraps_the_cycle() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "q");
    assert!(!completing(&session), "the typed text opens no completion");

    // The completion offers the full name, so `q` reaches `quit`. The text
    // holds no `!`, so `quit` is the whole offer and needs no list.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "quit");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Completed);
    press_code(&mut session, KeyCode::Esc);
    press_code(&mut session, KeyCode::Esc);

    // An empty line names every command, so the first cycle writes the first
    // candidate and opens the list.
    press(&mut session, ':');
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "diagnostics");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);
    for expected in ["edit", "logs", "quit", "wq", "write"] {
        press_code(&mut session, KeyCode::Tab);
        assert_eq!(prompt_text(&session), expected);
    }

    // The candidates stay anchored to the typed text, so the cycle wraps
    // instead of narrowing the list to the written candidate.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "diagnostics");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);
}

#[test]
fn no_completion_cycle_writes_a_force_variant_that_the_user_did_not_type() {
    let mut session = modified_session();
    press(&mut session, ':');
    type_keys(&mut session, "q");

    // `quit!` discards the unsaved changes and asks nothing, so no cycle of a
    // text without a `!` writes it.
    for _ in 0..4 {
        press_code(&mut session, KeyCode::Tab);
        assert_eq!(prompt_text(&session), "quit");
    }

    // The completed line runs `:quit`, which still asks before it discards.
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Running);
    assert_eq!(question(&session), QUIT_QUESTION);
    answer(&mut session, "n");
}

#[test]
fn the_command_line_completion_answers_the_size_of_its_candidate_set() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "w");
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "wq");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "write");
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(
        prompt_text(&session),
        "wq",
        "the list holds `wq` and `write` alone"
    );
    press_code(&mut session, KeyCode::Esc);
    press_code(&mut session, KeyCode::Esc);

    // One candidate completes the line and opens no list.
    press(&mut session, ':');
    type_keys(&mut session, "wq");
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Needed);
    assert_eq!(prompt_text(&session), "wq");
    assert_eq!(
        completion_outcome(&session),
        CompletionOutcome::Completed,
        "one candidate needs no list"
    );
    press_code(&mut session, KeyCode::Esc);
    press_code(&mut session, KeyCode::Esc);

    // A text that names no command changes nothing and reports nothing.
    press(&mut session, ':');
    type_keys(&mut session, "x");
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "x");
    assert!(!completing(&session));
    assert_eq!(message(&session), "");

    // A line number is no name, so the digits offer no candidate.
    press_code(&mut session, KeyCode::Backspace);
    type_keys(&mut session, "42");
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "42");
    assert!(!completing(&session));
}

#[test]
fn the_command_line_completion_cycles_backward_and_restores_the_typed_text() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "w");

    // A backward cycle from the typed text wraps to the last candidate.
    press_code(&mut session, KeyCode::BackTab);
    assert_eq!(prompt_text(&session), "write");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);
    press_code(&mut session, KeyCode::BackTab);
    assert_eq!(prompt_text(&session), "wq");
    press_code(&mut session, KeyCode::BackTab);
    assert_eq!(prompt_text(&session), "write");

    // The first cancel restores the typed text and closes the list.
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(prompt_text(&session), "w");
    assert!(!completing(&session));
    assert!(
        session.visible().prompt.is_some(),
        "the first cancel keeps the command line open"
    );

    // The second cancel closes the command line.
    press_code(&mut session, KeyCode::Esc);
    assert!(session.visible().prompt.is_none());
    assert_eq!(session.mode(), Mode::Normal);
}

#[test]
fn one_typed_key_after_a_cycle_closes_the_list_and_reads_the_new_line() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    // An empty line names every command, so the second cycle reaches `edit`
    // and the list stays open.
    press_code(&mut session, KeyCode::Tab);
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "edit");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);

    // The typed key continues from the line as it is shown.
    press(&mut session, '!');
    assert_eq!(prompt_text(&session), "edit!");
    assert!(
        !completing(&session),
        "one insert closes the candidate list"
    );

    // The next completion reads the new line and offers `edit!` alone, so it
    // never reuses the candidates of the closed list.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "edit!");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Completed);

    // One delete closes the list too, and the completion answers `edit` again.
    press_code(&mut session, KeyCode::Backspace);
    assert!(!completing(&session));
    assert_eq!(prompt_text(&session), "edit");
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(
        prompt_text(&session),
        "edit",
        "the new line holds no `!`, so it offers `edit` alone"
    );
    assert_eq!(completion_outcome(&session), CompletionOutcome::Completed);
}

#[test]
fn enter_runs_the_command_that_the_completion_wrote_into_the_line() {
    let mut session = modified_session();
    press(&mut session, ':');
    type_keys(&mut session, "qu!");
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "quit!");

    // The line shows `quit!`, so `Enter` discards the changes and asks nothing.
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(question(&session), "");
    assert_eq!(session.run_state(), RunState::Finished);
}

/// The workspace files that one test walk collects, in walk order.
///
/// The walk returns absolute paths below the workspace root, so the candidates
/// hold the same shape that the file picker receives.
fn walked_files() -> Vec<Candidate> {
    let root = test_root(workspace_root());
    [
        "src/session.rs",
        "src/main.rs",
        "docs/windows.md",
        "src/mode.rs",
    ]
    .into_iter()
    .map(|relative| {
        Candidate::file(
            &root,
            WorktreeRelativePath::new(relative).expect("the fixture path is valid"),
        )
    })
    .collect()
}

/// Answers the workspace walk that the open command line asked for.
///
/// The session performs no filesystem work, so the test plays the part of the
/// bounded worker service and hands the collected files back.
fn answer_completion_walk(session: &mut Session, files: Vec<Candidate>) {
    let request = session
        .take_completion_request()
        .expect("the open command line asks for one walk");
    assert!(
        matches!(&request, PickerRequest::Files { root } if root.as_path() == workspace_root()),
        "the walk starts at the workspace root, so no candidate leaves it"
    );
    apply_completion_walk(session, files);
}

/// Hands the collected files of one taken walk back to the session.
fn apply_completion_walk(session: &mut Session, files: Vec<Candidate>) {
    assert_eq!(
        session.apply_completion_result(PickerResult::Candidates {
            query: String::new(),
            candidates: files,
            truncated: false,
        }),
        Redraw::Skipped,
        "the list opens on the next completion key, so the frame is unchanged"
    );
}

#[test]
fn the_command_line_completes_a_path_with_the_ranking_of_the_picker() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e src/m");
    answer_completion_walk(&mut session, walked_files());

    // The query reaches the directory of a file, so the completion matches the
    // complete path as the picker does. The two names hold the same score and
    // the same width, so the source order decides between them.
    assert_eq!(
        crate::completion::command_line_candidates("e src/m", &walked_files()),
        ["e src/main.rs", "e src/mode.rs"],
        "the completion applies the shared fuzzy ranking"
    );

    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "e src/main.rs");
    assert_eq!(completion_outcome(&session), CompletionOutcome::Listed);
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "e src/mode.rs");

    // The completed line keeps the command name that the user typed, and the
    // parser accepts it.
    assert_eq!(
        CommandLineCommand::parse(&prompt_text(&session)),
        Ok(CommandLineCommand::Edit(PathBuf::from("src/mode.rs")))
    );

    // The candidates stay anchored to the typed text, so the cycle wraps.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "e src/main.rs");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(prompt_text(&session), "e src/m");
}

#[test]
fn a_completion_places_the_cursor_after_the_written_candidate() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e src/m");
    answer_completion_walk(&mut session, walked_files());

    // The candidate replaces the whole line, so the reader continues after it.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "e src/main.rs");
    assert_eq!(prompt_cursor(&session), 13);

    // The cancelled completion restores the typed text, and the cursor follows
    // that text to its end.
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(prompt_text(&session), "e src/m");
    assert_eq!(prompt_cursor(&session), 7);
}

#[test]
fn the_command_line_offers_no_path_before_the_walk_answers() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e src/ma");

    // The walk still waits for the worker service, so the key changes nothing
    // and reports nothing. The event loop never waits for the result.
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "e src/ma");
    assert!(!completing(&session));
    assert_eq!(message(&session), "");

    // The same key offers the files after the result arrives.
    answer_completion_walk(&mut session, walked_files());
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Needed);
    assert_eq!(prompt_text(&session), "e src/main.rs");
}

#[test]
fn a_path_without_a_match_and_an_empty_walk_open_no_list() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e zz");
    answer_completion_walk(&mut session, walked_files());
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "e zz");
    assert!(!completing(&session));
    assert_eq!(message(&session), "");
    press_code(&mut session, KeyCode::Esc);

    // A walk that collected no file leaves the command line in the same state.
    press(&mut session, ':');
    type_keys(&mut session, "e src");
    answer_completion_walk(&mut session, Vec::new());
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "e src");
    assert!(!completing(&session));
    assert_eq!(message(&session), "");
}

#[test]
fn only_the_path_argument_of_edit_reads_the_workspace_files() {
    let mut session = session(60, 12);
    press(&mut session, ':');

    // A line without a blank still names a command, so the name source answers
    // and no walk of the workspace starts.
    type_keys(&mut session, "e");
    assert!(session.take_completion_request().is_none());
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "edit");
    press_code(&mut session, KeyCode::Esc);
    press_code(&mut session, KeyCode::Esc);

    // `:e!` reloads the buffer, and `:w` saves it, so neither takes a path.
    for line in ["e! src", "w src"] {
        press(&mut session, ':');
        type_keys(&mut session, line);
        assert!(
            session.take_completion_request().is_none(),
            "`:{line}` takes no path, so it asks for no walk"
        );
        assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
        assert_eq!(prompt_text(&session), line);
        assert!(!completing(&session));
        press_code(&mut session, KeyCode::Esc);
    }
}

/// Types one command line and counts the walks that it asked for.
///
/// The test plays the part of the event loop and takes the request after every
/// key, exactly as `submit_completion_work` does.
fn walks_asked(session: &mut Session, line: &str) -> usize {
    let mut asked = 0;
    for value in line.chars() {
        press(session, value);
        if session.take_completion_request().is_some() {
            asked += 1;
        }
    }
    asked
}

#[test]
fn only_a_line_that_holds_a_path_argument_asks_for_the_workspace_walk() {
    // Most command lines take no path, so they walk no directory at all.
    for line in [
        "w", "q", "wq", "q!", "42", "e", "e!", "w src", "e! src", "wq foo",
    ] {
        let mut session = session(60, 12);
        press(&mut session, ':');
        assert_eq!(
            walks_asked(&mut session, line),
            0,
            "`:{line}` holds no path argument, so it asks for no walk"
        );
    }

    // The line asks once, when it first holds a path argument. Every later
    // character of that line asks for no second walk.
    for line in ["e ", "e src/ma", "edit src/main.rs", "e  x"] {
        let mut session = session(60, 12);
        press(&mut session, ':');
        assert_eq!(
            walks_asked(&mut session, line),
            1,
            "`:{line}` holds a path argument, so it asks for exactly one walk"
        );
    }
}

#[test]
fn one_open_command_line_asks_for_one_walk_and_the_next_line_asks_again() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e src/ma");
    assert!(
        session.take_completion_request().is_some(),
        "the path argument asks for one walk"
    );
    // The event loop already took the request, so the rest of the line asks for
    // nothing more.
    type_keys(&mut session, "in");
    assert!(session.take_completion_request().is_none());

    // The walk that the line asked for still answers it.
    apply_completion_walk(&mut session, walked_files());
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(prompt_text(&session), "e src/main.rs");

    // The closed line drops its files, so the next line asks for its own walk.
    press_code(&mut session, KeyCode::Esc);
    press_code(&mut session, KeyCode::Esc);
    press(&mut session, ':');
    type_keys(&mut session, "e src/ma");
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert!(session.take_completion_request().is_some());
}

#[test]
fn a_walk_that_answers_a_closed_command_line_fills_no_list() {
    let mut session = session(60, 12);
    press(&mut session, ':');
    type_keys(&mut session, "e ");
    assert!(
        session.take_completion_request().is_some(),
        "the path argument of the open command line asks for one walk"
    );
    press_code(&mut session, KeyCode::Esc);

    // The line that asked for the walk is gone, so its result fills no cache.
    assert_eq!(
        session.apply_completion_result(PickerResult::Candidates {
            query: String::new(),
            candidates: walked_files(),
            truncated: false,
        }),
        Redraw::Skipped
    );

    // The next command line asks for its own walk and offers no path until it
    // answers.
    press(&mut session, ':');
    type_keys(&mut session, "e src/ma");
    assert!(session.take_completion_request().is_some());
    assert_eq!(press_code(&mut session, KeyCode::Tab), Redraw::Skipped);
    assert_eq!(prompt_text(&session), "e src/ma");
}

/// The question that `:q` asks over the modified scratch buffer.
const QUIT_QUESTION: &str = "Quit and discard the unsaved changes of [Scratch]";

/// Returns a session whose scratch buffer holds unsaved changes.
fn modified_session() -> Session {
    let mut session = session(60, 12);
    press(&mut session, 'i');
    type_keys(&mut session, "one");
    press_code(&mut session, KeyCode::Esc);
    assert!(session.buffer().is_modified());
    session
}

/// Runs one command line and returns nothing, like a typed command.
fn run_command(session: &mut Session, line: &str) {
    press(session, ':');
    type_keys(session, line);
    press_code(session, KeyCode::Enter);
}

#[test]
fn the_quit_command_asks_and_a_confirmed_answer_ends_the_editor() {
    let mut session = modified_session();

    run_command(&mut session, "q");
    assert_eq!(
        question(&session),
        QUIT_QUESTION,
        "the question names the buffer"
    );
    assert_eq!(
        session.run_state(),
        RunState::Running,
        "the open question ends no editor"
    );

    // A lone `y` reaches the answer alone, so the editor keeps running.
    press(&mut session, 'y');
    assert_eq!(
        session.run_state(),
        RunState::Running,
        "one keypress ends no editor"
    );
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn a_cancelled_quit_keeps_the_buffer_and_the_window() {
    // `n` names the default of the question, the empty text names it as well,
    // and `no` and `ya` stand for every remaining answer.
    for value in ["n", "", "no", "ya"] {
        let mut session = modified_session();
        run_command(&mut session, "q");
        answer(&mut session, value);

        assert_eq!(
            session.run_state(),
            RunState::Running,
            "{value:?} keeps the editor running"
        );
        assert_eq!(
            session.buffer().to_string(),
            "one\n",
            "{value:?} keeps the text"
        );
        assert!(
            session.buffer().is_modified(),
            "{value:?} keeps the changes"
        );
        assert_eq!(message(&session), "", "{value:?} leaves no trace");
        assert_eq!(question(&session), "", "{value:?} closes the question");
        press(&mut session, 'i');
        assert_eq!(session.mode(), Mode::Insert, "{value:?} returns the keys");
    }

    let mut session = modified_session();
    run_command(&mut session, "q");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        session.run_state(),
        RunState::Running,
        "Esc keeps the window"
    );
    assert!(session.buffer().is_modified());
}

#[test]
fn the_forced_quit_command_asks_nothing_and_ends_the_editor() {
    let mut session = modified_session();

    run_command(&mut session, "q!");

    assert_eq!(question(&session), "", "`:q!` asks nothing");
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn a_quit_of_a_buffer_without_unsaved_changes_asks_nothing() {
    let mut session = session(60, 12);
    assert!(!session.buffer().is_modified());

    run_command(&mut session, "q");

    assert_eq!(
        question(&session),
        "",
        "a quit that destroys nothing asks nothing"
    );
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn a_cancelled_prompt_runs_no_command_and_gives_input_back_to_the_registry() {
    let mut session = session(40, 10);
    press(&mut session, ':');
    // The prompt owns input, so `q` becomes prompt text instead of a command.
    type_keys(&mut session, "q");
    assert_eq!(session.run_state(), RunState::Running);
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        session.run_state(),
        RunState::Running,
        "a cancelled command line never runs its line"
    );
    assert_eq!(session.mode(), Mode::Normal);
    press(&mut session, 'i');
    assert_eq!(
        session.mode(),
        Mode::Insert,
        "the registry owns input again"
    );
}

#[test]
fn a_backspace_on_the_empty_prompt_closes_it() {
    let mut session = session(40, 10);
    press(&mut session, ':');
    type_keys(&mut session, "q");
    press_code(&mut session, KeyCode::Backspace);
    press_code(&mut session, KeyCode::Backspace);
    // The prompt is closed, so the next key reaches the registry again.
    press(&mut session, 'i');
    assert_eq!(session.mode(), Mode::Insert);
}

#[test]
fn backspace_keeps_an_empty_rename_prompt_open_for_a_replacement_name() {
    let mut session = session(40, 10);
    session.open_prompt(PromptKind::Tree(TreePrompt::Rename));

    assert_eq!(prompt_text(&session), "");
    assert_eq!(
        press_code(&mut session, KeyCode::Backspace),
        Redraw::Skipped
    );
    assert!(
        session.visible().prompt.is_some(),
        "rename remains open after backspace reaches the empty line"
    );

    type_keys(&mut session, "replacement.txt");
    assert_eq!(prompt_text(&session), "replacement.txt");
    press_code(&mut session, KeyCode::Esc);
    assert!(
        session.visible().prompt.is_none(),
        "Escape still cancels rename"
    );
}

#[test]
fn the_control_w_chord_removes_one_word_from_the_prompt_line() {
    let mut session = session(40, 10);
    press(&mut session, ':');
    type_keys(&mut session, "e src/main.rs");
    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Needed);
    assert_eq!(prompt_text(&session), "e ", "the chord removes one word");

    // The blanks between the cursor and the word go with the word, as they do
    // in Vim, in readline, and in every terminal shell.
    type_keys(&mut session, "write   ");
    assert_eq!(prompt_text(&session), "e write   ");
    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Needed);
    assert_eq!(
        prompt_text(&session),
        "e ",
        "the trailing blanks go with the word"
    );
    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Needed);
    assert_eq!(prompt_text(&session), "");

    // Unlike `Backspace`, the chord never closes the prompt, because a host can
    // bind `Ctrl-W` as its own prefix.
    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Skipped);
    assert!(
        session.visible().prompt.is_some(),
        "the empty line keeps the prompt open"
    );
    assert_eq!(prompt_text(&session), "");
    // The prompt still owns input, so the next key writes text instead of
    // reaching the registry.
    press(&mut session, 'i');
    assert_eq!(session.mode(), Mode::Normal);
    assert_eq!(prompt_text(&session), "i");
}

#[test]
fn every_prompt_edit_moves_the_cursor_with_the_text() {
    let mut session = session(40, 10);
    press(&mut session, ':');
    assert_eq!(
        prompt_cursor(&session),
        0,
        "an empty prompt opens with the cursor at the start"
    );

    type_keys(&mut session, "e write");
    assert_eq!(
        prompt_cursor(&session),
        7,
        "each insert steps the cursor over its own character"
    );

    press_code(&mut session, KeyCode::Backspace);
    assert_eq!(prompt_text(&session), "e writ");
    assert_eq!(prompt_cursor(&session), 6);

    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Needed);
    assert_eq!(prompt_text(&session), "e ");
    assert_eq!(prompt_cursor(&session), 2);
}

#[test]
fn an_insert_writes_before_the_cursor_and_keeps_the_rest_of_the_line() {
    let mut session = session(40, 10);
    press(&mut session, ':');
    type_keys(&mut session, "e main.rs");
    place_prompt_cursor(&mut session, 2);

    type_keys(&mut session, "src/");
    assert_eq!(prompt_text(&session), "e src/main.rs");
    assert_eq!(
        prompt_cursor(&session),
        6,
        "the cursor stays after the written characters"
    );
}

#[test]
fn a_backspace_at_the_start_of_a_written_line_removes_nothing() {
    let mut session = session(40, 10);
    press(&mut session, ':');
    type_keys(&mut session, "quit");
    place_prompt_cursor(&mut session, 0);

    assert_eq!(
        press_code(&mut session, KeyCode::Backspace),
        Redraw::Skipped,
        "no character stands before the start of the line"
    );
    assert_eq!(prompt_text(&session), "quit");
    assert_eq!(prompt_cursor(&session), 0);
    assert!(
        session.visible().prompt.is_some(),
        "only the empty line closes the prompt"
    );
}

#[test]
fn a_word_delete_removes_the_word_before_the_cursor_alone() {
    let mut session = session(40, 10);
    press(&mut session, ':');
    type_keys(&mut session, "e src/main.rs");
    place_prompt_cursor(&mut session, 6);

    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Needed);
    assert_eq!(
        prompt_text(&session),
        "e main.rs",
        "the text after the cursor stays"
    );
    assert_eq!(prompt_cursor(&session), 2);
}

#[test]
fn a_reader_corrects_the_start_of_a_long_line_and_keeps_the_end_of_it() {
    let mut session = session(60, 10);
    press(&mut session, ':');
    type_keys(&mut session, "e srcc/main.rs");

    // `Home` reaches the start of the line, and the arrow keys walk to the
    // typing mistake without touching the name at the end.
    press_code(&mut session, KeyCode::Home);
    assert_eq!(prompt_cursor(&session), 0);
    for _ in 0..6 {
        press_code(&mut session, KeyCode::Right);
    }
    assert_eq!(prompt_cursor(&session), 6);

    assert_eq!(press_code(&mut session, KeyCode::Backspace), Redraw::Needed);
    assert_eq!(
        prompt_text(&session),
        "e src/main.rs",
        "the correction removed one character and kept the rest of the line"
    );
    assert_eq!(prompt_cursor(&session), 5);

    press_code(&mut session, KeyCode::End);
    assert_eq!(
        prompt_cursor(&session),
        13,
        "`End` returns to the end of the corrected line"
    );
}

#[test]
fn every_prompt_motion_stops_at_the_end_that_it_names() {
    let mut session = session(60, 10);
    press(&mut session, ':');
    type_keys(&mut session, "e src/main.rs");
    assert_eq!(prompt_cursor(&session), 13);

    // The line already ends where the cursor stands, so no forward motion
    // changes anything and none of them wraps to the start.
    assert_eq!(press_code(&mut session, KeyCode::Right), Redraw::Skipped);
    assert_eq!(press_code(&mut session, KeyCode::End), Redraw::Skipped);
    assert_eq!(
        press_ctrl_code(&mut session, KeyCode::Right),
        Redraw::Skipped
    );
    assert_eq!(prompt_cursor(&session), 13);

    // The word motion lands where the word delete cuts, so both keys name the
    // same two words.
    assert_eq!(press_ctrl_code(&mut session, KeyCode::Left), Redraw::Needed);
    assert_eq!(prompt_cursor(&session), 2);
    assert_eq!(press_ctrl_code(&mut session, KeyCode::Left), Redraw::Needed);
    assert_eq!(prompt_cursor(&session), 0);

    // The start of the line stops every backward motion in the same way.
    assert_eq!(
        press_ctrl_code(&mut session, KeyCode::Left),
        Redraw::Skipped
    );
    assert_eq!(press_code(&mut session, KeyCode::Left), Redraw::Skipped);
    assert_eq!(press_code(&mut session, KeyCode::Home), Redraw::Skipped);
    assert_eq!(prompt_cursor(&session), 0);
    assert_eq!(
        prompt_text(&session),
        "e src/main.rs",
        "no motion changes the text of the line"
    );

    // The forward word motion returns over the same two words.
    assert_eq!(
        press_ctrl_code(&mut session, KeyCode::Right),
        Redraw::Needed
    );
    assert_eq!(prompt_cursor(&session), 2);
    assert_eq!(
        press_ctrl_code(&mut session, KeyCode::Right),
        Redraw::Needed
    );
    assert_eq!(prompt_cursor(&session), 13);

    // One character forward and one character back reach the same neighbours.
    assert_eq!(press_code(&mut session, KeyCode::Left), Redraw::Needed);
    assert_eq!(prompt_cursor(&session), 12);
    assert_eq!(press_code(&mut session, KeyCode::Right), Redraw::Needed);
    assert_eq!(prompt_cursor(&session), 13);
}

#[test]
fn a_word_delete_after_a_motion_removes_the_word_before_the_moved_position() {
    let mut session = session(60, 10);
    press(&mut session, ':');
    type_keys(&mut session, "e src/main.rs");

    press_ctrl_code(&mut session, KeyCode::Left);
    assert_eq!(prompt_cursor(&session), 2);
    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Needed);
    assert_eq!(
        prompt_text(&session),
        "src/main.rs",
        "the delete removed the command word and kept the path after the cursor"
    );
    assert_eq!(prompt_cursor(&session), 0);
}

#[test]
fn the_prompt_cursor_counts_characters_and_not_bytes() {
    let mut session = session(40, 10);
    press(&mut session, ':');
    type_keys(&mut session, "eä語");
    assert_eq!(
        prompt_cursor(&session),
        3,
        "three characters stand before the cursor, and six bytes"
    );

    place_prompt_cursor(&mut session, 1);
    type_keys(&mut session, "ß");
    assert_eq!(prompt_text(&session), "eßä語");
    assert_eq!(prompt_cursor(&session), 2);

    press_code(&mut session, KeyCode::Backspace);
    assert_eq!(prompt_text(&session), "eä語");
    assert_eq!(prompt_cursor(&session), 1);

    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Needed);
    assert_eq!(
        prompt_text(&session),
        "ä語",
        "the word delete stops at the cursor as well"
    );
    assert_eq!(prompt_cursor(&session), 0);
}

#[test]
fn the_rename_seed_does_not_panic_for_a_name_with_no_stem() {
    // A submitted rename to `.` or `..` is later refused, but the seed
    // builds before any check runs, so it must still open without a panic.
    // The filesystem refuses to hold an entry literally named `.` or `..`,
    // so this test calls the constructor directly instead of driving it
    // through a real rename key over a real entry.
    for name in [".", ".."] {
        let seed = PromptSeed::before_extension(name.to_owned());
        assert_eq!(
            seed.cursor,
            name.chars().count(),
            "a name with no stem places the cursor at its end"
        );
    }
}

#[test]
fn every_prompt_but_rename_still_opens_with_the_cursor_after_its_text() {
    // `Session::prompt_seed` answers every other prompt kind with the same
    // catch-all arm, so one representative of it, driven through the command
    // line and the search prompt, stands for the rest; each specific prompt
    // kind keeps its own dedicated coverage elsewhere.
    let mut session = session(60, 10);
    press(&mut session, ':');
    assert_eq!(prompt_cursor(&session), 0, "the command line opens empty");
    press_code(&mut session, KeyCode::Esc);

    press(&mut session, '/');
    assert_eq!(prompt_cursor(&session), 0, "the search prompt opens empty");
}

/// The message that the test action of a confirmation reports.
const CONFIRMED: &str = "the confirmation reached its action";

/// Returns the typed answer of the open question, or an empty text.
fn typed_answer(session: &Session) -> String {
    session
        .visible()
        .confirmation
        .map_or_else(String::new, |confirmation| {
            confirmation.answer.text().to_owned()
        })
}

#[test]
fn a_confirmed_question_performs_its_action_and_returns_the_keys() {
    // Both accepted words perform the action, in every letter case.
    for value in ["y", "Y", "yes", "Yes", "YES", "yEs"] {
        let mut session = session(40, 10);
        assert_eq!(
            session.open_confirmation("Delete one entry", ConfirmedAction::Report),
            ConfirmationRequest::Opened
        );
        assert_eq!(answer(&mut session, value), Redraw::Needed);
        assert_eq!(message(&session), CONFIRMED, "{value} performs the action");
        assert_eq!(question(&session), "", "{value} closes the question");
        // The answer closes the question, so the mode below owns the keys again.
        press(&mut session, 'i');
        assert_eq!(session.mode(), Mode::Insert, "{value} returns the keys");
    }
}

#[test]
fn one_keypress_performs_no_action() {
    let mut session = session(40, 10);
    session.open_confirmation("Delete one entry", ConfirmedAction::Report);

    // The whole word reaches the answer, and no key of it performs the action.
    for value in ['y', 'e', 's'] {
        assert_eq!(press(&mut session, value), Redraw::Needed);
        assert_eq!(message(&session), "", "{value} alone performs no action");
        assert_eq!(
            question(&session),
            "Delete one entry",
            "{value} alone closes no question"
        );
    }
    assert_eq!(
        typed_answer(&session),
        "yes",
        "every key reached the answer"
    );

    // Only `Enter` reads the answer.
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(message(&session), CONFIRMED);
}

#[test]
fn every_other_answer_cancels_a_question_and_leaves_no_trace() {
    // `n` names the default of the question, the empty text names it as well,
    // and `no` and `ya` stand for every remaining answer.
    for value in ["n", "", "no", "ya"] {
        let mut session = session(40, 10);
        session.open_confirmation("Delete one entry", ConfirmedAction::Report);
        assert_eq!(answer(&mut session, value), Redraw::Needed);
        assert_eq!(message(&session), "", "{value:?} performs no action");
        assert_eq!(question(&session), "", "{value:?} closes the question");
        assert_eq!(
            session.buffer().to_string(),
            "\n",
            "{value:?} changes no text"
        );
        press(&mut session, 'i');
        assert_eq!(session.mode(), Mode::Insert, "{value:?} returns the keys");
    }
}

#[test]
fn a_cancel_key_closes_a_question_at_any_time() {
    // `Esc` and `Ctrl-C` cancel, and they cancel the typed `y` as well.
    let escape = Key::plain(KeyCode::Esc);
    let interrupt = Key::ctrl(KeyCode::Char('c'));
    for typed in ["", "y"] {
        for key in [escape, interrupt] {
            let mut editor = session(40, 10);
            editor.open_confirmation("Delete one entry", ConfirmedAction::Report);
            type_keys(&mut editor, typed);
            assert_eq!(
                editor.handle_event(TerminalEvent::Key(key), NOW),
                Redraw::Needed
            );
            assert_eq!(
                message(&editor),
                "",
                "{key:?} performs no action after {typed:?}"
            );
            assert_eq!(question(&editor), "", "{key:?} closes the question");
            assert_eq!(editor.mode(), Mode::Normal);
        }
    }
}

#[test]
fn a_question_completes_nothing_and_keeps_its_answer() {
    let mut session = session(40, 10);
    session.open_confirmation("Delete one entry", ConfirmedAction::Report);

    // `Tab` and `Shift-Tab` complete nothing, so they add no character.
    for code in [KeyCode::Tab, KeyCode::BackTab] {
        assert_eq!(press_code(&mut session, code), Redraw::Skipped);
        assert_eq!(typed_answer(&session), "", "{code:?} adds no character");
        assert_eq!(question(&session), "Delete one entry");
    }

    // A `Backspace` removes the character before the cursor, and one on the
    // empty answer keeps the question open.
    assert_eq!(
        press_code(&mut session, KeyCode::Backspace),
        Redraw::Skipped
    );
    assert_eq!(question(&session), "Delete one entry");
    type_keys(&mut session, "ye");
    press_code(&mut session, KeyCode::Backspace);
    assert_eq!(typed_answer(&session), "y");

    // `Tab` between the characters still completes nothing.
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(typed_answer(&session), "y", "Tab writes no candidate");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(message(&session), CONFIRMED);
}

#[test]
fn the_control_w_chord_removes_one_word_from_the_answer_of_a_question() {
    let mut session = session(40, 10);
    session.open_confirmation("Delete one entry", ConfirmedAction::Report);
    type_keys(&mut session, "yes");
    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Needed);
    assert_eq!(typed_answer(&session), "", "the chord removes the answer");

    // The empty answer keeps the question open, as a `Backspace` does.
    assert_eq!(press_ctrl(&mut session, 'w'), Redraw::Skipped);
    assert_eq!(question(&session), "Delete one entry");
}

#[test]
fn the_answer_of_a_question_holds_a_bounded_number_of_characters() {
    let mut session = session(40, 10);
    session.open_confirmation("Delete one entry", ConfirmedAction::Report);

    // The bound keeps the question and its answer inside one row.
    type_keys(&mut session, &"n".repeat(CONFIRM_ANSWER_CHARS_MAX + 4));
    assert_eq!(
        typed_answer(&session).chars().count(),
        CONFIRM_ANSWER_CHARS_MAX,
        "the answer drops the characters above the bound"
    );

    press_code(&mut session, KeyCode::Enter);
    assert_eq!(message(&session), "", "a long answer performs no action");
}

#[test]
fn an_open_question_owns_every_key_over_insert_mode() {
    // The overwrite question follows a worker result, so a question can open
    // over Insert mode. A key that the question does not read must reach no
    // buffer.
    let mut session = session(40, 10);
    press(&mut session, 'i');
    assert_eq!(session.mode(), Mode::Insert);
    session.open_confirmation("Overwrite one entry", ConfirmedAction::Report);

    for code in [KeyCode::Tab, KeyCode::BackTab] {
        assert_eq!(press_code(&mut session, code), Redraw::Skipped);
        assert_eq!(
            session.buffer().to_string(),
            "\n",
            "{code:?} inserts no buffer text"
        );
        assert_eq!(typed_answer(&session), "", "{code:?} adds no character");
    }
    assert_eq!(question(&session), "Overwrite one entry");

    // The answer still reaches the question, and Insert mode regains the keys.
    answer(&mut session, "y");
    assert_eq!(message(&session), CONFIRMED);
    assert_eq!(session.mode(), Mode::Insert);
}

#[test]
fn a_second_question_is_refused_while_one_waits() {
    let mut session = session(40, 10);
    session.open_confirmation("Delete one entry", ConfirmedAction::Report);
    assert_eq!(
        session.open_confirmation("Delete two entries", ConfirmedAction::Report),
        ConfirmationRequest::Refused
    );
    answer(&mut session, "y");
    assert_eq!(message(&session), CONFIRMED);
    // Only one question waited, so the next `y` reaches the yank operator.
    press(&mut session, 'y');
    assert_eq!(
        message(&session),
        "",
        "the refused question never reached the message line"
    );
}

#[test]
fn no_key_reaches_a_closed_question() {
    let mut session = session(40, 10);
    // Without an open question `y` reaches the yank operator instead.
    press(&mut session, 'y');
    press(&mut session, 'y');
    assert_ne!(
        message(&session),
        CONFIRMED,
        "`y` answers no question while none is open"
    );

    session.open_confirmation("Delete one entry", ConfirmedAction::Report);
    answer(&mut session, "n");
    press(&mut session, 'y');
    press(&mut session, 'y');
    assert_ne!(
        message(&session),
        CONFIRMED,
        "the answered question takes no further key"
    );
    press(&mut session, 'i');
    assert_eq!(session.mode(), Mode::Insert);
}

#[test]
fn a_question_over_a_prompt_returns_the_keys_to_that_prompt() {
    // A question can open while a prompt reads a line, because the overwrite
    // question follows a worker result instead of a key.
    for value in ["y", "n"] {
        let mut session = session(40, 10);
        press(&mut session, '/');
        type_keys(&mut session, "al");
        session.open_confirmation("Overwrite one entry", ConfirmedAction::Report);
        assert_eq!(
            prompt_text(&session),
            "al",
            "the question keeps the text of the prompt"
        );

        // The question owns the keys, so its own characters reach no prompt.
        type_keys(&mut session, value);
        assert_eq!(
            prompt_text(&session),
            "al",
            "{value} reaches the answer, not the prompt"
        );
        assert_eq!(typed_answer(&session), value);

        // The `Enter` of the answer closes the question alone. The prompt keeps
        // its text and runs nothing.
        press_code(&mut session, KeyCode::Enter);
        assert_eq!(question(&session), "", "{value} closes the question");
        assert_eq!(
            prompt_text(&session),
            "al",
            "one Enter reaches the question alone, so the prompt stays open"
        );
        type_keys(&mut session, "pha");
        assert_eq!(
            prompt_text(&session),
            "alpha",
            "the prompt reads the keys again after {value}"
        );

        press_code(&mut session, KeyCode::Esc);
        press(&mut session, 'i');
        assert_eq!(
            session.mode(),
            Mode::Insert,
            "the closed prompt returns the keys to the mode after {value}"
        );
    }
}

#[test]
fn a_search_without_a_match_reports_it_and_keeps_the_cursor() {
    let mut session = session(40, 10);
    press(&mut session, 'i');
    type_keys(&mut session, "alpha");
    press_code(&mut session, KeyCode::Esc);

    press(&mut session, '/');
    type_keys(&mut session, "zeta");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(message(&session), "no match");
    assert_eq!(session.buffer().to_string(), "alpha\n");
}

#[test]
fn a_new_command_clears_the_previous_message() {
    let mut session = session(60, 10);
    press(&mut session, 'u');
    assert_eq!(message(&session), "no further change");
    press(&mut session, 'j');
    assert_eq!(message(&session), "");
}

#[test]
fn an_exhausted_history_reports_instead_of_changing_the_buffer() {
    let mut session = session(40, 10);
    press(&mut session, 'u');
    assert_eq!(message(&session), "no further change");
    assert_eq!(session.buffer().to_string(), "\n");
}

#[test]
fn a_terminal_resize_keeps_every_window_identity() {
    let mut session = session(80, 24);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let before = session.windows().window_ids();

    session.handle_event(
        TerminalEvent::Resize {
            columns: 50,
            rows: 12,
        },
        NOW,
    );
    assert_eq!(session.windows().window_ids(), before);
    assert_eq!(session.area(), Rect::new(0, 0, 50, 12));
}

#[test]
fn the_viewport_follows_the_text_area_instead_of_the_window_rectangle() {
    let mut session = session(40, 12);
    press(&mut session, 'i');
    for index in 0..40 {
        if index > 0 {
            press_code(&mut session, KeyCode::Enter);
        }
        type_keys(&mut session, &format!("line{index}"));
    }
    press_code(&mut session, KeyCode::Esc);

    // The terminal holds twelve rows: one winbar, nine text rows, one
    // statusline, and one message line. The viewport must report the nine text
    // rows, so the scroll margin applies to the cells that the reader sees.
    let window = session.windows().focused_window();
    let viewport = session
        .windows()
        .viewport(window)
        .expect("the focused window is always visible");
    assert_eq!(viewport.height_rows().get(), 9);
    assert_eq!(
        viewport.width_cells().get(),
        34,
        "the gutter takes five cells and the scrollbar takes one cell"
    );
    // The cursor sits on the last line, so the view keeps it visible.
    assert!(viewport.first_line() + 9 > 39);
}

/// Creates a session that keeps no persistent undo file.
///
/// The tests below save real files. The undo file would reach the editor state
/// directory of the user, so these sessions keep it off.
fn file_session(root: &Path) -> Session {
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    Session::new(
        Rect::new(0, 0, 80, 24),
        settings,
        test_root(root.to_path_buf()),
    )
}

/// Refuses every queued language request, like an editor without a server.
///
/// The event loop performs the same step, so a save that waits for a formatter
/// continues instead of stalling. See `docs/language-services.md`.
fn refuse_language_requests(session: &mut Session) {
    asks_a_question(session);
}

/// Refuses every queued request and reports whether one asked a question.
///
/// A save that formats first asks its language server before it writes, so the
/// answer distinguishes a formatting save from a plain save.
fn asks_a_question(session: &mut Session) -> bool {
    let mut asked = false;
    while let Some(request) = session.take_language_request() {
        asked |= request.kind() == LanguageRequestKind::Query;
        let _ = session.apply_language_dispatch(&request, Err(LspError::NoServerDeclared));
    }
    asked
}

/// Refuses every queued language request with one typed language state.
///
/// The editor reports each normal state once, so a test that proves the report
/// hands the same state to every queued request.
fn refuse_language_requests_with(session: &mut Session, state: impl Fn() -> LspError) {
    while let Some(request) = session.take_language_request() {
        let _ = session.apply_language_dispatch(&request, Err(state()));
    }
}

/// Runs the queued file request, like the event loop and the worker service.
fn run_file_request(session: &mut Session) {
    refuse_language_requests(session);
    let request = session
        .take_file_request()
        .expect("the transition queued one file request");
    let _ = session.apply_file_result(request.run());
}

fn run_recovery_work(session: &mut Session) {
    while let Some(checkpoint) = session.take_recovery_checkpoint() {
        let _ = session.apply_recovery_checkpoint(checkpoint.run());
    }
}

fn write_recovery_for_open_file(
    directory: &TempDir,
    target_path: &Path,
    baseline: RecoveryBaseline,
    recovered: &str,
) -> PathBuf {
    let mut first =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    first.open_path(target_path.to_path_buf());
    run_file_request(&mut first);
    let target = first
        .active_buffer()
        .target()
        .expect("the file open resolved one target")
        .clone();
    let record = RecoveryRecord::new(
        &target,
        baseline,
        BufferRevision::from_parts(1, 7),
        recovered.to_owned(),
        1024,
        1024,
    )
    .expect("the test recovery text fits");
    let path = recovery_record_path(&directory.join("state"), &target);
    assert!(matches!(
        write_recovery_record(&path, &record),
        DurableOutcome::Committed(())
    ));
    path
}

fn other_recovery_target(directory: &TempDir, session: &mut Session) -> kvim_workspace::FileTarget {
    let original = session.active;
    let other_path = directory.write("other.rs", "other\n");
    session.open_path(other_path);
    run_file_request(session);
    let target = session
        .active_buffer()
        .target()
        .expect("the other file has a target")
        .clone();
    session.active = original;
    target
}

#[test]
fn current_recovery_restores_as_one_dirty_undoable_transaction() {
    let directory = TempDir::new("session-recovery-restore");
    let path = directory.write("main.rs", "disk\n");
    write_recovery_for_open_file(
        &directory,
        &path,
        RecoveryBaseline::saved("disk\n"),
        "recovered\n",
    );
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    let identity = session
        .pending_recovery()
        .expect("recovery asks for a choice");

    assert_eq!(
        session.decide_recovery(&identity, RecoveryDecision::Restore),
        Ok(Redraw::Needed)
    );
    assert_eq!(session.buffer().to_string(), "recovered\n");
    assert!(session.buffer().is_modified());
    press(&mut session, 'u');
    assert_eq!(session.buffer().to_string(), "disk\n");
}

#[test]
fn recovery_discard_deletes_in_order_and_defer_retains_the_record() {
    for (decision, deleted) in [
        (RecoveryDecision::Discard, true),
        (RecoveryDecision::Defer, false),
    ] {
        let directory = TempDir::new("session-recovery-resolution");
        let path = directory.write("main.rs", "disk\n");
        let record_path = write_recovery_for_open_file(
            &directory,
            &path,
            RecoveryBaseline::saved("disk\n"),
            "recovered\n",
        );
        let mut session =
            file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
        session.open_path(path);
        run_file_request(&mut session);
        let identity = session.pending_recovery().unwrap();
        assert_eq!(
            session.decide_recovery(&identity, decision),
            Ok(Redraw::Needed)
        );
        assert_eq!(session.buffer().to_string(), "disk\n");
        if deleted {
            let cleanup = session
                .take_recovery_checkpoint()
                .expect("discard queues ordered committing cleanup");
            let _ = session.apply_recovery_checkpoint(cleanup.run());
        }
        assert_eq!(record_path.exists(), !deleted);
    }
}

#[test]
fn changed_and_missing_recovery_baselines_never_restore_and_can_defer_or_discard() {
    for (missing, decision) in [
        (false, RecoveryDecision::Defer),
        (true, RecoveryDecision::Discard),
    ] {
        let directory = TempDir::new("session-recovery-stale");
        let path = directory.write("main.rs", "old\n");
        let record_path = write_recovery_for_open_file(
            &directory,
            &path,
            RecoveryBaseline::saved("old\n"),
            "recovered\n",
        );
        if missing {
            std::fs::remove_file(&path).expect("the test removes the target");
        } else {
            std::fs::write(&path, "new\n").expect("the test changes the target");
        }
        let mut session =
            file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
        session.open_path(path);
        run_file_request(&mut session);
        let identity = session
            .pending_recovery()
            .expect("stale recovery is retained");
        assert_eq!(
            session.decide_recovery(&identity, RecoveryDecision::Restore),
            Err(RecoveryDecisionError::RestoreForbidden)
        );
        assert!(record_path.exists());
        assert_eq!(
            session.buffer().to_string(),
            if missing { "\n" } else { "new\n" }
        );
        assert_eq!(
            session.decide_recovery(&identity, decision),
            Ok(Redraw::Needed)
        );
        if decision == RecoveryDecision::Defer {
            assert!(record_path.exists());
            continue;
        }
        let cleanup = session
            .take_recovery_checkpoint()
            .expect("explicit disposal queues stale-record cleanup");
        let _ = session.apply_recovery_checkpoint(cleanup.run());
        assert!(!record_path.exists());
    }
}

#[test]
fn recovery_open_refuses_configured_oversize_and_wrong_or_stale_decisions() {
    let directory = TempDir::new("session-recovery-address");
    let path = directory.write("main.rs", "disk\n");
    let record_path = write_recovery_for_open_file(
        &directory,
        &path,
        RecoveryBaseline::saved("disk\n"),
        "recovered\n",
    );
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    settings.files.recovery_max_bytes = 4;
    let mut bounded = Session::new(
        Rect::new(0, 0, 80, 24),
        settings,
        test_root(directory.path.clone()),
    )
    .with_recovery_state_directory(directory.join("state"));
    bounded.open_path(path.clone());
    run_file_request(&mut bounded);
    assert!(bounded.pending_recovery().is_none());
    assert_eq!(bounded.buffer().to_string(), "disk\n");
    assert!(record_path.exists());

    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    let identity = session.pending_recovery().unwrap();
    let mut wrong = identity.clone();
    wrong.instance = EditorInstanceId::allocate();
    assert_eq!(
        session.decide_recovery(&wrong, RecoveryDecision::Defer),
        Err(RecoveryDecisionError::WrongInstance)
    );
    wrong = identity.clone();
    wrong.recovery_revision = BufferRevision::from_parts(99, 99);
    assert_eq!(
        session.decide_recovery(&wrong, RecoveryDecision::Discard),
        Err(RecoveryDecisionError::Stale)
    );
    wrong = identity.clone();
    wrong.target = other_recovery_target(&directory, &mut session);
    assert_eq!(
        session.decide_recovery(&wrong, RecoveryDecision::Restore),
        Err(RecoveryDecisionError::Stale)
    );
    press(&mut session, 'i');
    press(&mut session, 'x');
    assert_eq!(
        session.decide_recovery(&identity, RecoveryDecision::Discard),
        Err(RecoveryDecisionError::Stale)
    );
    assert!(session.pending_recovery().is_some());
    assert!(record_path.exists());
}

#[test]
fn recovery_checkpoints_submit_once_and_coalesce_the_newest_pending_edit() {
    let directory = TempDir::new("session-recovery-coalescing");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);

    press(&mut session, 'i');
    assert!(
        session.take_recovery_checkpoint().is_none(),
        "a command that changes no text queues no checkpoint"
    );
    press(&mut session, 'a');
    let first = session
        .take_recovery_checkpoint()
        .expect("the first accepted edit queues immediately");
    assert!(
        session.take_recovery_checkpoint().is_none(),
        "one active checkpoint is submitted once"
    );
    press(&mut session, 'b');
    press(&mut session, 'c');
    assert!(
        session.take_recovery_checkpoint().is_none(),
        "new edits coalesce while the active checkpoint runs"
    );

    let first_revision = first.revision;
    let _ = session.apply_recovery_checkpoint(first.run());
    let pending = session
        .take_recovery_checkpoint()
        .expect("completion releases the newest pending checkpoint");
    assert!(pending.revision > first_revision);
    assert_eq!(
        match &pending.operation {
            RecoveryOperation::Write(record) => record.text(),
            RecoveryOperation::Delete => panic!("the pending operation is a write"),
        },
        "abcone\n"
    );
}

#[test]
fn lifecycle_cleanup_removes_records_only_after_current_save_or_destructive_discard() {
    for action in ["save", "reload", "quit", "forced-quit"] {
        let directory = TempDir::new("session-recovery-lifecycle-cleanup");
        let path = directory.write("main.rs", "one\n");
        let mut session =
            file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
        session.open_path(path);
        run_file_request(&mut session);
        press(&mut session, 'i');
        press(&mut session, 'a');
        press_code(&mut session, KeyCode::Esc);
        let checkpoint = session.take_recovery_checkpoint().unwrap();
        let record_path = checkpoint.path.clone();
        let _ = session.apply_recovery_checkpoint(checkpoint.run());
        assert!(record_path.exists());

        match action {
            "save" => {
                press_ctrl(&mut session, 's');
                run_file_request(&mut session);
            }
            "reload" => {
                run_command(&mut session, "e");
                answer(&mut session, "y");
                run_file_request(&mut session);
            }
            "quit" => {
                run_command(&mut session, "q");
                answer(&mut session, "y");
            }
            "forced-quit" => run_command(&mut session, "q!"),
            _ => unreachable!(),
        }
        run_recovery_work(&mut session);
        assert!(
            !record_path.exists(),
            "{action} removes the recovery record"
        );
        assert!(session.pending_recovery().is_none());
    }
}

#[test]
fn cancelled_reload_and_quit_keep_the_recovery_record() {
    for action in ["reload", "quit"] {
        let directory = TempDir::new("session-recovery-lifecycle-cancel");
        let path = directory.write("main.rs", "one\n");
        let mut session =
            file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
        session.open_path(path);
        run_file_request(&mut session);
        press(&mut session, 'i');
        press(&mut session, 'a');
        press_code(&mut session, KeyCode::Esc);
        let checkpoint = session.take_recovery_checkpoint().unwrap();
        let record_path = checkpoint.path.clone();
        let _ = session.apply_recovery_checkpoint(checkpoint.run());

        run_command(&mut session, if action == "reload" { "e" } else { "q" });
        answer(&mut session, "n");

        assert!(record_path.exists(), "cancelled {action} keeps recovery");
        assert!(session.take_recovery_checkpoint().is_none());
    }
}

#[test]
fn failed_save_preserves_the_committed_recovery_record() {
    let directory = TempDir::new("session-recovery-failed-save");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path.clone());
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    let checkpoint = session.take_recovery_checkpoint().unwrap();
    let record_path = checkpoint.path.clone();
    let _ = session.apply_recovery_checkpoint(checkpoint.run());
    std::fs::write(path, "external\n").expect("the test changes the file");

    press_ctrl(&mut session, 's');
    run_file_request(&mut session);

    assert!(session.buffer().is_modified());
    assert!(record_path.exists());
    assert!(session.take_recovery_checkpoint().is_none());
}

#[test]
fn stale_save_keeps_a_newer_checkpoint_instead_of_deleting_recovery() {
    let directory = TempDir::new("session-recovery-stale-save-cleanup");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    press_ctrl(&mut session, 's');
    refuse_language_requests(&mut session);
    let save = session.take_file_request().unwrap().run();
    press(&mut session, 'b');
    let _ = session.apply_file_result(save);

    let newest = session
        .take_recovery_checkpoint()
        .expect("the stale save retains the newer live edit");
    assert!(matches!(newest.operation, RecoveryOperation::Write(_)));
    assert_eq!(newest.baseline, RecoveryBaseline::saved("aone\n"));
}

#[test]
fn a_clean_window_close_does_not_discard_a_recovery_candidate() {
    let directory = TempDir::new("session-recovery-clean-close");
    let path = directory.write("main.rs", "disk\n");
    let record_path = write_recovery_for_open_file(
        &directory,
        &path,
        RecoveryBaseline::saved("disk\n"),
        "recovered\n",
    );
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);

    run_command(&mut session, "q");
    run_recovery_work(&mut session);

    assert!(record_path.exists());
}

#[test]
fn cleanup_failure_warns_without_changing_the_successful_save_fact() {
    let directory = TempDir::new("session-recovery-cleanup-failure");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    press_ctrl(&mut session, 's');
    run_file_request(&mut session);
    let cleanup = session.take_recovery_checkpoint().unwrap();
    std::fs::create_dir_all(&cleanup.path).expect("the test blocks record deletion");
    let _ = session.apply_recovery_checkpoint(cleanup.run());

    assert!(!session.buffer().is_modified(), "the save remains current");
    assert!(message(&session).contains("written"));
    assert!(session.log.snapshot().contains("recovery"));
}

#[test]
fn save_keeps_an_active_checkpoint_as_the_barrier_for_a_newer_edit() {
    let directory = TempDir::new("session-recovery-save-barrier");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    let active = session.take_recovery_checkpoint().unwrap();

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    press(&mut session, 'b');
    assert!(
        session.take_recovery_checkpoint().is_none(),
        "the pre-save checkpoint remains the ordering barrier"
    );

    let _ = session.apply_recovery_checkpoint(active.run());
    let newest = session
        .take_recovery_checkpoint()
        .expect("the post-save edit follows the completed checkpoint");
    assert_eq!(
        match &newest.operation {
            RecoveryOperation::Write(record) => record.text(),
            RecoveryOperation::Delete => panic!("the newest operation is a write"),
        },
        "abone\n"
    );
    assert_eq!(
        newest.baseline,
        RecoveryBaseline::saved("aone\n"),
        "the successor uses the new saved baseline"
    );
}

#[test]
fn clean_save_suppresses_obsolete_work_after_the_active_completion() {
    let directory = TempDir::new("session-recovery-clean-save");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    let buffer = session.active();
    let active = session.take_recovery_checkpoint().unwrap();

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    let _ = session.apply_recovery_checkpoint(active.run());
    let cleanup = session
        .take_recovery_checkpoint()
        .expect("the clean save orders cleanup after the active write");
    let _ = session.apply_recovery_checkpoint(cleanup.run());

    assert!(session.take_recovery_checkpoint().is_none());
    assert!(
        !session.recovery.buffers.contains_key(&buffer),
        "completion removes suppressed bookkeeping"
    );
}

#[test]
fn path_retarget_keeps_the_active_checkpoint_as_the_new_target_barrier() {
    let directory = TempDir::new("session-recovery-retarget-barrier");
    let path = directory.write("main.rs", "one\n");
    let moved = directory.join("moved.rs");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    let buffer = session.active();
    let active = session.take_recovery_checkpoint().unwrap();

    let _ = session.apply_workspace_result(WorkspaceResult::Mutated {
        outcome: DurableOutcome::Committed(MutationOutcome {
            updates: vec![BufferPathUpdate {
                buffer,
                path: moved.clone(),
            }],
            changed: Vec::new(),
            selection: None,
        }),
    });
    press(&mut session, 'b');
    assert!(session.take_recovery_checkpoint().is_none());

    let _ = session.apply_recovery_checkpoint(active.run());
    let newest = session
        .take_recovery_checkpoint()
        .expect("the retargeted edit follows the old-target checkpoint");
    assert_eq!(newest.target.as_path(), moved);
    assert_eq!(
        match &newest.operation {
            RecoveryOperation::Write(record) => record.text(),
            RecoveryOperation::Delete => panic!("the newest operation is a write"),
        },
        "abone\n"
    );
}

#[test]
fn unload_with_active_recovery_work_removes_bookkeeping_on_completion() {
    let directory = TempDir::new("session-recovery-unload-barrier");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    press_code(&mut session, KeyCode::Esc);
    let buffer = session.active();
    let active = session.take_recovery_checkpoint().unwrap();

    press(&mut session, 'u');
    assert!(!session.buffer().is_modified());
    type_keys(&mut session, " x");
    assert_ne!(session.active(), buffer);
    assert!(session.recovery.buffers.contains_key(&buffer));

    let _ = session.apply_recovery_checkpoint(active.run());
    let cleanup = session
        .take_recovery_checkpoint()
        .expect("the unload orders cleanup after the active write");
    let _ = session.apply_recovery_checkpoint(cleanup.run());
    assert!(!session.recovery.buffers.contains_key(&buffer));
}

#[test]
fn cancelled_recovery_submission_preserves_successor_delete_intent() {
    let directory = TempDir::new("session-recovery-cancelled-delete");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    let active = session.take_recovery_checkpoint().unwrap();

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    let _ = session.refuse_recovery_checkpoint(active, RecoverySubmissionFailure::Cancelled);

    let cleanup = session
        .take_recovery_checkpoint()
        .expect("the cancelled active write preserves its cleanup successor");
    assert!(matches!(cleanup.operation, RecoveryOperation::Delete));
}

#[test]
fn failed_recovery_job_preserves_successor_delete_intent() {
    let directory = TempDir::new("session-recovery-failed-delete");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    let buffer = session.active();
    let _active = session.take_recovery_checkpoint().unwrap();

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    let _ = session.fail_recovery_checkpoint(buffer, "passed its deadline");

    let cleanup = session
        .take_recovery_checkpoint()
        .expect("the failed active write preserves its cleanup successor");
    assert!(matches!(cleanup.operation, RecoveryOperation::Delete));
}

#[test]
fn saturated_recovery_checkpoint_stays_queued_for_a_later_dispatch() {
    let directory = TempDir::new("session-recovery-failure");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    let checkpoint = session.take_recovery_checkpoint().unwrap();

    let _ = session.refuse_recovery_checkpoint(checkpoint, RecoverySubmissionFailure::Saturated);
    assert!(
        session.take_recovery_checkpoint().is_some(),
        "capacity saturation keeps the checkpoint ready for later driver progress"
    );
    assert!(session.take_recovery_checkpoint().is_none());
}

#[test]
fn recovery_completion_rejects_each_mismatched_checkpoint_identity() {
    let directory = TempDir::new("session-recovery-identity");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    let checkpoint = session.take_recovery_checkpoint().unwrap();

    let other_path = directory.write("other.rs", "other\n");
    session.open_path(other_path);
    run_file_request(&mut session);
    let other_target = session.active_buffer().target().unwrap().clone();
    session.active = checkpoint.buffer;

    for mismatch in 0..3 {
        let mut result = checkpoint.clone().run();
        match mismatch {
            0 => result.target = other_target.clone(),
            1 => result.baseline = RecoveryBaseline::Missing,
            2 => result.revision = BufferRevision::from_parts(99, 99),
            _ => unreachable!(),
        }
        let _ = session.apply_recovery_checkpoint(result);
        assert!(session.take_recovery_checkpoint().is_none());
    }

    let _ = session.apply_recovery_checkpoint(checkpoint.run());
}

#[test]
fn recovery_checkpoints_are_independent_between_buffers() {
    let directory = TempDir::new("session-recovery-buffers");
    let first_path = directory.write("first.rs", "one\n");
    let second_path = directory.write("second.rs", "two\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(first_path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    press_code(&mut session, KeyCode::Esc);
    let first = session.take_recovery_checkpoint().unwrap();

    session.open_path(second_path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'b');
    press_code(&mut session, KeyCode::Esc);
    let second = session.take_recovery_checkpoint().unwrap();
    assert_ne!(first.buffer, second.buffer);

    let _ = session.apply_recovery_checkpoint(second.run());
    let _ = session.apply_recovery_checkpoint(first.run());
}

#[test]
fn wrong_instance_recovery_completion_cannot_advance_checkpoint_state() {
    let directory = TempDir::new("session-recovery-instance");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));
    session.open_path(path);
    run_file_request(&mut session);
    press(&mut session, 'i');
    press(&mut session, 'a');
    let checkpoint = session.take_recovery_checkpoint().unwrap();
    let mut result = checkpoint.run();
    result.instance = EditorInstanceId::allocate();

    let _ = session.apply_recovery_checkpoint(result);
    assert!(
        session.take_recovery_checkpoint().is_none(),
        "a wrong-instance completion leaves the active checkpoint submitted"
    );
}

#[test]
fn a_path_opens_one_buffer_and_ctrl_s_writes_it() {
    let directory = TempDir::new("session-save");
    let path = directory.write("main.rs", "fn main() {}\n");
    let mut session = file_session(&directory.path);

    session.open_path(path.clone());
    run_file_request(&mut session);
    assert_eq!(
        session.buffers().len(),
        2,
        "the file joins the scratch buffer"
    );
    assert_eq!(session.buffer().to_string(), "fn main() {}\n");
    assert_eq!(session.active_buffer().name(), "main.rs");
    assert!(!session.buffer().is_modified());

    press(&mut session, 'i');
    type_keys(&mut session, "// note");
    press_code(&mut session, KeyCode::Enter);
    assert!(session.buffer().is_modified());

    // `Ctrl-S` saves from every mode and forces no mode transition.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert_eq!(session.mode(), Mode::Insert);
    run_file_request(&mut session);
    assert_eq!(session.mode(), Mode::Insert);
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file exists"),
        "// note\nfn main() {}\n"
    );
    assert!(
        !session.buffer().is_modified(),
        "a successful save clears the dirty state"
    );

    // The saved buffer leaves the editor without a refusal.
    press(&mut session, ':');
    type_keys(&mut session, "q");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn a_path_outside_the_explicit_worktree_starts_no_file_operation() {
    let directory = TempDir::new("session-confined-root");
    let outside = TempDir::new("session-confined-outside");
    let path = outside.write("main.rs", "outside\n");
    let mut session = file_session(&directory.path);

    session.open_path(path);

    assert!(session.take_file_request().is_none());
    assert_eq!(session.buffers().len(), 1);
    assert_eq!(session.buffer().to_string(), "\n");
    assert!(session.message().is_some_and(|message| {
        message
            .text()
            .ends_with(": the path is outside the worktree")
    }));
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );
}

#[test]
fn current_directory_components_open_from_the_cli_and_edit_command() {
    let directory = TempDir::new("session-current-directory-path");
    directory.write("main.rs", "main\n");
    directory.write("other.rs", "other\n");
    let mut session = file_session(&directory.path);

    session.open_path(PathBuf::from("./main.rs"));
    run_file_request(&mut session);
    assert_eq!(session.buffer().to_string(), "main\n");

    press(&mut session, ':');
    type_keys(&mut session, "e ./other.rs");
    press_code(&mut session, KeyCode::Enter);
    run_file_request(&mut session);
    assert_eq!(session.buffer().to_string(), "other\n");
}

#[test]
fn one_file_reaches_one_buffer_however_the_user_spells_its_path() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new("session-duplicate");
    let path = directory.write("main.rs", "one\n");
    let link = directory.join("linked.rs");
    symlink("main.rs", &link).expect("the temporary directory supports links");
    let mut session = file_session(&directory.path);

    session.open_path(path);
    run_file_request(&mut session);
    let first = session.active();
    let loaded_path = session
        .active_buffer()
        .path()
        .expect("the buffer holds the file")
        .to_path_buf();

    let remaining = BUFFERS_MAX - session.buffers().len();
    for index in 0..remaining {
        let other = directory.write(&format!("other-{index}.rs"), "other\n");
        session.open_path(other);
        run_file_request(&mut session);
    }
    assert_eq!(session.buffers().len(), BUFFERS_MAX);

    // The canonical display path reuses the buffer without a filesystem read,
    // even when no new buffer can enter the list.
    session.open_path(loaded_path);
    assert!(session.take_file_request().is_none());
    assert_eq!(session.active(), first);

    // A contained symbolic-link spelling also deduplicates after its read at
    // capacity, because publication checks identity before insertion.
    session.open_path(link);
    run_file_request(&mut session);
    assert_eq!(session.active(), first);
    assert_eq!(session.buffers().len(), BUFFERS_MAX);
}

#[test]
fn a_conflict_keeps_the_buffer_dirty_and_usable() {
    let directory = TempDir::new("session-conflict");
    let path = directory.write("main.rs", "one\n");
    let mut session = file_session(&directory.path);

    session.open_path(path.clone());
    run_file_request(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "two");
    press_code(&mut session, KeyCode::Esc);

    std::fs::write(&path, "another program wrote this\n").expect("the file is writable");
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);

    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file exists"),
        "another program wrote this\n",
        "a conflict never overwrites the file"
    );
    assert!(session.buffer().is_modified());

    // The buffer stays usable after the refused save.
    press(&mut session, 'o');
    type_keys(&mut session, "three");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "twoone\nthree\n");
}

#[cfg(unix)]
#[test]
fn a_confinement_failure_writes_nothing_and_keeps_the_live_buffer_dirty() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new("session-save-confinement");
    let path = directory.write("main.rs", "one\n");
    let replacement = directory.write("replacement.rs", "replacement\n");
    let mut session = file_session(&directory.path);
    session.open_path(path.clone());
    run_file_request(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "edited ");
    press_code(&mut session, KeyCode::Esc);
    std::fs::remove_file(&path).expect("the target can be replaced");
    symlink("replacement.rs", &path).expect("the temporary directory supports links");

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);

    assert!(session.buffer().is_modified());
    assert_eq!(session.buffer().to_string(), "edited one\n");
    assert_eq!(
        std::fs::read_to_string(replacement).expect("the replacement remains readable"),
        "replacement\n"
    );
    assert!(
        std::fs::symlink_metadata(path)
            .expect("the link remains")
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_failed_save_keeps_the_buffer_usable() {
    let directory = TempDir::new("session-failure");
    let mut session = file_session(&directory.path);

    // The path holds no file yet, so the open starts a new empty buffer. Its
    // directory is missing, so no write can succeed.
    session.open_path(directory.join("missing").join("main.rs"));
    run_file_request(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "text");
    press_code(&mut session, KeyCode::Esc);

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );
    assert!(session.buffer().is_modified());
    assert_eq!(session.buffer().to_string(), "text\n");
}

#[test]
fn write_quit_saves_the_buffer_and_then_ends_the_editor() {
    let directory = TempDir::new("session-write-quit");
    let path = directory.write("main.rs", "one\n");
    let mut session = file_session(&directory.path);

    session.open_path(path.clone());
    run_file_request(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "two ");
    press_code(&mut session, KeyCode::Esc);

    press(&mut session, ':');
    type_keys(&mut session, "wq");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        question(&session),
        "",
        "the save keeps every change, so `:wq` asks nothing"
    );
    assert_eq!(
        session.run_state(),
        RunState::Running,
        "the editor waits for the save result"
    );
    run_file_request(&mut session);

    assert_eq!(
        std::fs::read_to_string(&path).expect("the file exists"),
        "two one\n"
    );
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn write_quit_keeps_a_newer_edit_when_the_save_result_is_stale() {
    let directory = TempDir::new("session-stale-write-quit");
    let path = directory.write("main.rs", "one\n");
    let mut session =
        file_session(&directory.path).with_recovery_state_directory(directory.join("state"));

    session.open_path(path.clone());
    run_file_request(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "saved ");
    press_code(&mut session, KeyCode::Esc);
    run_recovery_work(&mut session);

    press(&mut session, ':');
    type_keys(&mut session, "wq");
    press_code(&mut session, KeyCode::Enter);
    refuse_language_requests(&mut session);
    let request = session
        .take_file_request()
        .expect("write-quit queued one save request");
    let result = request.run();

    press(&mut session, 'o');
    type_keys(&mut session, "newer");
    press_code(&mut session, KeyCode::Esc);
    let _ = session.apply_file_result(result);

    assert_eq!(
        std::fs::read_to_string(&path).expect("the saved snapshot reached disk"),
        "saved one\n"
    );
    assert_eq!(session.buffer().to_string(), "saved one\nnewer\n");
    assert!(session.buffer().is_modified());
    assert_eq!(session.run_state(), RunState::Running);
    assert!(message(&session).ends_with(" 1L, 10B written"));

    let checkpoint = session
        .take_recovery_checkpoint()
        .expect("the newer edit remains queued after the stale save");
    assert_eq!(
        checkpoint.baseline,
        RecoveryBaseline::saved("saved one\n"),
        "recovery compares against the exact stale snapshot that reached disk"
    );

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    assert_eq!(
        std::fs::read_to_string(&path).expect("the newer save reached disk"),
        "saved one\nnewer\n"
    );
    assert!(!session.buffer().is_modified());
}

#[test]
fn an_indeterminate_save_keeps_dirty_state_and_queues_reconciliation() {
    let (directory, path, mut session) =
        opened_file("session-save-indeterminate", "main.rs", "one\n");
    press(&mut session, 'i');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    refuse_language_requests(&mut session);
    let request = session.take_file_request().expect("the save was queued");
    let FileRequest::Save(request) = request else {
        panic!("the request is a save");
    };
    let report = kvim_workspace::Indeterminate::new(
        SaveError::Write(std::io::Error::other("metadata failed")),
        Vec::new(),
        vec![path.clone()],
    )
    .expect("the report stays inside its collection bounds");
    let _ = session.apply_file_result(FileResult::Saved {
        buffer: request.buffer,
        requested: request.target,
        outcome: DurableOutcome::Indeterminate(report),
    });

    assert!(session.buffer().is_modified());
    assert!(
        matches!(
            session.take_workspace_request(),
            Some(kvim_workspace::WorkspaceRequest::ReadDirectory { .. })
        ),
        "tree reconciliation queued"
    );
    assert!(
        session.take_file_request().is_some(),
        "reload reconciliation queued"
    );
    let events = std::iter::from_fn(|| session.take_event()).collect::<Vec<_>>();
    assert!(events.iter().any(|published| matches!(
        published.event,
        crate::__private::EditorEvent::SaveReconciliationRequired { .. }
    )));
    assert!(!events.iter().any(|published| matches!(
        published.event,
        crate::__private::EditorEvent::FileWritten { .. }
    )));
    assert!(message(&session).contains("cannot prove save"));
    drop(directory);
}

#[test]
fn write_quit_keeps_an_undone_stale_save_dirty_and_open() {
    let directory = TempDir::new("session-undone-stale-write-quit");
    let path = directory.write("main.rs", "one\n");
    let mut session = file_session(&directory.path);

    session.open_path(path.clone());
    run_file_request(&mut session);
    press(&mut session, 'i');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);

    press(&mut session, ':');
    type_keys(&mut session, "wq");
    press_code(&mut session, KeyCode::Enter);
    refuse_language_requests(&mut session);
    let request = session
        .take_file_request()
        .expect("write-quit queued one save request");
    let result = request.run();
    let saved_revision = match &result {
        FileResult::Saved {
            outcome: DurableOutcome::Committed(saved),
            ..
        } => saved.revision,
        other => panic!("the save succeeds, got {other:?}"),
    };

    press(&mut session, 'i');
    type_keys(&mut session, "y");
    press_code(&mut session, KeyCode::Esc);
    press(&mut session, 'u');
    press(&mut session, 'u');
    assert_eq!(session.buffer().to_string(), "one\n");
    assert!(
        !session.buffer().is_modified(),
        "the text history returned to its old saved position"
    );
    assert_ne!(session.buffer().revision(), saved_revision);

    let _ = session.apply_file_result(result);

    assert_eq!(
        std::fs::read_to_string(path).expect("the request snapshot reached disk"),
        "xone\n"
    );
    assert_eq!(session.buffer().to_string(), "one\n");
    assert!(session.active_buffer().is_modified());
    assert_eq!(session.run_state(), RunState::Running);
}

#[test]
fn space_x_unloads_a_clean_buffer_and_refuses_a_dirty_buffer() {
    let directory = TempDir::new("session-unload");
    let path = directory.write("main.rs", "one\n");
    let mut session = file_session(&directory.path);

    session.open_path(path);
    run_file_request(&mut session);
    let loaded = session.active();

    // Insert mode records one transaction for each key, so one undo reverses
    // one character.
    press(&mut session, 'i');
    type_keys(&mut session, "z");
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, " x");
    assert_eq!(session.active(), loaded, "a dirty buffer stays loaded");
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );

    press(&mut session, 'u');
    assert!(!session.buffer().is_modified());
    type_keys(&mut session, " x");
    assert_ne!(session.active(), loaded);
    assert_eq!(session.buffers().len(), 1);
    assert_eq!(
        session.windows().buffer(session.windows().focused_window()),
        Some(session.active()),
        "every window follows the unload"
    );
}

/// Reports one workspace change, like the coalesced burst of the watcher.
///
/// A content change names no path at all, so one burst asks the session to
/// check every loaded buffer against its file.
///
/// `root` is the worktree root of the session under test. The session drops a
/// burst of another root, so a burst that names the working directory of the
/// test process would reach no buffer of a session over a temporary directory.
fn report_watch_change(session: &mut Session, root: &Path) -> Redraw {
    let watched = test_root(root.to_path_buf());
    let mut batch = WatchBatch::default();
    batch.push(
        &WatchEvent::new(watched, root.join("changed"), WatchKind::Modified)
            .expect("the event lies below the session root"),
    );
    session.apply_watch_batch(&batch)
}

/// Runs the reload check that one workspace change queued.
fn run_watch_reload(session: &mut Session, root: &Path) {
    let _ = report_watch_change(session, root);
    let request = session
        .take_file_request()
        .expect("the burst queued one reload check");
    let _ = session.apply_file_result(request.run());
}

/// Returns the external-change marker of the active buffer.
fn external(session: &Session) -> Option<ExternalChange> {
    session.active_buffer().external_change()
}

/// Opens one file in a session that keeps no persistent undo file.
fn opened_file(label: &str, name: &str, text: &str) -> (TempDir, PathBuf, Session) {
    let directory = TempDir::new(label);
    let path = directory.write(name, text);
    let mut session = file_session(&directory.path);
    session.open_path(path.clone());
    run_file_request(&mut session);
    (directory, path, session)
}

#[test]
fn status_summarizes_diagnostics_by_semantic_severity() {
    let diagnostic = |severity| Diagnostic {
        span: SourceSpan::new(DocumentPosition::new(0, 0), DocumentPosition::new(0, 1)),
        severity,
        message: "status test".to_owned(),
        source: "test".to_owned(),
    };
    let diagnostics = [
        diagnostic(DiagnosticSeverity::Error),
        diagnostic(DiagnosticSeverity::Warning),
        diagnostic(DiagnosticSeverity::Information),
        diagnostic(DiagnosticSeverity::Hint),
        diagnostic(DiagnosticSeverity::Error),
    ];

    assert_eq!(
        super::diagnostic_summary(&diagnostics),
        super::EditorDiagnosticSummary {
            errors: 2,
            warnings: 1,
            information: 1,
            hints: 1,
        }
    );
}

#[test]
fn a_dirty_buffer_never_reloads_and_reports_the_external_change_once() {
    let (directory, path, mut session) = opened_file("session-reload-dirty", "main.rs", "one\n");

    press(&mut session, 'i');
    type_keys(&mut session, "edited ");
    press_code(&mut session, KeyCode::Esc);
    assert!(session.buffer().is_modified());

    std::fs::write(&path, "another program wrote a much longer line\n")
        .expect("the file is writable");
    run_watch_reload(&mut session, &directory.path);

    assert_eq!(
        session.buffer().to_string(),
        "edited one\n",
        "a buffer with unsaved changes never reloads"
    );
    assert!(session.buffer().is_modified());
    assert_eq!(external(&session), Some(ExternalChange::Changed));
    assert_eq!(
        message(&session),
        "main.rs changed on disk; the buffer keeps its unsaved changes"
    );

    // The editor reports one external change once, so a workspace that changes
    // often never fills the message line.
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(message(&session), "");
    run_watch_reload(&mut session, &directory.path);
    assert_eq!(message(&session), "");
    assert_eq!(session.buffer().to_string(), "edited one\n");
}

#[test]
fn a_clean_buffer_reloads_after_an_external_change() {
    let (directory, path, mut session) = opened_file("session-reload-clean", "main.rs", "one\n");

    // A file that keeps its length reports no change, so the test changes it.
    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(message(&session), "", "the open message is cleared");
    run_watch_reload(&mut session, &directory.path);

    assert_eq!(session.buffer().to_string(), "one\ntwo\n");
    assert!(!session.buffer().is_modified());
    assert_eq!(
        session
            .status()
            .path
            .and_then(|path| path.as_path().file_name()),
        Some(std::ffi::OsStr::new("main.rs")),
    );
    assert!(!session.status().modified);
    assert_eq!(external(&session), None);
    assert_eq!(message(&session), "", "a background reload reports nothing");

    // The reload recorded the new file state, so the next save is no conflict.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    assert!(!session.buffer().is_modified());
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file exists"),
        "one\ntwo\n"
    );
}

#[test]
fn a_buffer_that_no_window_shows_reloads_in_the_background() {
    let directory = TempDir::new("session-reload-background");
    let first = directory.write("first.rs", "first\n");
    let second = directory.write("second.rs", "second\n");
    let mut session = file_session(&directory.path);

    session.open_path(second.clone());
    run_file_request(&mut session);
    let background = session.active();
    session.open_path(first);
    run_file_request(&mut session);
    assert_ne!(session.active(), background);

    std::fs::write(&second, "second, and changed\n").expect("the file is writable");
    run_watch_reload(&mut session, &directory.path);

    let reloaded = session
        .buffers()
        .get(background)
        .expect("the background buffer stays loaded");
    assert_eq!(reloaded.text().to_string(), "second, and changed\n");
    assert!(!reloaded.text().is_modified());
}

#[test]
fn a_reload_keeps_the_cursor_and_clamps_it_into_a_shorter_file() {
    let (directory, path, mut session) = opened_file(
        "session-reload-cursor",
        "main.rs",
        "one\ntwo\nthree\nfour\nfive\n",
    );

    type_keys(&mut session, "jj");
    assert_eq!(session.cursor().line().get(), 2);

    // A file that keeps the cursor line keeps the cursor.
    std::fs::write(&path, "one\ntwo\nthree, longer\nfour\nfive\n").expect("the file is writable");
    run_watch_reload(&mut session, &directory.path);
    assert_eq!(session.cursor().line().get(), 2);

    // A file that became shorter clamps the cursor and the viewport.
    std::fs::write(&path, "one\n").expect("the file is writable");
    run_watch_reload(&mut session, &directory.path);
    assert_eq!(session.buffer().to_string(), "one\n");
    assert_eq!(session.cursor().line().get(), 0);
    assert_eq!(
        session
            .windows()
            .state(session.windows().focused_window())
            .expect("the focused window is a leaf")
            .first_line(),
        0
    );
}

#[test]
fn a_deleted_file_keeps_its_buffer_editable_and_reports_it() {
    let (directory, path, mut session) = opened_file("session-reload-deleted", "main.rs", "one\n");

    std::fs::remove_file(&path).expect("the file exists");
    run_watch_reload(&mut session, &directory.path);

    assert_eq!(
        session.buffer().to_string(),
        "one\n",
        "the buffer holds the only remaining copy"
    );
    assert_eq!(external(&session), Some(ExternalChange::Missing));
    assert_eq!(
        message(&session),
        "main.rs is gone from disk; the buffer keeps the only copy"
    );

    // The buffer stays editable, and a save writes the file again.
    press(&mut session, 'i');
    type_keys(&mut session, "kept ");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "kept one\n");
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    run_file_request(&mut session);
    assert_eq!(
        std::fs::read_to_string(&path).expect("the save wrote the file again"),
        "kept one\n"
    );
    assert_eq!(external(&session), None, "the save cleared the marker");
}

#[test]
fn a_renamed_file_reaches_the_same_missing_state() {
    let (directory, path, mut session) = opened_file("session-reload-renamed", "main.rs", "one\n");

    std::fs::rename(&path, directory.join("other.rs")).expect("the file exists");
    run_watch_reload(&mut session, &directory.path);

    assert_eq!(session.buffer().to_string(), "one\n");
    assert_eq!(external(&session), Some(ExternalChange::Missing));
    assert!(!session.buffer().is_modified());
}

#[test]
fn a_reload_reaches_the_language_server_with_the_reloaded_text() {
    let (directory, path, mut session) =
        opened_file("session-reload-language", "main.rs", "fn main() {}\n");

    std::fs::write(&path, "fn main() { println!(); }\n").expect("the file is writable");
    let _ = report_watch_change(&mut session, &directory.path);
    let request = session
        .take_file_request()
        .expect("the burst queued one reload check");
    refuse_language_requests(&mut session);
    let _ = session.apply_file_result(request.run());

    let synchronization = session
        .take_language_request()
        .expect("the reload synchronizes the document");
    match synchronization {
        LanguageRequest::Open { revision, text, .. } => {
            assert_eq!(&*text, "fn main() { println!(); }\n");
            assert_eq!(
                revision,
                session.buffer().revision(),
                "the server copy carries the revision of the reloaded text"
            );
        }
        other => panic!("a reload opens the document again, not {other:?}"),
    }
}

#[test]
fn a_generation_zero_analysis_is_rejected_after_reload_to_generation_one() {
    let (directory, path, mut session) = opened_file(
        "session-reload-analysis-generation",
        "main.rs",
        "fn old() {}\n",
    );
    let buffer = session.active();
    let bytes_max = session.buffer().bytes_max();
    let old = session
        .take_analysis_request()
        .expect("the generation-zero text needs analysis");

    std::fs::write(&path, "fn new() {}\n").expect("the file is writable");
    run_watch_reload(&mut session, &directory.path);

    assert_eq!(
        session.active(),
        buffer,
        "reload keeps the stable buffer identity"
    );
    assert_eq!(session.buffer().revision().generation().get(), 1);
    assert_eq!(session.buffer().version().get(), 0);
    assert_eq!(session.buffer().bytes_max(), bytes_max);

    let cancellation = CancellationToken::new();
    assert_eq!(
        session.apply_analysis_result(old.run(&cancellation)),
        Redraw::Skipped,
        "a generation-zero result cannot publish into generation one"
    );
}

#[test]
fn an_obsolete_reload_result_never_replaces_the_buffer() {
    let (directory, path, mut session) = opened_file("session-reload-obsolete", "main.rs", "one\n");

    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");
    let _ = report_watch_change(&mut session, &directory.path);
    let request = session
        .take_file_request()
        .expect("the burst queued one reload check");
    let result = request.run();

    // The user edits the buffer while the check runs, so its outcome describes
    // a buffer state that the editor already left.
    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    let _ = session.apply_file_result(result);

    assert_eq!(session.buffer().to_string(), "typed one\n");
    assert!(session.buffer().is_modified());
}

#[test]
fn a_reload_result_for_a_moved_target_is_obsolete() {
    let (directory, path, mut session) = opened_file("session-reload-moved", "main.rs", "one\n");
    let buffer = session.active();

    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");
    let _ = report_watch_change(&mut session, &directory.path);
    let request = session
        .take_file_request()
        .expect("the burst queued one reload check");
    let result = request.run();

    let moved = directory.join("moved.rs");
    std::fs::rename(path, &moved).expect("the file can move inside the worktree");
    let _ = session.apply_workspace_result(WorkspaceResult::Mutated {
        outcome: DurableOutcome::Committed(MutationOutcome {
            updates: vec![BufferPathUpdate {
                buffer,
                path: moved.clone(),
            }],
            changed: Vec::new(),
            selection: None,
        }),
    });
    let _ = session.apply_file_result(result);

    assert_eq!(session.active_buffer().path(), Some(moved.as_path()));
    assert_eq!(session.buffer().to_string(), "one\n");
    assert_eq!(external(&session), None);
}

#[test]
fn a_file_that_grew_past_the_size_limit_keeps_its_buffer() {
    let directory = TempDir::new("session-reload-limit");
    let path = directory.write("main.rs", "one\n");
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    settings.files.max_file_bytes = 8;
    let mut session = Session::new(
        Rect::new(0, 0, 80, 24),
        settings,
        test_root(directory.path.clone()),
    );

    session.open_path(path.clone());
    run_file_request(&mut session);
    assert_eq!(session.buffer().to_string(), "one\n");

    std::fs::write(&path, "far above the limit\n").expect("the file is writable");
    run_watch_reload(&mut session, &directory.path);

    assert_eq!(session.buffer().to_string(), "one\n");
    assert_eq!(external(&session), Some(ExternalChange::Changed));
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );
}

#[test]
fn the_edit_command_reloads_a_clean_buffer_and_asks_before_a_dirty_one() {
    let (_directory, path, mut session) = opened_file("session-reload-command", "main.rs", "one\n");

    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");
    press(&mut session, ':');
    type_keys(&mut session, "e");
    press_code(&mut session, KeyCode::Enter);
    run_file_request(&mut session);
    assert_eq!(session.buffer().to_string(), "one\ntwo\n");
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Info)
    );

    // A buffer with unsaved changes asks before the file replaces its text.
    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    press(&mut session, ':');
    type_keys(&mut session, "e");
    press_code(&mut session, KeyCode::Enter);
    assert!(
        session.take_file_request().is_none(),
        "the open question reads no file"
    );
    assert_eq!(
        question(&session),
        "Reload main.rs and discard the unsaved changes",
        "the question names the buffer"
    );
    assert_eq!(session.buffer().to_string(), "typed one\ntwo\n");

    // A lone `y` reads no file, because it performs no action.
    press(&mut session, 'y');
    assert!(
        session.take_file_request().is_none(),
        "one keypress reloads nothing"
    );
    press_code(&mut session, KeyCode::Enter);
    run_file_request(&mut session);
    assert_eq!(session.buffer().to_string(), "one\ntwo\n");
    assert!(!session.buffer().is_modified());
}

#[test]
fn a_cancelled_reload_keeps_the_buffer_and_its_unsaved_text() {
    // `n` names the default of the question, the empty text names it as well,
    // and `no` and `ya` stand for every remaining answer.
    for value in ["n", "", "no", "ya"] {
        let (_directory, path, mut session) =
            opened_file("session-reload-cancel", "main.rs", "one\n");
        std::fs::write(&path, "one\ntwo\n").expect("the file is writable");
        press(&mut session, 'i');
        type_keys(&mut session, "typed ");
        press_code(&mut session, KeyCode::Esc);

        press(&mut session, ':');
        type_keys(&mut session, "e");
        press_code(&mut session, KeyCode::Enter);
        answer(&mut session, value);

        assert!(
            session.take_file_request().is_none(),
            "{value:?} reads no file"
        );
        assert_eq!(
            session.buffer().to_string(),
            "typed one\n",
            "{value:?} keeps the unsaved text"
        );
        assert!(
            session.buffer().is_modified(),
            "{value:?} keeps the changes"
        );
        assert_eq!(question(&session), "", "{value:?} closes the question");
        assert_eq!(message(&session), "", "{value:?} leaves no trace");
    }
}

#[test]
fn a_confirmed_quit_keeps_the_editor_running_after_another_buffer_became_active() {
    let (directory, _path, mut session) = opened_file("session-quit-moved", "main.rs", "one\n");
    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    let asked = session.active();

    press(&mut session, ':');
    type_keys(&mut session, "q");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        question(&session),
        "Quit and discard the unsaved changes of main.rs"
    );

    // One open completes while the question waits, so another buffer becomes
    // active. The user approved no loss of that buffer.
    session.open_path(directory.write("other.rs", "other\n"));
    run_file_request(&mut session);
    assert_ne!(session.active(), asked, "the open moved the focus");

    answer(&mut session, "y");

    assert_eq!(
        session.run_state(),
        RunState::Running,
        "the answer quits only while the named buffer holds the focus"
    );
    assert_eq!(
        message(&session),
        "the focused window shows another buffer now, so the editor kept running"
    );
}

#[test]
fn a_confirmed_reload_reads_the_file_of_the_buffer_that_the_question_named() {
    let (directory, path, mut session) = opened_file("session-reload-moved", "main.rs", "one\n");
    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    let asked = session.active();
    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");

    press(&mut session, ':');
    type_keys(&mut session, "e");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        question(&session),
        "Reload main.rs and discard the unsaved changes"
    );

    // One open completes while the question waits, so another buffer becomes
    // active. The answer still reads the file of the named buffer.
    session.open_path(directory.write("other.rs", "other\n"));
    run_file_request(&mut session);
    assert_ne!(session.active(), asked, "the open moved the focus");

    answer(&mut session, "y");
    run_file_request(&mut session);

    let reloaded = session
        .buffers()
        .get(asked)
        .expect("the named buffer stays loaded");
    assert_eq!(reloaded.text().to_string(), "one\ntwo\n");
    assert!(!reloaded.is_modified());
    assert_eq!(
        session.buffer().to_string(),
        "other\n",
        "the reload replaced no other buffer"
    );
}

#[test]
fn the_forced_edit_command_discards_the_unsaved_changes_and_reloads() {
    let (_directory, path, mut session) = opened_file("session-reload-forced", "main.rs", "one\n");

    press(&mut session, 'i');
    type_keys(&mut session, "typed ");
    press_code(&mut session, KeyCode::Esc);
    std::fs::write(&path, "one\ntwo\n").expect("the file is writable");

    press(&mut session, ':');
    type_keys(&mut session, "e!");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(question(&session), "", "`:e!` asks nothing");
    run_file_request(&mut session);

    assert_eq!(session.buffer().to_string(), "one\ntwo\n");
    assert!(!session.buffer().is_modified());
    assert_eq!(external(&session), None);
}

/// Returns the highlight spans that the frame reads for the active buffer.
fn highlights(session: &Session) -> usize {
    session.visible().highlights(session.active()).len()
}

/// Opens one file in a session that keeps no persistent undo file.
fn opened(name: &str, text: &str) -> (TempDir, Session) {
    let directory = TempDir::new("session-language");
    let path = directory.write(name, text);
    let mut session = file_session(&directory.path);
    session.open_path(path);
    run_file_request(&mut session);
    (directory, session)
}

/// Runs the queued analysis job, like the event loop and the worker service.
fn run_analysis(session: &mut Session) -> Redraw {
    let request = session
        .take_analysis_request()
        .expect("the buffer needs one analysis");
    let result = request.run(&CancellationToken::new());
    session.apply_analysis_result(result)
}

#[test]
fn an_accepted_analysis_reaches_the_view_and_an_obsolete_one_is_rejected() {
    let (_directory, mut session) = opened("main.rs", "fn main() {}\n");
    assert_eq!(highlights(&session), 0, "no result is accepted yet");

    let request = session
        .take_analysis_request()
        .expect("a Rust buffer needs one analysis");
    assert_eq!(request.buffer(), session.active());
    assert!(
        session.take_analysis_request().is_none(),
        "one analysis runs at a time"
    );
    let obsolete = request.run(&CancellationToken::new());

    // One edit moves the buffer past the version that the job read.
    press(&mut session, 'o');
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        session.apply_analysis_result(obsolete),
        Redraw::Skipped,
        "an obsolete buffer version changes nothing"
    );
    assert_eq!(
        highlights(&session),
        0,
        "an obsolete result enters no cache"
    );

    // The next job reads the current version, so its spans reach the view.
    assert_eq!(run_analysis(&mut session), Redraw::Needed);
    assert!(highlights(&session) > 0);
    assert!(
        session.take_analysis_request().is_none(),
        "the accepted result already describes the current version"
    );
}

#[test]
fn a_burst_of_obsolete_analyses_costs_one_log_entry_and_keeps_an_earlier_report() {
    let (_directory, mut session) = opened("main.rs", "fn main() {}\n");

    // One report reaches the message line before the burst starts.
    press(&mut session, ':');
    type_keys(&mut session, "nosuchcommand");
    press_code(&mut session, KeyCode::Enter);
    let earlier = message(&session);
    assert!(!earlier.is_empty(), "the command line rejected the command");

    // The user types while every analysis runs, so every result is obsolete.
    let burst = LOG_ENTRIES_MAX + 16;
    for _ in 0..burst {
        let request = session
            .take_analysis_request()
            .expect("the changed buffer needs one analysis");
        let obsolete = request.run(&CancellationToken::new());
        press(&mut session, 'o');
        press_code(&mut session, KeyCode::Esc);
        assert_eq!(
            session.apply_analysis_result(obsolete),
            Redraw::Skipped,
            "an obsolete buffer version changes nothing"
        );
    }

    let rows = open_log(&mut session, "logs");
    let jobs: Vec<&String> = rows.iter().filter(|row| row.contains(" JOB ")).collect();
    assert_eq!(
        jobs.len(),
        1,
        "the whole burst costs one entry, but the log holds {jobs:?}"
    );
    assert!(
        jobs[0].ends_with(&format!("analysis rejected: the buffer changed (x{burst})")),
        "the entry names its job, its outcome, and its count, not {:?}",
        jobs[0]
    );
    assert!(
        rows.iter().any(|row| row.ends_with(&earlier)),
        "the report from before the burst is still in {rows:?}"
    );
}

#[test]
fn a_buffer_without_an_adapter_needs_no_analysis_and_stays_editable() {
    let (_directory, mut session) = opened("notes.txt", "plain text\n");
    assert!(session.take_analysis_request().is_none());
    assert_eq!(highlights(&session), 0);

    press(&mut session, 'i');
    type_keys(&mut session, "more ");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "more plain text\n");
}

#[test]
fn space_slash_toggles_the_line_comment_of_the_language_adapter() {
    let (_directory, mut session) = opened("main.rs", "fn main() {}\n");
    type_keys(&mut session, " /");
    assert_eq!(session.buffer().to_string(), "// fn main() {}\n");

    // The toggle is one transaction, so one undo reverses it.
    press(&mut session, 'u');
    assert_eq!(session.buffer().to_string(), "fn main() {}\n");

    // A Visual Line selection toggles every selected line.
    let (_directory, mut session) = opened("pair.rs", "let a = 1;\nlet b = 2;\n");
    type_keys(&mut session, "Vj /");
    assert_eq!(
        session.buffer().to_string(),
        "// let a = 1;\n// let b = 2;\n"
    );
}

#[test]
fn a_comment_toggle_without_an_adapter_changes_nothing_and_reports_why() {
    let (_directory, mut session) = opened("notes.txt", "plain text\n");
    type_keys(&mut session, " /");
    assert_eq!(session.buffer().to_string(), "plain text\n");
    assert_eq!(
        message(&session),
        "no language adapter provides a line-comment token for this buffer"
    );
}

#[test]
fn the_syntax_indent_opens_a_line_one_level_deeper_inside_a_block() {
    let (_directory, mut session) = opened("block.rs", "fn main() {\n}\n");

    // Without a parse result the previous-line rule keeps column zero.
    press(&mut session, 'o');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "fn main() {\nx\n}\n");
    press(&mut session, 'u');
    press(&mut session, 'u');
    assert_eq!(session.buffer().to_string(), "fn main() {\n}\n");

    // With the accepted analysis the new line follows the syntax tree.
    run_analysis(&mut session);
    type_keys(&mut session, "gg");
    press(&mut session, 'o');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "fn main() {\n    x\n}\n");

    // `Enter` reads the same rule, and a closing delimiter loses one level.
    run_analysis(&mut session);
    type_keys(&mut session, "A");
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "y");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        session.buffer().to_string(),
        "fn main() {\n    x\n    y\n}\n"
    );
}

/// One attribute set inside another, so one level and two levels both appear.
///
/// The text names no `let`, so every level follows from the attribute sets
/// alone.
const NESTED_ATTRIBUTE_SETS: &str = "{\n  a = {\n    b = 1;\n  };\n}\n";

#[test]
fn the_language_adapter_declares_the_width_of_one_indent_level() {
    // Nix indents with two columns, while the settings tab width is four.
    let (_directory, mut session) = opened("one.nix", NESTED_ATTRIBUTE_SETS);
    run_analysis(&mut session);
    type_keys(&mut session, "gg");
    press(&mut session, 'o');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        session.buffer().to_string(),
        "{\n  x\n  a = {\n    b = 1;\n  };\n}\n"
    );

    // Two levels take twice the width of the language.
    let (_directory, mut session) = opened("two.nix", NESTED_ATTRIBUTE_SETS);
    run_analysis(&mut session);
    type_keys(&mut session, "ggj");
    press(&mut session, 'o');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(
        session.buffer().to_string(),
        "{\n  a = {\n    x\n    b = 1;\n  };\n}\n"
    );
}

/// One reported module, trimmed to the smallest shape that carries a `let`, an
/// `in` body, and one nested attribute set.
const NIX_LET_MODULE: &str = concat!(
    "{ config, pkgs, ... }:\n",
    "\n",
    "let\n",
    "  guard = 1;\n",
    "in\n",
    "{\n",
    "  xdg.configFile.\"keel\" = {\n",
    "    recursive = true;\n",
    "  };\n",
    "}\n",
);

#[test]
fn a_new_line_after_the_last_binding_of_a_nix_let_body_takes_one_level() {
    let (_directory, mut session) = opened("home.nix", NIX_LET_MODULE);
    run_analysis(&mut session);

    // `G` reaches the closing brace of the file, and `k` reaches the `};` that
    // closes the last attribute set. The `let` spans that body, so counting the
    // level of the `let` as well would open the new line eight columns deep.
    type_keys(&mut session, "G");
    press(&mut session, 'k');
    press(&mut session, 'o');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);

    assert_eq!(
        session.buffer().to_string(),
        concat!(
            "{ config, pkgs, ... }:\n",
            "\n",
            "let\n",
            "  guard = 1;\n",
            "in\n",
            "{\n",
            "  xdg.configFile.\"keel\" = {\n",
            "    recursive = true;\n",
            "  };\n",
            "  x\n",
            "}\n",
        )
    );
}

#[test]
fn the_tab_key_takes_the_width_of_the_language_or_of_the_settings() {
    // Nix indents with two columns, while the settings tab width is four.
    let (_directory, mut session) = opened("tab.nix", "a = 1;\n");
    press(&mut session, 'i');
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(session.buffer().to_string(), "  a = 1;\n");

    // Rust indents with four columns.
    let (_directory, mut session) = opened("tab.rs", "let x = 1;\n");
    press(&mut session, 'i');
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(session.buffer().to_string(), "    let x = 1;\n");

    // A buffer that no adapter serves keeps the settings width.
    let (_directory, mut session) = opened("tab.txt", "a = 1;\n");
    press(&mut session, 'i');
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(session.buffer().to_string(), "    a = 1;\n");

    // A language declares a column count, never a tab character, so a session
    // that keeps hard tabs inserts one tab in every language.
    let directory = TempDir::new("session-hard-tab");
    let path = directory.write("tab.nix", "a = 1;\n");
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    settings.indent.expand_tab = false;
    let mut hard = Session::new(
        Rect::new(0, 0, 80, 24),
        settings,
        test_root(directory.path.clone()),
    );
    hard.open_path(path);
    run_file_request(&mut hard);
    press(&mut hard, 'i');
    press_code(&mut hard, KeyCode::Tab);
    assert_eq!(hard.buffer().to_string(), "\ta = 1;\n");
}

#[test]
fn one_shift_step_takes_the_width_of_the_language_or_of_the_settings() {
    // The width of one level is also the step of `>` and `<`, as it is in Vim.
    let (_directory, mut session) = opened("shift.nix", "a = 1;\n");
    type_keys(&mut session, "V>");
    assert_eq!(session.buffer().to_string(), "  a = 1;\n");
    type_keys(&mut session, "<");
    assert_eq!(session.buffer().to_string(), "a = 1;\n");

    // A buffer that no adapter serves keeps the settings width.
    let (_directory, mut session) = opened("shift.txt", "a = 1;\n");
    type_keys(&mut session, "V>");
    assert_eq!(session.buffer().to_string(), "    a = 1;\n");
}

#[test]
fn every_window_paints_its_own_buffer_and_only_the_focused_one_holds_the_cursor() {
    let directory = TempDir::new("session-splits");
    let first = directory.write("first.rs", "fn first() {}\n");
    let second = directory.write("second.rs", "fn second() {}\n");
    let mut session = file_session(&directory.path);

    session.open_path(first);
    run_file_request(&mut session);
    let left = session.windows().focused_window();
    let left_buffer = session.active();

    // `Ctrl-Enter` splits with the adaptive rule and focuses the new window.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let right = session.windows().focused_window();
    assert_ne!(left, right);

    session.open_path(second);
    run_file_request(&mut session);
    let right_buffer = session.active();
    assert_ne!(left_buffer, right_buffer);
    assert_eq!(session.windows().buffer(left), Some(left_buffer));
    assert_eq!(session.windows().buffer(right), Some(right_buffer));

    // The focus moves back, and the editing state follows the focused window.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('h'))), NOW);
    assert_eq!(session.windows().focused_window(), left);
    assert_eq!(
        session.active(),
        left_buffer,
        "a key must change the buffer that the focused window shows"
    );
    press(&mut session, 'i');
    type_keys(&mut session, "// ");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "// fn first() {}\n");
}

#[test]
fn an_unsupported_target_is_rejected_and_leaves_the_editor_usable() {
    let directory = TempDir::new("session-reject");
    let mut session = file_session(&directory.path);

    // The target is a directory below the root, because the root itself names
    // no worktree-relative path and the transition rejects it before the read.
    session.open_path(directory.dir("nested"));
    run_file_request(&mut session);
    assert_eq!(
        session.message().map(|message| message.level()),
        Some(MessageLevel::Error)
    );
    assert_eq!(session.buffers().len(), 1, "no buffer holds a directory");

    press(&mut session, 'i');
    type_keys(&mut session, "text");
    assert_eq!(session.buffer().to_string(), "text\n");
}

#[test]
fn a_server_that_the_workspace_does_not_use_is_reported_once_and_editing_continues() {
    let directory = TempDir::new("session-unused-server");
    let path = directory.write("main.rs", "fn main() {}\n");
    let mut session = file_session(&directory.path);

    session.open_path(path);
    run_file_request(&mut session);
    // The load queues one open. This workspace uses no declared server of the
    // buffer, which is a normal state and not a failure.
    refuse_language_requests_with(&mut session, || LspError::UnusedInWorkspace);
    assert_eq!(
        message(&session),
        "this workspace uses no language server for this buffer; editing continues",
        "an unused server names its own state, not a missing installation"
    );

    // The state reaches the message line once, so a later question repeats it
    // never.
    type_keys(&mut session, " k");
    refuse_language_requests_with(&mut session, || LspError::UnusedInWorkspace);
    assert_eq!(message(&session), "");

    // The editor stays fully usable without the server.
    press(&mut session, 'i');
    type_keys(&mut session, "// ");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "// fn main() {}\n");
}

#[test]
fn a_missing_language_server_is_reported_once_and_editing_continues() {
    let directory = TempDir::new("session-missing-server");
    let path = directory.write("main.rs", "fn main() {}\n");
    let mut session = file_session(&directory.path);

    session.open_path(path);
    run_file_request(&mut session);
    // The load queues one open, which reaches no server on this system.
    refuse_language_requests(&mut session);
    assert_eq!(
        message(&session),
        "no language server serves this buffer",
        "a missing server is a normal state, not a failure"
    );

    // Every later question finds the state already reported. `Space e` reads
    // the published diagnostics instead, so it asks no server at all.
    for keys in [" k", "gd"] {
        type_keys(&mut session, keys);
        refuse_language_requests(&mut session);
        assert_eq!(
            message(&session),
            "",
            "`{keys}` must not repeat the report of a missing server"
        );
    }

    // The editor stays fully usable without a language server.
    press(&mut session, 'i');
    type_keys(&mut session, "// ");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "// fn main() {}\n");
}

#[test]
fn a_refusal_opens_no_document_again_without_a_lost_copy() {
    let directory = TempDir::new("session-refused-request");
    let path = directory.write("main.rs", "fn main() {}\n");
    let mut session = file_session(&directory.path);

    session.open_path(path);
    run_file_request(&mut session);
    refuse_language_requests(&mut session);

    // The edit queues one incremental change of the document.
    press(&mut session, 'i');
    type_keys(&mut session, "x");
    press_code(&mut session, KeyCode::Esc);
    let change = session
        .take_language_request()
        .expect("the edit queued one synchronization");
    assert_eq!(change.kind(), LanguageRequestKind::Synchronization);

    // No running session took the change, so no copy of that document exists
    // and no fresh open can repair one.
    let _ = session.apply_language_dispatch(&change, Err(LspError::NoServerDeclared));
    assert!(
        session.take_language_request().is_none(),
        "a refusal that names no running session opens no document again"
    );

    // A question carries no text, so its refusal leaves no copy behind. The
    // editor releases the question and opens no document again.
    type_keys(&mut session, "gd");
    let query = session
        .take_language_request()
        .expect("the keys asked one question");
    assert_eq!(query.kind(), LanguageRequestKind::Query);
    let _ = session.apply_language_dispatch(&query, Err(LspError::Saturated));
    assert!(
        session.take_language_request().is_none(),
        "a refused question opens no document again"
    );

    // The editor stays fully usable after both refusals.
    press(&mut session, 'i');
    type_keys(&mut session, "y");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "xyfn main() {}\n");
}

#[test]
fn the_format_on_save_toggle_changes_the_active_buffer_alone() {
    let directory = TempDir::new("session-format-toggle");
    let first = directory.write("first.rs", "one\n");
    let second = directory.write("second.rs", "two\n");
    let mut session = file_session(&directory.path);

    session.open_path(first.clone());
    run_file_request(&mut session);
    session.open_path(second);
    run_file_request(&mut session);

    assert_eq!(
        session.status().formatter,
        super::EditorFormatterStatus::AvailableEnabled
    );

    // Every new buffer follows the settings default, so its save formats first.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert!(
        asks_a_question(&mut session),
        "format-on-save asks the language server before the write"
    );
    run_file_request(&mut session);

    type_keys(&mut session, " cf");
    assert_eq!(message(&session), "format-on-save is off for this buffer");
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert!(
        !asks_a_question(&mut session),
        "the toggled buffer saves its content as it is"
    );
    run_file_request(&mut session);

    // The toggle is per buffer, so no other buffer and no default changed. The
    // first file is loaded already, so its path reaches its buffer without a
    // new read.
    session.open_path(first);
    assert!(session.take_file_request().is_none());
    assert_eq!(session.active_buffer().name(), "first.rs");
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert!(
        asks_a_question(&mut session),
        "the toggle of one buffer never changes another buffer"
    );
    run_file_request(&mut session);

    type_keys(&mut session, " cf");
    assert_eq!(message(&session), "format-on-save is off for this buffer");
    type_keys(&mut session, " cf");
    assert_eq!(message(&session), "format-on-save is on for this buffer");
}

#[test]
fn a_save_starts_no_format_that_no_formatter_can_answer() {
    let directory = TempDir::new("session-format-absent");
    let plain = directory.write("notes.txt", "plain\n");
    let code = directory.write("code.rs", "fn code() {}\n");
    let mut session = file_session(&directory.path);

    // The scratch buffer holds no file name, so no adapter serves it and its
    // save asks nothing.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert!(
        !asks_a_question(&mut session),
        "a buffer without a file name starts no format query"
    );

    session.open_path(plain);
    run_file_request(&mut session);

    // No adapter owns the plain-text path, so no formatter can answer a
    // question about it.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert!(
        !asks_a_question(&mut session),
        "a buffer that no formatter serves starts no format query"
    );
    run_file_request(&mut session);
    assert!(
        message(&session).ends_with("written"),
        "the save reports its own result alone, and got {}",
        message(&session)
    );

    // The save changed no remembered state, so the toggle still reports the
    // missing formatter instead of a state.
    type_keys(&mut session, " cf");
    assert_eq!(message(&session), "no formatter serves this buffer");

    // A buffer that one formatter serves keeps the question of its save.
    session.open_path(code);
    run_file_request(&mut session);
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    assert!(
        asks_a_question(&mut session),
        "a buffer that one formatter serves still formats before its save"
    );
    run_file_request(&mut session);
}

/// Returns the first visible line of one window.
fn first_line(session: &Session, window: WindowId) -> usize {
    session
        .windows()
        .state(window)
        .expect("the window exists")
        .first_line()
}

/// Returns the cursor line of one window.
fn cursor_line(session: &Session, window: WindowId) -> usize {
    session
        .windows()
        .state(window)
        .expect("the window exists")
        .cursor()
        .line()
        .get()
}

/// Creates a session with one long buffer and one vertical split.
///
/// The function returns the left window and the right window, and the right
/// window holds the focus, as a new split always does.
fn split_session(lines: usize) -> (Session, WindowId, WindowId) {
    let mut session = session(80, 24);
    press(&mut session, 'i');
    for index in 0..lines {
        if index > 0 {
            press_code(&mut session, KeyCode::Enter);
        }
        type_keys(&mut session, "line");
    }
    press_code(&mut session, KeyCode::Esc);
    type_keys(&mut session, "gg");

    let left = session.windows().focused_window();
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let right = session.windows().focused_window();
    assert_ne!(left, right, "the split opened a second window");
    (session, left, right)
}

#[test]
fn two_windows_on_one_buffer_scroll_independently() {
    let (mut session, left, right) = split_session(200);
    assert_eq!(first_line(&session, left), 0);
    assert_eq!(first_line(&session, right), 0);

    // The focused window scrolls to the buffer end.
    press(&mut session, 'G');
    let scrolled = first_line(&session, right);
    assert!(scrolled > 0, "the focused window followed its cursor");
    assert_eq!(
        first_line(&session, left),
        0,
        "the untouched window keeps its first visible line"
    );
    assert_eq!(
        cursor_line(&session, left),
        0,
        "the untouched window keeps its cursor"
    );

    // The focus returns to the left window, and both windows stay where they
    // were.
    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('h'))), NOW);
    assert_eq!(session.windows().focused_window(), left);
    assert_eq!(first_line(&session, left), 0);
    assert_eq!(first_line(&session, right), scrolled);
    assert_eq!(cursor_line(&session, right), 199);

    // A move in the left window moves no other window.
    type_keys(&mut session, "10j");
    assert_eq!(cursor_line(&session, left), 10);
    assert_eq!(cursor_line(&session, right), 199);
    assert_eq!(first_line(&session, right), scrolled);
}

#[test]
fn two_windows_on_two_buffers_scroll_independently() {
    let directory = TempDir::new("session-window-cursors");
    let first = directory.write("first.rs", &"first\n".repeat(200));
    let second = directory.write("second.rs", &"second\n".repeat(200));
    let mut session = file_session(&directory.path);

    session.open_path(first);
    run_file_request(&mut session);
    let left = session.windows().focused_window();

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let right = session.windows().focused_window();
    session.open_path(second);
    run_file_request(&mut session);
    assert_ne!(
        session.windows().buffer(left),
        session.windows().buffer(right),
        "the two windows show two buffers"
    );

    press(&mut session, 'G');
    let scrolled = first_line(&session, right);
    assert!(scrolled > 0);
    assert_eq!(
        first_line(&session, left),
        0,
        "the window of the other buffer did not scroll"
    );

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('h'))), NOW);
    assert_eq!(
        session.active_buffer().name(),
        "first.rs",
        "the focus move follows the buffer of its window"
    );
    assert_eq!(cursor_line(&session, left), 0);
    assert_eq!(first_line(&session, right), scrolled);
}

#[test]
fn a_new_split_copies_the_cursor_and_the_viewport_of_its_source() {
    let (mut session, _, right) = split_session(200);
    press(&mut session, 'G');
    let line = cursor_line(&session, right);
    assert!(first_line(&session, right) > 0);

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Enter)), NOW);
    let created = session.windows().focused_window();
    assert_ne!(created, right);
    assert_eq!(
        cursor_line(&session, created),
        line,
        "the new window opens at the cursor of its source"
    );
    // The split halves the height of the source window, so both windows
    // reconcile to the same smaller view.
    assert_eq!(
        first_line(&session, created),
        first_line(&session, right),
        "the new window opens at the view of its source"
    );
    assert!(
        first_line(&session, created) > 0,
        "the new window did not return to the buffer start"
    );
}

#[test]
fn closing_a_window_discards_its_cursor() {
    let (mut session, left, right) = split_session(200);
    press(&mut session, 'G');
    assert!(first_line(&session, right) > 0);

    type_keys(&mut session, " q");
    assert_eq!(session.windows().window_count(), 1);
    assert_eq!(session.windows().focused_window(), left);
    assert_eq!(
        first_line(&session, left),
        0,
        "the surviving window keeps its own view"
    );
    assert!(
        session.windows().state(right).is_none(),
        "the closed window discarded its view"
    );
}

#[test]
fn a_backward_jump_without_a_recorded_position_reports_the_end_of_the_list() {
    let mut session = with_text_and_no_jump(&["one", "two"]);
    assert_eq!(session.cursor().line().get(), 1);

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('o'))), NOW);
    assert_eq!(
        session.cursor().line().get(),
        1,
        "an empty list moves no cursor"
    );
    assert!(
        message(&session).contains("no older position"),
        "the empty list names the end that it reached: {}",
        message(&session)
    );
}

#[test]
fn a_recorded_line_past_the_end_of_the_buffer_clamps_to_the_last_line() {
    let mut session = with_text(&["one", "two", "three", "four", "five"]);
    type_keys(&mut session, "jjjj");
    session.record_jump();

    // The buffer shrinks under the recorded position. The editor adjusts no
    // recorded position while the user types, so the step clamps instead.
    type_keys(&mut session, "gg");
    type_keys(&mut session, "dddddd");
    let last = session.buffer().line_count() - 1;
    assert!(
        last < 4,
        "the buffer holds fewer lines than the record names"
    );

    session.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('o'))), NOW);
    assert_eq!(
        session.cursor().line().get(),
        last,
        "the recorded line clamps to the last line of the buffer"
    );
}

/// Builds one session over the given lines without any jump.
///
/// [`with_text`] ends on `gg`, which is a jump source, so a test that asserts
/// an empty jump list types the lines itself and leaves the cursor where the
/// last line ends.
fn with_text_and_no_jump(lines: &[&str]) -> Session {
    let mut session = session(60, 20);
    press(&mut session, 'i');
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            press_code(&mut session, KeyCode::Enter);
        }
        type_keys(&mut session, line);
    }
    press_code(&mut session, KeyCode::Esc);
    session
}

#[test]
fn the_first_line_and_last_line_motions_each_record_one_jump() {
    let mut session = with_text(&["one", "two", "three", "four", "five"]);
    assert_eq!(session.cursor().line().get(), 0);

    type_keys(&mut session, "G");
    assert_eq!(session.cursor().line().get(), 4);
    type_keys(&mut session, "gg");
    assert_eq!(session.cursor().line().get(), 0);

    press_ctrl(&mut session, 'o');
    assert_eq!(
        session.cursor().line().get(),
        4,
        "`Ctrl-O` returns to the line that `gg` left"
    );
    press_code(&mut session, KeyCode::Tab);
    assert_eq!(
        session.cursor().line().get(),
        0,
        "`Tab` returns to the target of `gg`"
    );
}

#[test]
fn the_matching_bracket_motion_records_one_jump() {
    let mut session = with_text(&["(", "alpha", ")"]);
    assert_eq!(session.cursor().line().get(), 0);

    type_keys(&mut session, "%");
    assert_eq!(
        session.cursor().line().get(),
        2,
        "`%` reaches the matching bracket"
    );

    press_ctrl(&mut session, 'o');
    assert_eq!(
        session.cursor().line().get(),
        0,
        "`Ctrl-O` returns to the opening bracket"
    );
}

#[test]
fn an_accepted_search_and_the_repeat_keys_each_record_one_jump() {
    let mut session = with_text(&["alpha", "beta", "alpha", "gamma", "alpha"]);
    type_keys(&mut session, "/alpha");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(
        session.cursor().line().get(),
        2,
        "the accepted query moves to the next match"
    );

    type_keys(&mut session, "n");
    assert_eq!(session.cursor().line().get(), 4);

    press_ctrl(&mut session, 'o');
    assert_eq!(
        session.cursor().line().get(),
        2,
        "`Ctrl-O` returns to the match that `n` left"
    );
    press_ctrl(&mut session, 'o');
    assert_eq!(
        session.cursor().line().get(),
        0,
        "the next step returns to the line under the search prompt"
    );

    // `N` records its starting line as well.
    type_keys(&mut session, "N");
    assert_eq!(
        session.cursor().line().get(),
        4,
        "`N` wraps to the last match"
    );
    press_ctrl(&mut session, 'o');
    assert_eq!(
        session.cursor().line().get(),
        0,
        "`Ctrl-O` returns to the line that `N` left"
    );
}

#[test]
fn the_line_number_command_records_one_jump() {
    let mut session = with_text(&["one", "two", "three", "four", "five"]);
    press(&mut session, ':');
    type_keys(&mut session, "4");
    press_code(&mut session, KeyCode::Enter);
    assert_eq!(session.cursor().line().get(), 3);

    press_ctrl(&mut session, 'o');
    assert_eq!(
        session.cursor().line().get(),
        0,
        "`Ctrl-O` returns to the line under the command line"
    );
}

#[test]
fn a_word_motion_and_a_half_page_motion_record_no_jump() {
    let lines: Vec<&str> = vec!["alpha beta"; 30];
    let mut session = with_text_and_no_jump(&lines);
    assert_eq!(session.cursor().line().get(), 29);

    // Vim treats neither motion as a jump, so neither records a position.
    type_keys(&mut session, "b");
    press_ctrl(&mut session, 'u');
    let reached = session.cursor().line().get();
    assert!(reached < 29, "the half-page motion moved the cursor");

    press_ctrl(&mut session, 'o');
    assert_eq!(
        session.cursor().line().get(),
        reached,
        "neither motion recorded a position, so nothing moves"
    );
    assert!(
        message(&session).contains("no older position"),
        "the empty list names the end that it reached: {}",
        message(&session)
    );
}

#[test]
fn a_jump_motion_that_completes_an_operator_records_no_jump() {
    let mut session = with_text_and_no_jump(&["one", "two", "three", "four"]);
    type_keys(&mut session, "kk");
    assert_eq!(session.cursor().line().get(), 1);

    // `dG` deletes to the end of the buffer. The motion serves the operator, so
    // it moves nothing on its own and records nothing, exactly as in Vim.
    type_keys(&mut session, "dG");
    assert_eq!(session.buffer().to_string(), "one\n");

    press_ctrl(&mut session, 'o');
    assert!(
        message(&session).contains("no older position"),
        "the operator target recorded nothing: {}",
        message(&session)
    );
}

#[test]
fn a_reported_deadline_always_reaches_a_transition_that_clears_it() {
    // The event loop runs one catch-up transition for a deadline that already
    // passed. A deadline that no transition can clear would keep the loop out of
    // its wait, and the editor would stop serving input. Every reported deadline
    // must therefore disappear after one tick.
    for keys in ["5", "12", " ", "5 ", "g", "5g", "z"] {
        let mut session = session(60, 20);
        type_keys(&mut session, keys);
        let Some(deadline) = session.next_deadline() else {
            continue;
        };
        session.tick(deadline);
        assert_eq!(
            session.next_deadline(),
            None,
            "the tick after the deadline of `{keys}` must clear it"
        );
    }
}

#[test]
fn a_pending_count_reports_no_deadline_at_all() {
    let mut session = session(60, 20);
    press(&mut session, '5');
    assert_eq!(
        session.next_deadline(),
        None,
        "a pending count shows no overlay, so the loop waits for the next key"
    );
    // The count still reaches the command that follows it.
    press(&mut session, 'j');
    assert_eq!(
        session.cursor().line().get(),
        0,
        "the buffer holds one line"
    );
    assert_eq!(session.mode(), Mode::Normal);
}

/// Creates a session whose clipboard reaches its value through one command.
///
/// The command never runs. Each test returns its output through
/// [`Session::apply_clipboard_result`], exactly as the event loop does.
fn clipboard_session(lines: &[&str]) -> Session {
    with_text(lines).with_session_clipboard(SessionClipboard::deferred())
}

/// Returns the standard input of the clipboard command that waits.
fn clipboard_text(session: &mut Session) -> String {
    let request = session
        .take_clipboard_request()
        .expect("the transition queued one clipboard command");
    String::from_utf8(request.stdin).expect("the editor writes UTF-8 text")
}

/// Returns the output of one clipboard command that succeeded.
fn clipboard_output(stdout: &str) -> ProcessOutput {
    ProcessOutput {
        status_code: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

#[test]
fn a_yank_sends_the_register_value_to_the_system_clipboard() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    assert_eq!(
        clipboard_text(&mut session),
        "alpha\n",
        "a linewise yank carries its line ending across the boundary"
    );
    assert_eq!(
        session.apply_clipboard_result(Ok(clipboard_output(""))),
        Redraw::Skipped,
        "a clipboard write that succeeded reports nothing"
    );
    assert_eq!(message(&session), "");
}

#[test]
fn a_failed_clipboard_write_keeps_the_register_value() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Err(ClipboardFailure::Failed));
    assert!(
        message(&session).contains("register still holds the value"),
        "the yank succeeded, so the report names the clipboard alone: {}",
        message(&session)
    );

    // The register survived the failure, so a paste still returns the value.
    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Err(ClipboardFailure::Failed));
    assert_eq!(session.buffer().to_string(), "alpha\nalpha\nbeta\n");
}

#[test]
fn a_clipboard_write_that_reported_no_outcome_reports_nothing() {
    // `wl-copy` and `xclip` own the selection through a background process that
    // inherits the captured output streams, so a write that succeeded holds
    // those streams open and reaches the process deadline. The write worked, so
    // the message line must stay empty. See `docs/clipboard.md`.
    for failure in [ClipboardFailure::Timeout, ClipboardFailure::Cancelled] {
        let mut session = clipboard_session(&["alpha", "beta"]);
        type_keys(&mut session, "yy");
        assert_eq!(clipboard_text(&mut session), "alpha\n");
        assert_eq!(
            session.apply_clipboard_result(Err(failure)),
            Redraw::Skipped,
            "{failure} proves no clipboard failure, so nothing changes"
        );
        assert_eq!(message(&session), "", "{failure} reports nothing");

        // The register kept the value on this path as well.
        type_keys(&mut session, "p");
        let _ = clipboard_text(&mut session);
        let _ = session.apply_clipboard_result(Err(failure));
        assert_eq!(session.buffer().to_string(), "alpha\nalpha\nbeta\n");
        assert_eq!(message(&session), "");
    }
}

#[test]
fn a_clipboard_write_that_a_signal_ended_reports_the_failure() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    // A signal leaves no exit status, so the command reported no success.
    let signalled = ProcessOutput {
        status_code: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    assert_eq!(
        session.apply_clipboard_result(Ok(signalled)),
        Redraw::Needed
    );
    assert!(
        message(&session).contains("register still holds the value"),
        "a proven failure still reaches the message line: {}",
        message(&session)
    );
}

#[test]
fn a_write_that_a_newer_write_displaced_reports_nothing() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    // The newer yank owns the clipboard, and the displaced write resolves from
    // internal state alone, so neither yank reports anything.
    type_keys(&mut session, "jyy");
    assert_eq!(message(&session), "");
    assert_eq!(clipboard_text(&mut session), "beta\n");
    assert_eq!(
        session.apply_clipboard_result(Ok(clipboard_output(""))),
        Redraw::Skipped
    );
    assert_eq!(message(&session), "");
}

#[test]
fn a_failed_clipboard_read_falls_back_to_the_internal_register() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Ok(clipboard_output("alpha\n")));

    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    // A refused submission is the same expected runtime state as a failed
    // command, so the paste still applies the internal register.
    let _ = session.apply_clipboard_result(Err(ClipboardFailure::Refused));
    assert_eq!(session.buffer().to_string(), "alpha\nalpha\nbeta\n");
}

#[test]
fn a_clipboard_paste_refreshes_search_positions_before_the_next_frame() {
    let mut session = clipboard_session(&[
        "first target",
        "second target",
        "third target",
        "fourth target",
        "fifth target",
    ]);
    type_keys(&mut session, "/target");
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "ggVGp");
    let _ = clipboard_text(&mut session);

    let _ = session.apply_clipboard_result(Ok(clipboard_output("one\ntwo\nthree\nfour\n")));

    let visible = session.visible();
    let search = visible.search.expect("the accepted search stays active");
    assert!(
        search
            .matches
            .iter()
            .all(|position| position.get() <= session.buffer().len_chars()),
        "the frame must not convert search positions from the longer buffer"
    );
}

#[test]
fn a_kvim_yank_pastes_with_the_shape_that_it_recorded() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    assert_eq!(clipboard_text(&mut session), "alpha\n");
    let _ = session.apply_clipboard_result(Ok(clipboard_output("")));

    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    // The clipboard still holds the text that kvim wrote, so the recorded
    // linewise shape applies. See `docs/clipboard.md`.
    let _ = session.apply_clipboard_result(Ok(clipboard_output("alpha\n")));
    assert_eq!(session.buffer().to_string(), "alpha\nalpha\nbeta\n");
}

#[test]
fn an_external_copy_pastes_characterwise() {
    let mut session = clipboard_session(&["alpha"]);
    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Ok(clipboard_output("gamma")));
    assert_eq!(
        session.buffer().to_string(),
        "agammalpha\n",
        "text that kvim never wrote is characterwise"
    );
}

#[test]
fn an_external_copy_that_ends_with_a_line_ending_pastes_linewise() {
    let mut session = clipboard_session(&["alpha"]);
    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Ok(clipboard_output("gamma\n")));
    assert_eq!(session.buffer().to_string(), "alpha\ngamma\n");
}

#[test]
fn an_oversized_clipboard_value_never_reaches_the_register() {
    let mut session = clipboard_session(&["alpha"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Ok(clipboard_output("")));

    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    let oversized = "b".repeat(CLIPBOARD_BYTES_MAX + 1);
    let _ = session.apply_clipboard_result(Ok(clipboard_output(&oversized)));
    assert!(
        message(&session).contains("clipboard bound"),
        "the report names the bound: {}",
        message(&session)
    );
    assert_eq!(
        session.buffer().to_string(),
        "alpha\nalpha\n",
        "the paste falls back to the internal register"
    );
}

#[test]
fn a_missing_clipboard_command_is_reported_once_for_each_session() {
    // A session without an injected clipboard reaches no command at all, which
    // is the supported state of a host without a clipboard tool.
    let mut session = with_text(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    assert!(
        message(&session).contains("no system clipboard command"),
        "the first operation names the missing command: {}",
        message(&session)
    );
    assert!(
        session.take_clipboard_request().is_none(),
        "a host without a command runs none"
    );

    type_keys(&mut session, "yy");
    assert_eq!(
        message(&session),
        "",
        "the missing command is reported once for each session"
    );
}

#[test]
fn a_clipboard_output_without_a_pending_operation_changes_nothing() {
    let mut session = clipboard_session(&["alpha"]);
    assert_eq!(
        session.apply_clipboard_result(Ok(clipboard_output("gamma"))),
        Redraw::Skipped,
        "an output that no operation waits for is obsolete"
    );
    assert_eq!(session.buffer().to_string(), "alpha\n");
    assert_eq!(message(&session), "");
}

#[test]
fn a_newer_clipboard_operation_never_leaves_a_paste_waiting() {
    let mut session = clipboard_session(&["alpha", "beta"]);
    type_keys(&mut session, "yy");
    let _ = clipboard_text(&mut session);
    let _ = session.apply_clipboard_result(Ok(clipboard_output("")));

    type_keys(&mut session, "p");
    let _ = clipboard_text(&mut session);
    // A yank displaces the read that the paste waits for. The paste must then
    // apply the internal register instead of waiting forever.
    type_keys(&mut session, "yy");
    assert_eq!(session.buffer().to_string(), "alpha\nalpha\nbeta\n");
    assert_eq!(
        clipboard_text(&mut session),
        "alpha\n",
        "the displacing yank owns the clipboard command"
    );
}

#[test]
fn a_named_register_carries_a_yank_to_a_paste() {
    let mut session = session(40, 10);
    press(&mut session, 'i');
    type_keys(&mut session, "alpha");
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "beta");
    press_code(&mut session, KeyCode::Esc);
    assert_eq!(session.buffer().to_string(), "alpha\nbeta\n");

    // `"ayy` on the first line, then `"ap` on the second one.
    type_keys(&mut session, "gg\"ayy");
    type_keys(&mut session, "j\"ap");
    assert_eq!(session.buffer().to_string(), "alpha\nbeta\nalpha\n");
}

#[test]
fn the_black_hole_register_keeps_the_yanked_value() {
    let mut session = session(40, 10);
    press(&mut session, 'i');
    type_keys(&mut session, "alpha");
    press_code(&mut session, KeyCode::Enter);
    type_keys(&mut session, "beta");
    press_code(&mut session, KeyCode::Esc);

    // `yy` on the first line, then `"_dd` on the same line.
    type_keys(&mut session, "ggyy");
    type_keys(&mut session, "\"_dd");
    assert_eq!(session.buffer().to_string(), "beta\n");

    // The yanked line survived the delete, so `p` pastes it again.
    type_keys(&mut session, "p");
    assert_eq!(session.buffer().to_string(), "beta\nalpha\n");
}

#[test]
fn the_review_owns_the_keys_and_gives_the_layout_back_unchanged() {
    let mut session = session(80, 24);
    press(&mut session, 'i');
    type_keys(&mut session, "alpha");
    press_code(&mut session, KeyCode::Esc);

    let before = session.buffer().to_string();
    let scope = session.input_context().scope;

    // `<leader>gg` opens the review, which then owns every key.
    type_keys(&mut session, " gg");
    assert_eq!(session.input_context().scope, BindingScope::Review);

    // A buffer key reaches no buffer while the review stays open.
    type_keys(&mut session, "dd");
    assert_eq!(session.buffer().to_string(), before);

    // `q` leaves the review and the editor is exactly as it was.
    press(&mut session, 'q');
    assert_eq!(session.input_context().scope, scope);
    assert_eq!(session.buffer().to_string(), before);

    // The buffer answers keys again.
    type_keys(&mut session, "dd");
    assert_ne!(session.buffer().to_string(), before);
}

#[test]
fn integrated_review_uses_the_expanded_host_owned_body_height() {
    let mut session = integrated_session(80, 10);
    session.review = Some(ReviewSurface::new(
        None,
        None,
        session.settings.diff,
        session.settings.windows.resize_step_cells,
        0,
    ));
    let _ = session.open_review();

    let review = session
        .visible()
        .review
        .expect("the integrated review is open");
    assert_eq!(review.height_rows(), 8);

    session
        .set_area(Rect::new(0, 0, 80, 12))
        .expect("the larger rectangle is valid");
    assert_eq!(
        session
            .visible()
            .review
            .expect("resize keeps the integrated review open")
            .height_rows(),
        10,
    );
}

#[test]
fn the_review_asks_for_both_halves_of_the_worktree() {
    let mut session = session(80, 24);
    type_keys(&mut session, " gg");

    // The session runs no `git` itself, so both captures leave it as requests.
    let first = session
        .take_diff_request()
        .expect("the review asks for the staged half");
    let second = session
        .take_diff_request()
        .expect("the review asks for the unstaged half");
    assert_ne!(first.0, second.0, "the two requests name two sections");
    assert!(
        session.take_diff_request().is_none(),
        "the review asks for two halves and no more"
    );
}

#[test]
fn a_jump_from_the_review_moves_the_keys_to_the_window() {
    // A reader who opened the review from the file tree left the keys there.
    // The jump must reach the file, not leave the keys on the sidebar.
    let mut session = session(80, 24);
    press_ctrl(&mut session, 'e');
    assert_eq!(session.input_context().scope, BindingScope::Sidebar);

    type_keys(&mut session, " gg");
    assert_eq!(session.input_context().scope, BindingScope::Review);

    // The review holds no capture yet, so the jump reaches no file and the
    // review still gives the keys back to a buffer rather than the sidebar.
    press(&mut session, 'q');
    assert_eq!(session.input_context().scope, BindingScope::Sidebar);
}
