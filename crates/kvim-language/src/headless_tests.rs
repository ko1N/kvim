use super::*;

use std::fs;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, duplex};
use tokio::sync::oneshot;
use tokio::time;
use tokio_util::sync::CancellationToken;

use kvim_lsp::{
    ChangedFile, DiagnosticsOutcome, DocumentRevision, LSP_MESSAGE_BYTES_MAX, LaunchedServer,
    ServerEvent, ServerOutcome, ServerProcessHandle, ServerReport, ServerTerminate, ServerWait,
    WaitPolicy, read_frame,
};

use kvim_settings::CheckDepth;

use crate::{LanguageServerDeclaration, ServerFormatting};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

fn options(_: LanguageSettings) -> Value {
    json!({"enabled": true})
}
fn settings(_: LanguageSettings) -> Value {
    json!({"mode": "strict"})
}

const MARKERS: &[&str] = &["marker.file", "marker.dir"];
const SERVERS: &[LanguageServerDeclaration] = &[
    LanguageServerDeclaration {
        id: "gated",
        program: "gated-server",
        args: &["--stdio"],
        language_id: "demo",
        formatting: ServerFormatting::Disabled,
        diagnostics_completion: CompletionPolicy::Unsupported,
        root_markers: MARKERS,
        initialization_options: options,
        workspace_settings: Some(settings),
    },
    LanguageServerDeclaration {
        id: "always",
        program: "always-server",
        args: &["--stdio", "--verbose"],
        language_id: "demo",
        formatting: ServerFormatting::Disabled,
        diagnostics_completion: CompletionPolicy::Pull,
        root_markers: &[],
        initialization_options: options,
        workspace_settings: None,
    },
];
const PROFILE: LanguageServiceProfile =
    LanguageServiceProfile::new("demo", "1", &["demo"], &["demo"], &["Demofile"], SERVERS);
const PROFILES: &[LanguageServiceProfile] = &[PROFILE];

const VERSIONED_SERVERS: &[LanguageServerDeclaration] = &[LanguageServerDeclaration {
    id: "push",
    program: "push-server",
    args: &["--stdio"],
    language_id: "demo",
    formatting: ServerFormatting::Disabled,
    diagnostics_completion: CompletionPolicy::VersionedPush,
    root_markers: &[],
    initialization_options: options,
    workspace_settings: None,
}];
const VERSIONED_PROFILE: LanguageServiceProfile =
    LanguageServiceProfile::new("demo", "1", &["demo"], &["demo"], &[], VERSIONED_SERVERS);

const STDERR_SENTINEL: &str = "KVIM_STDERR_SENTINEL_7";

fn root() -> PathBuf {
    let suffix = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("kvim-headless-{}-{suffix}", std::process::id()));
    fs::create_dir(&root).expect("create test root");
    root.canonicalize().expect("canonical test root")
}

fn project(root: PathBuf) -> HeadlessDiagnosticsProject {
    HeadlessDiagnosticsProject::new(
        DiagnosticsRegistry::new(PROFILES).expect("valid registry"),
        root,
        LanguageSettings::default(),
        ProjectId::FIRST,
    )
    .expect("realize project")
}

#[derive(Clone, Copy)]
enum FixtureBehavior {
    Pull,
    RustPull,
    VersionedPush,
    Silent,
    CrashWithStderr,
}

struct HandshakeGate {
    waiting: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

type RecordedRequest = (OsString, Vec<OsString>, PathBuf);

struct FixtureLauncher {
    launches: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    cleanup: Arc<AtomicUsize>,
    behavior: FixtureBehavior,
    handshake_gate: Option<HandshakeGate>,
}

struct FixtureLifecycle {
    exited: Option<oneshot::Receiver<()>>,
    cleanup: Arc<AtomicUsize>,
}

impl Drop for FixtureLifecycle {
    fn drop(&mut self) {
        self.cleanup.fetch_add(1, Ordering::Relaxed);
    }
}

impl ServerProcessHandle for FixtureLifecycle {
    fn wait(&mut self) -> ServerWait {
        let exited = self.exited.take().expect("wait is taken once");
        Box::pin(async move {
            exited.await.map_err(|_| {
                kvim_lsp::ServerWaitError(io::Error::other("fixture server stopped"))
            })?;
            Ok(ExitStatus::from_raw(0))
        })
    }

    fn terminate(&mut self) -> ServerTerminate<'_> {
        Box::pin(async { Ok(()) })
    }
}

