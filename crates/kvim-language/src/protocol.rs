//! Bounded JSON-RPC framing, protocol limits, and workspace containment.
//! Adapted from ReviewGraph (MIT), src/analysis/lsp.rs.
//!
//! The code in this file speaks the Language Server Protocol (LSP) only. It
//! knows no language and no server product. A language adapter supplies the
//! program, the arguments, the language identifier, and the initialization
//! options as data. See `docs/language-services.md`.
//!
//! Every read and every write passes one bound. [`enforce`] applies each bound,
//! so one rule holds for the frame header, the frame body, and the cumulative
//! session budgets.

use std::fmt;
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};

use serde::de::{DeserializeSeed, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The largest frame header that one message may carry, in bytes.
pub const LSP_HEADER_BYTES_MAX: usize = 256;

/// The largest frame body that one message may carry, in bytes.
pub const LSP_MESSAGE_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The bytes that one session may write to its server.
pub const LSP_INPUT_BYTES_MAX: usize = 512 * 1024 * 1024;

/// The bytes that one session may read from its server.
pub const LSP_OUTPUT_BYTES_MAX: usize = 512 * 1024 * 1024;

/// The requests that one session may send.
pub const LSP_REQUESTS_MAX: usize = 1_000_000;

/// The messages that one session may read.
pub const LSP_MESSAGES_MAX: usize = 4_000_000;

/// The JSON-RPC error code that reports an unknown method.
const RPC_METHOD_NOT_FOUND: i64 = -32601;

/// The position encoding that Kvim requires from every server.
///
/// The encoding makes one LSP character offset one UTF-8 byte offset inside its
/// line, which is the coordinate that `core` already validates.
pub const POSITION_ENCODING: &str = "utf-8";

/// The quantity that one protocol bound measures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LspBound {
    /// The size of one frame header, in bytes.
    HeaderBytes,
    /// The size of one frame body, in bytes.
    MessageBytes,
    /// The bytes written to the server in one session.
    InputBytes,
    /// The bytes read from the server in one session.
    OutputBytes,
    /// The requests sent in one session.
    Requests,
    /// The messages read in one session.
    Messages,
    /// The documents that one session holds open.
    OpenDocuments,
    /// The requests that wait for one answer at the same time.
    PendingRequests,
    /// The content changes of one document synchronization.
    ContentChanges,
    /// The diagnostics of one document.
    Diagnostics,
    /// The locations of one definition answer.
    Locations,
    /// The edits of one formatting answer.
    FormatEdits,
    /// The size of one hover text, in bytes.
    HoverBytes,
}

