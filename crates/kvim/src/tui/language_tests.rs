//! Behavior tests for the editor side of the language services.
//!
//! Every test drives the deterministic mock server of the `language` module, so
//! it covers the real protocol path and starts no language server of the host
//! system. The test performs the pump that the terminal event loop performs: it
//! takes the bounded requests of the session and returns the typed results.
//!
//! The tests assert what the user sees: the published diagnostics, the cursor
//! after a jump, the open float, the buffer after a format, and the file after
//! a save.

use std::path::PathBuf;
use std::time::Duration;

use ratatui::layout::Rect;
use serde_json::{Value, json};

use crate::language::mock::{Harness, MockServer, pipe, session_at};
use crate::settings::EditorSettings;
use crate::terminal::{Key, KeyCode, TerminalEvent};
use crate::workspace::temp::TempDir;

use super::language::send_request;
use super::session::{MessageLevel, Session};

/// The elapsed time of every transition. The session reads no clock.
const NOW: Duration = Duration::ZERO;

/// The terminal size of every test.
const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

/// The editor, its mock server, and the workspace that both share.
struct Editor {
    session: Session,
    harness: Harness,
    server: MockServer,
    /// The server that the session reaches after the running one fails.
    spare: Option<MockServer>,
    /// The workspace that holds every test file. Its drop removes the files.
    directory: TempDir,
    root: PathBuf,
}

impl Editor {
    /// Starts one editor over a workspace that holds the named files.
    ///
    /// The first file is the buffer that the editor opens.
    async fn start(label: &str, files: &[(&str, &str)]) -> Self {
        Self::start_with_attempts(label, files, 1).await
    }

    /// Starts one editor whose session may run over more than one server.
    ///
    /// A restart test needs a second server, because the session creates one
    /// transport for each attempt.
    async fn start_with_attempts(label: &str, files: &[(&str, &str)], attempts: usize) -> Self {
        assert!(
            (1..=2).contains(&attempts),
            "the harness prepares one server for each attempt"
        );
        let directory = TempDir::new(label);
        let root = std::fs::canonicalize(&directory.path).expect("the directory exists");
        for (name, content) in files {
            directory.write(name, content);
        }
        let mut transports = Vec::with_capacity(attempts);
        let mut servers = Vec::with_capacity(attempts);
        for _ in 0..attempts {
            let (transport, server) = pipe();
            transports.push(transport);
            servers.push(server);
        }
        let harness = session_at(root.clone(), transports, true);
        // The session takes the transports in order, so the second server
        // serves the attempt that follows a failure.
        let spare = (servers.len() > 1).then(|| servers.remove(1));
        let mut server = servers.remove(0);
        server.handshake().await;

        let mut settings = EditorSettings::default();
        settings.files.undo_file = false;
        let mut editor = Self {
            session: Session::new(AREA, settings, PathBuf::from("/workspace")),
            harness,
            server,
            spare,
            directory,
            root,
        };
        let first = files.first().expect("one test file exists").0;
        editor.open(first).await;
        editor
    }

    /// Drops the running server, so the session restarts over the spare one.
    ///
    /// The new server holds no document, which is the state that the editor
    /// must recover from. See `docs/language-services.md`.
    fn crash_server(&mut self) {
        let spare = self
            .spare
            .take()
            .expect("the test prepared a second server");
        self.server = spare;
    }

    /// Opens one workspace file and synchronizes it with the server.
    async fn open(&mut self, name: &str) {
        self.session.open_path(self.root.join(name));
        self.run_file_request();
        self.pump();
        self.server.expect("textDocument/didOpen").await;
    }

    /// Runs the queued file request, like the event loop and the worker service.
    fn run_file_request(&mut self) {
        let request = self
            .session
            .take_file_request()
            .expect("the transition queued one file request");
        self.session.apply_file_result(request.run());
    }

    /// Sends every queued language request, like the terminal event loop.
    fn pump(&mut self) {
        while let Some(request) = self.session.take_language_request() {
            let kind = request.kind();
            let result = send_request(self.harness.handle(), request);
            self.session.apply_language_dispatch(kind, result);
        }
    }

    /// Applies the next result of the language services.
    async fn publish(&mut self) {
        let event = self.harness.next_event().await;
        self.session.apply_language_event(event);
    }

