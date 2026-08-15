//! The window tree: leaf windows, split nodes, focus, splits, resize, and sidebars.
//!
//! The module is deterministic and pure. It reads no clock, no filesystem, and
//! no terminal. It holds no buffer text and no color. It holds window
//! identities, split structure, sidebar widths, and the terminal rectangle.
//!
//! [`Windows`] owns the tree and one cached [`WindowLayout`]. Every operation
//! recomputes that layout, so the tree and the rectangles never disagree. No
//! other code computes a window rectangle. See `docs/windows.md`.

use std::num::NonZeroU16;

use ratatui::layout::Rect;
use thiserror::Error;

use crate::editor::Viewport;
use crate::input::Command;
use crate::settings::{HorizontalSplitPlacement, VerticalSplitPlacement, WindowSettings};

use super::layout::{RegionKind, WindowLayout, compute_layout};

/// The largest number of leaf windows that the tree holds.
///
/// The bound stops a repeated split command from exhausting memory.
pub const WINDOWS_MAX: usize = 64;

/// The largest number of split levels between the root and one leaf window.
///
/// The bound stops a repeated split command from building an unbounded
/// recursion depth for the layout calculation.
pub const SPLIT_DEPTH_MAX: usize = 16;

/// The denominator of one split weight.
///
/// A split node stores the share of its first child as an integer numerator
/// over this value, so the layout calculation uses integer arithmetic only.
pub const SPLIT_WEIGHT_TOTAL: u32 = 1_000;

/// The smallest width that a sidebar accepts, in cells.
pub const SIDEBAR_WIDTH_MIN_CELLS: u16 = 10;

/// The largest width that a sidebar accepts, in cells.
pub const SIDEBAR_WIDTH_MAX_CELLS: u16 = 200;

/// The stable identity of one window or one sidebar.
///
/// The identity stays stable while the region exists. A split, a close, and a
/// terminal resize never change an existing identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId(u32);

impl WindowId {
    /// Returns the identity value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// The reference from one leaf window to the buffer that it shows.
///
/// The window tree only stores and compares the value. Slice 9 owns buffer
/// loading and buffer identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BufferId(u32);

impl BufferId {
    /// Creates a buffer reference from its identity value.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the identity value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// The arrangement of the two children of one split node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Orientation {
    /// Stack the children top and bottom.
    Horizontal,
    /// Place the children left and right.
    Vertical,
}

impl Orientation {
    /// Returns the other orientation.
    #[must_use]
    pub const fn inverse(self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }
}

/// One of the four sides of a window rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// Toward smaller column numbers.
    Left,
    /// Toward larger row numbers.
    Down,
    /// Toward smaller row numbers.
    Up,
    /// Toward larger column numbers.
    Right,
}

impl Direction {
    /// Returns the opposite side.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Down => Self::Up,
            Self::Up => Self::Down,
            Self::Right => Self::Left,
        }
    }

    /// Returns the split orientation that owns a divider on this side.
    #[must_use]
    pub const fn orientation(self) -> Orientation {
        match self {
            Self::Left | Self::Right => Orientation::Vertical,
            Self::Down | Self::Up => Orientation::Horizontal,
        }
    }

    /// Returns the edge that a resize command along this axis prefers.
    ///
    /// The far edge is the right edge for a horizontal command and the bottom
    /// edge for a vertical command. The far edge always wins while both edges
    /// hold a neighbor, so one key always moves the layout in one direction.
    const fn far_edge(self) -> Self {
        match self {
            Self::Left | Self::Right => Self::Right,
            Self::Up | Self::Down => Self::Down,
        }
    }

    /// Returns the child that a window occupies when the divider on this side
    /// belongs to one of its ancestors.
    const fn divider_side(self) -> ChildSide {
        match self {
            Self::Right | Self::Down => ChildSide::First,
            Self::Left | Self::Up => ChildSide::Second,
        }
    }

    /// Returns the sign that moves a divider toward this side.
    const fn divider_step(self) -> i32 {
        match self {
            Self::Right | Self::Down => 1,
            Self::Left | Self::Up => -1,
        }
    }
}

/// The edge of the terminal that holds one sidebar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SidebarSide {
    /// The left edge of the terminal.
    Left,
    /// The right edge of the terminal.
    Right,
}

/// A fixed-width region at one edge of the terminal.
///
/// A sidebar has no place in the window tree, keeps a fixed width instead of a
/// ratio, and never takes part in an adaptive split.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sidebar {
    id: WindowId,
    side: SidebarSide,
    width_cells: u16,
    visible: bool,
}

