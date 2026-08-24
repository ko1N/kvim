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

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use kvim_language::mock::{
    self, Harness, MockServer, OTHER_SERVER, named_session_at, pipe, session_at,
};
use kvim_language::{
    FormatterFailure, LanguageOutcome, LanguageRegistry, LanguageServerHandle, LspError,
    MARKUP_SOURCE_BYTES_MAX, MarkupDocument, MarkupKind, MarkupRole, MarkupText, SyntaxHighlighter,
    SyntaxRole,
};
use kvim_runtime::ProcessOutput;
use kvim_settings::EditorSettings;
use kvim_terminal::{Key, KeyCode, TerminalEvent};
use kvim_workspace::temp::TempDir;

use super::language::{
    FLOAT_COLUMNS_MAX, FLOAT_ROWS_MAX, Float, LanguageRequestKind, send_request,
};
use super::markup::{FloatLine, FloatStyle};
use super::overlay::float_lines;
use super::session::{MessageLevel, Redraw, Session, test_root};

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
    /// The second session of the same language, when the test drives two.
    ///
    /// Both sessions serve one adapter, so the merge rules of
    /// `docs/language-services.md` apply to their answers.
    second: Option<Harness>,
    /// The mock server behind that second session.
    other: Option<MockServer>,
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

    /// Starts one editor whose one language runs two servers.
    ///
    /// The second session carries its own server identity, so every result
    /// names the server that produced it. Both sessions run before the editor
    /// opens its buffer, so each of them holds the document.
    async fn start_with_two_servers(label: &str, files: &[(&str, &str)]) -> Self {
        let mut editor = Self::prepare(label, files, 1).await;
        let (transport, mut other) = pipe();
        let second = named_session_at(OTHER_SERVER, editor.root.clone(), vec![transport], true);
        other.handshake().await;
        editor.second = Some(second);
        editor.other = Some(other);
        editor.open_first(files).await;
        editor
    }

    /// Starts one editor whose session may run over more than one server.
    ///
    /// A restart test needs a second server, because the session creates one
    /// transport for each attempt.
    async fn start_with_attempts(label: &str, files: &[(&str, &str)], attempts: usize) -> Self {
        let mut editor = Self::prepare(label, files, attempts).await;
        editor.open_first(files).await;
        editor
    }

    /// Opens the first workspace file, which every test edits.
    async fn open_first(&mut self, files: &[(&str, &str)]) {
        let first = files.first().expect("one test file exists").0;
        self.open(first).await;
    }

    /// Prepares the editor and its sessions without opening a buffer.
    async fn prepare(label: &str, files: &[(&str, &str)], attempts: usize) -> Self {
        assert!(
            (1..=2).contains(&attempts),
            "the harness prepares one server for each attempt"
        );
        let directory = TempDir::new(label);
        let root = directory.path.clone();
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
        Self {
            session: Session::new(AREA, settings, test_root(root.clone())),
            harness,
            server,
            spare,
            second: None,
            other: None,
            directory,
            root,
        }
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

    /// Opens one workspace file and synchronizes it with every server.
    async fn open(&mut self, name: &str) {
        self.session.open_path(self.root.join(name));
        self.run_file_request();
        self.pump();
        self.server.expect("textDocument/didOpen").await;
        if let Some(other) = self.other.as_mut() {
            other.expect("textDocument/didOpen").await;
        }
    }

    /// Takes the queued formatter run, like the event loop.
    fn take_format(&mut self) -> kvim_language::FormatterRequest {
        self.session
            .take_format_request()
            .expect("the save queued one formatter run")
    }

    /// Runs the queued formatter with one recorded program output.
    ///
    /// The bounded process service performs this step for the real program, so
    /// the test proves the editor path without a formatter of the host system.
    fn run_format(&mut self, status_code: Option<i32>, stdout: &str) {
        let request = self.take_format();
        let output = ProcessOutput {
            status_code,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        };
        let _ = self.session.apply_format_result(request.publish(&output));
    }

    /// Runs the queued file request, like the event loop and the worker service.
    fn run_file_request(&mut self) {
        let request = self
            .session
            .take_file_request()
            .expect("the transition queued one file request");
        let _ = self.session.apply_file_result(request.run());
    }

    /// Returns the running sessions of the editor, in declaration order.
    fn handles(&self) -> Vec<&LanguageServerHandle> {
        let mut handles = vec![self.harness.handle()];
        handles.extend(self.second.as_ref().map(Harness::handle));
        handles
    }

    /// Sends every queued language request, like the terminal event loop.
    fn pump(&mut self) {
        while let Some(request) = self.session.take_language_request() {
            let result = send_request(&self.handles(), &request);
            let _ = self.session.apply_language_dispatch(&request, result);
        }
    }

    /// Applies the next result of the language services.
    async fn publish(&mut self) {
        let event = self.harness.next_event().await;
        let _ = self.session.apply_language_event(event);
    }

    /// Applies the next result of the second session.
    async fn publish_other(&mut self) {
        let event = self
            .second
            .as_mut()
            .expect("the test started the second server")
            .next_event()
            .await;
        let _ = self.session.apply_language_event(event);
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
        let cursor = self.session.cursor();
        (cursor.line().get(), cursor.column().get())
    }

    /// Returns the rows of the open float, or an empty list while none is open.
    ///
    /// The rows are the painted ones at the widest float, because a markup
    /// answer holds no row of its own until a width renders it.
    fn float_rows(&self) -> Vec<String> {
        self.session.visible().float.map_or_else(Vec::new, |float| {
            float_lines(float, FLOAT_COLUMNS_MAX)
                .iter()
                .map(FloatLine::text)
                .collect::<Vec<_>>()
        })
    }

    /// Returns the pieces of every float row, as text and style.
    ///
    /// A row of a fence holds one piece for each highlight span, so the test
    /// reads the roles that the float paints and never a color.
    fn float_spans(&self) -> Vec<Vec<(String, FloatStyle)>> {
        self.session.visible().float.map_or_else(Vec::new, |float| {
            float_lines(float, FLOAT_COLUMNS_MAX)
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| (span.text.clone(), span.style))
                        .collect()
                })
                .collect()
        })
    }

    /// Renders one frame and returns every row as text and the cursor cell.
    ///
    /// The float must follow the cell that the terminal draws its own cursor
    /// in, so every placement assertion reads both from the same frame. A row
    /// keeps its trailing blanks, because the title band of a float ends with
    /// one blank and its column must stay readable.
    fn frame(&self) -> (Vec<String>, (u16, u16)) {
        let area = self.session.area();
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("the test backend never fails");
        terminal
            .draw(|frame| self.session.render(frame))
            .expect("the test backend never fails");
        let cursor = terminal
            .get_cursor_position()
            .expect("the test backend never fails");
        let buffer = terminal.backend().buffer().clone();
        let rows = (area.y..area.bottom())
            .map(|y| {
                let mut text = String::new();
                for x in area.x..area.right() {
                    if let Some(cell) = buffer.cell((x, y)) {
                        text.push_str(cell.symbol());
                    }
                }
                text
            })
            .collect();
        (rows, (cursor.x, cursor.y))
    }

    /// Returns the frame rows, the cursor cell, and the corner of the float.
    ///
    /// Every test buffer holds plain characters of one cell each, so the byte
    /// offset of the title inside one row is also its terminal column.
    fn float_frame(&self, title: &str) -> (Vec<String>, (u16, u16), (u16, u16)) {
        let (rows, cursor) = self.frame();
        let corner = rows
            .iter()
            .enumerate()
            .find_map(|(y, row)| {
                let x = row.find(title)?;
                let corner = (u16::try_from(x).ok()?, u16::try_from(y).ok()?);
                Some(corner)
            })
            .unwrap_or_else(|| panic!("the frame shows the float title {title}\n{rows:#?}"));
        (rows, cursor, corner)
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

    /// Returns the producer of each diagnostic of the active buffer, in order.
    fn diagnostic_sources(&self) -> Vec<String> {
        let visible = self.session.visible();
        visible
            .diagnostics(visible.active)
            .iter()
            .map(|diagnostic| diagnostic.source.clone())
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
async fn two_servers_of_one_language_merge_their_diagnostics() {
    let mut editor = Editor::start_with_two_servers(
        "language-two-servers",
        &[("main.rs", "fn main() {}\nlet x = 1;\nlet y = 2;\n")],
    )
    .await;
    let uri = editor.uri("main.rs");

    // The earlier declaration reports one problem of its own and one that both
    // servers find.
    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([
                { "range": span(2, 0, 3), "severity": 2, "message": "shared" },
                { "range": span(0, 0, 2), "severity": 1, "message": "from the first server" },
            ]),
        ))
        .await;
    editor.publish().await;
    assert_eq!(
        editor.diagnostics(),
        vec!["from the first server".to_owned(), "shared".to_owned()],
    );

    // The later declaration reports the same problem and one of its own. The
    // set of the first server stays, so both servers describe the buffer.
    editor
        .other
        .as_mut()
        .expect("the test started the second server")
        .send(&diagnostics_notification(
            &uri,
            json!([
                { "range": span(1, 0, 3), "severity": 1, "message": "from the second server" },
                { "range": span(2, 0, 3), "severity": 2, "message": "shared" },
            ]),
        ))
        .await;
    editor.publish_other().await;
    assert_eq!(
        editor.diagnostics(),
        vec![
            "from the first server".to_owned(),
            "from the second server".to_owned(),
            // The identical range and message of both servers describe one
            // problem, so the merged list holds it exactly once.
            "shared".to_owned(),
        ],
        "the merge holds every diagnostic once, in ascending position order"
    );
    assert_eq!(
        editor.diagnostic_sources(),
        vec![
            mock::SERVER.server().to_owned(),
            mock::OTHER_SERVER.server().to_owned(),
            // The earlier declaration wins the duplicate, so its identifier
            // names the producer.
            mock::SERVER.server().to_owned(),
        ],
        "a server that sends no source field is named by its declaration"
    );

    // The second server stops. Only its own diagnostics leave, and the first
    // server keeps serving the buffer.
    editor
        .second
        .as_mut()
        .expect("the test started the second server")
        .stop();
    editor.publish_other().await;
    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([{ "range": span(0, 0, 2), "severity": 1, "message": "still serving" }]),
        ))
        .await;
    editor.publish().await;
    assert_eq!(
        editor.diagnostics(),
        vec![
            "still serving".to_owned(),
            "from the second server".to_owned(),
            "shared".to_owned(),
        ],
        "one stopped server never removes the diagnostics of the other server"
    );
    assert_eq!(
        editor.diagnostic_sources(),
        vec![
            mock::SERVER.server().to_owned(),
            mock::OTHER_SERVER.server().to_owned(),
            // The new set of the first server no longer holds the shared
            // problem, so the second server now owns it alone.
            mock::OTHER_SERVER.server().to_owned(),
        ],
        "a new set replaces the previous set of its own server alone"
    );
}

