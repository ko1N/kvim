use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream, duplex};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use kvim_path::WorktreeRelativePath;

use crate::process::{ServerLaunchRequest, Transport, TransportFactory};
use crate::project::{
    ManagerLimits, ProjectDeclaration, ProjectHandle, ProjectId, ProjectManager, ServerDeclaration,
    ServerId,
};
use crate::protocol::{LSP_OUTPUT_BYTES_MAX, LspBound, LspError, WorkspaceRoot, read_frame};

use super::{
    ChangedFile, CompletionPolicy, DiagnosticsHub, DiagnosticsLimits, DiagnosticsOutcome,
    DiagnosticsServer, DocumentRevision, LSP_DIAGNOSTICS_MAX, LSP_SERVER_LANGUAGES_MAX, LanguageId,
    RevisionPolicy, ServerOutcome, Truncation, WaitPolicy,
};

/// The capacity of one test pipe, in bytes.
const PIPE_BYTES: usize = 256 * 1024;

/// The guard that stops a broken test instead of hanging the suite.
const TEST_DEADLINE: Duration = Duration::from_secs(30);

/// The deadline of a request that must reach its own end.
const SHORT_DEADLINE: Duration = Duration::from_millis(300);

/// The deadline of a request that must not end before its servers answer.
const LONG_DEADLINE: Duration = Duration::from_secs(20);

/// The root of every test project.
const ROOT: &str = "/workspace";

/// The changed document of every test request.
const DOCUMENT: &str = "src/main.rs";

/// The `file` URI of [`DOCUMENT`] below [`ROOT`].
const DOCUMENT_URI: &str = "file:///workspace/src/main.rs";

/// The exact text of the changed revision.
const TEXT: &str = "fn main() {}\n";

/// The shell that runs the child of the process test.
const SHELL: &str = "/bin/sh";

/// The mock server side of one prepared session.
struct MockServer {
    input: DuplexStream,
    output: DuplexStream,
    read_bytes: usize,
}

impl MockServer {
    /// Reads the next message, or `None` after the session ended.
    async fn read_message(&mut self) -> Option<Value> {
        let body = time::timeout(
            TEST_DEADLINE,
            read_frame(&mut self.output, &mut self.read_bytes, LSP_OUTPUT_BYTES_MAX),
        )
        .await
        .expect("the session writes before the test deadline")
        .ok()?;
        serde_json::from_slice(&body).ok()
    }

    /// Writes one raw frame to the session.
    async fn send(&mut self, value: &Value) {
        let body = serde_json::to_vec(value).expect("the test value serializes");
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let _ = self.input.write_all(header.as_bytes()).await;
        let _ = self.input.write_all(&body).await;
        let _ = self.input.flush().await;
    }

    /// Answers `initialize` with the supplied capabilities.
    async fn handshake(&mut self, capabilities: Value) {
        let initialize = self
            .read_message()
            .await
            .expect("the session sends initialize");
        assert_eq!(initialize["method"], "initialize");
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": initialize["id"],
            "result": { "capabilities": capabilities },
        }))
        .await;
        let initialized = self
            .read_message()
            .await
            .expect("the session sends initialized");
        assert_eq!(initialized["method"], "initialized");
    }

    /// Reads messages until the named method arrives.
    async fn expect(&mut self, method: &str) -> Value {
        loop {
            let message = self
                .read_message()
                .await
                .expect("the session sends the expected method");
            if message["method"] == method {
                return message;
            }
        }
    }
}

/// Creates one connected stream pair.
fn pipe() -> (TransportFactory, MockServer) {
    let (session_input, server_output) = duplex(PIPE_BYTES);
    let (server_input, session_output) = duplex(PIPE_BYTES);
    (
        TransportFactory::Prepared(vec![Transport::prepared(session_input, session_output)]),
        MockServer {
            input: server_input,
            output: server_output,
            read_bytes: 0,
        },
    )
}

/// The capabilities of one server that answers a pull.
fn pull_capabilities() -> Value {
    json!({
        "positionEncoding": "utf-8",
        "textDocumentSync": 1,
        "diagnosticProvider": { "identifier": "test" },
    })
}

/// The capabilities of one server that publishes its sets.
fn push_capabilities() -> Value {
    json!({ "positionEncoding": "utf-8", "textDocumentSync": 1 })
}

/// Answers every pull of one server with the same items.
fn pull_server(mock: MockServer, items: Value) -> JoinHandle<()> {
    pull_server_after(mock, Duration::ZERO, items)
}

