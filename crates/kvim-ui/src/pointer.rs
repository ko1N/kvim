//! Pure half-open rectangle hit testing for published layouts.
//!
//! Callers supply published surface and overlay placements. This module never
//! calculates a second layout.

use ratatui::layout::Rect;

use crate::{RegionKind, SurfacePlacement, WindowId};

/// A zero-based terminal cell position.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Cell {
    column: u16,
    row: u16,
}

impl Cell {
    /// Creates a terminal cell position.
    #[must_use]
    pub const fn new(column: u16, row: u16) -> Self {
        Self { column, row }
    }

    /// Returns the terminal cell column.
    #[must_use]
    pub const fn column(self) -> u16 {
        self.column
    }

    /// Returns the terminal cell row.
    #[must_use]
    pub const fn row(self) -> u16 {
        self.row
    }
}

/// The ownership of an overlay for pointer input.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverlayInput {
    /// The overlay consumes pointer input inside its placement.
    Interactive,
    /// The overlay paints above a target but does not consume pointer input.
    #[default]
    Decorative,
}

/// One overlay placement and its pointer ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PointerOverlay<Id> {
    /// The overlay identity.
    pub id: Id,
    /// The published rectangle of the overlay.
    pub area: Rect,
    /// Whether the overlay consumes pointer input.
    pub input: OverlayInput,
}

/// The semantic target at one terminal cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HitTarget<Id> {
    /// An interactive overlay owns the cell.
    Overlay(Id),
    /// A sidebar owns the cell.
    Sidebar(WindowId),
    /// An editor surface owns the cell.
    Surface(WindowId),
    /// No interactive target owns the cell.
    Chrome,
}

/// Reports whether a half-open rectangle contains a terminal cell.
///
/// The right and bottom edges are outside the rectangle. This matches ratatui
/// rectangles and makes adjacent regions have no shared cell.
#[must_use]
pub const fn contains_cell(area: Rect, cell: Cell) -> bool {
    cell.column >= area.x
        && cell.column < area.x.saturating_add(area.width)
        && cell.row >= area.y
        && cell.row < area.y.saturating_add(area.height)
}

/// Resolves one pointer cell through published overlay and surface geometry.
///
/// Interactive overlays outrank every background region. Decorative overlays
/// pass through. A cell outside all published regions is shell chrome.
#[must_use]
pub fn hit_test<Sid, OverlayId: Copy>(
    surfaces: &[SurfacePlacement<Sid>],
    overlays: &[PointerOverlay<OverlayId>],
    cell: Cell,
) -> HitTarget<OverlayId> {
    if let Some(overlay) = overlays.iter().rev().find(|overlay| {
        overlay.input == OverlayInput::Interactive && contains_cell(overlay.area, cell)
    }) {
        return HitTarget::Overlay(overlay.id);
    }
    let Some(surface) = surfaces
        .iter()
        .find(|surface| contains_cell(surface.area, cell))
    else {
        return HitTarget::Chrome;
    };
    match surface.kind {
        RegionKind::Sidebar(_) => HitTarget::Sidebar(surface.region),
        RegionKind::Surface => HitTarget::Surface(surface.region),
    }
}

#[cfg(test)]
#[path = "pointer_tests.rs"]
mod tests;
