//! The system clipboard boundary.
//!
//! [`SystemClipboard`] is the boundary trait. One implementation exists for each
//! platform, plus [`NoClipboard`] for a host without any clipboard command and
//! [`MemoryClipboard`] for tests. [`detect_system_clipboard`] selects the
//! implementation once at startup, and the composition root injects the result.
//! No module above this one names a clipboard command or reads the platform.
//!
//! [`Clipboard`] holds the rules above the boundary: the transfer bound, the
//! shape rule, and the failure reports. It holds no register value, because the
//! `editor` module owns the registers. A clipboard failure therefore never
//! removes editor data. See `docs/clipboard.md`.
//!
//! # Examples
//!
//! ```
//! use kvim::clipboard::{
//!     Clipboard, ClipboardNotice, ClipboardRead, ClipboardShape, ClipboardValue, MemoryClipboard,
//!     NoClipboard,
//! };
//!
//! // A session without any clipboard command stays fully usable, and the
//! // missing command is reported once for each session.
//! let mut absent = Clipboard::new(Box::new(NoClipboard));
//! let value = ClipboardValue { text: "alpha", shape: ClipboardShape::Characterwise };
//! assert_eq!(absent.copy(value), Some(ClipboardNotice::NoCommand));
//! assert_eq!(absent.copy(value), None);
//!
//! // A working clipboard keeps the shape that Kvim recorded.
//! let mut clipboard = Clipboard::new(Box::new(MemoryClipboard::default()));
//! let lines = ClipboardValue { text: "one\n", shape: ClipboardShape::Linewise };
//! assert_eq!(clipboard.copy(lines), None);
//! match clipboard.paste() {
//!     ClipboardRead::Value(value) => assert_eq!(value.shape, ClipboardShape::Linewise),
//!     ClipboardRead::Fallback(notice) => panic!("the memory clipboard never fails: {notice:?}"),
//! }
//! ```

use thiserror::Error;

mod system;

#[cfg(test)]
mod tests;

pub use system::{
    DisplaySession, LinuxClipboard, LinuxTool, MacOsClipboard, MemoryClipboard, NoClipboard,
    ProcessExecutor, SystemClipboard, detect_system_clipboard, program_on_path, select_linux_tool,
};

/// The largest text that Kvim moves across the system clipboard, in bytes.
///
/// A larger register value stays inside the editor. The bound also limits the
/// output that one clipboard read accepts.
pub const CLIPBOARD_BYTES_MAX: usize = 1024 * 1024;

/// The shape of one clipboard value.
///
/// The value mirrors the register shape of the `editor` module. The clipboard
/// keeps its own type, because the dependency direction lets no boundary module
/// reach into the editor. The composition root converts between the two.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardShape {
    /// A run of characters.
    Characterwise,
    /// Complete lines.
    Linewise,
    /// A rectangle of columns.
    Blockwise,
}

/// One borrowed value on its way to the system clipboard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClipboardValue<'a> {
    /// The text that the clipboard receives.
    pub text: &'a str,
    /// The shape that the editor recorded for the text.
    pub shape: ClipboardShape,
}

/// One value that the system clipboard returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedClipboardValue {
    /// The clipboard text.
    pub text: String,
    /// The shape that the boundary rule decided.
    pub shape: ClipboardShape,
}

/// The reason that one clipboard operation produced no value.
///
/// Every reason is an expected runtime state. None of them loses editor data.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ClipboardFailure {
    /// The host provides no clipboard command.
    #[error("the host provides no clipboard command")]
    Unavailable,
    /// The command did not start.
    #[error("the clipboard command did not start")]
    NotStarted,
    /// The command passed its deadline.
    #[error("the clipboard command passed its deadline")]
    Timeout,
    /// The command was cancelled or superseded.
    #[error("the clipboard command was cancelled")]
    Cancelled,
    /// The background service refused the command.
    #[error("the background service refused the clipboard command")]
    Refused,
    /// The command ran and reported a failure.
    #[error("the clipboard command reported a failure")]
    Failed,
    /// The clipboard holds bytes that are not UTF-8 text.
    #[error("the system clipboard does not hold UTF-8 text")]
    NotText,
}

/// The message that the editor shows for one clipboard operation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ClipboardNotice {
    /// No clipboard command exists on this host.
    ///
    /// The editor reports this notice once for each session, not once for each
    /// operation.
    #[error("no system clipboard command is available; the editor register still holds the value")]
    NoCommand,
    /// The clipboard command ran and failed.
    #[error("the system clipboard command failed; the editor register still holds the value")]
    CommandFailed,
    /// The value passes [`CLIPBOARD_BYTES_MAX`].
    #[error(
        "the value holds {bytes} bytes, which passes the {CLIPBOARD_BYTES_MAX} byte clipboard bound"
    )]
    TooLarge {
        /// The size of the rejected value, in bytes.
        bytes: usize,
    },
    /// The clipboard holds bytes that are not UTF-8 text.
    #[error("the system clipboard does not hold UTF-8 text")]
    NotText,
}

