//! Tests for the window tree, the layout calculation, focus, resize, and sidebars.

use ratatui::layout::Rect;

use crate::{
    ChildSide, CloseOutcome, Direction, LayoutChange, LayoutFit, Orientation, RegionError,
    RegionKind, SPLIT_DEPTH_MAX, SidebarSide, SplitError, WindowId, WindowLayout, WindowLimits,
    WindowTree,
};

/// The surface identity that the host owns. The tree never reads the value.
type Surface = u32;

const SURFACE: Surface = 7;

/// The resize step of these tests, in cells.
const STEP: u16 = 6;

fn windows(width: u16, height: u16) -> WindowTree<Surface> {
    WindowTree::new(
        SURFACE,
        Rect::new(0, 0, width, height),
        WindowLimits::default(),
    )
}

fn area(tree: &WindowTree<Surface>, id: WindowId) -> Rect {
    tree.layout()
        .area(id)
        .expect("the test expects a visible region")
}

fn split(tree: &mut WindowTree<Surface>, orientation: Orientation) -> WindowId {
    tree.split(orientation, ChildSide::Second)
        .expect("the test expects room for both windows")
}

fn focus(tree: &mut WindowTree<Surface>, id: WindowId) {
    tree.focus_region(id).expect("the test expects a region");
}

/// Confirms that the regions cover the host area without a gap and without an
/// overlap, and that every region belongs to the tree or to a sidebar.
fn assert_tiles(tree: &WindowTree<Surface>) {
    let layout = tree.layout();
    let host = tree.area();
    let covered: u32 = layout
        .regions()
        .iter()
        .map(|region| region.area.area())
        .sum();
    assert_eq!(covered, host.area(), "the regions cover the host area");
    for (index, first) in layout.regions().iter().enumerate() {
        assert!(
            host.union(first.area) == host,
            "a region stays inside the host area"
        );
        for second in &layout.regions()[index + 1..] {
            assert!(
                first.area.intersection(second.area).is_empty(),
                "two regions never overlap: {first:?} and {second:?}",
            );
        }
    }
    let mut ids = tree.window_ids();
    ids.sort_unstable();
    let unique = {
        let mut unique = ids.clone();
        unique.dedup();
        unique
    };
    assert_eq!(ids, unique, "every window identity is unique");
    assert!(
        layout.area(tree.focused_window()).is_some(),
        "the layout keeps the focused window visible",
    );
}

/// Confirms that every visible window keeps the minimum dimensions.
fn assert_minimums(tree: &WindowTree<Surface>) {
    let limits = tree.limits();
    for region in tree.layout().regions() {
        if region.kind != RegionKind::Surface {
            continue;
        }
        assert!(
            region.area.width >= limits.min_width_cells()
                && region.area.height >= limits.min_height_rows(),
            "a visible window never falls below the minimum: {region:?}",
        );
    }
}

fn regions(tree: &WindowTree<Surface>) -> WindowLayout {
    tree.layout().clone()
}

#[test]
fn a_vertical_split_opens_the_new_window_to_the_right() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = split(&mut tree, Orientation::Vertical);

    assert_eq!(tree.focused_window(), right);
    assert_eq!(
        tree.surface(right),
        Some(&SURFACE),
        "the new window shows the surface of the source window"
    );
    assert!(area(&tree, left).x < area(&tree, right).x);
    assert_eq!(area(&tree, left).height, 40);
    assert_tiles(&tree);
}

#[test]
fn a_horizontal_split_opens_the_new_window_below() {
    let mut tree = windows(120, 40);
    let top = tree.focused_window();
    let bottom = split(&mut tree, Orientation::Horizontal);

    assert!(area(&tree, top).y < area(&tree, bottom).y);
    assert_eq!(area(&tree, top).width, 120);
    assert_tiles(&tree);
}

#[test]
fn the_named_child_receives_the_new_window() {
    let mut tree = windows(120, 40);
    let source = tree.focused_window();
    let created = tree
        .split(Orientation::Horizontal, ChildSide::First)
        .expect("the area is tall");

    assert!(area(&tree, created).y < area(&tree, source).y);
}

