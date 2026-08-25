//! The child process, the handshake, and the shutdown of one server session.
//! Adapted from ReviewGraph (MIT), src/analysis/lsp.rs.
//!
//! One session owns one server process. This module starts that process, reads
//! its frames in one task, drains its standard error in a second task, runs the
//! `initialize` handshake, and ends the process inside a deadline.
//!
//! [`ServerProcess`] owns the child. Dropping the value stops both reader tasks
//! and kills the child, so a cancelled caller leaves no untracked process.
//!
//! The module speaks the protocol only. The caller supplies the program, the
//! arguments, the working directory, the initialization options, and the
//! workspace settings as data, so no code in this file names one server
//! product. See `docs/language-services.md`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::value::RawValue;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::encoding::{PositionEncoding, TextMirroring};
use crate::protocol::{
    LspBound, LspError, ProtocolReader, ProtocolWriter, RpcEnvelope, RpcId, WorkspaceRoot, enforce,
};

/// The bytes of the standard error of one server attempt that the caller
/// records.
///
/// A server that fails names its cause in its first lines, so this bound holds
/// that cause. The reader drains every further byte of that attempt and records
/// none of it. A server that writes without limit therefore still runs, and it
/// costs bounded memory. See `docs/language-services.md`.
pub const LSP_STDERR_BYTES_MAX: usize = 64 * 1024;

/// The bytes that one recorded standard error line keeps.
///
/// One line of a server log names one state. A log entry clips one line
/// further, so this bound protects the reader from a stream that carries no
/// line break.
pub const LSP_STDERR_LINE_BYTES_MAX: usize = 1024;

/// The bytes that one read of the standard error takes.
///
/// The value is the size of one read buffer, not a bound on the recorded text.
/// [`LSP_STDERR_BYTES_MAX`] and [`LSP_STDERR_LINE_BYTES_MAX`] bound that text.
const STDERR_CHUNK_BYTES: usize = 4 * 1024;

/// The frames that the reader task holds for the session.
///
/// The reader task owns the byte stream, so no cancelled future can drop a
/// partly read frame and desynchronize that stream. This bound holds the frames
/// that the session did not read yet.
pub const LSP_ENVELOPE_QUEUE_CAPACITY: usize = 256;

/// The largest result identifier that one pulled report may carry, in bytes.
///
/// The identifier of a diagnostic provider passes the same bound, because a
/// session repeats it in every pull. See `docs/language-services.md`.
pub const LSP_RESULT_ID_BYTES_MAX: usize = 256;

/// The restarts that one session performs after a server failure.
pub const LSP_RESTARTS_MAX: usize = 3;

/// The deadline of the `initialize` handshake.
pub const LSP_INITIALIZE_DEADLINE: Duration = Duration::from_secs(30);

/// The deadline of the `shutdown` and `exit` sequence.
pub const LSP_SHUTDOWN_DEADLINE: Duration = Duration::from_millis(250);

/// The stream that one session writes its messages to.
pub type ServerInput = Box<dyn AsyncWrite + Send + Unpin>;

/// The frames that the reader task of one server delivers.
///
/// A `None` element never arrives. The channel closes after the stream ended or
/// after one bound stopped the reader.
pub type Envelopes = mpsc::Receiver<Result<RpcEnvelope, LspError>>;

/// One recorded fact about the server process of one session.
///
/// A report changes no document and no request. A caller records it in its log,
/// so a reader finds the cause of a failure that the protocol never names.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{LSP_STDERR_LINE_BYTES_MAX, ServerReport};
///
/// let report = ServerReport::Output("error: no toolchain".to_owned());
/// let ServerReport::Output(line) = &report else { unreachable!() };
/// assert!(line.len() <= LSP_STDERR_LINE_BYTES_MAX);
/// ```
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
    /// The recorder keeps no further line of that attempt. It still drains the
    /// stream, so the server never blocks on a full pipe.
    OutputBound,
}