/// A typed language-server transport, protocol, containment, or bounds failure.
///
/// No variant changes buffer content, a line mapping, or the cursor position. A
/// failure leaves the buffer editable.
#[derive(Debug, Error)]
pub enum LspError {
    /// No adapter of the registry owns the path.
    #[error("no language adapter supports the path")]
    UnsupportedPath,
    /// The adapter declares no language server.
    #[error("the language adapter declares no language server")]
    NoServerDeclared,
    /// The declared server is not installed on this system.
    #[error("the language server executable is not installed")]
    NotInstalled,
    /// The server process could not start.
    #[error("the language server process could not start")]
    Spawn(#[source] std::io::Error),
    /// The transport failed.
    #[error("the language server transport failed")]
    Io(#[source] std::io::Error),
    /// The session stopped, so it accepts no further request.
    #[error("the language server session stopped")]
    Stopped,
    /// The bounded request queue of the session is full.
    #[error("the language server request queue is saturated")]
    Saturated,
    /// The operation passed its deadline.
    #[error("the language server operation exceeded its deadline")]
    Timeout,
    /// The frame header or the frame body is malformed.
    #[error("the language server frame is malformed")]
    MalformedFrame,
    /// The message body is not the answer that the protocol requires.
    #[error("the language server response is malformed")]
    MalformedResponse,
    /// The server did not confirm the required position encoding.
    #[error("the language server does not support the {POSITION_ENCODING} position encoding")]
    UnsupportedEncoding,
    /// The server answered with a JSON-RPC error.
    #[error("the language server returned JSON-RPC error {code}")]
    Response {
        /// The JSON-RPC error code.
        code: i64,
    },
    /// The document is not open in this session.
    #[error("the document is not open in the language server session")]
    DocumentNotOpen,
    /// The buffer changed after the request, so the answer is obsolete.
    #[error("the language server result is obsolete")]
    StaleVersion,
    /// A path or a `file` URI falls outside the workspace root.
    #[error("the path is outside the workspace root")]
    PathEscape,
    /// A path, a URI, or a source is not UTF-8.
    #[error("the path or the source is not UTF-8")]
    Encoding,
    /// A Windows UNC path reached the URI conversion.
    #[error("UNC file paths are unsupported")]
    UnsupportedUncPath,
    /// The operation passed one bound. Kvim publishes no partial result.
    #[error("the language server exceeded its {measure:?} limit of {limit}")]
    Bounds {
        /// The quantity that the bound measures.
        measure: LspBound,
        /// The limit that the operation passed.
        limit: usize,
        /// The measured value.
        actual: usize,
    },
}

impl LspError {
    /// Reports whether this failure ends the session instead of one request.
    ///
    /// A transport failure, a malformed frame, a refused position encoding, or
    /// an exhausted cumulative budget leaves the message stream unusable. Every
    /// other failure belongs to one request, and the session continues.
    #[must_use]
    pub const fn is_fatal(&self) -> bool {
        match self {
            Self::Io(_)
            | Self::Spawn(_)
            | Self::Stopped
            | Self::MalformedFrame
            | Self::UnsupportedEncoding => true,
            Self::Bounds { measure, .. } => matches!(
                measure,
                LspBound::HeaderBytes
                    | LspBound::MessageBytes
                    | LspBound::InputBytes
                    | LspBound::OutputBytes
                    | LspBound::Requests
                    | LspBound::Messages
            ),
            _ => false,
        }
    }
}

/// Rejects a value above one limit.
///
/// One helper applies every protocol bound, so no bound can be forgotten at one
/// call site while another enforces it.
pub fn enforce(actual: usize, limit: usize, measure: LspBound) -> Result<(), LspError> {
    if actual > limit {
        return Err(LspError::Bounds {
            measure,
            limit,
            actual,
        });
    }
    Ok(())
}

/// One position inside one document.
///
/// The column is a UTF-8 byte offset inside its line, because the session
/// requires the [`POSITION_ENCODING`] position encoding.
///
/// # Examples
///
/// ```
/// use kvim_language::DocumentPosition;
///
/// let position = DocumentPosition::new(3, 8);
/// assert_eq!(position.line, 3);
/// assert_eq!(position.byte_column, 8);
/// ```
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DocumentPosition {
    /// The zero-based line index.
    pub line: u32,
    /// The zero-based UTF-8 byte offset inside the line.
    #[serde(rename = "character")]
    pub byte_column: u32,
}

impl DocumentPosition {
    /// Creates a position from a line index and a byte column.
    #[must_use]
    pub const fn new(line: u32, byte_column: u32) -> Self {
        Self { line, byte_column }
    }
}

/// One ascending range inside one document.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceSpan {
    /// The first position of the range.
    pub start: DocumentPosition,
    /// The position after the range.
    pub end: DocumentPosition,
}

impl SourceSpan {
    /// Creates a span from two positions.
    #[must_use]
    pub const fn new(start: DocumentPosition, end: DocumentPosition) -> Self {
        Self { start, end }
    }

    /// Reports whether the span covers one position.
    ///
    /// The end position lies after the range, so it belongs to the text that
    /// follows. An empty span covers its own start alone, so a marker without
    /// width still answers for the position that it marks.
    #[must_use]
    pub fn contains(&self, position: DocumentPosition) -> bool {
        if self.start == self.end {
            return position == self.start;
        }
        self.start <= position && position < self.end
    }