impl Sidebar {
    /// Returns the stable identity of the sidebar.
    #[must_use]
    pub const fn id(self) -> WindowId {
        self.id
    }

    /// Returns the edge that holds the sidebar.
    #[must_use]
    pub const fn side(self) -> SidebarSide {
        self.side
    }

    /// Returns the requested width, in cells.
    #[must_use]
    pub const fn width_cells(self) -> u16 {
        self.width_cells
    }

    /// Reports whether the caller asks for the sidebar.
    ///
    /// A visible sidebar still stays out of the layout while the terminal is
    /// too narrow to hold it beside the minimum editor width.
    #[must_use]
    pub const fn is_visible(self) -> bool {
        self.visible
    }
}

/// The rule that the adaptive split command applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptiveSplit {
    /// Select a vertical split for a wide window.
    Normal,
    /// Select a horizontal split for a wide window.
    Inverse,
}

/// The result of one focus or resize command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutChange {
    /// The command changed the focus or the rectangles.
    Changed,
    /// The command left the focus and the rectangles unchanged.
    Unchanged,
}

/// The result of one close command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseOutcome {
    /// The tree removed the named region.
    Closed(WindowId),
    /// The tree holds one window, so the caller decides whether to quit.
    LastWindow,
}

/// The result of one window command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowOutcome {
    /// The command does not address the window tree.
    Ignored,
    /// The command changed the window tree, the focus, or the rectangles.
    Changed,
    /// The command addressed the window tree and changed nothing.
    Unchanged,
    /// The close command reached the last window.
    LastWindow,
}

/// The reason that a split command produced no new window.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SplitError {
    /// The tree already holds [`WINDOWS_MAX`] windows.
    #[error("the window tree already holds the largest number of windows")]
    WindowLimit,
    /// The focused window already sits at [`SPLIT_DEPTH_MAX`] split levels.
    #[error("the focused window already sits at the largest split depth")]
    DepthLimit,
}

/// The identity of one split node.
///
/// The layout calculation reports the rectangle of every materialized split
/// node under this identity, so a resize command finds the divider without a
/// second layout rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SplitId(u32);

/// The child of one split node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChildSide {
    /// The left child of a vertical split, or the top child of a horizontal split.
    First,
    /// The right child of a vertical split, or the bottom child of a horizontal split.
    Second,
}

/// One window that shows one buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Leaf {
    pub(super) id: WindowId,
    buffer: BufferId,
    viewport: Viewport,
}

impl Leaf {
    /// A leaf that never reaches the caller.
    ///
    /// [`close_leaf`] writes this value into the tree for the moment between
    /// taking a split node apart and writing back the surviving child. The
    /// identity value zero is never issued.
    const VOID: Self = Self {
        id: WindowId(0),
        buffer: BufferId(0),
        viewport: Viewport::new(NonZeroU16::MIN, NonZeroU16::MIN),
    };
}

/// One node of the window tree.
#[derive(Clone, Debug)]
pub(super) enum Node {
    /// One window that shows one buffer.
    Leaf(Leaf),
    /// Two children with one shared divider.
    Split {
        id: SplitId,
        orientation: Orientation,
        /// The share of the split extent that the first child requests.
        first_weight: u32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    /// Returns the number of leaf windows in the subtree.
    fn window_count(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Split { first, second, .. } => first.window_count() + second.window_count(),
        }
    }

