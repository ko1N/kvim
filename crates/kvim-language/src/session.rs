//! The persistent language-server session.
//! Adapted from ReviewGraph (MIT), src/analysis/lsp.rs.
//!
//! One session owns one server process for one language. It runs as one
//! background task, so the terminal event loop never reads, writes, or waits
//! for the server. The editor sends bounded requests through one queue and
//! receives typed results through another queue.
//!
//! The session speaks the Language Server Protocol only. The adapter supplies
//! the program, the arguments, the language identifier, and the initialization
//! options as data, so no code in this file names one server product.
//!
//! Every request and every published result carries the buffer version that
//! produced its input. A result for an obsolete version is rejected before
//! publication and never applied. See `docs/language-services.md`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;

use kvim_core::BufferVersion;
use kvim_settings::IndentSettings;
use kvim_syntax::SyntaxHighlighter;

use super::LanguageRegistry;
use super::document::{
    ContentChange, DiagnosticSet, FormatEdits, MarkupKind, MarkupText, RawDiagnostic, RawTextEdit,
    SourceLocation, TextEdit,
};
use super::encoding::{DocumentMapping, PositionEncoding, TextMirroring};
use super::markup::MarkupDocument;
use super::progress::{ProgressReport, SessionGeneration, parse as parse_progress};
use super::protocol::{
    ArrayBudget, DocumentPosition, LspBound, LspError, ProtocolReader, ProtocolSpan,
    ProtocolWriter, RpcEnvelope, RpcId, WorkspaceRoot, deserialize_bounded_array, enforce,
};
use super::server::{LanguageServerId, ServerFormatting};

/// The documents that one session holds open at the same time.
pub const LSP_OPEN_DOCUMENTS_MAX: usize = 64;

/// The requests of one session that wait for an answer at the same time.
pub const LSP_PENDING_REQUESTS_MAX: usize = 32;

/// The editor requests that one session queue holds.
pub const LSP_REQUEST_QUEUE_CAPACITY: usize = 64;

/// The results that the language service holds for the event loop.
pub const LSP_EVENT_QUEUE_CAPACITY: usize = 256;

/// The content changes of one document synchronization.
pub const LSP_CONTENT_CHANGES_MAX: usize = 4_096;

/// The diagnostics that one document publishes.
pub const LSP_DIAGNOSTICS_MAX: usize = 1_024;

/// The locations of one definition answer.
pub const LSP_LOCATIONS_MAX: usize = 128;

/// The edits of one formatting answer.
pub const LSP_FORMAT_EDITS_MAX: usize = 4_096;

/// The largest hover text that one answer may carry, in bytes.
pub const LSP_HOVER_BYTES_MAX: usize = 16 * 1024;

/// The bytes of the standard error of one server attempt that the editor
/// records.
///
/// A server that fails names its cause in its first lines, so this bound holds
/// that cause. The reader drains every further byte of that attempt and records
/// none of it. A server that writes without limit therefore still runs, and it
/// costs bounded memory. See `docs/language-services.md`.
pub const LSP_STDERR_BYTES_MAX: usize = 64 * 1024;

/// The bytes that one recorded standard error line keeps.
///
/// One line of a server log names one state. The editor log clips one entry
/// further, so this bound protects the reader from a stream that carries no
/// line break.
pub const LSP_STDERR_LINE_BYTES_MAX: usize = 1024;

/// The bytes that one read of the standard error takes.
///
/// The value is the size of one read buffer, not a bound on the recorded text.
/// [`LSP_STDERR_BYTES_MAX`] and [`LSP_STDERR_LINE_BYTES_MAX`] bound that text.
const STDERR_CHUNK_BYTES: usize = 4 * 1024;

/// The largest result identifier that one pulled report may carry, in bytes.
///
/// The session holds one identifier for each open document of a pull session,
/// so the bound decides what the session keeps. See
/// `docs/language-services.md`.
pub const LSP_RESULT_ID_BYTES_MAX: usize = 256;

/// The sections that one workspace configuration request may ask for.
///
/// The value matches [`LSP_OPEN_DOCUMENTS_MAX`], so a server may ask for every
/// open document at once and no more.
pub const LSP_CONFIGURATION_ITEMS_MAX: usize = LSP_OPEN_DOCUMENTS_MAX;

/// The restarts that one session performs after a server failure.
pub const LSP_RESTARTS_MAX: usize = 3;

/// The deadline of the `initialize` handshake.
pub const LSP_INITIALIZE_DEADLINE: Duration = Duration::from_secs(30);

/// The deadline of one definition or hover request.
pub const LSP_REQUEST_DEADLINE: Duration = Duration::from_secs(5);

/// The deadline of one document formatting request.
pub const LSP_FORMAT_DEADLINE: Duration = Duration::from_secs(10);

/// The deadline of one pulled diagnostics request.
///
/// A pull analyses the complete document, and a cold linter loads its
/// configuration first, so it takes the time of a formatter.
pub const LSP_DIAGNOSTIC_DEADLINE: Duration = Duration::from_secs(10);

/// The delay after which a change settles and the session pulls again.
///
/// A typist produces keystrokes far below this interval, so one burst of edits
/// starts one pull. See `docs/language-services.md`.
pub const LSP_DIAGNOSTIC_PULL_DELAY: Duration = Duration::from_millis(300);

/// The deadline of the `shutdown` and `exit` sequence.
pub const LSP_SHUTDOWN_DEADLINE: Duration = Duration::from_millis(250);

/// The notification that publishes the diagnostics of one document.
const DIAGNOSTICS_METHOD: &str = "textDocument/publishDiagnostics";

/// The notification that reports the state of one long server operation.
const PROGRESS_METHOD: &str = "$/progress";

/// The server request that creates one work-done progress token.
const PROGRESS_CREATE_METHOD: &str = "window/workDoneProgress/create";

/// The request that asks one server for the diagnostics of one document.
const DIAGNOSTIC_PULL_METHOD: &str = "textDocument/diagnostic";

/// The server request that asks the client to pull the diagnostics again.
const DIAGNOSTIC_REFRESH_METHOD: &str = "workspace/diagnostic/refresh";

/// The server request that asks the client for its workspace configuration.
const CONFIGURATION_METHOD: &str = "workspace/configuration";

/// The identity of one language-server request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LanguageRequestId(u64);

impl LanguageRequestId {
    /// Returns the underlying value for logs and comparisons.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One recorded fact about the server process of one session.
///
/// A report changes no buffer text, no cursor, and no message line. The editor
/// records it in its log, so a reader finds the cause of a failure that the
/// protocol never names. See `docs/windows.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerReport {
    /// The handshake completed, so the server serves its documents.
    Started,
    /// The server wrote one line to its standard error.
    ///
    /// The line holds at most [`LSP_STDERR_LINE_BYTES_MAX`] bytes.
    Output(String),
    /// The output of one attempt passed [`LSP_STDERR_BYTES_MAX`].
    ///
    /// The session records no further line of that attempt. It still drains the
    /// stream, so the server never blocks on a full pipe.
    OutputBound,
}

/// One typed result of one language-server session.
#[derive(Debug)]
pub enum LanguageOutcome {
    /// The server published diagnostics for one document version.
    Diagnostics(DiagnosticSet),
    /// The server reported the state of one long operation.
    ///
    /// The report is decoration: it changes no buffer text and no cursor. It
    /// carries the generation of the attempt that produced it, so a report of a
    /// session that already restarted never changes visible state.
    Progress(ProgressReport),
    /// The server answered one definition request.
    Definition {
        /// The request that this answer completes.
        request: LanguageRequestId,
        /// The buffer version that produced the answer.
        version: BufferVersion,
        /// The targets inside the workspace root, in answer order.
        locations: Vec<SourceLocation>,
    },
    /// The server answered one hover request.
    Hover {
        /// The request that this answer completes.
        request: LanguageRequestId,
        /// The buffer version that produced the answer.
        version: BufferVersion,
        /// The text and its markup, or `None` when the server has nothing to
        /// say.
        markup: Option<MarkupText>,
    },
    /// The server answered one formatting request.
    Formatting {
        /// The request that this answer completes.
        request: LanguageRequestId,
        /// The accepted edits of one buffer version.
        edits: FormatEdits,
    },
    /// One request produced no value.
    Failed {
        /// The request that failed, or `None` for a session failure.
        request: Option<LanguageRequestId>,
        /// The typed reason.
        error: LspError,
    },
    /// The declared server is not installed, so this language has no service.
    ///
    /// The state is normal. The editor stays fully usable, and the session
    /// reports the state once.
    Unavailable,
    /// The session restarted after a server failure.
    ///
    /// The new server holds no document. The caller must open its buffers
    /// again.
    Restarted,
    /// The session stopped and accepts no further request.
    Stopped,
    /// The session recorded one fact about the server process.
    ///
    /// The editor writes the fact to its log and changes no visible state. See
    /// `docs/language-services.md`.
    Reported(ServerReport),
}

/// One typed result and the server whose session produced it.
#[derive(Debug)]
pub struct LanguageEvent {
    /// The server that owns the session.
    ///
    /// One language can run several servers, so every caller that records a
    /// state, or that merges an answer, reads this identity and never the
    /// adapter identifier alone.
    pub server: LanguageServerId,
    /// The typed result.
    pub outcome: LanguageOutcome,
}

/// The query that one request asks of the server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Query {
    /// Resolve the definition at one position.
    Definition(DocumentPosition),
    /// Describe the symbol at one position.
    Hover(DocumentPosition),
    /// Format the complete document.
    Format,
    /// Pull the diagnostics of the complete document.
    ///
    /// The session asks this query on its own, so no editor request waits for
    /// the answer. See `docs/language-services.md`.
    Diagnostics,
}

