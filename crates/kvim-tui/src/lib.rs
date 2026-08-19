//! The window tree, layout, rendering, the theme, and the event loop.
//!
//! The crate is the sole owner of visible editor state.
//!
//! [`Windows`] owns the split tree, the sidebars, the focus, and one cached
//! [`WindowLayout`]. Both are pure values: they read no clock, no filesystem,
//! and no terminal, and they hold no buffer text and no color. One layout
//! calculation converts the tree and the terminal rectangle into the exact
//! rectangle of every visible region. No other code computes a rectangle. See
//! `docs/windows.md`.
//!
//! [`Session`] owns the visible editor state and applies one pure transition
//! for each normalized terminal event. [`Theme`] maps a semantic role to one
//! terminal style, so no call site names a color. [`run`] is the imperative
//! shell: it owns the terminal, reads events, and renders only after a visible
//! state change. See `docs/responsiveness.md`.
//!
//! # Examples
//!
//! ```
//! use ratatui::layout::Rect;
//!
//! use kvim_settings::WindowSettings;
//! use kvim_tui::{AdaptiveSplit, Direction, LayoutChange, Orientation, Windows};
//! use kvim_workspace::BufferId;
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

mod app;
mod buffer_view;
mod cells;
mod chrome;
mod clipboard;
mod completion;
mod icons;
mod language;
mod layout;
mod notify;
mod overlay;
mod picker;
mod render;
mod session;
mod theme;
mod tree;
mod window;

#[cfg(test)]
mod language_tests;
#[cfg(test)]
mod picker_tests;
#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tree_tests;

pub use app::{EVENT_ERRORS_MAX, EditorError, PanicProbe, run};
pub use language::{
    DiagnosticJump, FLOAT_COLUMNS_MAX, FLOAT_ROWS_MAX, FormatOnSave, LANGUAGE_OUTBOX_MAX,
    LanguageQuery, LanguageRequest, LanguageRequestKind,
};
pub use layout::{Region, RegionKind, WindowLayout};
pub use picker::PickerFailure;
pub use session::{
    AnalysisRequest, AnalysisResult, FileRequestFailure, MESSAGE_CHARS_MAX, Message, MessageLevel,
    Redraw, RunState, Session,
};
pub use theme::{IconRole, Theme, ThemeRole};
pub use window::{
    AdaptiveSplit, CloseOutcome, Direction, LayoutChange, Orientation, SIDEBAR_WIDTH_MAX_CELLS,
    SIDEBAR_WIDTH_MIN_CELLS, SPLIT_DEPTH_MAX, SPLIT_WEIGHT_TOTAL, Sidebar, SidebarSide, SplitError,
    WINDOWS_MAX, WindowId, WindowOutcome, Windows,
};