#[tokio::test]
async fn the_float_names_the_producer_only_while_the_buffer_carries_several_names() {
    let mut editor = Editor::start_with_two_servers(
        "language-source-if-many",
        &[("main.rs", "let x = 1;\nlet y = 2;\n")],
    )
    .await;
    let uri = editor.uri("main.rs");

    // The second server runs, but it reports nothing yet. The buffer therefore
    // carries one producer name, and the float shows the message alone.
    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([{ "range": span(0, 0, 5), "severity": 1, "message": "from the first server" }]),
        ))
        .await;
    editor.publish().await;
    editor.press(" e");
    assert_eq!(
        editor.float_rows(),
        vec!["from the first server".to_owned()],
        "a server that reports nothing never adds a second producer name"
    );

    // The second server reports as well, so the buffer carries two names and
    // every diagnostic of it names the producer that found it.
    editor
        .other
        .as_mut()
        .expect("the test started the second server")
        .send(&diagnostics_notification(
            &uri,
            json!([{ "range": span(1, 0, 5), "severity": 1, "message": "from the second server" }]),
        ))
        .await;
    editor.publish_other().await;

    editor.press("j e");
    assert_eq!(
        editor.float_rows(),
        vec![format!(
            "{}: from the second server",
            mock::OTHER_SERVER.server()
        )],
    );

    // The first line carries one producer name of its own, and it names that
    // producer too, because the rule reads the complete buffer and not the
    // cursor position.
    editor.press("k e");
    assert_eq!(
        editor.float_rows(),
        vec![format!("{}: from the first server", mock::SERVER.server())],
        "one diagnostic never loses its name while the cursor moves"
    );
}