    /// Reports whether the subtree holds the named window.
    pub(super) fn contains(&self, id: WindowId) -> bool {
        match self {
            Self::Leaf(leaf) => leaf.id == id,
            Self::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    /// Returns the first leaf of the subtree in layout order.
    fn first_leaf_id(&self) -> WindowId {
        match self {
            Self::Leaf(leaf) => leaf.id,
            Self::Split { first, .. } => first.first_leaf_id(),
        }
    }

    /// Collects every leaf window of the subtree in layout order.
    fn collect_ids(&self, out: &mut Vec<WindowId>) {
        match self {
            Self::Leaf(leaf) => out.push(leaf.id),
            Self::Split { first, second, .. } => {
                first.collect_ids(out);
                second.collect_ids(out);
            }
        }
    }

    fn leaf(&self, id: WindowId) -> Option<&Leaf> {
        match self {
            Self::Leaf(leaf) => (leaf.id == id).then_some(leaf),
            Self::Split { first, second, .. } => first.leaf(id).or_else(|| second.leaf(id)),
        }
    }

    fn leaf_mut(&mut self, id: WindowId) -> Option<&mut Leaf> {
        match self {
            Self::Leaf(leaf) => (leaf.id == id).then_some(leaf),
            Self::Split { first, second, .. } => match first.leaf_mut(id) {
                Some(leaf) => Some(leaf),
                None => second.leaf_mut(id),
            },
        }
    }

    /// Collects the split nodes between the root and the named window.
    ///
    /// The steps run from the root to the leaf. The function returns `false`
    /// and leaves `path` unchanged when the subtree does not hold the window.
    fn leaf_path(&self, id: WindowId, path: &mut Vec<PathStep>) -> bool {
        let Self::Split {
            id: split,
            orientation,
            first,
            second,
            ..
        } = self
        else {
            return matches!(self, Self::Leaf(leaf) if leaf.id == id);
        };
        for (side, child) in [(ChildSide::First, first), (ChildSide::Second, second)] {
            path.push(PathStep {
                split: *split,
                orientation: *orientation,
                side,
            });
            if child.leaf_path(id, path) {
                return true;
            }
            path.pop();
        }
        false
    }
}

/// One split node between the root and one leaf window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PathStep {
    split: SplitId,
    orientation: Orientation,
    /// The child that holds the leaf window.
    side: ChildSide,
}

/// Replaces the named leaf window with a split node.
///
/// Returns `false` when the subtree does not hold the window.
fn split_leaf(
    node: &mut Node,
    target: WindowId,
    id: SplitId,
    orientation: Orientation,
    new_leaf: Leaf,
    new_side: ChildSide,
) -> bool {
    match node {
        Node::Leaf(leaf) if leaf.id == target => {
            let existing = *leaf;
            let (first, second) = match new_side {
                ChildSide::First => (new_leaf, existing),
                ChildSide::Second => (existing, new_leaf),
            };
            *node = Node::Split {
                id,
                orientation,
                first_weight: SPLIT_WEIGHT_TOTAL / 2,
                first: Box::new(Node::Leaf(first)),
                second: Box::new(Node::Leaf(second)),
            };
            true
        }
        Node::Leaf(_) => false,
        Node::Split { first, second, .. } => {
            split_leaf(first, target, id, orientation, new_leaf, new_side)
                || split_leaf(second, target, id, orientation, new_leaf, new_side)
        }
    }
}

/// Replaces the parent of the named leaf window with the remaining sibling.
///
/// Returns the first leaf window of that sibling, so the caller can move the
/// focus deterministically. Returns `None` when the subtree does not hold the
/// window, or when the window is the root.
fn close_leaf(node: &mut Node, target: WindowId) -> Option<WindowId> {
    let Node::Split { first, second, .. } = node else {
        return None;
    };
    let first_matches = matches!(first.as_ref(), Node::Leaf(leaf) if leaf.id == target);
    let second_matches = matches!(second.as_ref(), Node::Leaf(leaf) if leaf.id == target);
    if first_matches || second_matches {
        match std::mem::replace(node, Node::Leaf(Leaf::VOID)) {
            Node::Split { first, second, .. } => {
                *node = if first_matches { *second } else { *first };
            }
            leaf @ Node::Leaf(_) => {
                debug_assert!(false, "the guard above matched a split node");
                *node = leaf;
                return None;
            }
        }
        return Some(node.first_leaf_id());
    }
    close_leaf(first, target).or_else(|| close_leaf(second, target))
}

/// Writes the share of the first child of the named split node.
///
/// Returns `false` when the subtree does not hold the split node.
fn set_split_weight(node: &mut Node, id: SplitId, weight: u32) -> bool {
    match node {
        Node::Leaf(_) => false,
        Node::Split {
            id: node_id,
            first_weight,
            first,
            second,
            ..
        } => {
            if *node_id == id {
                *first_weight = weight.min(SPLIT_WEIGHT_TOTAL);
                return true;
            }
            set_split_weight(first, id, weight) || set_split_weight(second, id, weight)
        }
    }
}

/// The region that holds the input focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    /// The focused editor window holds the focus.
    Editor,
    /// The named sidebar holds the focus.
    Sidebar(SidebarSide),
}