#[test]
fn the_layout_is_deterministic() {
    let mut first = windows(120, 40);
    let mut second = windows(120, 40);
    for tree in [&mut first, &mut second] {
        split(tree, Orientation::Vertical);
        split(tree, Orientation::Horizontal);
        tree.focus_direction(Direction::Left);
        tree.resize(Direction::Right, STEP);
    }

    assert_eq!(regions(&first), regions(&second));

    // An equal host size recomputes equal rectangles.
    let before = regions(&first);
    first.set_area(Rect::new(0, 0, 120, 40));
    assert_eq!(regions(&first), before);
}

#[test]
fn directional_focus_uses_the_rectangles() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = split(&mut tree, Orientation::Vertical);
    let bottom_right = split(&mut tree, Orientation::Horizontal);

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
    let top_right = split(&mut tree, Orientation::Vertical);
    let bottom_right = split(&mut tree, Orientation::Horizontal);

    // Grow the lower right window, so it overlaps more of the left window.
    assert_eq!(tree.resize(Direction::Up, STEP), LayoutChange::Changed);
    assert!(area(&tree, bottom_right).height > area(&tree, top_right).height);

    focus(&mut tree, left);
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
    let right = split(&mut tree, Orientation::Vertical);
    focus(&mut tree, left);

    let before = area(&tree, left).width;
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
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
    let middle = split(&mut tree, Orientation::Vertical);
    let right = split(&mut tree, Orientation::Vertical);
    focus(&mut tree, middle);
    assert_eq!(area(&tree, left).width, 60);
    assert_eq!(area(&tree, middle).width, 30);
    assert_eq!(area(&tree, right).width, 30);

    // The command moves the right edge left, so the focused window shrinks.
    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 60, "the left edge stays in place");
    assert_eq!(area(&tree, middle).width, 24);
    assert_eq!(area(&tree, right).width, 36);

    // The command moves the right edge right, so the focused window grows.
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
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
    let middle = split(&mut tree, Orientation::Horizontal);
    let bottom = split(&mut tree, Orientation::Horizontal);
    focus(&mut tree, middle);
    assert_eq!(area(&tree, top).height, 30);
    assert_eq!(area(&tree, middle).height, 15);
    assert_eq!(area(&tree, bottom).height, 15);

    // The command moves the bottom edge up, so the focused window shrinks.
    assert_eq!(tree.resize(Direction::Up, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, top).height, 30, "the top edge stays in place");
    assert_eq!(area(&tree, middle).height, 9);
    assert_eq!(area(&tree, bottom).height, 21);

    // The command moves the bottom edge down, so the focused window grows.
    assert_eq!(tree.resize(Direction::Down, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, top).height, 30, "the top edge stays in place");
    assert_eq!(area(&tree, middle).height, 15);
    assert_eq!(area(&tree, bottom).height, 15);
    assert_tiles(&tree);
}

#[test]
fn a_resize_without_a_far_neighbor_moves_the_near_edge() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = split(&mut tree, Orientation::Vertical);

    // The focused window sits on the right, so the right edge holds no
    // neighbor. The command moves the left edge right, which shrinks it.
    let before = area(&tree, right).width;
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
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
        assert_eq!(tree.resize(direction, STEP), LayoutChange::Unchanged);
    }
    assert_eq!(regions(&tree), before);
}

#[test]
fn a_resize_below_the_minimum_leaves_the_layout_unchanged() {
    // Two windows of 20 cells already sit at the minimum window width.
    let mut tree = windows(40, 40);
    split(&mut tree, Orientation::Vertical);
    let before = regions(&tree);

    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Unchanged);
    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Unchanged);
    assert_eq!(regions(&tree), before);
}

