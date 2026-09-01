use std::cell::RefCell;
use std::error::Error;
use std::ffi::OsString;
use std::io;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time;

use super::{
    DefaultServerLauncher, ErrorRecorder, LSP_RESULT_ID_BYTES_MAX, LSP_SERVER_ARGUMENT_BYTES_MAX,
    LSP_SERVER_ARGUMENTS_MAX, LSP_SERVER_COMMAND_BYTES_MAX, LSP_SERVER_PROGRAM_BYTES_MAX,
    LSP_STDERR_BYTES_MAX, LSP_STDERR_LINE_BYTES_MAX, LaunchedServer, ServerLaunchError,
    ServerLaunchRequest, ServerLauncher, ServerProcess, ServerProcessHandle, ServerReport,
    ServerTerminate, ServerWait, SynchronizationMode, Transport, TransportFactory, WorkspaceRoot,
    diagnostics_model, synchronization_mode,
};

/// The shell that runs every child of these tests.
///
/// The child is no language server. The tests drive a real pipe and a real
/// process, so no prepared stream pair can replace them.
const SHELL: &str = "/bin/sh";

/// The guard that stops a broken test instead of hanging the suite.
const TEST_DEADLINE: Duration = Duration::from_secs(30);

/// The interval between two liveness probes of one child.
const PROBE_INTERVAL: Duration = Duration::from_millis(10);

/// The line that a broken server writes before it exits.
const SHIM_LINE: &str = "info: `rust-analyzer` is unavailable for the active toolchain";

fn request(
    program: OsString,
    arguments: Vec<OsString>,
) -> Result<ServerLaunchRequest, super::LspError> {
    ServerLaunchRequest::new(
        program,
        arguments,
        WorkspaceRoot::new(PathBuf::from("/")).expect("the root is valid"),
    )
}

struct FakeLifecycle {
    dropped: Arc<AtomicBool>,
    terminated: Arc<AtomicUsize>,
    waited: Arc<AtomicUsize>,
}

impl Drop for FakeLifecycle {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Relaxed);
    }
}

impl ServerProcessHandle for FakeLifecycle {
    fn wait(&mut self) -> ServerWait {
        let waited = self.waited.clone();
        Box::pin(async move {
            waited.fetch_add(1, Ordering::Relaxed);
            std::future::pending().await
        })
    }

    fn terminate(&mut self) -> ServerTerminate<'_> {
        let terminated = self.terminated.clone();
        Box::pin(async move {
            terminated.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
    }
}

struct RecordingLauncher {
    requests: Arc<Mutex<Vec<ServerLaunchRequest>>>,
    dropped: Arc<AtomicBool>,
    terminated: Arc<AtomicUsize>,
    waited: Arc<AtomicUsize>,
}

impl ServerLauncher for RecordingLauncher {
    fn launch(
        &mut self,
        request: &ServerLaunchRequest,
    ) -> Result<LaunchedServer, ServerLaunchError> {
        self.requests
            .lock()
            .expect("record lock")
            .push(request.clone());
        let (input, _) = duplex(64);
        let (output, _) = duplex(64);
        let (errors, _) = duplex(64);
        Ok(LaunchedServer::new(
            input,
            output,
            errors,
            FakeLifecycle {
                dropped: self.dropped.clone(),
                terminated: self.terminated.clone(),
                waited: self.waited.clone(),
            },
        ))
    }
}

#[test]
fn launch_request_preserves_valid_values_and_rejects_every_bound() {
    let root = WorkspaceRoot::new(PathBuf::from("/")).expect("the root is valid");
    let valid = ServerLaunchRequest::new(
        OsString::from("server"),
        vec![OsString::from("first"), OsString::from("second")],
        root.clone(),
    )
    .expect("the request is valid");
    assert_eq!(valid.program(), "server");
    assert_eq!(valid.arguments(), ["first", "second"]);
    assert_eq!(valid.root(), &root);

    assert!(matches!(
        request(OsString::new(), vec![]),
        Err(super::LspError::EmptyProgram)
    ));
    let cases = [
        request(
            OsString::from("x".repeat(LSP_SERVER_PROGRAM_BYTES_MAX + 1)),
            vec![],
        ),
        request(
            OsString::from("x"),
            vec![OsString::new(); LSP_SERVER_ARGUMENTS_MAX + 1],
        ),
        request(
            OsString::from("x"),
            vec![OsString::from(
                "x".repeat(LSP_SERVER_ARGUMENT_BYTES_MAX + 1),
            )],
        ),
        request(
            OsString::from("x"),
            vec![OsString::from("x".repeat(LSP_SERVER_COMMAND_BYTES_MAX))],
        ),
    ];
    for case in cases {
        assert!(matches!(case, Err(super::LspError::Bounds { .. })));
    }
}

#[test]
fn process_factory_forwards_the_exact_request_and_owns_the_lifecycle() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let dropped = Arc::new(AtomicBool::new(false));
    let terminated = Arc::new(AtomicUsize::new(0));
    let waited = Arc::new(AtomicUsize::new(0));
    let expected = request(
        OsString::from("fixture"),
        vec![OsString::from("--one"), OsString::from("two")],
    )
    .expect("the request is valid");
    let mut factory = TransportFactory::process_with(
        expected.clone(),
        RecordingLauncher {
            requests: requests.clone(),
            dropped: dropped.clone(),
            terminated: terminated.clone(),
            waited: waited.clone(),
        },
    );
    let transport = factory.create().expect("the launcher returns a transport");
    assert_eq!(*requests.lock().expect("record lock"), vec![expected]);
    assert!(!dropped.load(Ordering::Relaxed));
    assert_eq!(terminated.load(Ordering::Relaxed), 0);
    assert_eq!(waited.load(Ordering::Relaxed), 0);
    drop(transport);
    assert!(dropped.load(Ordering::Relaxed));
}

