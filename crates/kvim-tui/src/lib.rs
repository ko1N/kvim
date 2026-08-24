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
//! standalone editor, and `crates/kvim-tui/examples/embedded_editor.rs` is the
//! imperative shell of one embedded editor. See `docs/responsiveness.md` and
//! `docs/embedding.md`.
//!
//! # Public facades
//!
//! The crate publishes two facades, and every exported item belongs to one of
//! them. No other item leaves the crate.
//!
//! The embedded facade is the supported external package. It holds
//! [`EmbeddedEditor`], [`EmbeddedEditorBuilder`], [`EditorDriver`],
//! [`EditorAccess`], [`EditorCapacity`], [`EditorInstanceId`], [`EditorEvent`],
//! [`PublishedEvent`], [`Reduction`], [`ReductionOutcome`], [`Refusal`],
//! [`Saturated`], [`CursorRequest`], [`CursorShape`], [`GeometryError`],
//! [`InputRequest`], [`EditorShutdown`], [`EditorDrain`], [`ShutdownDrain`],
//! [`Completed`], [`EditorWork`], [`ClipboardAccess`], the forwarded `kvim-ui`
//! geometry values, and the bounds of each one. A host composes one editor from
//! these values alone. `crates/kvim-tui/examples/embedded_editor.rs` uses
//! nothing else.
//!
//! The standalone facade serves the `kvim` binary, which is the terminal
//! adapter of this repository. It holds [`Session`], [`Windows`],
//! [`AdaptiveSplit`], [`WindowOutcome`], [`Redraw`], [`RunState`],
//! [`AnalysisRequest`], [`AnalysisResult`], [`Message`], [`MessageLevel`],
//! [`Theme`], [`ThemeRole`], [`IconRole`], [`HostWorkspace`],
//! [`HostReportRequest`], [`GENERATED_NAMES`], the language request values, and
//! the failure values that the message line reports. These items compose one
//! whole editor application. An external host composes its own workspace
//! instead and uses the embedded facade.
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

// The crate is one supported external package. Every published item names
// its own contract, so no implementation API can reach a consumer by accident.
#![deny(missing_docs)]

mod buffer_view;
mod cells;
mod chrome;
mod clipboard;
mod completion;
mod diagnostics;
mod driver;
mod embed;
mod icons;
mod jumps;
mod language;
mod log;
mod markup;
mod notify;
mod overlay;
mod picker;
mod render;
mod session;
mod theme;
mod tree;
mod window;

#[cfg(test)]
mod embed_tests;
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

pub use clipboard::ClipboardAccess;
pub use diagnostics::{HOST_PROGRAMS_MAX, HostReportRequest, HostWorkspace};
pub use driver::{Completed, EditorDriver, EditorWork, ShutdownDrain};
pub use embed::{
    CursorRequest, CursorShape, EDITOR_EVENTS_MAX, EditorAccess, EditorCapacity, EditorDrain,
    EditorEvent, EditorInstanceId, EditorShutdown, EmbeddedEditor, EmbeddedEditorBuilder,
    GeometryError, InputRequest, PublishedEvent, Reduction, ReductionOutcome, Refusal, Saturated,
};
pub use kvim_ui::{
    CloseOutcome, Direction, LayoutChange, LayoutFit, Orientation, Region, RegionKind,
    SIDEBAR_WIDTH_MAX_CELLS, SIDEBAR_WIDTH_MIN_CELLS, SPLIT_DEPTH_MAX, SPLIT_WEIGHT_TOTAL, Sidebar,
    SidebarSide, SplitError, WINDOWS_MAX, WindowId, WindowLayout,
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
pub use window::{AdaptiveSplit, WindowOutcome, Windows};