/// The window tree, the sidebars, the focus, and the current layout.
///
/// Every mutating operation recomputes the layout, so [`Windows::layout`]
/// always describes the current tree.
///
/// # Examples
///
/// ```
/// use ratatui::layout::Rect;
///
/// use kvim::settings::WindowSettings;
/// use kvim::tui::{BufferId, Direction, LayoutChange, Orientation, Windows};
///
/// let terminal = Rect::new(0, 0, 120, 40);
/// let mut windows = Windows::new(BufferId::new(1), terminal, WindowSettings::default());
///
/// // A vertical split opens the new window to the right and focuses it.
/// let right = windows.split(Orientation::Vertical).expect("the terminal is wide");
/// assert_eq!(windows.focused_window(), right);
/// assert_eq!(windows.window_count(), 2);
///
/// // Directional focus uses the rectangles, not the tree order.
/// assert_eq!(windows.focus_direction(Direction::Left), LayoutChange::Changed);
/// assert_eq!(windows.focus_direction(Direction::Left), LayoutChange::Unchanged);
/// ```
#[derive(Clone, Debug)]
pub struct Windows {
    root: Node,
    /// The focused editor window. The value stays valid while a sidebar holds
    /// the focus, so hiding that sidebar restores the previous editor window.
    focused: WindowId,
    focus: Focus,
    left_sidebar: Option<Sidebar>,
    right_sidebar: Option<Sidebar>,
    next_id: u32,
    next_split_id: u32,
    terminal: Rect,
    settings: WindowSettings,
    layout: WindowLayout,
}

impl Windows {
    /// Creates a tree with one window that shows the named buffer.
    #[must_use]
    pub fn new(buffer: BufferId, terminal: Rect, settings: WindowSettings) -> Self {
        let id = WindowId(1);
        let mut windows = Self {
            root: Node::Leaf(Leaf {
                id,
                buffer,
                viewport: Viewport::new(NonZeroU16::MIN, NonZeroU16::MIN),
            }),
            focused: id,
            focus: Focus::Editor,
            left_sidebar: None,
            right_sidebar: None,
            next_id: 2,
            next_split_id: 1,
            terminal,
            settings,
            layout: WindowLayout::empty(),
        };
        windows.recompute();
        windows
    }

    /// Returns the rectangle of every visible window and sidebar.
    #[must_use]
    pub const fn layout(&self) -> &WindowLayout {
        &self.layout
    }

    /// Returns the terminal rectangle that produced the current layout.
    #[must_use]
    pub const fn terminal(&self) -> Rect {
        self.terminal
    }

    /// Returns the split, focus, and resize settings of the tree.
    #[must_use]
    pub const fn settings(&self) -> WindowSettings {
        self.settings
    }

    /// Returns the focused editor window.
    ///
    /// The value stays valid while a sidebar holds the focus.
    #[must_use]
    pub const fn focused_window(&self) -> WindowId {
        self.focused
    }

    /// Returns the region that holds the input focus.
    #[must_use]
    pub fn focused_region(&self) -> WindowId {
        match self.focus {
            Focus::Editor => self.focused,
            Focus::Sidebar(side) => self.sidebar(side).map_or(self.focused, Sidebar::id),
        }
    }

    /// Returns the number of leaf windows in the tree.
    #[must_use]
    pub fn window_count(&self) -> usize {
        self.root.window_count()
    }

    /// Returns every window in tree order.
    ///
    /// The list holds every window identity, including a window that the
    /// current layout hides.
    #[must_use]
    pub fn window_ids(&self) -> Vec<WindowId> {
        let mut ids = Vec::new();
        self.root.collect_ids(&mut ids);
        ids
    }

    /// Returns the buffer that the named window shows.
    #[must_use]
    pub fn buffer(&self, id: WindowId) -> Option<BufferId> {
        self.root.leaf(id).map(|leaf| leaf.buffer)
    }

    /// Points the named window at another buffer.
    ///
    /// Returns `false` when the tree does not hold the window.
    pub fn set_buffer(&mut self, id: WindowId, buffer: BufferId) -> bool {
        match self.root.leaf_mut(id) {
            Some(leaf) => {
                leaf.buffer = buffer;
                true
            }
            None => false,
        }
    }

    /// Returns the viewport of the named window.
    #[must_use]
    pub fn viewport(&self, id: WindowId) -> Option<Viewport> {
        self.root.leaf(id).map(|leaf| leaf.viewport)
    }