impl Query {
    /// Returns the protocol method of this query.
    const fn method(self) -> &'static str {
        match self {
            Self::Definition(_) => "textDocument/definition",
            Self::Hover(_) => "textDocument/hover",
            Self::Format => "textDocument/formatting",
            Self::Diagnostics => DIAGNOSTIC_PULL_METHOD,
        }
    }

    /// Returns the deadline of this query.
    ///
    /// A formatter and a diagnostic pull each run a complete pass over the
    /// document, so both need more time than a position query.
    const fn deadline(self) -> Duration {
        match self {
            Self::Definition(_) | Self::Hover(_) => LSP_REQUEST_DEADLINE,
            Self::Format => LSP_FORMAT_DEADLINE,
            Self::Diagnostics => LSP_DIAGNOSTIC_DEADLINE,
        }
    }
}

/// The model that carries the diagnostics of one session.
///
/// The handshake selects the model from the `diagnosticProvider` capability of
/// the server. See `docs/language-services.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
enum DiagnosticsModel {
    /// The server publishes one set without a request.
    Push,
    /// The client asks, and the server answers one report.
    Pull {
        /// The provider identifier that every request repeats, when the
        /// capability names one.
        identifier: Option<String>,
    },
}

/// The change notification that one session sends.
///
/// The handshake selects the mode from the `textDocumentSync` capability of the
/// server. See `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SynchronizationMode {
    /// The server accepts no change notification.
    None,
    /// The server receives the complete text of the document.
    Full,
    /// The server receives one range for each change.
    Incremental,
}

impl SynchronizationMode {
    /// The capability value of a full synchronization.
    const FULL: u64 = 1;

    /// The capability value of an incremental synchronization.
    const INCREMENTAL: u64 = 2;

    /// Reads the mode that one capability value names.
    ///
    /// The protocol defines the value 0 as no synchronization, and it reserves
    /// no further value, so every other number also sends no change. A wrong
    /// number must never send a change that the server reads as another shape.
    const fn from_kind(kind: u64) -> Self {
        match kind {
            Self::FULL => Self::Full,
            Self::INCREMENTAL => Self::Incremental,
            _ => Self::None,
        }
    }

    /// Reports whether one document of this session mirrors its text.
    ///
    /// A full synchronization sends the complete text of every change, and the
    /// session builds that text from the mirror.
    const fn mirroring(self) -> TextMirroring {
        match self {
            Self::Full => TextMirroring::Present,
            Self::None | Self::Incremental => TextMirroring::Absent,
        }
    }
}

/// One message from the editor to one session.
#[derive(Debug)]
enum SessionRequest {
    /// Open one document with its exact content.
    Open {
        path: PathBuf,
        version: BufferVersion,
        text: Arc<str>,
    },
    /// Synchronize one applied edit transaction.
    Change {
        path: PathBuf,
        version: BufferVersion,
        changes: Vec<ContentChange>,
    },
    /// Close one document.
    Close { path: PathBuf },
    /// Ask one question about one document version.
    Query {
        id: LanguageRequestId,
        path: PathBuf,
        version: BufferVersion,
        query: Query,
    },
}

/// The editor side of one session.
///
/// Every method returns without waiting, so the terminal event loop never
/// blocks on the server. A full queue returns [`LspError::Saturated`], and the
/// caller keeps its previous visible state.
#[derive(Debug)]
pub struct LanguageServerHandle {
    id: LanguageServerId,
    formatting: ServerFormatting,
    requests: mpsc::Sender<SessionRequest>,
    next_id: AtomicU64,
    cancellation: CancellationToken,
}

impl LanguageServerHandle {
    /// Returns the server that owns the session.
    #[must_use]
    pub const fn id(&self) -> LanguageServerId {
        self.id
    }

    /// Reports whether this server formats the documents of its language.
    ///
    /// Exactly one declared server of one adapter formats, so a caller sends
    /// every formatting request to the one handle that answers
    /// [`ServerFormatting::Enabled`].
    #[must_use]
    pub const fn formatting(&self) -> ServerFormatting {
        self.formatting
    }

    /// Opens one document with the exact text of one buffer version.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::Saturated`] when the queue is full and
    /// [`LspError::Stopped`] after the session stopped.
    pub fn open(
        &self,
        path: &Path,
        version: BufferVersion,
        text: Arc<str>,
    ) -> Result<(), LspError> {
        self.send(SessionRequest::Open {
            path: path.to_path_buf(),
            version,
            text,
        })
    }

    /// Synchronizes one applied edit transaction.
    ///
    /// Build the changes with [`ContentChange::from_transaction`] from the
    /// buffer as it was before the transaction, and pass the version that the
    /// transaction produced.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`LanguageServerHandle::open`].
    pub fn change(
        &self,
        path: &Path,
        version: BufferVersion,
        changes: Vec<ContentChange>,
    ) -> Result<(), LspError> {
        self.send(SessionRequest::Change {
            path: path.to_path_buf(),
            version,
            changes,
        })
    }

    /// Closes one document.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`LanguageServerHandle::open`].
    pub fn close(&self, path: &Path) -> Result<(), LspError> {
        self.send(SessionRequest::Close {
            path: path.to_path_buf(),
        })
    }

    /// Asks for the definition at one position of one buffer version.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`LanguageServerHandle::open`].
    pub fn definition(
        &self,
        path: &Path,
        version: BufferVersion,
        position: DocumentPosition,
    ) -> Result<LanguageRequestId, LspError> {
        self.query(path, version, Query::Definition(position))
    }

    /// Asks for the hover text at one position of one buffer version.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`LanguageServerHandle::open`].
    pub fn hover(
        &self,
        path: &Path,
        version: BufferVersion,
        position: DocumentPosition,
    ) -> Result<LanguageRequestId, LspError> {
        self.query(path, version, Query::Hover(position))
    }

    /// Asks for the formatting edits of one buffer version.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`LanguageServerHandle::open`].
    pub fn format(
        &self,
        path: &Path,
        version: BufferVersion,
    ) -> Result<LanguageRequestId, LspError> {
        self.query(path, version, Query::Format)
    }

    /// Cancels the session and every request that it holds.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    fn query(
        &self,
        path: &Path,
        version: BufferVersion,
        query: Query,
    ) -> Result<LanguageRequestId, LspError> {
        let id = LanguageRequestId(self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        self.send(SessionRequest::Query {
            id,
            path: path.to_path_buf(),
            version,
            query,
        })?;
        Ok(id)
    }

    fn send(&self, request: SessionRequest) -> Result<(), LspError> {
        self.requests
            .try_send(request)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => LspError::Saturated,
                mpsc::error::TrySendError::Closed(_) => LspError::Stopped,
            })
    }
}

/// The stable data of one session, which every restart reuses.
pub(super) struct SessionConfig {
    /// The server that owns the session.
    pub(super) id: LanguageServerId,
    /// The protocol language identifier of every document of this session.
    pub(super) language_id: &'static str,
    /// The program that runs the server, which titles one overlay group.
    pub(super) server: &'static str,
    /// Whether this server formats the documents of its language.
    pub(super) formatting: ServerFormatting,
    /// The containment boundary of every path and every `file` URI.
    pub(super) root: WorkspaceRoot,
    /// The initialization options that the adapter declared.
    pub(super) options: Value,
    /// The workspace settings that the adapter declared, or `None`.
    ///
    /// A declaration that names settings opens the configuration channel of its
    /// session: the handshake declares the client capability, the session sends
    /// one notification, and it answers the request of the server. See
    /// `docs/language-services.md`.
    pub(super) workspace_settings: Option<Value>,
    /// The indent settings that one formatting request sends.
    pub(super) indent: IndentSettings,
    /// Whether the session parses and publishes diagnostics.
    pub(super) diagnostics_enabled: bool,
    /// The adapters that name the code of one fence of a hover answer.
    ///
    /// The session runs off the terminal event loop, so it performs the
    /// Tree-sitter highlight that the loop must never run. See
    /// `docs/language-services.md`.
    pub(super) registry: LanguageRegistry,
}

/// The byte streams of one server attempt.
///
/// The streams are trait objects, because a session runs over the pipes of a
/// child process in the editor and over an in-memory pair in a test.
///
/// The type is public because the `mock` test seam hands prepared streams to a
/// session across the crate boundary. Editor code never names it.
pub struct Transport {
    input: Box<dyn AsyncWrite + Send + Unpin>,
    output: Box<dyn AsyncRead + Send + Unpin>,
    /// The standard error of the child, which one background task drains.
    ///
    /// A prepared stream pair holds no standard error, so a test transport
    /// carries `None` and the attempt starts no reader.
    errors: Option<Box<dyn AsyncRead + Send + Unpin>>,
    child: Option<Child>,
}

#[cfg(any(test, feature = "test-support"))]
impl Transport {
    /// Creates one transport over a prepared stream pair.
    pub(super) fn prepared(
        input: impl AsyncWrite + Send + Unpin + 'static,
        output: impl AsyncRead + Send + Unpin + 'static,
    ) -> Self {
        Self {
            input: Box::new(input),
            output: Box::new(output),
            errors: None,
            child: None,
        }
    }
}

/// Creates the transport of each session attempt.
pub(super) enum TransportFactory {
    /// Start the declared executable as a child process.
    Process {
        /// The declared executable.
        program: OsString,
        /// The declared arguments.
        args: Vec<OsString>,
        /// The working directory of the child.
        root: PathBuf,
    },
    /// Take the next prepared stream pair, which only tests supply.
    #[cfg(any(test, feature = "test-support"))]
    Prepared(Vec<Transport>),
}

