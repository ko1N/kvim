//! The deterministic mock server that every language test drives.
//!
//! The mock speaks the framing layer, so a test covers the real protocol path.
//! No test starts a language server of the host system.
//!
//! Both the protocol tests of this crate and the editor wiring tests of
//! `kvim-tui` use this harness, so one mock server serves every layer.
//!
//! The module is a test seam, never editor behavior. A test build of this
//! crate always holds it, and the `test-support` feature publishes it for
//! `kvim-tui`.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream, duplex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use kvim_settings::IndentSettings;

use super::protocol::{LSP_OUTPUT_BYTES_MAX, WorkspaceRoot, read_frame};
use super::server::{LanguageServerId, ServerFormatting};
use super::session::{
    LSP_EVENT_QUEUE_CAPACITY, LanguageEvent, LanguageOutcome, LanguageServerHandle, SessionConfig,
    TransportFactory, start,
};

/// The prepared byte streams that [`pipe`] hands to one session.
///
/// A caller never builds this value, and it names no protocol detail. It
/// exists in the signatures of this module, so it must be reachable.
pub use super::session::Transport;

/// The capacity of one test pipe, in bytes.
pub const PIPE_BYTES: usize = 1024 * 1024;

/// The guard that stops a broken test instead of hanging the suite.
pub const TEST_DEADLINE: Duration = Duration::from_secs(30);

/// The workspace root of every session test.
pub const ROOT: &str = "/workspace";

/// The document of every session test.
pub const DOCUMENT: &str = "/workspace/src/main.rs";

/// The `file` URI of that document.
pub const DOCUMENT_URI: &str = "file:///workspace/src/main.rs";

/// The mock server side of one session.
pub struct MockServer {
    input: DuplexStream,
    output: DuplexStream,
    read_bytes: usize,
}