/// Answers every pull after one startup delay.
fn pull_server_after(mut mock: MockServer, delay: Duration, items: Value) -> JoinHandle<()> {
    tokio::spawn(async move {
        if !delay.is_zero() {
            time::sleep(delay).await;
        }
        mock.handshake(pull_capabilities()).await;
        while let Some(message) = mock.read_message().await {
            if message["method"] == "textDocument/diagnostic" {
                mock.send(&json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": { "kind": "full", "items": items },
                }))
                .await;
            }
        }
    })
}

/// Answers an initial empty pull, requests a refresh, and answers the repeated pull.
fn refreshing_pull_server(mut mock: MockServer, refreshed_items: Value) -> JoinHandle<()> {
    tokio::spawn(async move {
        mock.handshake(pull_capabilities()).await;
        let first = mock.expect("textDocument/diagnostic").await;
        mock.send(&json!({
            "jsonrpc": "2.0",
            "id": first["id"],
            "result": { "kind": "full", "items": [] },
        }))
        .await;
        mock.send(&json!({
            "jsonrpc": "2.0",
            "id": 900,
            "method": "workspace/diagnostic/refresh",
        }))
        .await;
        let accepted = mock.read_message().await.expect("the refresh is answered");
        assert_eq!(accepted["id"], 900);
        assert_eq!(accepted["result"], Value::Null);
        let repeated = mock.expect("textDocument/diagnostic").await;
        mock.send(&json!({
            "jsonrpc": "2.0",
            "id": repeated["id"],
            "result": { "kind": "full", "items": refreshed_items },
        }))
        .await;
    })
}

/// Answers every pull while using the protocol-default UTF-16 encoding.
fn utf16_pull_server(mut mock: MockServer, items: Value) -> JoinHandle<()> {
    tokio::spawn(async move {
        mock.handshake(json!({
            "textDocumentSync": 1,
            "diagnosticProvider": { "identifier": "test" },
        }))
        .await;
        while let Some(message) = mock.read_message().await {
            if message["method"] == "textDocument/diagnostic" {
                mock.send(&json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": { "kind": "full", "items": items },
                }))
                .await;
            }
        }
    })
}

/// Publishes one set for the revision of every opened document.
fn push_server(mock: MockServer, items: Value) -> JoinHandle<()> {
    publishing_server(mock, Vec::new(), items)
}

/// Publishes the obsolete sets first and then the set of the revision.
///
/// Each obsolete entry names its own version, or JSON `null` for a set that
/// names no revision at all.
fn publishing_server(
    mut mock: MockServer,
    obsolete: Vec<(Value, Value)>,
    items: Value,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        mock.handshake(push_capabilities()).await;
        loop {
            let opened = mock.expect("textDocument/didOpen").await;
            let uri = opened["params"]["textDocument"]["uri"].clone();
            let version = opened["params"]["textDocument"]["version"].clone();
            for (stale_version, stale_items) in &obsolete {
                mock.send(&publication(&uri, stale_version, stale_items))
                    .await;
            }
            mock.send(&publication(&uri, &version, &items)).await;
        }
    })
}

/// Builds one `textDocument/publishDiagnostics` notification.
fn publication(uri: &Value, version: &Value, items: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": { "uri": uri, "version": version, "diagnostics": items },
    })
}

/// Answers the handshake and then answers nothing.
fn silent_server(mut mock: MockServer, capabilities: Value) -> JoinHandle<()> {
    tokio::spawn(async move {
        mock.handshake(capabilities).await;
        while mock.read_message().await.is_some() {}
    })
}

/// Builds one received diagnostic of the changed document.
fn item(line: u32, start: u32, end: u32, severity: u8, message: &str) -> Value {
    json!({
        "range": {
            "start": { "line": line, "character": start },
            "end": { "line": line, "character": end },
        },
        "severity": severity,
        "source": "test",
        "message": message,
    })
}

/// Returns the language that every test request names.
fn language() -> LanguageId {
    LanguageId::new("rust").expect("the identifier is short")
}

/// Declares one server of the hub.
fn declared(id: u64, completion: CompletionPolicy) -> DiagnosticsServer {
    DiagnosticsServer {
        id: ServerId::new(id),
        source: "test".to_owned(),
        languages: vec![language()],
        completion,
    }
}

/// Builds one request over the exact text of one revision.
fn request(revision: i32) -> ChangedFile {
    ChangedFile::new(
        WorktreeRelativePath::new(DOCUMENT).expect("the path is relative"),
        TEXT.to_owned(),
        DocumentRevision::new(revision),
        language(),
    )
    .wait(WaitPolicy::Until(LONG_DEADLINE))
}