impl TransportFactory {
    /// Creates the transport of the next attempt.
    fn create(&mut self) -> Result<Transport, LspError> {
        match self {
            Self::Process {
                program,
                args,
                root,
            } => {
                let mut command = Command::new(&*program);
                command
                    .args(&*args)
                    .current_dir(&*root)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    // The server log names the cause of a failure that the
                    // protocol never reports, so the session captures it. One
                    // background task drains this pipe from the first byte, so
                    // the pipe never fills and the child never blocks. See
                    // `docs/language-services.md`.
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);
                let mut child = command.spawn().map_err(|source| {
                    if source.kind() == std::io::ErrorKind::NotFound {
                        LspError::NotInstalled
                    } else {
                        LspError::Spawn(source)
                    }
                })?;
                let input = child
                    .stdin
                    .take()
                    .expect("the command configures a piped standard input");
                let output = child
                    .stdout
                    .take()
                    .expect("the command configures a piped standard output");
                let errors = child
                    .stderr
                    .take()
                    .expect("the command configures a piped standard error");
                Ok(Transport {
                    input: Box::new(input),
                    output: Box::new(output),
                    errors: Some(Box::new(errors)),
                    child: Some(child),
                })
            }
            #[cfg(any(test, feature = "test-support"))]
            Self::Prepared(prepared) => {
                if prepared.is_empty() {
                    return Err(LspError::NotInstalled);
                }
                Ok(prepared.remove(0))
            }
        }
    }
}

/// Why one session attempt ended.
enum AttemptOutcome {
    /// The editor closed or cancelled the session.
    Stopped,
    /// The declared executable is not installed.
    NotInstalled,
    /// The attempt failed, so a bounded restart may follow.
    Failed(LspError),
}

/// One document that the session holds open.
struct OpenDocument {
    /// The `file` URI of the document.
    uri: String,
    /// The buffer version of the content that the server holds.
    version: BufferVersion,
    /// The protocol document version of that content.
    revision: i64,
    /// The position conversion of this document.
    ///
    /// A UTF-16 session mirrors the exact text that the server holds, because a
    /// UTF-16 column only means something against the line that it indexes. A
    /// UTF-8 session mirrors no text. See `docs/language-services.md`.
    mapping: DocumentMapping,
    /// The result identifier of the last pulled report of this document.
    ///
    /// The next pull repeats it, so the server may answer that the previous set
    /// is unchanged instead of sending it again.
    result_id: Option<String>,
    /// The moment at which the next pull of this document is due.
    ///
    /// `None` names a document that needs no pull. A push session never sets
    /// the field.
    pull_due: Option<Instant>,
    /// Whether one pull of this document waits for its answer.
    ///
    /// One document holds at most one pull at a time, so a burst of edits
    /// starts no request storm.
    pull_running: bool,
}

impl OpenDocument {
    /// Reports whether one pull of this document is due at one moment.
    ///
    /// A document that already holds a running pull waits for its answer, so it
    /// is never due.
    fn is_pull_due(&self, now: Instant) -> bool {
        !self.pull_running && self.pull_due.is_some_and(|due| due <= now)
    }
}

/// One request that waits for an answer.
struct PendingRequest {
    /// The editor identity of the request, or `None` when the session asked.
    ///
    /// The session pulls the diagnostics on its own, so no editor request waits
    /// for that answer. See `docs/language-services.md`.
    id: Option<LanguageRequestId>,
    /// The document of the request.
    path: PathBuf,
    /// The buffer version that the request asked about.
    version: BufferVersion,
    /// The question that the request asked.
    query: Query,
    /// The moment after which the request is a timeout.
    deadline: Instant,
}

/// Starts one session and returns its editor handle.
///
/// The session runs as one background task. The task owns the child process,
/// the framing, and every deadline, so the terminal event loop stays free.
pub(super) fn start(
    factory: TransportFactory,
    config: SessionConfig,
    events: mpsc::Sender<LanguageEvent>,
    cancellation: CancellationToken,
) -> (LanguageServerHandle, tokio::task::JoinHandle<()>) {
    let (sender, receiver) = mpsc::channel(LSP_REQUEST_QUEUE_CAPACITY);
    let handle = LanguageServerHandle {
        id: config.id,
        formatting: config.formatting,
        requests: sender,
        next_id: AtomicU64::new(0),
        cancellation: cancellation.clone(),
    };
    let task = tokio::spawn(supervise(factory, config, receiver, events, cancellation));
    (handle, task)
}

/// Runs one session and restarts it a bounded number of times.
async fn supervise(
    mut factory: TransportFactory,
    config: SessionConfig,
    mut requests: mpsc::Receiver<SessionRequest>,
    events: mpsc::Sender<LanguageEvent>,
    cancellation: CancellationToken,
) {
    let mut restarts = 0_usize;
    let mut generation = SessionGeneration::FIRST;
    loop {
        let outcome = attempt(
            &mut factory,
            &config,
            generation,
            &mut requests,
            &events,
            &cancellation,
        )
        .await;
        match outcome {
            AttemptOutcome::Stopped => break,
            AttemptOutcome::NotInstalled => {
                emit(&events, config.id, LanguageOutcome::Unavailable).await;
                return;
            }
            AttemptOutcome::Failed(error) => {
                emit(
                    &events,
                    config.id,
                    LanguageOutcome::Failed {
                        request: None,
                        error,
                    },
                )
                .await;
                if restarts >= LSP_RESTARTS_MAX || cancellation.is_cancelled() {
                    break;
                }
                restarts += 1;
                // The new server assigns its own progress tokens, so the next
                // attempt reports a later generation and the editor drops every
                // report of the attempt that failed.
                generation = generation.next();
                // The new server holds no document, so the caller must open its
                // buffers again before it queries them.
                emit(&events, config.id, LanguageOutcome::Restarted).await;
            }
        }
    }
    emit(&events, config.id, LanguageOutcome::Stopped).await;
}

/// Runs one server process from the handshake to its end.
async fn attempt(
    factory: &mut TransportFactory,
    config: &SessionConfig,
    generation: SessionGeneration,
    requests: &mut mpsc::Receiver<SessionRequest>,
    events: &mpsc::Sender<LanguageEvent>,
    cancellation: &CancellationToken,
) -> AttemptOutcome {
    let transport = match factory.create() {
        Ok(transport) => transport,
        Err(LspError::NotInstalled) => return AttemptOutcome::NotInstalled,
        Err(error) => return AttemptOutcome::Failed(error),
    };
    let Transport {
        input,
        output,
        errors,
        mut child,
    } = transport;
    // The standard error of the child needs a reader from the first byte,
    // because a pipe that nobody drains fills and stops the child. See
    // `docs/language-services.md`.
    let errors =
        errors.map(|stream| tokio::spawn(record_errors(stream, config.id, events.clone())));
    let (envelope_sender, mut envelopes) = mpsc::channel(LSP_EVENT_QUEUE_CAPACITY);
    // The frame reader owns its stream in one task, so no cancelled future can
    // drop a partly read frame and desynchronize the stream.
    let reader = tokio::spawn(read_envelopes(ProtocolReader::new(output), envelope_sender));
    let mut session = Session {
        config,
        generation,
        encoding: PositionEncoding::Utf16,
        synchronization: SynchronizationMode::None,
        diagnostics: DiagnosticsModel::Push,
        events,
        writer: ProtocolWriter::new(input),
        documents: HashMap::new(),
        pending: HashMap::new(),
        highlighter: SyntaxHighlighter::new(),
    };

    let outcome = session.serve(&mut envelopes, requests, cancellation).await;
    if matches!(outcome, AttemptOutcome::Stopped) {
        let _ = time::timeout(LSP_SHUTDOWN_DEADLINE, session.shutdown(&mut envelopes)).await;
    }
    reader.abort();
    if let Some(child) = child.as_mut() {
        terminate(child).await;
    }
    if let Some(mut task) = errors {
        // The child ended, so the stream ends and the reader records its last
        // line. Another process may still hold the write end of that pipe, so
        // the wait carries the shutdown deadline and the rest stays unrecorded.
        if time::timeout(LSP_SHUTDOWN_DEADLINE, &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
    outcome
}

/// Drains the standard error of one server and records a bounded part of it.
///
/// The task drains the stream until the stream ends. A child that writes to a
/// pipe that nobody reads blocks when the pipe fills. Several servers write to
/// their standard error while they run correctly.
///
/// Draining and recording carry two different bounds. The task records at most
/// [`LSP_STDERR_BYTES_MAX`] bytes of one attempt, and it drains every further
/// byte without recording it. See `docs/language-services.md`.
async fn record_errors<R>(
    mut stream: R,
    server: LanguageServerId,
    events: mpsc::Sender<LanguageEvent>,
) where
    R: AsyncRead + Unpin,
{
    let mut chunk = [0_u8; STDERR_CHUNK_BYTES];
    let mut recorder = ErrorRecorder::new(server, events);
    loop {
        let read = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        debug_assert!(read <= chunk.len(), "one read fills at most the chunk");
        recorder.take(&chunk[..read]);
    }
    recorder.finish();
}

/// The bounded recorder of the standard error of one server attempt.
///
/// The recorder holds one partial line and two counters. It never waits, so the
/// task that owns it always returns to the stream and the pipe never fills.
struct ErrorRecorder {
    /// The server that wrote the stream.
    server: LanguageServerId,
    /// The result queue of the editor.
    events: mpsc::Sender<LanguageEvent>,
    /// The bytes of the line that no line break ended yet.
    line: Vec<u8>,
    /// The bytes that the recorder already recorded.
    recorded: usize,
    /// Whether the recorded bytes passed [`LSP_STDERR_BYTES_MAX`].
    stopped: bool,
}

impl ErrorRecorder {
    /// Creates the recorder of one attempt.
    fn new(server: LanguageServerId, events: mpsc::Sender<LanguageEvent>) -> Self {
        Self {
            server,
            events,
            line: Vec::new(),
            recorded: 0,
            stopped: false,
        }
    }

    /// Records the complete lines of one chunk and keeps the rest.
    ///
    /// The call returns at once after the recorder stopped, so the caller
    /// drains the stream at full speed.
    fn take(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            if self.stopped {
                return;
            }
            if byte == b'\n' {
                self.end_line();
            } else if self.line.len() < LSP_STDERR_LINE_BYTES_MAX {
                // A longer line loses its tail. The recorded start names the
                // state that the server reports.
                self.line.push(byte);
            }
        }
    }

    /// Records the line that the stream ended without a line break.
    fn finish(&mut self) {
        if !self.stopped {
            self.end_line();
        }
    }

    /// Records one complete line and starts the next one.
    fn end_line(&mut self) {
        debug_assert!(
            self.line.len() <= LSP_STDERR_LINE_BYTES_MAX,
            "every earlier byte left the line inside its bound"
        );
        let text = String::from_utf8_lossy(&self.line);
        let text = text.trim_end().to_owned();
        // The line break counts as one byte, so an empty line still moves the
        // recorder towards its bound.
        self.recorded = self.recorded.saturating_add(self.line.len() + 1);
        self.line.clear();
        if !text.is_empty() {
            self.report(ServerReport::Output(text));
        }
        if self.recorded >= LSP_STDERR_BYTES_MAX {
            self.stopped = true;
            self.report(ServerReport::OutputBound);
        }
    }

    /// Sends one report to the editor without waiting.
    ///
    /// A full result queue drops the report. The capture is a report, never a
    /// failure path, and a wait here would stop the drain and fill the pipe.
    fn report(&self, report: ServerReport) {
        let _ = self.events.try_send(LanguageEvent {
            server: self.server,
            outcome: LanguageOutcome::Reported(report),
        });
    }
}

/// Reads frames until the stream ends or one bound stops the session.
async fn read_envelopes<R>(
    mut reader: ProtocolReader<R>,
    sender: mpsc::Sender<Result<RpcEnvelope, LspError>>,
) where
    R: AsyncRead + Unpin,
{
    loop {
        let envelope = reader.read_envelope().await;
        let failed = envelope.is_err();
        if sender.send(envelope).await.is_err() || failed {
            return;
        }
    }
}

/// Stops one child process and waits a bounded time for its exit.
async fn terminate(child: &mut Child) {
    let _ = child.start_kill();
    let _ = time::timeout(LSP_SHUTDOWN_DEADLINE, child.wait()).await;
}

/// Sends one result to the editor.
async fn emit(
    events: &mpsc::Sender<LanguageEvent>,
    server: LanguageServerId,
    outcome: LanguageOutcome,
) {
    let _ = events.send(LanguageEvent { server, outcome }).await;
}

/// The live state of one server attempt.
struct Session<'a> {
    config: &'a SessionConfig,
    /// The attempt that this session serves, which every progress report names.
    generation: SessionGeneration,
    /// The position encoding that the handshake negotiated.
    ///
    /// The session serves no editor request before the handshake completes, so
    /// the value always names the encoding that the running server confirmed.
    /// UTF-16 is the encoding that the protocol defines until a server answers.
    encoding: PositionEncoding,
    /// The change notification that the handshake selected.
    ///
    /// The mode belongs to one server attempt, so a restart reads the
    /// capability again. No change notification for the value that the protocol
    /// defines for an absent capability. See `docs/language-services.md`.
    synchronization: SynchronizationMode,
    /// The diagnostic model that the handshake selected.
    ///
    /// The model belongs to one server attempt, so a restart reads the
    /// capability again. See `docs/language-services.md`.
    diagnostics: DiagnosticsModel,
    events: &'a mpsc::Sender<LanguageEvent>,
    writer: ProtocolWriter<Box<dyn AsyncWrite + Send + Unpin>>,
    documents: HashMap<PathBuf, OpenDocument>,
    pending: HashMap<u64, PendingRequest>,
    /// The highlighter that names the roles of the fences of a hover answer.
    ///
    /// The session owns it, so the compiled query of a language serves every
    /// later answer of that session and leaves with it.
    highlighter: SyntaxHighlighter,
}

