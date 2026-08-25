//! The generic window tree: topology, focus, splits, resize, and sidebars.
//!
//! The module is deterministic and pure. It reads no clock, no filesystem, and
//! no terminal. It holds one opaque surface identity for each leaf, the split
//! structure, the sidebar widths, and the host area. The host owns every
//! surface value.
//!
//! [`WindowTree`] owns the tree and one cached [`WindowLayout`]. Every
//! operation recomputes that layout, so the tree and the rectangles never
//! disagree. No other code computes a window rectangle.
//!
//! `examples/split_windows.rs` splits one host area between caller-owned
//! surfaces and prints the resulting layout.

use std::num::NonZeroU16;

use ratatui::layout::Rect;
use thiserror::Error;

use crate::layout::{RegionKind, WindowLayout, compute_layout, editor_area, first_extent};

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
///
/// The value is larger than the largest extent that a host rectangle holds,
/// which is [`u16::MAX`] cells. One weight therefore reproduces one absolute
/// cell count exactly, and a resize that works in cells loses nothing when it
/// stores its result.
pub const SPLIT_WEIGHT_TOTAL: u32 = 65_536;

/// The smallest width that a sidebar accepts, in cells.
pub const SIDEBAR_WIDTH_MIN_CELLS: u16 = 10;

/// The largest width that a sidebar accepts, in cells.
pub const SIDEBAR_WIDTH_MAX_CELLS: u16 = 200;

/// The stable identity of one window or one sidebar.
///
/// The identity stays stable while the region exists. A split, a close, and a
/// host resize never change an existing identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId(u32);

impl WindowId {
    /// Returns the identity value.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_ui::{WindowLimits, WindowTree};
    ///
    /// let tree = WindowTree::new("editor", Rect::new(0, 0, 80, 24), WindowLimits::default());
    /// assert!(tree.focused_window().get() > 0);
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_ui::Orientation;
    ///
    /// assert_eq!(Orientation::Vertical.inverse(), Orientation::Horizontal);
    /// ```
    #[must_use]
    pub const fn inverse(self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }
}

/// The rule that the adaptive split command applies.
///
/// [`WindowTree::adaptive_orientation`] selects the orientation. Every host
/// that binds an adaptive split key reaches the same rule through this type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptiveSplit {
    /// Select a vertical split for a wide window.
    Normal,
    /// Select a horizontal split for a wide window.
    Inverse,
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
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_ui::Direction;
    ///
    /// assert_eq!(Direction::Left.opposite(), Direction::Right);
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_ui::{Direction, Orientation};
    ///
    /// assert_eq!(Direction::Left.orientation(), Orientation::Vertical);
    /// ```
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

/// The child of one split node.
///
/// The first child is the left child of a vertical split and the top child of a
/// horizontal split. A caller names the child that receives a new window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildSide {
    /// The left child of a vertical split, or the top child of a horizontal split.
    First,
    /// The right child of a vertical split, or the bottom child of a horizontal split.
    Second,
}

/// The edge of the host area that holds one sidebar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SidebarSide {
    /// The left edge of the host area.
    Left,
    /// The right edge of the host area.
    Right,
}

/// A fixed-width region at one edge of the host area.
///
/// A sidebar has no place in the window tree, keeps a fixed width instead of a
/// ratio, and never takes part in a split.
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
    /// A visible sidebar still stays out of the layout while the host area is
    /// too narrow to hold it beside the minimum window width.
    #[must_use]
    pub const fn is_visible(self) -> bool {
        self.visible
    }
}

/// The smallest usable dimensions of one window.
///
/// Both values are non-zero, so every layout keeps one usable cell for each
/// child of a split node.
///
/// # Examples
///
/// ```
/// use std::num::NonZeroU16;
///
/// use kvim_ui::WindowLimits;
///
/// let limits = WindowLimits::new(
///     NonZeroU16::new(20).expect("the literal 20 is not zero"),
///     NonZeroU16::new(3).expect("the literal 3 is not zero"),
/// );
/// assert_eq!(limits.min_width_cells(), 20);
/// assert_eq!(limits, WindowLimits::default());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowLimits {
    min_width_cells: NonZeroU16,
    min_height_rows: NonZeroU16,
}

impl WindowLimits {
    /// Creates the minimum dimensions of one window.
    #[must_use]
    pub const fn new(min_width_cells: NonZeroU16, min_height_rows: NonZeroU16) -> Self {
        Self {
            min_width_cells,
            min_height_rows,
        }
    }

    /// Returns the smallest usable window width, in cells.
    #[must_use]
    pub const fn min_width_cells(self) -> u16 {
        self.min_width_cells.get()
    }

    /// Returns the smallest usable window height, in rows.
    #[must_use]
    pub const fn min_height_rows(self) -> u16 {
        self.min_height_rows.get()
    }