impl MockServer {
    /// Reads the next message that the session sent.
    pub async fn read_message(&mut self) -> Value {
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
    pub async fn expect(&mut self, method: &str) -> Value {
        let message = self.read_message().await;
        assert_eq!(message["method"], method, "unexpected message {message}");
        message
    }

    /// Writes one raw frame to the session.
    pub async fn send(&mut self, value: &Value) {
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
    pub async fn respond(&mut self, id: &Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .await;
    }

    /// Runs the handshake that every session starts with.
    ///
    /// The mock confirms UTF-8, so its session converts no column.
    pub async fn handshake(&mut self) {
        self.handshake_with(Some("utf-8")).await;
    }

    /// Runs the handshake and confirms one position encoding.
    ///
    /// `None` sends a result that names no encoding, which the protocol defines
    /// as UTF-16. The session then converts every column, so a test that drives
    /// the conversion path names this shape. See `docs/language-services.md`.
    pub async fn handshake_with(&mut self, encoding: Option<&str>) {
        let initialize = self.expect("initialize").await;
        let offered = &initialize["params"]["capabilities"]["general"]["positionEncodings"];
        assert_eq!(offered[0], "utf-8", "the client prefers UTF-8");
        assert_eq!(offered[1], "utf-16", "the client also offers UTF-16");
        let capabilities = match encoding {
            Some(encoding) => json!({ "positionEncoding": encoding }),
            None => json!({}),
        };
        self.respond(&initialize["id"], json!({ "capabilities": capabilities }))
            .await;
        self.expect("initialized").await;
    }

    /// Runs the handshake and advertises one diagnostic provider.
    ///
    /// The session then asks for the diagnostics of each document instead of
    /// waiting for a notification, and every request repeats `identifier`. See
    /// `docs/language-services.md`.
    pub async fn handshake_pulling(&mut self, identifier: &str) {
        let initialize = self.expect("initialize").await;
        self.respond(
            &initialize["id"],
            json!({
                "capabilities": {
                    "positionEncoding": "utf-8",
                    "diagnosticProvider": {
                        "identifier": identifier,
                        "interFileDependencies": false,
                        "workspaceDiagnostics": false,
                    },
                }
            }),
        )
        .await;
        self.expect("initialized").await;
    }
}

/// The editor side of one session under test.
pub struct Harness {
    handle: Option<LanguageServerHandle>,
    events: mpsc::Receiver<LanguageEvent>,
    /// The task that owns the mock server process.
    pub task: JoinHandle<()>,
}

impl Harness {
    /// Returns the handle of the running session.
    pub fn handle(&self) -> &LanguageServerHandle {
        self.handle
            .as_ref()
            .expect("the test keeps the handle until it drops it")
    }

    /// Waits for the next result.
    pub async fn next(&mut self) -> LanguageOutcome {
        self.next_event().await.outcome
    }

    /// Waits for the next result with the server that produced it.
    pub async fn next_event(&mut self) -> LanguageEvent {
        time::timeout(TEST_DEADLINE, self.events.recv())
            .await
            .expect("the session answers before the test deadline")
            .expect("the session queue stays open")
    }

    /// Drops the handle, so the session shuts the server down.
    pub fn stop(&mut self) {
        self.handle = None;
    }
}

/// Creates one connected stream pair.
pub fn pipe() -> (Transport, MockServer) {
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

/// The identity of the first mock server of the one mock adapter.
pub const SERVER: LanguageServerId = LanguageServerId::new("mock", 0, "mock");

/// The identity of the second mock server of the same mock adapter.
///
/// A test that drives two servers on one buffer names this identity for the
/// second session. The two servers share one adapter, so the merge rules of
/// `docs/language-services.md` apply to their answers, and this later
/// declaration loses every duplicate.
pub const OTHER_SERVER: LanguageServerId = LanguageServerId::new("mock", 1, "other");

/// Starts one session whose declaration names workspace settings.
///
/// A server that reads its behavior from the workspace configuration needs that
/// channel, so a test that drives it names this constructor. See
/// `docs/language-services.md`.
pub fn connected_with_settings(settings: Value) -> (Harness, MockServer) {
    let (transport, server) = pipe();
    let mut config = config(SERVER, PathBuf::from(ROOT), true);
    config.workspace_settings = Some(settings);
    let (events, receiver) = mpsc::channel(LSP_EVENT_QUEUE_CAPACITY);
    let (handle, task) = start(
        TransportFactory::Prepared(vec![transport]),
        config,
        events,
        CancellationToken::new(),
    );
    let harness = Harness {
        handle: Some(handle),
        events: receiver,
        task,
    };
    (harness, server)
}

/// Creates the stable configuration of one session under test.
fn config(id: LanguageServerId, root: PathBuf, diagnostics_enabled: bool) -> SessionConfig {
    SessionConfig {
        id,
        language_id: "mock",
        server: "mock-server",
        formatting: ServerFormatting::Enabled,
        root: WorkspaceRoot::new(root).expect("the root is absolute"),
        options: json!({}),
        workspace_settings: None,
        indent: IndentSettings::default(),
        diagnostics_enabled,
    }
}

/// Starts one session over prepared transports.
pub fn session(transports: Vec<Transport>, diagnostics_enabled: bool) -> Harness {
    session_at(PathBuf::from(ROOT), transports, diagnostics_enabled)
}

/// Starts one session over prepared transports and one workspace root.
///
/// An editor test needs a root that holds real files, so it names its own
/// temporary directory instead of the fixed protocol root.
pub fn session_at(root: PathBuf, transports: Vec<Transport>, diagnostics_enabled: bool) -> Harness {
    named_session_at(SERVER, root, transports, diagnostics_enabled)
}

/// Starts one named session over prepared transports and one workspace root.
///
/// The name is the declaration identifier of the server, so a test that drives
/// two servers of one language gives each session its own identity.
pub fn named_session_at(
    server: LanguageServerId,
    root: PathBuf,
    transports: Vec<Transport>,
    diagnostics_enabled: bool,
) -> Harness {
    let (events, receiver) = mpsc::channel(LSP_EVENT_QUEUE_CAPACITY);
    let (handle, task) = start(
        TransportFactory::Prepared(transports),
        config(server, root, diagnostics_enabled),
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
pub fn connected() -> (Harness, MockServer) {
    let (transport, server) = pipe();
    (session(vec![transport], true), server)
}