type Envelopes = mpsc::Receiver<Result<RpcEnvelope, LspError>>;

impl Session<'_> {
    /// Runs the handshake and then serves the editor until the session ends.
    async fn serve(
        &mut self,
        envelopes: &mut Envelopes,
        requests: &mut mpsc::Receiver<SessionRequest>,
        cancellation: &CancellationToken,
    ) -> AttemptOutcome {
        let handshake = tokio::select! {
            biased;
            () = cancellation.cancelled() => return AttemptOutcome::Stopped,
            result = time::timeout(LSP_INITIALIZE_DEADLINE, self.initialize(envelopes)) => {
                result.unwrap_or(Err(LspError::Timeout))
            }
        };
        if let Err(error) = handshake {
            return AttemptOutcome::Failed(error);
        }
        // The server answered the handshake, so it serves its documents from
        // here. The editor records the start beside the output of the server.
        emit(
            self.events,
            self.config.id,
            LanguageOutcome::Reported(ServerReport::Started),
        )
        .await;
        loop {
            let deadline = self.next_deadline();
            let step = tokio::select! {
                biased;
                () = cancellation.cancelled() => return AttemptOutcome::Stopped,
                envelope = envelopes.recv() => match envelope {
                    Some(Ok(envelope)) => self.dispatch(envelope).await,
                    Some(Err(error)) => return AttemptOutcome::Failed(error),
                    None => return AttemptOutcome::Failed(LspError::Stopped),
                },
                request = requests.recv() => match request {
                    Some(request) => self.handle(request).await,
                    None => return AttemptOutcome::Stopped,
                },
                () = sleep_until(deadline), if deadline.is_some() => self.expire().await,
            };
            if let Err(error) = step {
                return AttemptOutcome::Failed(error);
            }
            // One step can open a document, apply a change, or complete a pull,
            // and each of those can make one pull due.
            if let Err(error) = self.fire_pulls().await {
                return AttemptOutcome::Failed(error);
            }
        }
    }

    /// Declares the client capabilities and negotiates the position encoding.
    async fn initialize(&mut self, envelopes: &mut Envelopes) -> Result<(), LspError> {
        let root_uri = self.config.root.root_uri()?;
        // kvim declares the configuration capability only while its declaration
        // names settings, because a session without settings still reports the
        // request of a server as an unknown method.
        let configuration = self.config.workspace_settings.is_some();
        let id = self
            .writer
            .request(
                "initialize",
                json!({
                    "processId": Value::Null,
                    "rootUri": root_uri,
                    "capabilities": {
                        "general": {
                            "positionEncodings":
                                PositionEncoding::OFFERED.map(PositionEncoding::as_str),
                        },
                        // A server sends `$/progress` only after the client
                        // declares that it shows work-done progress.
                        "window": { "workDoneProgress": true },
                        "workspace": {
                            "configuration": configuration,
                            "didChangeConfiguration": { "dynamicRegistration": false },
                            // The session answers the refresh request of a pull
                            // server and asks for every open document again.
                            "diagnostics": { "refreshSupport": true },
                        },
                        "textDocument": {
                            "synchronization": {
                                "dynamicRegistration": false,
                                "didSave": false,
                            },
                            "publishDiagnostics": { "versionSupport": true },
                            "definition": {
                                "dynamicRegistration": false,
                                "linkSupport": true,
                            },
                            "hover": {
                                "dynamicRegistration": false,
                                // The order names the preference of the client,
                                // and the float renders markdown.
                                "contentFormat": ["markdown", "plaintext"],
                            },
                            "formatting": { "dynamicRegistration": false },
                        },
                    },
                    "initializationOptions": self.config.options,
                    "workspaceFolders": [{ "uri": root_uri, "name": "workspace" }],
                }),
            )
            .await?;
        let result = self.await_response(envelopes, id).await?;
        let capabilities: Value =
            serde_json::from_str(result.get()).map_err(|_| LspError::MalformedResponse)?;
        // kvim measures every column in UTF-8 bytes, and the protocol measures
        // one column in UTF-16 code units unless the server confirms UTF-8. The
        // session records the answer and converts every column against it.
        self.encoding = PositionEncoding::from_result(
            capabilities
                .pointer("/capabilities/positionEncoding")
                .and_then(Value::as_str),
        )?;
        // The server decides what one change notification carries. kvim sends
        // the complete text to a server that asks for a full synchronization,
        // and one range for each change to a server that asks for an
        // incremental one.
        self.synchronization =
            synchronization_mode(capabilities.pointer("/capabilities/textDocumentSync"));
        // A server that advertises a diagnostic provider answers the request of
        // the client instead of publishing a set on its own.
        self.diagnostics =
            diagnostics_model(capabilities.pointer("/capabilities/diagnosticProvider"))?;
        self.writer.notify("initialized", json!({})).await?;
        if let Some(settings) = self.config.workspace_settings.clone() {
            self.writer
                .notify(
                    "workspace/didChangeConfiguration",
                    json!({ "settings": settings }),
                )
                .await?;
        }
        Ok(())
    }

    /// Sends `shutdown` and `exit` in the order that the protocol requires.
    async fn shutdown(&mut self, envelopes: &mut Envelopes) -> Result<(), LspError> {
        let id = self.writer.request("shutdown", Value::Null).await?;
        let result = self.await_response(envelopes, id).await?;
        // The protocol requires exactly null here. Another value means that the
        // server did not accept the shutdown.
        if result.get().trim() != "null" {
            return Err(LspError::MalformedResponse);
        }
        self.writer.notify("exit", Value::Null).await
    }

    /// Waits for one response and answers every server request meanwhile.
    async fn await_response(
        &mut self,
        envelopes: &mut Envelopes,
        expected: u64,
    ) -> Result<Box<RawValue>, LspError> {
        loop {
            let envelope = envelopes.recv().await.ok_or(LspError::Stopped)??;
            if envelope.method.is_some() {
                if let Some(id) = envelope.id {
                    self.writer.reject_server_request(id).await?;
                }
                continue;
            }
            let Some(RpcId::Unsigned(id)) = envelope.id else {
                continue;
            };
            if id != expected {
                continue;
            }
            if let Some(error) = envelope.error {
                return Err(LspError::Response { code: error.code });
            }
            return envelope.result.ok_or(LspError::MalformedResponse);
        }
    }

    /// Routes one received message.
    async fn dispatch(&mut self, envelope: RpcEnvelope) -> Result<(), LspError> {
        if let Some(method) = envelope.method {
            if let Some(id) = envelope.id {
                // An unanswered server request stalls the server, so kvim always
                // answers. It accepts the creation of one progress token,
                // because the overlay shows the reports of that token. It asks
                // for every open document again after a refresh request, and it
                // answers the configuration request while its declaration names
                // settings. It reports every other method as unknown.
                return match method.as_str() {
                    PROGRESS_CREATE_METHOD => self.writer.accept_server_request(id).await,
                    DIAGNOSTIC_REFRESH_METHOD => {
                        self.writer.accept_server_request(id).await?;
                        self.schedule_every_pull();
                        Ok(())
                    }
                    CONFIGURATION_METHOD => {
                        self.answer_configuration(id, envelope.params.as_deref())
                            .await
                    }
                    _ => self.writer.reject_server_request(id).await,
                };
            }
            let result = self.notification(&method, envelope.params.as_deref());
            return self.report(None, result).await;
        }
        let Some(RpcId::Unsigned(id)) = envelope.id else {
            return Ok(());
        };
        let Some(pending) = self.pending.remove(&id) else {
            return Ok(());
        };
        let result = match (envelope.error, envelope.result) {
            (Some(error), _) => Err(LspError::Response { code: error.code }),
            (None, Some(value)) => self.answer(&pending, &value),
            (None, None) => Err(LspError::MalformedResponse),
        };
        match pending.id {
            Some(request) => self.report(Some(request), result).await,
            None => self.report_pull(&pending.path, result).await,
        }
    }

    /// Answers the workspace configuration request of one server.
    ///
    /// One item that names a section receives that member of the declared
    /// settings. An item that names no section, or the empty section, receives
    /// the complete object. A section that the object does not hold receives the
    /// null value. A declaration that names no settings, and a malformed
    /// request, both report the method as unknown, so the server never stalls.
    /// See `docs/language-services.md`.
    async fn answer_configuration(
        &mut self,
        id: RpcId,
        params: Option<&RawValue>,
    ) -> Result<(), LspError> {
        let Some(settings) = self.config.workspace_settings.clone() else {
            return self.writer.reject_server_request(id).await;
        };
        let Ok(items) = configuration_items(params) else {
            return self.writer.reject_server_request(id).await;
        };
        let answer: Vec<Value> = items
            .iter()
            .map(|item| configuration_section(&settings, item.section.as_deref()))
            .collect();
        self.writer
            .answer_server_request(id, Value::Array(answer))
            .await
    }

    /// Publishes the diagnostics and the progress of one notification.
    ///
    /// Every other notification carries no visible state, so the session
    /// ignores it.
    fn notification(
        &self,
        method: &str,
        params: Option<&RawValue>,
    ) -> Result<Option<LanguageOutcome>, LspError> {
        match method {
            PROGRESS_METHOD => Ok(parse_progress(params, self.generation, self.config.server)
                .map(LanguageOutcome::Progress)),
            DIAGNOSTICS_METHOD => self.diagnostics(params),
            _ => Ok(None),
        }
    }

    /// Publishes the diagnostics of one notification.
    fn diagnostics(&self, params: Option<&RawValue>) -> Result<Option<LanguageOutcome>, LspError> {
        if !self.config.diagnostics_enabled {
            return Ok(None);
        }
        let params = params.ok_or(LspError::MalformedResponse)?;
        let published: PublishedDiagnostics =
            serde_json::from_str(params.get()).map_err(|_| LspError::MalformedResponse)?;
        let path = self.config.root.path_from_uri(&published.uri)?;
        let Some(document) = self.documents.get(&path) else {
            // The server may describe a file that the editor never opened.
            return Ok(None);
        };
        // A notification for an earlier document revision is obsolete. It never
        // reaches visible state and never enters a cache.
        if published
            .version
            .is_some_and(|version| version != document.revision)
        {
            return Ok(None);
        }
        let mut budget = ArrayBudget::new(LSP_DIAGNOSTICS_MAX, LSP_DIAGNOSTICS_MAX);
        let raw: Vec<RawDiagnostic> = deserialize_bounded_array(
            &published.diagnostics,
            LSP_DIAGNOSTICS_MAX,
            LspBound::Diagnostics,
            &mut budget,
        )?;
        // Every diagnostic records its producer, so a buffer that several
        // servers describe can name the server that found each problem. Every
        // range converts against the text that the server holds.
        let diagnostics = raw
            .into_iter()
            .map(|diagnostic| {
                diagnostic.into_diagnostic(self.config.id.server(), &document.mapping)
            })
            .collect::<Result<Vec<_>, LspError>>()?;
        let set = DiagnosticSet::new(path, document.version, diagnostics);
        Ok(Some(LanguageOutcome::Diagnostics(set)))
    }

    /// Applies one editor request.
    async fn handle(&mut self, request: SessionRequest) -> Result<(), LspError> {
        match request {
            SessionRequest::Open {
                path,
                version,
                text,
            } => {
                let result = self.open(path, version, &text).await;
                self.report(None, result.map(|()| None)).await
            }
            SessionRequest::Change {
                path,
                version,
                changes,
            } => {
                let result = self.change(&path, version, &changes).await;
                self.report(None, result.map(|()| None)).await
            }
            SessionRequest::Close { path } => {
                let result = self.close(&path).await;
                self.report(None, result.map(|()| None)).await
            }
            SessionRequest::Query {
                id,
                path,
                version,
                query,
            } => {
                let result = self.query(id, path, version, query).await;
                self.report(Some(id), result.map(|()| None)).await
            }
        }
    }

    async fn open(
        &mut self,
        path: PathBuf,
        version: BufferVersion,
        text: &str,
    ) -> Result<(), LspError> {
        let uri = self.config.root.uri(&path)?;
        if self.documents.contains_key(&path) {
            // A second open replaces the server copy, so the old copy closes
            // first and the revision starts again.
            self.close(&path).await?;
        }
        enforce(
            self.documents.len().saturating_add(1),
            LSP_OPEN_DOCUMENTS_MAX,
            LspBound::OpenDocuments,
        )?;
        self.writer
            .notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": self.config.language_id,
                        "version": 1,
                        "text": text,
                    }
                }),
            )
            .await?;
        // A pull session asks for a new document at once, and a push session
        // waits for the notification of the server.
        let pull_due = self.pulls().then(Instant::now);
        self.documents.insert(
            path,
            OpenDocument {
                uri,
                version,
                revision: 1,
                mapping: DocumentMapping::new(
                    self.encoding,
                    self.synchronization.mirroring(),
                    text,
                ),
                result_id: None,
                pull_due,
                pull_running: false,
            },
        );
        Ok(())
    }

    async fn change(
        &mut self,
        path: &Path,
        version: BufferVersion,
        changes: &[ContentChange],
    ) -> Result<(), LspError> {
        enforce(
            changes.len(),
            LSP_CONTENT_CHANGES_MAX,
            LspBound::ContentChanges,
        )?;
        let pulls = self.pulls();
        let document = self.documents.get(path).ok_or(LspError::DocumentNotOpen)?;
        let revision = document.revision.saturating_add(1);
        let content_changes = match self.synchronization {
            // The server accepts no change notification, so the session sends
            // none. Its copy keeps the text of the open, and the recorded
            // version keeps that text, so every later request of this document
            // reports a stale version instead of an answer that describes text
            // the server does not hold.
            SynchronizationMode::None => return Ok(()),
            // A full synchronization carries no range. The mirror holds the
            // text that the server still holds, so the projection of the
            // changes is the text that the server must hold next.
            SynchronizationMode::Full => {
                vec![json!({ "text": document.mapping.projected(changes)? })]
            }
            // Every change addresses the text that the server still holds, so
            // every range converts against the mirror before the mirror moves
            // on.
            SynchronizationMode::Incremental => {
                let mut content_changes = Vec::with_capacity(changes.len());
                for change in changes {
                    let range = document.mapping.span_to_protocol(change.span)?;
                    content_changes.push(json!({ "range": range, "text": change.text }));
                }
                content_changes
            }
        };
        let uri = document.uri.clone();
        self.writer
            .notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": revision },
                    "contentChanges": content_changes,
                }),
            )
            .await?;
        // The server copy changes only after the notification reached it, so a
        // failed write leaves the recorded version, revision, and mirror
        // untouched.
        let document = self
            .documents
            .get_mut(path)
            .ok_or(LspError::DocumentNotOpen)?;
        document.revision = revision;
        document.version = version;
        // The next pull waits until the change settles, so one burst of edits
        // starts one request.
        document.pull_due = pulls.then(|| Instant::now() + LSP_DIAGNOSTIC_PULL_DELAY);
        if let Err(error) = document.mapping.apply(changes) {
            // The mirror and the server copy now hold different text, so every
            // later conversion of this document would read the wrong line. The
            // session drops the document instead, and every later request of
            // that document reports that it is not open. A refused earlier
            // change reaches this branch, because the editor then sent a change
            // that describes text the server never received.
            self.documents.remove(path);
            return Err(error);
        }
        Ok(())
    }

    async fn close(&mut self, path: &Path) -> Result<(), LspError> {
        let document = self
            .documents
            .remove(path)
            .ok_or(LspError::DocumentNotOpen)?;
        // A pending answer for a closed document can never publish, so the
        // session drops it instead of waiting for its deadline.
        self.pending
            .retain(|_, pending| pending.path.as_path() != path);
        self.writer
            .notify(
                "textDocument/didClose",
                json!({ "textDocument": { "uri": document.uri } }),
            )
            .await
    }

    async fn query(
        &mut self,
        id: LanguageRequestId,
        path: PathBuf,
        version: BufferVersion,
        query: Query,
    ) -> Result<(), LspError> {
        let document = self.documents.get(&path).ok_or(LspError::DocumentNotOpen)?;
        // The request must describe the content that the server holds.
        if document.version.get() != version.get() {
            return Err(LspError::StaleVersion);
        }
        enforce(
            self.pending.len().saturating_add(1),
            LSP_PENDING_REQUESTS_MAX,
            LspBound::PendingRequests,
        )?;
        let params = self.query_params(document, query)?;
        let protocol_id = self.writer.request(query.method(), params).await?;
        self.pending.insert(
            protocol_id,
            PendingRequest {
                id: Some(id),
                path,
                version,
                query,
                deadline: Instant::now() + query.deadline(),
            },
        );
        Ok(())
    }

    /// Builds the parameters of one question in the negotiated encoding.
    fn query_params(&self, document: &OpenDocument, query: Query) -> Result<Value, LspError> {
        let uri = &document.uri;
        Ok(match query {
            Query::Definition(position) | Query::Hover(position) => json!({
                "textDocument": { "uri": uri },
                "position": document.mapping.to_protocol(position)?,
            }),
            Query::Format => json!({
                "textDocument": { "uri": uri },
                "options": {
                    "tabSize": u32::from(self.config.indent.tab_width.get()),
                    "insertSpaces": self.config.indent.expand_tab,
                },
            }),
            // The session builds a pull on its own, because that request also
            // carries the provider identifier and the previous result
            // identifier.
            Query::Diagnostics => json!({ "textDocument": { "uri": uri } }),
        })
    }

    /// Converts one answer while its buffer version is still current.
    fn convert(
        &mut self,
        pending: &PendingRequest,
        result: &RawValue,
    ) -> Result<LanguageOutcome, LspError> {
        // Only an editor query reaches this conversion, and every editor query
        // carries its identity.
        let request = pending.id.ok_or(LspError::MalformedResponse)?;
        let document = self
            .documents
            .get(&pending.path)
            .ok_or(LspError::DocumentNotOpen)?;
        // The buffer changed after the request, so the answer describes text
        // that no longer exists.
        if document.version.get() != pending.version.get() {
            return Err(LspError::StaleVersion);
        }
        match pending.query {
            Query::Definition(_) => Ok(LanguageOutcome::Definition {
                request,
                version: pending.version,
                locations: self.definition_locations(result)?,
            }),
            Query::Hover(_) => Ok(LanguageOutcome::Hover {
                request,
                version: pending.version,
                markup: hover_markup(result, self.config.registry, &mut self.highlighter)?,
            }),
            Query::Diagnostics => {
                debug_assert!(
                    false,
                    "the answer path routes a pull before this conversion runs"
                );
                Err(LspError::MalformedResponse)
            }
            Query::Format => {
                let mut budget = ArrayBudget::new(LSP_FORMAT_EDITS_MAX, LSP_FORMAT_EDITS_MAX);
                let raw: Vec<RawTextEdit> = deserialize_bounded_array(
                    result,
                    LSP_FORMAT_EDITS_MAX,
                    LspBound::FormatEdits,
                    &mut budget,
                )?;
                let edits: Vec<TextEdit> = raw
                    .into_iter()
                    .map(|edit| edit.into_edit(&document.mapping))
                    .collect::<Result<_, LspError>>()?;
                Ok(LanguageOutcome::Formatting {
                    request,
                    edits: FormatEdits::new(pending.path.clone(), pending.version, edits),
                })
            }
        }
    }

    /// Converts one definition answer into contained workspace locations.
    ///
    /// A target outside the workspace root is rejected and never offered. A
    /// target of a document that this session does not hold open keeps the
    /// column of the server, because no mirrored text holds the line that the
    /// column indexes. See `docs/language-services.md`.
    fn definition_locations(&self, result: &RawValue) -> Result<Vec<SourceLocation>, LspError> {
        let text = result.get().trim_start();
        let raw = if text.starts_with('[') {
            let mut budget = ArrayBudget::new(LSP_LOCATIONS_MAX, LSP_LOCATIONS_MAX);
            deserialize_bounded_array(result, LSP_LOCATIONS_MAX, LspBound::Locations, &mut budget)?
        } else if text == "null" {
            Vec::new()
        } else {
            vec![
                serde_json::from_str::<RawLocation>(result.get())
                    .map_err(|_| LspError::MalformedResponse)?,
            ]
        };
        let unmirrored = DocumentMapping::Direct;
        Ok(raw
            .into_iter()
            .filter_map(|location| {
                let (uri, span) = location.parts();
                let path = self.config.root.path_from_uri(&uri).ok()?;
                let mapping = self
                    .documents
                    .get(&path)
                    .map_or(&unmirrored, |document| &document.mapping);
                let span = mapping.span_to_document(span).ok()?;
                Some(SourceLocation { path, span })
            })
            .collect())
    }

    /// Converts one answer, and records the state that a pull keeps.
    ///
    /// A pull records the result identifier of its report, so the next pull of
    /// the same document may ask for the unchanged answer.
    fn answer(
        &mut self,
        pending: &PendingRequest,
        result: &RawValue,
    ) -> Result<Option<LanguageOutcome>, LspError> {
        if pending.query == Query::Diagnostics {
            return self.pulled_diagnostics(pending, result);
        }
        self.convert(pending, result).map(Some)
    }

    /// Reads one pulled diagnostic report.
    ///
    /// A full report publishes its items and records its result identifier. An
    /// unchanged report publishes nothing, so the editor keeps the set that it
    /// already holds for this server. The report may also carry a
    /// `relatedDocuments` member, and the session ignores it, because it pulls
    /// each open document on its own. See `docs/language-services.md`.
    fn pulled_diagnostics(
        &mut self,
        pending: &PendingRequest,
        result: &RawValue,
    ) -> Result<Option<LanguageOutcome>, LspError> {
        let report: PulledReport =
            serde_json::from_str(result.get()).map_err(|_| LspError::MalformedResponse)?;
        if let Some(result_id) = report.result_id.as_deref() {
            enforce(
                result_id.len(),
                LSP_RESULT_ID_BYTES_MAX,
                LspBound::ResultIdBytes,
            )?;
        }
        let outcome = {
            let document = self
                .documents
                .get(&pending.path)
                .ok_or(LspError::DocumentNotOpen)?;
            // The buffer changed after the request, so the report describes text
            // that no longer exists.
            if document.version.get() != pending.version.get() {
                return Err(LspError::StaleVersion);
            }
            if report.kind == UNCHANGED_REPORT {
                None
            } else {
                let items = report.items.as_deref().ok_or(LspError::MalformedResponse)?;
                let mut budget = ArrayBudget::new(LSP_DIAGNOSTICS_MAX, LSP_DIAGNOSTICS_MAX);
                let raw: Vec<RawDiagnostic> = deserialize_bounded_array(
                    items,
                    LSP_DIAGNOSTICS_MAX,
                    LspBound::Diagnostics,
                    &mut budget,
                )?;
                // Every diagnostic records its producer, exactly as a published
                // set does, so a merge names the server that found each problem.
                let diagnostics = raw
                    .into_iter()
                    .map(|diagnostic| {
                        diagnostic.into_diagnostic(self.config.id.server(), &document.mapping)
                    })
                    .collect::<Result<Vec<_>, LspError>>()?;
                Some(LanguageOutcome::Diagnostics(DiagnosticSet::new(
                    pending.path.clone(),
                    document.version,
                    diagnostics,
                )))
            }
        };
        self.record_result_id(&pending.path, report.result_id);
        Ok(outcome)
    }

    /// Records the result identifier that the next pull of one document sends.
    fn record_result_id(&mut self, path: &Path, result_id: Option<String>) {
        if let Some(document) = self.documents.get_mut(path) {
            document.result_id = result_id;
        }
    }

    /// Publishes the answer of one pull that the session asked for.
    ///
    /// Diagnostics are decoration, and no editor request waits for this answer,
    /// so a failed, timed-out, or obsolete pull leaves the previous set and
    /// reports nothing. A fatal failure still ends the session attempt.
    async fn report_pull(
        &mut self,
        path: &Path,
        result: Result<Option<LanguageOutcome>, LspError>,
    ) -> Result<(), LspError> {
        self.finish_pull(path);
        match result {
            Ok(None) => Ok(()),
            Ok(Some(outcome)) => self.publish(outcome).await,
            Err(error) if error.is_fatal() => Err(error),
            Err(_) => Ok(()),
        }
    }

    /// Reports whether this session asks for its diagnostics.
    fn pulls(&self) -> bool {
        matches!(self.diagnostics, DiagnosticsModel::Pull { .. })
    }

    /// Records that one pull of one document no longer waits for an answer.
    fn finish_pull(&mut self, path: &Path) {
        if let Some(document) = self.documents.get_mut(path) {
            document.pull_running = false;
        }
    }

    /// Makes one pull of every open document due.
    ///
    /// The server asks for this with `workspace/diagnostic/refresh`.
    fn schedule_every_pull(&mut self) {
        if !self.pulls() {
            return;
        }
        let due = Instant::now();
        for document in self.documents.values_mut() {
            document.pull_due = Some(due);
        }
    }

    /// Sends every pull that is due.
    ///
    /// A document that already holds a running pull waits for its answer, so one
    /// document never holds two pulls. A session that already holds
    /// [`LSP_PENDING_REQUESTS_MAX`] requests delays the pull by one settle delay
    /// instead of failing it. See `docs/language-services.md`.
    async fn fire_pulls(&mut self) -> Result<(), LspError> {
        let DiagnosticsModel::Pull { identifier } = &self.diagnostics else {
            return Ok(());
        };
        let identifier = identifier.clone();
        let now = Instant::now();
        let due: Vec<PathBuf> = self
            .documents
            .iter()
            .filter(|(_, document)| document.is_pull_due(now))
            .map(|(path, _)| path.clone())
            .collect();
        for path in due {
            if self.pending.len() >= LSP_PENDING_REQUESTS_MAX {
                if let Some(document) = self.documents.get_mut(&path) {
                    document.pull_due = Some(now + LSP_DIAGNOSTIC_PULL_DELAY);
                }
                continue;
            }
            self.pull(&path, identifier.as_deref()).await?;
        }
        Ok(())
    }

    /// Asks the server for the diagnostics of one document.
    ///
    /// The request carries the provider identifier of the capability and the
    /// result identifier of the previous report, so the server may answer that
    /// the previous set is unchanged.
    async fn pull(&mut self, path: &Path, identifier: Option<&str>) -> Result<(), LspError> {
        let Some(document) = self.documents.get(path) else {
            return Ok(());
        };
        let version = document.version;
        let mut params = json!({ "textDocument": { "uri": document.uri } });
        if let Some(identifier) = identifier {
            params["identifier"] = Value::String(identifier.to_owned());
        }
        if let Some(result_id) = document.result_id.clone() {
            params["previousResultId"] = Value::String(result_id);
        }
        let protocol_id = self
            .writer
            .request(Query::Diagnostics.method(), params)
            .await?;
        self.pending.insert(
            protocol_id,
            PendingRequest {
                id: None,
                path: path.to_path_buf(),
                version,
                query: Query::Diagnostics,
                deadline: Instant::now() + Query::Diagnostics.deadline(),
            },
        );
        if let Some(document) = self.documents.get_mut(path) {
            document.pull_due = None;
            document.pull_running = true;
        }
        Ok(())
    }

    /// Fails every request that passed its deadline.
    async fn expire(&mut self) -> Result<(), LspError> {
        let now = Instant::now();
        let expired: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(protocol_id, _)| *protocol_id)
            .collect();
        for protocol_id in expired {
            let Some(pending) = self.pending.remove(&protocol_id) else {
                continue;
            };
            // The server keeps working unless the client withdraws the request.
            self.writer
                .notify("$/cancelRequest", json!({ "id": protocol_id }))
                .await?;
            match pending.id {
                Some(request) => {
                    self.publish(LanguageOutcome::Failed {
                        request: Some(request),
                        error: LspError::Timeout,
                    })
                    .await?;
                }
                // No editor request waits for a pull, and diagnostics are
                // decoration, so a timeout leaves the previous set.
                None => self.finish_pull(&pending.path),
            }
        }
        Ok(())
    }

    /// Returns the earliest deadline of the waiting requests and the due pulls.
    fn next_deadline(&self) -> Option<Instant> {
        let requests = self.pending.values().map(|pending| pending.deadline);
        let pulls = self
            .documents
            .values()
            .filter(|document| !document.pull_running)
            .filter_map(|document| document.pull_due);
        requests.chain(pulls).min()
    }

    /// Publishes one value, or reports the typed failure of one request.
    ///
    /// A fatal failure ends the attempt. Every other failure belongs to one
    /// request, and the session continues.
    async fn report(
        &mut self,
        request: Option<LanguageRequestId>,
        result: Result<Option<LanguageOutcome>, LspError>,
    ) -> Result<(), LspError> {
        match result {
            Ok(None) => Ok(()),
            Ok(Some(outcome)) => self.publish(outcome).await,
            Err(error) if error.is_fatal() => Err(error),
            Err(error) => {
                self.publish(LanguageOutcome::Failed { request, error })
                    .await
            }
        }
    }

    /// Sends one result to the editor.
    ///
    /// The send waits for queue space. It never blocks the terminal event loop,
    /// because the event loop is the reader of that queue.
    async fn publish(&mut self, outcome: LanguageOutcome) -> Result<(), LspError> {
        self.events
            .send(LanguageEvent {
                server: self.config.id,
                outcome,
            })
            .await
            .map_err(|_| LspError::Stopped)
    }
}

