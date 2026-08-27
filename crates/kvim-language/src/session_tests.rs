//! Behavior tests for the language-server client over a deterministic mock
//! server.
//!
//! The mock server speaks the framing layer, so the tests cover the real
//! protocol path. No test starts a language server of the host system.

use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::time;

use kvim_core::{BufferVersion, CharRange, EditTransaction, TextBuffer, TextChange};
use kvim_lsp::{
    ContentChange, DocumentPosition, LSP_RESTARTS_MAX, LSP_STDERR_BYTES_MAX,
    LSP_STDERR_LINE_BYTES_MAX, LspBound, LspError, ServerReport, SourceSpan,
};
use kvim_settings::{EditorSettings, FileSettings};

use crate::document::{MarkupKind, content_changes};
use crate::markup::MarkupDocument;
use crate::mock::{
    self, DOCUMENT, DOCUMENT_URI, FULL_SYNC, Harness, INCREMENTAL_SYNC, MockServer, ROOT,
    TEST_DEADLINE, connected, pipe, session,
};
use crate::progress::{ProgressPercentage, ProgressReport, ProgressStage};
use crate::session::{
    LSP_DIAGNOSTIC_PULL_DELAY, LSP_FORMAT_EDITS_MAX, LSP_HOVER_BYTES_MAX, LSP_LOCATIONS_MAX,
    LSP_PENDING_REQUESTS_MAX, LSP_REQUEST_QUEUE_CAPACITY, LanguageOutcome, MarkupText,
    hover_contents,
};
use crate::{
    CommentStyle, DiagnosticSeverity, Grammar, IndentRule, IndentScope, LSP_CONTENT_CHANGES_MAX,
    LSP_DIAGNOSTICS_MAX, LSP_OPEN_DOCUMENTS_MAX, LanguageAdapter, LanguageCatalogEntry,
    LanguageRegistry, LanguageServerDeclaration, LanguageServerId, LanguageServices, RustAdapter,
    ServerFormatting, SessionGeneration, SyntaxHighlighter,
};

/// The node kind that a test adapter indents.
const TEST_INDENT_SCOPES: [IndentScope; 1] = [IndentScope::whole("block")];

/// The number of columns that one indent level takes in a test adapter.
///
/// The value matches the four-column default of the settings, so no test
/// measures a language convention of its own.
const TEST_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// Returns a buffer with the exact test document content.
fn buffer(text: &str) -> TextBuffer {
    TextBuffer::from_text(text, &FileSettings::default()).expect("the test text is small")
}

/// Opens the test document and returns its buffer.
async fn opened(harness: &Harness, server: &mut MockServer, text: &str) -> TextBuffer {
    let buffer = buffer(text);
    harness
        .handle()
        .open(Path::new(DOCUMENT), buffer.version(), Arc::from(text))
        .expect("the queue is empty");
    let notification = server.expect("textDocument/didOpen").await;
    assert_eq!(notification["params"]["textDocument"]["uri"], DOCUMENT_URI);
    assert_eq!(notification["params"]["textDocument"]["text"], text);
    buffer
}

/// Edits the test document and synchronizes the new buffer version.
async fn edited(
    harness: &Harness,
    server: &mut MockServer,
    text: &mut TextBuffer,
) -> BufferVersion {
    let cursor = text.char_position(0).expect("the position exists");
    let transaction = EditTransaction::single(cursor, TextChange::insert(cursor, "// note\n"));
    let before = text.clone();
    let changes = content_changes(&before, &transaction).expect("one change stays in bounds");
    let version = text
        .apply(transaction)
        .expect("the position fits the buffer");
    harness
        .handle()
        .change(Path::new(DOCUMENT), version, changes)
        .expect("the queue is empty");
    server.expect("textDocument/didChange").await;
    version
}

#[tokio::test]
async fn initializes_and_shuts_down_in_protocol_order() {
    let (mut harness, mut server) = connected();
    server.handshake().await;

    harness.stop();
    let shutdown = server.expect("shutdown").await;
    server.respond(&shutdown["id"], Value::Null).await;
    server.expect("exit").await;

    assert!(matches!(harness.next().await, LanguageOutcome::Stopped));
    harness.task.await.expect("the session task ends cleanly");
}

#[tokio::test]
async fn rejects_a_server_that_confirms_an_unknown_position_encoding() {
    let (mut harness, mut server) = connected();
    let initialize = server.expect("initialize").await;
    server
        .respond(
            &initialize["id"],
            json!({ "capabilities": { "positionEncoding": "utf-32" } }),
        )
        .await;

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::UnsupportedEncoding,
            ..
        }
    ));
}

/// The test document of every position-encoding test.
///
/// The first line holds one character above the Basic Multilingual Plane, so
/// its byte columns and its UTF-16 columns differ after that character.
const WIDE_DOCUMENT: &str = "let a = \"\u{1f600}\";\nlet b = 1;\n";

/// The byte column of the closing quotation mark of [`WIDE_DOCUMENT`].
const QUOTE_BYTE_COLUMN: u32 = 13;

/// The UTF-16 column of that same quotation mark.
const QUOTE_UTF16_COLUMN: u32 = 11;

/// The byte column and the UTF-16 column of the character before it agree.
const EMOJI_COLUMN: u32 = 9;

/// Starts one session whose server names no position encoding.
///
/// The protocol defines UTF-16 for that answer, so the session converts every
/// column. See `docs/language-services.md`.
async fn utf16_session() -> (Harness, MockServer) {
    let (harness, mut server) = connected();
    server.handshake_with(None).await;
    (harness, server)
}

#[tokio::test]
async fn a_utf16_session_converts_a_received_diagnostic_range() {
    let (mut harness, mut server) = utf16_session().await;
    let text = opened(&harness, &mut server, WIDE_DOCUMENT).await;

    server
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": DOCUMENT_URI,
                "diagnostics": [{
                    "range": { "start": { "line": 0, "character": EMOJI_COLUMN },
                               "end": { "line": 0, "character": QUOTE_UTF16_COLUMN } },
                    "message": "wide",
                }],
            }
        }))
        .await;

    let LanguageOutcome::Diagnostics(set) = harness.next().await else {
        panic!("the session publishes the diagnostics");
    };
    assert!(set.is_current(text.version()));
    assert_eq!(
        set.diagnostics()[0].span,
        SourceSpan::new(
            DocumentPosition::new(0, EMOJI_COLUMN),
            DocumentPosition::new(0, QUOTE_BYTE_COLUMN),
        ),
        "the range marks the wide character in byte columns"
    );
}

#[tokio::test]
async fn a_utf16_session_rejects_a_range_that_splits_a_character() {
    let (mut harness, mut server) = utf16_session().await;
    let _text = opened(&harness, &mut server, WIDE_DOCUMENT).await;

    server
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": DOCUMENT_URI,
                "diagnostics": [{
                    "range": { "start": { "line": 0, "character": EMOJI_COLUMN + 1 },
                               "end": { "line": 0, "character": QUOTE_UTF16_COLUMN } },
                    "message": "inside the surrogate pair",
                }],
            }
        }))
        .await;

    // kvim publishes no partial result, so one rejected position rejects the
    // complete set.
    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::InvalidPosition,
            ..
        }
    ));
}

#[tokio::test]
async fn a_utf16_session_sends_a_converted_position_and_change_range() {
    let (mut harness, mut server) = utf16_session().await;
    let mut text = opened(&harness, &mut server, WIDE_DOCUMENT).await;

    harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, QUOTE_BYTE_COLUMN),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/hover").await;
    assert_eq!(
        sent["params"]["position"],
        json!({ "line": 0, "character": QUOTE_UTF16_COLUMN })
    );
    server.respond(&sent["id"], Value::Null).await;
    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Hover { .. }
    ));

    // The insertion sits behind the wide character, so its column differs
    // between the two encodings.
    let cursor = text.char_position(10).expect("the position exists");
    let transaction = EditTransaction::single(cursor, TextChange::insert(cursor, "x"));
    let before = text.clone();
    let changes = content_changes(&before, &transaction).expect("one change stays in bounds");
    assert_eq!(changes[0].span.start.byte_column, QUOTE_BYTE_COLUMN);
    let version = text
        .apply(transaction)
        .expect("the position fits the buffer");
    harness
        .handle()
        .change(Path::new(DOCUMENT), version, changes)
        .expect("the queue is empty");
    let sent = server.expect("textDocument/didChange").await;
    assert_eq!(
        sent["params"]["contentChanges"][0]["range"],
        json!({
            "start": { "line": 0, "character": QUOTE_UTF16_COLUMN },
            "end": { "line": 0, "character": QUOTE_UTF16_COLUMN },
        })
    );

    // The mirror now holds the changed text, so the next conversion reads the
    // line that the server holds.
    harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            version,
            DocumentPosition::new(0, QUOTE_BYTE_COLUMN + 1),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/hover").await;
    assert_eq!(
        sent["params"]["position"],
        json!({ "line": 0, "character": QUOTE_UTF16_COLUMN + 1 })
    );
    server.respond(&sent["id"], Value::Null).await;
    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Hover { .. }
    ));
}