    /// Rejects a span that the exact source bytes do not hold.
    ///
    /// The check runs before Kvim uses a server-supplied range, so a wrong or
    /// hostile answer cannot address text outside the buffer.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::MalformedResponse`] for a descending span or for a
    /// position outside the source, and [`LspError::Encoding`] for a source
    /// that is not UTF-8.
    pub fn validate(&self, source: &str) -> Result<(), LspError> {
        if self.end < self.start {
            return Err(LspError::MalformedResponse);
        }
        validate_position(source, self.start)?;
        validate_position(source, self.end)
    }
}

/// Rejects a position that the exact source bytes do not hold.
fn validate_position(source: &str, position: DocumentPosition) -> Result<(), LspError> {
    let line_index = usize::try_from(position.line).map_err(|_| LspError::MalformedResponse)?;
    let line = source
        .split('\n')
        .nth(line_index)
        .ok_or(LspError::MalformedResponse)?;
    // A line that ends with a carriage return keeps that byte addressable,
    // because the server counts the bytes that it received.
    let column = usize::try_from(position.byte_column).map_err(|_| LspError::MalformedResponse)?;
    if column > line.len() || !line.is_char_boundary(column) {
        return Err(LspError::MalformedResponse);
    }
    Ok(())
}

/// The absolute workspace root that contains every served document.
///
/// The root is the containment boundary of the session. Kvim rejects a path or
/// a `file` URI outside it, in both directions.
///
/// # Examples
///
/// ```
/// use std::path::{Path, PathBuf};
///
/// use kvim_language::WorkspaceRoot;
///
/// let root = WorkspaceRoot::new(PathBuf::from("/work/project")).expect("the path is absolute");
/// let uri = root.uri(Path::new("/work/project/src/main.rs")).expect("the path is contained");
/// assert_eq!(uri, "file:///work/project/src/main.rs");
/// assert!(root.uri(Path::new("/etc/passwd")).is_err());
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRoot(PathBuf);

impl WorkspaceRoot {
    /// Creates the containment boundary from an absolute path.
    ///
    /// The caller resolves the path before it calls this constructor, because
    /// the type performs no input and no output.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::PathEscape`] for a relative path or for a path with
    /// a `.` or `..` component.
    pub fn new(path: PathBuf) -> Result<Self, LspError> {
        if !path.is_absolute() {
            return Err(LspError::PathEscape);
        }
        reject_unsafe_components(&path)?;
        Ok(Self(path))
    }

    /// Returns the root path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Returns the `file` URI of the root.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`WorkspaceRoot::uri`].
    pub fn root_uri(&self) -> Result<String, LspError> {
        path_to_uri(&self.0)
    }

    /// Returns the `file` URI of one contained path.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::PathEscape`] when the path is relative, holds a `.`
    /// or `..` component, or falls outside the root.
    pub fn uri(&self, path: &Path) -> Result<String, LspError> {
        self.contain(path)?;
        path_to_uri(path)
    }

    /// Returns the contained path of one `file` URI.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::PathEscape`] for a URI with another scheme, a
    /// malformed escape, a traversal component, or a target outside the root.
    pub fn path_from_uri(&self, uri: &str) -> Result<PathBuf, LspError> {
        let path = uri_to_path(uri)?;
        self.contain(&path)?;
        Ok(path)
    }

    /// Rejects a path outside the root.
    fn contain(&self, path: &Path) -> Result<(), LspError> {
        if !path.is_absolute() {
            return Err(LspError::PathEscape);
        }
        reject_unsafe_components(path)?;
        if !path.starts_with(&self.0) {
            return Err(LspError::PathEscape);
        }
        Ok(())
    }
}

/// Rejects a path that holds a traversal or a current-directory component.
fn reject_unsafe_components(path: &Path) -> Result<(), LspError> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(LspError::PathEscape);
    }
    Ok(())
}

#[cfg(unix)]
fn path_to_uri(path: &Path) -> Result<String, LspError> {
    use std::os::unix::ffi::OsStrExt;

    if !path.is_absolute() {
        return Err(LspError::PathEscape);
    }
    Ok(format!(
        "file://{}",
        percent_encode(path.as_os_str().as_bytes())
    ))
}

#[cfg(windows)]
fn path_to_uri(path: &Path) -> Result<String, LspError> {
    let path = windows_local_path(path)?;
    let path = path.to_str().ok_or(LspError::Encoding)?.replace('\\', "/");
    Ok(format!("file:///{}", percent_encode(path.as_bytes())))
}

#[cfg(windows)]
fn windows_local_path(path: &Path) -> Result<PathBuf, LspError> {
    use std::path::Prefix;

    if !path.is_absolute() {
        return Err(LspError::PathEscape);
    }
    match path.components().next() {
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _)) =>
        {
            Err(LspError::UnsupportedUncPath)
        }
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)) => {
            Ok(path.to_path_buf())
        }
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::VerbatimDisk(_)) => {
            let path = path.to_str().ok_or(LspError::Encoding)?;
            let path = path.strip_prefix(r"\\?\").ok_or(LspError::Encoding)?;
            Ok(PathBuf::from(path))
        }
        _ => Err(LspError::Encoding),
    }
}

