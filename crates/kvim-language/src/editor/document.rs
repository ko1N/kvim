//! The document values that the language service exchanges with the editor.
//!
//! `kvim-lsp` owns the neutral protocol values. This file adds the values that
//! need a buffer revision or a markup document, and the conversions between a
//! [`TextBuffer`] and the protocol coordinates. Every conversion is pure, so
//! the terminal event loop can run it between two frames.
//!
//! Every published value carries the [`BufferRevision`] that produced it. A
//! value for an obsolete revision is rejected before publication and never
//! applied. See `docs/language-services.md`.

use std::path::{Path, PathBuf};

use kvim_core::{
    BufferRevision, BufferVersion, CharPosition, CharRange, EditTransaction, LineIndex, TextBuffer,
    TextChange,
};
use kvim_lsp::{
    ContentChange, Diagnostic, DocumentPosition, LspBound, LspError, SourceSpan, TextEdit, enforce,
};

use super::markup::MarkupDocument;

/// The complete diagnostics of one document revision.
///
/// The set is decoration. It never changes source text, a line mapping, or the
/// cursor position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticSet {
    path: PathBuf,
    revision: BufferRevision,
    diagnostics: Vec<Diagnostic>,
}

impl DiagnosticSet {
    /// Creates a set and orders it by position, so navigation is deterministic.
    #[must_use]
    pub(super) fn new(
        path: PathBuf,
        revision: BufferRevision,
        mut diagnostics: Vec<Diagnostic>,
    ) -> Self {
        diagnostics.sort_by(|left, right| {
            (left.span.start, left.span.end, left.severity)
                .cmp(&(right.span.start, right.span.end, right.severity))
                .then_with(|| left.message.cmp(&right.message))
        });
        Self {
            path,
            revision,
            diagnostics,
        }
    }

    /// Returns the document that the diagnostics describe.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the complete buffer revision that produced the diagnostics.
    #[must_use]
    pub const fn revision(&self) -> BufferRevision {
        self.revision
    }

    /// Returns the buffer version that produced the diagnostics.
    #[must_use]
    pub const fn version(&self) -> BufferVersion {
        self.revision.version()
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
    pub fn is_current(&self, current: impl Into<BufferRevision>) -> bool {
        self.revision == current.into()
    }
}

/// The markup language that a server declared for one text.
///
/// The protocol names exactly two kinds. A reader that shows markdown as it
/// stands keeps every character. A reader that parses plain text as markdown
/// removes the characters that mark up a document, so the two kinds must stay
/// apart. See `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkupKind {
    /// The text carries no markup, and a reader shows it unchanged.
    PlainText,
    /// The text carries CommonMark markup.
    Markdown,
}

impl MarkupKind {
    /// Returns the kind of one protocol name.
    ///
    /// The protocol defines `plaintext` and `markdown` only. An unknown name
    /// answers `None`, so the caller chooses the kind that loses no character.
    #[must_use]
    pub(super) fn from_protocol(name: &str) -> Option<Self> {
        match name {
            "plaintext" => Some(Self::PlainText),
            "markdown" => Some(Self::Markdown),
            _ => None,
        }
    }

    /// Returns the one kind that covers this kind and `other`.
    ///
    /// Plain text decides the pair. Markdown that a reader shows unchanged
    /// keeps every character, and plain text that a parser reads as markdown
    /// loses characters, so the pair takes the safe kind.
    #[must_use]
    pub(super) const fn merged(self, other: Self) -> Self {
        match (self, other) {
            (Self::Markdown, Self::Markdown) => Self::Markdown,
            _ => Self::PlainText,
        }
    }
}

/// One text, the markup that covers it, and the document of that markup.
///
/// The session names the document where the answer of the server arrives,
/// because the code of a fence takes the Tree-sitter highlight of its language
/// and the terminal event loop must never run that work.
///
/// A text of plain text carries an empty document. A markdown parse of a plain
/// text removes the characters that mark up a document, so no reader may parse
/// such a text. See `docs/language-services.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkupText {
    /// The markup language of the text.
    pub kind: MarkupKind,
    /// The text, exactly as the server wrote it.
    pub text: String,
    /// The blocks of the text, with the code of each fence named.
    pub document: MarkupDocument,
}

/// The complete formatter answer for one document version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatEdits {
    path: PathBuf,
    revision: BufferRevision,
    edits: Vec<TextEdit>,
}

impl FormatEdits {
    /// Creates the answer and orders the edits by position.
    #[must_use]
    pub(super) fn new(
        path: PathBuf,
        revision: impl Into<BufferRevision>,
        mut edits: Vec<TextEdit>,
    ) -> Self {
        edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
        Self {
            path,
            revision: revision.into(),
            edits,
        }
    }

    /// Returns the document that the formatter answered for.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the complete buffer revision that produced the edits.
    #[must_use]
    pub const fn revision(&self) -> BufferRevision {
        self.revision
    }