#[tokio::test]
async fn a_utf16_session_converts_a_definition_and_a_formatting_range() {
    let (mut harness, mut server) = utf16_session().await;
    let text = opened(&harness, &mut server, WIDE_DOCUMENT).await;
    let expected = SourceSpan::new(
        DocumentPosition::new(0, EMOJI_COLUMN),
        DocumentPosition::new(0, QUOTE_BYTE_COLUMN),
    );

    harness
        .handle()
        .definition(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/definition").await;
    server
        .respond(
            &sent["id"],
            json!([{
                "uri": DOCUMENT_URI,
                "range": { "start": { "line": 0, "character": EMOJI_COLUMN },
                           "end": { "line": 0, "character": QUOTE_UTF16_COLUMN } },
            }]),
        )
        .await;
    let LanguageOutcome::Definition { locations, .. } = harness.next().await else {
        panic!("the session answers the definition");
    };
    assert_eq!(locations[0].span, expected);

    harness
        .handle()
        .format(Path::new(DOCUMENT), text.version())
        .expect("the queue is empty");
    let sent = server.expect("textDocument/formatting").await;
    server
        .respond(
            &sent["id"],
            json!([{
                "range": { "start": { "line": 0, "character": EMOJI_COLUMN },
                           "end": { "line": 0, "character": QUOTE_UTF16_COLUMN } },
                "newText": "!",
            }]),
        )
        .await;
    let LanguageOutcome::Formatting { edits, .. } = harness.next().await else {
        panic!("the session answers the formatting");
    };
    assert_eq!(edits.edits()[0].span, expected);
}

#[tokio::test]
async fn synchronizes_open_change_and_close() {
    let (harness, mut server) = connected();
    server.handshake().await;
    let mut text = opened(&harness, &mut server, "fn main() {}\nlet x = 1;\n").await;

    let start = text.char_position(3).expect("the position exists");
    let end = text.char_position(7).expect("the position exists");
    let range = CharRange::new(start, end).expect("the range ascends");
    let transaction = EditTransaction::single(start, TextChange::replace(range, "run"));
    let before = text.clone();
    let changes =
        content_changes(&before, &transaction).expect("one change stays inside the bound");
    let version = text.apply(transaction).expect("the range fits the buffer");

    harness
        .handle()
        .change(Path::new(DOCUMENT), version, changes)
        .expect("the queue is empty");
    let notification = server.expect("textDocument/didChange").await;
    assert_eq!(notification["params"]["textDocument"]["version"], 2);
    let change = &notification["params"]["contentChanges"][0];
    assert_eq!(
        change["range"]["start"],
        json!({ "line": 0, "character": 3 })
    );
    assert_eq!(change["range"]["end"], json!({ "line": 0, "character": 7 }));
    assert_eq!(change["text"], "run");

    harness
        .handle()
        .close(Path::new(DOCUMENT))
        .expect("the queue is empty");
    let notification = server.expect("textDocument/didClose").await;
    assert_eq!(notification["params"]["textDocument"]["uri"], DOCUMENT_URI);
}

/// Applies one transaction to the buffer and synchronizes the new version.
///
/// The call returns the `didChange` notification that the session sent, so the
/// caller reads what the server receives.
async fn synchronized(
    harness: &Harness,
    server: &mut MockServer,
    text: &mut TextBuffer,
    transaction: EditTransaction,
) -> Value {
    let before = text.clone();
    let changes =
        content_changes(&before, &transaction).expect("the changes stay inside the bound");
    let version = text.apply(transaction).expect("the changes fit the buffer");
    harness
        .handle()
        .change(Path::new(DOCUMENT), version, changes)
        .expect("the queue is empty");
    server.expect("textDocument/didChange").await
}

/// Applies one received full change to the copy that the mock server holds.
///
/// A full change carries the complete text of the document and no range, so the
/// copy takes that text. The copy is then the exact text of the server, and the
/// caller compares it with the buffer.
fn applied_full_change(copy: &mut String, notification: &Value) {
    let changes = notification["params"]["contentChanges"]
        .as_array()
        .expect("the notification carries a change list");
    assert_eq!(changes.len(), 1, "one full change replaces the document");
    let change = &changes[0];
    assert!(
        change.get("range").is_none(),
        "a full change carries no range, but the session sent {change}"
    );
    *copy = change["text"]
        .as_str()
        .expect("the change carries the document text")
        .to_owned();
}

/// Returns the transaction that replaces the first `name` with `replacement`.
fn renaming(text: &TextBuffer, name: &str, replacement: &str) -> EditTransaction {
    let source = text.to_string();
    let offset = source.find(name).expect("the buffer holds the name");
    // Every character before the name is one byte, so the byte offset is also
    // the character offset.
    let start = text.char_position(offset).expect("the position exists");
    let end = text
        .char_position(offset + name.len())
        .expect("the position exists");
    let range = CharRange::new(start, end).expect("the range ascends");
    EditTransaction::single(start, TextChange::replace(range, replacement))
}

#[tokio::test]
async fn a_full_server_receives_the_complete_text_of_every_change() {
    let (harness, mut server) = connected();
    server
        .handshake_capabilities(json!({
            "positionEncoding": "utf-8",
            "textDocumentSync": FULL_SYNC,
        }))
        .await;
    let mut text = opened(&harness, &mut server, "fn main() {}\nlet x = 1;\n").await;
    // The mock applies every change that it receives, exactly as a server does,
    // so the test compares the text of the server with the text of the buffer.
    let mut copy = text.to_string();

    // One insertion at the start of the document.
    let start = text.char_position(0).expect("the position exists");
    let notification = synchronized(
        &harness,
        &mut server,
        &mut text,
        EditTransaction::single(start, TextChange::insert(start, "// note\n")),
    )
    .await;
    assert_eq!(notification["params"]["textDocument"]["version"], 2);
    applied_full_change(&mut copy, &notification);
    assert_eq!(copy, text.to_string(), "the copy holds the first edit");

    // One replacement inside a line.
    let transaction = renaming(&text, "main", "run");
    let notification = synchronized(&harness, &mut server, &mut text, transaction).await;
    assert_eq!(notification["params"]["textDocument"]["version"], 3);
    applied_full_change(&mut copy, &notification);
    assert_eq!(copy, text.to_string(), "the copy holds the replacement");

    // Two changes of one transaction, which the session sends in descending
    // order.
    let first = text.char_position(0).expect("the position exists");
    let second = text.char_position(3).expect("the position exists");
    let transaction = EditTransaction::new(
        first,
        vec![
            TextChange::insert(first, "> "),
            TextChange::insert(second, "< "),
        ],
    )
    .expect("the changes ascend");
    let notification = synchronized(&harness, &mut server, &mut text, transaction).await;
    applied_full_change(&mut copy, &notification);
    assert_eq!(copy, text.to_string(), "the copy holds both changes");

    // One insertion of text above the Basic Multilingual Plane, and one further
    // insertion after it, so the copy proves that no offset drifts.
    let start = text.char_position(0).expect("the position exists");
    let transaction = EditTransaction::single(start, TextChange::insert(start, "// \u{1f600}\n"));
    let notification = synchronized(&harness, &mut server, &mut text, transaction).await;
    applied_full_change(&mut copy, &notification);
    assert_eq!(copy, text.to_string(), "the copy holds the wide character");

    let transaction = renaming(&text, "let x", "let y");
    let notification = synchronized(&harness, &mut server, &mut text, transaction).await;
    assert_eq!(notification["params"]["textDocument"]["version"], 6);
    applied_full_change(&mut copy, &notification);
    assert_eq!(copy, text.to_string(), "the copy holds the last edit");
}

#[tokio::test]
async fn the_object_form_of_the_capability_selects_the_full_change() {
    let (harness, mut server) = connected();
    // `marksman` names a full synchronization in this exact shape, so the shape
    // is production traffic. See `docs/language-services.md`.
    server
        .handshake_capabilities(json!({
            "positionEncoding": "utf-8",
            "textDocumentSync": { "openClose": true, "change": FULL_SYNC },
        }))
        .await;
    let mut text = opened(&harness, &mut server, "fn main() {}\n").await;

    let transaction = renaming(&text, "main", "run");
    let notification = synchronized(&harness, &mut server, &mut text, transaction).await;

    assert_eq!(
        notification["params"]["contentChanges"],
        json!([{ "text": "fn run() {}\n" }])
    );
}

#[tokio::test]
async fn the_object_form_of_the_capability_selects_the_incremental_change() {
    let (harness, mut server) = connected();
    server
        .handshake_capabilities(json!({
            "positionEncoding": "utf-8",
            "textDocumentSync": { "openClose": true, "change": INCREMENTAL_SYNC },
        }))
        .await;
    let mut text = opened(&harness, &mut server, "fn main() {}\n").await;

    let transaction = renaming(&text, "main", "run");
    let notification = synchronized(&harness, &mut server, &mut text, transaction).await;

    // The incremental notification carries the range of the change and nothing
    // else, which is the exact shape that every incremental server receives.
    assert_eq!(
        notification["params"]["contentChanges"],
        json!([{
            "range": {
                "start": { "line": 0, "character": 3 },
                "end": { "line": 0, "character": 7 },
            },
            "text": "run",
        }])
    );
}

#[tokio::test]
async fn a_server_that_asks_for_no_synchronization_receives_no_change() {
    let (harness, mut server) = connected();
    // The result names no `textDocumentSync` capability. The protocol defines
    // that answer as no synchronization, so the session sends no `didChange`.
    server
        .handshake_capabilities(json!({ "positionEncoding": "utf-8" }))
        .await;
    let mut text = opened(&harness, &mut server, "fn main() {}\n").await;

    let transaction = renaming(&text, "main", "run");
    let before = text.clone();
    let changes =
        content_changes(&before, &transaction).expect("the changes stay inside the bound");
    let version = text.apply(transaction).expect("the changes fit the buffer");
    harness
        .handle()
        .change(Path::new(DOCUMENT), version, changes)
        .expect("the queue is empty");
    harness
        .handle()
        .close(Path::new(DOCUMENT))
        .expect("the queue is empty");

    // The close follows the change, so the next message proves that the change
    // sent nothing.
    let notification = server.expect("textDocument/didClose").await;
    assert_eq!(notification["params"]["textDocument"]["uri"], DOCUMENT_URI);
}

#[tokio::test]
async fn derives_descending_changes_from_one_transaction() {
    let text = buffer("ab\ncd\n");
    let first = text.char_position(0).expect("the position exists");
    let second = text.char_position(3).expect("the position exists");
    let transaction = EditTransaction::new(
        first,
        vec![
            TextChange::insert(first, "> "),
            TextChange::insert(second, "> "),
        ],
    )
    .expect("the changes ascend");

    let changes = content_changes(&text, &transaction).expect("two changes stay in bounds");

    // The protocol applies the changes in order, so the later change must come
    // first. Otherwise the second range would describe text that the first
    // change already moved.
    assert_eq!(
        changes[0].span,
        SourceSpan::new(DocumentPosition::new(1, 0), DocumentPosition::new(1, 0))
    );
    assert_eq!(
        changes[1].span,
        SourceSpan::new(DocumentPosition::new(0, 0), DocumentPosition::new(0, 0))
    );
}

#[tokio::test]
async fn publishes_diagnostics_in_position_order() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    server
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": DOCUMENT_URI,
                "version": 1,
                "diagnostics": [
                    { "range": { "start": { "line": 0, "character": 9 },
                                 "end": { "line": 0, "character": 11 } },
                      "severity": 2, "message": "late" },
                    { "range": { "start": { "line": 0, "character": 3 },
                                 "end": { "line": 0, "character": 7 } },
                      "severity": 1, "message": "early", "source": "mock" }
                ]
            }
        }))
        .await;

    let LanguageOutcome::Diagnostics(set) = harness.next().await else {
        panic!("the session publishes the diagnostics");
    };
    assert_eq!(set.path(), Path::new(DOCUMENT));
    assert!(set.is_current(text.version()));
    let messages: Vec<&str> = set
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert_eq!(messages, ["early", "late"]);
    assert_eq!(set.diagnostics()[0].severity, DiagnosticSeverity::Error);
    assert_eq!(set.diagnostics()[0].source, "mock");
    assert_eq!(set.diagnostics()[1].severity, DiagnosticSeverity::Warning);
    // The second diagnostic carries no `source` field, so the declaration
    // identifier of its session names the producer.
    assert_eq!(set.diagnostics()[1].source, mock::SERVER.server());
}