#[tokio::test]
async fn lifecycle_termination_and_wait_are_distinct_operations() {
    let dropped = Arc::new(AtomicBool::new(false));
    let terminated = Arc::new(AtomicUsize::new(0));
    let waited = Arc::new(AtomicUsize::new(0));
    let mut lifecycle = FakeLifecycle {
        dropped: dropped.clone(),
        terminated: terminated.clone(),
        waited: waited.clone(),
    };

    lifecycle
        .terminate()
        .await
        .expect("the fixture accepts termination");
    assert_eq!(terminated.load(Ordering::Relaxed), 1);
    assert_eq!(waited.load(Ordering::Relaxed), 0);

    assert!(
        time::timeout(Duration::from_millis(1), lifecycle.wait())
            .await
            .is_err(),
        "waiting remains a separate reap operation"
    );
    assert_eq!(terminated.load(Ordering::Relaxed), 1);
    assert_eq!(waited.load(Ordering::Relaxed), 1);

    drop(lifecycle);
    assert!(dropped.load(Ordering::Relaxed));
}

struct FailingLauncher(io::ErrorKind);

impl ServerLauncher for FailingLauncher {
    fn launch(&mut self, _: &ServerLaunchRequest) -> Result<LaunchedServer, ServerLaunchError> {
        let source = io::Error::new(self.0, "fixture start failure");
        Err(match self.0 {
            io::ErrorKind::NotFound => ServerLaunchError::Unavailable(source),
            _ => ServerLaunchError::Start(source),
        })
    }
}

#[test]
fn process_factory_preserves_typed_start_sources() {
    for (kind, unavailable) in [
        (io::ErrorKind::NotFound, true),
        (io::ErrorKind::PermissionDenied, false),
    ] {
        let mut factory = TransportFactory::process_with(
            request(OsString::from("fixture"), vec![]).expect("the request is valid"),
            FailingLauncher(kind),
        );
        let error = factory.create().err().expect("the launch fails");
        assert_eq!(
            matches!(error, super::LspError::Unavailable(_)),
            unavailable
        );
        let source = error
            .source()
            .and_then(|cause| cause.downcast_ref::<io::Error>())
            .expect("the source remains available");
        assert_eq!(source.kind(), kind);
    }
}

#[test]
fn default_launcher_classifies_an_absent_executable_with_its_source() {
    let mut launcher = DefaultServerLauncher;
    let error = launcher
        .launch(
            &request(OsString::from("/kvim/no/such/server"), vec![]).expect("the request is valid"),
        )
        .err()
        .expect("the executable is absent");
    let ServerLaunchError::Unavailable(source) = error else {
        panic!("the error is typed unavailable")
    };
    assert_eq!(source.kind(), io::ErrorKind::NotFound);
}

#[cfg(unix)]
#[tokio::test]
async fn default_lifecycle_termination_is_idempotent_after_reap() {
    let child = Command::new(SHELL)
        .args(["-c", "exit 0"])
        .spawn()
        .expect("the fixture child starts");
    let mut lifecycle = super::TokioServerHandle::new(child);

    let status = lifecycle.wait().await.expect("the owner reaps the child");
    assert!(status.success());
    lifecycle
        .terminate()
        .await
        .expect("termination after exit is idempotent");
}

