use std::cell::RefCell;
use std::error::Error;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time;

use super::{
    ErrorRecorder, LSP_RESULT_ID_BYTES_MAX, LSP_STDERR_BYTES_MAX, LSP_STDERR_LINE_BYTES_MAX,
    ServerProcess, ServerReport, SynchronizationMode, Transport, TransportFactory,
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
    let mut factory = TransportFactory::Process {
        program: OsString::from(SHELL),
        args: arguments,
        root: PathBuf::from("/"),
    };
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
            ServerReport::Started => {}
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

    for expected in [b'a', b'b'] {
        let (process, mut streams) = ServerProcess::open(&mut factory, |_| {})
            .expect("public owner accepts custom transport");
        streams
            .writer
            .notify("test/attempt", json!({ "byte": expected }))
            .await
            .expect("write request frame");
        drop(streams);
        process.close().await;
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
            ServerReport::Started => {}
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

    let (lines, bounds) = recorded(&mut reports).await;
    assert!(
        lines.iter().any(|line| line == SHIM_LINE),
        "the recorded output names the cause, not {lines:?}"
    );
    assert_eq!(bounds, 0, "a short output passes no bound");
    process.close().await;
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

    let (lines, bounds) = recorded(&mut reports).await;
    assert_eq!(bounds, 1, "the attempt reports the bound that it passed");
    assert!(
        lines
            .iter()
            .all(|line| line.len() <= LSP_STDERR_LINE_BYTES_MAX),
        "every recorded line stays inside the line bound"
    );
    process.close().await;
}

#[tokio::test]
async fn dropping_the_process_ends_the_child() {
    // A cancelled session drops its process without closing it. The child
    // must still end, because an untracked server would outlive its editor.
    let mut factory = TransportFactory::Process {
        program: OsString::from(SHELL),
        args: vec![OsString::from("-c"), OsString::from("sleep 600")],
        root: PathBuf::from("/"),
    };
    let (process, streams) = ServerProcess::open(&mut factory, |_| {})
        .expect("every supported platform holds the shell");
    drop(streams);
    let pid = process
        .child
        .as_ref()
        .and_then(Child::id)
        .expect("the child runs");
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
}
