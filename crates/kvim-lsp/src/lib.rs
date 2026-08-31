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
//! [`ProjectManager`] opens several projects. The caller names each
//! [`ProjectId`], so two projects on one root stay independent. Opening returns
//! one [`ProjectHandle`] and one [`ProjectDriver`], and the host runs that
//! driver future. The crate creates no runtime and detaches no task.
//!
//! [`ServerSupervisor`] owns the bounded restart loop of one server. It records
//! every step as one [`ProjectEvent`], and every event carries project identity
//! and server identity.
//!
//! [`DiagnosticsHub`] serves the changed-file diagnostics of one project. The
//! caller supplies one validated path, the exact text of one revision, and one
//! wait policy. [`WaitPolicy::Until`] keeps that exact request alive through
//! server startup and diagnostic completion, so the caller needs no watcher, no
//! polling, and no resubmission. See
//! `crates/kvim-lsp/examples/lsp_diagnostics.rs`.
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

// The crate is one supported external package. Every published item names
// its own contract, so no implementation API can reach a consumer by accident.
#![deny(missing_docs)]

mod diagnostics;
mod document;
mod encoding;
mod process;
mod project;
mod protocol;

pub use diagnostics::{
    ChangedFile, ChangedFileReport, CompletionPolicy, DiagnosticsConversation, DiagnosticsHub,
    DiagnosticsLimits, DiagnosticsOutcome, DiagnosticsServer, DocumentRevision,
    LSP_DIAGNOSTIC_DEADLINE, LSP_DIAGNOSTIC_MESSAGE_BYTES_MAX, LSP_DIAGNOSTICS_MAX,
    LSP_DOCUMENT_BYTES_MAX, LSP_LANGUAGE_BYTES_MAX, LSP_MERGED_DIAGNOSTICS_MAX,
    LSP_RELATED_INFORMATION_MAX, LSP_REQUEST_BYTES_MAX, LSP_SERVER_LANGUAGES_MAX,
    LSP_SERVER_SOURCE_BYTES_MAX, LanguageId, RelatedInformation, ReportedDiagnostic,
    RevisionPolicy, ServerDiagnostics, ServerOutcome, Truncation, WaitPolicy,
};
pub use document::{
    ContentChange, Diagnostic, DiagnosticSeverity, RawDiagnostic, RawTextEdit, SourceLocation,
    TextEdit,
};
pub use encoding::{DocumentMapping, DocumentMirror, PositionEncoding, TextMirroring};
pub use process::{
    DefaultServerLauncher, DiagnosticsModel, Envelopes, Handshake, HandshakeOutcome,
    LSP_ENVELOPE_QUEUE_CAPACITY, LSP_INITIALIZE_DEADLINE, LSP_RESTARTS_MAX,
    LSP_RESULT_ID_BYTES_MAX, LSP_SERVER_ARGUMENT_BYTES_MAX, LSP_SERVER_ARGUMENTS_MAX,
    LSP_SERVER_COMMAND_BYTES_MAX, LSP_SERVER_PROGRAM_BYTES_MAX, LSP_SHUTDOWN_DEADLINE,
    LSP_STDERR_BYTES_MAX, LSP_STDERR_LINE_BYTES_MAX, LaunchedServer, ServerCapabilities,
    ServerInput, ServerLaunchError, ServerLaunchRequest, ServerLauncher, ServerProcess,
    ServerProcessHandle, ServerReport, ServerStreams, ServerTerminate, ServerTerminateError,
    ServerWait, ServerWaitError, SynchronizationMode, Transport, TransportFactory, initialize,
    shutdown,
};
pub use project::{
    Attempt, AttemptEnd, LSP_EVENT_QUEUE_CAPACITY, LSP_MANAGER_DOCUMENTS_MAX,
    LSP_MANAGER_PROCESSES_MAX, LSP_MANAGER_QUEUE_CAPACITY_MAX, LSP_OPEN_DOCUMENTS_MAX,
    LSP_PROJECT_CLOSE_DEADLINE, LSP_PROJECTS_MAX, LSP_SESSIONS_MAX, ManagerLimits,
    ProjectDeclaration, ProjectDriver, ProjectEvent, ProjectEvents, ProjectHandle, ProjectId,
    ProjectManager, ProjectServer, RequestKey, ServerAddress, ServerConversation,
    ServerDeclaration, ServerEvent, ServerId, ServerSupervisor, SessionGeneration,
};
pub use protocol::{
    ArrayBudget, DocumentPosition, LSP_HEADER_BYTES_MAX, LSP_INPUT_BYTES_MAX,
    LSP_MESSAGE_BYTES_MAX, LSP_MESSAGES_MAX, LSP_OUTPUT_BYTES_MAX, LSP_REQUESTS_MAX, LspBound,
    LspError, ProtocolPosition, ProtocolSpan, ProtocolWriter, RpcEnvelope, RpcId, RpcResponseError,
    SourceSpan, WorkspaceRoot, deserialize_bounded_array, enforce, read_frame,
};
