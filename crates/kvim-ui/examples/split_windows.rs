//! Split one host area between caller-owned surfaces and print the layout.
//!
//! The host owns every surface value. The window tree stores only the opaque
//! `SurfaceId`, so this example needs no editor, no buffer, and no terminal. It
//! paints the result into a ratatui test buffer and prints that buffer.
//!
//! Run it with:
//!
//! ```sh
//! cargo run -p kvim-ui --example split_windows
//! ```

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use kvim_ui::{
    AdaptiveSplit, ChildSide, Direction, LayoutFit, Orientation, RegionKind, SidebarSide, WindowId,
    WindowLimits, WindowTree,
};

/// The width-to-height threshold that the adaptive split rule compares.
///
/// A host owns this value. `kvim-ui` reads no settings, so the rule takes it as
/// an argument. Kvim's own default is 2.5.
const ADAPTIVE_RATIO: f32 = 2.5;

/// The identity of one host surface.
///
/// The host keeps the surface values. The tree never reads them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SurfaceId(u32);

/// One surface that the host owns.
struct Surface {
    id: SurfaceId,
    title: &'static str,
    mark: char,
}

const HOST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 72,
    height: 18,
};

fn main() {
    let surfaces = [
        Surface {
            id: SurfaceId(1),
            title: "notes",
            mark: 'N',
        },
        Surface {
            id: SurfaceId(2),
            title: "chat",
            mark: 'C',
        },
        Surface {
            id: SurfaceId(3),
            title: "log",
            mark: 'L',
        },
    ];

    // One window shows the first surface. The limits keep every window usable.
    let mut tree = WindowTree::new(surfaces[0].id, HOST_AREA, WindowLimits::default());

    // A host that binds kvim's adaptive split key asks the tree which
    // orientation the current shape selects. A tree with one window always
    // selects a vertical split, because a full-width area would otherwise
    // divide into two short windows.
    let adaptive = tree.adaptive_orientation(AdaptiveSplit::Normal, ADAPTIVE_RATIO);
    println!("adaptive orientation with one window: {adaptive:?}");
    assert_eq!(adaptive, Orientation::Vertical);

    // A vertical split opens the new window on the right and focuses it.
    let right = tree
        .split(Orientation::Vertical, ChildSide::Second)
        .expect("the host area is wide enough for two windows");
    tree.replace_surface(right, surfaces[1].id)
        .expect("the split returned this window");

    // A horizontal split divides the focused window top and bottom.
    let bottom = tree
        .split(Orientation::Horizontal, ChildSide::Second)
        .expect("the host area is tall enough for two windows");
    tree.replace_surface(bottom, surfaces[2].id)
        .expect("the split returned this window");

    // A sidebar keeps a fixed width at one edge and never joins the tree.
    let sidebar = tree
        .open_sidebar(SidebarSide::Left, 14)
        .expect("the tree can issue one more identity");

    // Beyond one window the rule compares the focused rectangle against the
    // ratio. The inverse sense mirrors the answer.
    println!(
        "adaptive orientation beyond one window: {:?}, inverse {:?}",
        tree.adaptive_orientation(AdaptiveSplit::Normal, ADAPTIVE_RATIO),
        tree.adaptive_orientation(AdaptiveSplit::Inverse, ADAPTIVE_RATIO)
    );

    println!("focus: {:?}", tree.focused_region());
    println!("fit:   {:?}", tree.layout().fit());
    println!("{}", render(&tree, &surfaces, sidebar));

    // Directional focus compares rectangles, not tree order.
    tree.focus_direction(Direction::Left);
    println!(
        "focus after one move to the left: {:?}",
        tree.focused_region()
    );

    // A host area that cannot show every window reports its constraint instead
    // of hiding a surface silently.
    let fit = tree.set_area(Rect::new(0, 0, 24, 18));
    println!("fit after the host shrinks: {fit:?}");
    assert!(matches!(fit, LayoutFit::Constrained { .. }));
    assert!(
        tree.layout().area(tree.focused_window()).is_some(),
        "a constrained layout keeps the focused window visible"
    );
}

/// Paints one mark for every visible region and returns the printable rows.
fn render(tree: &WindowTree<SurfaceId>, surfaces: &[Surface], sidebar: WindowId) -> String {
    let mut buffer = Buffer::empty(tree.area());
    for region in tree.layout().regions() {
        let mark = match region.kind {
            RegionKind::Sidebar(_) => '#',
            RegionKind::Surface => tree
                .surface(region.id)
                .and_then(|id| surfaces.iter().find(|surface| surface.id == *id))
                .map_or('?', |surface| surface.mark),
        };
        for y in region.area.top()..region.area.bottom() {
            for x in region.area.left()..region.area.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.set_char(mark);
                }
            }
        }
    }

    let mut out = String::new();
    for surface in surfaces {
        let (mark, title) = (surface.mark, surface.title);
        out.push_str(&format!("{mark} = {title}\n"));
    }
    out.push_str(&format!("# = sidebar {sidebar:?}\n"));
    for y in buffer.area.top()..buffer.area.bottom() {
        for x in buffer.area.left()..buffer.area.right() {
            let symbol = buffer.cell((x, y)).map_or(" ", |cell| cell.symbol());
            out.push_str(symbol);
        }
        out.push('\n');
    }
    out
}
