use ratatui::layout::Rect;

use crate::{
    Cell, HitTarget, OverlayInput, PointerOverlay, SurfacePlacement, WindowLimits, WindowTree,
    contains_cell, hit_test,
};

fn layout() -> Vec<SurfacePlacement<&'static str>> {
    let tree = WindowTree::new("editor", Rect::new(2, 3, 20, 10), WindowLimits::default());
    tree.layout()
        .regions()
        .iter()
        .map(|region| SurfacePlacement {
            surface: "editor",
            region: region.id,
            kind: region.kind,
            area: region.area,
        })
        .collect()
}

#[test]
fn rectangle_edges_are_half_open() {
    let area = Rect::new(4, 5, 3, 2);
    assert!(contains_cell(area, Cell::new(4, 5)));
    assert!(contains_cell(area, Cell::new(6, 6)));
    assert!(!contains_cell(area, Cell::new(7, 6)));
    assert!(!contains_cell(area, Cell::new(6, 7)));
}

#[test]
fn interactive_overlays_outrank_and_decorative_overlays_pass_through() {
    let layout = layout();
    let point = Cell::new(3, 4);
    let interactive = [PointerOverlay {
        id: "menu",
        area: Rect::new(3, 4, 4, 2),
        input: OverlayInput::Interactive,
    }];
    assert_eq!(
        hit_test(&layout, &interactive, point),
        HitTarget::Overlay("menu")
    );

    let decorative = [PointerOverlay {
        id: "notice",
        area: Rect::new(3, 4, 4, 2),
        input: OverlayInput::Decorative,
    }];
    assert!(matches!(
        hit_test(&layout, &decorative, point),
        HitTarget::Surface(_)
    ));
}

#[test]
fn cells_outside_published_regions_are_chrome() {
    assert_eq!(
        hit_test::<_, &'static str>(&layout(), &[], Cell::new(0, 0)),
        HitTarget::Chrome
    );
}
