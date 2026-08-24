//! The neutral document values that one language-server answer carries.
//!
//! Every value in this file is language neutral and free of input and output.
//! It holds protocol coordinates only, so no consumer of this crate needs a
//! text buffer, a syntax tree, or an editor. A consumer that owns a buffer
//! converts these values into its own coordinates at its own boundary. See
//! `docs/language-services.md`.

use std::path::PathBuf;

use serde::Deserialize;

use crate::encoding::DocumentMapping;
use crate::protocol::{LspError, ProtocolSpan, SourceSpan};

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
///
/// # Examples
///
/// ```
/// use kvim_lsp::{Diagnostic, DiagnosticSeverity, DocumentPosition, SourceSpan};
///
/// let diagnostic = Diagnostic {
///     span: SourceSpan::new(DocumentPosition::new(3, 4), DocumentPosition::new(3, 9)),
///     severity: DiagnosticSeverity::Error,
///     message: "unknown name".to_owned(),
///     source: "rust-analyzer".to_owned(),
/// };
/// assert!(diagnostic.span.contains(DocumentPosition::new(3, 4)));
/// ```
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
pub struct RawDiagnostic {
    range: ProtocolSpan,
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
    ///
    /// `mapping` converts the range into the byte columns of the editor. See
    /// `docs/language-services.md`.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::InvalidPosition`] for a range that the document text
    /// does not hold.
    pub fn into_diagnostic(
        self,
        server: &'static str,
        mapping: &DocumentMapping,
    ) -> Result<Diagnostic, LspError> {
        let source = self
            .source
            .filter(|source| !source.is_empty())
            .unwrap_or_else(|| server.to_owned());
        Ok(Diagnostic {
            span: mapping.span_to_document(self.range)?,
            severity: DiagnosticSeverity::from_code(self.severity),
            message: self.message,
            source,
        })
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
pub struct RawTextEdit {
    range: ProtocolSpan,
    #[serde(rename = "newText")]
    new_text: String,
}

impl RawTextEdit {
    /// Converts one received edit into its editor value.
    ///
    /// `mapping` converts the range into the byte columns of the editor.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::InvalidPosition`] for a range that the document text
    /// does not hold.
    pub fn into_edit(self, mapping: &DocumentMapping) -> Result<TextEdit, LspError> {
        Ok(TextEdit {
            span: mapping.span_to_document(self.range)?,
            text: self.new_text,
        })
    }
}

/// One incremental document change, in protocol coordinates.
///
/// A session sends the changes of one synchronization in descending order, so
/// each later change keeps the coordinates that the earlier changes left
/// untouched. A consumer that owns a text buffer derives that order at its own
/// boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentChange {
    /// The range of the buffer state that this change replaces.
    pub span: SourceSpan,
    /// The text that replaces the range.
    pub text: String,
}