/// Creates three windows in one row and returns them from left to right.
///
/// The row holds 60, 30, and 30 cells, so every window stays above the minimum
/// window width of 20 cells.
fn row_of_three(tree: &mut WindowTree<Surface>) -> (WindowId, WindowId, WindowId) {
    let left = tree.focused_window();
    let middle = split(tree, Orientation::Vertical);
    let right = split(tree, Orientation::Vertical);
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
    focus(&mut tree, left);
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
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
    focus(&mut tree, middle);
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
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
fn a_row_resize_from_the_middle_window_keeps_the_far_border() {
    // Three panes side by side with the focus in the middle one. Both commands
    // move the border that the middle window shares with the right window, so
    // the left border never moves.
    let mut tree = windows(120, 40);
    let (left, middle, right) = row_of_three(&mut tree);
    focus(&mut tree, middle);

    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 60, "the far border stays in place");
    assert_eq!(area(&tree, middle).width, 36);
    assert_eq!(area(&tree, right).width, 24);
    assert_tiles(&tree);

    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 60, "the far border stays in place");
    assert_eq!(area(&tree, middle).width, 30);
    assert_eq!(area(&tree, right).width, 30);
    assert_tiles(&tree);

    // The opposite command reproduces the exact cell sizes, so no step drifts.
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 60);
    assert_eq!(area(&tree, middle).width, 30);
    assert_eq!(area(&tree, right).width, 30);
}

/// Creates one sidebar of 40 cells and three panes of 60, 30, and 30 cells.
fn row_beside_a_sidebar(
    tree: &mut WindowTree<Surface>,
    side: SidebarSide,
) -> (WindowId, WindowId, WindowId, WindowId) {
    let sidebar = tree
        .open_sidebar(side, 40)
        .expect("the tree issues one more identity");
    let (left, middle, right) = row_of_three(tree);
    (sidebar, left, middle, right)
}

#[test]
fn a_sidebar_resize_moves_one_border_and_keeps_every_other_pane() {
    // Three panes beside the sidebar. The command moves the inner border of the
    // sidebar, so only the pane at that border changes its width.
    let mut tree = windows(160, 40);
    let (sidebar, left, middle, right) = row_beside_a_sidebar(&mut tree, SidebarSide::Right);
    focus(&mut tree, right);

    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 34);
    assert_eq!(area(&tree, right).width, 36);
    assert_eq!(
        area(&tree, left).width,
        60,
        "the pane keeps its exact cells"
    );
    assert_eq!(area(&tree, middle).width, 30);
    assert_tiles(&tree);

    // The opposite command restores the exact cell sizes, so no step drifts.
    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 40);
    assert_eq!(area(&tree, left).width, 60);
    assert_eq!(area(&tree, middle).width, 30);
    assert_eq!(area(&tree, right).width, 30);
    assert_tiles(&tree);
}

#[test]
fn a_sidebar_resize_from_the_sidebar_moves_the_same_border() {
    // The focused sidebar answers the resize command itself, and it moves the
    // same border as a command from the neighboring window.
    let mut tree = windows(160, 40);
    let (sidebar, left, middle, right) = row_beside_a_sidebar(&mut tree, SidebarSide::Right);
    assert_eq!(tree.focus_region(sidebar), Ok(LayoutChange::Changed));

    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 46);
    assert_eq!(area(&tree, right).width, 24);
    assert_eq!(area(&tree, left).width, 60);
    assert_eq!(area(&tree, middle).width, 30);
    assert_eq!(
        tree.focused_region(),
        sidebar,
        "the focus stays in the sidebar"
    );
    assert_tiles(&tree);

    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 40);
    assert_eq!(area(&tree, left).width, 60);
    assert_eq!(area(&tree, middle).width, 30);
    assert_eq!(area(&tree, right).width, 30);
    assert_tiles(&tree);
}

