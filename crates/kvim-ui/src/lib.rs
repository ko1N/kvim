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

mod layout;
mod sidebar;
mod which_key;
mod window;

#[cfg(test)]
mod sidebar_tests;
#[cfg(test)]
mod which_key_tests;
#[cfg(test)]
mod window_tests;

pub use layout::{Region, RegionKind, WindowLayout};
pub use sidebar::{
    RowKind, SIDEBAR_ACTION_CHARS_MAX, SIDEBAR_LABEL_CHARS_MAX, SIDEBAR_ROW_DRAWS_MAX,
    SIDEBAR_ROW_LINES_MAX, SIDEBAR_ROWS_MAX, SidebarAction, SidebarCanvas, SidebarError,
    SidebarEvent, SidebarInput, SidebarMotion, SidebarPlacement, SidebarRow, SidebarState,
};
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
