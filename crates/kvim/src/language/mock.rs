//! The deterministic mock server that every language test drives.
//!
//! The mock speaks the framing layer, so a test covers the real protocol path.
//! No test starts a language server of the host system.
//!
//! Both the protocol tests of this module and the editor wiring tests of `tui`
//! use this harness, so one mock server serves every layer.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream, duplex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::settings::IndentSettings;

use super::protocol::{LSP_OUTPUT_BYTES_MAX, WorkspaceRoot, read_frame};
use super::session::{
    LSP_EVENT_QUEUE_CAPACITY, LanguageEvent, LanguageOutcome, LanguageServerHandle, SessionConfig,
    Transport, TransportFactory, start,
};

/// The capacity of one test pipe, in bytes.
pub(crate) const PIPE_BYTES: usize = 1024 * 1024;

/// The guard that stops a broken test instead of hanging the suite.
pub(crate) const TEST_DEADLINE: Duration = Duration::from_secs(30);

/// The workspace root of every session test.
pub(crate) const ROOT: &str = "/workspace";

/// The document of every session test.
pub(crate) const DOCUMENT: &str = "/workspace/src/main.rs";

/// The `file` URI of that document.
pub(crate) const DOCUMENT_URI: &str = "file:///workspace/src/main.rs";

/// The mock server side of one session.
pub(crate) struct MockServer {
    input: DuplexStream,
    output: DuplexStream,
    read_bytes: usize,
}

impl MockServer {
    /// Reads the next message that the session sent.
    pub(crate) async fn read_message(&mut self) -> Value {
        let body = time::timeout(
            TEST_DEADLINE,
            read_frame(&mut self.output, &mut self.read_bytes, LSP_OUTPUT_BYTES_MAX),
        )
        .await
        .expect("the session sends a message before the test deadline")
        .expect("the session writes a valid frame");
        serde_json::from_slice(&body).expect("the session writes valid JSON")
    }

    /// Reads the next message and asserts its method.
    pub(crate) async fn expect(&mut self, method: &str) -> Value {
        let message = self.read_message().await;
        assert_eq!(message["method"], method, "unexpected message {message}");
        message
    }

    /// Writes one raw frame to the session.
    pub(crate) async fn send(&mut self, value: &Value) {
        let body = serde_json::to_vec(value).expect("the test value serializes");
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.input
            .write_all(header.as_bytes())
            .await
            .expect("the pipe accepts the header");
        self.input
            .write_all(&body)
            .await
            .expect("the pipe accepts the body");
        self.input.flush().await.expect("the pipe flushes");
    }

    /// Answers one request with a result.
    pub(crate) async fn respond(&mut self, id: &Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .await;
    }

    /// Runs the handshake that every session starts with.
    pub(crate) async fn handshake(&mut self) {
        let initialize = self.expect("initialize").await;
        assert_eq!(
            initialize["params"]["capabilities"]["general"]["positionEncodings"][0],
            "utf-8"
        );
        self.respond(
            &initialize["id"],
            json!({ "capabilities": { "positionEncoding": "utf-8" } }),
        )
        .await;
        self.expect("initialized").await;
    }
}

/// The editor side of one session under test.
pub(crate) struct Harness {
    handle: Option<LanguageServerHandle>,
    events: mpsc::Receiver<LanguageEvent>,
    /// The task that owns the mock server process.
    pub(crate) task: JoinHandle<()>,
}

impl Harness {
    /// Returns the handle of the running session.
    pub(crate) fn handle(&self) -> &LanguageServerHandle {
        self.handle
            .as_ref()
            .expect("the test keeps the handle until it drops it")
    }

    /// Waits for the next result.
    pub(crate) async fn next(&mut self) -> LanguageOutcome {
        self.next_event().await.outcome
    }

    /// Waits for the next result with the adapter that produced it.
    pub(crate) async fn next_event(&mut self) -> LanguageEvent {
        time::timeout(TEST_DEADLINE, self.events.recv())
            .await
            .expect("the session answers before the test deadline")
            .expect("the session queue stays open")
    }

    /// Drops the handle, so the session shuts the server down.
    pub(crate) fn stop(&mut self) {
        self.handle = None;
    }
}

/// Creates one connected stream pair.
pub(crate) fn pipe() -> (Transport, MockServer) {
    let (session_input, server_output) = duplex(PIPE_BYTES);
    let (server_input, session_output) = duplex(PIPE_BYTES);
    (
        Transport::prepared(session_input, session_output),
        MockServer {
            input: server_input,
            output: server_output,
            read_bytes: 0,
        },
    )
}

/// Creates the stable configuration of one session under test.
fn config(root: PathBuf, diagnostics_enabled: bool) -> SessionConfig {
    SessionConfig {
        adapter: "mock",
        language_id: "mock",
        root: WorkspaceRoot::new(root).expect("the root is absolute"),
        options: json!({}),
        indent: IndentSettings::default(),
        diagnostics_enabled,
    }
}

/// Starts one session over prepared transports.
pub(crate) fn session(transports: Vec<Transport>, diagnostics_enabled: bool) -> Harness {
    session_at(PathBuf::from(ROOT), transports, diagnostics_enabled)
}

/// Starts one session over prepared transports and one workspace root.
///
/// An editor test needs a root that holds real files, so it names its own
/// temporary directory instead of the fixed protocol root.
pub(crate) fn session_at(
    root: PathBuf,
    transports: Vec<Transport>,
    diagnostics_enabled: bool,
) -> Harness {
    let (events, receiver) = mpsc::channel(LSP_EVENT_QUEUE_CAPACITY);
    let (handle, task) = start(
        TransportFactory::Prepared(transports),
        config(root, diagnostics_enabled),
        events,
        CancellationToken::new(),
    );
    Harness {
        handle: Some(handle),
        events: receiver,
        task,
    }
}

/// Starts one session over one transport and returns its mock server.
pub(crate) fn connected() -> (Harness, MockServer) {
    let (transport, server) = pipe();
    (session(vec![transport], true), server)
}