/// Waits until one deadline, or forever when no request waits.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// The `kind` value of a report that repeats the previous set.
const UNCHANGED_REPORT: &str = "unchanged";

/// The wire shape of one pulled diagnostic report.
///
/// The shape names no `relatedDocuments` member, so the session never parses
/// one and allocates nothing for it. See `docs/language-services.md`.
#[derive(Debug, Deserialize)]
struct PulledReport {
    /// `full` for a complete report, and `unchanged` for the previous set.
    #[serde(default)]
    kind: String,
    /// The identifier that the next pull of this document repeats.
    #[serde(default, rename = "resultId")]
    result_id: Option<String>,
    /// The unparsed items of a full report.
    #[serde(default)]
    items: Option<Box<RawValue>>,
}

/// The wire shape of one workspace configuration request.
#[derive(Debug, Deserialize)]
struct ConfigurationParams {
    /// The unparsed items of the request.
    items: Box<RawValue>,
}

/// The wire shape of one requested configuration section.
#[derive(Debug, Deserialize)]
struct ConfigurationItem {
    /// The section name, which may be absent or empty.
    #[serde(default)]
    section: Option<String>,
}

/// Selects the change notification from the capability of one server.
///
/// The protocol carries `textDocumentSync` as one number, or as one object that
/// names that number in its `change` member. An absent capability, an absent
/// `change` member, and every number that is not 1 or 2 all name no
/// synchronization, which is the value that the protocol defines. See
/// `docs/language-services.md`.
fn synchronization_mode(capability: Option<&Value>) -> SynchronizationMode {
    let Some(capability) = capability else {
        return SynchronizationMode::None;
    };
    let kind = match capability {
        Value::Object(options) => options.get("change").and_then(Value::as_u64),
        other => other.as_u64(),
    };
    kind.map_or(SynchronizationMode::None, SynchronizationMode::from_kind)
}

