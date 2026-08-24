//! Returns the diagnostics of one changed file through a warm project session.
//!
//! The example is one complete consumer of this crate. It needs no installed
//! language server, no network, and no terminal. It starts itself again as a
//! deterministic fixture server that speaks the protocol over its standard
//! streams.
//!
//! The fixture delays its `initialize` answer. The example still sends exactly
//! one request, with `WaitPolicy::Until`, before that server is ready. The one
//! request stays alive through the startup and returns the diagnostics of the
//! exact revision that it named. It starts no watcher, polls nothing, and
//! resubmits nothing.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p kvim-lsp --example lsp_diagnostics
//! ```

use std::error::Error;
use std::ffi::OsString;
use std::time::Duration;

use kvim_lsp::{
    ChangedFile, CompletionPolicy, DiagnosticsHub, DiagnosticsOutcome, DiagnosticsServer,
    DocumentRevision, LSP_MESSAGE_BYTES_MAX, LanguageId, ManagerLimits, ProjectDeclaration,
    ProjectId, ProjectManager, ServerDeclaration, ServerId, ServerOutcome, TransportFactory,
    WaitPolicy, WorkspaceRoot, read_frame,
};
use kvim_path::WorktreeRelativePath;
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, stdin, stdout};
use tokio::time::{Duration as TokioDuration, Instant, sleep};

/// The argument that starts this program as the fixture server.
const FIXTURE_FLAG: &str = "--fixture-server";

/// The delay that the fixture waits before it answers `initialize`.
///
/// The delay stands for the index pass of a cold language server. The example
/// sends its request before this delay ends, so the run proves that one request
/// survives the startup of its server.
const STARTUP_DELAY: Duration = Duration::from_millis(750);

/// The deadline of the one changed-file request.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// The document that the example changes.
const DOCUMENT: &str = "src/main.rs";

/// The exact text of the changed revision.
const TEXT: &str = "fn main() {\n    let answer = 42; // TODO: name it\n}\n";

/// The marker that the fixture reports as a warning.
const MARKER: &str = "TODO";

/// The revision of the changed text.
const REVISION: i32 = 7;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args_os().any(|argument| argument == FIXTURE_FLAG) {
        return serve_fixture().await;
    }
    request_diagnostics().await
}

/// Asks one warm project session for the diagnostics of one revision.
async fn request_diagnostics() -> Result<(), Box<dyn Error>> {
    let root = WorkspaceRoot::new(std::env::temp_dir().canonicalize()?)?;
    let language = LanguageId::new("rust")?;

    // The hub owns the request side. It creates one conversation for each
    // declared server, and the project driver keeps that server warm.
    let hub = DiagnosticsHub::new();
    let conversation = hub.server(DiagnosticsServer {
        id: ServerId::new(0),
        source: "fixture".to_owned(),
        languages: vec![language.clone()],
        completion: CompletionPolicy::Pull,
    })?;

    let manager = ProjectManager::new(ManagerLimits::default());
    let declaration = ProjectDeclaration::new(ProjectId::FIRST, root).server(
        ServerDeclaration {
            id: ServerId::new(0),
            transport: TransportFactory::Process {
                program: std::env::current_exe()?.into_os_string(),
                args: vec![OsString::from(FIXTURE_FLAG)],
                root: std::env::temp_dir(),
            },
            options: json!({}),
            workspace_settings: None,
        },
        conversation,
    );
    let (handle, driver) = manager.open(declaration)?;
    // The host runs the driver. This crate creates no runtime and detaches no
    // task of its own.
    let project = tokio::spawn(driver.run());

    // The one request starts here, while the fixture still sleeps in its
    // handshake. No watcher and no second request follow it.
    let started = Instant::now();
    let request = ChangedFile::new(
        WorktreeRelativePath::new(DOCUMENT)?,
        TEXT.to_owned(),
        DocumentRevision::new(REVISION),
        language,
    )
    .wait(WaitPolicy::Until(REQUEST_DEADLINE));
    let outcome = hub.changed_file(request).await?;
    let waited = started.elapsed();

    let DiagnosticsOutcome::Ready(report) = outcome else {
        return Err(format!("the request returned {outcome:?} instead of one report").into());
    };
    println!("revision: {}", report.revision().get());
    println!(
        "waited: {} ms, which passes the {} ms startup delay of the fixture",
        waited.as_millis(),
        STARTUP_DELAY.as_millis()
    );
    for server in report.servers() {
        println!("server {}: {:?}", server.server.get(), server.outcome);
        if !matches!(server.outcome, ServerOutcome::Ready { .. }) {
            return Err("the fixture server reached no ready outcome".into());
        }
    }
    for reported in report.diagnostics() {
        let diagnostic = &reported.diagnostic;
        println!(
            "  {:?} line {} columns {}..{} [{}] {}",
            diagnostic.severity,
            diagnostic.span.start.line,
            diagnostic.span.start.byte_column,
            diagnostic.span.end.byte_column,
            diagnostic.source,
            diagnostic.message
        );
        for related in &reported.related {
            println!(
                "    also line {}: {}",
                related.span.start.line, related.message
            );
        }
    }

    assert_eq!(report.revision(), DocumentRevision::new(REVISION));
    assert!(
        waited >= STARTUP_DELAY,
        "the one request waited through the startup of its server"
    );

    handle.close().await;
    let _ = project.await;
    Ok(())
}

