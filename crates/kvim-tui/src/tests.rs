//! Tests for the kvim adapter over the generic window tree.
//!
//! `kvim-ui` owns the topology, the focus, the resize rules, and the geometry,
//! and its own tests cover them. These tests cover the parts that this crate
//! adds: the split and adaptive settings, the semantic window commands, the
//! buffer of each window, and the view of each window.

use ratatui::layout::Rect;

use kvim_core::TextBuffer;
use kvim_editor::EditingState;
use kvim_input::Command;
use kvim_settings::{DisplaySettings, HorizontalSplitPlacement, WindowSettings};
use kvim_workspace::BufferId;

use super::buffer_view::WINBAR_ROWS;
use super::{AdaptiveSplit, CloseOutcome, Orientation, WindowId, WindowOutcome, Windows};

const BUFFER: BufferId = BufferId::new(1);

fn windows(width: u16, height: u16) -> Windows {
    Windows::new(
        BUFFER,
        Rect::new(0, 0, width, height),
        WindowSettings::default(),
    )
}

fn area(windows: &Windows, id: WindowId) -> Rect {
    windows
        .layout()
        .area(id)
        .expect("the test expects a visible region")
}

#[test]
fn the_placement_setting_moves_the_new_horizontal_window_above() {
    let settings = WindowSettings {
        horizontal_split_placement: HorizontalSplitPlacement::Above,
        ..WindowSettings::default()
    };
    let mut tree = Windows::new(BUFFER, Rect::new(0, 0, 120, 40), settings);
    let source = tree.focused_window();
    let created = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");

    assert!(area(&tree, created).y < area(&tree, source).y);
}

#[test]
fn one_window_always_splits_vertically() {
    // A full-width terminal would otherwise divide into two short windows, so
    // the single-window exception comes before the ratio.
    let tall = windows(40, 120);
    assert_eq!(
        tall.adaptive_orientation(AdaptiveSplit::Normal),
        Orientation::Vertical
    );
    assert_eq!(
        tall.adaptive_orientation(AdaptiveSplit::Inverse),
        Orientation::Horizontal
    );

    let wide = windows(200, 40);
    assert_eq!(
        wide.adaptive_orientation(AdaptiveSplit::Normal),
        Orientation::Vertical
    );
}

#[test]
fn the_adaptive_rule_follows_the_ratio_beyond_one_window() {
    // Two stacked windows of 200 by 20 leave a width above 20 times 2.5.
    let mut wide = windows(200, 40);
    wide.split(Orientation::Horizontal)
        .expect("the terminal is tall");
    assert_eq!(
        wide.adaptive_orientation(AdaptiveSplit::Normal),
        Orientation::Vertical
    );
    assert_eq!(
        wide.adaptive_orientation(AdaptiveSplit::Inverse),
        Orientation::Horizontal
    );

    // Two windows of 45 by 40 leave a width below 40 times 2.5.
    let mut narrow = windows(90, 40);
    narrow
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    assert_eq!(
        narrow.adaptive_orientation(AdaptiveSplit::Normal),
        Orientation::Horizontal
    );
    assert_eq!(
        narrow.adaptive_orientation(AdaptiveSplit::Inverse),
        Orientation::Vertical
    );
}

#[test]
fn the_inverse_adaptive_command_mirrors_the_orientation() {
    let mut tree = windows(120, 40);
    let source = tree.focused_window();
    let created = tree
        .split_adaptive(AdaptiveSplit::Inverse)
        .expect("the terminal is tall");

    assert!(area(&tree, created).y > area(&tree, source).y);
    assert_eq!(area(&tree, created).width, 120);
}

#[test]
fn the_window_tree_answers_only_the_window_commands() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();

    assert_eq!(tree.apply(Command::MoveDown), WindowOutcome::Ignored);
    assert_eq!(tree.apply(Command::SplitAdaptive), WindowOutcome::Changed);
    assert_eq!(tree.window_count(), 2);
    assert_eq!(tree.apply(Command::FocusWindowLeft), WindowOutcome::Changed);
    assert_eq!(tree.focused_window(), left);
    assert_eq!(
        tree.apply(Command::FocusWindowLeft),
        WindowOutcome::Unchanged
    );
    assert_eq!(tree.apply(Command::CloseWindow), WindowOutcome::Changed);
    assert_eq!(tree.apply(Command::CloseWindow), WindowOutcome::LastWindow);
}

#[test]
fn a_refused_split_reports_one_unchanged_command() {
    // The terminal holds 30 cells, and two windows need 40, so the window tree
    // refuses the split and the command changes nothing.
    let mut tree = windows(30, 40);
    assert_eq!(tree.apply(Command::SplitAdaptive), WindowOutcome::Unchanged);
    assert_eq!(tree.window_count(), 1);
}