/// Selects the diagnostic model from the capability of one server.
///
/// A server that advertises a diagnostic provider answers the request of the
/// client instead of publishing a set on its own. See
/// `docs/language-services.md`.
///
/// # Errors
///
/// Returns [`LspError::Bounds`] for a provider identifier above
/// [`LSP_RESULT_ID_BYTES_MAX`].
fn diagnostics_model(provider: Option<&Value>) -> Result<DiagnosticsModel, LspError> {
    let Some(provider) = provider.filter(|provider| !provider.is_null()) else {
        return Ok(DiagnosticsModel::Push);
    };
    let identifier = provider.get("identifier").and_then(Value::as_str);
    if let Some(identifier) = identifier {
        enforce(
            identifier.len(),
            LSP_RESULT_ID_BYTES_MAX,
            LspBound::ResultIdBytes,
        )?;
    }
    Ok(DiagnosticsModel::Pull {
        identifier: identifier.map(str::to_owned),
    })
}

/// Reads the bounded items of one workspace configuration request.
///
/// # Errors
///
/// Returns [`LspError::MalformedResponse`] for another shape, and
/// [`LspError::Bounds`] above [`LSP_CONFIGURATION_ITEMS_MAX`].
fn configuration_items(params: Option<&RawValue>) -> Result<Vec<ConfigurationItem>, LspError> {
    let params = params.ok_or(LspError::MalformedResponse)?;
    let params: ConfigurationParams =
        serde_json::from_str(params.get()).map_err(|_| LspError::MalformedResponse)?;
    let mut budget = ArrayBudget::new(LSP_CONFIGURATION_ITEMS_MAX, LSP_CONFIGURATION_ITEMS_MAX);
    deserialize_bounded_array(
        &params.items,
        LSP_CONFIGURATION_ITEMS_MAX,
        LspBound::ConfigurationItems,
        &mut budget,
    )
}