#[test]
fn a_left_sidebar_resize_moves_the_pane_at_its_border() {
    // The left sidebar shares its border with the leftmost pane, so that pane
    // absorbs the cells and the two panes to its right keep their exact width.
    let mut tree = windows(160, 40);
    let (sidebar, left, middle, right) = row_beside_a_sidebar(&mut tree, SidebarSide::Left);
    assert_eq!(tree.focus_region(sidebar), Ok(LayoutChange::Changed));

    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 46);
    assert_eq!(area(&tree, left).width, 54);
    assert_eq!(
        area(&tree, middle).width,
        30,
        "the other panes keep their cells"
    );
    assert_eq!(area(&tree, right).width, 30);
    assert_tiles(&tree);

    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 40);
    assert_eq!(area(&tree, left).width, 60);
    assert_eq!(area(&tree, middle).width, 30);
    assert_eq!(area(&tree, right).width, 30);
    assert_tiles(&tree);
}

#[test]
fn a_sidebar_resize_cascades_past_a_pane_that_reaches_its_minimum() {
    let mut tree = windows(160, 40);
    let (sidebar, left, middle, right) = row_beside_a_sidebar(&mut tree, SidebarSide::Right);
    focus(&mut tree, right);

    // The right pane gives the first six cells alone.
    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, right).width, 24);

    // It reaches the minimum width of 20 cells after four more cells, so the
    // middle pane gives the remaining two.
    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 52);
    assert_eq!(area(&tree, left).width, 60);
    assert_eq!(area(&tree, middle).width, 28);
    assert_eq!(area(&tree, right).width, 20);
    assert_tiles(&tree);

    // The three panes need 60 cells, so the sidebar stops where they reach it.
    while tree.resize(Direction::Left, STEP) == LayoutChange::Changed {}
    assert_eq!(area(&tree, sidebar).width, 100);
    assert_eq!(area(&tree, left).width, 20);
    assert_eq!(area(&tree, middle).width, 20);
    assert_eq!(area(&tree, right).width, 20);
    assert_tiles(&tree);
}

#[test]
fn a_stacked_resize_moves_one_border_and_keeps_every_other_pane() {
    // Three stacked windows of 30, 15, and 15 rows.
    let mut tree = windows(120, 60);
    let top = tree.focused_window();
    let middle = split(&mut tree, Orientation::Horizontal);
    let bottom = split(&mut tree, Orientation::Horizontal);
    focus(&mut tree, top);

    assert_eq!(tree.resize(Direction::Down, STEP), LayoutChange::Changed);
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
    focus(&mut tree, left);

    // The first step takes six cells from the middle window.
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, middle).width, 24);
    assert_eq!(area(&tree, right).width, 30);

    // The middle window reaches the minimum width of 20 cells after four more
    // cells, so the right window gives the remaining two.
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 72);
    assert_eq!(area(&tree, middle).width, 20);
    assert_eq!(area(&tree, right).width, 28);

    // The middle window can give nothing more, so the right window gives every
    // cell of the next step.
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, left).width, 78);
    assert_eq!(area(&tree, middle).width, 20);
    assert_eq!(area(&tree, right).width, 22);
    assert_tiles(&tree);
}

#[test]
fn a_resize_without_room_for_every_minimum_leaves_the_layout_unchanged() {
    let mut tree = windows(120, 40);
    let (left, middle, right) = row_of_three(&mut tree);
    focus(&mut tree, left);
    for _ in 0..3 {
        assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
    }
    let before = regions(&tree);

    // The middle window and the right window hold 42 cells and need 40, so no
    // arrangement of the next six cells keeps both above the minimum.
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Unchanged);
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
    let top_right = split(&mut tree, Orientation::Vertical);
    let bottom_right = split(&mut tree, Orientation::Horizontal);
    focus(&mut tree, top_right);
    assert_eq!(area(&tree, left).width, 60);
    assert_eq!(area(&tree, top_right).height, 20);

    // The focused window holds no neighbor on its right, so the command moves
    // the left border. Both windows of the right column follow that border, and
    // their heights stay as they are.
    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Changed);
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
    let bottom = split(&mut tree, Orientation::Horizontal);

    // The focused window sits at the bottom, so the bottom edge holds no
    // neighbor. The command moves the top edge up, which grows it.
    let before = area(&tree, bottom).height;
    assert_eq!(tree.resize(Direction::Up, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, bottom).height, before + 6);
    assert_eq!(area(&tree, top).height, 40 - before - 6);
    assert_tiles(&tree);
}

