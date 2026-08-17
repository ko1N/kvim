//! Tests for the window tree, the layout calculation, focus, resize, and sidebars.

use ratatui::layout::Rect;

use kvim_core::TextBuffer;
use kvim_editor::EditingState;
use kvim_input::Command;
use kvim_settings::{DisplaySettings, FileSettings, HorizontalSplitPlacement, WindowSettings};

use kvim_workspace::BufferId;

use super::{
    AdaptiveSplit, CloseOutcome, Direction, LayoutChange, Orientation, RegionKind, SPLIT_DEPTH_MAX,
    SidebarSide, SplitError, WindowId, WindowLayout, WindowOutcome, Windows,
};

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

/// Confirms that the regions cover the terminal without a gap and without an
/// overlap, and that every region belongs to the tree or to a sidebar.
fn assert_tiles(windows: &Windows) {
    let layout = windows.layout();
    let terminal = windows.terminal();
    let covered: u32 = layout
        .regions()
        .iter()
        .map(|region| region.area.area())
        .sum();
    assert_eq!(covered, terminal.area(), "the regions cover the terminal");
    for (index, first) in layout.regions().iter().enumerate() {
        assert!(
            terminal.union(first.area) == terminal,
            "a region stays inside the terminal"
        );
        for second in &layout.regions()[index + 1..] {
            assert!(
                first.area.intersection(second.area).is_empty(),
                "two regions never overlap: {first:?} and {second:?}",
            );
        }
    }
    let mut ids = windows.window_ids();
    ids.sort_unstable();
    let unique = {
        let mut unique = ids.clone();
        unique.dedup();
        unique
    };
    assert_eq!(ids, unique, "every window identity is unique");
    assert!(
        layout.area(windows.focused_window()).is_some(),
        "the layout keeps the focused window visible",
    );
}

fn regions(windows: &Windows) -> WindowLayout {
    windows.layout().clone()
}

#[test]
fn a_vertical_split_opens_the_new_window_to_the_right() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");

    assert_eq!(tree.focused_window(), right);
    assert_eq!(tree.buffer(right), Some(BUFFER));
    assert!(area(&tree, left).x < area(&tree, right).x);
    assert_eq!(area(&tree, left).height, 40);
    assert_tiles(&tree);
}

#[test]
fn a_horizontal_split_opens_the_new_window_below() {
    let mut tree = windows(120, 40);
    let top = tree.focused_window();
    let bottom = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");

    assert!(area(&tree, top).y < area(&tree, bottom).y);
    assert_eq!(area(&tree, top).width, 120);
    assert_tiles(&tree);
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
fn the_layout_is_deterministic() {
    let mut first = windows(120, 40);
    let mut second = windows(120, 40);
    for tree in [&mut first, &mut second] {
        tree.split(Orientation::Vertical)
            .expect("the terminal is wide");
        tree.split(Orientation::Horizontal)
            .expect("the terminal is tall");
        tree.focus_direction(Direction::Left);
        tree.resize(Direction::Right);
    }

    assert_eq!(regions(&first), regions(&second));

    // An equal terminal size recomputes equal rectangles.
    let before = regions(&first);
    first.set_terminal(Rect::new(0, 0, 120, 40));
    assert_eq!(regions(&first), before);
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
fn directional_focus_uses_the_rectangles() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    let bottom_right = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");

    assert_eq!(tree.focus_direction(Direction::Up), LayoutChange::Changed);
    assert_eq!(tree.focused_window(), right);
    assert_eq!(tree.focus_direction(Direction::Left), LayoutChange::Changed);
    assert_eq!(tree.focused_window(), left);
    assert_eq!(
        tree.focus_direction(Direction::Left),
        LayoutChange::Unchanged
    );
    assert_eq!(tree.focus_direction(Direction::Up), LayoutChange::Unchanged);
    assert_eq!(
        tree.focus_direction(Direction::Right),
        LayoutChange::Changed
    );
    assert_eq!(
        tree.focused_window(),
        right,
        "an equal overlap selects the first window in layout order",
    );
    assert_ne!(tree.focused_window(), bottom_right);
}

#[test]
fn the_largest_overlap_wins_the_neighbor() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let top_right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    let bottom_right = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");

    // Grow the lower right window, so it overlaps more of the left window.
    assert_eq!(tree.resize(Direction::Up), LayoutChange::Changed);
    assert!(area(&tree, bottom_right).height > area(&tree, top_right).height);

    tree.focus_region(left);
    assert_eq!(
        tree.layout().neighbor(left, Direction::Right),
        Some(bottom_right),
    );
    assert_eq!(
        tree.focus_direction(Direction::Right),
        LayoutChange::Changed
    );
    assert_eq!(tree.focused_window(), bottom_right);
}