type ExitResult = Result<ExitStatus, io::Error>;
type ExitSender = tokio::sync::oneshot::Sender<ExitResult>;
type ExitReceiver = tokio::sync::oneshot::Receiver<ExitResult>;

struct ControlledLifecycle {
    dropped: Arc<AtomicBool>,
    terminated: Arc<AtomicUsize>,
    waited: Arc<AtomicUsize>,
    exit: Option<ExitReceiver>,
    terminate_result: Option<io::ErrorKind>,
    exit_on_terminate: Arc<Mutex<Option<ExitSender>>>,
}

impl Drop for ControlledLifecycle {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Relaxed);
    }
}

impl ServerProcessHandle for ControlledLifecycle {
    fn wait(&mut self) -> ServerWait {
        self.waited.fetch_add(1, Ordering::Relaxed);
        let exit = self.exit.take().expect("Kvim takes one wait future");
        Box::pin(async move {
            exit.await
                .expect("the fixture keeps the lifecycle result sender")
                .map_err(super::ServerWaitError)
        })
    }

    fn terminate(&mut self) -> ServerTerminate<'_> {
        self.terminated.fetch_add(1, Ordering::Relaxed);
        let result = self.terminate_result;
        let exit = self
            .exit_on_terminate
            .lock()
            .expect("fixture exit lock")
            .take();
        Box::pin(async move {
            if let Some(exit) = exit {
                let _ = exit.send(Ok(ExitStatus::from_raw(0)));
            }
            match result {
                Some(kind) => Err(super::ServerTerminateError(io::Error::new(
                    kind,
                    "fixture termination failure",
                ))),
                None => Ok(()),
            }
        })
    }
}

struct ControlledLauncher(Option<LaunchedServer>);

impl ServerLauncher for ControlledLauncher {
    fn launch(&mut self, _: &ServerLaunchRequest) -> Result<LaunchedServer, ServerLaunchError> {
        Ok(self.0.take().expect("one fixture launch"))
    }
}

struct ControlledProcess {
    process: ServerProcess,
    reports: mpsc::UnboundedReceiver<ServerReport>,
    exit: Option<tokio::sync::oneshot::Sender<Result<ExitStatus, io::Error>>>,
    stderr: Option<tokio::io::DuplexStream>,
    dropped: Arc<AtomicBool>,
    terminated: Arc<AtomicUsize>,
    waited: Arc<AtomicUsize>,
}

fn controlled_process(
    terminate_result: Option<io::ErrorKind>,
    exit_on_terminate: bool,
) -> ControlledProcess {
    let dropped = Arc::new(AtomicBool::new(false));
    let terminated = Arc::new(AtomicUsize::new(0));
    let waited = Arc::new(AtomicUsize::new(0));
    let (exit, waited_exit) = tokio::sync::oneshot::channel();
    let (exit_on_terminate_sender, manual_exit) = if exit_on_terminate {
        (Some(exit), None)
    } else {
        (None, Some(exit))
    };
    let (input, _) = duplex(64);
    let (output, _output_writer) = duplex(64);
    let (errors, stderr) = duplex(64);
    let launched = LaunchedServer::new(
        input,
        output,
        errors,
        ControlledLifecycle {
            dropped: dropped.clone(),
            terminated: terminated.clone(),
            waited: waited.clone(),
            exit: Some(waited_exit),
            terminate_result,
            exit_on_terminate: Arc::new(Mutex::new(exit_on_terminate_sender)),
        },
    );
    let mut factory = TransportFactory::process_with(
        request(OsString::from("fixture"), vec![]).expect("valid request"),
        ControlledLauncher(Some(launched)),
    );
    let (reports, received) = mpsc::unbounded_channel();
    let (process, streams) = ServerProcess::open(&mut factory, move |report| {
        let _ = reports.send(report);
    })
    .expect("fixture opens");
    drop(streams);
    ControlledProcess {
        process,
        reports: received,
        exit: manual_exit,
        stderr: Some(stderr),
        dropped,
        terminated,
        waited,
    }
}