/// One open project, its hub, and its running driver.
struct Session {
    hub: DiagnosticsHub,
    handle: ProjectHandle,
    driver: JoinHandle<()>,
}

impl Session {
    /// Opens one project over the declared servers and their transports.
    fn open(servers: Vec<DiagnosticsServer>, transports: Vec<TransportFactory>) -> Self {
        Self::open_at(PathBuf::from(ROOT), servers, transports)
    }

    /// Opens one project at one test root.
    fn open_at(
        root: PathBuf,
        servers: Vec<DiagnosticsServer>,
        transports: Vec<TransportFactory>,
    ) -> Self {
        assert_eq!(servers.len(), transports.len(), "one transport per server");
        let hub = DiagnosticsHub::new();
        let manager = ProjectManager::new(ManagerLimits::default());
        let root = WorkspaceRoot::new(root).expect("the root is absolute");
        let mut declaration = ProjectDeclaration::new(ProjectId::FIRST, root);
        for (server, transport) in servers.into_iter().zip(transports) {
            let id = server.id;
            let conversation = hub.server(server).expect("the hub accepts the declaration");
            declaration = declaration.server(
                ServerDeclaration {
                    id,
                    transport,
                    options: json!({}),
                    workspace_settings: None,
                },
                conversation,
            );
        }
        let (handle, driver) = manager.open(declaration).expect("the budget holds it");
        Self {
            hub,
            handle,
            driver: tokio::spawn(driver.run()),
        }
    }

    /// Asks for the diagnostics of one revision.
    async fn ask(&self, request: ChangedFile) -> DiagnosticsOutcome {
        time::timeout(TEST_DEADLINE, self.hub.changed_file(request))
            .await
            .expect("the request ends before the test deadline")
            .expect("the request holds every bound")
    }

    /// Ends the project and waits for its driver.
    async fn close(self) {
        self.handle.close().await;
        let _ = time::timeout(TEST_DEADLINE, self.driver).await;
    }
}

/// Returns the report of one ready outcome.
fn ready(outcome: DiagnosticsOutcome) -> std::sync::Arc<super::ChangedFileReport> {
    match outcome {
        DiagnosticsOutcome::Ready(report) => report,
        other => panic!("the request returned {other:?} instead of one report"),
    }
}

/// Returns the failure of the first server of one report.
fn failure(report: &super::ChangedFileReport) -> &LspError {
    match &report.servers()[0].outcome {
        ServerOutcome::Failed(error) => error,
        other => panic!("the server returned {other:?} instead of one failure"),
    }
}