#[test]
fn closing_a_window_keeps_the_sibling_and_its_identity() {
    let mut tree = windows(120, 40);
    let left = tree.focused_window();
    let right = split(&mut tree, Orientation::Vertical);

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
    let top_right = split(&mut tree, Orientation::Vertical);
    let bottom_right = split(&mut tree, Orientation::Horizontal);

    focus(&mut tree, left);
    assert_eq!(tree.close_focused(), CloseOutcome::Closed(left));
    assert_eq!(tree.window_ids(), vec![top_right, bottom_right]);
    assert_eq!(tree.focused_window(), top_right);
    assert_tiles(&tree);
}

#[test]
fn a_closed_identity_never_returns() {
    // The identity allocator is checked and monotonic, so a new window never
    // reuses the identity of a closed window.
    let mut tree = windows(120, 40);
    let first = split(&mut tree, Orientation::Vertical);
    assert_eq!(tree.close_focused(), CloseOutcome::Closed(first));
    let second = split(&mut tree, Orientation::Vertical);

    assert_ne!(second, first);
    assert!(second.get() > first.get());
}

#[test]
fn a_host_resize_keeps_the_tree_and_every_identity() {
    let mut tree = windows(120, 40);
    split(&mut tree, Orientation::Vertical);
    split(&mut tree, Orientation::Horizontal);
    let before = tree.window_ids();
    let focused = tree.focused_window();

    assert_eq!(tree.set_area(Rect::new(0, 0, 200, 60)), LayoutFit::Complete);
    assert_eq!(tree.window_ids(), before);
    assert_eq!(tree.focused_window(), focused);
    assert_tiles(&tree);

    assert_eq!(tree.set_area(Rect::new(0, 0, 120, 40)), LayoutFit::Complete);
    assert_eq!(tree.window_ids(), before);
    assert_tiles(&tree);
}

#[test]
fn a_host_area_that_is_too_small_reports_its_hidden_windows() {
    let mut tree = windows(120, 40);
    split(&mut tree, Orientation::Vertical);
    let focused = tree.focused_window();

    // The minimum window width is 20 cells, so 30 cells hold one window only.
    assert_eq!(
        tree.set_area(Rect::new(0, 0, 30, 40)),
        LayoutFit::Constrained {
            hidden_windows: 1,
            hidden_sidebars: 0,
        },
    );
    assert_eq!(tree.window_count(), 2, "the tree keeps both windows");
    assert_eq!(tree.layout().window_count(), 1);
    assert_eq!(area(&tree, focused), Rect::new(0, 0, 30, 40));
    assert_tiles(&tree);
}

#[test]
fn a_nested_subtree_receives_its_own_recursive_minimum() {
    // The regression: the layout divided every split node by one leaf minimum,
    // so a nested subtree of two windows could receive the space of one window
    // and lose a leaf. Both child minima now decide the allocation.
    let mut tree = windows(120, 40);
    let first = tree.focused_window();
    let second = split(&mut tree, Orientation::Vertical);
    let third = split(&mut tree, Orientation::Vertical);

    // Three windows of 20 cells need exactly 60 cells.
    assert_eq!(tree.set_area(Rect::new(0, 0, 60, 40)), LayoutFit::Complete);
    assert_eq!(tree.layout().window_count(), 3);
    assert_eq!(area(&tree, first).width, 20);
    assert_eq!(area(&tree, second).width, 20);
    assert_eq!(area(&tree, third).width, 20);
    assert_minimums(&tree);
    assert_tiles(&tree);
}

#[test]
fn a_split_that_cannot_show_both_subtrees_is_refused() {
    // The regression: an explicit split succeeded while the layout hid the new
    // window, and the following resize worked on an extent that no layout
    // produced. The split now refuses the area and changes nothing.
    let mut tree = windows(30, 40);
    let only = tree.focused_window();
    let before = regions(&tree);

    assert_eq!(
        tree.split(Orientation::Vertical, ChildSide::Second),
        Err(SplitError::AreaTooSmall {
            available: 30,
            required: 40,
        }),
    );
    assert_eq!(tree.window_count(), 1);
    assert_eq!(tree.focused_window(), only);
    assert_eq!(regions(&tree), before);

    // The refused split leaves a layout that every resize command accepts.
    for direction in [
        Direction::Left,
        Direction::Down,
        Direction::Up,
        Direction::Right,
    ] {
        assert_eq!(tree.resize(direction, STEP), LayoutChange::Unchanged);
    }
}