#[cfg(unix)]
fn uri_to_path(uri: &str) -> Result<PathBuf, LspError> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let encoded = uri.strip_prefix("file://").ok_or(LspError::PathEscape)?;
    if !encoded.starts_with('/') {
        return Err(LspError::PathEscape);
    }
    Ok(PathBuf::from(OsString::from_vec(percent_decode(encoded)?)))
}

#[cfg(windows)]
fn uri_to_path(uri: &str) -> Result<PathBuf, LspError> {
    use std::path::Prefix;

    let encoded = uri.strip_prefix("file://").ok_or(LspError::PathEscape)?;
    let encoded = encoded
        .strip_prefix('/')
        .ok_or(LspError::UnsupportedUncPath)?;
    if encoded.starts_with('/') {
        return Err(LspError::UnsupportedUncPath);
    }
    let decoded = percent_decode(encoded)?;
    let decoded = std::str::from_utf8(&decoded).map_err(|_| LspError::Encoding)?;
    let bytes = decoded.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'/' {
        return Err(LspError::PathEscape);
    }
    let absolute = PathBuf::from(decoded.replace('/', "\\"));
    match absolute.components().next() {
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _)) =>
        {
            Err(LspError::UnsupportedUncPath)
        }
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)) => Ok(absolute),
        _ => Err(LspError::PathEscape),
    }
}

/// Escapes every byte that a `file` URI path may not carry unescaped.
fn percent_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len());
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b':' | b'-' | b'.' | b'_' | b'~')
        {
            encoded.push(char::from(*byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// Decodes the escapes of a `file` URI path.
fn percent_decode(value: &str) -> Result<Vec<u8>, LspError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let Some(hex) = bytes.get(index + 1..index + 3) else {
            return Err(LspError::PathEscape);
        };
        let high = hex_value(hex[0]).ok_or(LspError::PathEscape)?;
        let low = hex_value(hex[1]).ok_or(LspError::PathEscape)?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    Ok(decoded)
}

/// Returns the value of one hexadecimal digit.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// One JSON-RPC message of any kind.
#[derive(Debug, Deserialize)]
pub struct RpcEnvelope {
    /// The protocol version, which must be `2.0`.
    pub jsonrpc: String,
    /// The identity of a request or of a response.
    #[serde(default)]
    pub id: Option<RpcId>,
    /// The method of a request or of a notification.
    #[serde(default)]
    pub method: Option<String>,
    /// The unparsed parameters of a request or of a notification.
    #[serde(default, deserialize_with = "deserialize_present_raw")]
    pub params: Option<Box<RawValue>>,
    /// The unparsed result of a response.
    #[serde(default, deserialize_with = "deserialize_present_raw")]
    pub result: Option<Box<RawValue>>,
    /// The error of a response.
    #[serde(default)]
    pub error: Option<RpcResponseError>,
}

/// Keeps an explicit JSON `null` distinct from an absent member.
///
/// A `shutdown` response carries `"result": null`, and the protocol requires
/// that exact answer.
fn deserialize_present_raw<'de, D>(deserializer: D) -> Result<Option<Box<RawValue>>, D::Error>
where
    D: Deserializer<'de>,
{
    Box::<RawValue>::deserialize(deserializer).map(Some)
}

/// The identity of one JSON-RPC request or response.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RpcId {
    /// An unsigned number, which is the form that Kvim sends.
    Unsigned(u64),
    /// A signed number, which a server may send.
    Signed(i64),
    /// A text identity, which a server may send.
    String(String),
}

/// The error member of one JSON-RPC response.
#[derive(Debug, Deserialize)]
pub struct RpcResponseError {
    /// The stable JSON-RPC or LSP error code.
    pub code: i64,
}

/// Writes bounded JSON-RPC frames to one server.
///
/// The writer owns the request identity counter and the cumulative input
/// budget of the session.
pub struct ProtocolWriter<W> {
    stream: W,
    input_bytes: usize,
    requests: usize,
    next_id: u64,
}

impl<W> ProtocolWriter<W>
where
    W: AsyncWrite + Unpin,
{
    /// Creates a writer over one server input stream.
    pub const fn new(stream: W) -> Self {
        Self {
            stream,
            input_bytes: 0,
            requests: 0,
            next_id: 1,
        }
    }

    /// Sends one request and returns its identity.
    ///
    /// The caller matches the identity against a later response, because the
    /// session reads every message in one place.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::Bounds`] above the request or the byte budget, and
    /// [`LspError::Io`] when the transport fails.
    pub async fn request(&mut self, method: &str, params: Value) -> Result<u64, LspError> {
        self.requests = self.requests.saturating_add(1);
        enforce(self.requests, LSP_REQUESTS_MAX, LspBound::Requests)?;
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or(LspError::Bounds {
            measure: LspBound::Requests,
            limit: LSP_REQUESTS_MAX,
            actual: usize::MAX,
        })?;
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;
        Ok(id)
    }

    /// Sends one notification, which expects no answer.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`ProtocolWriter::request`] except the request
    /// budget.
    pub async fn notify(&mut self, method: &str, params: Value) -> Result<(), LspError> {
        self.write(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    /// Accepts one server request that needs no value, such as the creation of
    /// one work-done progress token.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`ProtocolWriter::notify`].
    pub async fn accept_server_request(&mut self, id: RpcId) -> Result<(), LspError> {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }))
            .await
    }

    /// Answers one unsolicited server request, so the server does not stall.
    ///
    /// Kvim implements no server-to-client request, so it reports the method as
    /// unknown instead of leaving the request unanswered.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`ProtocolWriter::notify`].
    pub async fn reject_server_request(&mut self, id: RpcId) -> Result<(), LspError> {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": RPC_METHOD_NOT_FOUND,
                "message": "server requests are not supported",
            }
        }))
        .await
    }

    async fn write(&mut self, value: &Value) -> Result<(), LspError> {
        let body = serde_json::to_vec(value).map_err(|_| LspError::MalformedResponse)?;
        enforce(body.len(), LSP_MESSAGE_BYTES_MAX, LspBound::MessageBytes)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let frame_bytes = header.len().saturating_add(body.len());
        let next = self.input_bytes.saturating_add(frame_bytes);
        enforce(next, LSP_INPUT_BYTES_MAX, LspBound::InputBytes)?;
        self.stream
            .write_all(header.as_bytes())
            .await
            .map_err(LspError::Io)?;
        self.stream.write_all(&body).await.map_err(LspError::Io)?;
        self.stream.flush().await.map_err(LspError::Io)?;
        self.input_bytes = next;
        Ok(())
    }
}

