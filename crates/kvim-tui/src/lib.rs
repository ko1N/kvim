//! The window tree, layout, rendering, the theme, and the visible editor state.
//!
//! The crate is the sole owner of visible editor state.
//!
//! With the default `editor` feature, `Windows` owns the split tree, the
//! sidebars, the focus, and one cached `WindowLayout`. Both are pure values.
//! They read no clock, filesystem, or terminal. They hold no buffer text or
//! color. One layout calculation converts the tree and terminal rectangle into
//! rectangle of every visible region. No other code computes a rectangle. See
//! `docs/windows.md`.
//!
//! With the default `editor` feature, `Session` owns the visible editor state
//! and applies one pure transition for each normalized terminal event. `Theme`
//! maps a semantic role to one terminal style, so no call site names a color.
//! The crate owns no terminal and no event loop. The `kvim` binary is the
//! standalone editor, and `crates/kvim-embed/examples/worktree_editor.rs` is the
//! imperative shell of one embedded editor. See `docs/responsiveness.md` and
//! `docs/embedding.md`.
//!
//! The crate provides internal presentation for `kvim-embed` and focused
//! lower-level components. Its `__private` module is a non-contract adapter
//! seam. External high-level hosts use `kvim-embed`.
//!
//! # Editor example
//!
//! Run `cargo run -p kvim-ui --example split_windows` for a maintained layout
//! example. The default `editor` feature publishes the complete editor API.

// The crate is one supported external package. Every published item names
// its own contract, so no implementation API can reach a consumer by accident.
#![deny(missing_docs)]

#[cfg(feature = "editor")]
mod buffer_view;
#[cfg_attr(not(feature = "editor"), allow(dead_code))]
mod cells;
#[cfg(feature = "review")]
mod changes;
#[cfg(feature = "editor")]
mod chrome;
#[cfg(feature = "editor")]
mod clipboard;
#[cfg(feature = "editor")]
mod completion;
#[cfg(feature = "editor")]
mod diagnostics;
#[cfg(feature = "review")]
mod diff_view;
#[cfg(feature = "editor")]
mod driver;
#[cfg(feature = "editor")]
mod embed;
#[cfg(feature = "editor")]
mod file_sidebar;
#[cfg(feature = "editor")]
mod icons;
#[cfg(feature = "editor")]
mod jumps;
#[cfg(feature = "editor")]
mod language;
#[cfg(feature = "editor")]
mod log;
#[cfg(feature = "editor")]
mod markup;
#[cfg(feature = "editor")]
mod notify;
#[cfg(feature = "editor")]
mod overlay;
#[cfg(feature = "editor")]
mod picker;
#[cfg(feature = "editor")]
mod pointer;
#[cfg(feature = "editor")]
mod render;
#[cfg(feature = "review")]
mod review;
#[cfg(feature = "editor")]
mod session;
#[cfg(feature = "review")]
mod theme;
#[cfg(feature = "editor")]
mod tree;
#[cfg(feature = "editor")]
mod window;

#[cfg(all(test, feature = "editor"))]
mod tests;

#[cfg(feature = "editor")]
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
    pub use crate::file_sidebar::{
        FILE_SIDEBAR_LABEL_CHARS_MAX, FILE_SIDEBAR_ROWS_MAX, FileRow, FileRowDimming, FileRowGit,
        FileRowIdentity, FileRowKind, FileRowNoticeKind, FileSidebarInput, FileSidebarOutcome,
    };
    pub use crate::review::{
        ReviewFocus, ReviewModel, ReviewOutcome, ReviewPainter, ReviewRestore,
    };
    pub use crate::session::{
        EditorDiagnosticSummary, EditorFormatterStatus, EditorStatus, RecoveryDecision,
        RecoveryDecisionError, RecoveryIdentity, RecoveryStatus, Redraw, RunState, Session,
    };
    pub use crate::theme::{IconRole, Theme};
    pub use crate::tree::{
        FILE_SIDEBAR_DEPTH_MAX, FILE_SIDEBAR_ROOT_LABEL_BYTES_MAX, GENERATED_NAMES,
    };
    pub use crate::{Direction, ListMotion, RegionFocus, draw_file_row};
    pub use kvim_terminal::TerminalEvent;
}

#[cfg(feature = "review")]
#[doc(hidden)]
pub mod __review {
    //! Internal pure-review adapter seam for `kvim-embed`.

    pub use crate::review::{
        PanelPlacement, PanelRow, ReviewFocus, ReviewModel, ReviewOutcome, ReviewPainter,
        ReviewPanelGitState, ReviewPanelRowId, ReviewPanelSection, ReviewPanelSectionKind,
        ReviewPanelSnapshot, ReviewRestore,
    };
    pub use crate::theme::Theme;
}

#[cfg(feature = "editor")]
pub use buffer_view::RegionFocus;
#[cfg(feature = "editor")]
pub use clipboard::ClipboardAccess;
#[cfg(feature = "editor")]
pub use completion::{
    COMPLETION_CANDIDATES_MAX, COMPLETION_COLUMNS_MAX, COMPLETION_ROWS_MAX, CompletionCycle,
    CompletionOutcome, LineCompletion, draw_completion_menu,
};
#[cfg(feature = "editor")]
pub use diagnostics::{HOST_PROGRAMS_MAX, HostReportRequest, HostWorkspace};
#[cfg(feature = "editor")]
pub use file_sidebar::{
    FILE_SIDEBAR_ICON_CELLS, FILE_SIDEBAR_LABEL_CHARS_MAX, FILE_SIDEBAR_LINK_SUFFIX,
    FILE_SIDEBAR_MARK_CELLS, FILE_SIDEBAR_ROWS_MAX, FILE_SIDEBAR_SELECTION_MARK, FileRow,
    FileRowGit, FileRowKind, FileSidebarInput, FileSidebarOutcome, draw_file_row,
};
#[cfg(feature = "editor")]
pub use kvim_ui::{
    AdaptiveSplit, CloseOutcome, Direction, LayoutChange, LayoutFit, ListMotion, Orientation,
    Region, RegionKind, SIDEBAR_WIDTH_MAX_CELLS, SIDEBAR_WIDTH_MIN_CELLS, SPLIT_DEPTH_MAX,
    SPLIT_WEIGHT_TOTAL, Sidebar, SidebarSide, SplitError, WINDOWS_MAX, WindowId, WindowLayout,
};
#[cfg(feature = "editor")]
pub use language::{
    DiagnosticJump, FLOAT_COLUMNS_MAX, FLOAT_ROWS_MAX, FormatOnSave, LANGUAGE_OUTBOX_MAX,
    LanguageQuery, LanguageRequest, LanguageRequestKind,
};
#[cfg(feature = "editor")]
pub use picker::PickerFailure;
#[cfg(feature = "editor")]
pub use session::{
    AnalysisRequest, AnalysisResult, FileRequestFailure, HostProbeFailure, MESSAGE_CHARS_MAX,
    Message, MessageLevel, Redraw, RunState, Session,
};
#[cfg(feature = "editor")]
pub use theme::{IconRole, Theme, ThemeRole};
#[cfg(feature = "editor")]
pub use tree::GENERATED_NAMES;
#[cfg(feature = "editor")]
pub use window::{WindowOutcome, Windows};