    /// Reads the next message that is not an incremental synchronization.
    ///
    /// Every edit produces one `didChange` before the next question, so a test
    /// that edits first still asserts the question that it asked for.
    async fn expect_request(&mut self, method: &str) -> Value {
        loop {
            let message = self.server.read_message().await;
            if message["method"] == "textDocument/didChange" {
                continue;
            }
            assert_eq!(message["method"], method, "unexpected message {message}");
            return message;
        }
    }

    /// Feeds a run of plain character keys.
    fn press(&mut self, keys: &str) {
        for value in keys.chars() {
            self.session
                .handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char(value))), NOW);
        }
    }

    /// Feeds one plain key without a character.
    fn press_code(&mut self, code: KeyCode) {
        self.session
            .handle_event(TerminalEvent::Key(Key::plain(code)), NOW);
    }

    /// Saves the active buffer, like `Ctrl-S`.
    fn save(&mut self) {
        self.session
            .handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    }

    /// Returns the `file` URI of one workspace file.
    fn uri(&self, name: &str) -> String {
        format!("file://{}", self.root.join(name).display())
    }

    /// Returns the message text, or an empty text while the line is empty.
    fn message(&self) -> String {
        self.session
            .message()
            .map_or_else(String::new, |message| message.text().to_owned())
    }

    /// Returns the cursor line and source column of the focused window.
    fn cursor(&self) -> (usize, usize) {
        let cursor = self.session.visible().editing.cursor();
        (cursor.line().get(), cursor.column().get())
    }

    /// Returns the rows of the open float, or an empty list while none is open.
    fn float_rows(&self) -> Vec<String> {
        self.session.visible().float.map_or_else(Vec::new, |float| {
            float
                .rows
                .iter()
                .map(|row| row.text.clone())
                .collect::<Vec<_>>()
        })
    }

    /// Returns the diagnostic messages of the active buffer, in order.
    fn diagnostics(&self) -> Vec<String> {
        let visible = self.session.visible();
        visible
            .diagnostics(visible.active)
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect()
    }

    /// Returns the current content of one workspace file.
    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.directory.join(name)).expect("the test file exists")
    }
}

/// Returns one protocol range over one line.
fn span(line: u32, start: u32, end: u32) -> Value {
    json!({
        "start": { "line": line, "character": start },
        "end": { "line": line, "character": end },
    })
}

/// Returns one diagnostics notification for one document.
fn diagnostics_notification(uri: &str, entries: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": { "uri": uri, "diagnostics": entries },
    })
}

#[tokio::test]
async fn diagnostics_reach_the_buffer_and_an_obsolete_set_is_dropped() {
    let mut editor = Editor::start(
        "language-diagnostics",
        &[("main.rs", "fn main() {}\nlet x = 1;\n")],
    )
    .await;
    let uri = editor.uri("main.rs");

    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([{ "range": span(1, 0, 3), "severity": 1, "message": "first" }]),
        ))
        .await;
    editor.publish().await;
    assert_eq!(editor.diagnostics(), vec!["first".to_owned()]);

    // The buffer moves to a new version, and the event loop has not
    // synchronized it yet, so the next set describes text that no longer
    // exists.
    editor.press("ix");
    editor.press_code(KeyCode::Esc);
    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([{ "range": span(1, 0, 3), "severity": 1, "message": "second" }]),
        ))
        .await;
    editor.publish().await;
    assert_eq!(
        editor.diagnostics(),
        vec!["first".to_owned()],
        "a set for an obsolete buffer version never reaches visible state"
    );
    assert_eq!(
        editor.session.buffer().to_string(),
        "xfn main() {}\nlet x = 1;\n",
        "a diagnostic never changes the buffer text"
    );
}