#[tokio::test]
async fn graceful_cleanup_reaps_without_termination_and_finishes_stderr() {
    let mut fixture = controlled_process(None, false);
    fixture.stderr.take();
    fixture
        .exit
        .take()
        .expect("manual exit sender")
        .send(Ok(ExitStatus::from_raw(0)))
        .expect("wait owner remains");
    fixture
        .process
        .close(super::ServerCloseIntent::Graceful {
            deadline: time::Instant::now() + Duration::from_secs(1),
        })
        .await;

    assert_eq!(fixture.terminated.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.waited.load(Ordering::Relaxed), 1);
    assert!(fixture.dropped.load(Ordering::Relaxed));
    assert!(fixture.reports.recv().await.is_none());
}

#[tokio::test]
async fn hung_graceful_cleanup_terminates_once_and_reaps_once() {
    let fixture = controlled_process(None, true);
    fixture
        .process
        .close(super::ServerCloseIntent::Graceful {
            deadline: time::Instant::now(),
        })
        .await;
    assert_eq!(fixture.terminated.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.waited.load(Ordering::Relaxed), 1);
    assert!(fixture.dropped.load(Ordering::Relaxed));
}

#[tokio::test]
async fn immediate_cleanup_terminates_before_its_single_reap() {
    let fixture = controlled_process(None, true);
    fixture
        .process
        .close(super::ServerCloseIntent::Immediate)
        .await;
    assert_eq!(fixture.terminated.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.waited.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn immediate_cleanup_of_exited_lifecycle_reports_no_termination_failure() {
    let mut fixture = controlled_process(None, false);
    fixture.stderr.take();
    fixture
        .exit
        .take()
        .expect("manual exit sender")
        .send(Ok(ExitStatus::from_raw(0)))
        .expect("wait owner remains");

    fixture
        .process
        .close(super::ServerCloseIntent::Immediate)
        .await;

    assert_eq!(fixture.terminated.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.waited.load(Ordering::Relaxed), 1);
    assert!(fixture.dropped.load(Ordering::Relaxed));
    assert!(fixture.reports.recv().await.is_none());
}

#[tokio::test]
async fn cleanup_reports_typed_wait_and_terminate_sources_once() {
    let mut fixture = controlled_process(Some(io::ErrorKind::PermissionDenied), false);
    fixture
        .exit
        .take()
        .expect("manual exit sender")
        .send(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "fixture wait failure",
        )))
        .expect("wait owner remains");
    fixture
        .process
        .close(super::ServerCloseIntent::Immediate)
        .await;

    let mut terminate = 0;
    let mut wait = 0;
    while let Ok(report) = fixture.reports.try_recv() {
        match report {
            ServerReport::CleanupFailed(super::ServerCleanupError::Terminate(error)) => {
                assert_eq!(error.0.kind(), io::ErrorKind::PermissionDenied);
                terminate += 1;
            }
            ServerReport::CleanupFailed(super::ServerCleanupError::Wait(error)) => {
                assert_eq!(error.0.kind(), io::ErrorKind::BrokenPipe);
                wait += 1;
            }
            _ => {}
        }
    }
    assert_eq!((terminate, wait), (1, 1));
}

#[tokio::test]
async fn forced_cleanup_timeout_is_bounded_and_drop_still_fires() {
    let fixture = controlled_process(None, false);
    let dropped = fixture.dropped.clone();
    time::timeout(
        super::LSP_FORCED_CLEANUP_DEADLINE + Duration::from_secs(1),
        fixture.process.close(super::ServerCloseIntent::Immediate),
    )
    .await
    .expect("forced cleanup stays bounded");
    assert!(dropped.load(Ordering::Relaxed));
}

#[tokio::test]
async fn stream_only_cleanup_is_bounded_when_the_remote_keeps_output_open() {
    let (input, _) = duplex(64);
    let (output, _remote_output) = duplex(64);
    let mut streams = Some((input, output));
    let mut factory = TransportFactory::Custom(Box::new(move || {
        let (input, output) = streams.take().expect("one custom transport");
        Ok(Transport::new(input, output))
    }));
    let (process, streams) = ServerProcess::open(&mut factory, |_| {}).expect("streams open");
    drop(streams);
    time::timeout(
        super::LSP_FORCED_CLEANUP_DEADLINE + Duration::from_secs(1),
        process.close(super::ServerCloseIntent::Immediate),
    )
    .await
    .expect("stream close stays bounded");
}

/// Returns the mode of one `textDocumentSync` capability value.
fn mode(capability: &Value) -> SynchronizationMode {
    synchronization_mode(capability.pointer("/capabilities/textDocumentSync"))
}

