use std::ffi::OsString;
use std::io;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream, duplex};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::process::{
    LSP_RESTARTS_MAX, LaunchedServer, ServerLaunchError, ServerLaunchRequest, ServerLauncher,
    ServerProcessHandle, ServerTerminate, ServerWait, Transport, TransportFactory,
};
use crate::protocol::{LSP_OUTPUT_BYTES_MAX, LspBound, LspError, RpcId, read_frame};

use super::{
    Attempt, AttemptEnd, LSP_EVENT_QUEUE_CAPACITY, LSP_OPEN_DOCUMENTS_MAX, ManagerLimits,
    ProjectDeclaration, ProjectDriver, ProjectEvent, ProjectHandle, ProjectId, ProjectManager,
    RequestKey, ServerConversation, ServerDeclaration, ServerEvent, ServerId, ServerSupervisor,
    WorkspaceRoot,
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

struct NoopConversation;

impl ServerConversation for NoopConversation {
    async fn serve(&mut self, _: Attempt<'_>) -> AttemptEnd {
        panic!("a failed handshake never starts the conversation")
    }
}

struct FixtureLifecycle {
    active: Arc<AtomicUsize>,
    terminated: Arc<AtomicUsize>,
    waited: Arc<AtomicUsize>,
    exit: Arc<Mutex<Option<oneshot::Sender<ExitStatus>>>>,
    result: Option<oneshot::Receiver<ExitStatus>>,
}

impl Drop for FixtureLifecycle {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
        if let Some(exit) = self.exit.lock().expect("fixture exit lock").take() {
            let _ = exit.send(ExitStatus::from_raw(0));
        }
    }
}

impl ServerProcessHandle for FixtureLifecycle {
    fn wait(&mut self) -> ServerWait {
        self.waited.fetch_add(1, Ordering::Relaxed);
        let result = self.result.take().expect("Kvim takes one wait future");
        Box::pin(async move {
            result
                .await
                .map_err(|_| crate::process::ServerWaitError(io::Error::other("fixture stopped")))
        })
    }

    fn terminate(&mut self) -> ServerTerminate<'_> {
        self.terminated.fetch_add(1, Ordering::Relaxed);
        let exit = self.exit.clone();
        Box::pin(async move {
            if let Some(exit) = exit.lock().expect("fixture exit lock").take() {
                let _ = exit.send(ExitStatus::from_raw(0));
            }
            Ok(())
        })
    }
}

struct RecordingProjectLauncher {
    expected: ServerLaunchRequest,
    requests: Arc<Mutex<Vec<ServerLaunchRequest>>>,
    launch_signal: Option<mpsc::UnboundedSender<()>>,
    keep_handshake_open: bool,
    peers: Vec<DuplexStream>,
    active: Arc<AtomicUsize>,
    terminated: Arc<AtomicUsize>,
    waited: Arc<AtomicUsize>,
}

impl ServerLauncher for RecordingProjectLauncher {
    fn launch(
        &mut self,
        request: &ServerLaunchRequest,
    ) -> Result<LaunchedServer, ServerLaunchError> {
        assert_eq!(request, &self.expected);
        self.requests
            .lock()
            .expect("request record lock")
            .push(request.clone());
        self.active.fetch_add(1, Ordering::Relaxed);
        if let Some(signal) = &self.launch_signal {
            let _ = signal.send(());
        }

        let (input, input_peer) = duplex(PIPE_BYTES);
        self.peers.push(input_peer);
        let (output, output_peer) = duplex(PIPE_BYTES);
        if self.keep_handshake_open {
            self.peers.push(output_peer);
        }
        let (errors, _errors_peer) = duplex(PIPE_BYTES);
        let (exit, result) = oneshot::channel();
        Ok(LaunchedServer::new(
            input,
            output,
            errors,
            FixtureLifecycle {
                active: self.active.clone(),
                terminated: self.terminated.clone(),
                waited: self.waited.clone(),
                exit: Arc::new(Mutex::new(Some(exit))),
                result: Some(result),
            },
        ))
    }
}

fn launch_request() -> ServerLaunchRequest {
    ServerLaunchRequest::new(
        OsString::from("fixture-server"),
        vec![OsString::from("--stdio"), OsString::from("ordered")],
        WorkspaceRoot::new(PathBuf::from(FIRST_ROOT)).expect("the root is absolute"),
    )
    .expect("the launch request is valid")
}

fn supervisor(
    launcher: impl ServerLauncher + 'static,
    events: mpsc::Sender<ProjectEvent>,
) -> ServerSupervisor<
    'static,
    NoopConversation,
    mpsc::Sender<ProjectEvent>,
    impl Fn(crate::process::ServerReport) + Clone + Send + 'static,
> {
    let root = Box::leak(Box::new(
        WorkspaceRoot::new(PathBuf::from(FIRST_ROOT)).expect("the root is absolute"),
    ));
    let options = Box::leak(Box::new(json!({})));
    ServerSupervisor {
        address: ProjectId::FIRST.server(ServerId::new(0)),
        factory: TransportFactory::process_with(launch_request(), launcher),
        handshake: super::Handshake {
            root,
            options,
            settings: None,
        },
        conversation: NoopConversation,
        events,
        report: |_| {},
    }
}