#[tokio::test]
async fn a_refresh_aware_pull_repeats_an_initial_empty_report() {
    let (transport, mock) = pipe();
    let session = Session::open(
        vec![declared(0, CompletionPolicy::PullAfterRefresh)],
        vec![transport],
    );
    let server = refreshing_pull_server(mock, json!([item(0, 3, 7, 1, "ready error")]));

    let report = ready(session.ask(request(7)).await);

    assert_eq!(report.revision(), DocumentRevision::new(7));
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].diagnostic.message, "ready error");
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_refresh_driven_empty_report_is_clean() {
    let (transport, mock) = pipe();
    let session = Session::open(
        vec![declared(0, CompletionPolicy::PullAfterRefresh)],
        vec![transport],
    );
    let server = refreshing_pull_server(mock, json!([]));

    let report = ready(session.ask(request(8)).await);

    assert!(report.diagnostics().is_empty());
    assert!(matches!(
        report.servers()[0].outcome,
        ServerOutcome::Ready {
            diagnostics: 0,
            truncation: Truncation::Complete,
        }
    ));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_refresh_before_the_initial_empty_report_is_remembered() {
    let (transport, mut mock) = pipe();
    let session = Session::open(
        vec![declared(0, CompletionPolicy::PullAfterRefresh)],
        vec![transport],
    );
    let server = tokio::spawn(async move {
        mock.handshake(pull_capabilities()).await;
        let initial = mock.expect("textDocument/diagnostic").await;
        mock.send(&json!({
            "jsonrpc": "2.0",
            "id": 901,
            "method": "workspace/diagnostic/refresh",
        }))
        .await;
        let accepted = mock.read_message().await.expect("the refresh is answered");
        assert_eq!(accepted["id"], 901);
        assert_eq!(accepted["result"], Value::Null);
        mock.send(&json!({
            "jsonrpc": "2.0",
            "id": initial["id"],
            "result": { "kind": "full", "items": [] },
        }))
        .await;
        let repeated = mock.expect("textDocument/diagnostic").await;
        mock.send(&json!({
            "jsonrpc": "2.0",
            "id": repeated["id"],
            "result": { "kind": "full", "items": [] },
        }))
        .await;
    });

    let report = ready(session.ask(request(9)).await);

    assert!(report.diagnostics().is_empty());
    session.close().await;
    server.await.expect("the mock server completes");
}

#[tokio::test]
async fn a_ready_refresh_aware_server_accepts_a_warm_empty_report() {
    let (transport, mut mock) = pipe();
    let session = Session::open(
        vec![declared(0, CompletionPolicy::PullAfterRefresh)],
        vec![transport],
    );
    let server = tokio::spawn(async move {
        mock.handshake(pull_capabilities()).await;
        let first = mock.expect("textDocument/diagnostic").await;
        mock.send(&json!({
            "jsonrpc": "2.0",
            "id": first["id"],
            "result": {
                "kind": "full",
                "items": [item(0, 3, 7, 1, "initial error")],
            },
        }))
        .await;
        let warm = mock.expect("textDocument/diagnostic").await;
        mock.send(&json!({
            "jsonrpc": "2.0",
            "id": warm["id"],
            "result": { "kind": "full", "items": [] },
        }))
        .await;
    });

    let initial = ready(session.ask(request(11)).await);
    let warm = ready(session.ask(request(12)).await);

    assert_eq!(initial.diagnostics().len(), 1);
    assert!(warm.diagnostics().is_empty());
    session.close().await;
    server.await.expect("the mock server completes");
}

#[tokio::test]
async fn a_refresh_aware_pull_bounds_refresh_cancellation_turnover() {
    let (transport, mut mock) = pipe();
    let session = Session::open(
        vec![declared(0, CompletionPolicy::PullAfterRefresh)],
        vec![transport],
    );
    let server = tokio::spawn(async move {
        mock.handshake(pull_capabilities()).await;
        let mut pull = mock.expect("textDocument/diagnostic").await;
        for pull_index in 0..=super::LSP_DIAGNOSTIC_REFRESH_PULLS_MAX {
            mock.send(&json!({
                "jsonrpc": "2.0",
                "id": 902 + pull_index,
                "method": "workspace/diagnostic/refresh",
            }))
            .await;
            let accepted = mock.read_message().await.expect("the refresh is answered");
            assert_eq!(accepted["id"], 902 + pull_index);
            assert_eq!(accepted["result"], Value::Null);
            mock.send(&json!({
                "jsonrpc": "2.0",
                "id": pull["id"],
                "error": { "code": -32802, "message": "cancelled" },
            }))
            .await;
            if pull_index < super::LSP_DIAGNOSTIC_REFRESH_PULLS_MAX {
                pull = mock.expect("textDocument/diagnostic").await;
            }
        }
    });

    let report = ready(session.ask(request(10)).await);

    assert!(matches!(
        failure(&report),
        LspError::Response { code: -32802 }
    ));
    assert!(report.diagnostics().is_empty());
    session.close().await;
    server.await.expect("the mock server completes");
}

#[tokio::test]
async fn one_until_request_survives_the_startup_of_its_server() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    // The server answers its handshake long after the request starts, and
    // the request still returns the diagnostics of its own revision.
    let server = pull_server_after(
        mock,
        Duration::from_millis(400),
        json!([item(0, 3, 7, 2, "one marker")]),
    );

    let report = ready(session.ask(request(7)).await);

    assert_eq!(report.revision(), DocumentRevision::new(7));
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].diagnostic.message, "one marker");
    assert!(matches!(
        report.servers()[0].outcome,
        ServerOutcome::Ready {
            diagnostics: 1,
            truncation: Truncation::Complete,
        }
    ));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn an_immediate_request_reports_a_server_that_did_not_answer() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let server = silent_server(mock, pull_capabilities());

    let outcome = session.ask(request(1).wait(WaitPolicy::Immediate)).await;

    assert!(matches!(outcome, DiagnosticsOutcome::Starting));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn an_immediate_request_reads_the_report_of_the_finished_revision() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let server = pull_server(mock, json!([item(0, 0, 2, 1, "one error")]));

    let first = ready(session.ask(request(3)).await);
    // The finished revision keeps its report, so the second request reads
    // it without a second dispatch.
    let second = ready(session.ask(request(3).wait(WaitPolicy::Immediate)).await);

    assert_eq!(first.revision(), second.revision());
    assert_eq!(second.diagnostics().len(), 1);
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_versioned_push_server_completes_the_exact_revision() {
    let (transport, mock) = pipe();
    let session = Session::open(
        vec![declared(0, CompletionPolicy::VersionedPush)],
        vec![transport],
    );
    let server = push_server(mock, json!([item(0, 0, 2, 1, "one error")]));

    let report = ready(session.ask(request(5)).await);

    assert_eq!(report.revision(), DocumentRevision::new(5));
    assert_eq!(report.diagnostics().len(), 1);
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_published_set_of_another_revision_never_publishes() {
    let (transport, mock) = pipe();
    let session = Session::open(
        vec![declared(0, CompletionPolicy::VersionedPush)],
        vec![transport],
    );
    // The server publishes the set of an earlier revision, and then one set
    // that names no revision at all. Neither completes the request.
    let server = publishing_server(
        mock,
        vec![
            (json!(4), json!([item(0, 0, 2, 1, "stale error")])),
            (Value::Null, json!([item(0, 0, 2, 1, "versionless error")])),
        ],
        json!([item(0, 3, 7, 2, "current marker")]),
    );

    let report = ready(session.ask(request(5)).await);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].diagnostic.message,
        "current marker",
        "an obsolete set reached the report"
    );
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn utf16_diagnostics_use_byte_columns_of_the_exact_requested_text() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let server = utf16_pull_server(mock, json!([item(0, 2, 3, 1, "after emoji")]));
    let request = ChangedFile::new(
        WorktreeRelativePath::new(DOCUMENT).expect("the path is relative"),
        "😀ab\n".to_owned(),
        DocumentRevision::new(9),
        language(),
    )
    .wait(WaitPolicy::Until(LONG_DEADLINE));

    let report = ready(session.ask(request).await);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].diagnostic.span.start,
        crate::DocumentPosition::new(0, 4),
        "the default UTF-16 column after the emoji becomes its UTF-8 byte column"
    );
    assert_eq!(
        report.diagnostics()[0].diagnostic.span.end,
        crate::DocumentPosition::new(0, 5)
    );
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_range_outside_the_exact_text_refuses_the_answer() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    // The document holds two lines, so line nine addresses no text of the
    // revision that the caller supplied.
    let server = pull_server(mock, json!([item(9, 0, 4, 1, "outside")]));

    let report = ready(session.ask(request(1)).await);

    assert!(matches!(failure(&report), LspError::MalformedResponse));
    assert!(report.diagnostics().is_empty());
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn the_per_server_bound_keeps_the_errors_of_that_server() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let server = pull_server(
        mock,
        json!([
            item(0, 3, 7, 2, "one warning"),
            item(0, 0, 2, 1, "one error")
        ]),
    );

    let report = ready(
        session
            .ask(request(1).limits(DiagnosticsLimits::default().per_server(1)))
            .await,
    );

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].diagnostic.message, "one error");
    assert!(matches!(
        report.servers()[0].outcome,
        ServerOutcome::Ready {
            diagnostics: 1,
            truncation: Truncation::Truncated { dropped: 1 },
        }
    ));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn the_aggregate_bound_keeps_the_errors_of_every_server() {
    let (first_transport, first_mock) = pipe();
    let (second_transport, second_mock) = pipe();
    let session = Session::open(
        vec![
            declared(0, CompletionPolicy::Pull),
            declared(1, CompletionPolicy::VersionedPush),
        ],
        vec![first_transport, second_transport],
    );
    let first = pull_server(first_mock, json!([item(0, 3, 7, 2, "one warning")]));
    let second = push_server(second_mock, json!([item(0, 0, 2, 1, "one error")]));

    let report = ready(
        session
            .ask(request(1).limits(DiagnosticsLimits::default().merged(1)))
            .await,
    );

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].diagnostic.message, "one error");
    assert_eq!(report.truncation(), Truncation::Truncated { dropped: 1 });
    session.close().await;
    first.abort();
    second.abort();
}

