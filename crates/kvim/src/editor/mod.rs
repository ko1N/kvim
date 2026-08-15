//! Modal editing state: cursors, motions, selections, search, and the viewport.
//!
//! The module is deterministic and pure. It reads no clock, no filesystem, and
//! no terminal. It depends on `core` and `settings` only, and it consumes the
//! semantic commands that `input` names. This slice changes no text: Slice 6
//! adds the operators, the registers, the paste commands, and dot-repeat.
//!
//! [`EditingState`] owns the cursor, the mode, and the selection anchor.
//! [`Viewport`] stays a separate value, because one window owns one viewport.
//! See `docs/text-model.md`, `docs/input-actions.md`, and `docs/windows.md`.
//!
//! # Examples
//!
//! ```
//! use std::num::NonZeroU16;
//!
//! use kvim::core::TextBuffer;
//! use kvim::editor::{CommandContext, EditingState, SearchDirection, SearchQuery, Viewport};
//! use kvim::input::Command;
//! use kvim::settings::{EditorSettings, FileSettings};
//!
//! let buffer = TextBuffer::from_text("fn main() {\n    let value = 1;\n}\n", &FileSettings::default())
//!     .expect("the text is small");
//! let settings = EditorSettings::default();
//! let query = SearchQuery::new("value", SearchDirection::Forward)
//!     .expect("the query holds one short line");
//! let context = CommandContext {
//!     buffer: &buffer,
//!     settings: &settings,
//!     search: Some(&query),
//! };
//!
//! let rows = NonZeroU16::new(24).expect("the literal 24 is not zero");
//! let cells = NonZeroU16::new(80).expect("the literal 80 is not zero");
//! let mut viewport = Viewport::new(rows, cells);
//! let mut state = EditingState::new(&buffer);
//!
//! state.apply(&context, &mut viewport, Command::SearchNext, None);
//! assert_eq!(state.cursor().line().get(), 1);
//! assert_eq!(state.cursor().column().get(), 8);
//!
//! // The buffer text is untouched.
//! assert_eq!(buffer.version().get(), 0);
//! ```

mod cursor;
mod motion;
mod search;
mod selection;
mod state;
mod viewport;

#[cfg(test)]
mod tests;

pub use cursor::{ColumnLimit, Cursor, PreferredColumn};
pub use motion::CharClass;
pub use search::{
    SEARCH_MATCHES_MAX, SEARCH_QUERY_CHARS_MAX, SEARCH_SCAN_BYTES_MAX, SearchDirection,
    SearchError, SearchQuery,
};
pub use selection::{BlockAnchor, ModeState, Selection};
pub use state::{CommandContext, CommandOutcome, EditingState, MOTION_COUNT_MAX};
pub use viewport::{Viewport, ViewportAlignment};