    /// Returns the viewport of the named window for reconciliation.
    ///
    /// A layout change keeps both scroll offsets and only replaces the window
    /// size. The caller holds the buffer and the cursor, so the caller
    /// reconciles the viewport with the scroll margin after that change.
    pub fn viewport_mut(&mut self, id: WindowId) -> Option<&mut Viewport> {
        self.root.leaf_mut(id).map(|leaf| &mut leaf.viewport)
    }

    /// Recomputes the layout for a new terminal size.
    ///
    /// The tree structure and every window identity stay unchanged.
    pub fn set_terminal(&mut self, terminal: Rect) {
        self.terminal = terminal;
        self.recompute();
    }

    /// Returns the sidebar at the named edge.
    #[must_use]
    pub const fn sidebar(&self, side: SidebarSide) -> Option<Sidebar> {
        match side {
            SidebarSide::Left => self.left_sidebar,
            SidebarSide::Right => self.right_sidebar,
        }
    }

    /// Creates or replaces the sidebar at the named edge.
    ///
    /// The width is clamped to [`SIDEBAR_WIDTH_MIN_CELLS`] and
    /// [`SIDEBAR_WIDTH_MAX_CELLS`]. The new sidebar is visible.
    pub fn open_sidebar(&mut self, side: SidebarSide, width_cells: u16) -> WindowId {
        let id = self.take_window_id();
        let sidebar = Sidebar {
            id,
            side,
            width_cells: width_cells.clamp(SIDEBAR_WIDTH_MIN_CELLS, SIDEBAR_WIDTH_MAX_CELLS),
            visible: true,
        };
        *self.sidebar_slot(side) = Some(sidebar);
        self.recompute();
        id
    }

    /// Shows or hides the sidebar at the named edge.
    ///
    /// Hiding a sidebar that holds the focus returns the focus to the
    /// previously focused editor window.
    pub fn set_sidebar_visible(&mut self, side: SidebarSide, visible: bool) -> LayoutChange {
        let Some(sidebar) = self.sidebar_slot(side) else {
            return LayoutChange::Unchanged;
        };
        if sidebar.visible == visible {
            return LayoutChange::Unchanged;
        }
        sidebar.visible = visible;
        self.recompute();
        LayoutChange::Changed
    }

    /// Moves the focus to the named region.
    ///
    /// Returns [`LayoutChange::Unchanged`] when the layout does not show the
    /// region, so a hidden sidebar never holds the focus.
    pub fn focus_region(&mut self, id: WindowId) -> LayoutChange {
        let Some(region) = self.layout.region(id) else {
            return LayoutChange::Unchanged;
        };
        let focus = match region.kind {
            RegionKind::Editor => Focus::Editor,
            RegionKind::Sidebar(side) => Focus::Sidebar(side),
        };
        let focused = match region.kind {
            RegionKind::Editor => id,
            RegionKind::Sidebar(_) => self.focused,
        };
        if self.focus == focus && self.focused == focused {
            return LayoutChange::Unchanged;
        }
        self.focus = focus;
        self.focused = focused;
        LayoutChange::Changed
    }

    /// Moves the focus to the nearest region on the named side.
    ///
    /// The move compares layout rectangles, not tree order. The focus stays
    /// unchanged when no region touches that side.
    pub fn focus_direction(&mut self, direction: Direction) -> LayoutChange {
        match self.layout.neighbor(self.focused_region(), direction) {
            Some(id) => self.focus_region(id),
            None => LayoutChange::Unchanged,
        }
    }

    /// Splits the focused window and focuses the new window.
    ///
    /// The new window shows the same buffer. The settings decide which side
    /// receives it.
    ///
    /// # Errors
    ///
    /// Returns [`SplitError`] when the tree already holds [`WINDOWS_MAX`]
    /// windows, or when the focused window already sits at
    /// [`SPLIT_DEPTH_MAX`] split levels.
    pub fn split(&mut self, orientation: Orientation) -> Result<WindowId, SplitError> {
        if self.window_count() >= WINDOWS_MAX {
            return Err(SplitError::WindowLimit);
        }
        let mut path = Vec::new();
        self.root.leaf_path(self.focused, &mut path);
        if path.len() >= SPLIT_DEPTH_MAX {
            return Err(SplitError::DepthLimit);
        }

        let Some(source) = self.root.leaf(self.focused).copied() else {
            debug_assert!(false, "the focused window is always a leaf of the tree");
            unreachable!("the focused window is always a leaf of the tree");
        };
        let id = self.take_window_id();
        let split = SplitId(self.next_split_id);
        self.next_split_id = self.next_split_id.wrapping_add(1);
        let new_leaf = Leaf { id, ..source };
        let new_side = match orientation {
            Orientation::Horizontal => match self.settings.horizontal_split_placement {
                HorizontalSplitPlacement::Above => ChildSide::First,
                HorizontalSplitPlacement::Below => ChildSide::Second,
            },
            Orientation::Vertical => match self.settings.vertical_split_placement {
                VerticalSplitPlacement::Left => ChildSide::First,
                VerticalSplitPlacement::Right => ChildSide::Second,
            },
        };
        let replaced = split_leaf(
            &mut self.root,
            self.focused,
            split,
            orientation,
            new_leaf,
            new_side,
        );
        debug_assert!(replaced, "the focused window is always a leaf of the tree");
        self.focused = id;
        self.focus = Focus::Editor;
        self.recompute();
        Ok(id)
    }