/// The byte streams of one server attempt.
///
/// The streams are trait objects, because a session runs over the pipes of a
/// child process in an editor and over an in-memory pair in a test.
pub struct Transport {
    input: ServerInput,
    output: Box<dyn AsyncRead + Send + Unpin>,
    /// The standard error of the child, which one background task drains.
    ///
    /// A prepared stream pair holds no standard error, so a test transport
    /// carries `None` and the attempt starts no recorder.
    errors: Option<Box<dyn AsyncRead + Send + Unpin>>,
    child: Option<Child>,
}

#[cfg(any(test, feature = "test-support"))]
impl Transport {
    /// Creates one transport over a prepared stream pair.
    ///
    /// The pair carries no child and no standard error, so a session over it
    /// starts no process. Only a test builds this value.
    #[must_use]
    pub fn prepared(
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
///
/// One factory serves the first attempt and every restart of one session, so a
/// caller declares the program once.
pub enum TransportFactory {
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
    ///
    /// # Errors
    ///
    /// Returns [`LspError::NotInstalled`] when the system holds no such
    /// executable, and [`LspError::Spawn`] for every other start failure. Both
    /// keep the cause of the operating system as the source of the failure.
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

/// The framed byte streams of one running server.
///
/// The two members stand apart, because one session writes while it waits for
/// the next frame. A caller destructures the value and keeps both halves.
pub struct ServerStreams {
    /// The writer of every request and every notification of the session.
    pub writer: ProtocolWriter<ServerInput>,
    /// The frames that the reader task delivers.
    pub envelopes: Envelopes,
}

/// One running language server and the tasks that read it.
///
/// The value owns the child process, the frame reader, and the standard-error
/// recorder. Dropping it aborts both tasks and kills the child, so no cancelled
/// caller leaves an untracked process. [`ServerProcess::close`] performs the
/// same work inside [`LSP_SHUTDOWN_DEADLINE`] and waits for the exit.
pub struct ServerProcess {
    /// The child, or `None` after [`ServerProcess::close`] ended it.
    ///
    /// The child carries `kill_on_drop`, so every path that drops this value
    /// also kills the process.
    child: Option<Child>,
    /// The task that reads one frame after another.
    reader: JoinHandle<()>,
    /// The task that drains the standard error, or `None` for a prepared pair.
    errors: Option<JoinHandle<()>>,
}

impl ServerProcess {
    /// Starts the next attempt of one session and returns its byte streams.
    ///
    /// `report` receives every recorded fact about the process. The call never
    /// waits, because the standard-error task calls it while it drains the
    /// pipe, and a call that waits would fill that pipe and stop the child.
    ///
    /// # Errors
    ///
    /// Returns the failures of the transport: [`LspError::NotInstalled`] for a
    /// missing executable, and [`LspError::Spawn`] for every other start
    /// failure.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::ffi::OsString;
    /// use std::path::PathBuf;
    ///
    /// use kvim_lsp::{LspError, ServerProcess, TransportFactory};
    ///
    /// # async fn open() -> Result<(), LspError> {
    /// let mut factory = TransportFactory::Process {
    ///     program: OsString::from("rust-analyzer"),
    ///     args: Vec::new(),
    ///     root: PathBuf::from("/work/project"),
    /// };
    /// let (process, streams) = ServerProcess::open(&mut factory, |report| {
    ///     eprintln!("{report:?}");
    /// })?;
    /// drop(streams);
    /// process.close().await;
    /// # Ok(())
    /// # }
    /// ```
    pub fn open<F>(
        factory: &mut TransportFactory,
        report: F,
    ) -> Result<(Self, ServerStreams), LspError>
    where
        F: Fn(ServerReport) + Send + 'static,
    {
        let Transport {
            input,
            output,
            errors,
            child,
        } = factory.create()?;
        // The standard error of the child needs a reader from the first byte,
        // because a pipe that nobody drains fills and stops the child. See
        // `docs/language-services.md`.
        let errors = errors.map(|stream| tokio::spawn(record_errors(stream, report)));
        let (sender, envelopes) = mpsc::channel(LSP_ENVELOPE_QUEUE_CAPACITY);
        // The frame reader owns its stream in one task, so no cancelled future
        // can drop a partly read frame and desynchronize the stream.
        let reader = tokio::spawn(read_envelopes(ProtocolReader::new(output), sender));
        let process = Self {
            child,
            reader,
            errors,
        };
        let streams = ServerStreams {
            writer: ProtocolWriter::new(input),
            envelopes,
        };
        Ok((process, streams))
    }

    /// Ends the process and waits a bounded time for the last recorded line.
    ///
    /// The call consumes the value, so no caller can read the process after it.
    /// Every step carries [`LSP_SHUTDOWN_DEADLINE`], so one server that never
    /// exits cannot stop the caller.
    pub async fn close(mut self) {
        self.reader.abort();
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let _ = time::timeout(LSP_SHUTDOWN_DEADLINE, child.wait()).await;
        }
        if let Some(mut task) = self.errors.take() {
            // The child ended, so the stream ends and the recorder keeps its
            // last line. Another process may still hold the write end of that
            // pipe, so the wait carries the deadline and the rest stays
            // unrecorded.
            if time::timeout(LSP_SHUTDOWN_DEADLINE, &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        self.reader.abort();
        if let Some(errors) = self.errors.as_ref() {
            errors.abort();
        }
        // The child carries `kill_on_drop`, so dropping the handle here also
        // kills the process. A cancelled session therefore leaves no untracked
        // child. See `docs/language-services.md`.
    }
}

/// The change notification that one session sends.
///
/// The handshake selects the mode from the `textDocumentSync` capability of the
/// server. See `docs/language-services.md`.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{SynchronizationMode, TextMirroring};
///
/// // A full synchronization sends the complete text, so its session mirrors
/// // the text that the server holds.
/// assert_eq!(SynchronizationMode::Full.mirroring(), TextMirroring::Present);
/// assert_eq!(SynchronizationMode::Incremental.mirroring(), TextMirroring::Absent);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SynchronizationMode {
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
    #[must_use]
    pub const fn mirroring(self) -> TextMirroring {
        match self {
            Self::Full => TextMirroring::Present,
            Self::None | Self::Incremental => TextMirroring::Absent,
        }
    }
}

/// The model that carries the diagnostics of one session.
///
/// The handshake selects the model from the `diagnosticProvider` capability of
/// the server. See `docs/language-services.md`.
///
/// # Examples
///
/// ```
/// use kvim_lsp::DiagnosticsModel;
///
/// let push = DiagnosticsModel::Push;
/// assert_eq!(push.identifier(), None);
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticsModel {
    /// The server publishes one set without a request.
    Push,
    /// The client asks, and the server answers one report.
    Pull {
        /// The provider identifier that every request repeats, when the
        /// capability names one.
        identifier: Option<String>,
    },
}

impl DiagnosticsModel {
    /// Returns the provider identifier that every pull of this model repeats.
    ///
    /// A push model and a pull capability without an identifier both answer
    /// `None`, so one accessor serves both.
    #[must_use]
    pub fn identifier(&self) -> Option<&str> {
        match self {
            Self::Push => None,
            Self::Pull { identifier } => identifier.as_deref(),
        }
    }
}

/// What one server confirmed in its handshake.
///
/// The values belong to one server attempt, so a restart reads them again. See
/// `docs/language-services.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerCapabilities {
    encoding: PositionEncoding,
    synchronization: SynchronizationMode,
    diagnostics: DiagnosticsModel,
}

impl ServerCapabilities {
    /// Returns the position encoding that the handshake negotiated.
    #[must_use]
    pub const fn encoding(&self) -> PositionEncoding {
        self.encoding
    }

    /// Returns the change notification that the handshake selected.
    #[must_use]
    pub const fn synchronization(&self) -> SynchronizationMode {
        self.synchronization
    }

    /// Returns the diagnostic model that the handshake selected.
    #[must_use]
    pub const fn diagnostics(&self) -> &DiagnosticsModel {
        &self.diagnostics
    }
}

/// What one client declares in its handshake.
///
/// Every member is data of the caller. The handshake sends what these members
/// name, so no code of this crate names one server product.
pub struct Handshake<'a> {
    /// The containment boundary of every path and every `file` URI.
    pub root: &'a WorkspaceRoot,
    /// The initialization options that the caller declared.
    pub options: &'a Value,
    /// The workspace settings that the caller declared, or `None`.
    ///
    /// A declaration that names settings opens the configuration channel of
    /// its session: the handshake declares the client capability, and the
    /// handshake sends one notification. See `docs/language-services.md`.
    pub settings: Option<&'a Value>,
}

/// Whether one handshake completed or the caller cancelled it.
pub enum HandshakeOutcome {
    /// The server confirmed its capabilities, so the session may serve.
    Ready(ServerCapabilities),
    /// The caller cancelled the session before the server answered.
    Cancelled,
}

/// Declares the client capabilities and negotiates the position encoding.
///
/// The call carries [`LSP_INITIALIZE_DEADLINE`] and `cancellation`, so a server
/// that never answers cannot hold the session. It answers every unsolicited
/// server request meanwhile, so an unimplemented request cannot stall the
/// server.
///
/// # Errors
///
/// Returns [`LspError::Timeout`] after the deadline,
/// [`LspError::UnsupportedEncoding`] when the server confirms an encoding that
/// the client never offered, [`LspError::MalformedResponse`] for another
/// answer, and the transport failures of the streams.
pub async fn initialize(
    writer: &mut ProtocolWriter<ServerInput>,
    envelopes: &mut Envelopes,
    declaration: &Handshake<'_>,
    cancellation: &CancellationToken,
) -> Result<HandshakeOutcome, LspError> {
    let negotiation = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Ok(HandshakeOutcome::Cancelled),
        result = time::timeout(
            LSP_INITIALIZE_DEADLINE,
            negotiate(writer, envelopes, declaration),
        ) => result.unwrap_or(Err(LspError::Timeout)),
    };
    negotiation.map(HandshakeOutcome::Ready)
}