#[tokio::test]
async fn one_server_that_reports_under_two_names_shows_both_of_them() {
    // rust-analyzer is one server, and it separates its compiler diagnostics
    // from its lints through the `source` field. The reader needs both names,
    // so the count reads the producer names and never the server count.
    let mut editor = Editor::start(
        "language-two-sources",
        &[("main.rs", "let x = 1;\nlet y = 2;\n")],
    )
    .await;
    let uri = editor.uri("main.rs");

    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([
                {
                    "range": span(0, 0, 5),
                    "severity": 1,
                    "source": "rustc",
                    "message": "unused variable",
                },
                {
                    "range": span(1, 0, 5),
                    "severity": 2,
                    "source": "clippy",
                    "message": "needless late initialization",
                },
            ]),
        ))
        .await;
    editor.publish().await;

    editor.press(" e");
    assert_eq!(
        editor.float_rows(),
        vec!["rustc: unused variable".to_owned()],
    );
    editor.press("j e");
    assert_eq!(
        editor.float_rows(),
        vec!["clippy: needless late initialization".to_owned()],
        "one server that reports under two names keeps both of them visible"
    );
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
    // The buffer carries one producer name, so the float shows the message
    // alone. A name on every row would tell the reader nothing.
    assert_eq!(editor.float_rows(), vec!["unused value".to_owned()]);
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

/// The title band of the diagnostic float.
const DIAGNOSTIC_TITLE: &str = " Diagnostics ";

/// The title band of the hover float.
const HOVER_TITLE: &str = " Hover ";

/// Returns the text of one float row, without the padding of the float.
///
/// `offset` counts rows from the title band, so offset one is the first text
/// row. The text of a row starts one cell inside the float.
fn float_text(rows: &[String], corner: (u16, u16), offset: usize) -> String {
    let row = &rows[usize::from(corner.1) + offset];
    row[usize::from(corner.0) + 1..].trim_end().to_owned()
}