    /// Returns the orientation that the adaptive split command selects.
    ///
    /// One rule comes before the ratio: a terminal that holds exactly one
    /// editor window always selects a vertical split, because a full-width
    /// terminal would otherwise divide into two short windows. The inverse
    /// command mirrors both the exception and the ratio.
    #[must_use]
    pub fn adaptive_orientation(&self, sense: AdaptiveSplit) -> Orientation {
        let ratio = self.settings.adaptive_split_ratio.get();
        let normal = if self.window_count() == 1 {
            Orientation::Vertical
        } else {
            let area = self.layout.area(self.focused).unwrap_or(self.terminal);
            if f32::from(area.width) > f32::from(area.height) * ratio {
                Orientation::Vertical
            } else {
                Orientation::Horizontal
            }
        };
        match sense {
            AdaptiveSplit::Normal => normal,
            AdaptiveSplit::Inverse => normal.inverse(),
        }
    }

    /// Splits the focused window with the adaptive rule.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Windows::split`].
    pub fn split_adaptive(&mut self, sense: AdaptiveSplit) -> Result<WindowId, SplitError> {
        self.split(self.adaptive_orientation(sense))
    }

    /// Closes the focused region.
    ///
    /// Closing a focused sidebar hides it and returns the focus to the
    /// previously focused editor window. Closing a window replaces its parent
    /// split node with the remaining sibling and focuses the first window of
    /// that sibling.
    pub fn close_focused(&mut self) -> CloseOutcome {
        if let Focus::Sidebar(side) = self.focus {
            let id = self.sidebar(side).map(Sidebar::id);
            self.set_sidebar_visible(side, false);
            if let Some(id) = id {
                return CloseOutcome::Closed(id);
            }
        }
        let closed = self.focused;
        let Some(next) = close_leaf(&mut self.root, closed) else {
            return CloseOutcome::LastWindow;
        };
        self.focused = next;
        self.focus = Focus::Editor;
        self.recompute();
        CloseOutcome::Closed(closed)
    }

    /// Moves one shared edge by the configured resize step.
    ///
    /// The command names the direction that the edge moves, not a size change.
    /// The far edge wins: a horizontal command prefers the right edge, and a
    /// vertical command prefers the bottom edge. The focused window therefore
    /// grows or shrinks according to which side holds a neighbor. A near
    /// neighbor moves the near edge only while the far side holds none. No
    /// neighbor on either side leaves the layout unchanged.
    ///
    /// A resize that would push any window below its minimum dimensions, or
    /// that would hide a window, leaves the layout unchanged. A resize whose
    /// neighbor is a sidebar changes the sidebar width.
    pub fn resize(&mut self, direction: Direction) -> LayoutChange {
        let focused = self.focused_region();
        if let Some(RegionKind::Sidebar(side)) = self.layout.region(focused).map(|r| r.kind) {
            return self.resize_sidebar(side, direction);
        }
        let far = direction.far_edge();
        let (neighbor, edge) = match self.layout.neighbor(focused, far) {
            Some(neighbor) => (neighbor, far),
            None => match self.layout.neighbor(focused, far.opposite()) {
                Some(neighbor) => (neighbor, far.opposite()),
                None => return LayoutChange::Unchanged,
            },
        };
        match self.layout.region(neighbor).map(|region| region.kind) {
            Some(RegionKind::Sidebar(side)) => self.resize_sidebar(side, direction),
            Some(RegionKind::Editor) => self.resize_divider(direction, edge),
            None => LayoutChange::Unchanged,
        }
    }