/// Starts one shell child and returns its process with a report queue.
///
/// The queue is unbounded, so no test drops a report and the recorder never
/// waits. The session half stays unused, exactly as a cancelled attempt
/// leaves it.
fn shell(script: &str, args: &[&str]) -> (ServerProcess, mpsc::UnboundedReceiver<ServerReport>) {
    let mut arguments = vec![OsString::from("-c"), OsString::from(script)];
    arguments.extend(args.iter().map(OsString::from));
    let mut factory = TransportFactory::process(
        ServerLaunchRequest::new(
            OsString::from(SHELL),
            arguments,
            WorkspaceRoot::new(PathBuf::from("/")).expect("the process root is valid"),
        )
        .expect("the process request is valid"),
    );
    let (sender, reports) = mpsc::unbounded_channel();
    let (process, streams) = ServerProcess::open(&mut factory, move |report| {
        let _ = sender.send(report);
    })
    .expect("every supported platform holds the shell");
    drop(streams);
    (process, reports)
}

/// Collects every report until the standard error of the child ends.
async fn recorded(reports: &mut mpsc::UnboundedReceiver<ServerReport>) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut bounds = 0;
    while let Some(report) = time::timeout(TEST_DEADLINE, reports.recv())
        .await
        .expect("the child ends before the test deadline")
    {
        match report {
            ServerReport::Output(text) => lines.push(text),
            ServerReport::OutputBound => bounds += 1,
            ServerReport::Started | ServerReport::CleanupFailed(_) => {}
        }
    }
    (lines, bounds)
}

/// Reports whether the system still holds one process identifier.
///
/// The probe runs the shell built-in, because no supported platform
/// guarantees a `kill` executable at a fixed path.
async fn is_running(pid: u32) -> bool {
    Command::new(SHELL)
        .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
        .status()
        .await
        .expect("the probe runs")
        .success()
}

#[tokio::test]
async fn server_process_opens_a_fresh_custom_transport_for_each_attempt() {
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let created = attempts.clone();
    let mut factory = TransportFactory::Custom(Box::new(move || {
        created.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (client, mut server) = duplex(256);
        let (client_output, client_input) = tokio::io::split(client);
        tokio::spawn(async move {
            let mut byte = [0_u8; 1];
            server
                .read_exact(&mut byte)
                .await
                .expect("read request byte");
            server.write_all(&byte).await.expect("write response byte");
        });
        Ok(Transport::new(client_input, client_output))
    }));

    for expected in *b"ab" {
        let (process, mut streams) = ServerProcess::open(&mut factory, |_| {})
            .expect("public owner accepts custom transport");
        streams
            .writer
            .notify("test/attempt", json!({ "byte": expected }))
            .await
            .expect("write request frame");
        drop(streams);
        process.close(super::ServerCloseIntent::Immediate).await;
    }
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
}

#[test]
fn server_process_preserves_a_custom_transport_error_source() {
    let mut factory = TransportFactory::Custom(Box::new(|| {
        Err(super::LspError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "fixture refused connection",
        )))
    }));

    let error = ServerProcess::open(&mut factory, |_| {})
        .err()
        .expect("the custom transport fails");
    assert!(matches!(error, super::LspError::Io(_)));
    let source = error
        .source()
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .expect("the typed input/output cause remains available");
    assert_eq!(source.kind(), std::io::ErrorKind::ConnectionRefused);
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
fn one_provider_identifier_above_its_bound_refuses_the_capability() {
    // Every pull repeats the identifier, so an unbounded one would enter
    // every later request of the session.
    let identifier = "x".repeat(LSP_RESULT_ID_BYTES_MAX + 1);
    assert!(diagnostics_model(Some(&json!({ "identifier": identifier }))).is_err());
}