#[tokio::test]
async fn mixed_servers_merge_in_declaration_order_and_drop_one_duplicate() {
    let (first_transport, first_mock) = pipe();
    let (second_transport, second_mock) = pipe();
    let session = Session::open(
        vec![
            declared(0, CompletionPolicy::Pull),
            declared(1, CompletionPolicy::VersionedPush),
        ],
        vec![first_transport, second_transport],
    );
    let shared = item(0, 0, 2, 1, "both servers report this");
    let first = pull_server(
        first_mock,
        json!([shared, item(0, 3, 7, 2, "only the pull server")]),
    );
    let second = push_server(
        second_mock,
        json!([shared, item(1, 0, 0, 3, "only the push server")]),
    );

    let report = ready(session.ask(request(1)).await);

    assert_eq!(report.servers().len(), 2);
    assert_eq!(report.servers()[0].server, ServerId::new(0));
    assert_eq!(report.servers()[1].server, ServerId::new(1));
    let messages: Vec<&str> = report
        .diagnostics()
        .iter()
        .map(|reported| reported.diagnostic.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "both servers report this",
            "only the pull server",
            "only the push server",
        ],
        "the merge sorts by severity and removes the duplicate once"
    );
    session.close().await;
    first.abort();
    second.abort();
}

#[tokio::test]
async fn the_related_information_bound_refuses_a_longer_list() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let related = json!({
        "location": {
            "uri": DOCUMENT_URI,
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 2 },
            },
        },
        "message": "also here",
    });
    let mut diagnostic = item(0, 0, 2, 1, "one error");
    diagnostic["relatedInformation"] = json!([related, related]);
    let server = pull_server(mock, json!([diagnostic]));

    let report = ready(
        session
            .ask(request(1).limits(DiagnosticsLimits::default().related_information(1)))
            .await,
    );

    assert!(matches!(
        failure(&report),
        LspError::Bounds {
            measure: LspBound::RelatedInformation,
            ..
        }
    ));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn the_message_bound_refuses_a_longer_message() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let server = pull_server(mock, json!([item(0, 0, 2, 1, "a message above the bound")]));

    let report = ready(
        session
            .ask(request(1).limits(DiagnosticsLimits::default().message_bytes(4)))
            .await,
    );

    assert!(matches!(
        failure(&report),
        LspError::Bounds {
            measure: LspBound::DiagnosticMessageBytes,
            limit: 4,
            ..
        }
    ));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn the_traffic_bound_refuses_a_server_that_answers_too_much() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let items: Vec<Value> = (0..64)
        .map(|index| item(0, 0, 2, 1, &format!("diagnostic number {index}")))
        .collect();
    let server = pull_server(mock, json!(items));

    let report = ready(
        session
            .ask(request(1).limits(DiagnosticsLimits::default().protocol_bytes(128)))
            .await,
    );

    assert!(matches!(
        failure(&report),
        LspError::Bounds {
            measure: LspBound::RequestBytes,
            limit: 128,
            ..
        }
    ));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn the_text_bound_refuses_a_longer_document() {
    let hub = DiagnosticsHub::new();
    let long = "x".repeat(64);
    let request = ChangedFile::new(
        WorktreeRelativePath::new(DOCUMENT).expect("the path is relative"),
        long,
        DocumentRevision::FIRST,
        language(),
    )
    .limits(DiagnosticsLimits::default().text_bytes(16));

    let error = hub
        .changed_file(request)
        .await
        .expect_err("the text passes its bound");

    assert!(matches!(
        error,
        LspError::Bounds {
            measure: LspBound::DocumentBytes,
            limit: 16,
            actual: 64,
        }
    ));
}