impl ServerLauncher for FixtureLauncher {
    fn launch(
        &mut self,
        request: &ServerLaunchRequest,
    ) -> Result<LaunchedServer, ServerLaunchError> {
        self.launches.fetch_add(1, Ordering::Relaxed);
        self.requests.lock().unwrap().push((
            request.program().to_os_string(),
            request
                .arguments()
                .iter()
                .map(|value| value.to_os_string())
                .collect(),
            request.root().path().to_path_buf(),
        ));
        let (session_input, server_output) = duplex(64 * 1024);
        let (server_input, session_output) = duplex(64 * 1024);
        let (errors, errors_peer) = duplex(64 * 1024);
        let (exit, exited) = oneshot::channel();
        tokio::spawn(serve_fixture(
            server_output,
            server_input,
            errors_peer,
            self.behavior,
            self.handshake_gate.take(),
            exit,
        ));
        Ok(LaunchedServer::new(
            session_input,
            session_output,
            errors,
            FixtureLifecycle {
                exited: Some(exited),
                cleanup: Arc::clone(&self.cleanup),
            },
        ))
    }
}

async fn serve_fixture(
    mut input: tokio::io::DuplexStream,
    mut output: tokio::io::DuplexStream,
    mut errors: tokio::io::DuplexStream,
    behavior: FixtureBehavior,
    mut handshake_gate: Option<HandshakeGate>,
    exited: oneshot::Sender<()>,
) {
    if matches!(behavior, FixtureBehavior::CrashWithStderr) {
        errors
            .write_all(format!("{STDERR_SENTINEL}\n").as_bytes())
            .await
            .unwrap();
        errors.shutdown().await.unwrap();
        let _ = exited.send(());
        return;
    }
    let mut read_bytes = 0;
    let mut document_text = String::new();
    loop {
        let Ok(body) = read_frame(&mut input, &mut read_bytes, LSP_MESSAGE_BYTES_MAX * 8).await
        else {
            break;
        };
        let message: Value = serde_json::from_slice(&body).unwrap();
        match message["method"].as_str().unwrap_or_default() {
            "initialize" => {
                if let Some(gate) = handshake_gate.take() {
                    let _ = gate.waiting.send(());
                    let _ = gate.release.await;
                }
                let capabilities = match behavior {
                    FixtureBehavior::Pull | FixtureBehavior::RustPull | FixtureBehavior::Silent => {
                        json!({
                            "textDocumentSync": 1,
                            "diagnosticProvider": { "identifier": "fixture" }
                        })
                    }
                    FixtureBehavior::VersionedPush => json!({"textDocumentSync": 1}),
                    FixtureBehavior::CrashWithStderr => unreachable!(),
                };
                send_response(
                    &mut output,
                    &message["id"],
                    json!({"capabilities": capabilities}),
                )
                .await
            }
            "textDocument/didOpen" => {
                document_text = message["params"]["textDocument"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                if matches!(behavior, FixtureBehavior::VersionedPush) {
                    let document = &message["params"]["textDocument"];
                    let version = document["version"].as_i64().unwrap();
                    send_notification(
                        &mut output,
                        json!({
                            "jsonrpc": "2.0",
                            "method": "textDocument/publishDiagnostics",
                            "params": {
                                "uri": document["uri"],
                                "version": version - 1,
                                "diagnostics": [{
                                    "range": {
                                        "start": {"line": 0, "character": 0},
                                        "end": {"line": 0, "character": 1}
                                    },
                                    "message": "stale fixture diagnostic"
                                }]
                            }
                        }),
                    )
                    .await;
                    send_notification(
                        &mut output,
                        json!({
                            "jsonrpc": "2.0",
                            "method": "textDocument/publishDiagnostics",
                            "params": {
                                "uri": document["uri"],
                                "version": version,
                                "diagnostics": [{
                                    "range": {
                                        "start": {"line": 0, "character": 0},
                                        "end": {"line": 0, "character": 1}
                                    },
                                    "message": "current fixture diagnostic"
                                }]
                            }
                        }),
                    )
                    .await;
                }
            }
            "textDocument/didChange" => {
                document_text = message["params"]["contentChanges"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
            }
            "textDocument/diagnostic"
                if matches!(behavior, FixtureBehavior::Pull | FixtureBehavior::RustPull) =>
            {
                let items = if matches!(behavior, FixtureBehavior::RustPull)
                    && document_text.contains("broken")
                {
                    json!([{
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 2}
                        },
                        "severity": 1,
                        "message": "matching Rust fixture diagnostic"
                    }])
                } else {
                    json!([])
                };
                send_response(
                    &mut output,
                    &message["id"],
                    json!({"kind": "full", "items": items}),
                )
                .await
            }
            "shutdown" => send_response(&mut output, &message["id"], Value::Null).await,
            "exit" => break,
            _ => {}
        }
    }
    let _ = exited.send(());
}

