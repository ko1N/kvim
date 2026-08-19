//! The document values that the language service exchanges with the editor.
//!
//! Every value in this file is language neutral and free of input and output.
//! The conversions between a [`TextBuffer`] and the protocol coordinates are
//! pure, so the terminal event loop can run them between two frames.
//!
//! Every published value carries the [`BufferVersion`] that produced it. A
//! value for an obsolete version is rejected before publication and never
//! applied. See `docs/language-services.md`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use kvim_core::{
    BufferVersion, CharPosition, CharRange, EditTransaction, LineIndex, TextBuffer, TextChange,
};

use super::protocol::{DocumentPosition, LspBound, LspError, SourceSpan, enforce};

/// The severity that a language server reports for one diagnostic.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticSeverity {
    /// The code does not build or does not type check.
    Error,
    /// The code builds, but the server reports a defect.
    Warning,
    /// The server reports a neutral fact.
    Information,
    /// The server reports an optional improvement.
    Hint,
}

impl DiagnosticSeverity {
    /// Returns the severity of one protocol code.
    ///
    /// An absent or unknown code becomes [`DiagnosticSeverity::Error`], because
    /// the protocol lets the client choose, and the strictest choice never
    /// hides a defect.
    #[must_use]
    fn from_code(code: Option<u8>) -> Self {
        match code {
            Some(2) => Self::Warning,
            Some(3) => Self::Information,
            Some(4) => Self::Hint,
            _ => Self::Error,
        }
    }
}

/// One diagnostic of one document version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// The range that the diagnostic marks.
    pub span: SourceSpan,
    /// The severity of the diagnostic.
    pub severity: DiagnosticSeverity,
    /// The message of the diagnostic.
    pub message: String,
    /// The producer of the diagnostic.
    ///
    /// The value is the `source` field of the protocol when the server sends
    /// one, and the declaration identifier of that server otherwise. One buffer
    /// can merge the diagnostics of several servers, so every diagnostic
    /// records its origin. The editor decides on its own whether it shows that
    /// name. See `docs/language-services.md`.
    pub source: String,
}

/// The wire shape of one diagnostic.
#[derive(Debug, Deserialize)]
pub(super) struct RawDiagnostic {
    range: SourceSpan,
    #[serde(default)]
    severity: Option<u8>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    source: Option<String>,
}

impl RawDiagnostic {
    /// Converts one received diagnostic into its editor value.
    ///
    /// `server` is the declaration identifier of the session that received the
    /// diagnostic. It names the producer when the server sends no `source`
    /// field, so every merged diagnostic of one buffer names its origin.
    pub(super) fn into_diagnostic(self, server: &'static str) -> Diagnostic {
        let source = self
            .source
            .filter(|source| !source.is_empty())
            .unwrap_or_else(|| server.to_owned());
        Diagnostic {
            span: self.range,
            severity: DiagnosticSeverity::from_code(self.severity),
            message: self.message,
            source,
        }
    }
}

/// The complete diagnostics of one document version.
///
/// The set is decoration. It never changes source text, a line mapping, or the
/// cursor position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticSet {
    path: PathBuf,
    version: BufferVersion,
    diagnostics: Vec<Diagnostic>,
}

impl DiagnosticSet {
    /// Creates a set and orders it by position, so navigation is deterministic.
    #[must_use]
    pub(super) fn new(
        path: PathBuf,
        version: BufferVersion,
        mut diagnostics: Vec<Diagnostic>,
    ) -> Self {
        diagnostics.sort_by(|left, right| {
            (left.span.start, left.span.end, left.severity)
                .cmp(&(right.span.start, right.span.end, right.severity))
                .then_with(|| left.message.cmp(&right.message))
        });
        Self {
            path,
            version,
            diagnostics,
        }
    }

    /// Returns the document that the diagnostics describe.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the buffer version that produced the diagnostics.
    #[must_use]
    pub const fn version(&self) -> BufferVersion {
        self.version
    }

    /// Returns the diagnostics in ascending position order.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Reports whether the set still describes the current buffer version.
    ///
    /// The event loop asks before it publishes, so an obsolete set never
    /// reaches visible state.
    #[must_use]
    pub const fn is_current(&self, current: BufferVersion) -> bool {
        self.version.get() == current.get()
    }
}

/// One resolved definition target inside the workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    /// The contained path of the target document.
    pub path: PathBuf,
    /// The range of the target inside that document.
    pub span: SourceSpan,
}

/// One replacement that a formatter computed for one document version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    /// The range that the edit replaces.
    pub span: SourceSpan,
    /// The text that replaces the range.
    pub text: String,
}

/// The wire shape of one text edit.
#[derive(Debug, Deserialize)]
pub(super) struct RawTextEdit {
    range: SourceSpan,
    #[serde(rename = "newText")]
    new_text: String,
}

impl RawTextEdit {
    /// Converts one received edit into its editor value.
    pub(super) fn into_edit(self) -> TextEdit {
        TextEdit {
            span: self.range,
            text: self.new_text,
        }
    }
}

/// The complete formatter answer for one document version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatEdits {
    path: PathBuf,
    version: BufferVersion,
    edits: Vec<TextEdit>,
}

impl FormatEdits {
    /// Creates the answer and orders the edits by position.
    #[must_use]
    pub(super) fn new(path: PathBuf, version: BufferVersion, mut edits: Vec<TextEdit>) -> Self {
        edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
        Self {
            path,
            version,
            edits,
        }
    }