#[tokio::test]
async fn drops_diagnostics_of_an_obsolete_document_revision() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let mut text = opened(&harness, &mut server, "fn main() {}\n").await;

    // Revision 1 is obsolete once the buffer reaches revision 2.
    let current = edited(&harness, &mut server, &mut text).await;
    server
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": DOCUMENT_URI, "version": 1, "diagnostics": [] }
        }))
        .await;
    server
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": DOCUMENT_URI, "version": 2, "diagnostics": [] }
        }))
        .await;

    let LanguageOutcome::Diagnostics(set) = harness.next().await else {
        panic!("the session publishes the current diagnostics");
    };
    // Only the current revision reaches the editor, so the obsolete set never
    // becomes visible.
    assert!(set.is_current(current));
}

#[tokio::test]
async fn answers_a_definition_inside_the_workspace() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    let request = harness
        .handle()
        .definition(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 3),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/definition").await;
    assert_eq!(
        sent["params"]["position"],
        json!({ "line": 0, "character": 3 })
    );
    server
        .respond(
            &sent["id"],
            json!([
                { "uri": DOCUMENT_URI,
                  "range": { "start": { "line": 0, "character": 3 },
                             "end": { "line": 0, "character": 7 } } },
                { "uri": "file:///etc/passwd",
                  "range": { "start": { "line": 0, "character": 0 },
                             "end": { "line": 0, "character": 1 } } }
            ]),
        )
        .await;

    let LanguageOutcome::Definition {
        request: answered,
        version,
        locations,
    } = harness.next().await
    else {
        panic!("the session answers the definition");
    };
    assert_eq!(answered, request);
    assert_eq!(version, text.version());
    // The target outside the workspace root never reaches the editor.
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].path, Path::new(DOCUMENT));
}

#[tokio::test]
async fn answers_a_hover() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 3),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/hover").await;
    server
        .respond(
            &sent["id"],
            json!({ "contents": { "kind": "markdown", "value": "fn main()" } }),
        )
        .await;

    let LanguageOutcome::Hover { markup, .. } = harness.next().await else {
        panic!("the session answers the hover");
    };
    let markup = markup.expect("the server described the symbol");
    assert_eq!(markup.text, "fn main()");
    assert_eq!(
        markup.kind,
        MarkupKind::Markdown,
        "the answer names its markup, and the session carries that name"
    );
}

#[tokio::test]
async fn applies_formatting_as_one_undoable_transaction() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let mut text = opened(&harness, &mut server, "fn  main() {}\n").await;

    harness
        .handle()
        .format(Path::new(DOCUMENT), text.version())
        .expect("the queue is empty");
    let sent = server.expect("textDocument/formatting").await;
    // The mock server serves no adapter, so the request carries the settings
    // width. See `a_formatting_request_carries_the_indent_width_of_its_language`
    // for a language that declares its own width.
    assert_eq!(sent["params"]["options"]["tabSize"], 4);
    assert_eq!(sent["params"]["options"]["insertSpaces"], true);
    server
        .respond(
            &sent["id"],
            json!([{
                "range": { "start": { "line": 0, "character": 2 },
                           "end": { "line": 0, "character": 4 } },
                "newText": " "
            }]),
        )
        .await;

    let LanguageOutcome::Formatting { edits, .. } = harness.next().await else {
        panic!("the session answers the formatting");
    };
    let cursor = text.char_position(0).expect("the position exists");
    let transaction = edits
        .transaction(&text, cursor)
        .expect("the version still matches")
        .expect("the formatter changes the buffer");
    text.apply(transaction).expect("the range fits the buffer");
    assert_eq!(text.to_string(), "fn main() {}\n");

    // One undo reverses the complete format.
    text.undo().expect("the transaction is undoable");
    assert_eq!(text.to_string(), "fn  main() {}\n");
}

#[tokio::test]
async fn a_formatting_request_carries_the_indent_width_of_its_language() {
    // The session serves a language that indents with two columns, while the
    // settings tab width stays at four.
    let columns = NonZeroU8::new(2).expect("the literal 2 is not zero");
    let (harness, mut server) = mock::connected_with_indent_columns(columns);
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn  main() {}\n").await;

    harness
        .handle()
        .format(Path::new(DOCUMENT), text.version())
        .expect("the queue is empty");
    let sent = server.expect("textDocument/formatting").await;
    assert_eq!(sent["params"]["options"]["tabSize"], 2);
    assert_eq!(
        sent["params"]["options"]["insertSpaces"], true,
        "the language declares the width alone, so the settings keep the tab rule"
    );
}

#[test]
fn formatting_edits_reject_an_obsolete_buffer_version() {
    let mut text = buffer("fn  main() {}\n");
    let version = text.version();
    let edits = crate::FormatEdits::new(
        PathBuf::from(DOCUMENT),
        version,
        vec![crate::TextEdit {
            span: SourceSpan::new(DocumentPosition::new(0, 2), DocumentPosition::new(0, 4)),
            text: " ".to_owned(),
        }],
    );
    let cursor = text.char_position(0).expect("the position exists");
    let insert = TextChange::insert(cursor, "//\n");
    text.apply(EditTransaction::single(cursor, insert))
        .expect("the position fits the buffer");

    let error = edits
        .transaction(&text, cursor)
        .expect_err("the buffer changed after the request");
    assert!(matches!(error, LspError::StaleVersion));
}