    /// Applies one semantic window command.
    ///
    /// The method ignores every command that does not address the window tree,
    /// so the event loop passes each command through one call.
    pub fn apply(&mut self, command: Command) -> WindowOutcome {
        match command {
            Command::FocusWindowLeft => self.focus_direction(Direction::Left).into(),
            Command::FocusWindowDown => self.focus_direction(Direction::Down).into(),
            Command::FocusWindowUp => self.focus_direction(Direction::Up).into(),
            Command::FocusWindowRight => self.focus_direction(Direction::Right).into(),
            Command::ResizeWindowLeft => self.resize(Direction::Left).into(),
            Command::ResizeWindowDown => self.resize(Direction::Down).into(),
            Command::ResizeWindowUp => self.resize(Direction::Up).into(),
            Command::ResizeWindowRight => self.resize(Direction::Right).into(),
            Command::SplitAdaptive => self.split_outcome(AdaptiveSplit::Normal),
            Command::SplitInverseAdaptive => self.split_outcome(AdaptiveSplit::Inverse),
            Command::CloseWindow => match self.close_focused() {
                CloseOutcome::Closed(_) => WindowOutcome::Changed,
                CloseOutcome::LastWindow => WindowOutcome::LastWindow,
            },
            _ => WindowOutcome::Ignored,
        }
    }

    fn split_outcome(&mut self, sense: AdaptiveSplit) -> WindowOutcome {
        match self.split_adaptive(sense) {
            Ok(_) => WindowOutcome::Changed,
            Err(_) => WindowOutcome::Unchanged,
        }
    }

    /// Moves the divider that the focused window shares with an editor window.
    fn resize_divider(&mut self, direction: Direction, edge: Direction) -> LayoutChange {
        let orientation = direction.orientation();
        let side = edge.divider_side();
        let mut path = Vec::new();
        self.root.leaf_path(self.focused, &mut path);
        let Some(step) = path
            .iter()
            .rposition(|step| step.orientation == orientation && step.side == side)
            .map(|index| path[index])
        else {
            return LayoutChange::Unchanged;
        };
        let Some(area) = self.layout.split_area(step.split) else {
            return LayoutChange::Unchanged;
        };
        let (extent, minimum) = self.axis(orientation, area);
        let current = super::layout::first_extent(extent, self.current_weight(step.split), minimum);
        let step_cells = i32::from(self.settings.resize_step_cells);
        let target = i32::from(current) + direction.divider_step() * step_cells;
        let target = target.clamp(0, i32::from(extent));
        let numerator = u32::try_from(target).unwrap_or(0) * SPLIT_WEIGHT_TOTAL;
        let weight = (numerator + u32::from(extent) / 2) / u32::from(extent);

        let mut candidate = self.root.clone();
        if !set_split_weight(&mut candidate, step.split, weight) {
            return LayoutChange::Unchanged;
        }
        self.commit(candidate)
    }

    /// Moves the inner edge of one sidebar in the named direction.
    fn resize_sidebar(&mut self, side: SidebarSide, direction: Direction) -> LayoutChange {
        let grows = match (side, direction) {
            (SidebarSide::Left, Direction::Right) | (SidebarSide::Right, Direction::Left) => true,
            (SidebarSide::Left, Direction::Left) | (SidebarSide::Right, Direction::Right) => false,
            (_, Direction::Up | Direction::Down) => return LayoutChange::Unchanged,
        };
        let step = i32::from(self.settings.resize_step_cells);
        let Some(sidebar) = self.sidebar(side) else {
            return LayoutChange::Unchanged;
        };
        let target = i32::from(sidebar.width_cells) + if grows { step } else { -step };
        let width = u16::try_from(target.max(0))
            .unwrap_or(SIDEBAR_WIDTH_MAX_CELLS)
            .clamp(SIDEBAR_WIDTH_MIN_CELLS, SIDEBAR_WIDTH_MAX_CELLS);
        if width == sidebar.width_cells {
            return LayoutChange::Unchanged;
        }

        let staged = Sidebar {
            width_cells: width,
            ..sidebar
        };
        let (left, right) = match side {
            SidebarSide::Left => (Some(staged), self.right_sidebar),
            SidebarSide::Right => (self.left_sidebar, Some(staged)),
        };
        let candidate = compute_layout(
            &self.root,
            self.focused,
            left,
            right,
            self.terminal,
            &self.settings,
        );
        if !self.accepts(&candidate) {
            return LayoutChange::Unchanged;
        }
        *self.sidebar_slot(side) = Some(staged);
        self.layout = candidate;
        self.sync_viewports();
        LayoutChange::Changed
    }