    /// Returns the minimum that one axis enforces.
    pub(crate) const fn axis_minimum(self, orientation: Orientation) -> u16 {
        match orientation {
            Orientation::Vertical => self.min_width_cells.get(),
            Orientation::Horizontal => self.min_height_rows.get(),
        }
    }
}

impl Default for WindowLimits {
    /// Returns 20 cells of width and 3 rows of height.
    ///
    /// The width keeps a line number column, a sign column, and readable text
    /// visible. The height keeps a header row and readable text visible.
    fn default() -> Self {
        Self {
            min_width_cells: NonZeroU16::new(20).expect("the literal 20 is not zero"),
            min_height_rows: NonZeroU16::new(3).expect("the literal 3 is not zero"),
        }
    }
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
    /// The tree holds one window, so the caller decides what happens next.
    LastWindow,
}

/// How much of the tree the current host area shows.
///
/// The layout never hides a surface silently. A host area that cannot hold
/// every window reports the number of hidden regions here, and the layout keeps
/// the focused window visible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutFit {
    /// Every window and every requested sidebar has a rectangle.
    Complete,
    /// The host area is too small, so the layout hides the named counts.
    Constrained {
        /// The number of leaf windows without a rectangle.
        hidden_windows: usize,
        /// The number of requested sidebars without a rectangle.
        hidden_sidebars: usize,
    },
}

/// The reason that the tree cannot issue a new region identity.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum IdentityError {
    /// The identity counter reached its largest value.
    #[error("the window tree issued every available region identity")]
    Exhausted,
    /// The next identity is already in use, so issuing it would reuse it.
    #[error("the window tree already holds the region identity {0:?}")]
    Duplicate(WindowId),
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
    /// The focused rectangle cannot show both subtrees of the new split node.
    #[error("the focused area holds {available} cells and both subtrees need {required}")]
    AreaTooSmall {
        /// The extent of the focused rectangle along the split axis.
        available: u16,
        /// The extent that both subtrees need along the split axis.
        required: u16,
    },
    /// The tree cannot issue an identity for the new window.
    #[error("the window tree cannot issue an identity for the new window")]
    Identity(#[source] IdentityError),
}

impl From<IdentityError> for SplitError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error)
    }
}

/// The reason that a command cannot address one region.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RegionError {
    /// Neither the tree nor a sidebar holds the identity.
    #[error("the window tree holds no region with the identity {0:?}")]
    Unknown(WindowId),
    /// The region exists, but the current layout shows no rectangle for it.
    #[error("the current layout does not show the region {0:?}")]
    Hidden(WindowId),
}

/// The identity of one split node.
///
/// The layout calculation reports the rectangle of every materialized split
/// node under this identity, so a resize command finds the divider without a
/// second layout rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SplitId(u32);

/// One window that shows one host surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Leaf<S> {
    pub(crate) id: WindowId,
    surface: S,
}

/// One node of the window tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Node<S> {
    /// One window that shows one host surface.
    Leaf(Leaf<S>),
    /// Two children with one shared divider.
    Split {
        id: SplitId,
        orientation: Orientation,
        /// The share of the split extent that the first child requests.
        first_weight: u32,
        first: Box<Node<S>>,
        second: Box<Node<S>>,
    },
}

