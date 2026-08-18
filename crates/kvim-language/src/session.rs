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
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;

use kvim_core::BufferVersion;
use kvim_settings::IndentSettings;

use super::document::{
    ContentChange, DiagnosticSet, FormatEdits, RawDiagnostic, RawTextEdit, SourceLocation, TextEdit,
};
use super::progress::{ProgressReport, SessionGeneration, parse as parse_progress};
use super::protocol::{
    ArrayBudget, DocumentPosition, LspBound, LspError, POSITION_ENCODING, ProtocolReader,
    ProtocolWriter, RpcEnvelope, RpcId, SourceSpan, WorkspaceRoot, deserialize_bounded_array,
    enforce,
};

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

/// The restarts that one session performs after a server failure.
pub const LSP_RESTARTS_MAX: usize = 3;

/// The deadline of the `initialize` handshake.
pub const LSP_INITIALIZE_DEADLINE: Duration = Duration::from_secs(30);

/// The deadline of one definition or hover request.
pub const LSP_REQUEST_DEADLINE: Duration = Duration::from_secs(5);

/// The deadline of one document formatting request.
pub const LSP_FORMAT_DEADLINE: Duration = Duration::from_secs(10);

/// The deadline of the `shutdown` and `exit` sequence.
pub const LSP_SHUTDOWN_DEADLINE: Duration = Duration::from_millis(250);

/// The notification that publishes the diagnostics of one document.
const DIAGNOSTICS_METHOD: &str = "textDocument/publishDiagnostics";

/// The notification that reports the state of one long server operation.
const PROGRESS_METHOD: &str = "$/progress";

/// The server request that creates one work-done progress token.
const PROGRESS_CREATE_METHOD: &str = "window/workDoneProgress/create";

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
        /// The hover text, or `None` when the server has nothing to say.
        text: Option<String>,
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
}

/// One typed result and the adapter whose session produced it.
#[derive(Debug)]
pub struct LanguageEvent {
    /// The identifier of the adapter that owns the session.
    pub adapter: &'static str,
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
}

impl Query {
    /// Returns the protocol method of this query.
    const fn method(self) -> &'static str {
        match self {
            Self::Definition(_) => "textDocument/definition",
            Self::Hover(_) => "textDocument/hover",
            Self::Format => "textDocument/formatting",
        }
    }

    /// Returns the deadline of this query.
    ///
    /// A formatter runs a complete pass over the document, so it needs more
    /// time than a position query.
    const fn deadline(self) -> Duration {
        match self {
            Self::Definition(_) | Self::Hover(_) => LSP_REQUEST_DEADLINE,
            Self::Format => LSP_FORMAT_DEADLINE,
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
    adapter: &'static str,
    requests: mpsc::Sender<SessionRequest>,
    next_id: AtomicU64,
    cancellation: CancellationToken,
}

impl LanguageServerHandle {
    /// Returns the identifier of the adapter that owns the session.
    #[must_use]
    pub const fn adapter(&self) -> &'static str {
        self.adapter
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
    /// The identifier of the adapter that owns the session.
    pub(super) adapter: &'static str,
    /// The protocol language identifier of every document of this session.
    pub(super) language_id: &'static str,
    /// The program that runs the server, which titles one overlay group.
    pub(super) server: &'static str,
    /// The containment boundary of every path and every `file` URI.
    pub(super) root: WorkspaceRoot,
    /// The initialization options that the adapter declared.
    pub(super) options: Value,
    /// The indent settings that one formatting request sends.
    pub(super) indent: IndentSettings,
    /// Whether the session parses and publishes diagnostics.
    pub(super) diagnostics_enabled: bool,
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
                    // The server log is not editor state, so it goes nowhere and
                    // cannot fill a pipe that no reader drains.
                    .stderr(Stdio::null())
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
                Ok(Transport {
                    input: Box::new(input),
                    output: Box::new(output),
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
}

/// One request that waits for an answer.
struct PendingRequest {
    /// The editor identity of the request.
    id: LanguageRequestId,
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
        adapter: config.adapter,
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
                emit(&events, config.adapter, LanguageOutcome::Unavailable).await;
                return;
            }
            AttemptOutcome::Failed(error) => {
                emit(
                    &events,
                    config.adapter,
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
                emit(&events, config.adapter, LanguageOutcome::Restarted).await;
            }
        }
    }
    emit(&events, config.adapter, LanguageOutcome::Stopped).await;
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
        mut child,
    } = transport;
    let (envelope_sender, mut envelopes) = mpsc::channel(LSP_EVENT_QUEUE_CAPACITY);
    // The frame reader owns its stream in one task, so no cancelled future can
    // drop a partly read frame and desynchronize the stream.
    let reader = tokio::spawn(read_envelopes(ProtocolReader::new(output), envelope_sender));
    let mut session = Session {
        config,
        generation,
        events,
        writer: ProtocolWriter::new(input),
        documents: HashMap::new(),
        pending: HashMap::new(),
    };

    let outcome = session.serve(&mut envelopes, requests, cancellation).await;
    if matches!(outcome, AttemptOutcome::Stopped) {
        let _ = time::timeout(LSP_SHUTDOWN_DEADLINE, session.shutdown(&mut envelopes)).await;
    }
    reader.abort();
    if let Some(child) = child.as_mut() {
        terminate(child).await;
    }
    outcome
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
    adapter: &'static str,
    outcome: LanguageOutcome,
) {
    let _ = events.send(LanguageEvent { adapter, outcome }).await;
}