/// Sends `initialize`, reads the capabilities, and opens the session.
async fn negotiate(
    writer: &mut ProtocolWriter<ServerInput>,
    envelopes: &mut Envelopes,
    declaration: &Handshake<'_>,
) -> Result<ServerCapabilities, LspError> {
    let root_uri = declaration.root.root_uri()?;
    // kvim declares the configuration capability only while its declaration
    // names settings, because a session without settings still reports the
    // request of a server as an unknown method.
    let configuration = declaration.settings.is_some();
    let id = writer
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
                "initializationOptions": declaration.options,
                "workspaceFolders": [{ "uri": root_uri, "name": "workspace" }],
            }),
        )
        .await?;
    let result = await_response(writer, envelopes, id).await?;
    let answer: Value =
        serde_json::from_str(result.get()).map_err(|_| LspError::MalformedResponse)?;
    // kvim measures every column in UTF-8 bytes, and the protocol measures one
    // column in UTF-16 code units unless the server confirms UTF-8. The session
    // records the answer and converts every column against it.
    let encoding = PositionEncoding::from_result(
        answer
            .pointer("/capabilities/positionEncoding")
            .and_then(Value::as_str),
    )?;
    // The server decides what one change notification carries. kvim sends the
    // complete text to a server that asks for a full synchronization, and one
    // range for each change to a server that asks for an incremental one.
    let synchronization = synchronization_mode(answer.pointer("/capabilities/textDocumentSync"));
    // A server that advertises a diagnostic provider answers the request of the
    // client instead of publishing a set on its own.
    let diagnostics = diagnostics_model(answer.pointer("/capabilities/diagnosticProvider"))?;
    writer.notify("initialized", json!({})).await?;
    if let Some(settings) = declaration.settings {
        writer
            .notify(
                "workspace/didChangeConfiguration",
                json!({ "settings": settings }),
            )
            .await?;
    }
    Ok(ServerCapabilities {
        encoding,
        synchronization,
        diagnostics,
    })
}

