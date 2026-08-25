use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream, duplex};
use tokio::task::JoinHandle;
use tokio::time;

use crate::process::{Transport, TransportFactory};
use crate::protocol::{LSP_OUTPUT_BYTES_MAX, LspBound, LspError, RpcId, read_frame};

use super::{
    Attempt, AttemptEnd, LSP_EVENT_QUEUE_CAPACITY, LSP_OPEN_DOCUMENTS_MAX, ManagerLimits,
    ProjectDeclaration, ProjectDriver, ProjectHandle, ProjectId, ProjectManager, RequestKey,
    ServerConversation, ServerDeclaration, ServerEvent, ServerId, WorkspaceRoot,
};

/// The capacity of one test pipe, in bytes.
const PIPE_BYTES: usize = 64 * 1024;

/// The guard that stops a broken test instead of hanging the suite.
const TEST_DEADLINE: Duration = Duration::from_secs(30);

/// The request that every test conversation sends after its handshake.
const ECHO_METHOD: &str = "test/echo";

/// The root of the first project of every test.
const FIRST_ROOT: &str = "/workspace/first";

/// The root of the second project of every test.
const SECOND_ROOT: &str = "/workspace/second";

/// The answers that every conversation of one test records.
///
/// The key names the project, the server, and the request number, so a test
/// proves that two projects with one request number never share an answer.
type Answers = Arc<Mutex<Vec<(RequestKey, String)>>>;

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

    /// Answers `initialize` and reads the `initialized` notification.
    async fn handshake(&mut self) {
        let initialize = self
            .read_message()
            .await
            .expect("the session sends initialize");
        assert_eq!(initialize["method"], "initialize");
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": initialize["id"],
            "result": { "capabilities": { "positionEncoding": "utf-8" } },
        }))
        .await;
        let initialized = self
            .read_message()
            .await
            .expect("the session sends initialized");
        assert_eq!(initialized["method"], "initialized");
    }
}