#[test]
fn formatting_edits_reject_a_range_outside_the_buffer() {
    let text = buffer("fn main() {}\n");
    let edits = crate::FormatEdits::new(
        PathBuf::from(DOCUMENT),
        text.version(),
        vec![crate::TextEdit {
            span: SourceSpan::new(DocumentPosition::new(0, 2), DocumentPosition::new(0, 200)),
            text: " ".to_owned(),
        }],
    );
    let cursor = text.char_position(0).expect("the position exists");

    let error = edits
        .transaction(&text, cursor)
        .expect_err("the column passes the end of its line");
    assert!(matches!(error, LspError::MalformedResponse));
}

#[tokio::test]
async fn rejects_a_query_for_an_obsolete_buffer_version() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let mut text = opened(&harness, &mut server, "fn main() {}\n").await;
    let stale = text.version();

    edited(&harness, &mut server, &mut text).await;
    harness
        .handle()
        .hover(Path::new(DOCUMENT), stale, DocumentPosition::new(0, 0))
        .expect("the queue is empty");

    // The session never sends a request that describes obsolete content.
    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::StaleVersion,
            ..
        }
    ));
}

#[tokio::test]
async fn rejects_an_answer_for_a_buffer_that_changed_meanwhile() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let mut text = opened(&harness, &mut server, "fn main() {}\n").await;

    harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/hover").await;
    edited(&harness, &mut server, &mut text).await;
    server
        .respond(&sent["id"], json!({ "contents": "obsolete" }))
        .await;

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::StaleVersion,
            ..
        }
    ));
}

#[tokio::test]
async fn reports_a_malformed_answer_without_stopping_the_session() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    harness
        .handle()
        .definition(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/definition").await;
    server.respond(&sent["id"], json!(42)).await;
    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::MalformedResponse,
            ..
        }
    ));

    // The session still serves the next request.
    harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/hover").await;
    server
        .respond(&sent["id"], json!({ "contents": "ok" }))
        .await;
    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Hover { .. }
    ));
}

#[tokio::test]
async fn reports_a_protocol_error_code() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/hover").await;
    server
        .send(&json!({
            "jsonrpc": "2.0",
            "id": sent["id"],
            "error": { "code": -32603, "message": "internal" }
        }))
        .await;

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::Response { code: -32603 },
            ..
        }
    ));
}

/// The test waits for the real request deadline, because the workspace does
/// not enable the tokio clock-control feature.
#[tokio::test]
async fn reports_a_timeout_and_withdraws_the_request() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    let request = harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/hover").await;

    // The server never answers, so the request deadline expires.
    let cancel = server.expect("$/cancelRequest").await;
    assert_eq!(cancel["params"]["id"], sent["id"]);
    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            request: Some(failed),
            error: LspError::Timeout,
        } if failed == request
    ));
}

#[tokio::test]
async fn cancellation_stops_the_session() {
    let (mut harness, mut server) = connected();
    server.handshake().await;

    harness.handle().cancel();

    assert!(matches!(harness.next().await, LanguageOutcome::Stopped));
    harness.task.await.expect("the session task ends cleanly");
}

#[tokio::test]
async fn answers_an_unsolicited_server_request() {
    let (harness, mut server) = connected();
    server.handshake().await;

    server
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 91,
            "method": "workspace/applyEdit",
            "params": { "edit": {} }
        }))
        .await;

    // An unanswered server request stalls the server, so the session always
    // answers. kvim implements no such request, so it reports the method as
    // unknown.
    let answer = server.read_message().await;
    assert_eq!(answer["id"], 91);
    assert_eq!(answer["error"]["code"], -32601);
    drop(harness);
}

#[tokio::test]
async fn restarts_a_bounded_number_of_times() {
    let mut transports = Vec::new();
    let mut servers = Vec::new();
    for _ in 0..=LSP_RESTARTS_MAX {
        let (transport, server) = pipe();
        transports.push(transport);
        servers.push(server);
    }
    let mut harness = session(transports, true);
    // Every mock server closes its streams, so every attempt fails.
    servers.clear();

    let mut failures = 0;
    let mut restarts = 0;
    loop {
        match harness.next().await {
            LanguageOutcome::Failed { .. } => failures += 1,
            LanguageOutcome::Restarted => restarts += 1,
            LanguageOutcome::Stopped => break,
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    assert_eq!(restarts, LSP_RESTARTS_MAX);
    assert_eq!(failures, LSP_RESTARTS_MAX + 1);
    harness.task.await.expect("the session task ends cleanly");
}

#[tokio::test]
async fn restarts_and_serves_the_reopened_document() {
    let (first_transport, first_server) = pipe();
    let (second_transport, mut second_server) = pipe();
    let mut harness = session(vec![first_transport, second_transport], true);
    let mut first_server = first_server;
    first_server.handshake().await;
    drop(first_server);

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed { .. }
    ));
    assert!(matches!(harness.next().await, LanguageOutcome::Restarted));

    // The new server holds no document, so the editor opens its buffer again.
    second_server.handshake().await;
    let text = opened(&harness, &mut second_server, "fn main() {}\n").await;
    harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = second_server.expect("textDocument/hover").await;
    second_server
        .respond(&sent["id"], json!({ "contents": "restarted" }))
        .await;

    let LanguageOutcome::Hover { markup, .. } = harness.next().await else {
        panic!("the restarted session answers the hover");
    };
    assert_eq!(
        markup.map(|markup| markup.text).as_deref(),
        Some("restarted")
    );
}

#[tokio::test]
async fn rejects_a_document_outside_the_workspace_root() {
    let (mut harness, mut server) = connected();
    server.handshake().await;

    harness
        .handle()
        .open(
            Path::new("/etc/passwd"),
            buffer("root\n").version(),
            Arc::from("root\n"),
        )
        .expect("the queue is empty");

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::PathEscape,
            ..
        }
    ));
}

#[tokio::test]
async fn rejects_more_open_documents_than_the_bound_allows() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    for index in 0..=LSP_OPEN_DOCUMENTS_MAX {
        let path = PathBuf::from(format!("{ROOT}/src/file{index}.rs"));
        harness
            .handle()
            .open(&path, buffer("\n").version(), Arc::from("\n"))
            .expect("the queue holds one request");
        if index < LSP_OPEN_DOCUMENTS_MAX {
            server.expect("textDocument/didOpen").await;
        }
    }

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::Bounds {
                measure: LspBound::OpenDocuments,
                limit: LSP_OPEN_DOCUMENTS_MAX,
                ..
            },
            ..
        }
    ));
}

#[tokio::test]
async fn rejects_more_pending_requests_than_the_bound_allows() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    for index in 0..=LSP_PENDING_REQUESTS_MAX {
        harness
            .handle()
            .hover(
                Path::new(DOCUMENT),
                text.version(),
                DocumentPosition::new(0, 0),
            )
            .expect("the queue holds one request");
        if index < LSP_PENDING_REQUESTS_MAX {
            server.expect("textDocument/hover").await;
        }
    }

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::Bounds {
                measure: LspBound::PendingRequests,
                limit: LSP_PENDING_REQUESTS_MAX,
                ..
            },
            ..
        }
    ));
}

#[tokio::test]
async fn rejects_a_full_request_queue_without_waiting() {
    let (transport, mut server) = pipe();
    let harness = session(vec![transport], true);
    // The session waits for the handshake, so it reads no editor request yet.
    server.expect("initialize").await;

    for _ in 0..LSP_REQUEST_QUEUE_CAPACITY {
        harness
            .handle()
            .close(Path::new(DOCUMENT))
            .expect("the queue holds the request");
    }
    let error = harness
        .handle()
        .close(Path::new(DOCUMENT))
        .expect_err("the queue is full");

    assert!(matches!(error, LspError::Saturated));
}

#[tokio::test]
async fn rejects_more_diagnostics_than_the_bound_allows() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    opened(&harness, &mut server, "fn main() {}\n").await;

    let diagnostic = json!({
        "range": { "start": { "line": 0, "character": 0 },
                   "end": { "line": 0, "character": 1 } },
        "message": "too many"
    });
    let diagnostics = vec![diagnostic; LSP_DIAGNOSTICS_MAX + 1];
    server
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": DOCUMENT_URI, "version": 1, "diagnostics": diagnostics }
        }))
        .await;

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::Bounds {
                measure: LspBound::Diagnostics,
                ..
            },
            ..
        }
    ));
}