/// Sends `shutdown` and `exit` in the order that the protocol requires.
///
/// The call carries [`LSP_SHUTDOWN_DEADLINE`], so a server that never answers
/// cannot hold the caller. The caller ends the process after this sequence, so
/// a failure here still leaves no running child.
///
/// # Errors
///
/// Returns [`LspError::Timeout`] after the deadline,
/// [`LspError::MalformedResponse`] when the server answers another value than
/// the null that the protocol requires, and the transport failures of the
/// streams.
pub async fn shutdown(
    writer: &mut ProtocolWriter<ServerInput>,
    envelopes: &mut Envelopes,
) -> Result<(), LspError> {
    time::timeout(LSP_SHUTDOWN_DEADLINE, exit(writer, envelopes))
        .await
        .unwrap_or(Err(LspError::Timeout))
}

/// Runs the `shutdown` request and the `exit` notification.
async fn exit(
    writer: &mut ProtocolWriter<ServerInput>,
    envelopes: &mut Envelopes,
) -> Result<(), LspError> {
    let id = writer.request("shutdown", Value::Null).await?;
    let result = await_response(writer, envelopes, id).await?;
    // The protocol requires exactly null here. Another value means that the
    // server did not accept the shutdown.
    if result.get().trim() != "null" {
        return Err(LspError::MalformedResponse);
    }
    writer.notify("exit", Value::Null).await
}