    /// Publishes a staged tree only when its layout keeps every minimum.
    fn commit(&mut self, candidate: Node) -> LayoutChange {
        let layout = compute_layout(
            &candidate,
            self.focused,
            self.left_sidebar,
            self.right_sidebar,
            self.terminal,
            &self.settings,
        );
        if !self.accepts(&layout) {
            return LayoutChange::Unchanged;
        }
        self.root = candidate;
        self.layout = layout;
        self.sync_viewports();
        LayoutChange::Changed
    }

    /// Reports whether a staged layout keeps every region usable and visible.
    ///
    /// A resize never hides a window or a sidebar, and never pushes a window
    /// below its minimum dimensions. A staged layout that changes nothing is
    /// also refused, so the caller reports one unchanged result.
    fn accepts(&self, candidate: &WindowLayout) -> bool {
        if candidate.regions() == self.layout.regions() {
            return false;
        }
        if candidate.regions().len() != self.layout.regions().len() {
            return false;
        }
        candidate.regions().iter().all(|region| {
            region.kind != RegionKind::Editor
                || (region.area.width >= self.settings.min_window_width_cells
                    && region.area.height >= self.settings.min_window_height_rows)
        })
    }

    fn current_weight(&self, id: SplitId) -> u32 {
        fn find(node: &Node, id: SplitId) -> Option<u32> {
            match node {
                Node::Leaf(_) => None,
                Node::Split {
                    id: node_id,
                    first_weight,
                    first,
                    second,
                    ..
                } => {
                    if *node_id == id {
                        return Some(*first_weight);
                    }
                    find(first, id).or_else(|| find(second, id))
                }
            }
        }
        find(&self.root, id).unwrap_or(SPLIT_WEIGHT_TOTAL / 2)
    }

    fn axis(&self, orientation: Orientation, area: Rect) -> (u16, u16) {
        match orientation {
            Orientation::Vertical => (area.width, self.settings.min_window_width_cells.max(1)),
            Orientation::Horizontal => (area.height, self.settings.min_window_height_rows.max(1)),
        }
    }

    fn sidebar_slot(&mut self, side: SidebarSide) -> &mut Option<Sidebar> {
        match side {
            SidebarSide::Left => &mut self.left_sidebar,
            SidebarSide::Right => &mut self.right_sidebar,
        }
    }

    fn take_window_id(&mut self) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    /// Recomputes the layout and restores the focus and viewport invariants.
    fn recompute(&mut self) {
        self.layout = compute_layout(
            &self.root,
            self.focused,
            self.left_sidebar,
            self.right_sidebar,
            self.terminal,
            &self.settings,
        );
        if let Focus::Sidebar(side) = self.focus {
            let shown = self
                .sidebar(side)
                .is_some_and(|sidebar| self.layout.region(sidebar.id).is_some());
            if !shown {
                self.focus = Focus::Editor;
            }
        }
        debug_assert!(
            self.terminal.is_empty() || self.layout.area(self.focused).is_some(),
            "the layout keeps the focused window visible in every non-empty terminal"
        );
        self.sync_viewports();
    }

    /// Resizes every visible viewport to its window rectangle.
    ///
    /// A size change keeps both scroll offsets, so a split, a close, and a
    /// terminal resize never move the reader back to the start of the buffer.
    /// The caller holds the buffer and the cursor, so the caller reconciles the
    /// viewport with the scroll margin after the change.
    fn sync_viewports(&mut self) {
        let sizes: Vec<(WindowId, u16, u16)> = self
            .layout
            .regions()
            .iter()
            .filter(|region| region.kind == RegionKind::Editor)
            .map(|region| (region.id, region.area.width, region.area.height))
            .collect();
        for (id, width, height) in sizes {
            let (Some(width), Some(height)) = (NonZeroU16::new(width), NonZeroU16::new(height))
            else {
                continue;
            };
            let Some(leaf) = self.root.leaf_mut(id) else {
                continue;
            };
            if leaf.viewport.height_rows() != height || leaf.viewport.width_cells() != width {
                leaf.viewport = leaf.viewport.resized(height, width);
            }
        }
    }
}

impl From<LayoutChange> for WindowOutcome {
    fn from(change: LayoutChange) -> Self {
        match change {
            LayoutChange::Changed => Self::Changed,
            LayoutChange::Unchanged => Self::Unchanged,
        }
    }
}