/// Reads bounded JSON-RPC frames from one server.
///
/// The reader owns the cumulative output budget and the message budget of the
/// session.
pub struct ProtocolReader<R> {
    stream: R,
    output_bytes: usize,
    messages: usize,
}

impl<R> ProtocolReader<R>
where
    R: AsyncRead + Unpin,
{
    /// Creates a reader over one server output stream.
    pub const fn new(stream: R) -> Self {
        Self {
            stream,
            output_bytes: 0,
            messages: 0,
        }
    }

    /// Reads the next message.
    ///
    /// The call is not cancel safe, because a dropped future can leave a
    /// partial frame in the stream. The session therefore reads in one task
    /// that no `select` interrupts.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::Io`] when the stream ends or fails,
    /// [`LspError::MalformedFrame`] for a bad header, and [`LspError::Bounds`]
    /// above one budget.
    pub async fn read_envelope(&mut self) -> Result<RpcEnvelope, LspError> {
        self.messages = self.messages.saturating_add(1);
        enforce(self.messages, LSP_MESSAGES_MAX, LspBound::Messages)?;
        let body = read_frame(
            &mut self.stream,
            &mut self.output_bytes,
            LSP_OUTPUT_BYTES_MAX,
        )
        .await?;
        let envelope: RpcEnvelope =
            serde_json::from_slice(&body).map_err(|_| LspError::MalformedResponse)?;
        if envelope.jsonrpc != "2.0" {
            return Err(LspError::MalformedResponse);
        }
        Ok(envelope)
    }
}

/// Reads one frame and bounds the header and the body separately.
///
/// A split header or a split body is normal on a pipe, so the reader collects
/// the header byte by byte and then reads the exact body length.
pub async fn read_frame<R>(
    reader: &mut R,
    output_bytes: &mut usize,
    output_limit: usize,
) -> Result<Vec<u8>, LspError>
where
    R: AsyncRead + Unpin,
{
    let mut header = Vec::with_capacity(64);
    loop {
        let byte = reader.read_u8().await.map_err(LspError::Io)?;
        header.push(byte);
        enforce(header.len(), LSP_HEADER_BYTES_MAX, LspBound::HeaderBytes)?;
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let header_text = std::str::from_utf8(&header).map_err(|_| LspError::MalformedFrame)?;
    let mut length = None;
    for line in header_text[..header_text.len() - 4].split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            return Err(LspError::MalformedFrame);
        };
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(LspError::MalformedFrame);
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| LspError::MalformedFrame)?,
            );
        }
    }
    let length = length.ok_or(LspError::MalformedFrame)?;
    enforce(length, LSP_MESSAGE_BYTES_MAX, LspBound::MessageBytes)?;
    let frame_bytes = header.len().saturating_add(length);
    let next = output_bytes.saturating_add(frame_bytes);
    enforce(next, output_limit, LspBound::OutputBytes)?;
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await.map_err(LspError::Io)?;
    *output_bytes = next;
    Ok(body)
}