/// Starts one editor over a buffer that holds `lines` numbered lines.
async fn editor_with_lines(label: &str, lines: usize) -> Editor {
    let text: String = (0..lines).map(|index| format!("line{index}\n")).collect();
    Editor::start(label, &[("main.rs", &text)]).await
}

/// Publishes one diagnostic over one line and opens the float at its start.
async fn open_diagnostic_float(editor: &mut Editor, line: u32, message: &str) {
    let uri = editor.uri("main.rs");
    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([{ "range": span(line, 0, 4), "severity": 1, "message": message }]),
        ))
        .await;
    editor.publish().await;
    editor.press(" e");
}

#[tokio::test]
async fn the_diagnostic_float_sits_below_the_cursor_cell() {
    let mut editor = editor_with_lines("float-below", 8).await;
    editor.press("jj");
    open_diagnostic_float(&mut editor, 2, "unused value").await;

    let (rows, cursor, corner) = editor.float_frame(DIAGNOSTIC_TITLE);
    assert_eq!(
        corner,
        (cursor.0, cursor.1 + 1),
        "the float starts at the cursor column, one row below the cursor line",
    );
    assert!(
        rows[usize::from(cursor.1)].contains("line2"),
        "the float never covers the line that it describes",
    );
    assert_eq!(float_text(&rows, corner, 1), "unused value");
}

#[tokio::test]
async fn the_diagnostic_float_flips_above_the_cursor_line_at_the_bottom() {
    // The buffer is longer than the window, so `G` leaves the cursor on the
    // last visible row and no row remains below it.
    let mut editor = editor_with_lines("float-above", 60).await;
    editor.press("G");
    open_diagnostic_float(&mut editor, 59, "unused value").await;

    let (rows, cursor, corner) = editor.float_frame(DIAGNOSTIC_TITLE);
    assert!(
        corner.1 < cursor.1,
        "the float flips above the cursor line: {corner:?} and {cursor:?}",
    );
    // The float holds its title band and the one message row, so its last row
    // sits directly above the cursor line.
    assert_eq!(float_text(&rows, corner, 1), "unused value");
    assert_eq!(
        corner.1 + 2,
        cursor.1,
        "the float ends directly above the cursor line",
    );
    assert!(
        rows[usize::from(cursor.1)].contains("line59"),
        "the float never covers the line that it describes",
    );
}

#[tokio::test]
async fn the_diagnostic_float_moves_left_at_the_right_edge_of_the_window() {
    let text = format!("{}\n", "x".repeat(70));
    let mut editor = Editor::start("float-right-edge", &[("main.rs", &text)]).await;
    let uri = editor.uri("main.rs");
    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([{ "range": span(0, 60, 70), "severity": 1, "message": "unused value" }]),
        ))
        .await;
    editor.publish().await;
    editor.press("$ e");

    let (rows, cursor, corner) = editor.float_frame(DIAGNOSTIC_TITLE);
    assert!(
        corner.0 < cursor.0,
        "the float moves left of the cursor column: {corner:?} and {cursor:?}",
    );
    let row = float_text(&rows, corner, 1);
    assert_eq!(
        row, "unused value",
        "the complete message stays inside the window",
    );
}

#[tokio::test]
async fn the_diagnostic_float_shows_every_diagnostic_of_the_cursor_position() {
    let mut editor = editor_with_lines("float-many", 8).await;
    let uri = editor.uri("main.rs");
    editor
        .server
        .send(&diagnostics_notification(
            &uri,
            json!([
                { "range": span(0, 0, 4), "severity": 1, "message": "first fault" },
                { "range": span(0, 0, 4), "severity": 2, "message": "second fault" },
            ]),
        ))
        .await;
    editor.publish().await;
    editor.press(" e");

    let (rows, _, corner) = editor.float_frame(DIAGNOSTIC_TITLE);
    let float: Vec<String> = (1..=3)
        .map(|offset| float_text(&rows, corner, offset))
        .collect();
    assert_eq!(
        float,
        vec!["first fault", "", "second fault"],
        "one blank row separates the two diagnostics of the position",
    );
}

#[tokio::test]
async fn the_diagnostic_float_wraps_a_long_message_inside_a_narrow_window() {
    let mut editor = editor_with_lines("float-wrap", 8).await;
    editor.session.handle_event(
        TerminalEvent::Resize {
            columns: 34,
            rows: 20,
        },
        NOW,
    );
    open_diagnostic_float(
        &mut editor,
        0,
        "cannot borrow the value twice in the same scope",
    )
    .await;

    let (rows, _, corner) = editor.float_frame(DIAGNOSTIC_TITLE);
    let float: Vec<String> = (1..=2)
        .map(|offset| float_text(&rows, corner, offset))
        .collect();
    assert_eq!(
        float,
        vec!["cannot borrow the value twice", "in the same scope"],
        "the message wraps at the width of the narrow window",
    );
    // The float keeps one padding cell on each side of its widest row.
    assert!(
        usize::from(corner.0) + 2 + float[0].chars().count() <= 34,
        "the float stays inside the narrow window",
    );
}