#[test]
fn a_resize_right_moves_the_far_edge_right_and_grows_the_window() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    tree.focus_region(left);

    let before = area(&tree, left).width;
    assert_eq!(tree.resize(Direction::Right), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, before + 6);
    assert_eq!(area(&tree, right).width, 120 - before - 6);
    assert_tiles(&tree);
}

#[test]
fn the_far_edge_wins_while_both_edges_hold_a_neighbor() {
    // Three windows in one row. The focused window sits in the middle, so both
    // the left edge and the right edge hold a neighbor.
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let middle = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    let right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    tree.focus_region(middle);
    assert_eq!(area(&tree, left).width, 60);
    assert_eq!(area(&tree, middle).width, 30);
    assert_eq!(area(&tree, right).width, 30);

    // The command moves the right edge left, so the focused window shrinks.
    assert_eq!(tree.resize(Direction::Left), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 60, "the left edge stays in place");
    assert_eq!(area(&tree, middle).width, 24);
    assert_eq!(area(&tree, right).width, 36);

    // The command moves the right edge right, so the focused window grows.
    assert_eq!(tree.resize(Direction::Right), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 60, "the left edge stays in place");
    assert_eq!(area(&tree, middle).width, 30);
    assert_eq!(area(&tree, right).width, 30);
    assert_tiles(&tree);
}

#[test]
fn the_bottom_edge_wins_while_both_edges_hold_a_neighbor() {
    // Three stacked windows. The focused window sits in the middle.
    let mut tree = windows(120, 60);
    let top = tree.focused_window();
    let middle = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");
    let bottom = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");
    tree.focus_region(middle);
    assert_eq!(area(&tree, top).height, 30);
    assert_eq!(area(&tree, middle).height, 15);
    assert_eq!(area(&tree, bottom).height, 15);

    // The command moves the bottom edge up, so the focused window shrinks.
    assert_eq!(tree.resize(Direction::Up), LayoutChange::Changed);
    assert_eq!(area(&tree, top).height, 30, "the top edge stays in place");
    assert_eq!(area(&tree, middle).height, 9);
    assert_eq!(area(&tree, bottom).height, 21);

    // The command moves the bottom edge down, so the focused window grows.
    assert_eq!(tree.resize(Direction::Down), LayoutChange::Changed);
    assert_eq!(area(&tree, top).height, 30, "the top edge stays in place");
    assert_eq!(area(&tree, middle).height, 15);
    assert_eq!(area(&tree, bottom).height, 15);
    assert_tiles(&tree);
}

#[test]
fn a_resize_without_a_far_neighbor_moves_the_near_edge() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");

    // The focused window sits on the right, so the right edge holds no
    // neighbor. The command moves the left edge right, which shrinks it.
    let before = area(&tree, right).width;
    assert_eq!(tree.resize(Direction::Right), LayoutChange::Changed);
    assert_eq!(area(&tree, right).width, before - 6);
    assert_eq!(area(&tree, left).width, 120 - before + 6);
    assert_tiles(&tree);
}