async fn send_notification(output: &mut tokio::io::DuplexStream, value: Value) {
    let body = serde_json::to_vec(&value).unwrap();
    output
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .unwrap();
    output.write_all(&body).await.unwrap();
    output.flush().await.unwrap();
}

async fn send_response(output: &mut tokio::io::DuplexStream, id: &Value, result: Value) {
    let body = serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": id, "result": result})).unwrap();
    output
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .unwrap();
    output.write_all(&body).await.unwrap();
    output.flush().await.unwrap();
}

async fn close_project(
    opened: OpenedHeadlessDiagnosticsProject,
    task: tokio::task::JoinHandle<()>,
    cleanup: &AtomicUsize,
    manager: &ProjectManager,
) {
    let (_, handle) = opened.into_parts();
    handle.close().await;
    time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the fixture driver ends")
        .expect("the fixture driver task succeeds");
    assert_eq!(cleanup.load(Ordering::Relaxed), 1);
    assert_eq!(manager.projects(), 0);
}

#[tokio::test]
async fn one_open_project_serves_two_requests_through_one_warm_launch() {
    let root = root();
    let launches = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let cleanup = Arc::new(AtomicUsize::new(0));
    let project = HeadlessDiagnosticsProject::with_launchers(
        DiagnosticsRegistry::new(PROFILES).unwrap(),
        root.clone(),
        LanguageSettings::default(),
        ProjectId::FIRST,
        {
            let launches = Arc::clone(&launches);
            let requests = Arc::clone(&requests);
            let cleanup = Arc::clone(&cleanup);
            move |_| {
                Box::new(FixtureLauncher {
                    launches: Arc::clone(&launches),
                    requests: Arc::clone(&requests),
                    cleanup: Arc::clone(&cleanup),
                    behavior: FixtureBehavior::Pull,
                    handshake_gate: None,
                })
            }
        },
    )
    .unwrap();
    let manager = ProjectManager::new(kvim_lsp::ManagerLimits::default());
    let path = WorktreeRelativePath::new("main.demo").unwrap();
    let (opened, driver) = project.open(&manager, &path).unwrap();
    let language = opened.declarations()[1].language_id().clone();
    let task = tokio::spawn(driver.run());

    for revision in [DocumentRevision::FIRST, DocumentRevision::new(1)] {
        let outcome = opened
            .hub()
            .changed_file(
                ChangedFile::new(
                    path.clone(),
                    "demo\n".to_owned(),
                    revision,
                    language.clone(),
                )
                .wait(WaitPolicy::Until(Duration::from_secs(5))),
            )
            .await
            .unwrap();
        let DiagnosticsOutcome::Ready(report) = outcome else {
            panic!("warm request did not return ready diagnostics");
        };
        assert_eq!(report.revision(), revision);
    }
    assert_eq!(launches.load(Ordering::Relaxed), 1);
    assert_eq!(
        requests.lock().unwrap().as_slice(),
        &[(
            OsString::from("always-server"),
            vec![OsString::from("--stdio"), OsString::from("--verbose")],
            root,
        )]
    );

    close_project(opened, task, &cleanup, &manager).await;
}