/// Serves the protocol over the standard streams of this process.
///
/// The fixture is deterministic. It answers `initialize` after
/// [`STARTUP_DELAY`], it advertises the pull model, and it reports the marker
/// of the text that it received.
async fn serve_fixture() -> Result<(), Box<dyn Error>> {
    let mut input = stdin();
    let mut output = stdout();
    let mut read_bytes = 0_usize;
    let mut document = String::new();
    let mut uri = String::new();
    loop {
        let Ok(body) = read_frame(&mut input, &mut read_bytes, LSP_MESSAGE_BYTES_MAX * 8).await
        else {
            return Ok(());
        };
        let message: Value = serde_json::from_slice(&body)?;
        let method = message["method"].as_str().unwrap_or_default();
        match method {
            "initialize" => {
                // The delay stands for the index pass of a cold server.
                sleep(TokioDuration::from(STARTUP_DELAY)).await;
                answer(
                    &mut output,
                    &message["id"],
                    json!({
                        "capabilities": {
                            "positionEncoding": "utf-8",
                            "textDocumentSync": 1,
                            "diagnosticProvider": { "identifier": "fixture" },
                        }
                    }),
                )
                .await?;
            }
            "textDocument/didOpen" => {
                document = message["params"]["textDocument"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                uri = message["params"]["textDocument"]["uri"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
            }
            "textDocument/diagnostic" => {
                answer(
                    &mut output,
                    &message["id"],
                    json!({ "kind": "full", "items": report(&document, &uri) }),
                )
                .await?;
            }
            "shutdown" => answer(&mut output, &message["id"], Value::Null).await?,
            "exit" => return Ok(()),
            _ => {}
        }
    }
}

/// Builds the diagnostics of the exact text that the fixture received.
fn report(document: &str, uri: &str) -> Value {
    let Some((line, column)) = find(document, MARKER) else {
        return json!([]);
    };
    json!([
        {
            "range": {
                "start": { "line": line, "character": column },
                "end": { "line": line, "character": column + MARKER.len() },
            },
            "severity": 2,
            "source": "fixture",
            "message": "the fixture found one marker",
            "relatedInformation": [
                {
                    "location": {
                        "uri": uri,
                        "range": {
                            "start": { "line": 0, "character": 0 },
                            "end": { "line": 0, "character": 2 },
                        },
                    },
                    "message": "the function starts here",
                }
            ],
        },
        {
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 2 },
            },
            "severity": 1,
            "source": "fixture",
            "message": "the fixture reports one error before every warning",
        }
    ])
}

/// Returns the line and the byte column of one marker inside the text.
fn find(document: &str, marker: &str) -> Option<(usize, usize)> {
    document
        .split('\n')
        .enumerate()
        .find_map(|(line, text)| text.find(marker).map(|column| (line, column)))
}

/// Writes one JSON-RPC response frame.
async fn answer<W>(output: &mut W, id: &Value, result: Value) -> Result<(), Box<dyn Error>>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))?;
    output
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    output.write_all(&body).await?;
    output.flush().await?;
    Ok(())
}