impl<S> Node<S> {
    /// Returns the number of leaf windows in the subtree.
    pub(crate) fn window_count(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Split { first, second, .. } => first.window_count() + second.window_count(),
        }
    }

    /// Reports whether the subtree holds the named window.
    pub(crate) fn contains(&self, id: WindowId) -> bool {
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

    fn leaf(&self, id: WindowId) -> Option<&Leaf<S>> {
        match self {
            Self::Leaf(leaf) => (leaf.id == id).then_some(leaf),
            Self::Split { first, second, .. } => first.leaf(id).or_else(|| second.leaf(id)),
        }
    }

    fn leaf_mut(&mut self, id: WindowId) -> Option<&mut Leaf<S>> {
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
fn split_leaf<S: Clone>(
    node: &mut Node<S>,
    target: WindowId,
    id: SplitId,
    orientation: Orientation,
    new_leaf: Leaf<S>,
    new_side: ChildSide,
) -> bool {
    match node {
        Node::Leaf(leaf) if leaf.id == target => {
            let existing = leaf.clone();
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
            split_leaf(first, target, id, orientation, new_leaf.clone(), new_side)
                || split_leaf(second, target, id, orientation, new_leaf, new_side)
        }
    }
}

/// Replaces the parent of the named leaf window with the remaining sibling.
///
/// Returns the first leaf window of that sibling, so the caller can move the
/// focus deterministically. Returns `None` when the subtree does not hold the
/// window, or when the window is the root.
fn close_leaf<S: Clone>(node: &mut Node<S>, target: WindowId) -> Option<WindowId> {
    let Node::Split { first, second, .. } = node else {
        return None;
    };
    let first_matches = matches!(first.as_ref(), Node::Leaf(leaf) if leaf.id == target);
    let second_matches = matches!(second.as_ref(), Node::Leaf(leaf) if leaf.id == target);
    if first_matches || second_matches {
        // The surviving child replaces its parent. The clone keeps the function
        // total for a surface type that owns its value, and the leaf bound
        // keeps the copied subtree small.
        let survivor = if first_matches {
            second.as_ref().clone()
        } else {
            first.as_ref().clone()
        };
        *node = survivor;
        return Some(node.first_leaf_id());
    }
    close_leaf(first, target).or_else(|| close_leaf(second, target))
}

/// Returns the smallest extent that one subtree occupies along one axis.
///
/// One leaf window needs the minimum window dimension. A split along the axis
/// needs both children, and a split across the axis needs the larger child.
/// [`SPLIT_DEPTH_MAX`] bounds the recursion, and the sum saturates, so a tree
/// in a small host area produces no overflow.
pub(crate) fn min_extent<S>(node: &Node<S>, orientation: Orientation, minimum: u16) -> u16 {
    match node {
        Node::Leaf(_) => minimum,
        Node::Split {
            orientation: axis,
            first,
            second,
            ..
        } => {
            let first = min_extent(first, orientation, minimum);
            let second = min_extent(second, orientation, minimum);
            if *axis == orientation {
                first.saturating_add(second)
            } else {
                first.max(second)
            }
        }
    }
}

/// Returns the smallest extent of each child of one split node.
///
/// A nested subtree needs more than one leaf minimum, so both children report
/// their own recursive minimum. The layout and the resize rules divide the
/// extent by these two values, never by one shared leaf minimum.
pub(crate) fn child_minima<S>(
    first: &Node<S>,
    second: &Node<S>,
    orientation: Orientation,
    minimum: u16,
) -> (u16, u16) {
    (
        min_extent(first, orientation, minimum),
        min_extent(second, orientation, minimum),
    )
}

/// Returns the weight that reproduces one absolute first-child extent.
///
/// The layout calculation multiplies the extent by the weight and divides by
/// [`SPLIT_WEIGHT_TOTAL`]. That denominator is above every host extent, so the
/// smallest weight that reaches `head` reproduces exactly `head` cells.
fn split_weight(head: u16, extent: u16) -> u32 {
    debug_assert!(
        head <= extent,
        "one child of a split node never passes the extent that the node divides"
    );
    if extent == 0 {
        return SPLIT_WEIGHT_TOTAL / 2;
    }
    let extent = u64::from(extent);
    let numerator = u64::from(head) * u64::from(SPLIT_WEIGHT_TOTAL) + extent - 1;
    u32::try_from(numerator / extent)
        .map_or(SPLIT_WEIGHT_TOTAL, |weight| weight.min(SPLIT_WEIGHT_TOTAL))
}

/// Moves one end of a subtree by `delta` cells and rewrites its weights.
///
/// `extent` is the current extent of the subtree along `orientation`, and `end`
/// names the end that moves. The pane at that end absorbs the cells first. A
/// pane that would fall below its own recursive minimum keeps that minimum and
/// passes the remaining cells to the next pane along the same direction. Every
/// other pane keeps its exact extent.
///
/// Returns the new extent of the subtree, or `None` when no arrangement of the
/// subtree keeps every window at its minimum. A subtree that returns `None`
/// holds no usable state, so the caller discards the staged tree.
fn move_edge<S>(
    node: &mut Node<S>,
    orientation: Orientation,
    extent: u16,
    end: ChildSide,
    delta: i32,
    minimum: u16,
) -> Option<u16> {
    let floor = min_extent(node, orientation, minimum);
    // A constrained layout can hide part of the tree, so the current extent can
    // already sit below the minimum. No move starts from that state.
    if extent < floor {
        return None;
    }
    let requested = i32::from(extent) + delta;
    if requested < i32::from(floor) {
        return None;
    }
    let next = u16::try_from(requested).ok()?;
    match node {
        Node::Leaf(_) => {}
        Node::Split {
            orientation: axis,
            first_weight,
            first,
            second,
            ..
        } if *axis == orientation => {
            let (first_min, second_min) = child_minima(first, second, orientation, minimum);
            let head = first_extent(extent, *first_weight, first_min, second_min);
            let (near, near_extent, far, far_extent) = match end {
                ChildSide::First => (first, head, second, extent - head),
                ChildSide::Second => (second, extent - head, first, head),
            };
            // The pane at the moved end gives or takes the cells first. It stops
            // at its own minimum, and the rest moves the divider between the two
            // children, so the next pane along absorbs it.
            let near_floor = i32::from(min_extent(near, orientation, minimum));
            let near_delta =
                (i32::from(near_extent) + delta).max(near_floor) - i32::from(near_extent);
            let near_next = move_edge(near, orientation, near_extent, end, near_delta, minimum)?;
            let far_next = move_edge(
                far,
                orientation,
                far_extent,
                end,
                delta - near_delta,
                minimum,
            )?;
            debug_assert_eq!(
                u32::from(near_next) + u32::from(far_next),
                u32::from(next),
                "the two children divide the new extent of the split node"
            );
            let head_next = match end {
                ChildSide::First => near_next,
                ChildSide::Second => far_next,
            };
            *first_weight = split_weight(head_next, next);
        }
        Node::Split { first, second, .. } => {
            // The axis crosses this divider, so both children hold the extent of
            // the node, and both move the same end by the same cells.
            move_edge(first, orientation, extent, end, delta, minimum)?;
            move_edge(second, orientation, extent, end, delta, minimum)?;
        }
    }
    Some(next)
}

/// Moves the divider of one split node of a staged tree by `delta` cells.
///
/// The divider moves toward larger coordinates for a positive `delta`. The
/// panes across the divider give up the cells, and every pane that shares no
/// border with the divider keeps its exact extent.
///
/// Returns `false` when the tree holds no such divider, or when no arrangement
/// keeps every window at its minimum. The staged tree then holds no usable
/// state, so the caller discards it.
fn move_divider<S>(
    node: &mut Node<S>,
    id: SplitId,
    orientation: Orientation,
    extent: u16,
    minimum: u16,
    delta: i32,
) -> bool {
    let Some(Node::Split {
        first_weight,
        first,
        second,
        ..
    }) = split_node_mut(node, id)
    else {
        return false;
    };
    let (first_min, second_min) = child_minima(first, second, orientation, minimum);
    if extent < first_min.saturating_add(second_min) {
        return false;
    }
    let head = first_extent(extent, *first_weight, first_min, second_min);
    let tail = extent - head;
    let Some(head_next) = u16::try_from(i32::from(head) + delta)
        .ok()
        .filter(|head_next| *head_next <= extent)
    else {
        return false;
    };
    // The first child grows by the cells that the second child gives up, so one
    // divider moves and the extent of the split node stays as it is.
    if move_edge(first, orientation, head, ChildSide::Second, delta, minimum).is_none() {
        return false;
    }
    if move_edge(second, orientation, tail, ChildSide::First, -delta, minimum).is_none() {
        return false;
    }
    *first_weight = split_weight(head_next, extent);
    true
}

/// Returns the named split node of one subtree.
fn split_node_mut<S>(node: &mut Node<S>, id: SplitId) -> Option<&mut Node<S>> {
    if matches!(node, Node::Split { id: node_id, .. } if *node_id == id) {
        return Some(node);
    }
    let Node::Split { first, second, .. } = node else {
        return None;
    };
    match split_node_mut(first, id) {
        Some(found) => Some(found),
        None => split_node_mut(second, id),
    }
}

/// The region that holds the input focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    /// The focused window holds the focus.
    Window,
    /// The named sidebar holds the focus.
    Sidebar(SidebarSide),
}

/// The window tree, the sidebars, the focus, and the current layout.
///
/// Each leaf holds one opaque surface identity `S`. The tree never reads that
/// value: it copies the identity of the source window into a new window and
/// hands it back on request. The host owns every surface value.
///
/// Every mutating operation recomputes the layout, so [`WindowTree::layout`]
/// always describes the current tree.
///
/// `examples/split_windows.rs` shows one complete host that owns its surfaces.
///
/// # Examples
///
/// ```
/// use ratatui::layout::Rect;
///
/// use kvim_ui::{ChildSide, Direction, LayoutChange, Orientation, WindowLimits, WindowTree};
///
/// let area = Rect::new(0, 0, 120, 40);
/// let mut tree = WindowTree::new("chat", area, WindowLimits::default());
///
/// // The caller names the child that receives the new window.
/// let right = tree
///     .split(Orientation::Vertical, ChildSide::Second)
///     .expect("the area is wide");
/// assert_eq!(tree.focused_window(), right);
/// assert_eq!(tree.window_count(), 2);
/// assert_eq!(tree.surface(right), Some(&"chat"));
///
/// // Directional focus uses the rectangles, not the tree order.
/// assert_eq!(tree.focus_direction(Direction::Left), LayoutChange::Changed);
/// assert_eq!(tree.focus_direction(Direction::Left), LayoutChange::Unchanged);
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowTree<S> {
    root: Node<S>,
    /// The focused window. The value stays valid while a sidebar holds the
    /// focus, so hiding that sidebar restores the previous window.
    focused: WindowId,
    focus: Focus,
    left_sidebar: Option<Sidebar>,
    right_sidebar: Option<Sidebar>,
    next_id: u32,
    next_split_id: u32,
    area: Rect,
    limits: WindowLimits,
    layout: WindowLayout,
}

impl<S: Clone> WindowTree<S> {
    /// Creates a tree with one window that shows the named surface.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_ui::{WindowLimits, WindowTree};
    ///
    /// let tree = WindowTree::new(7_u32, Rect::new(0, 0, 80, 24), WindowLimits::default());
    /// assert_eq!(tree.window_count(), 1);
    /// assert_eq!(tree.surface(tree.focused_window()), Some(&7));
    /// ```
    #[must_use]
    pub fn new(surface: S, area: Rect, limits: WindowLimits) -> Self {
        let id = WindowId(1);
        let mut tree = Self {
            root: Node::Leaf(Leaf { id, surface }),
            focused: id,
            focus: Focus::Window,
            left_sidebar: None,
            right_sidebar: None,
            next_id: 2,
            next_split_id: 1,
            area,
            limits,
            layout: WindowLayout::empty(),
        };
        tree.recompute();
        tree
    }

    /// Returns the rectangle of every visible window and sidebar.
    #[must_use]
    pub const fn layout(&self) -> &WindowLayout {
        &self.layout
    }

    /// Returns the host rectangle that produced the current layout.
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
    }

    /// Returns the minimum dimensions that every layout enforces.
    #[must_use]
    pub const fn limits(&self) -> WindowLimits {
        self.limits
    }

    /// Returns the focused window.
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
            Focus::Window => self.focused,
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

    /// Returns the surface identity that the named window shows.
    #[must_use]
    pub fn surface(&self, id: WindowId) -> Option<&S> {
        self.root.leaf(id).map(|leaf| &leaf.surface)
    }

    /// Points the named window at another surface and returns the previous one.
    ///
    /// # Errors
    ///
    /// Returns [`RegionError::Unknown`] when the tree holds no such window. A
    /// hidden window still accepts a new surface, so this call never reports
    /// [`RegionError::Hidden`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_ui::{WindowLimits, WindowTree};
    ///
    /// let mut tree = WindowTree::new("draft", Rect::new(0, 0, 80, 24), WindowLimits::default());
    /// let window = tree.focused_window();
    /// assert_eq!(tree.replace_surface(window, "review"), Ok("draft"));
    /// assert_eq!(tree.surface(window), Some(&"review"));
    /// ```
    pub fn replace_surface(&mut self, id: WindowId, surface: S) -> Result<S, RegionError> {
        match self.root.leaf_mut(id) {
            Some(leaf) => Ok(std::mem::replace(&mut leaf.surface, surface)),
            None => Err(RegionError::Unknown(id)),
        }
    }

    /// Recomputes the layout for a new host rectangle.
    ///
    /// The tree structure and every window identity stay unchanged. A host area
    /// that cannot hold every window returns [`LayoutFit::Constrained`] and
    /// keeps the focused window visible.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_ui::{ChildSide, LayoutFit, Orientation, WindowLimits, WindowTree};
    ///
    /// let mut tree = WindowTree::new(0_u32, Rect::new(0, 0, 120, 40), WindowLimits::default());
    /// tree.split(Orientation::Vertical, ChildSide::Second)
    ///     .expect("the area is wide");
    ///
    /// // Two windows of 20 cells no longer fit, so the layout hides one.
    /// assert_eq!(
    ///     tree.set_area(Rect::new(0, 0, 30, 40)),
    ///     LayoutFit::Constrained { hidden_windows: 1, hidden_sidebars: 0 },
    /// );
    /// assert!(tree.layout().area(tree.focused_window()).is_some());
    /// ```
    pub fn set_area(&mut self, area: Rect) -> LayoutFit {
        self.area = area;
        self.recompute()
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
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError`] when the tree cannot issue another region
    /// identity.
    pub fn open_sidebar(
        &mut self,
        side: SidebarSide,
        width_cells: u16,
    ) -> Result<WindowId, IdentityError> {
        let (id, next_id) = self.peek_window_id()?;
        *self.sidebar_slot(side) = Some(Sidebar {
            id,
            side,
            width_cells: width_cells.clamp(SIDEBAR_WIDTH_MIN_CELLS, SIDEBAR_WIDTH_MAX_CELLS),
            visible: true,
        });
        self.next_id = next_id;
        self.recompute();
        Ok(id)
    }

    /// Shows or hides the sidebar at the named edge.
    ///
    /// Hiding a sidebar that holds the focus returns the focus to the
    /// previously focused window.
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
    /// # Errors
    ///
    /// Returns [`RegionError::Hidden`] when the region exists but the current
    /// layout shows no rectangle for it, so a hidden sidebar never holds the
    /// focus. Returns [`RegionError::Unknown`] when no such region exists.
    pub fn focus_region(&mut self, id: WindowId) -> Result<LayoutChange, RegionError> {
        let Some(region) = self.layout.region(id) else {
            return Err(if self.holds_identity(id) {
                RegionError::Hidden(id)
            } else {
                RegionError::Unknown(id)
            });
        };
        let focus = match region.kind {
            RegionKind::Surface => Focus::Window,
            RegionKind::Sidebar(side) => Focus::Sidebar(side),
        };
        let focused = match region.kind {
            RegionKind::Surface => id,
            RegionKind::Sidebar(_) => self.focused,
        };
        if self.focus == focus && self.focused == focused {
            return Ok(LayoutChange::Unchanged);
        }
        self.focus = focus;
        self.focused = focused;
        Ok(LayoutChange::Changed)
    }

    /// Moves the focus to the nearest region on the named side.
    ///
    /// The move compares layout rectangles, not tree order. The focus stays
    /// unchanged when no region touches that side.
    pub fn focus_direction(&mut self, direction: Direction) -> LayoutChange {
        let Some(id) = self.layout.neighbor(self.focused_region(), direction) else {
            return LayoutChange::Unchanged;
        };
        match self.focus_region(id) {
            Ok(change) => change,
            Err(error) => {
                debug_assert!(false, "the layout reported a visible neighbor: {error}");
                LayoutChange::Unchanged
            }
        }
    }

    /// Splits the focused window and focuses the new window.
    ///
    /// The new window shows the surface of the source window, and `new_side`
    /// names the child that receives it.
    ///
    /// The split is staged: the tree changes nothing until the staged layout
    /// shows both subtrees.
    ///
    /// # Errors
    ///
    /// Returns [`SplitError::WindowLimit`] at [`WINDOWS_MAX`] windows,
    /// [`SplitError::DepthLimit`] at [`SPLIT_DEPTH_MAX`] split levels,
    /// [`SplitError::AreaTooSmall`] when the focused rectangle cannot show both
    /// subtrees, and [`SplitError::Identity`] when the tree cannot issue
    /// another identity.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_ui::{ChildSide, Orientation, SplitError, WindowLimits, WindowTree};
    ///
    /// // The area holds 30 cells, and two windows need 40.
    /// let mut tree = WindowTree::new(0_u32, Rect::new(0, 0, 30, 40), WindowLimits::default());
    /// assert_eq!(
    ///     tree.split(Orientation::Vertical, ChildSide::Second),
    ///     Err(SplitError::AreaTooSmall { available: 30, required: 40 }),
    /// );
    /// assert_eq!(tree.window_count(), 1);
    /// ```
    pub fn split(
        &mut self,
        orientation: Orientation,
        new_side: ChildSide,
    ) -> Result<WindowId, SplitError> {
        if self.window_count() >= WINDOWS_MAX {
            return Err(SplitError::WindowLimit);
        }
        let mut path = Vec::new();
        self.root.leaf_path(self.focused, &mut path);
        if path.len() >= SPLIT_DEPTH_MAX {
            return Err(SplitError::DepthLimit);
        }
        let (id, next_id) = self.peek_window_id()?;
        let (split, next_split_id) = self.peek_split_id()?;
        let Some(source) = self.root.leaf(self.focused) else {
            debug_assert!(false, "the focused window is always a leaf of the tree");
            unreachable!("the focused window is always a leaf of the tree");
        };
        let new_leaf = Leaf {
            id,
            surface: source.surface.clone(),
        };

        let mut candidate = self.root.clone();
        let replaced = split_leaf(
            &mut candidate,
            self.focused,
            split,
            orientation,
            new_leaf,
            new_side,
        );
        debug_assert!(replaced, "the focused window is always a leaf of the tree");
        let layout = self.staged_layout(&candidate, id, self.left_sidebar, self.right_sidebar);
        // The split must show both subtrees. A staged layout that hides one of
        // them would leave a window without a rectangle, and a later resize
        // would work on an extent that no layout produced.
        if layout.region(id).is_none()
            || layout.region(self.focused).is_none()
            || layout.window_count() != self.layout.window_count() + 1
        {
            let available = self
                .layout
                .area(self.focused)
                .map_or(0, |area| axis_extent(orientation, area));
            return Err(SplitError::AreaTooSmall {
                available,
                required: self.limits.axis_minimum(orientation).saturating_mul(2),
            });
        }

        self.root = candidate;
        self.next_id = next_id;
        self.next_split_id = next_split_id;
        self.focused = id;
        self.focus = Focus::Window;
        self.layout = layout;
        Ok(id)
    }

    /// Returns the orientation that the adaptive split command selects.
    ///
    /// One rule comes before the ratio: a tree that holds exactly one window
    /// always selects a vertical split, because a full-width host area would
    /// otherwise divide into two short windows. Beyond one window, the rule
    /// compares the focused rectangle: a vertical split wins while the width
    /// exceeds the height multiplied by `ratio`, and a horizontal split wins
    /// otherwise. The inverse sense mirrors both the exception and the ratio.
    ///
    /// `ratio` names a width-to-height threshold, for example the value that
    /// `kvim_settings::SplitRatio::get` returns. The caller may pass a value
    /// that is not finite, zero, or negative. The rule then falls back to the
    /// neutral ratio 1.0, so it stays one defined width-to-height comparison.
    /// A comparison against a value such as `NaN` would otherwise silently
    /// answer `false` every time and always select [`Orientation::Horizontal`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_ui::{AdaptiveSplit, ChildSide, Orientation, WindowLimits, WindowTree};
    ///
    /// // Two windows, so the single-window exception does not decide the split.
    /// let mut wide = WindowTree::new("chat", Rect::new(0, 0, 200, 20), WindowLimits::default());
    /// wide.split(Orientation::Horizontal, ChildSide::Second)
    ///     .expect("the area is tall enough for two rows");
    /// // The focused window is 200 cells wide and 10 cells tall: wider than 10 * 2.5.
    /// assert_eq!(
    ///     wide.adaptive_orientation(AdaptiveSplit::Normal, 2.5),
    ///     Orientation::Vertical
    /// );
    ///
    /// let mut tall = WindowTree::new("chat", Rect::new(0, 0, 20, 200), WindowLimits::default());
    /// tall.split(Orientation::Horizontal, ChildSide::Second)
    ///     .expect("the area is tall enough for two rows");
    /// // The focused window is 20 cells wide and 100 cells tall: not wider than 100 * 2.5.
    /// assert_eq!(
    ///     tall.adaptive_orientation(AdaptiveSplit::Normal, 2.5),
    ///     Orientation::Horizontal
    /// );
    /// ```
    #[must_use]
    pub fn adaptive_orientation(&self, sense: AdaptiveSplit, ratio: f32) -> Orientation {
        // A ratio outside the validated domain falls back to the neutral
        // ratio 1.0, so the comparison below stays a defined answer instead
        // of silently favoring Horizontal through a false NaN comparison.
        let ratio = if ratio.is_finite() && ratio > 0.0 {
            ratio
        } else {
            1.0
        };
        let normal = if self.window_count() == 1 {
            Orientation::Vertical
        } else {
            let area = self
                .layout()
                .area(self.focused_window())
                .unwrap_or(self.area());
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

    /// Closes the focused region.
    ///
    /// Closing a focused sidebar hides it and returns the focus to the
    /// previously focused window. Closing a window replaces its parent split
    /// node with the remaining sibling and focuses the first window of that
    /// sibling.
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
        self.focus = Focus::Window;
        self.recompute();
        CloseOutcome::Closed(closed)
    }

    /// Moves one shared edge by `step_cells`.
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
    pub fn resize(&mut self, direction: Direction, step_cells: u16) -> LayoutChange {
        let focused = self.focused_region();
        if let Some(RegionKind::Sidebar(side)) = self.layout.region(focused).map(|r| r.kind) {
            return self.resize_sidebar(side, direction, step_cells);
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
            Some(RegionKind::Sidebar(side)) => self.resize_sidebar(side, direction, step_cells),
            Some(RegionKind::Surface) => self.resize_divider(direction, edge, step_cells),
            None => LayoutChange::Unchanged,
        }
    }

    /// Moves the divider that the focused window shares with another window.
    ///
    /// The move works in absolute cells. The panes across the divider give up
    /// the cells, a pane that reaches its minimum passes the rest to the next
    /// pane along the same direction, and every other pane keeps its exact
    /// size. The weights follow the resulting cell sizes, so the layout
    /// calculation reproduces them.
    fn resize_divider(
        &mut self,
        direction: Direction,
        edge: Direction,
        step_cells: u16,
    ) -> LayoutChange {
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
        let extent = axis_extent(orientation, area);
        let minimum = self.limits.axis_minimum(orientation);
        let delta = direction.divider_step() * i32::from(step_cells);

        let mut candidate = self.root.clone();
        if !move_divider(
            &mut candidate,
            step.split,
            orientation,
            extent,
            minimum,
            delta,
        ) {
            return LayoutChange::Unchanged;
        }
        self.commit(candidate)
    }

    /// Moves the inner edge of one sidebar in the named direction.
    ///
    /// The edge is one border of the layout, so it follows the same absolute
    /// rule as a divider between two windows: the pane that touches the sidebar
    /// absorbs the cells, a pane that reaches its minimum passes the rest to the
    /// next pane along, and every other pane keeps its exact width.
    fn resize_sidebar(
        &mut self,
        side: SidebarSide,
        direction: Direction,
        step_cells: u16,
    ) -> LayoutChange {
        let grows = match (side, direction) {
            (SidebarSide::Left, Direction::Right) | (SidebarSide::Right, Direction::Left) => true,
            (SidebarSide::Left, Direction::Left) | (SidebarSide::Right, Direction::Right) => false,
            (_, Direction::Up | Direction::Down) => return LayoutChange::Unchanged,
        };
        let step = i32::from(step_cells);
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
        let Some(candidate) = self.staged_tree_beside(left, right, side) else {
            return LayoutChange::Unchanged;
        };
        let layout = self.staged_layout(&candidate, self.focused, left, right);
        if !self.accepts(&layout) {
            return LayoutChange::Unchanged;
        }
        self.root = candidate;
        *self.sidebar_slot(side) = Some(staged);
        self.layout = layout;
        LayoutChange::Changed
    }

    /// Returns the tree that fills the window rectangle of two staged sidebars.
    ///
    /// The named sidebar owns the border that moves, so the end of the tree at
    /// that border absorbs the cells. Returns `None` when no arrangement of the
    /// tree keeps every window at its minimum width.
    fn staged_tree_beside(
        &self,
        left: Option<Sidebar>,
        right: Option<Sidebar>,
        side: SidebarSide,
    ) -> Option<Node<S>> {
        let before = editor_area(
            self.left_sidebar,
            self.right_sidebar,
            self.area,
            self.limits,
        );
        let after = editor_area(left, right, self.area, self.limits);
        let delta = i32::from(after.width) - i32::from(before.width);
        let mut candidate = self.root.clone();
        if delta == 0 {
            return Some(candidate);
        }
        let end = match side {
            SidebarSide::Left => ChildSide::First,
            SidebarSide::Right => ChildSide::Second,
        };
        move_edge(
            &mut candidate,
            Orientation::Vertical,
            before.width,
            end,
            delta,
            self.limits.min_width_cells(),
        )?;
        Some(candidate)
    }

    /// Publishes a staged tree only when its layout keeps every minimum.
    fn commit(&mut self, candidate: Node<S>) -> LayoutChange {
        let layout = self.staged_layout(
            &candidate,
            self.focused,
            self.left_sidebar,
            self.right_sidebar,
        );
        if !self.accepts(&layout) {
            return LayoutChange::Unchanged;
        }
        self.root = candidate;
        self.layout = layout;
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
            region.kind != RegionKind::Surface
                || (region.area.width >= self.limits.min_width_cells()
                    && region.area.height >= self.limits.min_height_rows())
        })
    }

    /// Returns the layout of one staged tree without publishing it.
    fn staged_layout(
        &self,
        root: &Node<S>,
        focused: WindowId,
        left: Option<Sidebar>,
        right: Option<Sidebar>,
    ) -> WindowLayout {
        compute_layout(root, focused, left, right, self.area, self.limits)
    }

    fn sidebar_slot(&mut self, side: SidebarSide) -> &mut Option<Sidebar> {
        match side {
            SidebarSide::Left => &mut self.left_sidebar,
            SidebarSide::Right => &mut self.right_sidebar,
        }
    }

    /// Reports whether the tree or a sidebar already holds the identity.
    fn holds_identity(&self, id: WindowId) -> bool {
        self.root.contains(id)
            || self.left_sidebar.is_some_and(|sidebar| sidebar.id == id)
            || self.right_sidebar.is_some_and(|sidebar| sidebar.id == id)
    }

    /// Returns the next region identity and the counter that follows it.
    ///
    /// The counter is checked, so it never wraps back to an issued value, and
    /// the duplicate check refuses an identity that is still in use. The caller
    /// stores the new counter only after its staged change succeeds.
    fn peek_window_id(&self) -> Result<(WindowId, u32), IdentityError> {
        let Some(next) = self.next_id.checked_add(1) else {
            return Err(IdentityError::Exhausted);
        };
        let id = WindowId(self.next_id);
        if self.holds_identity(id) {
            return Err(IdentityError::Duplicate(id));
        }
        Ok((id, next))
    }

    /// Returns the next split identity and the counter that follows it.
    fn peek_split_id(&self) -> Result<(SplitId, u32), IdentityError> {
        let Some(next) = self.next_split_id.checked_add(1) else {
            return Err(IdentityError::Exhausted);
        };
        Ok((SplitId(self.next_split_id), next))
    }

    /// Recomputes the layout and restores the focus invariant.
    fn recompute(&mut self) -> LayoutFit {
        self.layout = self.staged_layout(
            &self.root,
            self.focused,
            self.left_sidebar,
            self.right_sidebar,
        );
        if let Focus::Sidebar(side) = self.focus {
            let shown = self
                .sidebar(side)
                .is_some_and(|sidebar| self.layout.region(sidebar.id).is_some());
            if !shown {
                self.focus = Focus::Window;
            }
        }
        debug_assert!(
            self.area.is_empty() || self.layout.area(self.focused).is_some(),
            "the layout keeps the focused window visible in every non-empty host area"
        );
        self.layout.fit()
    }
}

/// Returns the extent of one rectangle along one axis.
pub(crate) const fn axis_extent(orientation: Orientation, area: Rect) -> u16 {
    match orientation {
        Orientation::Vertical => area.width,
        Orientation::Horizontal => area.height,
    }
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;
