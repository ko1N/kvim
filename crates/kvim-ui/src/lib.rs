//! Generic split topology, focus, resize, sidebars, and deterministic geometry.
//!
//! The crate holds no editor state. [`WindowTree`] stores one opaque surface
//! identity for each leaf window, the split structure, the sidebar widths, and
//! the host rectangle. The host owns every surface value, so one tree serves an
//! editor, a chat panel, a review panel, or any other caller-owned surface.
//!
//! [`SidebarState`] stores the rows of one sidebar the same way: one opaque row
//! identity, the terminal rows that the row occupies, the selection, and the
//! viewport. Rows, heights, styles, labels, and the meaning of every action
//! stay with the host, and one host callback draws every cell.
//!
//! [`Selector`] holds the mechanism that narrows a bounded candidate list
//! through a query: the query, the ranked matches, and a selection that
//! survives a refiltering while the query still matches it. It names no path,
//! no buffer, and no file, so any host that ranks a list of its own values
//! through a name and a container string can hold it.
//!
//! [`WorkspaceComposer`] joins those parts into one composition model of a
//! complete host-owned workspace: split geometry, sidebar regions, overlay
//! ownership, focus, one shared resolver, and which-key state. It owns no
//! surface value and no host command. A focus or overlay transition that needs
//! surface state returns one addressed [`CompositionEffect::CancelPending`],
//! and the host resumes it with the reset context. See `docs/embedding.md`.
//!
//! [`WhichKeyOverlay`] renders the keys that may follow a pending key sequence.
//! It holds no binding table: the caller derives its hints from the one shared
//! registry of `kvim-keymap` and supplies final texts, optional icons, and its
//! own styles.
//!
//! Every value is pure and deterministic. The crate reads no clock, no
//! filesystem, and no terminal. One layout calculation converts the tree and
//! the host rectangle into the exact rectangle of every visible region, so no
//! other code computes a rectangle.
//!
//! Leaf count, split depth, ratio precision, identity allocation, and minimum
//! window dimensions are explicit limits. A layout that cannot show every
//! window reports [`LayoutFit::Constrained`] and keeps the focused window
//! visible.
//!
//! # Examples
//!
//! `examples/split_windows.rs` runs the complete flow with caller-owned
//! surfaces, `examples/sidebar.rs` renders two-line sidebar rows with state
//! markers, and `examples/which_key.rs` derives overlay hints from one shared
//! registry and renders them:
//!
//! ```sh
//! cargo run -p kvim-ui --example split_windows
//! cargo run -p kvim-ui --example sidebar
//! cargo run -p kvim-ui --example which_key
//! ```
//!
//! `crates/kvim-tui/examples/host_workspace.rs` composes host-owned chat, one
//! embedded editor, one review surface, and one sidebar into one workspace
//! through [`WorkspaceComposer`]:
//!
//! ```sh
//! cargo run -p kvim-tui --example host_workspace
//! ```
//!
//! ```
//! use ratatui::layout::Rect;
//!
//! use kvim_ui::{ChildSide, Direction, LayoutChange, Orientation, WindowLimits, WindowTree};
//!
//! // The host names its own surfaces. The tree copies the identity only.
//! #[derive(Clone, Copy, Debug, Eq, PartialEq)]
//! struct SurfaceId(u32);
//!
//! let mut tree = WindowTree::new(
//!     SurfaceId(1),
//!     Rect::new(0, 0, 120, 40),
//!     WindowLimits::default(),
//! );
//! let right = tree
//!     .split(Orientation::Vertical, ChildSide::Second)
//!     .expect("the area is wide");
//! tree.replace_surface(right, SurfaceId(2)).expect("the window exists");
//!
//! assert_eq!(tree.surface(right), Some(&SurfaceId(2)));
//! assert_eq!(tree.focus_direction(Direction::Left), LayoutChange::Changed);
//! ```

// The crate is one supported external package. Every published item names
// its own contract, so no implementation API can reach a consumer by accident.
#![deny(missing_docs)]

mod composer;
mod layout;
mod selector;
mod sidebar;
mod tabs;
mod which_key;
mod window;

pub use composer::{
    COMPOSED_SURFACES_MAX, Composition, CompositionEffect, CompositionLayout, OverlayPlacement,
    ResumeError, SurfacePlacement, TransitionId, UnknownSurface, WorkspaceComposer,
};
pub use layout::{Region, RegionKind, WindowLayout};
pub use selector::{
    SELECTOR_CANDIDATES_MAX, SELECTOR_QUERY_CHARS_MAX, Selector, SelectorCandidate,
};
pub use sidebar::{
    RowKind, SIDEBAR_ACTION_CHARS_MAX, SIDEBAR_LABEL_CHARS_MAX, SIDEBAR_ROW_DRAWS_MAX,
    SIDEBAR_ROW_LINES_MAX, SIDEBAR_ROWS_MAX, SidebarAction, SidebarCanvas, SidebarError,
    SidebarEvent, SidebarInput, SidebarMotion, SidebarPlacement, SidebarRow, SidebarState,
};
pub use tabs::{TAB_LABEL_CHARS_MAX, TABS_MAX, Tab, TabError, TabPlacement, TabStrip};
pub use which_key::{
    WHICH_KEY_BODY_SHARE, WHICH_KEY_COLUMN_ROWS_MAX, WHICH_KEY_HINTS_MAX, WHICH_KEY_TEXT_CHARS_MAX,
    WhichKeyError, WhichKeyHint, WhichKeyIcon, WhichKeyOverlay, WhichKeyStyles,
};
pub use window::{
    ChildSide, CloseOutcome, Direction, IdentityError, LayoutChange, LayoutFit, Orientation,
    RegionError, SIDEBAR_WIDTH_MAX_CELLS, SIDEBAR_WIDTH_MIN_CELLS, SPLIT_DEPTH_MAX,
    SPLIT_WEIGHT_TOTAL, Sidebar, SidebarSide, SplitError, WINDOWS_MAX, WindowId, WindowLimits,
    WindowTree,
};
