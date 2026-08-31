use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncWriteExt, duplex};

use super::{
    ArrayBudget, LSP_HEADER_BYTES_MAX, LSP_MESSAGE_BYTES_MAX, LSP_OUTPUT_BYTES_MAX, LspBound,
    LspError, WorkspaceRoot, deserialize_bounded_array, enforce, read_frame,
};

/// The capacity of the in-memory pipe that stands in for one server stream.
const PIPE_BYTES: usize = 1024 * 1024;

/// The workspace root of the containment tests.
const ROOT: &str = "/workspace";

/// One document inside [`ROOT`].
const DOCUMENT: &str = "/workspace/src/main.rs";

/// The `file` URI of [`DOCUMENT`].
const DOCUMENT_URI: &str = "file:///workspace/src/main.rs";

#[tokio::test]
async fn reads_a_frame_that_arrives_in_pieces() {
    let (mut writer, mut reader) = duplex(PIPE_BYTES);
    let feeder = tokio::spawn(async move {
        for piece in ["Content-Le", "ngth: 12\r", "\n\r\n{\"a\"", ":\"bcde\"}"] {
            writer
                .write_all(piece.as_bytes())
                .await
                .expect("the pipe accepts the piece");
            writer.flush().await.expect("the pipe flushes");
            tokio::task::yield_now().await;
        }
    });

    let mut output_bytes = 0;
    let body = read_frame(&mut reader, &mut output_bytes, LSP_OUTPUT_BYTES_MAX)
        .await
        .expect("a split header and a split body still form one frame");
    assert_eq!(body, br#"{"a":"bcde"}"#);
    // The budget counts the header bytes and the body bytes together.
    assert_eq!(output_bytes, 34);
    feeder.await.expect("the feeder ends");
}

#[tokio::test]
async fn rejects_a_header_above_its_bound() {
    let (mut writer, mut reader) = duplex(PIPE_BYTES);
    let padding = "X".repeat(LSP_HEADER_BYTES_MAX);
    let feeder = tokio::spawn(async move {
        let _ = writer
            .write_all(format!("Content-Length: 2\r\nPadding: {padding}\r\n\r\n{{}}").as_bytes())
            .await;
    });

    let mut output_bytes = 0;
    let error = read_frame(&mut reader, &mut output_bytes, LSP_OUTPUT_BYTES_MAX)
        .await
        .expect_err("the header passes its bound");
    assert!(matches!(
        error,
        LspError::Bounds {
            measure: LspBound::HeaderBytes,
            limit: LSP_HEADER_BYTES_MAX,
            ..
        }
    ));
    feeder.await.expect("the feeder ends");
}

#[tokio::test]
async fn rejects_a_body_above_its_bound() {
    let (mut writer, mut reader) = duplex(PIPE_BYTES);
    let length = LSP_MESSAGE_BYTES_MAX + 1;
    let feeder = tokio::spawn(async move {
        let _ = writer
            .write_all(format!("Content-Length: {length}\r\n\r\n").as_bytes())
            .await;
    });

    let mut output_bytes = 0;
    let error = read_frame(&mut reader, &mut output_bytes, LSP_OUTPUT_BYTES_MAX)
        .await
        .expect_err("the body passes its bound");
    // The bound stops the read before the body arrives, so no allocation
    // grows with the claimed length.
    assert!(matches!(
        error,
        LspError::Bounds {
            measure: LspBound::MessageBytes,
            limit: LSP_MESSAGE_BYTES_MAX,
            ..
        }
    ));
    assert_eq!(output_bytes, 0);
    feeder.await.expect("the feeder ends");
}

#[tokio::test]
async fn rejects_a_frame_without_a_content_length() {
    let (mut writer, mut reader) = duplex(PIPE_BYTES);
    let feeder = tokio::spawn(async move {
        let _ = writer.write_all(b"Content-Type: json\r\n\r\n{}").await;
    });

    let mut output_bytes = 0;
    let error = read_frame(&mut reader, &mut output_bytes, LSP_OUTPUT_BYTES_MAX)
        .await
        .expect_err("a frame without a length is malformed");
    assert!(matches!(error, LspError::MalformedFrame));
    feeder.await.expect("the feeder ends");
}

#[test]
fn launch_unavailability_is_not_an_existing_session_failure() {
    let error = LspError::Unavailable(io::Error::new(
        io::ErrorKind::NotFound,
        "fixture executable is absent",
    ));

    assert!(!error.is_fatal());
}

#[test]
fn nested_arrays_share_one_element_budget() {
    let outer: Box<serde_json::value::RawValue> =
        serde_json::from_str("[1, 2, 3]").expect("the test value parses");
    let mut budget = ArrayBudget::new(4, 4);

    let first: Vec<u8> = deserialize_bounded_array(&outer, 8, LspBound::Locations, &mut budget)
        .expect("three elements fit the budget");
    assert_eq!(first, [1, 2, 3]);

    // A hostile server cannot split many elements over many short arrays,
    // because one budget counts them all.
    let error = deserialize_bounded_array::<u8>(&outer, 8, LspBound::Locations, &mut budget)
        .expect_err("the shared budget holds only one further element");
    assert!(matches!(
        error,
        LspError::Bounds {
            measure: LspBound::Locations,
            ..
        }
    ));
}

#[test]
fn one_helper_enforces_every_cumulative_budget() {
    assert!(enforce(4, 4, LspBound::Requests).is_ok());
    let error = enforce(5, 4, LspBound::OutputBytes).expect_err("five passes the limit of four");
    assert!(matches!(
        error,
        LspError::Bounds {
            measure: LspBound::OutputBytes,
            limit: 4,
            actual: 5,
        }
    ));
}

#[test]
fn the_workspace_root_contains_every_path_and_uri() {
    let root = WorkspaceRoot::new(PathBuf::from(ROOT)).expect("the root is absolute");

    assert_eq!(
        root.uri(Path::new(DOCUMENT))
            .expect("the path is contained"),
        DOCUMENT_URI
    );
    assert_eq!(
        root.path_from_uri(DOCUMENT_URI)
            .expect("the URI is contained"),
        PathBuf::from(DOCUMENT)
    );
    assert!(matches!(
        root.uri(Path::new("/etc/passwd")),
        Err(LspError::PathEscape)
    ));
    assert!(matches!(
        root.path_from_uri("file:///workspace/../etc/passwd"),
        Err(LspError::PathEscape)
    ));
    assert!(matches!(
        root.path_from_uri("http://example.com/workspace/src/main.rs"),
        Err(LspError::PathEscape)
    ));
    assert!(matches!(
        root.path_from_uri("file:///workspace/src/%zz.rs"),
        Err(LspError::PathEscape)
    ));
    assert!(matches!(
        WorkspaceRoot::new(PathBuf::from("relative")),
        Err(LspError::PathEscape)
    ));
}

#[test]
fn a_space_in_a_path_survives_the_uri_round_trip() {
    let root = WorkspaceRoot::new(PathBuf::from(ROOT)).expect("the root is absolute");
    let path = PathBuf::from("/workspace/src/my file.rs");

    let uri = root.uri(&path).expect("the path is contained");

    assert_eq!(uri, "file:///workspace/src/my%20file.rs");
    assert_eq!(
        root.path_from_uri(&uri).expect("the URI is contained"),
        path
    );
}

#[test]
fn a_contained_path_names_its_worktree_relative_path() {
    let root = WorkspaceRoot::new(PathBuf::from(ROOT)).expect("the root is absolute");

    assert_eq!(
        root.relative_path(Path::new(DOCUMENT))
            .expect("the path is contained")
            .as_path(),
        Path::new("src/main.rs")
    );
    // The root carries no relative remainder, so it names no document.
    assert!(matches!(
        root.relative_path(Path::new(ROOT)),
        Err(LspError::PathEscape)
    ));
    assert!(matches!(
        root.relative_path(Path::new("/workspace/../etc/passwd")),
        Err(LspError::PathEscape)
    ));
}
