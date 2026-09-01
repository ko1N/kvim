//! Runs two diagnostics requests through one grammar-free warm project.
//!
//! The host owns the path, text, and revision. Kvim owns language declarations,
//! completion policy, protocol sessions, and process lifecycle. This example uses
//! an in-memory launcher, so it needs no installed server, network, or terminal.
//!
//! Run with:
//!
//! ```text
//! cargo run -p kvim-language --example headless_diagnostics --no-default-features
//! ```

use std::error::Error;
use std::io;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kvim_language::{DiagnosticsRegistry, HeadlessDiagnosticsProject};
use kvim_lsp::{
    ChangedFile, DiagnosticsOutcome, DocumentRevision, LSP_MESSAGE_BYTES_MAX, LaunchedServer,
    ManagerLimits, ProjectId, ProjectManager, ServerLaunchError, ServerLaunchRequest,
    ServerLauncher, ServerProcessHandle, ServerTerminate, ServerWait, WaitPolicy, read_frame,
};
use kvim_path::WorktreeRelativePath;
use kvim_settings::LanguageSettings;
use serde_json::{Value, json};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use tokio::io::{AsyncWriteExt, DuplexStream, duplex};
use tokio::sync::oneshot;
use tokio::time;

const DOCUMENT: &str = "src/main.js";
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);

struct FixtureLauncher {
    launches: Arc<Mutex<Vec<ServerLaunchRequest>>>,
}

struct FixtureLifecycle {
    terminate: Option<oneshot::Sender<()>>,
    exited: Option<oneshot::Receiver<()>>,
}

impl Drop for FixtureLifecycle {
    fn drop(&mut self) {
        if let Some(terminate) = self.terminate.take() {
            let _ = terminate.send(());
        }
    }
}

impl ServerProcessHandle for FixtureLifecycle {
    fn wait(&mut self) -> ServerWait {
        let exited = self.exited.take().expect("Kvim waits once");
        Box::pin(async move {
            exited
                .await
                .map_err(|_| kvim_lsp::ServerWaitError(io::Error::other("fixture task stopped")))?;
            #[cfg(unix)]
            return Ok(ExitStatus::from_raw(0));
            #[cfg(not(unix))]
            compile_error!("kvim supports macOS and Linux");
        })
    }

    fn terminate(&mut self) -> ServerTerminate<'_> {
        if let Some(terminate) = self.terminate.take() {
            let _ = terminate.send(());
        }
        Box::pin(async { Ok(()) })
    }
}

impl ServerLauncher for FixtureLauncher {
    fn launch(
        &mut self,
        request: &ServerLaunchRequest,
    ) -> Result<LaunchedServer, ServerLaunchError> {
        self.launches
            .lock()
            .expect("launch log lock")
            .push(request.clone());
        let (session_input, server_output) = duplex(64 * 1024);
        let (server_input, session_output) = duplex(64 * 1024);
        let (errors, errors_peer) = duplex(64);
        let (terminate, terminated) = oneshot::channel();
        let (exit, exited) = oneshot::channel();
        tokio::spawn(serve_fixture(
            server_output,
            server_input,
            errors_peer,
            terminated,
            exit,
        ));
        Ok(LaunchedServer::new(
            session_input,
            session_output,
            errors,
            FixtureLifecycle {
                terminate: Some(terminate),
                exited: Some(exited),
            },
        ))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("kvim-headless-example-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir(&root)?;
    std::fs::write(root.join("eslint.config.js"), "export default [];\n")?;
    let root = root.canonicalize()?;
    let launches = Arc::new(Mutex::new(Vec::new()));
    let project = HeadlessDiagnosticsProject::with_launchers(
        DiagnosticsRegistry::first_release(),
        root,
        LanguageSettings::default(),
        ProjectId::FIRST,
        {
            let launches = Arc::clone(&launches);
            move |_| {
                Box::new(FixtureLauncher {
                    launches: Arc::clone(&launches),
                })
            }
        },
    )?;
    let path = WorktreeRelativePath::new(DOCUMENT)?;
    let selection = project.select(&path)?;
    println!("language: {}", selection.profile().id());
    println!(
        "declared server: {}",
        selection.declarations()[0].id().server()
    );

    let manager = ProjectManager::new(ManagerLimits::default());
    let (opened, driver) = project.open(&manager, &path)?;
    let driver = tokio::spawn(driver.run());

    for (revision, text) in [(7, "fn first() {}\n"), (8, "fn second() {}\n")] {
        let request = ChangedFile::new(
            path.clone(),
            text.to_owned(),
            DocumentRevision::new(revision),
            selection_language(&opened)?,
        )
        .wait(WaitPolicy::Until(REQUEST_DEADLINE));
        let DiagnosticsOutcome::Ready(report) = opened.hub().changed_file(request).await? else {
            return Err("the fixture did not complete the requested revision".into());
        };
        assert_eq!(report.revision(), DocumentRevision::new(revision));
        println!("completed revision: {}", report.revision().get());
    }

    assert_eq!(launches.lock().expect("launch log lock").len(), 2);
    let cleanup_root = opened.root().path().to_path_buf();
    let (_, handle) = opened.into_parts();
    handle.close().await;
    time::timeout(Duration::from_secs(5), driver).await??;
    assert_eq!(manager.projects(), 0);
    std::fs::remove_dir_all(cleanup_root)?;
    Ok(())
}

fn selection_language(
    opened: &kvim_language::OpenedHeadlessDiagnosticsProject,
) -> Result<kvim_lsp::LanguageId, kvim_lsp::LspError> {
    opened
        .declarations()
        .iter()
        .find_map(|declaration| {
            declaration
                .neutral_id()
                .map(|_| declaration.language_id().clone())
        })
        .ok_or(kvim_lsp::LspError::NoServerDeclared)
}

async fn serve_fixture(
    mut input: DuplexStream,
    mut output: DuplexStream,
    _errors: DuplexStream,
    mut terminated: oneshot::Receiver<()>,
    exited: oneshot::Sender<()>,
) {
    let mut read_bytes = 0;
    loop {
        let body = tokio::select! {
            _ = &mut terminated => break,
            result = read_frame(&mut input, &mut read_bytes, LSP_MESSAGE_BYTES_MAX * 8) => {
                let Ok(body) = result else { break };
                body
            }
        };
        let message: Value = serde_json::from_slice(&body).expect("fixture receives JSON");
        match message["method"].as_str().unwrap_or_default() {
            "initialize" => {
                respond(
                    &mut output,
                    &message["id"],
                    json!({"capabilities": {
                        "textDocumentSync": 1,
                        "diagnosticProvider": {"identifier": "fixture"}
                    }}),
                )
                .await;
            }
            "textDocument/diagnostic" => {
                respond(
                    &mut output,
                    &message["id"],
                    json!({"kind": "full", "items": []}),
                )
                .await;
            }
            "shutdown" => respond(&mut output, &message["id"], Value::Null).await,
            "exit" => break,
            _ => {}
        }
    }
    let _ = exited.send(());
}

async fn respond(output: &mut DuplexStream, id: &Value, result: Value) {
    let body = serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
        .expect("fixture response serializes");
    output
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .expect("fixture writes header");
    output.write_all(&body).await.expect("fixture writes body");
    output.flush().await.expect("fixture flushes response");
}