/// Waits for one response and answers every server request meanwhile.
async fn await_response(
    writer: &mut ProtocolWriter<ServerInput>,
    envelopes: &mut Envelopes,
    expected: u64,
) -> Result<Box<RawValue>, LspError> {
    loop {
        let envelope = envelopes.recv().await.ok_or(LspError::Stopped)??;
        if envelope.method.is_some() {
            if let Some(id) = envelope.id {
                writer.reject_server_request(id).await?;
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

/// Drains the standard error of one server and records a bounded part of it.
///
/// The task drains the stream until the stream ends. A child that writes to a
/// pipe that nobody reads blocks when the pipe fills. Several servers write to
/// their standard error while they run correctly.
///
/// Draining and recording carry two different bounds. The task records at most
/// [`LSP_STDERR_BYTES_MAX`] bytes of one attempt, and it drains every further
/// byte without recording it. See `docs/language-services.md`.
async fn record_errors<R, F>(mut stream: R, report: F)
where
    R: AsyncRead + Unpin,
    F: Fn(ServerReport),
{
    let mut chunk = [0_u8; STDERR_CHUNK_BYTES];
    let mut recorder = ErrorRecorder::new(report);
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
struct ErrorRecorder<F> {
    /// The sink of every recorded fact, which never waits.
    report: F,
    /// The bytes of the line that no line break ended yet.
    line: Vec<u8>,
    /// The bytes that the recorder already recorded.
    recorded: usize,
    /// Whether the recorded bytes passed [`LSP_STDERR_BYTES_MAX`].
    stopped: bool,
}

impl<F> ErrorRecorder<F>
where
    F: Fn(ServerReport),
{
    /// Creates the recorder of one attempt.
    fn new(report: F) -> Self {
        Self {
            report,
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
            (self.report)(ServerReport::Output(text));
        }
        if self.recorded >= LSP_STDERR_BYTES_MAX {
            self.stopped = true;
            (self.report)(ServerReport::OutputBound);
        }
    }
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