/// The result of one clipboard read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardRead {
    /// The system clipboard holds this value.
    Value(OwnedClipboardValue),
    /// The read produced no value. The caller keeps the internal register.
    Fallback(Option<ClipboardNotice>),
}

/// The one-time report for a missing clipboard command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MissingReport {
    /// The editor has not reported the missing command yet.
    Pending,
    /// The editor reported the missing command in this session.
    Delivered,
}

/// The text and the shape that Kvim wrote to the clipboard last.
#[derive(Clone, Debug, Eq, PartialEq)]
struct WrittenValue {
    text: String,
    shape: ClipboardShape,
}

/// The clipboard rules of one editor session.
///
/// The value owns the selected [`SystemClipboard`] and the text that Kvim wrote
/// last. It holds no register value.
#[derive(Debug)]
pub struct Clipboard {
    system: Box<dyn SystemClipboard>,
    last_written: Option<WrittenValue>,
    missing: MissingReport,
}

impl Clipboard {
    /// Creates the boundary over one system clipboard implementation.
    #[must_use]
    pub fn new(system: Box<dyn SystemClipboard>) -> Self {
        Self {
            system,
            last_written: None,
            missing: MissingReport::Pending,
        }
    }

    /// Selects the clipboard of this host and creates the boundary.
    ///
    /// The selection runs once at startup. It never guesses per operation.
    #[must_use]
    pub fn detect<E>(executor: E) -> Self
    where
        E: ProcessExecutor + 'static,
    {
        Self::new(detect_system_clipboard(executor))
    }

    /// Writes one register value to the system clipboard.
    ///
    /// Returns the notice that the editor shows, or `None` when the transfer
    /// succeeded or when the missing command was already reported. The caller
    /// keeps its register value in every case, so a failed write loses nothing.
    pub fn copy(&mut self, value: ClipboardValue<'_>) -> Option<ClipboardNotice> {
        if value.text.len() > CLIPBOARD_BYTES_MAX {
            return Some(ClipboardNotice::TooLarge {
                bytes: value.text.len(),
            });
        }

        match self.system.write(value.text) {
            Ok(()) => {
                self.last_written = Some(WrittenValue {
                    text: value.text.to_owned(),
                    shape: value.shape,
                });
                None
            }
            // A failed write leaves the clipboard with unknown content, so the
            // recorded value no longer describes it.
            Err(failure) => {
                self.last_written = None;
                self.notice_for(failure)
            }
        }
    }

    /// Reads the system clipboard and decides the shape of the text.
    ///
    /// Text that equals the last Kvim write keeps the recorded shape. Any other
    /// text comes from another application, so it is characterwise, or linewise
    /// when it ends with a line ending. A failed read falls back to the internal
    /// register of the caller.
    pub fn paste(&mut self) -> ClipboardRead {
        let text = match self.system.read() {
            Ok(text) => text,
            Err(failure) => return ClipboardRead::Fallback(self.notice_for(failure)),
        };
        if text.len() > CLIPBOARD_BYTES_MAX {
            return ClipboardRead::Fallback(Some(ClipboardNotice::TooLarge { bytes: text.len() }));
        }

        let shape = match &self.last_written {
            Some(written) if written.text == text => written.shape,
            _ if text.ends_with('\n') => ClipboardShape::Linewise,
            _ => ClipboardShape::Characterwise,
        };
        ClipboardRead::Value(OwnedClipboardValue { text, shape })
    }

    /// Converts one failure into the notice that the editor shows.
    ///
    /// A missing command is reported once for each session. Every other failure
    /// is reported for each operation, because it can pass.
    fn notice_for(&mut self, failure: ClipboardFailure) -> Option<ClipboardNotice> {
        match failure {
            ClipboardFailure::Unavailable => match self.missing {
                MissingReport::Pending => {
                    self.missing = MissingReport::Delivered;
                    Some(ClipboardNotice::NoCommand)
                }
                MissingReport::Delivered => None,
            },
            ClipboardFailure::NotText => Some(ClipboardNotice::NotText),
            ClipboardFailure::NotStarted
            | ClipboardFailure::Timeout
            | ClipboardFailure::Cancelled
            | ClipboardFailure::Refused
            | ClipboardFailure::Failed => Some(ClipboardNotice::CommandFailed),
        }
    }
}