#[tokio::test]
async fn rejects_more_definition_locations_than_the_bound_allows() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    harness
        .handle()
        .definition(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/definition").await;
    let location = json!({
        "uri": DOCUMENT_URI,
        "range": { "start": { "line": 0, "character": 0 },
                   "end": { "line": 0, "character": 1 } }
    });
    server
        .respond(&sent["id"], json!(vec![location; LSP_LOCATIONS_MAX + 1]))
        .await;

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::Bounds {
                measure: LspBound::Locations,
                ..
            },
            ..
        }
    ));
}

#[tokio::test]
async fn rejects_more_format_edits_than_the_bound_allows() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    harness
        .handle()
        .format(Path::new(DOCUMENT), text.version())
        .expect("the queue is empty");
    let sent = server.expect("textDocument/formatting").await;
    let edit = json!({
        "range": { "start": { "line": 0, "character": 0 },
                   "end": { "line": 0, "character": 0 } },
        "newText": ""
    });
    server
        .respond(&sent["id"], json!(vec![edit; LSP_FORMAT_EDITS_MAX + 1]))
        .await;

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::Bounds {
                measure: LspBound::FormatEdits,
                ..
            },
            ..
        }
    ));
}

#[tokio::test]
async fn rejects_a_hover_text_above_its_bound() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/hover").await;
    server
        .respond(
            &sent["id"],
            json!({ "contents": "h".repeat(LSP_HOVER_BYTES_MAX + 1) }),
        )
        .await;

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::Bounds {
                measure: LspBound::HoverBytes,
                ..
            },
            ..
        }
    ));
}

#[tokio::test]
async fn rejects_more_content_changes_than_the_bound_allows() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    let change = ContentChange {
        span: SourceSpan::new(DocumentPosition::new(0, 0), DocumentPosition::new(0, 0)),
        text: String::new(),
    };
    harness
        .handle()
        .change(
            Path::new(DOCUMENT),
            text.version(),
            vec![change; LSP_CONTENT_CHANGES_MAX + 1],
        )
        .expect("the queue is empty");

    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed {
            error: LspError::Bounds {
                measure: LspBound::ContentChanges,
                ..
            },
            ..
        }
    ));
}

/// Returns the grammar that every test language reuses.
///
/// The tests exercise the session, not a grammar, so each one borrows
/// the bundled Rust grammar instead of adding a second one.
fn test_grammar() -> Grammar {
    RustAdapter::new().grammar()
}

/// The extensions of the serverless test language.
static SERVERLESS_EXTENSIONS: [&str; 1] = ["kv"];

/// The catalog entry of the serverless test language.
static SERVERLESS_CATALOG: LanguageCatalogEntry =
    LanguageCatalogEntry::new("serverless", &[], &SERVERLESS_EXTENSIONS, &[], test_grammar);

/// The extensions of the two test language.
static TWO_EXTENSIONS: [&str; 1] = ["two"];

/// The catalog entry of the two test language.
static TWO_CATALOG: LanguageCatalogEntry =
    LanguageCatalogEntry::new("two", &[], &TWO_EXTENSIONS, &[], test_grammar);

/// The extensions of the gate test language.
static GATE_EXTENSIONS: [&str; 1] = ["gate"];

/// The catalog entry of the gate test language.
static GATE_CATALOG: LanguageCatalogEntry =
    LanguageCatalogEntry::new("gate", &[], &GATE_EXTENSIONS, &[], test_grammar);

/// The extensions of the unused test language.
static UNUSED_EXTENSIONS: [&str; 1] = ["unused"];

/// The catalog entry of the unused test language.
static UNUSED_CATALOG: LanguageCatalogEntry =
    LanguageCatalogEntry::new("unused", &[], &UNUSED_EXTENSIONS, &[], test_grammar);

/// One adapter that serves a language without a language server.
#[derive(Clone, Copy, Debug)]
struct ServerlessAdapter;

impl LanguageAdapter for ServerlessAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        &SERVERLESS_CATALOG
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("#"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &TEST_INDENT_SCOPES,
            width: TEST_INDENT_WIDTH,
            closing_delimiters: &['}'],
        }
    }
}

static SERVERLESS: ServerlessAdapter = ServerlessAdapter;

/// One registry whose only language declares no server.
static SERVERLESS_REGISTRY: [&dyn LanguageAdapter; 1] = [&SERVERLESS];

#[test]
fn a_language_without_a_server_leaves_the_editor_usable() {
    let mut services = LanguageServices::new(
        LanguageRegistry::new(&SERVERLESS_REGISTRY).expect("the test registry is valid"),
        PathBuf::from(ROOT),
        EditorSettings::default(),
    )
    .expect("the root is absolute");

    // Neither path starts a process, and neither failure stops the editor.
    assert!(matches!(
        services.sessions(Path::new("/workspace/notes.kv")),
        Err(LspError::NoServerDeclared)
    ));
    assert!(matches!(
        services.sessions(Path::new("/workspace/notes.txt")),
        Err(LspError::UnsupportedPath)
    ));
    assert!(services.try_recv().is_none());
}

/// The adapter that declares two servers, neither of which is installed.
///
/// The two programs carry a name that no host provides, so every start reports
/// [`LanguageOutcome::Unavailable`] without running a program of the host
/// system.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TwoServerAdapter;

/// The two servers of [`TwoServerAdapter`], in declaration order.
static TWO_SERVERS: [LanguageServerDeclaration; 2] = [
    LanguageServerDeclaration {
        id: "first",
        program: "kvim-absent-language-server-first",
        args: &[],
        language_id: "two",
        formatting: ServerFormatting::Enabled,
        root_markers: &[],
        initialization_options: no_options,
        workspace_settings: None,
    },
    LanguageServerDeclaration {
        id: "second",
        program: "kvim-absent-language-server-second",
        args: &[],
        language_id: "two",
        formatting: ServerFormatting::Disabled,
        root_markers: &[],
        initialization_options: no_options,
        workspace_settings: None,
    },
];

/// Returns the empty initialization options of a test declaration.
fn no_options(_settings: kvim_settings::LanguageSettings) -> Value {
    json!({})
}

impl LanguageAdapter for TwoServerAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        &TWO_CATALOG
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("#"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &TEST_INDENT_SCOPES,
            width: TEST_INDENT_WIDTH,
            closing_delimiters: &['}'],
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &TWO_SERVERS
    }
}

static TWO_SERVER: TwoServerAdapter = TwoServerAdapter;

/// One registry whose only language declares two servers.
static TWO_SERVER_REGISTRY: [&dyn LanguageAdapter; 1] = [&TWO_SERVER];

#[tokio::test]
async fn one_failing_server_leaves_the_other_server_of_the_language_running() {
    let mut services = LanguageServices::new(
        LanguageRegistry::new(&TWO_SERVER_REGISTRY).expect("the test registry is valid"),
        PathBuf::from(ROOT),
        EditorSettings::default(),
    )
    .expect("the root is absolute");
    let path = Path::new("/workspace/notes.two");

    // One path starts one session for each declaration, in declaration order.
    let ids: Vec<LanguageServerId> = services
        .sessions(path)
        .expect("both sessions start")
        .iter()
        .map(|handle| handle.id())
        .collect();
    assert_eq!(
        ids,
        [
            LanguageServerId::new("two", 0, "first"),
            LanguageServerId::new("two", 1, "second"),
        ]
    );

    // The first server proves missing. Only its own session becomes
    // unavailable, so the language keeps the second server.
    let first = next_unavailable(&mut services).await;
    let running: Vec<LanguageServerId> = services
        .sessions(path)
        .expect("the second session still serves the path")
        .iter()
        .map(|handle| handle.id())
        .collect();
    assert_eq!(running.len(), 1);
    assert_ne!(running[0], first);

    // Only after every server proves missing does the language lose its
    // service, and no restart of the recorded servers follows.
    next_unavailable(&mut services).await;
    assert!(matches!(
        services.sessions(path),
        Err(LspError::NotInstalled)
    ));
}

/// Waits for the next server that reports that it is not installed.
async fn next_unavailable(services: &mut LanguageServices) -> LanguageServerId {
    loop {
        let event = time::timeout(TEST_DEADLINE, services.recv())
            .await
            .expect("a missing server reports its state before the test deadline")
            .expect("the result queue stays open");
        if matches!(event.outcome, LanguageOutcome::Unavailable) {
            return event.server;
        }
    }
}

/// The workspace root of the root-marker tests.
///
/// The directory of this crate is a real workspace root. Every build of this
/// test holds its `Cargo.toml` file and its `src` directory, so one root proves
/// the file shape and the directory shape of a marker.
const MARKER_ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// One marker name that no workspace root of this repository holds.
const ABSENT_MARKER: &str = "kvim-absent-root-marker";

/// The adapter that covers every root-marker case in one table.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct GatedAdapter;