#[tokio::test]
async fn first_release_rust_profile_waits_through_cold_start_and_reports_clean() {
    let root = root();
    let launches = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let cleanup = Arc::new(AtomicUsize::new(0));
    let (handshake_waiting, initialize_seen) = oneshot::channel();
    let (release_handshake, handshake_release) = oneshot::channel();
    let handshake_gate = Arc::new(Mutex::new(Some(HandshakeGate {
        waiting: handshake_waiting,
        release: handshake_release,
    })));
    let project = HeadlessDiagnosticsProject::with_launchers(
        DiagnosticsRegistry::first_release(),
        root,
        LanguageSettings::default(),
        ProjectId::FIRST,
        {
            let launches = Arc::clone(&launches);
            let requests = Arc::clone(&requests);
            let cleanup = Arc::clone(&cleanup);
            let handshake_gate = Arc::clone(&handshake_gate);
            move |_| {
                Box::new(FixtureLauncher {
                    launches: Arc::clone(&launches),
                    requests: Arc::clone(&requests),
                    cleanup: Arc::clone(&cleanup),
                    behavior: FixtureBehavior::RustPull,
                    handshake_gate: handshake_gate.lock().unwrap().take(),
                })
            }
        },
    )
    .unwrap();
    let manager = ProjectManager::new(kvim_lsp::ManagerLimits::default());
    let path = WorktreeRelativePath::new("src/main.rs").unwrap();
    let (opened, driver) = project.open(&manager, &path).unwrap();
    let declaration = &opened.declarations()[0];
    assert_eq!(declaration.id().server(), "rust_analyzer");
    assert_eq!(declaration.completion(), CompletionPolicy::Pull);
    let language = declaration.language_id().clone();
    let task = tokio::spawn(driver.run());

    {
        let cold_request = opened.hub().changed_file(
            ChangedFile::new(
                path.clone(),
                "broken Rust source\n".to_owned(),
                DocumentRevision::new(9),
                language.clone(),
            )
            .wait(WaitPolicy::Until(Duration::from_secs(5))),
        );
        tokio::pin!(cold_request);
        tokio::select! {
            biased;
            outcome = &mut cold_request => {
                panic!("the cold request completed before initialize was released: {outcome:?}");
            }
            seen = initialize_seen => {
                seen.expect("the fixture observes initialize");
            }
        }
        assert_eq!(launches.load(Ordering::Relaxed), 1);
        let starting = opened
            .hub()
            .changed_file(
                ChangedFile::new(
                    path.clone(),
                    "broken Rust source\n".to_owned(),
                    DocumentRevision::new(9),
                    language.clone(),
                )
                .wait(WaitPolicy::Immediate),
            )
            .await
            .unwrap();
        assert!(matches!(starting, DiagnosticsOutcome::Starting));
        release_handshake
            .send(())
            .expect("the cold request still waits at initialize");

        let outcome = cold_request.as_mut().await.unwrap();
        let DiagnosticsOutcome::Ready(report) = outcome else {
            panic!("the Rust profile did not complete through pull diagnostics");
        };
        assert_eq!(report.revision(), DocumentRevision::new(9));
        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].diagnostic.message,
            "matching Rust fixture diagnostic"
        );
    }

    let clean = opened
        .hub()
        .changed_file(
            ChangedFile::new(
                path,
                "fn main() {}\n".to_owned(),
                DocumentRevision::new(10),
                language,
            )
            .wait(WaitPolicy::Until(Duration::from_secs(5))),
        )
        .await
        .unwrap();
    let DiagnosticsOutcome::Ready(clean) = clean else {
        panic!("matching empty Rust diagnostics did not complete the report");
    };
    assert_eq!(clean.revision(), DocumentRevision::new(10));
    assert!(clean.diagnostics().is_empty());
    assert!(matches!(
        clean.servers()[0].outcome,
        ServerOutcome::Ready {
            diagnostics: 0,
            truncation: kvim_lsp::Truncation::Complete,
        }
    ));

    close_project(opened, task, &cleanup, &manager).await;
}

#[tokio::test]
async fn versioned_push_ignores_stale_publications_and_completes_exact_revision() {
    let root = root();
    let launches = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let cleanup = Arc::new(AtomicUsize::new(0));
    let project = HeadlessDiagnosticsProject::with_launchers(
        DiagnosticsRegistry::new(&[VERSIONED_PROFILE]).unwrap(),
        root,
        LanguageSettings::default(),
        ProjectId::FIRST,
        {
            let launches = Arc::clone(&launches);
            let requests = Arc::clone(&requests);
            let cleanup = Arc::clone(&cleanup);
            move |_| {
                Box::new(FixtureLauncher {
                    launches: Arc::clone(&launches),
                    requests: Arc::clone(&requests),
                    cleanup: Arc::clone(&cleanup),
                    behavior: FixtureBehavior::VersionedPush,
                    handshake_gate: None,
                })
            }
        },
    )
    .unwrap();
    let manager = ProjectManager::new(kvim_lsp::ManagerLimits::default());
    let path = WorktreeRelativePath::new("main.demo").unwrap();
    let (opened, driver) = project.open(&manager, &path).unwrap();
    let language = opened.declarations()[0].language_id().clone();
    let task = tokio::spawn(driver.run());

    let outcome = time::timeout(
        Duration::from_secs(5),
        opened.hub().changed_file(
            ChangedFile::new(
                path,
                "demo\n".to_owned(),
                DocumentRevision::new(7),
                language,
            )
            .wait(WaitPolicy::Until(Duration::from_secs(2))),
        ),
    )
    .await
    .expect("the exact publication completes")
    .unwrap();
    let DiagnosticsOutcome::Ready(report) = outcome else {
        panic!("versioned push did not return ready diagnostics");
    };
    assert_eq!(report.revision(), DocumentRevision::new(7));
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].diagnostic.message,
        "current fixture diagnostic"
    );
    assert_eq!(launches.load(Ordering::Relaxed), 1);

    close_project(opened, task, &cleanup, &manager).await;
}