#[tokio::test]
async fn the_diagnostic_float_bounds_its_height_and_reports_the_missing_rows() {
    let mut editor = editor_with_lines("float-height", 8).await;
    let message: String = (0..40)
        .map(|index| format!("note{index}\n"))
        .collect::<String>();
    open_diagnostic_float(&mut editor, 0, message.trim_end()).await;

    let (rows, _, corner) = editor.float_frame(DIAGNOSTIC_TITLE);
    // The float shows the row bound of the language module and no more.
    assert_eq!(float_text(&rows, corner, 1), "note0");
    assert_eq!(
        float_text(&rows, corner, FLOAT_ROWS_MAX - 1),
        format!("note{}", FLOAT_ROWS_MAX - 2),
    );
    assert_eq!(
        float_text(&rows, corner, FLOAT_ROWS_MAX),
        "...",
        "the last row reports that the float hides rows",
    );
    assert!(
        !rows[usize::from(corner.1) + FLOAT_ROWS_MAX + 1].contains("note"),
        "the float ends after its row bound",
    );
}

#[tokio::test]
async fn the_language_float_stays_inside_its_own_window_after_a_split() {
    let mut editor = editor_with_lines("float-split", 8).await;
    editor.press(" ");
    editor.press_code(KeyCode::Enter);
    let right = editor.session.area().width / 2;
    open_diagnostic_float(&mut editor, 0, "unused value").await;

    let (_, cursor, corner) = editor.float_frame(DIAGNOSTIC_TITLE);
    assert!(
        cursor.0 >= right,
        "the split moves the focus into the right window",
    );
    assert_eq!(
        corner,
        (cursor.0, cursor.1 + 1),
        "the float follows the cursor of the focused window",
    );
    assert!(
        corner.0 >= right,
        "the float sits inside the window that asked, not in the body band",
    );
}

#[tokio::test]
async fn the_hover_float_follows_the_cursor_through_the_same_rule() {
    let mut editor = editor_with_lines("float-hover", 8).await;
    editor.press("jj");
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

    let (rows, cursor, corner) = editor.float_frame(HOVER_TITLE);
    assert_eq!(
        corner,
        (cursor.0, cursor.1 + 1),
        "the hover float uses the placement rule of the diagnostic float",
    );
    assert_eq!(float_text(&rows, corner, 1), "fn main()");
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

/// The answer that rust-analyzer sends for one function of kvim.
///
/// The shape is the common one: one fence that names the module path, one fence
/// that holds the signature, one thematic break, and the document comment.
const RUST_ANALYZER_HOVER: &str = "```rust\nkvim_tui::language\n```\n\n```rust\nfn hover(&self) -> \
                                   Vec<&MarkupText>\n```\n\n---\n\nReturns the hover answers of \
                                   every *server*, in declaration order.";

/// Answers one hover request with `contents` and opens the float.
async fn open_hover(editor: &mut Editor, contents: Value) {
    editor.press(" k");
    editor.pump();
    let request = editor.expect_request("textDocument/hover").await;
    editor
        .server
        .respond(&request["id"], json!({ "contents": contents }))
        .await;
    editor.publish().await;
}

#[tokio::test]
async fn the_hover_float_renders_the_markdown_that_the_server_wrote() {
    let mut editor = Editor::start("hover-markdown", &[("main.rs", "fn main() {}\n")]).await;

    open_hover(
        &mut editor,
        json!({ "kind": "markdown", "value": RUST_ANALYZER_HOVER }),
    )
    .await;

    let painted = editor.float_rows();
    assert_eq!(
        painted,
        vec![
            "kvim_tui::language".to_owned(),
            String::new(),
            "fn hover(&self) -> Vec<&MarkupText>".to_owned(),
            String::new(),
            "─".repeat(64),
            String::new(),
            "Returns the hover answers of every server, in declaration order.".to_owned(),
        ],
        "no fence, no backtick, and no dash of the source reaches one row",
    );
}

#[tokio::test]
async fn the_hover_float_paints_the_code_of_a_fence_in_its_syntax_roles() {
    let mut editor = Editor::start("hover-highlight", &[("main.rs", "fn main() {}\n")]).await;

    open_hover(
        &mut editor,
        json!({ "kind": "markdown", "value": "```rust\nfn hover(&self) -> Vec<&MarkupText>\n```" }),
    )
    .await;

    let painted = editor.float_spans();
    assert_eq!(
        painted[0][0],
        ("fn".to_owned(), FloatStyle::Syntax(SyntaxRole::Keyword)),
        "the signature opens with the keyword role of a buffer: {painted:?}",
    );
    assert!(
        painted[0]
            .iter()
            .filter(|(_, style)| matches!(style, FloatStyle::Syntax(_)))
            .count()
            >= 3,
        "the signature paints several roles and not one flat color: {painted:?}",
    );
}

/// Returns the answer that one server gives for one hover request.
///
/// The language-server task names the document of a markdown answer where that
/// answer arrives, so the helper builds the value that reaches the float.
fn hover_answer(kind: MarkupKind, text: &str) -> MarkupText {
    let document = match kind {
        MarkupKind::Markdown => MarkupDocument::parse(text).highlighted(
            LanguageRegistry::first_release(),
            &mut SyntaxHighlighter::new(),
        ),
        MarkupKind::PlainText => MarkupDocument::default(),
    };
    MarkupText {
        kind,
        text: text.to_owned(),
        document,
    }
}

/// Returns the pieces of every row of one float, as text and style.
fn float_pieces(float: &Float) -> Vec<Vec<(String, FloatStyle)>> {
    float_lines(float, FLOAT_COLUMNS_MAX)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| (span.text.clone(), span.style))
                .collect()
        })
        .collect()
}

