//! Neutral Language Server Protocol (LSP) values, framing, and bounds.
//!
//! The crate speaks the protocol and nothing else. It knows no language, no
//! server product, no syntax tree, no settings file, and no editor. A consumer
//! supplies the program, the arguments, the language identifier, and the
//! initialization options as data.
//!
//! Every read and every write passes one bound. [`enforce`] applies each bound,
//! so one rule holds for the frame header, the frame body, the bounded lists of
//! one answer, and the cumulative traffic of one session.
//!
//! Every column of this crate carries its unit in its type.
//! [`DocumentPosition`] counts UTF-8 bytes inside its line, and
//! [`ProtocolPosition`] counts the unit that the handshake negotiated. A
//! [`DocumentMapping`] converts between them against the exact text that the
//! server holds, so no conversion can split a character.
//!
//! [`ServerProcess`] owns one child process, its frame reader, and its
//! standard-error recorder. Dropping the value kills the child, so a cancelled
//! caller can leave no untracked process. [`initialize`] and [`shutdown`] each
//! carry their own deadline.
//!
//! [`WorkspaceRoot`] contains every served document. It names each contained
//! document as one `kvim_path::WorktreeRelativePath`, so one path rule holds
//! for the URI boundary and for the filesystem boundary.
//!
//! See `docs/language-services.md`.
//!
//! # Examples
//!
//! ```
//! use std::path::{Path, PathBuf};
//!
//! use kvim_lsp::{LspBound, LspError, WorkspaceRoot, enforce};
//!
//! let root = WorkspaceRoot::new(PathBuf::from("/work/project"))?;
//! assert_eq!(
//!     root.uri(Path::new("/work/project/src/main.rs"))?,
//!     "file:///work/project/src/main.rs"
//! );
//! assert!(root.uri(Path::new("/etc/passwd")).is_err());
//! assert!(enforce(9, 8, LspBound::Diagnostics).is_err());
//! # Ok::<(), LspError>(())
//! ```

mod document;
mod encoding;
mod process;
mod protocol;

pub use document::{
    ContentChange, Diagnostic, DiagnosticSeverity, RawDiagnostic, RawTextEdit, SourceLocation,
    TextEdit,
};
pub use encoding::{DocumentMapping, DocumentMirror, PositionEncoding, TextMirroring};
pub use process::{
    DiagnosticsModel, Envelopes, Handshake, HandshakeOutcome, LSP_ENVELOPE_QUEUE_CAPACITY,
    LSP_INITIALIZE_DEADLINE, LSP_RESTARTS_MAX, LSP_RESULT_ID_BYTES_MAX, LSP_SHUTDOWN_DEADLINE,
    LSP_STDERR_BYTES_MAX, LSP_STDERR_LINE_BYTES_MAX, ServerCapabilities, ServerInput,
    ServerProcess, ServerReport, ServerStreams, SynchronizationMode, Transport, TransportFactory,
    initialize, shutdown,
};
pub use protocol::{
    ArrayBudget, DocumentPosition, LSP_HEADER_BYTES_MAX, LSP_INPUT_BYTES_MAX,
    LSP_MESSAGE_BYTES_MAX, LSP_MESSAGES_MAX, LSP_OUTPUT_BYTES_MAX, LSP_REQUESTS_MAX, LspBound,
    LspError, ProtocolPosition, ProtocolReader, ProtocolSpan, ProtocolWriter, RpcEnvelope, RpcId,
    RpcResponseError, SourceSpan, WorkspaceRoot, deserialize_bounded_array, enforce, read_frame,
};