#[tokio::test]
async fn every_restart_reuses_one_launcher_and_the_exact_request() {
    let expected = launch_request();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let terminated = Arc::new(AtomicUsize::new(0));
    let waited = Arc::new(AtomicUsize::new(0));
    let launcher = RecordingProjectLauncher {
        expected: expected.clone(),
        requests: requests.clone(),
        launch_signal: None,
        keep_handshake_open: false,
        peers: Vec::new(),
        active: active.clone(),
        terminated: terminated.clone(),
        waited: waited.clone(),
    };
    let (events, mut received) = mpsc::channel(32);

    supervisor(launcher, events)
        .run(&CancellationToken::new())
        .await;

    assert_eq!(
        *requests.lock().expect("request record lock"),
        vec![expected; LSP_RESTARTS_MAX + 1],
        "one caller-owned launcher receives every bounded attempt"
    );
    let mut recorded = Vec::new();
    while let Ok(event) = received.try_recv() {
        recorded.push(event.event);
    }
    assert_eq!(recorded.len(), LSP_RESTARTS_MAX * 2 + 2);
    for restart in 0..LSP_RESTARTS_MAX {
        assert!(
            matches!(recorded[restart * 2], ServerEvent::Failed(_)),
            "a transport failure precedes restart: {:?}",
            recorded[restart * 2]
        );
        assert!(matches!(
            recorded[restart * 2 + 1],
            ServerEvent::Restarted { generation }
                if generation.get() == u64::try_from(restart + 1).expect("the bound fits")
        ));
    }
    assert!(matches!(
        recorded[LSP_RESTARTS_MAX * 2],
        ServerEvent::Failed(_)
    ));
    assert!(matches!(recorded.last(), Some(ServerEvent::Stopped)));
    assert_eq!(active.load(Ordering::Relaxed), 0);
    assert_eq!(terminated.load(Ordering::Relaxed), LSP_RESTARTS_MAX + 1);
    assert_eq!(waited.load(Ordering::Relaxed), LSP_RESTARTS_MAX + 1);
}

struct UnavailableLauncher {
    calls: Arc<AtomicUsize>,
}

impl ServerLauncher for UnavailableLauncher {
    fn launch(&mut self, _: &ServerLaunchRequest) -> Result<LaunchedServer, ServerLaunchError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(ServerLaunchError::Unavailable(io::Error::new(
            io::ErrorKind::NotFound,
            "typed fixture absence",
        )))
    }
}

struct StartFailingLauncher {
    calls: Arc<AtomicUsize>,
}

impl ServerLauncher for StartFailingLauncher {
    fn launch(&mut self, _: &ServerLaunchRequest) -> Result<LaunchedServer, ServerLaunchError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(ServerLaunchError::Start(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "typed fixture start failure",
        )))
    }
}

#[tokio::test]
async fn restartable_launch_failure_keeps_its_typed_source_and_bound() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (events, mut received) = mpsc::channel(16);
    supervisor(
        StartFailingLauncher {
            calls: calls.clone(),
        },
        events,
    )
    .run(&CancellationToken::new())
    .await;

    assert_eq!(calls.load(Ordering::Relaxed), LSP_RESTARTS_MAX + 1);
    for attempt in 0..=LSP_RESTARTS_MAX {
        let event = received.recv().await.expect("the attempt records failure");
        let ServerEvent::Failed(LspError::Spawn(source)) = event.event else {
            panic!("the restartable launch failure stays typed")
        };
        assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
        if attempt < LSP_RESTARTS_MAX {
            let restart = received.recv().await.expect("the attempt records restart");
            assert!(matches!(restart.event, ServerEvent::Restarted { .. }));
        }
    }
    assert!(matches!(
        received.recv().await.map(|event| event.event),
        Some(ServerEvent::Stopped)
    ));
}

#[tokio::test]
async fn unavailable_launcher_is_terminal_without_a_restart() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (events, mut received) = mpsc::channel(4);
    supervisor(
        UnavailableLauncher {
            calls: calls.clone(),
        },
        events,
    )
    .run(&CancellationToken::new())
    .await;

    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(matches!(
        received.recv().await.map(|event| event.event),
        Some(ServerEvent::Unavailable)
    ));
    assert!(received.try_recv().is_err());
}

#[tokio::test]
async fn cancellation_before_launch_starts_no_attempt() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (events, mut received) = mpsc::channel(4);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    supervisor(
        UnavailableLauncher {
            calls: calls.clone(),
        },
        events,
    )
    .run(&cancellation)
    .await;

    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(matches!(
        received.recv().await.map(|event| event.event),
        Some(ServerEvent::Stopped)
    ));
}

#[tokio::test]
async fn cancellation_during_launch_cleans_up_without_a_restart() {
    let expected = launch_request();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let terminated = Arc::new(AtomicUsize::new(0));
    let waited = Arc::new(AtomicUsize::new(0));
    let (launched, mut launch_signal) = mpsc::unbounded_channel();
    let launcher = RecordingProjectLauncher {
        expected,
        requests: requests.clone(),
        launch_signal: Some(launched),
        keep_handshake_open: true,
        peers: Vec::new(),
        active: active.clone(),
        terminated: terminated.clone(),
        waited: waited.clone(),
    };
    let (events, mut received) = mpsc::channel(4);
    let cancellation = CancellationToken::new();
    let run_cancellation = cancellation.clone();
    let running = tokio::spawn(async move {
        supervisor(launcher, events).run(&run_cancellation).await;
    });
    launch_signal.recv().await.expect("the first launch starts");
    cancellation.cancel();
    running.await.expect("the supervisor ends");

    assert_eq!(requests.lock().expect("request record lock").len(), 1);
    assert_eq!(active.load(Ordering::Relaxed), 0);
    assert_eq!(terminated.load(Ordering::Relaxed), 1);
    assert_eq!(waited.load(Ordering::Relaxed), 1);
    assert!(matches!(
        received.recv().await.map(|event| event.event),
        Some(ServerEvent::Stopped)
    ));
    assert!(received.try_recv().is_err());
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
