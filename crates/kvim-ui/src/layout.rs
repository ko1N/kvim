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

/// The opaque identity of one visible layout border.
///
/// A split keeps this identity through terminal reflow while its topology
/// survives. A visible sidebar uses its stable window identity instead.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BorderId(BorderOwner);

impl BorderId {
    /// Returns the layout node that this border belongs to.
    pub(crate) const fn owner(self) -> BorderOwner {
        self.0
    }
}

/// The layout node that owns one border.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BorderOwner {
    /// The divider of one split node.
    Split(SplitId),
    /// The inner edge of one visible sidebar.
    Sidebar(WindowId),
}

/// The one-cell hit area of a visible split or sidebar border.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorderPlacement {
    id: BorderId,
    orientation: Orientation,
    area: Rect,
}

impl BorderPlacement {
    /// Returns the stable identity of this border.
    #[must_use]
    pub const fn id(self) -> BorderId {
        self.id
    }

    /// Returns the orientation of the border.
    #[must_use]
    pub const fn orientation(self) -> Orientation {
        self.orientation
    }

    /// Returns the one-cell-wide hit area of the border.
    #[must_use]
    pub const fn area(self) -> Rect {
        self.area
    }
}

/// Returns the border column of one sidebar rectangle.
///
/// A vertical border is the last column of the pane left of it, which is the
/// scrollbar column of that pane. A left sidebar owns that column itself. A
/// right sidebar borders the window tree beside it, so the border is the last
/// column of that tree. A right sidebar at the left edge of the host area
/// borders no pane and publishes no border.
const fn sidebar_border_column(area: Rect, side: SidebarSide) -> Option<u16> {
    match side {
        SidebarSide::Left => Some(area.x.saturating_add(area.width.saturating_sub(1))),
        SidebarSide::Right => area.x.checked_sub(1),
    }
}

/// Reports whether one rectangle names only cells that a buffer holds.
///
/// Every public render checks its rectangle with this function before it writes
/// one cell, because `ratatui::Buffer` panics on a cell outside its own
/// rectangle. An empty rectangle names no cell at all, so every buffer holds it.
pub(crate) fn fits(area: Rect, buffer: Rect) -> bool {
    area.is_empty()
        || (area.x >= buffer.x
            && area.y >= buffer.y
            && area.right() <= buffer.right()
            && area.bottom() <= buffer.bottom())
}

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
    borders: Vec<BorderPlacement>,
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

    /// Returns every visible split and sidebar border in layout order.
    ///
    /// Each placement has a one-cell hit area. A vertical split uses the last
    /// column of its first child. A horizontal split uses the first row of its
    /// second child. A sidebar uses its edge beside the window tree.
    #[must_use]
    pub fn borders(&self) -> &[BorderPlacement] {
        &self.borders
    }

    /// Returns the visible placement of the named border.
    #[must_use]
    pub fn border(&self, id: BorderId) -> Option<&BorderPlacement> {
        self.borders.iter().find(|border| border.id == id)
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

    for region in [left, right].into_iter().flatten() {
        let RegionKind::Sidebar(side) = region.kind else {
            continue;
        };
        let Some(column) = sidebar_border_column(region.area, side) else {
            continue;
        };
        let area = Rect::new(column, region.area.y, 1, region.area.height);
        layout.borders.push(BorderPlacement {
            id: BorderId(BorderOwner::Sidebar(region.id)),
            orientation: Orientation::Vertical,
            area,
        });
    }

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
            let border_area = match orientation {
                Orientation::Vertical => {
                    Rect::new(first_area.right() - 1, first_area.y, 1, first_area.height)
                }
                Orientation::Horizontal => {
                    Rect::new(second_area.x, second_area.y, second_area.width, 1)
                }
            };
            layout.borders.push(BorderPlacement {
                id: BorderId(BorderOwner::Split(*id)),
                orientation: *orientation,
                area: border_area,
            });
            layout_node(first, first_area, focused, limits, layout);
            layout_node(second, second_area, focused, limits, layout);
        }
    }
}