#[test]
fn a_resize_without_a_neighbor_leaves_the_layout_unchanged() {
    let mut tree = windows(120, 40);
    let before = regions(&tree);
    for direction in [
        Direction::Left,
        Direction::Down,
        Direction::Up,
        Direction::Right,
    ] {
        assert_eq!(tree.resize(direction), LayoutChange::Unchanged);
    }
    assert_eq!(regions(&tree), before);
}

#[test]
fn a_resize_below_the_minimum_leaves_the_layout_unchanged() {
    // Two windows of 20 cells already sit at the minimum window width.
    let mut tree = windows(40, 40);
    tree.split(Orientation::Vertical)
        .expect("the terminal is wide");
    let before = regions(&tree);

    assert_eq!(tree.resize(Direction::Right), LayoutChange::Unchanged);
    assert_eq!(tree.resize(Direction::Left), LayoutChange::Unchanged);
    assert_eq!(regions(&tree), before);
}

/// Creates three windows in one row and returns them from left to right.
///
/// The row holds 60, 30, and 30 cells, so every window stays above the minimum
/// window width of 20 cells.
fn row_of_three(tree: &mut Windows) -> (WindowId, WindowId, WindowId) {
    let left = tree.focused_window();
    let middle = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    let right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    assert_eq!(area(tree, left).width, 60);
    assert_eq!(area(tree, middle).width, 30);
    assert_eq!(area(tree, right).width, 30);
    (left, middle, right)
}

#[test]
fn a_row_resize_moves_one_border_and_keeps_every_other_pane() {
    let mut tree = windows(120, 40);
    let (left, middle, right) = row_of_three(&mut tree);

    // The focused window sits on the left, so the border it shares with the
    // middle window moves. The right window shares no border with it and keeps
    // its exact width, although the tree holds it one split level deeper.
    tree.focus_region(left);
    assert_eq!(tree.resize(Direction::Right), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 66);
    assert_eq!(area(&tree, middle).width, 24);
    assert_eq!(
        area(&tree, right).width,
        30,
        "a pane that shares no border with the moved one keeps its cells"
    );
    assert_tiles(&tree);

    // The same rule holds from the middle window: the far edge wins, so the
    // right window gives the cells and the left window keeps its width.
    tree.focus_region(middle);
    assert_eq!(tree.resize(Direction::Right), LayoutChange::Changed);
    assert_eq!(
        area(&tree, left).width,
        66,
        "the left border stays in place"
    );
    assert_eq!(area(&tree, middle).width, 30);
    assert_eq!(area(&tree, right).width, 24);
    assert_tiles(&tree);
}

#[test]
fn a_stacked_resize_moves_one_border_and_keeps_every_other_pane() {
    // Three stacked windows of 30, 15, and 15 rows.
    let mut tree = windows(120, 60);
    let top = tree.focused_window();
    let middle = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");
    let bottom = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");
    tree.focus_region(top);

    assert_eq!(tree.resize(Direction::Down), LayoutChange::Changed);
    assert_eq!(area(&tree, top).height, 36);
    assert_eq!(area(&tree, middle).height, 9);
    assert_eq!(
        area(&tree, bottom).height,
        15,
        "the bottom window shares no border with the moved one"
    );
    assert_tiles(&tree);
}

#[test]
fn a_resize_cascades_past_a_pane_that_reaches_its_minimum() {
    let mut tree = windows(120, 40);
    let (left, middle, right) = row_of_three(&mut tree);
    tree.focus_region(left);

    // The first step takes six cells from the middle window.
    assert_eq!(tree.resize(Direction::Right), LayoutChange::Changed);
    assert_eq!(area(&tree, middle).width, 24);
    assert_eq!(area(&tree, right).width, 30);

    // The middle window reaches the minimum width of 20 cells after four more
    // cells, so the right window gives the remaining two.
    assert_eq!(tree.resize(Direction::Right), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 72);
    assert_eq!(area(&tree, middle).width, 20);
    assert_eq!(area(&tree, right).width, 28);

    // The middle window can give nothing more, so the right window gives every
    // cell of the next step.
    assert_eq!(tree.resize(Direction::Right), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 78);
    assert_eq!(area(&tree, middle).width, 20);
    assert_eq!(area(&tree, right).width, 22);
    assert_tiles(&tree);
}