#[test]
fn a_nested_split_that_cannot_show_both_subtrees_is_refused() {
    // Two windows hold 25 cells each, and a third window would need 60 cells in
    // an area of 50. The recursive minima refuse the split instead of hiding a
    // leaf that the layout cannot place.
    let mut tree = windows(50, 40);
    let created = split(&mut tree, Orientation::Vertical);
    assert_eq!(area(&tree, created).width, 25);
    let before = regions(&tree);

    assert_eq!(
        tree.split(Orientation::Vertical, ChildSide::Second),
        Err(SplitError::AreaTooSmall {
            available: 25,
            required: 40,
        }),
    );
    assert_eq!(tree.window_count(), 2);
    assert_eq!(regions(&tree), before);
    assert_minimums(&tree);

    // A resize after the refused split never reads an invalid extent.
    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Unchanged);
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Unchanged);
    assert_eq!(regions(&tree), before);
}

#[test]
fn the_default_minimum_dimensions_hold_one_split_in_a_small_host_area() {
    // The confirmation that `docs/windows.md` requests: 20 cells and 3 rows are
    // the smallest window that the layout publishes.
    let mut tree = windows(40, 6);
    let first = tree.focused_window();
    let second = split(&mut tree, Orientation::Vertical);
    assert_eq!(area(&tree, first).width, 20);
    assert_eq!(area(&tree, second).width, 20);

    let third = split(&mut tree, Orientation::Horizontal);
    assert_eq!(area(&tree, second).height, 3);
    assert_eq!(area(&tree, third).height, 3);
    assert_minimums(&tree);
    assert_tiles(&tree);
}

#[test]
fn a_split_stops_at_the_depth_limit() {
    // Seventeen windows of 20 cells need 340 cells, so the area holds them all
    // and the depth limit is the only rule that stops the last split.
    let mut tree = windows(400, 60);
    for _ in 0..SPLIT_DEPTH_MAX {
        split(&mut tree, Orientation::Vertical);
    }
    assert_eq!(
        tree.split(Orientation::Vertical, ChildSide::Second),
        Err(SplitError::DepthLimit)
    );
    assert_eq!(tree.window_count(), SPLIT_DEPTH_MAX + 1);
    assert_eq!(tree.layout().fit(), LayoutFit::Complete);
    assert_minimums(&tree);
    assert_tiles(&tree);
}

#[test]
fn a_sidebar_keeps_a_fixed_width_beside_the_tree() {
    let mut tree = windows(120, 40);
    let window = tree.focused_window();
    let sidebar = tree
        .open_sidebar(SidebarSide::Right, 30)
        .expect("the tree issues one more identity");

    assert_eq!(area(&tree, sidebar), Rect::new(90, 0, 30, 40));
    assert_eq!(area(&tree, window), Rect::new(0, 0, 90, 40));
    assert_eq!(
        tree.layout().region(sidebar).map(|region| region.kind),
        Some(RegionKind::Sidebar(SidebarSide::Right)),
    );
    assert_eq!(
        tree.window_ids(),
        vec![window],
        "a sidebar stays out of the tree"
    );
    assert_tiles(&tree);
}

#[test]
fn a_resize_toward_a_sidebar_changes_the_sidebar_width() {
    let mut tree = windows(120, 40);
    let sidebar = tree
        .open_sidebar(SidebarSide::Right, 30)
        .expect("the tree issues one more identity");

    // The named side holds the sidebar, so the shared edge moves right and the
    // sidebar becomes narrower.
    assert_eq!(tree.resize(Direction::Right, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 24);

    // The named side holds no neighbor, so the sidebar edge moves left instead.
    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Changed);
    assert_eq!(area(&tree, sidebar).width, 30);
    assert_tiles(&tree);
}

