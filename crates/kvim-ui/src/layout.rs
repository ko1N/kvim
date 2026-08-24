//! The one layout calculation of the window tree.
//!
//! The calculation converts the window tree, the sidebars, and the host
//! rectangle into the exact rectangle of every visible region. Rendering,
//! scrolling, focus, resize, and tests all read these rectangles. No other code
//! computes a rectangle.
//!
//! The calculation is deterministic. An equal tree, equal weights, and an equal
//! host size produce equal rectangles.

use ratatui::layout::Rect;

use crate::window::{
    Direction, LayoutFit, Node, Orientation, SPLIT_WEIGHT_TOTAL, Sidebar, SidebarSide, SplitId,
    WindowId, WindowLimits, axis_extent, child_minima,
};

/// The purpose of one region of the host area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionKind {
    /// One window from the window tree, which shows one host surface.
    Surface,
    /// One fixed-width sidebar at one edge of the host area.
    Sidebar(SidebarSide),
}

/// One visible region and its rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Region {
    /// The stable identity of the window or the sidebar.
    pub id: WindowId,
    /// The purpose of the region.
    pub kind: RegionKind,
    /// The rectangle that the region occupies.
    pub area: Rect,
}

/// The rectangle of every visible region of one host area.
///
/// The regions cover the host area without a gap and without an overlap while
/// that area is large enough to hold the tree. A smaller area hides windows in
/// a deterministic order, reports [`LayoutFit::Constrained`], and keeps the
/// focused window visible.
///
/// # Examples
///
/// ```
/// use ratatui::layout::Rect;
///
/// use kvim_ui::{ChildSide, Direction, LayoutFit, Orientation, WindowLimits, WindowTree};
///
/// let area = Rect::new(0, 0, 100, 30);
/// let mut tree = WindowTree::new(0_u32, area, WindowLimits::default());
/// let left = tree.focused_window();
/// let right = tree
///     .split(Orientation::Vertical, ChildSide::Second)
///     .expect("the area is wide");
///
/// let layout = tree.layout();
/// let covered: u32 = layout.regions().iter().map(|region| region.area.area()).sum();
/// assert_eq!(covered, area.area());
/// assert_eq!(layout.fit(), LayoutFit::Complete);
/// assert_eq!(layout.neighbor(left, Direction::Right), Some(right));
/// assert_eq!(layout.neighbor(right, Direction::Right), None);
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WindowLayout {
    regions: Vec<Region>,
    splits: Vec<(SplitId, Rect)>,
    hidden_windows: usize,
    hidden_sidebars: usize,
}

impl WindowLayout {
    /// Creates a layout without a region.
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// Returns every visible region in layout order.
    ///
    /// The order is the left sidebar, then the windows in tree order, then the
    /// right sidebar.
    #[must_use]
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// Returns the named region.
    #[must_use]
    pub fn region(&self, id: WindowId) -> Option<&Region> {
        self.regions.iter().find(|region| region.id == id)
    }

    /// Returns the rectangle of the named region.
    #[must_use]
    pub fn area(&self, id: WindowId) -> Option<Rect> {
        self.region(id).map(|region| region.area)
    }

    /// Returns the number of visible windows.
    #[must_use]
    pub fn window_count(&self) -> usize {
        self.regions
            .iter()
            .filter(|region| region.kind == RegionKind::Surface)
            .count()
    }

    /// Reports how much of the tree this layout shows.
    ///
    /// A layout that hides a window or a requested sidebar names the counts, so
    /// no surface disappears silently.
    #[must_use]
    pub const fn fit(&self) -> LayoutFit {
        if self.hidden_windows == 0 && self.hidden_sidebars == 0 {
            return LayoutFit::Complete;
        }
        LayoutFit::Constrained {
            hidden_windows: self.hidden_windows,
            hidden_sidebars: self.hidden_sidebars,
        }
    }