#[tokio::test]
async fn a_limit_above_the_bound_of_this_crate_refuses_the_request() {
    let hub = DiagnosticsHub::new();
    let request =
        request(1).limits(DiagnosticsLimits::default().per_server(LSP_DIAGNOSTICS_MAX + 1));

    let error = hub
        .changed_file(request)
        .await
        .expect_err("the limit passes the bound of this crate");

    assert!(matches!(
        error,
        LspError::Bounds {
            measure: LspBound::Diagnostics,
            limit: LSP_DIAGNOSTICS_MAX,
            ..
        }
    ));
}

#[tokio::test]
async fn a_language_that_no_server_declares_is_unsupported() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let server = pull_server(mock, json!([]));
    let other = LanguageId::new("zig").expect("the identifier is short");

    let outcome = session
        .ask(
            ChangedFile::new(
                WorktreeRelativePath::new(DOCUMENT).expect("the path is relative"),
                TEXT.to_owned(),
                DocumentRevision::FIRST,
                other,
            )
            .wait(WaitPolicy::Until(SHORT_DEADLINE)),
        )
        .await;

    assert!(matches!(outcome, DiagnosticsOutcome::Unsupported));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_server_without_a_safe_completion_reports_one_unsupported_outcome() {
    let (first_transport, first_mock) = pipe();
    let (second_transport, second_mock) = pipe();
    let session = Session::open(
        vec![
            declared(0, CompletionPolicy::Unsupported),
            // The declaration asks for a pull, and the server advertises no
            // diagnostic provider, so it answers no pull.
            declared(1, CompletionPolicy::Pull),
        ],
        vec![first_transport, second_transport],
    );
    let first = silent_server(first_mock, pull_capabilities());
    let second = silent_server(second_mock, push_capabilities());

    let report = ready(session.ask(request(1)).await);

    assert!(matches!(
        report.servers()[0].outcome,
        ServerOutcome::Unsupported
    ));
    assert!(matches!(
        report.servers()[1].outcome,
        ServerOutcome::Unsupported
    ));
    session.close().await;
    first.abort();
    second.abort();
}