#[test]
fn a_sidebar_resize_that_would_hide_a_region_is_refused() {
    // The host area holds a sidebar of 20 cells beside the minimum window width
    // of 20 cells. One more step would leave no room for the window.
    let mut tree = windows(45, 40);
    let sidebar = tree
        .open_sidebar(SidebarSide::Right, 20)
        .expect("the tree issues one more identity");
    let before = regions(&tree);

    assert_eq!(tree.resize(Direction::Left, STEP), LayoutChange::Unchanged);
    assert_eq!(regions(&tree), before);
    assert_eq!(area(&tree, sidebar).width, 20);
}

#[test]
fn a_hidden_sidebar_cannot_hold_the_focus() {
    let mut tree = windows(120, 40);
    let window = tree.focused_window();
    let sidebar = tree
        .open_sidebar(SidebarSide::Right, 30)
        .expect("the tree issues one more identity");

    assert_eq!(
        tree.focus_direction(Direction::Right),
        LayoutChange::Changed
    );
    assert_eq!(tree.focused_region(), sidebar);
    assert_eq!(
        tree.focused_window(),
        window,
        "the window focus stays valid"
    );

    assert_eq!(
        tree.set_sidebar_visible(SidebarSide::Right, false),
        LayoutChange::Changed,
    );
    assert_eq!(tree.focused_region(), window);
    assert_eq!(
        tree.focus_region(sidebar),
        Err(RegionError::Hidden(sidebar)),
        "a hidden region refuses the focus"
    );
    assert_eq!(area(&tree, window), Rect::new(0, 0, 120, 40));
    assert_tiles(&tree);
}

#[test]
fn a_narrow_host_area_hides_the_sidebar() {
    let mut tree = windows(120, 40);
    let sidebar = tree
        .open_sidebar(SidebarSide::Right, 30)
        .expect("the tree issues one more identity");
    tree.focus_direction(Direction::Right);
    assert_eq!(tree.focused_region(), sidebar);

    // The sidebar and the minimum window width no longer fit together.
    assert_eq!(
        tree.set_area(Rect::new(0, 0, 45, 40)),
        LayoutFit::Constrained {
            hidden_windows: 0,
            hidden_sidebars: 1,
        },
    );
    assert_eq!(tree.layout().region(sidebar), None);
    assert_eq!(tree.focused_region(), tree.focused_window());
    assert_tiles(&tree);
}

#[test]
fn closing_a_focused_sidebar_hides_it() {
    let mut tree = windows(120, 40);
    let window = tree.focused_window();
    let sidebar = tree
        .open_sidebar(SidebarSide::Right, 30)
        .expect("the tree issues one more identity");
    tree.focus_direction(Direction::Right);

    assert_eq!(tree.close_focused(), CloseOutcome::Closed(sidebar));
    assert_eq!(tree.focused_region(), window);
    assert_eq!(tree.window_count(), 1);
}

#[test]
fn an_unknown_region_reports_a_distinct_error() {
    let mut tree = windows(120, 40);
    let window = tree.focused_window();
    let sidebar = tree
        .open_sidebar(SidebarSide::Left, 20)
        .expect("the tree issues one more identity");
    tree.set_sidebar_visible(SidebarSide::Left, false);
    let closed = split(&mut tree, Orientation::Vertical);
    tree.close_focused();

    // A hidden region and an unknown region report distinct errors.
    assert_eq!(
        tree.focus_region(sidebar),
        Err(RegionError::Hidden(sidebar))
    );
    assert_eq!(tree.focus_region(closed), Err(RegionError::Unknown(closed)));
    assert_eq!(
        tree.replace_surface(closed, 11),
        Err(RegionError::Unknown(closed)),
    );
    assert_eq!(tree.replace_surface(window, 11), Ok(SURFACE));
    assert_eq!(tree.surface(window), Some(&11));
}
