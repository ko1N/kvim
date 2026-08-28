//! The window tree, layout, rendering, the theme, and the visible editor state.
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
//! terminal style, so no call site names a color. The crate owns no terminal
//! and no event loop: the `kvim` binary is the imperative shell of the
//! standalone editor, and `crates/kvim-embed/examples/worktree_editor.rs` is the
//! imperative shell of one embedded editor. See `docs/responsiveness.md` and
//! `docs/embedding.md`.
//!
//! The crate provides internal presentation for `kvim-embed` and focused
//! lower-level components. Its `__private` module is a non-contract adapter
//! seam. External high-level hosts use `kvim-embed`.
//!
//! # Example
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

// The crate is one supported external package. Every published item names
// its own contract, so no implementation API can reach a consumer by accident.
#![deny(missing_docs)]

mod buffer_view;
mod cells;
mod changes;
mod chrome;
mod clipboard;
mod completion;
mod diagnostics;
mod diff_view;
mod driver;
mod embed;
mod file_sidebar;
mod icons;
mod jumps;
mod language;
mod log;
mod markup;
mod notify;
mod overlay;
mod picker;
mod render;
mod review;
mod session;
mod theme;
mod tree;
mod window;

#[cfg(test)]
mod tests;

#[doc(hidden)]
pub mod __private {
    //! Internal adapter seams for `kvim-embed`.
    //!
    //! These exports are not a supported host contract. They can change or
    //! disappear without compatibility guarantees.

    pub use crate::clipboard::ClipboardAccess;
    pub use crate::diagnostics::{HostReportRequest, HostWorkspace};
    pub use crate::driver::{Completed, EditorDriver, EditorWork};
    pub use crate::embed::{
        CursorShape, EDITOR_EVENTS_MAX, EditorAccess, EditorCapacity, EditorDrain, EditorEvent,
        EditorInstanceId, EditorOpenError, EditorPresentation, EditorShutdown, EmbeddedEditor,
        GeometryError, InputRequest, PublishedEvent, Reduction, ReductionOutcome, Refusal,
    };
    pub use crate::file_sidebar::{FileSidebarInput, FileSidebarOutcome};
    pub use crate::session::{
        EditorDiagnosticSummary, EditorFormatterStatus, EditorStatus, Redraw, RunState, Session,
    };
    pub use crate::theme::Theme;
    pub use crate::tree::GENERATED_NAMES;
    pub use crate::{Direction, ListMotion, RegionFocus, draw_file_row};
    pub use kvim_terminal::TerminalEvent;
}

pub use buffer_view::RegionFocus;
pub use clipboard::ClipboardAccess;
pub use completion::{
    COMPLETION_CANDIDATES_MAX, COMPLETION_COLUMNS_MAX, COMPLETION_ROWS_MAX, CompletionCycle,
    CompletionOutcome, LineCompletion, draw_completion_menu,
};
pub use diagnostics::{HOST_PROGRAMS_MAX, HostReportRequest, HostWorkspace};
pub use file_sidebar::{
    FILE_SIDEBAR_ICON_CELLS, FILE_SIDEBAR_LABEL_CHARS_MAX, FILE_SIDEBAR_LINK_SUFFIX,
    FILE_SIDEBAR_MARK_CELLS, FILE_SIDEBAR_ROWS_MAX, FILE_SIDEBAR_SELECTION_MARK, FileRow,
    FileRowGit, FileRowKind, FileSidebarInput, FileSidebarOutcome, draw_file_row,
};
pub use kvim_ui::{
    AdaptiveSplit, CloseOutcome, Direction, LayoutChange, LayoutFit, ListMotion, Orientation,
    Region, RegionKind, SIDEBAR_WIDTH_MAX_CELLS, SIDEBAR_WIDTH_MIN_CELLS, SPLIT_DEPTH_MAX,
    SPLIT_WEIGHT_TOTAL, Sidebar, SidebarSide, SplitError, WINDOWS_MAX, WindowId, WindowLayout,
};
pub use language::{
    DiagnosticJump, FLOAT_COLUMNS_MAX, FLOAT_ROWS_MAX, FormatOnSave, LANGUAGE_OUTBOX_MAX,
    LanguageQuery, LanguageRequest, LanguageRequestKind,
};
pub use picker::PickerFailure;
pub use session::{
    AnalysisRequest, AnalysisResult, CONFIRM_ANSWER_CHARS_MAX, FileRequestFailure,
    HostProbeFailure, MESSAGE_CHARS_MAX, Message, MessageLevel, Redraw, RunState, Session,
};
pub use theme::{IconRole, Theme, ThemeRole};
pub use tree::GENERATED_NAMES;
pub use window::{WindowOutcome, Windows};