#[tokio::test]
async fn launcher_backed_timeout_and_cancellation_cleanup_the_warm_lifecycle() {
    for cancelled in [false, true] {
        let root = root();
        let launches = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let cleanup = Arc::new(AtomicUsize::new(0));
        let project = HeadlessDiagnosticsProject::with_launchers(
            DiagnosticsRegistry::new(PROFILES).unwrap(),
            root,
            LanguageSettings::default(),
            ProjectId::FIRST,
            {
                let launches = Arc::clone(&launches);
                let requests = Arc::clone(&requests);
                let cleanup = Arc::clone(&cleanup);
                move |_| {
                    Box::new(FixtureLauncher {
                        launches: Arc::clone(&launches),
                        requests: Arc::clone(&requests),
                        cleanup: Arc::clone(&cleanup),
                        behavior: FixtureBehavior::Silent,
                        handshake_gate: None,
                    })
                }
            },
        )
        .unwrap();
        let manager = ProjectManager::new(kvim_lsp::ManagerLimits::default());
        let path = WorktreeRelativePath::new("main.demo").unwrap();
        let (opened, driver) = project.open(&manager, &path).unwrap();
        let language = opened.declarations()[1].language_id().clone();
        let task = tokio::spawn(driver.run());
        let cancellation = CancellationToken::new();
        let mut request =
            ChangedFile::new(path, "demo\n".to_owned(), DocumentRevision::FIRST, language)
                .wait(WaitPolicy::Until(Duration::from_millis(100)));
        if cancelled {
            let owner = cancellation.clone();
            let launched = Arc::clone(&launches);
            tokio::spawn(async move {
                let deadline = time::Instant::now() + Duration::from_secs(2);
                while launched.load(Ordering::Relaxed) == 0 {
                    assert!(
                        time::Instant::now() < deadline,
                        "the fixture launch starts before cancellation"
                    );
                    tokio::task::yield_now().await;
                }
                owner.cancel();
            });
            request = request.cancellation(cancellation);
        }

        let outcome = time::timeout(Duration::from_secs(5), opened.hub().changed_file(request))
            .await
            .expect("the bounded request ends")
            .unwrap();
        assert!(
            matches!(
                (cancelled, &outcome),
                (false, DiagnosticsOutcome::TimedOut) | (true, DiagnosticsOutcome::Cancelled)
            ),
            "unexpected bounded outcome: {outcome:?}"
        );
        assert_eq!(launches.load(Ordering::Relaxed), 1);

        close_project(opened, task, &cleanup, &manager).await;
    }
}

#[tokio::test]
async fn unavailable_launcher_reaches_the_public_typed_outcome_without_source_text() {
    let calls = Arc::new(AtomicUsize::new(0));
    let project = HeadlessDiagnosticsProject::with_launchers(
        DiagnosticsRegistry::new(PROFILES).unwrap(),
        root(),
        LanguageSettings::default(),
        ProjectId::FIRST,
        {
            let calls = Arc::clone(&calls);
            move |_| Box::new(CountingLauncher(Arc::clone(&calls)))
        },
    )
    .unwrap();
    let manager = ProjectManager::new(kvim_lsp::ManagerLimits::default());
    let path = WorktreeRelativePath::new("main.demo").unwrap();
    let (opened, driver) = project.open(&manager, &path).unwrap();
    let language = opened.declarations()[1].language_id().clone();
    let task = tokio::spawn(driver.run());

    let outcome = time::timeout(
        Duration::from_secs(5),
        opened.hub().changed_file(ChangedFile::new(
            path,
            "demo\n".to_owned(),
            DocumentRevision::FIRST,
            language,
        )),
    )
    .await
    .expect("the unavailable result is terminal")
    .unwrap();
    let DiagnosticsOutcome::Ready(report) = outcome else {
        panic!("unavailable launcher did not complete the report");
    };
    assert!(matches!(
        report.servers()[0].outcome,
        ServerOutcome::Unavailable
    ));
    assert!(!format!("{report:?}").contains("fixture launcher"));
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    let (_, handle) = opened.into_parts();
    handle.close().await;
    time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manager.projects(), 0);
}