#[test]
fn two_servers_join_their_hover_answers_into_one_document() {
    // Each server answers on its own, so each answer carries its own document
    // and the float joins documents.
    let first = hover_answer(MarkupKind::Markdown, "```rust\nfn first()\n```");
    let second = hover_answer(MarkupKind::Markdown, "The *second* server.");

    let float = Float::hover(HOVER_TITLE, &[&first, &second]);

    let painted = float_pieces(&float);
    assert_eq!(
        painted
            .iter()
            .map(|row| row.iter().map(|(text, _)| text.as_str()).collect())
            .collect::<Vec<String>>(),
        vec![
            "fn first()".to_owned(),
            String::new(),
            "The second server.".to_owned(),
        ],
        "one blank row stands between the answers of two servers",
    );
    assert_eq!(
        painted[0][0],
        ("fn".to_owned(), FloatStyle::Syntax(SyntaxRole::Keyword)),
        "the fence of the first answer keeps the roles of its code",
    );
    assert_eq!(
        painted[2][1],
        (
            "second".to_owned(),
            FloatStyle::Markup(MarkupRole::Emphasis)
        ),
        "the prose of the second answer keeps the roles of its markup",
    );
    assert!(!float.is_clipped(), "neither answer reached a bound");
}

#[test]
fn one_clipped_hover_answer_reports_the_join_as_clipped() {
    let complete = hover_answer(MarkupKind::Markdown, "a short answer");
    let long = hover_answer(
        MarkupKind::Markdown,
        &"word ".repeat(MARKUP_SOURCE_BYTES_MAX),
    );
    assert!(
        long.document.is_clipped(),
        "the long answer reached a bound"
    );

    assert!(Float::hover(HOVER_TITLE, &[&complete, &long]).is_clipped());
    assert!(Float::hover(HOVER_TITLE, &[&long, &complete]).is_clipped());
}

#[test]
fn one_answer_of_plain_text_keeps_the_whole_join_as_text() {
    // A markdown parse of the plain answer would drop every marker of it, so
    // the answer of the other server stays text as well.
    let markdown = hover_answer(MarkupKind::Markdown, "```rust\nfn first()\n```");
    let plain = hover_answer(MarkupKind::PlainText, "*not emphasis* and `not code`");

    let float = Float::hover(HOVER_TITLE, &[&markdown, &plain]);

    assert_eq!(
        float_pieces(&float)
            .iter()
            .map(|row| row.iter().map(|(text, _)| text.as_str()).collect())
            .collect::<Vec<String>>(),
        vec![
            "```rust".to_owned(),
            "fn first()".to_owned(),
            "```".to_owned(),
            String::new(),
            "*not emphasis* and `not code`".to_owned(),
        ],
        "every character of both answers reaches one row",
    );
}

#[tokio::test]
async fn a_hover_answer_of_plain_text_keeps_every_character() {
    let mut editor = Editor::start("hover-plain", &[("main.rs", "fn main() {}\n")]).await;
    // A markdown parse of this text would drop every marker of it.
    let message = "*not emphasis* and `not code` and a - b";

    open_hover(
        &mut editor,
        json!({ "kind": "plaintext", "value": message }),
    )
    .await;

    assert_eq!(editor.float_rows(), vec![message.to_owned()]);
}