#[test]
fn a_split_copies_the_buffer_and_a_close_discards_the_view() {
    let mut tree = windows(120, 40);
    let source = tree.focused_window();
    let created = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");

    assert_eq!(tree.buffer(created), Some(BUFFER));
    assert!(tree.state(created).is_some());

    // A window that shows another buffer leaves its sibling unchanged.
    let other = BufferId::new(2);
    assert!(tree.set_buffer(created, other));
    assert_eq!(tree.buffer(created), Some(other));
    assert_eq!(tree.buffer(source), Some(BUFFER));

    assert_eq!(tree.close_focused(), CloseOutcome::Closed(created));
    assert_eq!(
        tree.state(created),
        None,
        "a closed window discards its view"
    );
    assert!(!tree.set_buffer(created, other));
}

#[test]
fn the_viewport_of_a_window_follows_the_text_rows_of_its_rectangle() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");

    // The winbar row belongs to the rectangle and shows no buffer line, so the
    // viewport reports one row less than the rectangle holds.
    let viewport = tree.viewport(right).expect("the window exists");
    assert_eq!(viewport.width_cells().get(), area(&tree, right).width);
    assert_eq!(viewport.height_rows().get(), 40 - WINBAR_ROWS);

    tree.set_terminal(Rect::new(0, 0, 80, 20));
    let viewport = tree.viewport(left).expect("the window exists");
    assert_eq!(viewport.width_cells().get(), area(&tree, left).width);
    assert_eq!(viewport.height_rows().get(), 20 - WINBAR_ROWS);
}

#[test]
fn a_split_and_a_terminal_resize_keep_the_scroll_offset() {
    let mut tree = windows(120, 40);
    let scrolled = tree.focused_window();

    // Scroll the window down and right, so both offsets leave the buffer start.
    let line = "x".repeat(400);
    let text = format!("{line}\n").repeat(200);
    let buffer = TextBuffer::from_text(&text, kvim_core::BufferBytesMax::default())
        .expect("the text is small");
    let mut state = tree.state(scrolled).expect("the window exists");
    EditingState::new().move_to(&buffer, &mut state, 120, 300);
    *tree.state_mut(scrolled).expect("the window exists") =
        state.reconciled(&buffer, &DisplaySettings::default());

    let scroll = tree.viewport(scrolled).expect("the window exists");
    assert!(scroll.first_line() > 0, "the test needs a scrolled window");
    assert!(scroll.left_column() > 0, "the test needs a scrolled window");

    // A split changes the height of the source window.
    tree.split(Orientation::Horizontal)
        .expect("the terminal is tall");
    let viewport = tree.viewport(scrolled).expect("the window exists");
    assert_eq!(viewport.first_line(), scroll.first_line());
    assert_eq!(viewport.left_column(), scroll.left_column());
    assert_eq!(
        viewport.height_rows().get(),
        area(&tree, scrolled).height - WINBAR_ROWS
    );

    // A terminal resize changes both dimensions of every window.
    tree.set_terminal(Rect::new(0, 0, 60, 24));
    let viewport = tree.viewport(scrolled).expect("the window exists");
    assert_eq!(viewport.first_line(), scroll.first_line());
    assert_eq!(viewport.left_column(), scroll.left_column());
    assert_eq!(
        viewport.height_rows().get(),
        area(&tree, scrolled).height - WINBAR_ROWS
    );
    assert_eq!(viewport.width_cells().get(), area(&tree, scrolled).width);
}

#[test]
fn a_new_window_opens_at_the_place_of_its_source_window() {
    let mut tree = windows(120, 40);
    let source = tree.focused_window();
    let line = "y".repeat(400);
    let text = format!("{line}\n").repeat(200);
    let buffer = TextBuffer::from_text(&text, kvim_core::BufferBytesMax::default())
        .expect("the text is small");
    let mut state = tree.state(source).expect("the window exists");
    EditingState::new().move_to(&buffer, &mut state, 90, 200);
    *tree.state_mut(source).expect("the window exists") =
        state.reconciled(&buffer, &DisplaySettings::default());
    let scroll = tree.viewport(source).expect("the window exists");

    let created = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    let viewport = tree.viewport(created).expect("the window exists");

    assert_eq!(viewport.first_line(), scroll.first_line());
    assert_eq!(viewport.left_column(), scroll.left_column());
    assert_eq!(
        tree.state(created).map(|state| state.cursor()),
        tree.state(source).map(|state| state.cursor()),
        "the new window copies the cursor of its source window",
    );
}

#[test]
fn directional_focus_and_resize_reach_the_generic_tree() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    tree.focus_region(left);

    let before = area(&tree, left).width;
    assert_eq!(
        tree.apply(Command::ResizeWindowRight),
        WindowOutcome::Changed
    );
    assert_eq!(
        area(&tree, left).width,
        before + tree.settings().resize_step_cells,
    );
    assert_eq!(
        tree.apply(Command::FocusWindowRight),
        WindowOutcome::Changed
    );
    assert_eq!(tree.focused_window(), right);
}
