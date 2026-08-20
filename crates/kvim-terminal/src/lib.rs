//! Terminal lifecycle, raw mode, the alternate screen, enhanced keyboard reporting,
//! and normalized terminal events.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! The crate is the only place that uses crossterm. It converts raw terminal
//! events into the terminal-independent [`Key`] and [`TerminalEvent`] values that
//! the rest of kvim consumes. It holds no editor concept: modes, mappings, and
//! commands belong to `kvim-input`.
//!
//! [`TerminalSession`] owns the setup steps and restores them on every exit path,
//! including a panic. Restoration never depends on unwinding: the session
//! installs a panic hook that writes every [`RestoreStep`], because a panic
//! aborts without running a destructor on some platforms. [`TerminationSource`]
//! covers the remaining exit path: it reports the first termination signal of
//! the process, so the event loop leaves and the ordinary restore runs.
//! [`EventSource`] delivers normalized events. This crate produces events only.
//! The event loop in `tui` consumes them.

mod events;
mod key;
mod lifecycle;
mod signal;

use std::io;

use thiserror::Error;

pub use events::{EventSource, FocusChange, TerminalEvent, UNMAPPED_EVENT_SKIP_MAX};
pub use key::{Chord, Key, KeyCode};
pub use lifecycle::{CrosstermControl, CursorShape, RestoreStep, TerminalControl, TerminalSession};
pub use signal::{TerminationSignal, TerminationSource};

/// A failure of a terminal control step or of the terminal event stream.
#[derive(Debug, Error)]
pub enum TerminalError {
    /// A terminal setup or restore step failed.
    #[error("the terminal control step failed")]
    Control(#[source] io::Error),
    /// Reading the terminal event stream failed.
    #[error("the terminal event read failed")]
    Read(#[source] io::Error),
    /// The terminal sent [`UNMAPPED_EVENT_SKIP_MAX`] consecutive events without
    /// a normalized form. The source stays usable, so the caller may read again.
    #[error("the terminal sent too many events without a normalized form")]
    UnmappedEventBurst,
}
