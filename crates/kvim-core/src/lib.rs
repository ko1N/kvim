//! Deterministic text model: rope buffer, validated coordinates, edit transactions, undo and redo.
//!
//! The crate performs no input and no output. It depends on no other crate
//! except `settings`. A caller reads a file elsewhere and hands the text to
//! [`TextBuffer::from_text`].
//!
//! The crate keeps its parts in private modules and re-exports the public
//! items from this file. `ropey` is the text storage. The rope stays private, so
//! no other crate sees a rope type. See `docs/text-model.md` and the dependency
//! ledger in `docs/architecture.md`.
//!
//! # Examples
//!
//! ```
//! use kvim_core::{CharRange, EditTransaction, TextBuffer, TextChange};
//! use kvim_settings::FileSettings;
//!
//! let mut buffer = TextBuffer::from_text("let x = 1;\n", &FileSettings::default())
//!     .expect("the text is small");
//!
//! let cursor = buffer.char_position(8).expect("the position exists");
//! let end = buffer.char_position(9).expect("the position exists");
//! let range = CharRange::new(cursor, end).expect("the range ascends");
//! buffer
//!     .apply(EditTransaction::single(cursor, TextChange::replace(range, "42")))
//!     .expect("the range fits the buffer");
//! assert_eq!(buffer.to_string(), "let x = 42;\n");
//!
//! // One undo reverses one user-visible change and restores the cursor.
//! assert_eq!(buffer.undo(), Some(cursor));
//! assert_eq!(buffer.to_string(), "let x = 1;\n");
//! ```

mod buffer;
mod coordinates;
mod history;
mod indent;
mod transaction;

#[cfg(test)]
mod tests;

pub use buffer::{BufferVersion, EditError, FinalLineEnding, LineEnding, LoadError, TextBuffer};
pub use coordinates::{
    ByteOffset, CharPosition, CoordinateError, LineIndex, SourceColumn, TerminalColumn,
};
pub use history::{UNDO_HISTORY_BYTES_MAX, UNDO_HISTORY_ENTRIES_MAX};
pub use indent::{INDENT_COLUMNS_MAX, IndentPolicy, LineIndent, ShiftDirection};
pub use transaction::{
    CharRange, EditTransaction, TRANSACTION_CHANGES_MAX, TextChange, TransactionError,
};