/// Returns the declared value of one requested configuration section.
///
/// An absent section and an empty section both name the complete object, which
/// is the shape that the protocol defines. A section that the object does not
/// hold answers the null value.
fn configuration_section(settings: &Value, section: Option<&str>) -> Value {
    let Some(section) = section.filter(|section| !section.is_empty()) else {
        return settings.clone();
    };
    let mut value = settings;
    for part in section.split('.') {
        match value.get(part) {
            Some(member) => value = member,
            None => return Value::Null,
        }
    }
    value.clone()
}

/// The wire shape of one diagnostics notification.
#[derive(Debug, Deserialize)]
struct PublishedDiagnostics {
    uri: String,
    #[serde(default)]
    version: Option<i64>,
    diagnostics: Box<RawValue>,
}

/// The wire shape of one definition target.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawLocation {
    /// A plain location, which names the document and the range.
    Direct { uri: String, range: ProtocolSpan },
    /// A location link, which names the target document and its range.
    Link {
        #[serde(rename = "targetUri")]
        target_uri: String,
        #[serde(rename = "targetSelectionRange")]
        target_selection_range: ProtocolSpan,
    },
}

impl RawLocation {
    /// Returns the URI and the range of the target.
    fn parts(self) -> (String, ProtocolSpan) {
        match self {
            Self::Direct { uri, range } => (uri, range),
            Self::Link {
                target_uri,
                target_selection_range,
            } => (target_uri, target_selection_range),
        }
    }
}

/// Returns the bounded text of one hover answer and the markup that covers it.
fn hover_markup(
    result: &RawValue,
    registry: LanguageRegistry,
    highlighter: &mut SyntaxHighlighter,
) -> Result<Option<MarkupText>, LspError> {
    let value: Value =
        serde_json::from_str(result.get()).map_err(|_| LspError::MalformedResponse)?;
    let Some(contents) = value.get("contents") else {
        return Ok(None);
    };
    hover_contents(contents, registry, highlighter)
}

/// Returns the bounded answer of one hover `contents` value.
///
/// The protocol allows a string, a marked block, a markup block, or an array of
/// those. One reader serves every shape and answers the one kind that covers
/// the complete text. See `docs/language-services.md`.
fn hover_contents(
    contents: &Value,
    registry: LanguageRegistry,
    highlighter: &mut SyntaxHighlighter,
) -> Result<Option<MarkupText>, LspError> {
    let mut answer = HoverAnswer::default();
    answer.append(contents);
    enforce(answer.text.len(), LSP_HOVER_BYTES_MAX, LspBound::HoverBytes)?;
    Ok(answer.finish(registry, highlighter))
}