/// The live state of one server attempt.
struct Session<'a> {
    config: &'a SessionConfig,
    /// The attempt that this session serves, which every progress report names.
    generation: SessionGeneration,
    events: &'a mpsc::Sender<LanguageEvent>,
    writer: ProtocolWriter<Box<dyn AsyncWrite + Send + Unpin>>,
    documents: HashMap<PathBuf, OpenDocument>,
    pending: HashMap<u64, PendingRequest>,
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
        }
    }

    /// Declares the client capabilities and requires the position encoding.
    async fn initialize(&mut self, envelopes: &mut Envelopes) -> Result<(), LspError> {
        let root_uri = self.config.root.root_uri()?;
        let id = self
            .writer
            .request(
                "initialize",
                json!({
                    "processId": Value::Null,
                    "rootUri": root_uri,
                    "capabilities": {
                        "general": { "positionEncodings": [POSITION_ENCODING] },
                        // A server sends `$/progress` only after the client
                        // declares that it shows work-done progress.
                        "window": { "workDoneProgress": true },
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
                                "contentFormat": ["plaintext", "markdown"],
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
        // Kvim measures every column in UTF-8 bytes. A server that answers in
        // another encoding would report ranges that the buffer does not hold.
        if capabilities
            .pointer("/capabilities/positionEncoding")
            .and_then(Value::as_str)
            != Some(POSITION_ENCODING)
        {
            return Err(LspError::UnsupportedEncoding);
        }
        self.writer.notify("initialized", json!({})).await
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
                // An unanswered server request stalls the server, so Kvim always
                // answers. It accepts the creation of one progress token,
                // because the overlay shows the reports of that token, and it
                // reports every other method as unknown.
                return if method == PROGRESS_CREATE_METHOD {
                    self.writer.accept_server_request(id).await
                } else {
                    self.writer.reject_server_request(id).await
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
            (None, Some(value)) => self.convert(&pending, &value).map(Some),
            (None, None) => Err(LspError::MalformedResponse),
        };
        self.report(Some(pending.id), result).await
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
        let diagnostics = raw
            .into_iter()
            .map(RawDiagnostic::into_diagnostic)
            .collect();
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
        self.documents.insert(
            path,
            OpenDocument {
                uri,
                version,
                revision: 1,
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
        let document = self
            .documents
            .get_mut(path)
            .ok_or(LspError::DocumentNotOpen)?;
        let revision = document.revision.saturating_add(1);
        let content_changes = changes
            .iter()
            .map(|change| json!({ "range": change.span, "text": change.text }))
            .collect::<Vec<_>>();
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
        // failed write leaves the recorded version and revision untouched.
        let document = self
            .documents
            .get_mut(path)
            .ok_or(LspError::DocumentNotOpen)?;
        document.revision = revision;
        document.version = version;
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
        let params = self.query_params(&document.uri, query);
        let protocol_id = self.writer.request(query.method(), params).await?;
        self.pending.insert(
            protocol_id,
            PendingRequest {
                id,
                path,
                version,
                query,
                deadline: Instant::now() + query.deadline(),
            },
        );
        Ok(())
    }

    fn query_params(&self, uri: &str, query: Query) -> Value {
        match query {
            Query::Definition(position) | Query::Hover(position) => json!({
                "textDocument": { "uri": uri },
                "position": position,
            }),
            Query::Format => json!({
                "textDocument": { "uri": uri },
                "options": {
                    "tabSize": u32::from(self.config.indent.tab_width.get()),
                    "insertSpaces": self.config.indent.expand_tab,
                },
            }),
        }
    }

    /// Converts one answer while its buffer version is still current.
    fn convert(
        &self,
        pending: &PendingRequest,
        result: &RawValue,
    ) -> Result<LanguageOutcome, LspError> {
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
                request: pending.id,
                version: pending.version,
                locations: self.definition_locations(result)?,
            }),
            Query::Hover(_) => Ok(LanguageOutcome::Hover {
                request: pending.id,
                version: pending.version,
                text: hover_text(result)?,
            }),
            Query::Format => {
                let mut budget = ArrayBudget::new(LSP_FORMAT_EDITS_MAX, LSP_FORMAT_EDITS_MAX);
                let raw: Vec<RawTextEdit> = deserialize_bounded_array(
                    result,
                    LSP_FORMAT_EDITS_MAX,
                    LspBound::FormatEdits,
                    &mut budget,
                )?;
                let edits: Vec<TextEdit> = raw.into_iter().map(RawTextEdit::into_edit).collect();
                Ok(LanguageOutcome::Formatting {
                    request: pending.id,
                    edits: FormatEdits::new(pending.path.clone(), pending.version, edits),
                })
            }
        }
    }

    /// Converts one definition answer into contained workspace locations.
    ///
    /// A target outside the workspace root is rejected and never offered.
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
        Ok(raw
            .into_iter()
            .filter_map(|location| {
                let (uri, span) = location.parts();
                let path = self.config.root.path_from_uri(&uri).ok()?;
                Some(SourceLocation { path, span })
            })
            .collect())
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
            self.publish(LanguageOutcome::Failed {
                request: Some(pending.id),
                error: LspError::Timeout,
            })
            .await?;
        }
        Ok(())
    }

    /// Returns the earliest deadline of the waiting requests.
    fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|pending| pending.deadline).min()
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
                adapter: self.config.adapter,
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
    Direct { uri: String, range: SourceSpan },
    /// A location link, which names the target document and its range.
    Link {
        #[serde(rename = "targetUri")]
        target_uri: String,
        #[serde(rename = "targetSelectionRange")]
        target_selection_range: SourceSpan,
    },
}