/// The remaining elements that one group of nested arrays may hold.
///
/// Nested arrays share one budget, so a server cannot allocate without limit by
/// splitting many elements over many short arrays.
pub struct ArrayBudget {
    remaining: usize,
    limit: usize,
    exceeded: Option<(usize, usize)>,
}

impl ArrayBudget {
    /// Creates a budget with a remaining count and the limit that names it.
    #[must_use]
    pub const fn new(remaining: usize, limit: usize) -> Self {
        Self {
            remaining,
            limit,
            exceeded: None,
        }
    }
}

struct BoundedArraySeed<'a, T> {
    per_array_limit: usize,
    measure: LspBound,
    budget: &'a mut ArrayBudget,
    marker: PhantomData<T>,
}

impl<'de, T> DeserializeSeed<'de> for BoundedArraySeed<'_, T>
where
    T: Deserialize<'de>,
{
    type Value = Option<Vec<T>>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BoundedArrayVisitor {
            per_array_limit: self.per_array_limit,
            measure: self.measure,
            budget: self.budget,
            marker: self.marker,
        })
    }
}

struct BoundedArrayVisitor<'a, T> {
    per_array_limit: usize,
    measure: LspBound,
    budget: &'a mut ArrayBudget,
    marker: PhantomData<T>,
}

impl<'de, T> Visitor<'de> for BoundedArrayVisitor<'_, T>
where
    T: Deserialize<'de>,
{
    type Value = Option<Vec<T>>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("null or a bounded JSON array")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        // The stream never reserves more than the smaller of the two bounds, so
        // a large size hint cannot allocate past the budget.
        let capacity = sequence
            .size_hint()
            .unwrap_or(0)
            .min(self.per_array_limit)
            .min(self.budget.remaining);
        let mut values = Vec::with_capacity(capacity);
        loop {
            let per_array_exhausted = values.len() == self.per_array_limit;
            let total_exhausted = self.budget.remaining == 0;
            if per_array_exhausted || total_exhausted {
                if sequence.next_element::<IgnoredAny>()?.is_some() {
                    self.budget.exceeded = Some(if per_array_exhausted {
                        (self.per_array_limit, self.per_array_limit.saturating_add(1))
                    } else {
                        (self.budget.limit, self.budget.limit.saturating_add(1))
                    });
                    return Err(serde::de::Error::custom(format_args!(
                        "{:?} array bound exceeded",
                        self.measure
                    )));
                }
                return Ok(Some(values));
            }
            self.budget.remaining -= 1;
            match sequence.next_element::<T>()? {
                Some(value) => values.push(value),
                None => {
                    self.budget.remaining += 1;
                    return Ok(Some(values));
                }
            }
        }
    }
}

/// Deserializes one JSON array without allocating past its budget.
///
/// The value may also be JSON `null`, which every optional LSP array result
/// allows. A `null` result becomes an empty list.
///
/// # Errors
///
/// Returns [`LspError::Bounds`] above the per-array limit or the shared budget,
/// and [`LspError::MalformedResponse`] for any other shape.
pub fn deserialize_bounded_array<T>(
    raw: &RawValue,
    per_array_limit: usize,
    measure: LspBound,
    budget: &mut ArrayBudget,
) -> Result<Vec<T>, LspError>
where
    T: for<'de> Deserialize<'de>,
{
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let result = BoundedArraySeed {
        per_array_limit,
        measure,
        budget,
        marker: PhantomData,
    }
    .deserialize(&mut deserializer);
    if let Some((limit, actual)) = budget.exceeded.take() {
        return Err(LspError::Bounds {
            measure,
            limit,
            actual,
        });
    }
    let values = result.map_err(|_| LspError::MalformedResponse)?;
    deserializer
        .end()
        .map_err(|_| LspError::MalformedResponse)?;
    Ok(values.unwrap_or_default())
}