#[tokio::test]
async fn diagnostic_navigation_wraps_and_an_empty_set_moves_no_cursor() {
    let mut editor = Editor::start(
        "language-navigation",
        &[("main.rs", "one\ntwo\nthree\nfour\n")],
    )
    .await;
    let uri = editor.uri("main.rs");

    // An empty set answers no jump and moves no cursor.
    editor.press("]d");
    assert_eq!(editor.cursor(), (0, 0));
    assert_eq!(editor.message(), "the buffer holds no diagnostic");

    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([
                { "range": span(2, 0, 5), "severity": 2, "message": "later" },
                { "range": span(0, 0, 3), "severity": 1, "message": "earlier" },
            ]),
        ))
        .await;
    editor.publish().await;
    assert_eq!(
        editor.diagnostics(),
        vec!["earlier".to_owned(), "later".to_owned()],
        "the set ascends by position, so navigation is deterministic"
    );

    // The cursor starts on the first diagnostic, so the next jump reaches the
    // second one.
    editor.press("]d");
    assert_eq!(editor.cursor(), (2, 0));
    // No diagnostic follows the last one, so the jump wraps to the first.
    editor.press("]d");
    assert_eq!(editor.cursor(), (0, 0));
    // No diagnostic precedes the first one, so the jump wraps to the last.
    editor.press("[d");
    assert_eq!(editor.cursor(), (2, 0));
    editor.press("[d");
    assert_eq!(editor.cursor(), (0, 0));
}

#[tokio::test]
async fn the_diagnostic_float_shows_the_diagnostics_at_the_cursor() {
    let mut editor = Editor::start("language-float", &[("main.rs", "let x = 1;\n")]).await;
    let uri = editor.uri("main.rs");
    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([{
                "range": span(0, 4, 5),
                "severity": 1,
                "source": "check",
                "message": "unused value",
            }]),
        ))
        .await;
    editor.publish().await;

    // The cursor stands before the marked range, so no diagnostic answers.
    editor.press(" e");
    assert_eq!(editor.message(), "no diagnostic at the cursor");
    assert!(editor.float_rows().is_empty());

    editor.press("llll e");
    assert_eq!(editor.float_rows(), vec!["check: unused value".to_owned()]);
    // The next key closes the float, which changes no buffer text.
    editor.press("l");
    assert!(editor.float_rows().is_empty());
    assert_eq!(editor.session.buffer().to_string(), "let x = 1;\n");
}

#[tokio::test]
async fn a_definition_moves_the_cursor_inside_this_file_and_into_another_file() {
    let mut editor = Editor::start(
        "language-definition",
        &[
            ("main.rs", "fn main() {\n    helper();\n}\n"),
            ("helper.rs", "pub fn helper() {}\n"),
        ],
    )
    .await;

    editor.press("gd");
    editor.pump();
    let request = editor.expect_request("textDocument/definition").await;
    editor
        .server
        .respond(
            &request["id"],
            json!({ "uri": editor.uri("main.rs"), "range": span(0, 3, 7) }),
        )
        .await;
    editor.publish().await;
    assert_eq!(
        editor.cursor(),
        (0, 3),
        "a target of this file moves only the cursor"
    );
    assert_eq!(editor.session.active_buffer().name(), "main.rs");

    editor.press("gd");
    editor.pump();
    let request = editor.expect_request("textDocument/definition").await;
    editor
        .server
        .respond(
            &request["id"],
            json!([{ "uri": editor.uri("helper.rs"), "range": span(0, 7, 13) }]),
        )
        .await;
    editor.publish().await;
    // The target lives in another file, so the focused window opens it.
    editor.run_file_request();
    assert_eq!(editor.session.active_buffer().name(), "helper.rs");
    assert_eq!(editor.cursor(), (0, 7));

    editor.pump();
    editor.expect_request("textDocument/didOpen").await;
}

#[tokio::test]
async fn a_definition_answer_without_a_target_reports_it() {
    let mut editor =
        Editor::start("language-no-definition", &[("main.rs", "fn main() {}\n")]).await;

    editor.press("gd");
    editor.pump();
    let request = editor.expect_request("textDocument/definition").await;
    editor.server.respond(&request["id"], Value::Null).await;
    editor.publish().await;

    assert_eq!(editor.message(), "no definition found");
    assert_eq!(editor.cursor(), (0, 0));
}

#[tokio::test]
async fn hover_opens_a_float_and_an_empty_answer_reports_it() {
    let mut editor = Editor::start("language-hover", &[("main.rs", "fn main() {}\n")]).await;

    editor.press(" k");
    editor.pump();
    let request = editor.expect_request("textDocument/hover").await;
    editor
        .server
        .respond(
            &request["id"],
            json!({ "contents": { "kind": "markdown", "value": "fn main()" } }),
        )
        .await;
    editor.publish().await;
    assert_eq!(editor.float_rows(), vec!["fn main()".to_owned()]);

    editor.press(" k");
    assert!(
        editor.float_rows().is_empty(),
        "the next key closes the float"
    );
    editor.pump();
    let request = editor.expect_request("textDocument/hover").await;
    editor.server.respond(&request["id"], Value::Null).await;
    editor.publish().await;
    assert_eq!(editor.message(), "no hover information");
    assert!(editor.float_rows().is_empty());
}