#[test]
fn the_recorder_holds_both_of_its_bounds() {
    let lines: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let bounds = RefCell::new(0_usize);
    let long = "x".repeat(LSP_STDERR_LINE_BYTES_MAX * 2);
    let writes = LSP_STDERR_BYTES_MAX / LSP_STDERR_LINE_BYTES_MAX * 4;
    {
        let mut recorder = ErrorRecorder::new(|report| match report {
            ServerReport::Output(text) => lines.borrow_mut().push(text),
            ServerReport::OutputBound => *bounds.borrow_mut() += 1,
            ServerReport::Started | ServerReport::CleanupFailed(_) => {}
        });
        for _ in 0..writes {
            recorder.take(long.as_bytes());
            recorder.take(b"\n");
        }
        recorder.finish();
    }

    let lines = lines.into_inner();
    assert!(
        lines
            .iter()
            .all(|line| line.len() <= LSP_STDERR_LINE_BYTES_MAX),
        "every recorded line stays inside the line bound"
    );
    assert_eq!(
        bounds.into_inner(),
        1,
        "one attempt reports its byte bound exactly once"
    );
    // The recorder measures one line break for each line, and it stops at
    // the first line that reaches the bound.
    let recorded: usize = lines.iter().map(|line| line.len() + 1).sum();
    assert!(
        recorded <= LSP_STDERR_BYTES_MAX + LSP_STDERR_LINE_BYTES_MAX + 1,
        "the recorder stops within one line of its byte bound, not at {recorded}"
    );
}

#[tokio::test]
async fn records_the_standard_error_of_a_server_that_exits_at_once() {
    // The child repeats the failure that this capture exists for: the
    // program names its cause on the standard error and exits at once.
    let (process, mut reports) = shell("printf '%s\\n' \"$1\" >&2; exit 1", &["shim", SHIM_LINE]);

    process
        .close(super::ServerCloseIntent::Graceful {
            deadline: time::Instant::now() + TEST_DEADLINE,
        })
        .await;
    let (lines, bounds) = recorded(&mut reports).await;
    assert!(
        lines.iter().any(|line| line == SHIM_LINE),
        "the recorded output names the cause, not {lines:?}"
    );
    assert_eq!(bounds, 0, "a short output passes no bound");
}

#[tokio::test]
async fn drains_a_server_that_writes_more_than_its_bound() {
    // Every line passes the line bound, and the child writes several times
    // the recording bound. A reader that stopped draining would fill the
    // pipe, and the child would never exit, so this test would never reach
    // the end of the stream.
    let line = "x".repeat(LSP_STDERR_LINE_BYTES_MAX * 2);
    let writes = LSP_STDERR_BYTES_MAX / LSP_STDERR_LINE_BYTES_MAX * 4;
    let script = format!(
        "count=0; while [ $count -lt {writes} ]; \
             do printf '%s\\n' \"$1\" >&2; count=$((count + 1)); done; exit 1"
    );
    let (process, mut reports) = shell(&script, &["flood", &line]);

    process
        .close(super::ServerCloseIntent::Graceful {
            deadline: time::Instant::now() + TEST_DEADLINE,
        })
        .await;
    let (lines, bounds) = recorded(&mut reports).await;
    assert_eq!(bounds, 1, "the attempt reports the bound that it passed");
    assert!(
        lines
            .iter()
            .all(|line| line.len() <= LSP_STDERR_LINE_BYTES_MAX),
        "every recorded line stays inside the line bound"
    );
}

#[tokio::test]
async fn dropping_the_process_ends_the_child() {
    // A cancelled session drops its process without closing it. The child
    // must still end, because an untracked server would outlive its editor.
    let marker = std::env::temp_dir().join(format!("kvim-lsp-drop-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let script = format!("printf '%s' $$ > '{}'; exec sleep 600", marker.display());
    let mut factory = TransportFactory::process(
        ServerLaunchRequest::new(
            OsString::from(SHELL),
            vec![OsString::from("-c"), OsString::from(script)],
            WorkspaceRoot::new(PathBuf::from("/")).expect("the process root is valid"),
        )
        .expect("the process request is valid"),
    );
    let (process, streams) = ServerProcess::open(&mut factory, |_| {})
        .expect("every supported platform holds the shell");
    drop(streams);
    let deadline = time::Instant::now() + TEST_DEADLINE;
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(&marker)
            && let Ok(pid) = text.parse::<u32>()
        {
            break pid;
        }
        assert!(
            time::Instant::now() < deadline,
            "the child records its identifier"
        );
        time::sleep(PROBE_INTERVAL).await;
    };
    assert!(is_running(pid).await, "the child runs before the drop");

    drop(process);

    // The runtime kills the child and reaps it, so the identifier leaves
    // the process table. A bounded probe keeps a broken build from hanging.
    let deadline = time::Instant::now() + TEST_DEADLINE;
    while is_running(pid).await {
        assert!(
            time::Instant::now() < deadline,
            "the dropped process left child {pid} running"
        );
        time::sleep(PROBE_INTERVAL).await;
    }
    let _ = std::fs::remove_file(marker);
}