#[tokio::test]
async fn stderr_is_operator_report_only_across_bounded_restarts() {
    let launches = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let cleanup = Arc::new(AtomicUsize::new(0));
    let project = HeadlessDiagnosticsProject::with_launchers(
        DiagnosticsRegistry::new(PROFILES).unwrap(),
        root(),
        LanguageSettings::default(),
        ProjectId::FIRST,
        {
            let launches = Arc::clone(&launches);
            let requests = Arc::clone(&requests);
            let cleanup = Arc::clone(&cleanup);
            move |_| {
                Box::new(FixtureLauncher {
                    launches: Arc::clone(&launches),
                    requests: Arc::clone(&requests),
                    cleanup: Arc::clone(&cleanup),
                    behavior: FixtureBehavior::CrashWithStderr,
                    handshake_gate: None,
                })
            }
        },
    )
    .unwrap();
    let manager = ProjectManager::new(kvim_lsp::ManagerLimits::default());
    let path = WorktreeRelativePath::new("main.demo").unwrap();
    let (opened, driver) = project.open(&manager, &path).unwrap();
    let language = opened.declarations()[1].language_id().clone();
    let task = tokio::spawn(driver.run());

    let outcome = time::timeout(
        Duration::from_secs(5),
        opened.hub().changed_file(ChangedFile::new(
            path,
            "demo\n".to_owned(),
            DocumentRevision::FIRST,
            language,
        )),
    )
    .await
    .expect("bounded restarts reach a terminal diagnostics outcome")
    .unwrap();
    assert!(!format!("{outcome:?}").contains(STDERR_SENTINEL));

    let (_, mut handle) = opened.into_parts();
    let mut saw_stderr = false;
    while let Some(event) = handle.try_recv() {
        if let ServerEvent::Reported(ServerReport::Output(line)) = event.event {
            saw_stderr |= line == STDERR_SENTINEL;
        }
    }
    assert!(
        saw_stderr,
        "the bounded operator report sink receives stderr"
    );
    assert_eq!(
        launches.load(Ordering::Relaxed),
        kvim_lsp::LSP_RESTARTS_MAX + 1
    );
    handle.close().await;
    time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        cleanup.load(Ordering::Relaxed),
        kvim_lsp::LSP_RESTARTS_MAX + 1
    );
    assert_eq!(manager.projects(), 0);
}

struct CountingLauncher(Arc<AtomicUsize>);

impl ServerLauncher for CountingLauncher {
    fn launch(
        &mut self,
        _: &ServerLaunchRequest,
    ) -> Result<kvim_lsp::LaunchedServer, ServerLaunchError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(ServerLaunchError::Unavailable(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "fixture launcher",
        )))
    }
}

#[test]
fn launcher_factory_is_delayed_and_selected_by_profile() {
    let factories = Arc::new(Mutex::new(Vec::new()));
    let project = HeadlessDiagnosticsProject::with_launchers(
        DiagnosticsRegistry::first_release(),
        root(),
        LanguageSettings::default(),
        ProjectId::FIRST,
        {
            let factories = Arc::clone(&factories);
            move |declaration| {
                factories.lock().unwrap().push(declaration.id());
                Box::new(CountingLauncher(Arc::new(AtomicUsize::new(0))))
            }
        },
    )
    .unwrap();
    assert!(factories.lock().unwrap().is_empty());

    let manager = ProjectManager::new(kvim_lsp::ManagerLimits::default());
    let path = WorktreeRelativePath::new("src/main.rs").unwrap();
    let (opened, driver) = project.open(&manager, &path).unwrap();
    assert_eq!(
        factories.lock().unwrap().as_slice(),
        &[LanguageServerId::new("rust", 0, "rust_analyzer")]
    );
    drop(driver);
    drop(opened);
}