    /// Returns the document that the formatter answered for.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the buffer version that produced the edits.
    #[must_use]
    pub const fn version(&self) -> BufferVersion {
        self.version
    }

    /// Returns the edits in ascending position order.
    #[must_use]
    pub fn edits(&self) -> &[TextEdit] {
        &self.edits
    }

    /// Builds one undoable transaction for the current buffer.
    ///
    /// The complete formatter answer becomes one transaction, so one undo
    /// reverses a complete format. `None` reports that the buffer already
    /// matches the formatter and needs no change.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::StaleVersion`] when the buffer changed after the
    /// request, and [`LspError::MalformedResponse`] when one edit does not
    /// address the exact buffer text or two edits overlap.
    pub fn transaction(
        &self,
        buffer: &TextBuffer,
        cursor: CharPosition,
    ) -> Result<Option<EditTransaction>, LspError> {
        if buffer.version().get() != self.version.get() {
            return Err(LspError::StaleVersion);
        }
        if self.edits.is_empty() {
            return Ok(None);
        }
        let mut changes = Vec::with_capacity(self.edits.len());
        for edit in &self.edits {
            let range = edit.span.char_range(buffer)?;
            changes.push(TextChange::replace(range, edit.text.clone()));
        }
        EditTransaction::new(cursor, changes)
            .map(Some)
            .map_err(|_| LspError::MalformedResponse)
    }
}

impl SourceSpan {
    /// Converts one protocol range into a validated buffer range.
    ///
    /// The conversion rejects a range that the exact buffer text does not hold,
    /// so a wrong or hostile answer cannot address text outside the buffer.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::MalformedResponse`] for a descending range or for a
    /// position that the buffer does not hold.
    pub fn char_range(&self, buffer: &TextBuffer) -> Result<CharRange, LspError> {
        let start = self.start.char_position(buffer)?;
        let end = self.end.char_position(buffer)?;
        CharRange::new(start, end).map_err(|_| LspError::MalformedResponse)
    }
}

impl DocumentPosition {
    /// Returns the position of one buffer character.
    ///
    /// The buffer bound of `settings` keeps every line index and every byte
    /// column inside the 32-bit protocol range.
    #[must_use]
    pub fn of_buffer(buffer: &TextBuffer, position: CharPosition) -> Self {
        let line = buffer.char_to_line(position);
        let line_start = buffer.char_to_byte(buffer.line_start(line)).get();
        let byte_column = buffer.char_to_byte(position).get() - line_start;
        debug_assert!(
            u32::try_from(line.get()).is_ok() && u32::try_from(byte_column).is_ok(),
            "the file size bound of settings keeps every buffer offset below 32 bits"
        );
        Self::new(
            u32::try_from(line.get()).unwrap_or(u32::MAX),
            u32::try_from(byte_column).unwrap_or(u32::MAX),
        )
    }

    /// Converts one protocol position into a validated buffer position.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::MalformedResponse`] when the buffer holds no such
    /// line, when the column passes the end of its line, or when the column
    /// falls inside a character.
    pub fn char_position(self, buffer: &TextBuffer) -> Result<CharPosition, LspError> {
        let index = usize::try_from(self.line).map_err(|_| LspError::MalformedResponse)?;
        let line = buffer
            .line_index(index)
            .map_err(|_| LspError::MalformedResponse)?;
        let start_byte = buffer.char_to_byte(buffer.line_start(line)).get();
        let end_byte = line_end_byte(buffer, line);
        let column = usize::try_from(self.byte_column).map_err(|_| LspError::MalformedResponse)?;
        // The column must stay inside its own line, so a large column cannot
        // address the following lines.
        if column > end_byte - start_byte {
            return Err(LspError::MalformedResponse);
        }
        let offset = buffer
            .byte_offset(start_byte + column)
            .map_err(|_| LspError::MalformedResponse)?;
        Ok(buffer.byte_to_char(offset))
    }
}

/// Returns the byte after the last byte of one line, including its terminator.
fn line_end_byte(buffer: &TextBuffer, line: LineIndex) -> usize {
    match buffer.line_index(line.get() + 1) {
        Ok(next) => buffer.char_to_byte(buffer.line_start(next)).get(),
        Err(_) => buffer.len_bytes(),
    }
}

/// One incremental document change, in protocol coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentChange {
    /// The range of the buffer state that this change replaces.
    pub span: SourceSpan,
    /// The text that replaces the range.
    pub text: String,
}

impl ContentChange {
    /// Derives the incremental changes of one applied edit transaction.
    ///
    /// The caller passes the buffer as it was before the transaction, because
    /// every change of a transaction addresses that text. The changes descend,
    /// so each later change keeps the coordinates that the earlier changes
    /// already left untouched. This is the order that the protocol requires
    /// for one notification.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::Bounds`] above [`LSP_CONTENT_CHANGES_MAX`].
    ///
    /// [`LSP_CONTENT_CHANGES_MAX`]: super::LSP_CONTENT_CHANGES_MAX
    pub fn from_transaction(
        before: &TextBuffer,
        transaction: &EditTransaction,
    ) -> Result<Vec<Self>, LspError> {
        let changes = transaction.changes();
        enforce(
            changes.len(),
            super::LSP_CONTENT_CHANGES_MAX,
            LspBound::ContentChanges,
        )?;
        Ok(changes
            .iter()
            .rev()
            .map(|change| Self {
                span: SourceSpan::new(
                    DocumentPosition::of_buffer(before, change.range().start()),
                    DocumentPosition::of_buffer(before, change.range().end()),
                ),
                text: change.replacement().to_owned(),
            })
            .collect())
    }
}