/// Creates one connected stream pair.
fn pipe() -> (Transport, MockServer) {
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

/// Answers the handshake and every echo request with one payload.
///
/// The payload names the project of the server, so a test proves that an
/// answer never crosses a project boundary.
fn serve_mock(mut server: MockServer, payload: &'static str) -> JoinHandle<()> {
    tokio::spawn(async move {
        server.handshake().await;
        while let Some(message) = server.read_message().await {
            if message["method"] == ECHO_METHOD {
                server
                    .send(&json!({
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": payload,
                    }))
                    .await;
            }
        }
    })
}

/// The conversation that every project test drives.
///
/// It sends one request, correlates the answer by project, server, and
/// request number, and then waits for the cancellation of its project.
struct EchoConversation {
    answers: Answers,
}

impl ServerConversation for EchoConversation {
    async fn serve(&mut self, attempt: Attempt<'_>) -> AttemptEnd {
        let number = match attempt.writer.request(ECHO_METHOD, json!({})).await {
            Ok(number) => number,
            Err(error) => return AttemptEnd::Failed(error),
        };
        let key = attempt.address.request(number);
        loop {
            tokio::select! {
                biased;
                () = attempt.cancellation.cancelled() => return AttemptEnd::Stopped,
                envelope = attempt.envelopes.recv() => match envelope {
                    Some(Ok(envelope)) => {
                        let answered =
                            matches!(envelope.id, Some(RpcId::Unsigned(id)) if id == number);
                        if let (true, Some(result)) = (answered, envelope.result) {
                            self.answers
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push((key, result.get().to_owned()));
                        }
                    }
                    Some(Err(error)) => return AttemptEnd::Failed(error),
                    None => return AttemptEnd::Failed(LspError::Stopped),
                },
            }
        }
    }
}

/// Declares one project of one root over one prepared transport.
fn declaration(
    id: ProjectId,
    root: &str,
    answers: &Answers,
) -> (ProjectDeclaration<EchoConversation>, MockServer) {
    let (transport, server) = pipe();
    let declaration = ProjectDeclaration::new(
        id,
        WorkspaceRoot::new(PathBuf::from(root)).expect("the root is absolute"),
    )
    .server(
        ServerDeclaration {
            id: ServerId::new(0),
            transport: TransportFactory::Prepared(vec![transport]),
            options: json!({}),
            workspace_settings: None,
        },
        EchoConversation {
            answers: Arc::clone(answers),
        },
    );
    (declaration, server)
}

/// Returns the failure of one refused project.
///
/// The accepted values of the manager hold running futures and process
/// handles, so neither is comparable and neither prints.
fn refusal(opened: Result<(ProjectHandle, ProjectDriver<EchoConversation>), LspError>) -> LspError {
    match opened {
        Ok(_) => panic!("the manager accepted a project that passes its budget"),
        Err(error) => error,
    }
}

/// Waits until one project answered its request.
async fn await_answer(answers: &Answers, project: ProjectId) -> String {
    let deadline = time::Instant::now() + TEST_DEADLINE;
    loop {
        let found = answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(key, _)| key.address().project() == project)
            .map(|(_, payload)| payload.clone());
        if let Some(found) = found {
            return found;
        }
        assert!(
            time::Instant::now() < deadline,
            "project {project:?} answered no request"
        );
        time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn two_roots_run_two_independent_projects() {
    let answers: Answers = Arc::new(Mutex::new(Vec::new()));
    let manager = ProjectManager::new(ManagerLimits::default());
    let first = ProjectId::new(1);
    let second = ProjectId::new(2);
    let (one, server_one) = declaration(first, FIRST_ROOT, &answers);
    let (two, server_two) = declaration(second, SECOND_ROOT, &answers);
    let (handle_one, driver_one) = manager.open(one).expect("the budget holds one project");
    let (handle_two, driver_two) = manager.open(two).expect("the budget holds two projects");
    let mock_one = serve_mock(server_one, "first");
    let mock_two = serve_mock(server_two, "second");
    let task_one = tokio::spawn(driver_one.run());
    let task_two = tokio::spawn(driver_two.run());

    assert_eq!(await_answer(&answers, first).await, "\"first\"");
    assert_eq!(await_answer(&answers, second).await, "\"second\"");
    assert_eq!(manager.projects(), 2, "both projects hold their budget");

    handle_one.close().await;
    handle_two.close().await;
    let _ = task_one.await;
    let _ = task_two.await;
    mock_one.abort();
    mock_two.abort();
    assert_eq!(manager.projects(), 0, "a closed project returns its budget");
}

#[tokio::test]
async fn two_projects_of_one_root_stay_separate() {
    let answers: Answers = Arc::new(Mutex::new(Vec::new()));
    let manager = ProjectManager::new(ManagerLimits::default());
    let first = ProjectId::new(1);
    let second = ProjectId::new(2);
    let (one, server_one) = declaration(first, FIRST_ROOT, &answers);
    let (two, server_two) = declaration(second, FIRST_ROOT, &answers);
    let (handle_one, driver_one) = manager.open(one).expect("the budget holds one project");
    let (handle_two, driver_two) = manager.open(two).expect("one root holds two projects");
    let mock_one = serve_mock(server_one, "first");
    let mock_two = serve_mock(server_two, "second");
    let task_one = tokio::spawn(driver_one.run());
    let task_two = tokio::spawn(driver_two.run());

    assert_eq!(await_answer(&answers, first).await, "\"first\"");
    assert_eq!(await_answer(&answers, second).await, "\"second\"");

    handle_one.close().await;
    handle_two.close().await;
    let _ = task_one.await;
    let _ = task_two.await;
    mock_one.abort();
    mock_two.abort();
}

#[tokio::test]
async fn one_request_number_of_two_projects_correlates_apart() {
    let answers: Answers = Arc::new(Mutex::new(Vec::new()));
    let manager = ProjectManager::new(ManagerLimits::default());
    let first = ProjectId::new(1);
    let second = ProjectId::new(2);
    let (one, server_one) = declaration(first, FIRST_ROOT, &answers);
    let (two, server_two) = declaration(second, SECOND_ROOT, &answers);
    let (handle_one, driver_one) = manager.open(one).expect("the budget holds one project");
    let (handle_two, driver_two) = manager.open(two).expect("the budget holds two projects");
    let mock_one = serve_mock(server_one, "first");
    let mock_two = serve_mock(server_two, "second");
    let task_one = tokio::spawn(driver_one.run());
    let task_two = tokio::spawn(driver_two.run());

    await_answer(&answers, first).await;
    await_answer(&answers, second).await;
    let recorded = answers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    handle_one.close().await;
    handle_two.close().await;
    let _ = task_one.await;
    let _ = task_two.await;
    mock_one.abort();
    mock_two.abort();

    assert_eq!(recorded.len(), 2, "both projects answered exactly once");
    let (first_key, first_payload) = &recorded[0];
    let (second_key, second_payload) = &recorded[1];
    // Both servers number their first request the same way, so only the
    // project and the server of the key keep the two answers apart.
    assert_eq!(first_key.number(), second_key.number());
    assert_ne!(first_key, second_key);
    assert_ne!(first_payload, second_payload);
    for (key, payload) in &recorded {
        let expected = match key.address().project() {
            project if project == first => "\"first\"",
            _ => "\"second\"",
        };
        assert_eq!(payload, expected, "an answer crossed a project boundary");
    }
}

#[tokio::test]
async fn closing_one_project_leaves_the_other_serving() {
    let answers: Answers = Arc::new(Mutex::new(Vec::new()));
    let manager = ProjectManager::new(ManagerLimits::default());
    let first = ProjectId::new(1);
    let second = ProjectId::new(2);
    let (one, server_one) = declaration(first, FIRST_ROOT, &answers);
    let (two, server_two) = declaration(second, SECOND_ROOT, &answers);
    let (handle_one, driver_one) = manager.open(one).expect("the budget holds one project");
    let (mut handle_two, driver_two) = manager.open(two).expect("the budget holds two projects");
    let mock_one = serve_mock(server_one, "first");
    let mock_two = serve_mock(server_two, "second");
    let task_one = tokio::spawn(driver_one.run());
    let task_two = tokio::spawn(driver_two.run());

    await_answer(&answers, first).await;
    handle_one.close().await;
    let _ = task_one.await;
    assert_eq!(manager.projects(), 1, "only the closed project left");

    // The second project never received the cancellation of the first, so
    // it still answers and still records its steps.
    assert_eq!(await_answer(&answers, second).await, "\"second\"");
    let event = time::timeout(TEST_DEADLINE, handle_two.recv())
        .await
        .expect("the second project records before the test deadline")
        .expect("the second project queue stays open");
    assert_eq!(event.address.project(), second);
    assert!(matches!(event.event, ServerEvent::Started));
    assert!(!task_two.is_finished(), "the second driver still runs");

    handle_two.close().await;
    let _ = task_two.await;
    mock_one.abort();
    mock_two.abort();
}

#[tokio::test]
async fn dropping_the_handle_cancels_the_project() {
    let answers: Answers = Arc::new(Mutex::new(Vec::new()));
    let manager = ProjectManager::new(ManagerLimits::default());
    let project = ProjectId::new(1);
    let (declared, server) = declaration(project, FIRST_ROOT, &answers);
    let (handle, driver) = manager
        .open(declared)
        .expect("the budget holds one project");
    let mock = serve_mock(server, "first");
    let task = tokio::spawn(driver.run());
    await_answer(&answers, project).await;

    // A host that forgets one project must still leave no running child, so
    // the drop requests the cancellation of the complete project.
    drop(handle);

    time::timeout(TEST_DEADLINE, task)
        .await
        .expect("the dropped handle ends the driver before the test deadline")
        .expect("the driver ends without a panic");
    mock.abort();
    assert_eq!(
        manager.projects(),
        0,
        "the dropped handle returns its budget"
    );
}

#[test]
fn one_identity_opens_exactly_one_project() {
    let answers: Answers = Arc::new(Mutex::new(Vec::new()));
    let manager = ProjectManager::new(ManagerLimits::default());
    let project = ProjectId::new(1);
    let (first, _server_one) = declaration(project, FIRST_ROOT, &answers);
    let (second, _server_two) = declaration(project, SECOND_ROOT, &answers);
    let (handle, _driver) = manager.open(first).expect("the budget holds one project");

    assert!(matches!(manager.open(second), Err(LspError::ProjectOpen)));

    // The released identity opens again, so a closed project leaves no
    // reservation behind.
    drop(handle);
    let (third, _server_three) = declaration(project, SECOND_ROOT, &answers);
    assert!(manager.open(third).is_ok());
}

#[test]
fn two_servers_of_one_project_never_take_one_identity() {
    let answers: Answers = Arc::new(Mutex::new(Vec::new()));
    let manager = ProjectManager::new(ManagerLimits::default());
    let (declared, _server) = declaration(ProjectId::new(1), FIRST_ROOT, &answers);
    let (transport, _second) = pipe();
    let declared = declared.server(
        ServerDeclaration {
            id: ServerId::new(0),
            transport: TransportFactory::Prepared(vec![transport]),
            options: json!({}),
            workspace_settings: None,
        },
        EchoConversation {
            answers: Arc::clone(&answers),
        },
    );

    assert!(matches!(
        refusal(manager.open(declared)),
        LspError::DuplicateServer
    ));
    assert_eq!(manager.projects(), 0, "a refused project reserves nothing");
}

#[test]
fn the_shared_budget_refuses_the_project_that_passes_it() {
    let answers: Answers = Arc::new(Mutex::new(Vec::new()));
    let limits = ManagerLimits {
        projects: 2,
        processes: 1,
        open_documents: LSP_OPEN_DOCUMENTS_MAX,
        queue_capacity: LSP_EVENT_QUEUE_CAPACITY,
    };
    let manager = ProjectManager::new(limits);
    let (first, _server_one) = declaration(ProjectId::new(1), FIRST_ROOT, &answers);
    let (second, _server_two) = declaration(ProjectId::new(2), SECOND_ROOT, &answers);
    let (_handle, _driver) = manager
        .open(first.open_documents(1))
        .expect("the first process fits");

    // The second project needs one further process, and the manager holds
    // one process in total.
    let refused = refusal(manager.open(second.open_documents(1)));
    assert!(matches!(
        refused,
        LspError::Bounds {
            measure: LspBound::Processes,
            limit: 1,
            actual: 2,
        }
    ));
}

#[test]
fn the_shared_queue_capacity_refuses_the_project_that_passes_it() {
    let answers: Answers = Arc::new(Mutex::new(Vec::new()));
    let limits = ManagerLimits {
        projects: 4,
        processes: 4,
        open_documents: LSP_OPEN_DOCUMENTS_MAX,
        queue_capacity: 8,
    };
    let manager = ProjectManager::new(limits);
    let (first, _server_one) = declaration(ProjectId::new(1), FIRST_ROOT, &answers);
    let (second, _server_two) = declaration(ProjectId::new(2), SECOND_ROOT, &answers);
    let (_handle, _driver) = manager
        .open(first.open_documents(1).queue_capacity(8))
        .expect("the first reservation fits");

    let refused = refusal(manager.open(second.open_documents(1).queue_capacity(1)));
    assert!(matches!(
        refused,
        LspError::Bounds {
            measure: LspBound::QueueCapacity,
            limit: 8,
            actual: 9,
        }
    ));
}

#[test]
fn one_project_never_reserves_more_than_its_own_limits() {
    let answers: Answers = Arc::new(Mutex::new(Vec::new()));
    let manager = ProjectManager::new(ManagerLimits::default());
    let (declared, _server) = declaration(ProjectId::new(1), FIRST_ROOT, &answers);
    let refused = refusal(manager.open(declared.open_documents(LSP_OPEN_DOCUMENTS_MAX + 1)));
    assert!(matches!(
        refused,
        LspError::Bounds {
            measure: LspBound::OpenDocuments,
            limit: LSP_OPEN_DOCUMENTS_MAX,
            ..
        }
    ));
    assert_eq!(manager.projects(), 0, "a refused project reserves nothing");
}