#[test]
fn gated_selected_declaration_never_invokes_launcher_factory() {
    const GATED_SERVERS: &[LanguageServerDeclaration] = &[SERVERS[0]];
    const GATED_PROFILE: LanguageServiceProfile =
        LanguageServiceProfile::new("demo", "1", &["demo"], &["demo"], &[], GATED_SERVERS);
    let factories = Arc::new(AtomicUsize::new(0));
    let project = HeadlessDiagnosticsProject::with_launchers(
        DiagnosticsRegistry::new(&[GATED_PROFILE]).unwrap(),
        root(),
        LanguageSettings::default(),
        ProjectId::FIRST,
        {
            let factories = Arc::clone(&factories);
            move |_| {
                factories.fetch_add(1, Ordering::Relaxed);
                Box::new(CountingLauncher(Arc::new(AtomicUsize::new(0))))
            }
        },
    )
    .unwrap();
    let manager = ProjectManager::new(kvim_lsp::ManagerLimits::default());
    let (opened, driver) = project
        .open(&manager, &WorktreeRelativePath::new("main.demo").unwrap())
        .unwrap();
    assert_eq!(factories.load(Ordering::Relaxed), 0);
    drop(driver);
    drop(opened);
}

#[test]
fn first_release_typescript_preserves_gated_source_order() {
    let project = HeadlessDiagnosticsProject::new(
        DiagnosticsRegistry::first_release(),
        root(),
        LanguageSettings::default(),
        ProjectId::FIRST,
    )
    .unwrap();
    let selection = project
        .select(&WorktreeRelativePath::new("src/main.ts").unwrap())
        .unwrap();
    assert_eq!(selection.declarations().len(), 2);
    assert_eq!(selection.declarations()[0].id().server(), "eslint");
    assert_eq!(selection.declarations()[0].id().order(), 0);
    assert_eq!(selection.declarations()[0].neutral_id(), None);
    assert_eq!(selection.declarations()[1].neutral_id(), None);
    assert_eq!(selection.declarations()[1].id().server(), "ts_ls");
    assert_eq!(selection.declarations()[1].id().order(), 1);
    assert_eq!(selection.declarations()[1].source(), "ts_ls");
}

#[test]
fn first_release_typescript_activates_both_servers_with_marker() {
    let root = root();
    fs::write(root.join("eslint.config.js"), "").unwrap();
    let project = HeadlessDiagnosticsProject::new(
        DiagnosticsRegistry::first_release(),
        root,
        LanguageSettings::default(),
        ProjectId::FIRST,
    )
    .unwrap();
    let manager = ProjectManager::new(kvim_lsp::ManagerLimits::default());
    let path = WorktreeRelativePath::new("main.ts").unwrap();
    let (opened, driver) = project.open(&manager, &path).unwrap();
    assert_eq!(
        opened.declarations()[0].neutral_id(),
        Some(ServerId::new(0))
    );
    assert_eq!(
        opened.declarations()[1].neutral_id(),
        Some(ServerId::new(1))
    );
    drop(driver);
    drop(opened);
}

#[test]
fn first_release_realizes_rust_check_depths_and_eslint_settings() {
    for (depth, command) in [
        (CheckDepth::Compile, "check"),
        (CheckDepth::Lints, "clippy"),
    ] {
        let project = HeadlessDiagnosticsProject::new(
            DiagnosticsRegistry::first_release(),
            root(),
            LanguageSettings {
                check_depth: depth,
                diagnostics_enabled: true,
            },
            ProjectId::FIRST,
        )
        .unwrap();
        let rust = project
            .select(&WorktreeRelativePath::new("src/lib.rs").unwrap())
            .unwrap();
        assert_eq!(
            rust.declarations()[0].initialization_options(),
            &json!({"check": {"command": command}})
        );
        assert_eq!(rust.declarations()[0].completion(), CompletionPolicy::Pull);
        let typescript = project
            .select(&WorktreeRelativePath::new("src/main.ts").unwrap())
            .unwrap();
        assert_eq!(
            typescript.declarations()[0].workspace_settings(),
            Some(&json!({
                "validate": "on",
                "nodePath": Value::Null,
                "problems": {"shortenToSingleLine": false},
                "rulesCustomizations": [],
            }))
        );
    }
}

#[test]
fn completion_projection_is_exact() {
    let project = project(root());
    assert_eq!(
        project.declarations()[0].completion(),
        CompletionPolicy::Unsupported
    );
    assert_eq!(
        project.declarations()[1].completion(),
        CompletionPolicy::Pull
    );
}

#[test]
fn validates_absolute_relative_dot_and_dotdot_roots() {
    assert!(project(root()).root().path().is_absolute());
    for rejected in ["relative", ".", ".."] {
        assert!(matches!(
            HeadlessDiagnosticsProject::new(
                DiagnosticsRegistry::new(PROFILES).unwrap(),
                PathBuf::from(rejected),
                LanguageSettings::default(),
                ProjectId::FIRST,
            ),
            Err(HeadlessDiagnosticsError::Root(LspError::PathEscape))
        ));
    }
}