    /// Returns the buffer version that produced the edits.
    #[must_use]
    pub const fn version(&self) -> BufferVersion {
        self.revision.version()
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
        if buffer.revision() != self.revision {
            return Err(LspError::StaleVersion);
        }
        if self.edits.is_empty() {
            return Ok(None);
        }
        let mut changes = Vec::with_capacity(self.edits.len());
        for edit in &self.edits {
            let range = buffer_range(edit.span, buffer)?;
            changes.push(TextChange::replace(range, edit.text.clone()));
        }
        EditTransaction::new(cursor, changes)
            .map(Some)
            .map_err(|_| LspError::MalformedResponse)
    }
}

/// Returns the protocol position of one buffer character.
///
/// The buffer bound of `settings` keeps every line index and every byte column
/// inside the 32-bit protocol range.
///
/// # Examples
///
/// ```
/// use kvim_core::TextBuffer;
/// use kvim_language::document_position;
/// use kvim_settings::FileSettings;
///
/// let buffer = TextBuffer::from_text("let value = 1;\n", kvim_core::BufferBytesMax::default())?;
/// let cursor = buffer.char_position(4).expect("the position exists");
/// assert_eq!(document_position(&buffer, cursor).byte_column, 4);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn document_position(buffer: &TextBuffer, position: CharPosition) -> DocumentPosition {
    let line = buffer.char_to_line(position);
    let line_start = buffer.char_to_byte(buffer.line_start(line)).get();
    let byte_column = buffer.char_to_byte(position).get() - line_start;
    debug_assert!(
        u32::try_from(line.get()).is_ok() && u32::try_from(byte_column).is_ok(),
        "the file size bound of settings keeps every buffer offset below 32 bits"
    );
    DocumentPosition::new(
        u32::try_from(line.get()).unwrap_or(u32::MAX),
        u32::try_from(byte_column).unwrap_or(u32::MAX),
    )
}

/// Converts one protocol position into a validated buffer position.
///
/// # Errors
///
/// Returns [`LspError::MalformedResponse`] when the buffer holds no such line,
/// when the column passes the end of its line, or when the column falls inside
/// a character.
pub fn buffer_position(
    position: DocumentPosition,
    buffer: &TextBuffer,
) -> Result<CharPosition, LspError> {
    let index = usize::try_from(position.line).map_err(|_| LspError::MalformedResponse)?;
    let line = buffer
        .line_index(index)
        .map_err(|_| LspError::MalformedResponse)?;
    let start_byte = buffer.char_to_byte(buffer.line_start(line)).get();
    let end_byte = line_end_byte(buffer, line);
    let column = usize::try_from(position.byte_column).map_err(|_| LspError::MalformedResponse)?;
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

/// Converts one protocol range into a validated buffer range.
///
/// The conversion rejects a range that the exact buffer text does not hold, so
/// a wrong or hostile answer cannot address text outside the buffer.
///
/// # Errors
///
/// Returns [`LspError::MalformedResponse`] for a descending range or for a
/// position that the buffer does not hold.
pub fn buffer_range(span: SourceSpan, buffer: &TextBuffer) -> Result<CharRange, LspError> {
    let start = buffer_position(span.start, buffer)?;
    let end = buffer_position(span.end, buffer)?;
    CharRange::new(start, end).map_err(|_| LspError::MalformedResponse)
}

/// Returns the byte after the last byte of one line, including its terminator.
fn line_end_byte(buffer: &TextBuffer, line: LineIndex) -> usize {
    match buffer.line_index(line.get() + 1) {
        Ok(next) => buffer.char_to_byte(buffer.line_start(next)).get(),
        Err(_) => buffer.len_bytes(),
    }
}

/// Derives the incremental changes of one applied edit transaction.
///
/// The caller passes the buffer as it was before the transaction, because every
/// change of a transaction addresses that text. The changes descend, so each
/// later change keeps the coordinates that the earlier changes already left
/// untouched. This is the order that the protocol requires for one
/// notification.
///
/// # Errors
///
/// Returns [`LspError::Bounds`] above [`LSP_CONTENT_CHANGES_MAX`].
///
/// [`LSP_CONTENT_CHANGES_MAX`]: super::LSP_CONTENT_CHANGES_MAX
pub fn content_changes(
    before: &TextBuffer,
    transaction: &EditTransaction,
) -> Result<Vec<ContentChange>, LspError> {
    let changes = transaction.changes();
    enforce(
        changes.len(),
        super::LSP_CONTENT_CHANGES_MAX,
        LspBound::ContentChanges,
    )?;
    Ok(changes
        .iter()
        .rev()
        .map(|change| ContentChange {
            span: SourceSpan::new(
                document_position(before, change.range().start()),
                document_position(before, change.range().end()),
            ),
            text: change.replacement().to_owned(),
        })
        .collect())
}