#[test]
fn a_resize_without_room_for_every_minimum_leaves_the_layout_unchanged() {
    let mut tree = windows(120, 40);
    let (left, middle, right) = row_of_three(&mut tree);
    tree.focus_region(left);
    for _ in 0..3 {
        assert_eq!(tree.resize(Direction::Right), LayoutChange::Changed);
    }
    let before = regions(&tree);

    // The middle window and the right window hold 42 cells and need 40, so no
    // arrangement of the next six cells keeps both above the minimum.
    assert_eq!(tree.resize(Direction::Right), LayoutChange::Unchanged);
    assert_eq!(regions(&tree), before);
    assert_eq!(area(&tree, left).width, 78);
    assert_eq!(area(&tree, middle).width, 20);
    assert_eq!(area(&tree, right).width, 22);
}

#[test]
fn a_resize_moves_a_border_that_sits_above_the_focused_window() {
    // One window on the left, and two stacked windows on the right. The border
    // that the command moves belongs to the root, while the focused window sits
    // two split levels below it.
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let top_right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    let bottom_right = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");
    tree.focus_region(top_right);
    assert_eq!(area(&tree, left).width, 60);
    assert_eq!(area(&tree, top_right).height, 20);

    // The focused window holds no neighbor on its right, so the command moves
    // the left border. Both windows of the right column follow that border, and
    // their heights stay as they are.
    assert_eq!(tree.resize(Direction::Left), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 54);
    assert_eq!(area(&tree, top_right).width, 66);
    assert_eq!(area(&tree, bottom_right).width, 66);
    assert_eq!(area(&tree, top_right).height, 20, "the rows stay in place");
    assert_eq!(area(&tree, bottom_right).height, 20);
    assert_tiles(&tree);
}

#[test]
fn a_vertical_resize_moves_the_shared_row() {
    let mut tree = windows(120, 40);
    let top = tree.focused_window();
    let bottom = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");

    // The focused window sits at the bottom, so the bottom edge holds no
    // neighbor. The command moves the top edge up, which grows it.
    let before = area(&tree, bottom).height;
    assert_eq!(tree.resize(Direction::Up), LayoutChange::Changed);
    assert_eq!(area(&tree, bottom).height, before + 6);
    assert_eq!(area(&tree, top).height, 40 - before - 6);
    assert_tiles(&tree);
}

#[test]
fn closing_a_window_keeps_the_sibling_and_its_identity() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");

    assert_eq!(tree.close_focused(), CloseOutcome::Closed(right));
    assert_eq!(tree.window_ids(), vec![left]);
    assert_eq!(tree.focused_window(), left);
    assert_eq!(area(&tree, left), Rect::new(0, 0, 120, 40));
    assert_tiles(&tree);
}

#[test]
fn closing_the_last_window_keeps_the_tree() {
    let mut tree = windows(120, 40);
    let only = tree.focused_window();

    assert_eq!(tree.close_focused(), CloseOutcome::LastWindow);
    assert_eq!(tree.window_ids(), vec![only]);
    assert_eq!(tree.window_count(), 1);
}

#[test]
fn a_close_moves_the_focus_into_the_remaining_sibling() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let top_right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");
    let bottom_right = tree
        .split(Orientation::Horizontal)
        .expect("the terminal is tall");

    tree.focus_region(left);
    assert_eq!(tree.close_focused(), CloseOutcome::Closed(left));
    assert_eq!(tree.window_ids(), vec![top_right, bottom_right]);
    assert_eq!(tree.focused_window(), top_right);
    assert_tiles(&tree);
}