    /// Returns the nearest region on the named side of the named region.
    ///
    /// Two regions are neighbors when one edge meets the other edge and the
    /// perpendicular ranges overlap. The region with the largest overlap wins.
    /// The first region in layout order wins an equal overlap.
    #[must_use]
    pub fn neighbor(&self, from: WindowId, direction: Direction) -> Option<WindowId> {
        let origin = self.area(from)?;
        let mut best: Option<(u16, WindowId)> = None;
        for region in &self.regions {
            if region.id == from {
                continue;
            }
            let other = region.area;
            let (adjacent, overlap) = match direction {
                Direction::Left => (other.right() == origin.x, row_overlap(origin, other)),
                Direction::Right => (other.x == origin.right(), row_overlap(origin, other)),
                Direction::Up => (other.bottom() == origin.y, column_overlap(origin, other)),
                Direction::Down => (other.y == origin.bottom(), column_overlap(origin, other)),
            };
            if !adjacent || overlap == 0 {
                continue;
            }
            if best.is_none_or(|(best_overlap, _)| overlap > best_overlap) {
                best = Some((overlap, region.id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Returns the rectangle that one materialized split node divides.
    pub(crate) fn split_area(&self, id: SplitId) -> Option<Rect> {
        self.splits
            .iter()
            .find(|(split, _)| *split == id)
            .map(|(_, area)| *area)
    }
}

/// Returns the number of rows that two rectangles share.
fn row_overlap(first: Rect, second: Rect) -> u16 {
    first
        .bottom()
        .min(second.bottom())
        .saturating_sub(first.y.max(second.y))
}

/// Returns the number of columns that two rectangles share.
fn column_overlap(first: Rect, second: Rect) -> u16 {
    first
        .right()
        .min(second.right())
        .saturating_sub(first.x.max(second.x))
}

/// Returns the extent of the first child of one split node.
///
/// Each child reports its own recursive minimum, so a nested subtree keeps room
/// for every leaf that it holds. The result always keeps both children at or
/// above their minimum while `extent >= first_min + second_min`.
pub(crate) fn first_extent(extent: u16, weight: u32, first_min: u16, second_min: u16) -> u16 {
    debug_assert!(
        extent >= first_min.saturating_add(second_min),
        "the caller materializes a split node only when both subtrees fit"
    );
    let share = u32::from(extent) * weight.min(SPLIT_WEIGHT_TOTAL) / SPLIT_WEIGHT_TOTAL;
    let share = u16::try_from(share).unwrap_or(extent);
    // The bounds saturate, so an extent below the two minima still returns a
    // value inside the extent instead of overflowing.
    let low = first_min.min(extent);
    let high = extent.saturating_sub(second_min).max(low);
    share.clamp(low, high)
}

/// Converts the window tree, the sidebars, and the host area into rectangles.
pub(crate) fn compute_layout<S>(
    root: &Node<S>,
    focused: WindowId,
    left: Option<Sidebar>,
    right: Option<Sidebar>,
    area: Rect,
    limits: WindowLimits,
) -> WindowLayout {
    let mut layout = WindowLayout::empty();
    if area.is_empty() {
        layout.hidden_windows = root.window_count();
        layout.hidden_sidebars = [left, right]
            .into_iter()
            .flatten()
            .filter(|sidebar| sidebar.is_visible())
            .count();
        return layout;
    }
    // Carve the left sidebar first, then the right sidebar, so a host area that
    // cannot hold both hides them in a deterministic order.
    let mut windows = area;
    let mut hidden_sidebars = 0;
    let left = carve_sidebar(&mut windows, left, limits, &mut hidden_sidebars);
    let right = carve_sidebar(&mut windows, right, limits, &mut hidden_sidebars);
    layout.hidden_sidebars = hidden_sidebars;

    layout.regions.extend(left);
    layout_node(root, windows, focused, limits, &mut layout);
    layout.regions.extend(right);
    layout
}

/// Returns the rectangle that the window tree occupies.
///
/// The calculation removes the width of every visible sidebar in the same
/// deterministic order that [`compute_layout`] uses, so both agree on the
/// extent that the tree divides.
pub(crate) fn editor_area(
    left: Option<Sidebar>,
    right: Option<Sidebar>,
    area: Rect,
    limits: WindowLimits,
) -> Rect {
    let mut windows = area;
    let mut hidden_sidebars = 0;
    let _ = carve_sidebar(&mut windows, left, limits, &mut hidden_sidebars);
    let _ = carve_sidebar(&mut windows, right, limits, &mut hidden_sidebars);
    windows
}

/// Removes the width of one visible sidebar from the window rectangle.
///
/// The sidebar stays hidden while the remaining width would fall below the
/// minimum window width. A hidden sidebar raises the constrained count, so the
/// layout never drops it silently.
fn carve_sidebar(
    windows: &mut Rect,
    sidebar: Option<Sidebar>,
    limits: WindowLimits,
    hidden_sidebars: &mut usize,
) -> Option<Region> {
    let sidebar = sidebar.filter(|sidebar| sidebar.is_visible())?;
    let width = sidebar.width_cells();
    let minimum = limits.min_width_cells();
    if width == 0 || windows.width < width.saturating_add(minimum) {
        *hidden_sidebars += 1;
        return None;
    }
    let area = match sidebar.side() {
        SidebarSide::Left => {
            let area = Rect::new(windows.x, windows.y, width, windows.height);
            *windows = Rect::new(
                windows.x + width,
                windows.y,
                windows.width - width,
                windows.height,
            );
            area
        }
        SidebarSide::Right => {
            let area = Rect::new(windows.right() - width, windows.y, width, windows.height);
            *windows = Rect::new(windows.x, windows.y, windows.width - width, windows.height);
            area
        }
    };
    Some(Region {
        id: sidebar.id(),
        kind: RegionKind::Sidebar(sidebar.side()),
        area,
    })
}

/// Places one subtree inside one rectangle.
fn layout_node<S>(
    node: &Node<S>,
    area: Rect,
    focused: WindowId,
    limits: WindowLimits,
    layout: &mut WindowLayout,
) {
    if area.is_empty() {
        layout.hidden_windows += node.window_count();
        return;
    }
    match node {
        Node::Leaf(leaf) => layout.regions.push(Region {
            id: leaf.id,
            kind: RegionKind::Surface,
            area,
        }),
        Node::Split {
            id,
            orientation,
            first_weight,
            first,
            second,
        } => {
            let extent = axis_extent(*orientation, area);
            let minimum = limits.axis_minimum(*orientation);
            let (first_min, second_min) = child_minima(first, second, *orientation, minimum);
            if extent < first_min.saturating_add(second_min) {
                // The rectangle cannot hold both subtrees. Keep the subtree
                // that holds the focused window, so the focus stays visible.
                let (kept, dropped) = if second.contains(focused) {
                    (second, first)
                } else {
                    (first, second)
                };
                layout.hidden_windows += dropped.window_count();
                layout_node(kept, area, focused, limits, layout);
                return;
            }
            layout.splits.push((*id, area));
            let head = first_extent(extent, *first_weight, first_min, second_min);
            let (first_area, second_area) = match orientation {
                Orientation::Vertical => (
                    Rect::new(area.x, area.y, head, area.height),
                    Rect::new(area.x + head, area.y, area.width - head, area.height),
                ),
                Orientation::Horizontal => (
                    Rect::new(area.x, area.y, area.width, head),
                    Rect::new(area.x, area.y + head, area.width, area.height - head),
                ),
            };
            layout_node(first, first_area, focused, limits, layout);
            layout_node(second, second_area, focused, limits, layout);
        }
    }
}