/// The four servers of [`GatedAdapter`], in declaration order.
static GATED_SERVERS: [LanguageServerDeclaration; 4] = [
    LanguageServerDeclaration {
        id: "file_marker",
        program: "kvim-absent-language-server-file",
        args: &[],
        language_id: "gate",
        formatting: ServerFormatting::Enabled,
        root_markers: &["Cargo.toml"],
        initialization_options: no_options,
        workspace_settings: None,
    },
    LanguageServerDeclaration {
        id: "absent_marker",
        program: "kvim-absent-language-server-absent",
        args: &[],
        language_id: "gate",
        formatting: ServerFormatting::Disabled,
        root_markers: &[ABSENT_MARKER],
        initialization_options: no_options,
        workspace_settings: None,
    },
    LanguageServerDeclaration {
        id: "directory_marker",
        program: "kvim-absent-language-server-directory",
        args: &[],
        language_id: "gate",
        formatting: ServerFormatting::Disabled,
        root_markers: &["src"],
        initialization_options: no_options,
        workspace_settings: None,
    },
    LanguageServerDeclaration {
        id: "no_marker",
        program: "kvim-absent-language-server-none",
        args: &[],
        language_id: "gate",
        formatting: ServerFormatting::Disabled,
        root_markers: &[],
        initialization_options: no_options,
        workspace_settings: None,
    },
];

impl LanguageAdapter for GatedAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        &GATE_CATALOG
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("#"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &TEST_INDENT_SCOPES,
            width: TEST_INDENT_WIDTH,
            closing_delimiters: &['}'],
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &GATED_SERVERS
    }
}

static GATED: GatedAdapter = GatedAdapter;

/// One registry whose only language mixes gated and ungated servers.
static GATED_REGISTRY: [&dyn LanguageAdapter; 1] = [&GATED];

/// The adapter whose one server no workspace of this repository uses.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct UnusedAdapter;

/// The one server of [`UnusedAdapter`], which every workspace gates off.
static UNUSED_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "unused",
    program: "kvim-absent-language-server-unused",
    args: &[],
    language_id: "unused",
    formatting: ServerFormatting::Enabled,
    root_markers: &[ABSENT_MARKER],
    initialization_options: no_options,
    workspace_settings: None,
}];

impl LanguageAdapter for UnusedAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        &UNUSED_CATALOG
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("#"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &TEST_INDENT_SCOPES,
            width: TEST_INDENT_WIDTH,
            closing_delimiters: &['}'],
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &UNUSED_SERVERS
    }
}

static UNUSED: UnusedAdapter = UnusedAdapter;

/// One registry whose only language declares one gated server.
static UNUSED_REGISTRY: [&dyn LanguageAdapter; 1] = [&UNUSED];

#[tokio::test]
async fn a_server_starts_only_when_the_workspace_holds_one_of_its_root_markers() {
    let root = PathBuf::from(MARKER_ROOT);
    let mut services = LanguageServices::new(
        LanguageRegistry::new(&GATED_REGISTRY).expect("the test registry is valid"),
        root.clone(),
        EditorSettings::default(),
    )
    .expect("the root is absolute");

    let ids: Vec<LanguageServerId> = services
        .sessions(&root.join("notes.gate"))
        .expect("the workspace uses three of the four declared servers")
        .iter()
        .map(|handle| handle.id())
        .collect();

    // A file marker, a directory marker, and an empty marker table each start
    // their server. The one server whose marker the root does not hold never
    // starts, and every server that starts keeps its own declaration order.
    assert_eq!(
        ids,
        [
            LanguageServerId::new("gate", 0, "file_marker"),
            LanguageServerId::new("gate", 2, "directory_marker"),
            LanguageServerId::new("gate", 3, "no_marker"),
        ]
    );

    // The gated server holds no session, so it takes no part of the session
    // budget that `LSP_SESSIONS_MAX` bounds.
    assert_eq!(services.session_count(), 3);
}

#[test]
fn a_workspace_without_the_root_marker_starts_no_server_of_its_language() {
    let root = PathBuf::from(MARKER_ROOT);
    let mut services = LanguageServices::new(
        LanguageRegistry::new(&UNUSED_REGISTRY).expect("the test registry is valid"),
        root.clone(),
        EditorSettings::default(),
    )
    .expect("the root is absolute");

    // The workspace uses the one declared server of this language nowhere, so
    // the path starts no process and reports a normal state.
    assert!(matches!(
        services.sessions(&root.join("notes.unused")),
        Err(LspError::UnusedInWorkspace)
    ));
    assert_eq!(services.session_count(), 0);
    assert!(services.try_recv().is_none());
}

/// Sends one `$/progress` notification from the mock server.
async fn send_progress(server: &mut MockServer, token: &Value, value: Value) {
    server
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": { "token": token, "value": value },
        }))
        .await;
}

/// Waits for the next progress report of the session.
async fn next_progress(harness: &mut Harness) -> ProgressReport {
    match harness.next().await {
        LanguageOutcome::Progress(report) => report,
        other => panic!("unexpected outcome {other:?}"),
    }
}

#[tokio::test]
async fn declares_work_done_progress_and_accepts_the_token_creation() {
    let (mut harness, mut server) = connected();
    let initialize = server.expect("initialize").await;
    // A server sends no progress before the client declares the capability.
    assert_eq!(
        initialize["params"]["capabilities"]["window"]["workDoneProgress"],
        true
    );
    server
        .respond(
            &initialize["id"],
            json!({ "capabilities": { "positionEncoding": "utf-8" } }),
        )
        .await;
    server.expect("initialized").await;

    server
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 91,
            "method": "window/workDoneProgress/create",
            "params": { "token": "index" },
        }))
        .await;
    let answer = server.read_message().await;
    assert_eq!(answer["id"], 91);
    assert_eq!(answer["result"], Value::Null);
    assert!(
        answer.get("error").is_none(),
        "the client accepts the token"
    );
    harness.stop();
}

#[tokio::test]
async fn publishes_the_begin_report_and_end_of_one_progress_token() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let token = json!("rustAnalyzer/Indexing");

    send_progress(
        &mut server,
        &token,
        json!({ "kind": "begin", "title": "Indexing", "message": "start" }),
    )
    .await;
    let begin = next_progress(&mut harness).await;
    assert_eq!(begin.token.get(), "rustAnalyzer/Indexing");
    assert_eq!(begin.server, "mock-server");
    assert_eq!(begin.generation, SessionGeneration::FIRST);
    assert_eq!(
        begin.stage,
        ProgressStage::Begin {
            title: "Indexing".to_owned(),
            message: Some("start".to_owned()),
            percentage: None,
        }
    );

    send_progress(
        &mut server,
        &token,
        json!({ "kind": "report", "message": "Building compile-time-deps" }),
    )
    .await;
    assert_eq!(
        next_progress(&mut harness).await.stage,
        ProgressStage::Report {
            message: Some("Building compile-time-deps".to_owned()),
            percentage: None,
        }
    );

    send_progress(&mut server, &token, json!({ "kind": "end" })).await;
    assert_eq!(
        next_progress(&mut harness).await.stage,
        ProgressStage::End { message: None }
    );
}

#[tokio::test]
async fn publishes_the_reported_percentage_and_drops_one_outside_its_range() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    // An integer token reaches the same identity as a string token.
    let token = json!(7);

    send_progress(
        &mut server,
        &token,
        json!({ "kind": "begin", "title": "Indexing", "percentage": 42 }),
    )
    .await;
    let begin = next_progress(&mut harness).await;
    assert_eq!(begin.token.get(), "7");
    assert_eq!(
        begin.stage,
        ProgressStage::Begin {
            title: "Indexing".to_owned(),
            message: None,
            percentage: ProgressPercentage::new(42),
        }
    );

    send_progress(
        &mut server,
        &token,
        json!({ "kind": "report", "percentage": 250 }),
    )
    .await;
    assert_eq!(
        next_progress(&mut harness).await.stage,
        ProgressStage::Report {
            message: None,
            percentage: None,
        },
        "a percentage outside the protocol range reports no completion"
    );
}

#[tokio::test]
async fn a_server_that_sends_no_progress_publishes_no_report() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;
    harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/hover").await;
    server
        .respond(&sent["id"], json!({ "contents": "quiet" }))
        .await;

    // The hover answer is the first outcome, so no progress preceded it.
    let LanguageOutcome::Hover { markup, .. } = harness.next().await else {
        panic!("the session answers the hover without any progress report");
    };
    assert_eq!(markup.map(|markup| markup.text).as_deref(), Some("quiet"));
}

#[tokio::test]
async fn a_progress_value_that_carries_no_stage_reports_nothing_and_never_fails() {
    let (mut harness, mut server) = connected();
    server.handshake().await;
    let text = opened(&harness, &mut server, "fn main() {}\n").await;

    // The same method carries the partial results of a request, whose value
    // holds no work-done stage. Progress is decoration, so the session drops
    // every unreadable report instead of reporting a failure.
    send_progress(
        &mut server,
        &json!("partial"),
        json!([{ "uri": "file:///x" }]),
    )
    .await;
    send_progress(&mut server, &json!("partial"), json!({ "kind": "future" })).await;

    harness
        .handle()
        .hover(
            Path::new(DOCUMENT),
            text.version(),
            DocumentPosition::new(0, 0),
        )
        .expect("the queue is empty");
    let sent = server.expect("textDocument/hover").await;
    server
        .respond(&sent["id"], json!({ "contents": "still alive" }))
        .await;

    let LanguageOutcome::Hover { markup, .. } = harness.next().await else {
        panic!("the session drops the unreadable reports and answers the hover");
    };
    assert_eq!(
        markup.map(|markup| markup.text).as_deref(),
        Some("still alive")
    );
}