#[test]
fn selects_extension_complete_name_and_reports_unsupported() {
    let project = project(root());
    for path in ["src/main.demo", "Demofile"] {
        let path = WorktreeRelativePath::new(path).unwrap();
        assert_eq!(project.select(&path).unwrap().profile().id(), "demo");
    }
    let path = WorktreeRelativePath::new("README").unwrap();
    assert_eq!(
        project.select(&path).unwrap_err(),
        DiagnosticsSelectionError::UnsupportedPath
    );
}

#[test]
fn publishes_file_directory_absent_and_ungated_marker_outcomes() {
    let absent = project(root());
    assert_eq!(
        absent.declarations()[0].gate(),
        &DiagnosticsMarkerGate::Gated { required: MARKERS }
    );
    assert_eq!(absent.declarations()[0].neutral_id(), None);
    assert_eq!(
        absent.declarations()[1].gate(),
        &DiagnosticsMarkerGate::NoMarkersRequired
    );
    assert_eq!(absent.declarations()[1].neutral_id(), None);

    let file_root = root();
    fs::write(file_root.join("marker.file"), "").unwrap();
    let file = project(file_root);
    assert_eq!(
        file.declarations()[0].gate(),
        &DiagnosticsMarkerGate::Matched {
            marker: "marker.file",
            kind: DiagnosticsMarkerKind::File,
        }
    );

    let directory_root = root();
    fs::create_dir(directory_root.join("marker.dir")).unwrap();
    let directory = project(directory_root);
    assert_eq!(
        directory.declarations()[0].gate(),
        &DiagnosticsMarkerGate::Matched {
            marker: "marker.dir",
            kind: DiagnosticsMarkerKind::Directory,
        }
    );
}

#[test]
fn preserves_stable_order_realized_values_and_neutral_reverse_mapping() {
    let project = project(root());
    let declarations = project.declarations();
    assert_eq!(
        declarations[0].id(),
        LanguageServerId::new("demo", 0, "gated")
    );
    assert_eq!(
        declarations[1].id(),
        LanguageServerId::new("demo", 1, "always")
    );
    assert_eq!(declarations[0].source(), "gated");
    assert_eq!(declarations[0].arguments(), &["--stdio"]);
    assert_eq!(declarations[0].completion(), CompletionPolicy::Unsupported);
    assert_eq!(declarations[1].completion(), CompletionPolicy::Pull);
    assert_eq!(
        declarations[0].initialization_options(),
        &json!({"enabled": true})
    );
    assert_eq!(
        declarations[0].workspace_settings(),
        Some(&json!({"mode": "strict"}))
    );
    assert_eq!(declarations[1].neutral_id(), None);
}

#[test]
fn reports_ambiguous_explicit_registry_selection() {
    const OTHER: LanguageServiceProfile =
        LanguageServiceProfile::new("other", "1", &["other"], &["demo"], &[], &[]);
    const AMBIGUOUS: &[LanguageServiceProfile] = &[PROFILE, OTHER];
    let project = HeadlessDiagnosticsProject::new(
        DiagnosticsRegistry::new(AMBIGUOUS).unwrap(),
        root(),
        LanguageSettings::default(),
        ProjectId::FIRST,
    )
    .unwrap();
    let path = WorktreeRelativePath::new("main.demo").unwrap();
    assert_eq!(
        project.select(&path).unwrap_err(),
        DiagnosticsSelectionError::AmbiguousPath
    );
}

#[test]
fn gated_servers_do_not_reserve_manager_capacity() {
    let project = project(root());
    let manager = ProjectManager::new(kvim_lsp::ManagerLimits {
        processes: 1,
        ..kvim_lsp::ManagerLimits::default()
    });
    let path = WorktreeRelativePath::new("main.demo").unwrap();
    let (opened, driver) = project
        .open(&manager, &path)
        .expect("only ungated server reserves capacity");
    assert_eq!(manager.projects(), 1);
    assert_eq!(opened.declarations().len(), 2);
    assert_eq!(opened.declarations()[0].neutral_id(), None);
    assert_eq!(
        opened.declarations()[1].neutral_id(),
        Some(ServerId::new(0))
    );
    assert_eq!(
        opened
            .declaration_for(ServerId::new(0))
            .unwrap()
            .id()
            .server(),
        "always"
    );
    drop(driver);
    drop(opened);
    assert_eq!(manager.projects(), 0);
}