/// The collected parts of one hover answer and the kind that covers them.
#[derive(Debug, Default)]
struct HoverAnswer {
    /// The parts, one part on each line.
    text: String,
    /// The kind of the parts so far, or `None` while no part arrived.
    kind: Option<MarkupKind>,
}

impl HoverAnswer {
    /// Appends every part of one hover content value.
    fn append(&mut self, contents: &Value) {
        match contents {
            // A bare string is the deprecated `MarkedString`, which the
            // protocol defines as markdown.
            Value::String(value) => self.push(value, MarkupKind::Markdown),
            Value::Object(object) => {
                if let Some((part, kind)) = hover_part(object) {
                    self.push(&part, kind);
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.append(value);
                }
            }
            _ => {}
        }
    }

    /// Appends one part and keeps the parts on separate lines.
    fn push(&mut self, part: &str, kind: MarkupKind) {
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(part);
        self.kind = Some(match self.kind {
            Some(seen) => seen.merged(kind),
            None => kind,
        });
    }

    /// Returns the trimmed answer, or `None` when no part carried text.
    ///
    /// A markdown answer also carries its document, with the code of each fence
    /// named. This session runs off the terminal event loop, so the highlight
    /// of a fence runs here and the float paints a finished value.
    ///
    /// A plain text carries an empty document, because a markdown parse of a
    /// plain text removes the characters that mark up a document.
    fn finish(
        self,
        registry: LanguageRegistry,
        highlighter: &mut SyntaxHighlighter,
    ) -> Option<MarkupText> {
        let text = self.text.trim().to_owned();
        if text.is_empty() {
            return None;
        }
        debug_assert!(
            self.kind.is_some(),
            "a text of this answer arrived with one part, and every part names its kind"
        );
        let kind = self.kind.unwrap_or(MarkupKind::PlainText);
        let document = match kind {
            MarkupKind::Markdown => MarkupDocument::parse(&text).highlighted(registry, highlighter),
            MarkupKind::PlainText => MarkupDocument::default(),
        };
        Some(MarkupText {
            kind,
            text,
            document,
        })
    }
}

/// Returns the text and the markup kind of one hover object part.
///
/// `MarkupContent` names its kind and carries its text unchanged. The
/// deprecated object form of `MarkedString` names a `language` instead, and the
/// protocol defines that form as one fenced markdown code block, so the reader
/// writes that fence. An object that names neither is no shape of the protocol,
/// so it takes plain text, which loses no character. An object without a
/// `value` carries no text at all.
fn hover_part(object: &Map<String, Value>) -> Option<(String, MarkupKind)> {
    let Some(Value::String(value)) = object.get("value") else {
        return None;
    };
    if let Some(Value::String(name)) = object.get("kind")
        && let Some(kind) = MarkupKind::from_protocol(name)
    {
        return Some((value.clone(), kind));
    }
    if let Some(Value::String(language)) = object.get("language") {
        let fence = "`".repeat(fence_backticks(value));
        return Some((
            format!("{fence}{language}\n{value}\n{fence}"),
            MarkupKind::Markdown,
        ));
    }
    Some((value.clone(), MarkupKind::PlainText))
}

/// Returns the number of backticks that one fence around `value` needs.
///
/// CommonMark closes a fence at the first line that holds as many backticks as
/// the opening one, so a fence around a text that holds backticks must be
/// longer than the longest run inside that text.
fn fence_backticks(value: &str) -> usize {
    /// The backticks of the shortest fence of CommonMark.
    const FENCE_BACKTICKS_MIN: usize = 3;

    let mut longest = 0_usize;
    let mut run = 0_usize;
    for character in value.chars() {
        run = if character == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest.saturating_add(1).max(FENCE_BACKTICKS_MIN)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::SyntaxHighlighter;
    use crate::markup::MarkupDocument;

    use super::{
        LanguageRegistry, MarkupKind, MarkupText, SynchronizationMode, hover_contents,
        synchronization_mode,
    };

    /// Returns the mode of one `textDocumentSync` capability value.
    fn mode(capability: &Value) -> SynchronizationMode {
        synchronization_mode(capability.pointer("/capabilities/textDocumentSync"))
    }

    /// Returns the answer of one hover `contents` value.
    fn answer(contents: &Value) -> Option<MarkupText> {
        hover_contents(
            contents,
            LanguageRegistry::first_release(),
            &mut SyntaxHighlighter::new(),
        )
        .expect("the text stays under the bound")
    }

    #[test]
    fn the_number_form_of_the_capability_names_the_mode() {
        assert_eq!(
            mode(&json!({ "capabilities": { "textDocumentSync": 1 } })),
            SynchronizationMode::Full
        );
        assert_eq!(
            mode(&json!({ "capabilities": { "textDocumentSync": 2 } })),
            SynchronizationMode::Incremental
        );
        assert_eq!(
            mode(&json!({ "capabilities": { "textDocumentSync": 0 } })),
            SynchronizationMode::None
        );
    }

    #[test]
    fn the_object_form_of_the_capability_names_the_mode_in_its_change_member() {
        assert_eq!(
            mode(&json!({
                "capabilities": {
                    "textDocumentSync": { "openClose": true, "change": 1 }
                }
            })),
            SynchronizationMode::Full
        );
        assert_eq!(
            mode(&json!({
                "capabilities": {
                    "textDocumentSync": { "openClose": true, "change": 2 }
                }
            })),
            SynchronizationMode::Incremental
        );
    }

    #[test]
    fn every_capability_that_names_no_mode_sends_no_change() {
        // The protocol defines no synchronization for an absent capability, for
        // an object without a `change` member, and for the value 0.
        assert_eq!(
            mode(&json!({ "capabilities": {} })),
            SynchronizationMode::None
        );
        assert_eq!(
            mode(&json!({ "capabilities": { "textDocumentSync": Value::Null } })),
            SynchronizationMode::None
        );
        assert_eq!(
            mode(&json!({ "capabilities": { "textDocumentSync": { "openClose": true } } })),
            SynchronizationMode::None
        );
        // The protocol reserves no further number and no other type, so both
        // send no change instead of a shape that the server misreads.
        assert_eq!(
            mode(&json!({ "capabilities": { "textDocumentSync": 3 } })),
            SynchronizationMode::None
        );
        assert_eq!(
            mode(&json!({ "capabilities": { "textDocumentSync": "full" } })),
            SynchronizationMode::None
        );
    }

    #[test]
    fn a_markup_block_carries_the_kind_that_it_names() {
        let markdown = answer(&json!({ "kind": "markdown", "value": "`fn main()`" }))
            .expect("the block carries text");
        assert_eq!(markdown.kind, MarkupKind::Markdown);
        assert_eq!(markdown.text, "`fn main()`");

        let plain = answer(&json!({ "kind": "plaintext", "value": "a * b" }))
            .expect("the block carries text");
        assert_eq!(plain.kind, MarkupKind::PlainText);
        assert_eq!(plain.text, "a * b");
    }

    #[test]
    fn every_deprecated_marked_string_carries_markdown() {
        // The protocol defines a bare string as markdown, and it defines the
        // pair of a language and a value as one fenced markdown code block.
        let bare = answer(&json!("*emphasis*")).expect("the string carries text");
        assert_eq!(bare.kind, MarkupKind::Markdown);

        let fenced = answer(&json!({ "language": "rust", "value": "fn main()" }))
            .expect("the block carries text");
        assert_eq!(fenced.kind, MarkupKind::Markdown);
        assert_eq!(
            fenced.text, "```rust\nfn main()\n```",
            "the pair of a language and a value is one code block, so the reader writes its fence"
        );

        let array = answer(&json!([
            { "language": "rust", "value": "fn main()" },
            "*emphasis*",
        ]))
        .expect("the array carries text");
        assert_eq!(array.kind, MarkupKind::Markdown);
        assert_eq!(array.text, "```rust\nfn main()\n```\n*emphasis*");
    }

    #[test]
    fn a_deprecated_pair_that_holds_a_fence_keeps_its_whole_value() {
        // CommonMark closes a fence at the first line that holds as many
        // backticks as the opening one, so the fence must be the longer one.
        let fenced = answer(&json!({ "language": "md", "value": "a\n```\nb\n```\nc" }))
            .expect("the block carries text");

        assert_eq!(fenced.text, "````md\na\n```\nb\n```\nc\n````");
        let document = MarkupDocument::parse(&fenced.text);
        assert_eq!(
            document.blocks().len(),
            1,
            "the value stands in one code block: {document:?}"
        );
    }

    #[test]
    fn one_part_of_plain_text_makes_the_whole_answer_plain_text() {
        // A parser that reads plain text as markdown loses the characters that
        // mark up a document, so the safe kind covers the joined text.
        let mixed = answer(&json!([
            "*emphasis*",
            { "kind": "plaintext", "value": "a * b" },
        ]))
        .expect("the array carries text");
        assert_eq!(mixed.kind, MarkupKind::PlainText);
        assert_eq!(mixed.text, "*emphasis*\na * b");
    }

    #[test]
    fn a_kind_that_the_protocol_defines_nowhere_takes_plain_text() {
        // An object that names no kind and no language is no shape of the
        // protocol, and neither is an unknown kind name.
        let nameless = answer(&json!({ "value": "a * b" })).expect("the object carries text");
        assert_eq!(nameless.kind, MarkupKind::PlainText);

        let unknown =
            answer(&json!({ "kind": "html", "value": "a * b" })).expect("the object carries text");
        assert_eq!(unknown.kind, MarkupKind::PlainText);
    }

    #[test]
    fn an_answer_without_text_names_no_markup() {
        assert!(answer(&json!("   ")).is_none());
        assert!(answer(&json!([])).is_none());
        assert!(answer(&json!({ "kind": "markdown" })).is_none());
    }
}