#[tokio::test]
async fn a_restart_during_progress_reports_a_later_generation() {
    let (first_transport, first_server) = pipe();
    let (second_transport, mut second_server) = pipe();
    let mut harness = session(vec![first_transport, second_transport], true);
    let mut first_server = first_server;
    first_server.handshake().await;
    send_progress(
        &mut first_server,
        &json!("index"),
        json!({ "kind": "begin", "title": "Indexing" }),
    )
    .await;
    let before = next_progress(&mut harness).await;
    assert_eq!(before.generation, SessionGeneration::FIRST);

    // The server ends while the operation still runs.
    drop(first_server);
    assert!(matches!(
        harness.next().await,
        LanguageOutcome::Failed { .. }
    ));
    assert!(matches!(harness.next().await, LanguageOutcome::Restarted));

    second_server.handshake().await;
    send_progress(
        &mut second_server,
        &json!("index"),
        json!({ "kind": "begin", "title": "Indexing" }),
    )
    .await;
    let after = next_progress(&mut harness).await;
    // The new attempt assigns its own tokens, so the editor drops every report
    // of the attempt that failed.
    assert_eq!(after.generation, SessionGeneration::FIRST.next());
    assert!(after.generation > before.generation);
}

/// Opens the test document on a pull session and reads the first pull.
///
/// A pull session asks for a new document at once, so the request follows the
/// `didOpen` notification. See `docs/language-services.md`.
async fn pull_opened(
    harness: &Harness,
    server: &mut MockServer,
    text: &str,
) -> (TextBuffer, Value) {
    let buffer = opened(harness, server, text).await;
    let pull = server.expect("textDocument/diagnostic").await;
    (buffer, pull)
}

/// One diagnostic of the test document with one message.
fn pulled_item(message: &str) -> Value {
    json!({
        "range": { "start": { "line": 0, "character": 3 },
                   "end": { "line": 0, "character": 7 } },
        "severity": 1,
        "message": message,
    })
}

#[tokio::test]
async fn pulls_the_diagnostics_of_a_server_that_publishes_none() {
    let (mut harness, mut server) = connected();
    server.handshake_pulling("mock-lint").await;
    let (text, pull) = pull_opened(&harness, &mut server, "fn main() {}\n").await;

    assert_eq!(pull["params"]["textDocument"]["uri"], DOCUMENT_URI);
    // The request repeats the provider identifier of the capability, and it
    // carries no previous result identifier for a new document.
    assert_eq!(pull["params"]["identifier"], "mock-lint");
    assert_eq!(pull["params"]["previousResultId"], Value::Null);
    server
        .respond(
            &pull["id"],
            json!({
                "kind": "full",
                "resultId": "first",
                "items": [pulled_item("pulled")],
                // The session ignores a related report, because it asks for
                // each open document on its own.
                "relatedDocuments": {
                    "file:///workspace/src/other.rs": { "kind": "full", "items": [] }
                },
            }),
        )
        .await;

    let LanguageOutcome::Diagnostics(set) = harness.next().await else {
        panic!("the session publishes the pulled diagnostics");
    };
    assert!(set.is_current(text.version()));
    assert_eq!(set.diagnostics().len(), 1);
    assert_eq!(set.diagnostics()[0].message, "pulled");
    // The report names no producer, so the declaration identifier of the
    // session names it.
    assert_eq!(set.diagnostics()[0].source, mock::SERVER.server());
}

#[tokio::test]
async fn an_unchanged_report_keeps_the_previous_diagnostics() {
    let (mut harness, mut server) = connected();
    server.handshake_pulling("mock-lint").await;
    let (mut text, first) = pull_opened(&harness, &mut server, "fn main() {}\n").await;
    server
        .respond(
            &first["id"],
            json!({ "kind": "full", "resultId": "first", "items": [pulled_item("kept")] }),
        )
        .await;
    let LanguageOutcome::Diagnostics(set) = harness.next().await else {
        panic!("the session publishes the first report");
    };
    assert_eq!(set.diagnostics()[0].message, "kept");

    edited(&harness, &mut server, &mut text).await;
    let second = server.expect("textDocument/diagnostic").await;
    // The next pull repeats the recorded identifier, so the server may answer
    // that the previous set still describes the document.
    assert_eq!(second["params"]["previousResultId"], "first");
    server
        .respond(
            &second["id"],
            json!({ "kind": "unchanged", "resultId": "second" }),
        )
        .await;

    edited(&harness, &mut server, &mut text).await;
    let third = server.expect("textDocument/diagnostic").await;
    // The unchanged report replaced the recorded identifier, although it
    // carried no items.
    assert_eq!(third["params"]["previousResultId"], "second");
    server
        .respond(
            &third["id"],
            json!({ "kind": "full", "resultId": "third", "items": [pulled_item("later")] }),
        )
        .await;

    // The unchanged report published nothing, so the next set that reaches the
    // editor is the later full report.
    let LanguageOutcome::Diagnostics(set) = harness.next().await else {
        panic!("the session publishes the later report");
    };
    assert_eq!(set.diagnostics()[0].message, "later");
}

#[tokio::test]
async fn rejects_a_pulled_report_for_an_obsolete_buffer_version() {
    let (mut harness, mut server) = connected();
    server.handshake_pulling("mock-lint").await;
    let (mut text, first) = pull_opened(&harness, &mut server, "fn main() {}\n").await;

    // The buffer moves on while the first pull waits for its answer.
    edited(&harness, &mut server, &mut text).await;
    server
        .respond(
            &first["id"],
            json!({ "kind": "full", "resultId": "first", "items": [pulled_item("obsolete")] }),
        )
        .await;

    let second = server.expect("textDocument/diagnostic").await;
    // The obsolete report never recorded an identifier, because the session
    // rejected the complete answer.
    assert_eq!(second["params"]["previousResultId"], Value::Null);
    server
        .respond(
            &second["id"],
            json!({ "kind": "full", "items": [pulled_item("current")] }),
        )
        .await;

    let LanguageOutcome::Diagnostics(set) = harness.next().await else {
        panic!("the session publishes the current report");
    };
    assert!(set.is_current(text.version()));
    assert_eq!(set.diagnostics()[0].message, "current");
}

#[tokio::test]
async fn a_refresh_request_asks_for_every_open_document_again() {
    let (mut harness, mut server) = connected();
    server.handshake_pulling("mock-lint").await;
    let (_text, first) = pull_opened(&harness, &mut server, "fn main() {}\n").await;
    server
        .respond(&first["id"], json!({ "kind": "full", "items": [] }))
        .await;
    let LanguageOutcome::Diagnostics(set) = harness.next().await else {
        panic!("the session publishes the first report");
    };
    assert!(set.diagnostics().is_empty());

    server
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 77,
            "method": "workspace/diagnostic/refresh"
        }))
        .await;

    let answer = server.read_message().await;
    assert_eq!(answer["id"], 77);
    assert_eq!(answer["result"], Value::Null);
    // The refresh means "ask me again", so the session pulls the document once
    // more instead of only answering the request.
    let second = server.expect("textDocument/diagnostic").await;
    assert_eq!(second["params"]["textDocument"]["uri"], DOCUMENT_URI);
    drop(harness);
}

#[tokio::test]
async fn a_push_server_receives_no_pull_request() {
    let (mut harness, mut server) = connected();
    // The handshake names no diagnostic provider, so the session keeps the push
    // model of every other declared server.
    server.handshake().await;
    let mut text = opened(&harness, &mut server, "fn main() {}\n").await;

    server
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": DOCUMENT_URI,
                "version": 1,
                "diagnostics": [pulled_item("pushed")]
            }
        }))
        .await;
    let LanguageOutcome::Diagnostics(set) = harness.next().await else {
        panic!("the push path still publishes the diagnostics");
    };
    assert_eq!(set.diagnostics()[0].message, "pushed");

    edited(&harness, &mut server, &mut text).await;
    // A pull session would ask after this delay, and a push session sends
    // nothing, so the next message is the close notification.
    time::sleep(LSP_DIAGNOSTIC_PULL_DELAY * 3).await;
    harness
        .handle()
        .close(Path::new(DOCUMENT))
        .expect("the queue is empty");
    server.expect("textDocument/didClose").await;
}