#[test]
fn a_terminal_resize_keeps_the_tree_and_every_identity() {
    let mut tree = windows(120, 40);
    tree.split(Orientation::Vertical)
        .expect("the terminal is wide");
    tree.split(Orientation::Horizontal)
        .expect("the terminal is tall");
    let before = tree.window_ids();
    let focused = tree.focused_window();

    tree.set_terminal(Rect::new(0, 0, 200, 60));
    assert_eq!(tree.window_ids(), before);
    assert_eq!(tree.focused_window(), focused);
    assert_tiles(&tree);

    tree.set_terminal(Rect::new(0, 0, 120, 40));
    assert_eq!(tree.window_ids(), before);
    assert_tiles(&tree);
}

#[test]
fn a_terminal_that_is_too_small_hides_windows_and_keeps_the_focus_visible() {
    let mut tree = windows(120, 40);
    tree.split(Orientation::Vertical)
        .expect("the terminal is wide");
    let focused = tree.focused_window();

    // The minimum window width is 20 cells, so 30 cells hold one window only.
    tree.set_terminal(Rect::new(0, 0, 30, 40));
    assert_eq!(tree.window_count(), 2, "the tree keeps both windows");
    assert_eq!(tree.layout().window_count(), 1);
    assert_eq!(area(&tree, focused), Rect::new(0, 0, 30, 40));
    assert_tiles(&tree);
}

#[test]
fn the_default_minimum_dimensions_hold_one_split_in_a_small_terminal() {
    // The confirmation that `docs/windows.md` requests: 20 cells and 3 rows are
    // the smallest window that the layout publishes.
    let settings = WindowSettings::default();
    let mut tree = Windows::new(BUFFER, Rect::new(0, 0, 40, 6), settings);
    let first = tree.focused_window();
    let second = tree
        .split(Orientation::Vertical)
        .expect("the terminal holds two windows");
    assert_eq!(area(&tree, first).width, 20);
    assert_eq!(area(&tree, second).width, 20);

    let third = tree
        .split(Orientation::Horizontal)
        .expect("the terminal holds three windows");
    assert_eq!(area(&tree, second).height, 3);
    assert_eq!(area(&tree, third).height, 3);
    assert_tiles(&tree);
}

#[test]
fn a_split_stops_at_the_depth_limit() {
    let mut tree = windows(200, 60);
    for _ in 0..SPLIT_DEPTH_MAX {
        tree.split(Orientation::Vertical)
            .expect("the depth limit is not reached");
    }
    assert_eq!(
        tree.split(Orientation::Vertical),
        Err(SplitError::DepthLimit)
    );
    assert_eq!(tree.window_count(), SPLIT_DEPTH_MAX + 1);
    assert_tiles(&tree);
}

#[test]
fn a_sidebar_keeps_a_fixed_width_beside_the_tree() {
    let mut tree = windows(120, 40);
    let editor = tree.focused_window();
    let sidebar = tree.open_sidebar(SidebarSide::Right, 30);

    assert_eq!(area(&tree, sidebar), Rect::new(90, 0, 30, 40));
    assert_eq!(area(&tree, editor), Rect::new(0, 0, 90, 40));
    assert_eq!(
        tree.layout().region(sidebar).map(|region| region.kind),
        Some(RegionKind::Sidebar(SidebarSide::Right)),
    );
    assert_eq!(
        tree.window_ids(),
        vec![editor],
        "a sidebar stays out of the tree"
    );
    assert_tiles(&tree);
}

#[test]
fn a_resize_toward_a_sidebar_changes_the_sidebar_width() {
    let mut tree = windows(120, 40);
    let sidebar = tree.open_sidebar(SidebarSide::Right, 30);

    // The named side holds the sidebar, so the shared edge moves right and the
    // sidebar becomes narrower.
    assert_eq!(tree.resize(Direction::Right), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 24);

    // The named side holds no neighbor, so the sidebar edge moves left instead.
    assert_eq!(tree.resize(Direction::Left), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 30);
    assert_tiles(&tree);
}