impl RawLocation {
    /// Returns the URI and the range of the target.
    fn parts(self) -> (String, SourceSpan) {
        match self {
            Self::Direct { uri, range } => (uri, range),
            Self::Link {
                target_uri,
                target_selection_range,
            } => (target_uri, target_selection_range),
        }
    }
}

/// Returns the bounded plain text of one hover answer.
fn hover_text(result: &RawValue) -> Result<Option<String>, LspError> {
    let value: Value =
        serde_json::from_str(result.get()).map_err(|_| LspError::MalformedResponse)?;
    let Some(contents) = value.get("contents") else {
        return Ok(None);
    };
    let mut text = String::new();
    append_hover_text(contents, &mut text);
    enforce(text.len(), LSP_HOVER_BYTES_MAX, LspBound::HoverBytes)?;
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(text))
}

/// Collects the text of one hover content value.
///
/// The protocol allows a string, a marked block, a markup block, or an array of
/// those. One reader serves every shape.
fn append_hover_text(contents: &Value, text: &mut String) {
    match contents {
        Value::String(value) => push_hover_part(text, value),
        Value::Object(object) => {
            if let Some(Value::String(value)) = object.get("value") {
                push_hover_part(text, value);
            }
        }
        Value::Array(values) => {
            for value in values {
                append_hover_text(value, text);
            }
        }
        _ => {}
    }
}

/// Appends one hover part and keeps the parts on separate lines.
fn push_hover_part(text: &mut String, part: &str) {
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(part);
}