#[tokio::test]
async fn format_on_save_applies_one_transaction_that_one_undo_reverses() {
    let mut editor = Editor::start("language-format", &[("main.rs", "fn  main( )  {}\n")]).await;

    editor.save();
    editor.pump();
    let request = editor.expect_request("textDocument/formatting").await;
    editor
        .server
        .respond(
            &request["id"],
            json!([{ "range": span(0, 0, 15), "newText": "fn main() {}" }]),
        )
        .await;
    editor.publish().await;
    assert_eq!(editor.session.buffer().to_string(), "fn main() {}\n");

    editor.run_file_request();
    assert_eq!(editor.file("main.rs"), "fn main() {}\n");
    assert!(!editor.session.active_buffer().is_modified());

    // The complete formatter answer is one transaction, so one undo reverses it.
    editor.press("u");
    assert_eq!(editor.session.buffer().to_string(), "fn  main( )  {}\n");
}

#[tokio::test]
async fn a_stale_formatting_answer_is_discarded_and_the_save_still_runs() {
    let mut editor =
        Editor::start("language-stale-format", &[("main.rs", "fn  main() {}\n")]).await;

    editor.save();
    editor.pump();
    let request = editor.expect_request("textDocument/formatting").await;
    // The buffer moves to a new version while the formatter works, so its
    // answer describes text that no longer exists.
    editor.press("ix");
    editor.press_code(KeyCode::Esc);
    editor
        .server
        .respond(
            &request["id"],
            json!([{ "range": span(0, 0, 13), "newText": "fn main() {}" }]),
        )
        .await;
    editor.publish().await;
    assert_eq!(
        editor.session.buffer().to_string(),
        "xfn  main() {}\n",
        "an obsolete formatter answer never changes the buffer"
    );

    editor.run_file_request();
    assert_eq!(
        editor.file("main.rs"),
        "xfn  main() {}\n",
        "the save writes the content that the user typed"
    );
}

#[tokio::test]
async fn a_failed_formatter_still_saves_the_buffer() {
    let mut editor =
        Editor::start("language-format-failure", &[("main.rs", "fn main() {}\n")]).await;
    editor.press("ox");
    editor.press_code(KeyCode::Esc);

    editor.save();
    editor.pump();
    let request = editor.expect_request("textDocument/formatting").await;
    editor
        .server
        .send(&json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "error": { "code": -32603, "message": "the formatter failed" },
        }))
        .await;
    editor.publish().await;

    editor.run_file_request();
    assert_eq!(
        editor.file("main.rs"),
        "fn main() {}\nx\n",
        "a formatter failure never loses the buffer content"
    );
    assert_eq!(
        editor.session.message().map(|message| message.level()),
        Some(MessageLevel::Info),
        "the save reports its own result last"
    );
}

#[tokio::test]
async fn a_restart_opens_every_document_again() {
    let mut editor =
        Editor::start_with_attempts("language-restart", &[("main.rs", "fn main() {}\n")], 2).await;
    let uri = editor.uri("main.rs");
    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([{ "range": span(0, 0, 2), "severity": 1, "message": "first" }]),
        ))
        .await;
    editor.publish().await;
    assert_eq!(editor.diagnostics(), vec!["first".to_owned()]);

    // The running server dies, so the session reports the failure and restarts.
    editor.crash_server();
    editor.publish().await;
    editor.publish().await;
    assert!(
        editor.diagnostics().is_empty(),
        "every published diagnostic belongs to the server that stopped"
    );

    // The new server holds no document, so the editor opens its buffer again.
    editor.server.handshake().await;
    editor.pump();
    let reopened = editor.expect_request("textDocument/didOpen").await;
    assert_eq!(reopened["params"]["textDocument"]["uri"], uri);
    assert_eq!(
        reopened["params"]["textDocument"]["text"], "fn main() {}\n",
        "the new open carries the exact text of the current buffer version"
    );
    assert_eq!(
        editor.session.buffer().to_string(),
        "fn main() {}\n",
        "a restart never changes the buffer text"
    );
}
