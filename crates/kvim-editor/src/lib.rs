//! Modal editing state: cursors, motions, selections, operators, registers, and repeat.
//!
//! The crate is deterministic and pure. It reads no clock, no filesystem, and
//! no terminal. It depends on `core` and `settings` only, and it consumes the
//! semantic commands that `kvim-input` names. Every text change leaves the crate as
//! one [`EditTransaction`](kvim_core::EditTransaction).
//!
//! [`EditingState`] owns the mode, the operator-pending state, and the
//! description that `.` replays. [`WindowState`] owns the cursor, the selection
//! anchor, and the viewport, because one window owns its view into a buffer. The
//! system clipboard stays outside the crate: the caller passes the register
//! value in and out. See `docs/text-model.md`, `docs/input-actions.md`,
//! `docs/windows.md`, and `docs/clipboard.md`.
//!
//! # Examples
//!
//! ```
//! use std::num::NonZeroU16;
//!
//! use kvim_core::TextBuffer;
//! use kvim_editor::{
//!     CommandOutcome, EditContext, EditingState, RegisterShape, Registers, Viewport, WindowState,
//! };
//! use kvim_input::Command;
//! use kvim_settings::{EditorSettings, FileSettings};
//!
//! let mut buffer = TextBuffer::from_text("fn main() {\n    let value = 1;\n}\n", &FileSettings::default())
//!     .expect("the text is small");
//! let settings = EditorSettings::default();
//! let mut registers = Registers::default();
//! let mut context = EditContext {
//!     buffer: &mut buffer,
//!     settings: &settings,
//!     search: None,
//!     registers: &mut registers,
//!     applied: Vec::new(),
//! };
//!
//! let rows = NonZeroU16::new(24).expect("the literal 24 is not zero");
//! let cells = NonZeroU16::new(80).expect("the literal 80 is not zero");
//! let mut window = WindowState::new(Viewport::new(rows, cells));
//! let mut state = EditingState::new();
//!
//! // `yy` yanks the current line, and `p` pastes it below.
//! state.apply(&mut context, &mut window, Command::YankOverMotion, None);
//! state.apply(&mut context, &mut window, Command::YankOverMotion, None);
//! assert_eq!(
//!     context.registers.unnamed().map(|value| value.shape()),
//!     Some(RegisterShape::Linewise),
//! );
//!
//! let outcome = state.apply(&mut context, &mut window, Command::PasteAfter, None);
//! assert_eq!(outcome, CommandOutcome::Changed);
//! assert_eq!(context.buffer.to_string(), "fn main() {\nfn main() {\n    let value = 1;\n}\n");
//! ```

mod cursor;
mod edit;
mod motion;
mod operator;
mod register;
mod search;
mod selection;
mod state;
mod viewport;
mod window;

#[cfg(test)]
mod edit_tests;
#[cfg(test)]
mod tests;

pub use cursor::{ColumnLimit, Cursor, PreferredColumn};
pub use edit::AutoIndent;
pub use motion::CharClass;
pub use operator::{MotionKind, Operator, motion_kind};
pub use register::{REGISTER_BYTES_MAX, RegisterShape, RegisterValue, Registers};
pub use search::{
    SEARCH_MATCHES_MAX, SEARCH_QUERY_CHARS_MAX, SEARCH_SCAN_BYTES_MAX, SearchDirection,
    SearchError, SearchQuery,
};
pub use selection::{AnchorPoint, ModeState, Selection};
pub use state::{
    CommandContext, CommandOutcome, EditContext, EditingState, INSERT_TEXT_BYTES_MAX,
    MOTION_COUNT_MAX,
};
pub use viewport::{Viewport, ViewportAlignment};
pub use window::WindowState;
