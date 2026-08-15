//! The window tree, layout, rendering, the theme, and the event loop.
//!
//! The module is the sole owner of visible editor state.
//!
//! [`Windows`] owns the split tree, the sidebars, the focus, and one cached
//! [`WindowLayout`]. Both are pure values: they read no clock, no filesystem,
//! and no terminal, and they hold no buffer text and no color. One layout
//! calculation converts the tree and the terminal rectangle into the exact
//! rectangle of every visible region. No other code computes a rectangle. See
//! `docs/windows.md`.
//!
//! Rendering, the theme, and the event loop arrive in Slice 8.
//!
//! # Examples
//!
//! ```
//! use ratatui::layout::Rect;
//!
//! use kvim::settings::WindowSettings;
//! use kvim::tui::{AdaptiveSplit, BufferId, Direction, LayoutChange, Orientation, Windows};
//!
//! let mut windows = Windows::new(
//!     BufferId::new(1),
//!     Rect::new(0, 0, 120, 40),
//!     WindowSettings::default(),
//! );
//!
//! // A terminal that holds one window always splits vertically.
//! assert_eq!(windows.adaptive_orientation(AdaptiveSplit::Normal), Orientation::Vertical);
//! let right = windows.split_adaptive(AdaptiveSplit::Normal).expect("the terminal is wide");
//! assert_eq!(windows.focused_window(), right);
//!
//! // The resize command names the direction that the edge moves. The focused
//! // window sits on the right, so its right edge holds no neighbor and the
//! // command moves the left edge left, which grows the window.
//! let before = windows.layout().area(right).expect("the window is visible");
//! assert_eq!(windows.resize(Direction::Left), LayoutChange::Changed);
//! let after = windows.layout().area(right).expect("the window is visible");
//! assert_eq!(after.width, before.width + 6);
//! ```

mod layout;
mod window;

#[cfg(test)]
mod tests;

pub use layout::{Region, RegionKind, WindowLayout};
pub use window::{
    AdaptiveSplit, BufferId, CloseOutcome, Direction, LayoutChange, Orientation,
    SIDEBAR_WIDTH_MAX_CELLS, SIDEBAR_WIDTH_MIN_CELLS, SPLIT_DEPTH_MAX, SPLIT_WEIGHT_TOTAL, Sidebar,
    SidebarSide, SplitError, WINDOWS_MAX, WindowId, WindowOutcome, Windows,
};