#[tokio::test]
async fn a_long_hover_answer_reports_the_rows_that_the_float_hides() {
    let mut editor = editor_with_lines("hover-height", 8).await;
    let value: String = (0..40).map(|index| format!("note{index}\n\n")).collect();

    open_hover(&mut editor, json!({ "kind": "markdown", "value": value })).await;

    let (rows, _, corner) = editor.float_frame(HOVER_TITLE);
    assert_eq!(float_text(&rows, corner, 1), "note0");
    assert_eq!(
        float_text(&rows, corner, FLOAT_ROWS_MAX),
        "...",
        "the last row reports that the float hides rows",
    );
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
        Some(MessageLevel::Warning),
        "the save reports its own result last, and it names the lost format"
    );
    assert!(
        editor
            .message()
            .starts_with("the formatter failed, so the file holds unformatted content; "),
        "the save report names the failure: {}",
        editor.message()
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

#[tokio::test]
async fn a_refused_change_opens_the_document_again_with_the_current_text() {
    let mut editor =
        Editor::start("language-refused-change", &[("main.rs", "fn main() {}\n")]).await;
    let uri = editor.uri("main.rs");

    // The edit produces one incremental change of the open document.
    editor.press("ix");
    editor.press_code(KeyCode::Esc);
    let change = editor
        .session
        .take_language_request()
        .expect("the edit queued one synchronization");
    assert_eq!(change.kind(), LanguageRequestKind::Synchronization);

    // A full request queue drops that change, so the copy of the running
    // session stays behind the buffer.
    let redraw = editor
        .session
        .apply_language_dispatch(&change, Err(LspError::Saturated));
    assert_eq!(redraw, Redraw::Needed);
    assert_eq!(
        editor.message(),
        "the language server queue is full; the editor opens the buffer again"
    );

    // The next pass repairs the copy. The old document closes, and the fresh
    // open carries the text that the buffer holds now.
    editor.pump();
    let closed = editor.server.expect("textDocument/didClose").await;
    assert_eq!(closed["params"]["textDocument"]["uri"], uri);
    let reopened = editor.server.expect("textDocument/didOpen").await;
    assert_eq!(reopened["params"]["textDocument"]["uri"], uri);
    assert_eq!(
        reopened["params"]["textDocument"]["text"], "xfn main() {}\n",
        "the fresh open carries the exact text of the current buffer version"
    );
    assert_eq!(
        editor.session.buffer().to_string(),
        "xfn main() {}\n",
        "a repair never changes the buffer text"
    );

    // The editor stays usable, and the next edit synchronizes against the copy
    // that the server holds now.
    editor.press("iy");
    editor.press_code(KeyCode::Esc);
    editor.pump();
    let changed = editor.server.expect("textDocument/didChange").await;
    assert_eq!(changed["params"]["contentChanges"][0]["text"], "y");
    assert_eq!(editor.session.buffer().to_string(), "xyfn main() {}\n");
}

#[tokio::test]
async fn an_external_formatter_formats_a_buffer_that_its_server_never_formats() {
    // The Nix adapter declares `nixfmt`, so the declared program formats the
    // buffer and the session sends no formatting request to its server.
    let mut editor = Editor::start("language-external-format", &[("flake.nix", "{  }\n")]).await;

    editor.save();
    editor.run_format(Some(0), "{ }\n");
    assert_eq!(editor.session.buffer().to_string(), "{ }\n");

    editor.run_file_request();
    assert_eq!(editor.file("flake.nix"), "{ }\n");
    assert!(!editor.session.active_buffer().is_modified());
    // A save that wrote formatted content reports its own result alone, so the
    // user reads a formatted save apart from an unformatted one.
    assert!(
        editor.message().ends_with("B written"),
        "the save of formatted content names no formatter state: {}",
        editor.message()
    );

    // The complete formatter answer is one transaction, so one undo reverses it.
    editor.press("u");
    assert_eq!(editor.session.buffer().to_string(), "{  }\n");
}

#[tokio::test]
async fn a_failed_external_formatter_still_saves_the_buffer_and_reports_the_failure() {
    let mut editor = Editor::start("language-external-failure", &[("flake.nix", "{  }\n")]).await;

    editor.save();
    // The program reports its refusal through its exit code.
    editor.run_format(Some(1), "");
    assert_eq!(
        editor.session.buffer().to_string(),
        "{  }\n",
        "a formatter failure never changes the buffer"
    );

    editor.run_file_request();
    assert_eq!(editor.file("flake.nix"), "{  }\n");
    // The save writes the message line after the format, so the save report
    // itself must name the failure. A report that the save replaced would leave
    // the user with no sign that the file holds unformatted content.
    assert!(
        editor
            .message()
            .starts_with("the formatter failed, so the file holds unformatted content; "),
        "the save report names the failure: {}",
        editor.message()
    );
    assert_eq!(
        editor.session.message().map(|message| message.level()),
        Some(MessageLevel::Warning),
        "a formatter that refused the document needs attention"
    );

    // A second save repeats no extra message. The note qualifies the save
    // report that every save writes, so it never fills the message line.
    editor.press("ox");
    editor.press_code(KeyCode::Esc);
    editor.save();
    editor.run_format(Some(1), "");
    editor.run_file_request();
    assert_eq!(editor.file("flake.nix"), "{  }\nx\n");
    assert!(
        editor
            .message()
            .starts_with("the formatter failed, so the file holds unformatted content; "),
        "every save names the state of the file that it wrote: {}",
        editor.message()
    );
}

#[tokio::test]
async fn an_obsolete_external_format_never_changes_the_buffer() {
    let mut editor = Editor::start("language-external-stale", &[("flake.nix", "{  }\n")]).await;

    editor.save();
    let request = editor.take_format();
    // The buffer moves to a new version while the formatter runs, so its answer
    // describes text that no longer exists.
    editor.press("ix");
    editor.press_code(KeyCode::Esc);
    let output = ProcessOutput {
        status_code: Some(0),
        stdout: b"{ }\n".to_vec(),
        stderr: Vec::new(),
    };
    let _ = editor.session.apply_format_result(request.publish(&output));

    assert_eq!(
        editor.session.buffer().to_string(),
        "x{  }\n",
        "an obsolete formatter answer never changes the buffer"
    );
    editor.run_file_request();
    assert_eq!(
        editor.file("flake.nix"),
        "x{  }\n",
        "the save writes the content that the user typed"
    );
}

#[tokio::test]
async fn a_missing_formatter_program_names_its_state_in_the_save_report() {
    let mut editor = Editor::start("language-external-missing", &[("flake.nix", "{  }\n")]).await;

    editor.save();
    let _ = editor.take_format();
    let _ = editor
        .session
        .apply_format_result(Err(FormatterFailure::NotInstalled));
    editor.run_file_request();
    assert_eq!(editor.file("flake.nix"), "{  }\n");
    assert!(
        editor
            .message()
            .starts_with("the formatter is not installed, so the file holds unformatted content; "),
        "the save report names the absent program: {}",
        editor.message()
    );
    assert_eq!(
        editor.session.message().map(|message| message.level()),
        Some(MessageLevel::Info),
        "a formatter that the host does not hold is a normal state"
    );

    // The state never changes while the editor runs, so every later save names
    // it again as part of its own report and adds no second message.
    editor.press("ox");
    editor.press_code(KeyCode::Esc);
    editor.save();
    let _ = editor.take_format();
    let _ = editor
        .session
        .apply_format_result(Err(FormatterFailure::NotInstalled));
    editor.run_file_request();
    assert_eq!(editor.file("flake.nix"), "{  }\nx\n");
    assert!(
        editor
            .message()
            .starts_with("the formatter is not installed, so the file holds unformatted content; "),
        "every save names the state of the file that it wrote: {}",
        editor.message()
    );
}

/// The line that the broken server of the user wrote before it exited.
///
/// The program was a `rustup` shim that found no `rust-analyzer` in the active
/// toolchain. It named the cause on its standard error and exited, and the
/// editor reported a restart and a stop that named no cause.
const SHIM_LINE: &str = "info: `rust-analyzer` is unavailable for the active toolchain";

/// Opens the editor log and returns the rows of the new buffer.
fn open_log(session: &mut Session) -> Vec<String> {
    for key in ":logs".chars() {
        session.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char(key))), NOW);
    }
    session.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Enter)), NOW);
    assert_eq!(session.active_buffer().name(), "[Logs]");
    session
        .buffer()
        .to_string()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn the_log_names_the_cause_of_a_server_that_cannot_start() {
    let directory = TempDir::new("log_broken_server");
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    let mut session = Session::new(AREA, settings, test_root(directory.path.clone()));
    // The child repeats the failure that this capture exists for. It is no
    // language server: it names its cause on the standard error and exits.
    let mut harness = mock::process_session(
        "/bin/sh",
        &["-c", "printf '%s\\n' \"$1\" >&2; exit 1", "shim", SHIM_LINE],
        directory.path.clone(),
    );

    // The event loop applies every result of the session until it stops.
    loop {
        let event = harness.next_any().await;
        let stopped = matches!(event.outcome, LanguageOutcome::Stopped);
        let _ = session.apply_language_event(event);
        if stopped {
            break;
        }
    }

    // The editor stays fully usable while the server fails.
    for key in "ihello".chars() {
        session.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char(key))), NOW);
    }
    session.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Esc)), NOW);
    assert_eq!(session.buffer().to_string(), "hello\n");

    let rows = open_log(&mut session);
    assert!(
        rows.iter()
            .any(|row| row.contains("SERVER") && row.contains(SHIM_LINE)),
        "the log names the cause that the server wrote, not {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("ERROR SERVER") && row.contains("mock/mock failed:")),
        "the log names the failure of the server, not {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("WARN  SERVER") && row.contains("mock/mock restarted")),
        "the log names every restart, not {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("INFO  SERVER") && row.contains("mock/mock stopped")),
        "the log names the stop, not {rows:?}"
    );
}
