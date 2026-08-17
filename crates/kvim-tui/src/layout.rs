//! The one layout calculation of the editor.
//!
//! The calculation converts the window tree, the sidebars, and the terminal
//! rectangle into the exact rectangle of every visible region. Rendering,
//! scrolling, focus, resize, and tests all read these rectangles. No other code
//! computes a rectangle. See `docs/windows.md`.
//!
//! The calculation is deterministic. An equal tree, equal weights, and an equal
//! terminal size produce equal rectangles.

use ratatui::layout::Rect;

use kvim_settings::WindowSettings;

use super::window::{
    Direction, Node, Orientation, SPLIT_WEIGHT_TOTAL, Sidebar, SidebarSide, SplitId, WindowId,
};

/// The purpose of one region of the terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionKind {
    /// One editor window from the window tree.
    Editor,
    /// One fixed-width sidebar at one edge of the terminal.
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

/// The rectangle of every visible region of one terminal.
///
/// The regions cover the terminal without a gap and without an overlap while
/// the terminal is large enough to hold the tree. A terminal that is too small
/// hides windows in a deterministic order and keeps the focused window visible.
///
/// # Examples
///
/// ```
/// use ratatui::layout::Rect;
///
/// use kvim_settings::WindowSettings;
/// use kvim_tui::{Direction, Orientation, Windows};
/// use kvim_workspace::BufferId;
///
/// let terminal = Rect::new(0, 0, 100, 30);
/// let mut windows = Windows::new(BufferId::new(1), terminal, WindowSettings::default());
/// let left = windows.focused_window();
/// let right = windows.split(Orientation::Vertical).expect("the terminal is wide");
///
/// let layout = windows.layout();
/// let covered: u32 = layout.regions().iter().map(|region| region.area.area()).sum();
/// assert_eq!(covered, terminal.area());
/// assert_eq!(layout.neighbor(left, Direction::Right), Some(right));
/// assert_eq!(layout.neighbor(right, Direction::Right), None);
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WindowLayout {
    regions: Vec<Region>,
    splits: Vec<(SplitId, Rect)>,
}

impl WindowLayout {
    /// Creates a layout without a region.
    #[must_use]
    pub(super) fn empty() -> Self {
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

    /// Returns the number of visible editor windows.
    #[must_use]
    pub fn window_count(&self) -> usize {
        self.regions
            .iter()
            .filter(|region| region.kind == RegionKind::Editor)
            .count()
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
    pub(super) fn split_area(&self, id: SplitId) -> Option<Rect> {
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
/// The result always keeps both children at or above `minimum`, so the layout
/// enforces the minimum dimensions before it publishes rectangles. The caller
/// guarantees `extent >= minimum * 2` and `minimum >= 1`.
pub(super) fn first_extent(extent: u16, weight: u32, minimum: u16) -> u16 {
    debug_assert!(
        minimum >= 1 && extent >= minimum.saturating_mul(2),
        "the caller materializes a split node only when both children fit"
    );
    let share = u32::from(extent) * weight.min(SPLIT_WEIGHT_TOTAL) / SPLIT_WEIGHT_TOTAL;
    let share = u16::try_from(share).unwrap_or(extent);
    share.clamp(minimum, extent - minimum)
}

/// Converts the window tree, the sidebars, and the terminal into rectangles.
pub(super) fn compute_layout(
    root: &Node,
    focused: WindowId,
    left: Option<Sidebar>,
    right: Option<Sidebar>,
    terminal: Rect,
    settings: &WindowSettings,
) -> WindowLayout {
    let mut layout = WindowLayout::empty();
    if terminal.is_empty() {
        return layout;
    }
    // Carve the left sidebar first, then the right sidebar, so a terminal that
    // cannot hold both hides them in a deterministic order.
    let mut editor = terminal;
    let left = carve_sidebar(&mut editor, left, settings);
    let right = carve_sidebar(&mut editor, right, settings);

    layout.regions.extend(left);
    layout_node(root, editor, focused, settings, &mut layout);
    layout.regions.extend(right);
    layout
}

/// Removes the width of one visible sidebar from the editor rectangle.
///
/// The sidebar stays hidden while the remaining width would fall below the
/// minimum window width.
fn carve_sidebar(
    editor: &mut Rect,
    sidebar: Option<Sidebar>,
    settings: &WindowSettings,
) -> Option<Region> {
    let sidebar = sidebar.filter(|sidebar| sidebar.is_visible())?;
    let width = sidebar.width_cells();
    let minimum = settings.min_window_width_cells.max(1);
    if width == 0 || editor.width < width.saturating_add(minimum) {
        return None;
    }
    let area = match sidebar.side() {
        SidebarSide::Left => {
            let area = Rect::new(editor.x, editor.y, width, editor.height);
            *editor = Rect::new(
                editor.x + width,
                editor.y,
                editor.width - width,
                editor.height,
            );
            area
        }
        SidebarSide::Right => {
            let area = Rect::new(editor.right() - width, editor.y, width, editor.height);
            *editor = Rect::new(editor.x, editor.y, editor.width - width, editor.height);
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
fn layout_node(
    node: &Node,
    area: Rect,
    focused: WindowId,
    settings: &WindowSettings,
    layout: &mut WindowLayout,
) {
    if area.is_empty() {
        return;
    }
    match node {
        Node::Leaf(leaf) => layout.regions.push(Region {
            id: leaf.id,
            kind: RegionKind::Editor,
            area,
        }),
        Node::Split {
            id,
            orientation,
            first_weight,
            first,
            second,
        } => {
            let (extent, minimum) = match orientation {
                Orientation::Vertical => (area.width, settings.min_window_width_cells.max(1)),
                Orientation::Horizontal => (area.height, settings.min_window_height_rows.max(1)),
            };
            if extent < minimum.saturating_mul(2) {
                // The rectangle cannot hold both children. Keep the subtree
                // that holds the focused window, so the focus stays visible.
                let kept = if second.contains(focused) {
                    second
                } else {
                    first
                };
                layout_node(kept, area, focused, settings, layout);
                return;
            }
            layout.splits.push((*id, area));
            let head = first_extent(extent, *first_weight, minimum);
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
            layout_node(first, first_area, focused, settings, layout);
            layout_node(second, second_area, focused, settings, layout);
        }
    }
}