#[test]
fn a_sidebar_resize_that_would_hide_a_region_is_refused() {
    // The terminal holds a sidebar of 20 cells beside the minimum window width
    // of 20 cells. One more step would leave no room for the editor window.
    let mut tree = windows(45, 40);
    let sidebar = tree.open_sidebar(SidebarSide::Right, 20);
    let before = regions(&tree);

    assert_eq!(tree.resize(Direction::Left), LayoutChange::Unchanged);
    assert_eq!(regions(&tree), before);
    assert_eq!(area(&tree, sidebar).width, 20);
}

#[test]
fn a_hidden_sidebar_cannot_hold_the_focus() {
    let mut tree = windows(120, 40);
    let editor = tree.focused_window();
    let sidebar = tree.open_sidebar(SidebarSide::Right, 30);

    assert_eq!(
        tree.focus_direction(Direction::Right),
        LayoutChange::Changed
    );
    assert_eq!(tree.focused_region(), sidebar);
    assert_eq!(
        tree.focused_window(),
        editor,
        "the editor focus stays valid"
    );

    assert_eq!(
        tree.set_sidebar_visible(SidebarSide::Right, false),
        LayoutChange::Changed,
    );
    assert_eq!(tree.focused_region(), editor);
    assert_eq!(tree.focus_region(sidebar), LayoutChange::Unchanged);
    assert_eq!(area(&tree, editor), Rect::new(0, 0, 120, 40));
    assert_tiles(&tree);
}

#[test]
fn a_narrow_terminal_hides_the_sidebar() {
    let mut tree = windows(120, 40);
    let sidebar = tree.open_sidebar(SidebarSide::Right, 30);
    tree.focus_direction(Direction::Right);
    assert_eq!(tree.focused_region(), sidebar);

    // The sidebar and the minimum window width no longer fit together.
    tree.set_terminal(Rect::new(0, 0, 45, 40));
    assert_eq!(tree.layout().region(sidebar), None);
    assert_eq!(tree.focused_region(), tree.focused_window());
    assert_tiles(&tree);
}

#[test]
fn closing_a_focused_sidebar_hides_it() {
    let mut tree = windows(120, 40);
    let editor = tree.focused_window();
    let sidebar = tree.open_sidebar(SidebarSide::Right, 30);
    tree.focus_direction(Direction::Right);

    assert_eq!(tree.close_focused(), CloseOutcome::Closed(sidebar));
    assert_eq!(tree.focused_region(), editor);
    assert_eq!(tree.window_count(), 1);
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
fn the_viewport_of_a_window_follows_its_rectangle() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = tree
        .split(Orientation::Vertical)
        .expect("the terminal is wide");

    let viewport = tree.viewport(right).expect("the window exists");
    assert_eq!(viewport.width_cells().get(), area(&tree, right).width);
    assert_eq!(viewport.height_rows().get(), 40);

    tree.set_terminal(Rect::new(0, 0, 80, 20));
    let viewport = tree.viewport(left).expect("the window exists");
    assert_eq!(viewport.width_cells().get(), area(&tree, left).width);
    assert_eq!(viewport.height_rows().get(), 20);
}

#[test]
fn a_split_and_a_terminal_resize_keep_the_scroll_offset() {
    let mut tree = windows(120, 40);
    let scrolled = tree.focused_window();

    // Scroll the window down and right, so both offsets leave the buffer start.
    let line = "x".repeat(400);
    let text = format!("{line}\n").repeat(200);
    let buffer = TextBuffer::from_text(&text, &FileSettings::default()).expect("the text is small");
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
    assert_eq!(viewport.height_rows().get(), area(&tree, scrolled).height);

    // A terminal resize changes both dimensions of every window.
    tree.set_terminal(Rect::new(0, 0, 60, 24));
    let viewport = tree.viewport(scrolled).expect("the window exists");
    assert_eq!(viewport.first_line(), scroll.first_line());
    assert_eq!(viewport.left_column(), scroll.left_column());
    assert_eq!(viewport.height_rows().get(), area(&tree, scrolled).height);
    assert_eq!(viewport.width_cells().get(), area(&tree, scrolled).width);
}