#[tokio::test]
async fn answers_the_workspace_configuration_of_a_declared_server() {
    let settings = json!({ "validate": "on", "problems": { "shortenToSingleLine": false } });
    let (harness, mut server) = mock::connected_with_settings(settings);

    let initialize = server.expect("initialize").await;
    // The session declares the capability only while its declaration names
    // settings, so a server without settings never asks.
    assert_eq!(
        initialize["params"]["capabilities"]["workspace"]["configuration"],
        true
    );
    server
        .respond(
            &initialize["id"],
            json!({ "capabilities": { "positionEncoding": "utf-8" } }),
        )
        .await;
    server.expect("initialized").await;
    let pushed = server.expect("workspace/didChangeConfiguration").await;
    assert_eq!(pushed["params"]["settings"]["validate"], "on");

    server
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "workspace/configuration",
            "params": { "items": [
                { "section": "" },
                { "section": "problems.shortenToSingleLine" },
                { "section": "absent" }
            ] }
        }))
        .await;

    let answer = server.read_message().await;
    assert_eq!(answer["id"], 12);
    // An empty section names the complete object, a dotted section names one
    // member, and an unknown section answers the null value.
    assert_eq!(answer["result"][0]["validate"], "on");
    assert_eq!(answer["result"][1], false);
    assert_eq!(answer["result"][2], Value::Null);
    drop(harness);
}

#[tokio::test]
async fn reports_the_configuration_request_of_a_server_without_settings() {
    let (harness, mut server) = connected();
    server.handshake().await;

    server
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 13,
            "method": "workspace/configuration",
            "params": { "items": [{ "section": "" }] }
        }))
        .await;

    // A declaration that names no settings keeps the present answer, so the
    // server runs with its own defaults and never stalls.
    let answer = server.read_message().await;
    assert_eq!(answer["id"], 13);
    assert_eq!(answer["error"]["code"], -32601);
    drop(harness);
}

/// The shell that runs every child of the standard error tests.
///
/// The child is no language server. The tests drive a real pipe and a real
/// process, which the prepared streams of the mock cannot give.
const SHELL: &str = "/bin/sh";

/// The line that the broken server of the user wrote before it exited.
const SHIM_LINE: &str = "info: `rust-analyzer` is unavailable for the active toolchain";

/// Collects every server report of one session until the session stops.
///
/// The result holds the recorded lines and the number of bound reports.
async fn recorded_output(harness: &mut Harness) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut bounds = 0;
    loop {
        match harness.next_any().await.outcome {
            LanguageOutcome::Reported(ServerReport::Output(text)) => lines.push(text),
            LanguageOutcome::Reported(ServerReport::OutputBound) => bounds += 1,
            LanguageOutcome::Stopped => return (lines, bounds),
            _ => {}
        }
    }
}

#[tokio::test]
async fn reports_the_start_of_one_server() {
    let (mut harness, mut server) = connected();
    server.handshake().await;

    // The handshake completed, so the session reports the start and the editor
    // records it beside the output of the server.
    assert!(matches!(
        harness.next_any().await.outcome,
        LanguageOutcome::Reported(ServerReport::Started)
    ));
    drop(harness);
}

#[tokio::test]
async fn records_the_standard_error_of_a_server_that_exits_at_once() {
    // The child repeats the failure that this capture exists for: the program
    // names its cause on the standard error and exits at once.
    let mut harness = mock::process_session(
        SHELL,
        &["-c", "printf '%s\\n' \"$1\" >&2; exit 1", "shim", SHIM_LINE],
        PathBuf::from("/"),
    );

    let (lines, bounds) = recorded_output(&mut harness).await;
    assert!(
        lines.iter().any(|line| line == SHIM_LINE),
        "the recorded output names the cause, not {lines:?}"
    );
    assert_eq!(bounds, 0, "a short output passes no bound");
    harness.task.await.expect("the session task ends cleanly");
}

#[tokio::test]
async fn drains_a_server_that_writes_more_than_its_bound() {
    // Every line passes the line bound, and the child writes several times the
    // recording bound. A reader that stopped draining would fill the pipe, and
    // the child would never exit, so this test would never reach the stop.
    let line = "x".repeat(LSP_STDERR_LINE_BYTES_MAX * 2);
    let writes = LSP_STDERR_BYTES_MAX / LSP_STDERR_LINE_BYTES_MAX * 4;
    let script = format!(
        "count=0; while [ $count -lt {writes} ]; \
         do printf '%s\\n' \"$1\" >&2; count=$((count + 1)); done; exit 1"
    );
    let mut harness =
        mock::process_session(SHELL, &["-c", &script, "flood", &line], PathBuf::from("/"));

    let (lines, bounds) = recorded_output(&mut harness).await;
    assert!(bounds >= 1, "the session reports the bound that it passed");
    // One attempt records at most the bound, and every restart records again.
    let attempts = LSP_RESTARTS_MAX + 1;
    let lines_max = attempts * (LSP_STDERR_BYTES_MAX / LSP_STDERR_LINE_BYTES_MAX + 1);
    assert!(
        lines.len() <= lines_max,
        "the session records at most {lines_max} lines, not {}",
        lines.len()
    );
    assert!(
        lines
            .iter()
            .all(|line| line.len() <= LSP_STDERR_LINE_BYTES_MAX),
        "every recorded line stays inside the line bound"
    );
    harness.task.await.expect("the session task ends cleanly");
}

/// Returns the answer of one hover `contents` value.
fn answer(contents: &Value) -> Option<MarkupText> {
    hover_contents(
        contents,
        LanguageRegistry::first_release(),
        &mut SyntaxHighlighter::new(),
    )
    .expect("the text stays under the bound")
}

#[test]
fn a_markup_block_carries_the_kind_that_it_names() {
    let markdown = answer(&json!({ "kind": "markdown", "value": "`fn main()`" }))
        .expect("the block carries text");
    assert_eq!(markdown.kind, MarkupKind::Markdown);
    assert_eq!(markdown.text, "`fn main()`");

    let plain =
        answer(&json!({ "kind": "plaintext", "value": "a * b" })).expect("the block carries text");
    assert_eq!(plain.kind, MarkupKind::PlainText);
    assert_eq!(plain.text, "a * b");
}

#[test]
fn every_deprecated_marked_string_carries_markdown() {
    // The protocol defines a bare string as markdown, and it defines the
    // pair of a language and a value as one fenced markdown code block.
    let bare = answer(&json!("*emphasis*")).expect("the string carries text");
    assert_eq!(bare.kind, MarkupKind::Markdown);

    let fenced = answer(&json!({ "language": "rust", "value": "fn main()" }))
        .expect("the block carries text");
    assert_eq!(fenced.kind, MarkupKind::Markdown);
    assert_eq!(
        fenced.text, "```rust\nfn main()\n```",
        "the pair of a language and a value is one code block, so the reader writes its fence"
    );

    let array = answer(&json!([
        { "language": "rust", "value": "fn main()" },
        "*emphasis*",
    ]))
    .expect("the array carries text");
    assert_eq!(array.kind, MarkupKind::Markdown);
    assert_eq!(array.text, "```rust\nfn main()\n```\n*emphasis*");
}

#[test]
fn a_deprecated_pair_that_holds_a_fence_keeps_its_whole_value() {
    // CommonMark closes a fence at the first line that holds as many
    // backticks as the opening one, so the fence must be the longer one.
    let fenced = answer(&json!({ "language": "md", "value": "a\n```\nb\n```\nc" }))
        .expect("the block carries text");

    assert_eq!(fenced.text, "````md\na\n```\nb\n```\nc\n````");
    let document = MarkupDocument::parse(&fenced.text);
    assert_eq!(
        document.blocks().len(),
        1,
        "the value stands in one code block: {document:?}"
    );
}

#[test]
fn one_part_of_plain_text_makes_the_whole_answer_plain_text() {
    // A parser that reads plain text as markdown loses the characters that
    // mark up a document, so the safe kind covers the joined text.
    let mixed = answer(&json!([
        "*emphasis*",
        { "kind": "plaintext", "value": "a * b" },
    ]))
    .expect("the array carries text");
    assert_eq!(mixed.kind, MarkupKind::PlainText);
    assert_eq!(mixed.text, "*emphasis*\na * b");
}

#[test]
fn a_kind_that_the_protocol_defines_nowhere_takes_plain_text() {
    // An object that names no kind and no language is no shape of the
    // protocol, and neither is an unknown kind name.
    let nameless = answer(&json!({ "value": "a * b" })).expect("the object carries text");
    assert_eq!(nameless.kind, MarkupKind::PlainText);

    let unknown =
        answer(&json!({ "kind": "html", "value": "a * b" })).expect("the object carries text");
    assert_eq!(unknown.kind, MarkupKind::PlainText);
}

#[test]
fn an_answer_without_text_names_no_markup() {
    assert!(answer(&json!("   ")).is_none());
    assert!(answer(&json!([])).is_none());
    assert!(answer(&json!({ "kind": "markdown" })).is_none());
}