#[tokio::test]
async fn a_missing_program_reports_one_unavailable_server() {
    let (transport, mock) = pipe();
    let session = Session::open(
        vec![
            declared(0, CompletionPolicy::Pull),
            declared(1, CompletionPolicy::Pull),
        ],
        // An empty list of prepared transports reports the state of a
        // program that the system does not hold.
        vec![transport, TransportFactory::Prepared(Vec::new())],
    );
    let server = pull_server(mock, json!([item(0, 0, 2, 1, "one error")]));

    let report = ready(session.ask(request(1)).await);

    assert!(matches!(
        report.servers()[0].outcome,
        ServerOutcome::Ready { .. }
    ));
    assert!(matches!(
        report.servers()[1].outcome,
        ServerOutcome::Unavailable
    ));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn the_deadline_ends_a_request_that_no_server_answers() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let server = silent_server(mock, pull_capabilities());

    let outcome = session
        .ask(request(1).wait(WaitPolicy::Until(SHORT_DEADLINE)))
        .await;

    assert!(matches!(outcome, DiagnosticsOutcome::TimedOut));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_cancelled_request_returns_the_cancelled_outcome() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let server = silent_server(mock, pull_capabilities());
    let cancellation = CancellationToken::new();
    let owner = cancellation.clone();
    tokio::spawn(async move {
        time::sleep(Duration::from_millis(50)).await;
        owner.cancel();
    });

    let outcome = session.ask(request(1).cancellation(cancellation)).await;

    assert!(matches!(outcome, DiagnosticsOutcome::Cancelled));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_newer_revision_supersedes_the_running_one() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let server = silent_server(mock, pull_capabilities());
    let hub = &session.hub;

    let (older, newer) = tokio::join!(hub.changed_file(request(1)), async {
        time::sleep(Duration::from_millis(100)).await;
        hub.changed_file(
            request(2)
                .revisions(RevisionPolicy::Supersede)
                .wait(WaitPolicy::Until(SHORT_DEADLINE)),
        )
        .await
    });

    assert!(matches!(
        older.expect("the request holds every bound"),
        DiagnosticsOutcome::Superseded
    ));
    assert!(matches!(
        newer.expect("the request holds every bound"),
        DiagnosticsOutcome::TimedOut
    ));
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_queued_revision_waits_behind_the_running_one() {
    let (transport, mock) = pipe();
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
    let server = silent_server(mock, pull_capabilities());
    let hub = &session.hub;

    let (older, newer) = tokio::join!(
        hub.changed_file(request(1).wait(WaitPolicy::Until(LONG_DEADLINE))),
        async {
            time::sleep(Duration::from_millis(50)).await;
            // The newer revision keeps the running one, so it never
            // installs and it ends on its own deadline instead.
            hub.changed_file(request(2).wait(WaitPolicy::Until(SHORT_DEADLINE)))
                .await
        }
    );

    assert!(matches!(
        newer.expect("the request holds every bound"),
        DiagnosticsOutcome::TimedOut
    ));
    // The running revision never received the result of the newer one.
    assert!(matches!(
        older.expect("the request holds every bound"),
        DiagnosticsOutcome::TimedOut
    ));
    session.close().await;
    server.abort();
}

#[test]
fn one_hub_refuses_a_server_declaration_that_passes_its_bounds() {
    let hub = DiagnosticsHub::new();
    assert!(hub.server(declared(0, CompletionPolicy::Pull)).is_ok());
    assert!(matches!(
        hub.server(declared(0, CompletionPolicy::Pull)),
        Err(LspError::DuplicateServer)
    ));

    let mut without = declared(1, CompletionPolicy::Pull);
    without.languages = Vec::new();
    assert!(matches!(
        hub.server(without),
        Err(LspError::Bounds {
            measure: LspBound::Languages,
            actual: 0,
            ..
        })
    ));

    let mut many = declared(2, CompletionPolicy::Pull);
    many.languages = (0..=LSP_SERVER_LANGUAGES_MAX)
        .map(|index| LanguageId::new(&format!("language{index}")).expect("it is short"))
        .collect();
    assert!(matches!(
        hub.server(many),
        Err(LspError::Bounds {
            measure: LspBound::Languages,
            ..
        })
    ));

    let mut named = declared(3, CompletionPolicy::Pull);
    named.source = "x".repeat(super::LSP_SERVER_SOURCE_BYTES_MAX + 1);
    assert!(matches!(
        hub.server(named),
        Err(LspError::Bounds {
            measure: LspBound::SourceBytes,
            ..
        })
    ));
}

#[tokio::test]
async fn real_rust_analyzer_reports_a_finding_and_then_clean() {
    let root = std::env::temp_dir().join(format!("kvim-lsp-rust-analyzer-{}", std::process::id()));
    let source_dir = root.join("src");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&source_dir).expect("the fixture directory is created");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"kvim_lsp_smoke\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .expect("the fixture manifest is written");
    let invalid = "fn main() { missing(); }\n";
    std::fs::write(source_dir.join("main.rs"), invalid).expect("the invalid source is written");
    let workspace = WorkspaceRoot::new(root.clone()).expect("the fixture root is absolute");
    let transport = TransportFactory::process(
        ServerLaunchRequest::new(OsString::from("rust-analyzer"), Vec::new(), workspace)
            .expect("the rust-analyzer launch request is valid"),
    );
    let session = Session::open_at(
        root.clone(),
        vec![declared(0, CompletionPolicy::PullAfterRefresh)],
        vec![transport],
    );

    let finding = ready(
        session
            .ask(
                ChangedFile::new(
                    WorktreeRelativePath::new(DOCUMENT).expect("the path is relative"),
                    invalid.to_owned(),
                    DocumentRevision::new(1),
                    language(),
                )
                .wait(WaitPolicy::Until(Duration::from_secs(60))),
            )
            .await,
    );
    assert!(
        !finding.diagnostics().is_empty(),
        "invalid Rust must return a finding: {:?}",
        finding.servers()
    );

    let clean = ready(session.ask(request(2)).await);
    assert!(
        clean.diagnostics().is_empty(),
        "the corrected revision must return Clean"
    );

    session.close().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_request_over_a_real_child_leaves_no_untracked_process() {
    let marker = std::env::temp_dir().join(format!("kvim-lsp-child-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    // The child records its own identifier and then replaces itself, so the
    // recorded identifier names the process that the session must end.
    let script = format!("printf '%s' $$ > '{}'; exec sleep 600", marker.display());
    let transport = TransportFactory::process(
        ServerLaunchRequest::new(
            OsString::from(SHELL),
            vec![OsString::from("-c"), OsString::from(script)],
            WorkspaceRoot::new(PathBuf::from("/")).expect("the process root is valid"),
        )
        .expect("the process request is valid"),
    );
    let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);

    // The child answers no handshake, so the request reaches its deadline.
    let outcome = session
        .ask(request(1).wait(WaitPolicy::Until(SHORT_DEADLINE)))
        .await;
    assert!(matches!(outcome, DiagnosticsOutcome::TimedOut));
    let pid = recorded_pid(&marker).await;

    session.close().await;

    let deadline = time::Instant::now() + TEST_DEADLINE;
    while is_running(pid).await {
        assert!(
            time::Instant::now() < deadline,
            "the closed project left child {pid} running"
        );
        time::sleep(Duration::from_millis(10)).await;
    }
    let _ = std::fs::remove_file(&marker);
}

/// Waits until the child recorded its process identifier.
async fn recorded_pid(marker: &Path) -> u32 {
    let deadline = time::Instant::now() + TEST_DEADLINE;
    loop {
        if let Ok(text) = std::fs::read_to_string(marker)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            return pid;
        }
        assert!(
            time::Instant::now() < deadline,
            "the child recorded no process identifier"
        );
        time::sleep(Duration::from_millis(10)).await;
    }
}

/// Reports whether the system still holds one process identifier.
async fn is_running(pid: u32) -> bool {
    Command::new(SHELL)
        .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
        .status()
        .await
        .expect("the probe runs")
        .success()
}
