//! The language adapter registry and the language-neutral Tree-sitter analysis.
//! Adapted from ReviewGraph (MIT), src/analysis.rs.
//!
//! The adapter boundary is the multi-language extension point of Kvim. An
//! adapter supplies data: the paths of its language, the Tree-sitter grammar
//! with its highlight query, the comment tokens, and the indent rule. Nothing
//! above the trait names a language, so a later release adds a language by
//! registering one more adapter. Rust is the only adapter of the first release.
//!
//! Only an adapter can select a path by language or by file extension. Generic
//! `kvim-core`, `kvim-editor`, `kvim-runtime`, `kvim-terminal`, `kvim-tui`, and
//! `kvim-workspace` code passes a path and exact buffer content, and never
//! inspects the extension.
//!
//! One analysis reads the exact text of one buffer version. It returns bounded
//! highlight spans, the syntax tree of that version, and the indent level for a
//! new line. It changes no buffer content, no line mapping, and no cursor.
//!
//! Analysis runs only on the bounded worker service of `kvim-runtime`. It never
//! runs on the terminal event loop. [`BufferSyntax`] holds the newest accepted
//! result and rejects a result for an obsolete buffer version before it enters
//! the cache. See `docs/language-services.md` and `docs/responsiveness.md`.
//!
//! The crate also owns the Language Server Protocol client. [`LanguageServices`]
//! holds one persistent session for each language that declares a server, and
//! it delivers every result as a [`LanguageEvent`]. The client speaks the
//! protocol only. An adapter declares its server through
//! [`LanguageAdapter::language_server`], so no code above the adapter boundary
//! names a server product.
//!
//! [`LanguageAdapter`] stays object-safe, so the registry holds one adapter for
//! each language. A later asynchronous method must return a boxed future
//! instead of using `async fn`, which would break object safety.
//!
//! # Examples
//!
//! ```
//! use std::path::Path;
//! use std::sync::Arc;
//!
//! use kvim_core::TextBuffer;
//! use kvim_language::{AnalysisInput, LanguageRegistry};
//! use kvim_settings::FileSettings;
//! use tokio_util::sync::CancellationToken;
//!
//! let registry = LanguageRegistry::first_release();
//! let adapter = registry.adapter(Path::new("src/main.rs")).expect("the Rust adapter owns the path");
//! assert_eq!(adapter.comment().line_token(), Some("//"));
//!
//! let buffer = TextBuffer::from_text("fn main() {}\n", &FileSettings::default())
//!     .expect("the text is small");
//! let input = AnalysisInput::new(buffer.version(), Arc::from(buffer.to_string()));
//! let analysis = adapter
//!     .analyze(&input, &CancellationToken::new())
//!     .expect("the source is valid Rust");
//! assert!(!analysis.highlights().is_empty());
//! ```

use std::ffi::OsStr;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tree_sitter::{InputEdit, Language, Point, Tree};

use kvim_core::{BufferVersion, CharPosition, EditTransaction, TextBuffer, TextChange};

mod analysis;
mod document;
mod protocol;
mod rust;
mod server;
mod services;
mod session;

#[cfg(any(test, feature = "test-support"))]
pub mod mock;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod tests;

pub use document::{
    ContentChange, Diagnostic, DiagnosticSet, DiagnosticSeverity, FormatEdits, SourceLocation,
    TextEdit,
};
pub use protocol::{
    DocumentPosition, LSP_HEADER_BYTES_MAX, LSP_INPUT_BYTES_MAX, LSP_MESSAGE_BYTES_MAX,
    LSP_MESSAGES_MAX, LSP_OUTPUT_BYTES_MAX, LSP_REQUESTS_MAX, LspBound, LspError,
    POSITION_ENCODING, SourceSpan, WorkspaceRoot,
};
pub use rust::RustAdapter;
pub use server::LanguageServerDeclaration;
pub use services::LanguageServices;
pub use session::{
    LSP_CONTENT_CHANGES_MAX, LSP_DIAGNOSTICS_MAX, LSP_EVENT_QUEUE_CAPACITY, LSP_FORMAT_DEADLINE,
    LSP_FORMAT_EDITS_MAX, LSP_HOVER_BYTES_MAX, LSP_INITIALIZE_DEADLINE, LSP_LOCATIONS_MAX,
    LSP_OPEN_DOCUMENTS_MAX, LSP_PENDING_REQUESTS_MAX, LSP_REQUEST_DEADLINE,
    LSP_REQUEST_QUEUE_CAPACITY, LSP_RESTARTS_MAX, LSP_SHUTDOWN_DEADLINE, LanguageEvent,
    LanguageOutcome, LanguageRequestId, LanguageServerHandle,
};

/// The largest source that one analysis reads, in bytes.
pub const ANALYSIS_SOURCE_BYTES_MAX: usize = 4 * 1024 * 1024;

/// The largest source that one analysis reads, in lines.
pub const ANALYSIS_SOURCE_LINES_MAX: usize = 100_000;

/// The largest syntax tree that one analysis publishes, in nodes.
pub const ANALYSIS_NODES_MAX: usize = 1_000_000;

/// The largest number of ancestors that the indent query walks.
pub const ANALYSIS_DEPTH_MAX: usize = 128;

/// The largest number of highlight spans that one analysis publishes.
pub const ANALYSIS_HIGHLIGHT_SPANS_MAX: usize = 100_000;

/// The deadline of one analysis on the bounded worker service.
pub const ANALYSIS_DEADLINE: Duration = Duration::from_secs(2);

/// The quantity that one bound measures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundMeasure {
    /// The source size, in bytes.
    Bytes,
    /// The source size, in lines.
    Lines,
    /// The number of nodes in the syntax tree.
    Nodes,
    /// The number of ancestors that one query walks.
    Depth,
    /// The number of highlight spans.
    HighlightSpans,
}

/// A typed registry, parser, cancellation, or bounds failure.
///
/// Every variant renders plain text. No variant changes buffer content, a line
/// mapping, or the cursor position.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AnalysisError {
    /// No adapter owns the path.
    #[error("no language adapter supports the path")]
    UnsupportedPath,
    /// More than one adapter owns the path.
    #[error("more than one language adapter supports the path")]
    AmbiguousPath,
    /// The grammar or the highlight query did not load.
    #[error("the language parser could not be configured")]
    ParserSetup,
    /// The parser returned no syntax tree.
    #[error("the language parser did not return a syntax tree")]
    ParseFailure,
    /// The request was cancelled or superseded.
    #[error("analysis was cancelled")]
    Cancelled,
    /// The complete result exceeds one bound. Kvim publishes no partial result.
    #[error("analysis exceeded its {measure:?} limit of {limit}")]
    Bounds {
        /// The quantity that the bound measures.
        measure: BoundMeasure,
        /// The limit that the analysis passed.
        limit: usize,
        /// The measured value.
        actual: usize,
    },
    /// The parser returned a range that the source does not hold.
    #[error("the language adapter returned malformed spans")]
    MalformedOutput,
}

/// The delimiters of one block comment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockComment {
    /// The text that opens the comment.
    pub open: &'static str,
    /// The text that closes the comment.
    pub close: &'static str,
}

impl BlockComment {
    /// Creates the delimiter pair of one language.
    #[must_use]
    pub const fn new(open: &'static str, close: &'static str) -> Self {
        Self { open, close }
    }
}

/// The comment tokens of one language.
///
/// The tokens are adapter data, so the one comment-toggle path serves every
/// language. A language without a line token has `None`, and the first-release
/// toggle then changes nothing. See `docs/language-services.md`.
///
/// # Examples
///
/// ```
/// use kvim_language::{BlockComment, CommentStyle};
///
/// let rust = CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")));
/// assert_eq!(rust.line_token(), Some("//"));
/// assert_eq!(rust.block().map(|block| block.close), Some("*/"));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommentStyle {
    line: Option<&'static str>,
    block: Option<BlockComment>,
}

impl CommentStyle {
    /// Creates the comment metadata of one language.
    #[must_use]
    pub const fn new(line: Option<&'static str>, block: Option<BlockComment>) -> Self {
        Self { line, block }
    }

    /// Returns the token that starts a line comment.
    #[must_use]
    pub const fn line_token(self) -> Option<&'static str> {
        self.line
    }

    /// Returns the delimiters of a block comment.
    #[must_use]
    pub const fn block(self) -> Option<BlockComment> {
        self.block
    }
}

/// The Tree-sitter grammar and highlight query of one language.
///
/// The value is adapter data. The analysis reads it and knows no language.
#[derive(Clone, Copy)]
pub struct Grammar {
    /// The stable grammar name, which also keys the query cache.
    pub name: &'static str,
    /// The entry point of the compiled grammar.
    pub language: fn() -> Language,
    /// The highlight query of the grammar.
    pub highlights_query: &'static str,
    /// The injection query, or the empty text when the grammar has none.
    pub injections_query: &'static str,
    /// The local-variable query, or the empty text when the grammar has none.
    pub locals_query: &'static str,
}

/// The indent rule of one language, as syntax-tree data.
///
/// The rule stays a level count over node kinds, so it holds for every
/// language whose grammar names its nested nodes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndentRule {
    /// The node kinds whose content takes one more indent level.
    pub scopes: &'static [&'static str],
    /// The characters that close such a node at the start of a new line.
    pub closing_delimiters: &'static [char],
}

/// The number of indent levels that one new line takes.
///
/// The value is a level count, not a column count, so `EditorSettings` keeps
/// the tab width and the shift width. See `docs/settings.md`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct IndentLevel(u16);

impl IndentLevel {
    /// Creates a level count.
    #[must_use]
    pub(crate) const fn new(levels: u16) -> Self {
        Self(levels)
    }

    /// Returns the number of levels.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// One terminal-independent syntax role.
///
/// A language adapter emits these roles. A role names what a range of source
/// is, never how it looks, so the language boundary needs no palette and no
/// terminal. The interface layer maps each role to one style. See
/// `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SyntaxRole {
    /// An attribute, such as a Rust derive attribute.
    Attribute,
    /// A boolean literal.
    Boolean,
    /// A bracket, brace, or parenthesis.
    Bracket,
    /// A comment.
    Comment,
    /// A named constant.
    Constant,
    /// A constructor, such as an enum variant.
    Constructor,
    /// A delimiter, such as a comma or a semicolon.
    Delimiter,
    /// A function name.
    Function,
    /// A language keyword.
    Keyword,
    /// A macro name.
    Macro,
    /// A numeric literal.
    Number,
    /// An operator.
    Operator,
    /// A function parameter.
    Parameter,
    /// A preprocessor directive.
    Preprocessor,
    /// A structure field or a property.
    Property,
    /// A statement keyword.
    Statement,
    /// A string literal.
    String,
    /// A type name.
    Type,
    /// A variable name.
    Variable,
}

/// One bounded highlight range inside one buffer line.
///
/// The range holds byte offsets inside the line, never terminal cells and never
/// a color. The interface layer maps [`SyntaxRole`] to one theme role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighlightSpan {
    /// The zero-based line index.
    pub line: u32,
    /// The first byte of the range inside the line.
    pub start_byte: u32,
    /// The byte after the range inside the line.
    pub end_byte: u32,
    /// The terminal-independent role of the range.
    pub role: SyntaxRole,
}

/// The syntax tree of one buffer version.
///
/// The tree stays opaque, so no other module sees a Tree-sitter type.
#[derive(Clone, Debug)]
pub struct SyntaxTree(Tree);

impl SyntaxTree {
    /// Moves the tree over one applied edit transaction.
    ///
    /// The caller passes the buffer as it was before the transaction, because
    /// the tree describes that text. The moved tree is the reuse input of the
    /// next parse, so a small change does not reparse the complete buffer.
    #[must_use]
    pub fn edited(mut self, before: &TextBuffer, transaction: &EditTransaction) -> Self {
        // The changes ascend, so the edits run backward. Every remaining change
        // then keeps the coordinates that the tree still holds.
        for change in transaction.changes().iter().rev() {
            self.0.edit(&input_edit(before, change));
        }
        self
    }

    /// Returns the number of nodes in the tree.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.0.root_node().descendant_count()
    }
}

/// The exact text of one buffer version, ready for analysis.
#[derive(Clone, Debug)]
pub struct AnalysisInput {
    version: BufferVersion,
    source: Arc<str>,
    previous: Option<SyntaxTree>,
}

impl AnalysisInput {
    /// Creates an input that parses the complete source.
    #[must_use]
    pub const fn new(version: BufferVersion, source: Arc<str>) -> Self {
        Self {
            version,
            source,
            previous: None,
        }
    }

    /// Reuses the moved tree of the previous buffer version.
    ///
    /// Pass the tree of [`BufferSyntax::analysis`] after
    /// [`SyntaxTree::edited`] moved it over the applied transaction.
    #[must_use]
    pub fn reusing(mut self, previous: SyntaxTree) -> Self {
        self.previous = Some(previous);
        self
    }

    /// Returns the buffer version that produced the source.
    #[must_use]
    pub const fn version(&self) -> BufferVersion {
        self.version
    }

    /// Returns the exact source text.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// The complete analysis of one buffer version.
#[derive(Clone)]
pub struct Analysis {
    version: BufferVersion,
    source: Arc<str>,
    tree: SyntaxTree,
    highlights: Vec<HighlightSpan>,
    /// The indent rule of the adapter that produced this result.
    ///
    /// The result carries the rule, so an indent query needs no adapter lookup
    /// and no language of its own.
    indent: IndentRule,
}

impl Analysis {
    /// Returns the buffer version that produced this result.
    #[must_use]
    pub const fn version(&self) -> BufferVersion {
        self.version
    }

    /// Returns the syntax tree of that buffer version.
    #[must_use]
    pub const fn tree(&self) -> &SyntaxTree {
        &self.tree
    }

    /// Returns the highlight spans in ascending line and byte order.
    #[must_use]
    pub fn highlights(&self) -> &[HighlightSpan] {
        &self.highlights
    }

    /// Returns the indent level of a new line that opens at one byte offset.
    ///
    /// A position inside a block gains one level over its enclosing node. A
    /// closing delimiter after the position loses one level.
    ///
    /// # Errors
    ///
    /// Returns [`AnalysisError::MalformedOutput`] when the offset falls outside
    /// the analyzed source or inside a character, and
    /// [`AnalysisError::Bounds`] when the tree is deeper than
    /// [`ANALYSIS_DEPTH_MAX`].
    pub fn indent_level(&self, byte: usize) -> Result<IndentLevel, AnalysisError> {
        analysis::indent_level(&self.tree.0, self.indent, &self.source, byte)
    }
}

impl fmt::Debug for Analysis {
    /// Writes the shape of the result, never the buffer text.
    ///
    /// The result holds a complete buffer copy, so a derived format would print
    /// megabytes into a log or into a panic message.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Analysis")
            .field("version", &self.version)
            .field("source_bytes", &self.source.len())
            .field("highlights", &self.highlights.len())
            .finish_non_exhaustive()
    }
}

/// One language and everything that Kvim needs to serve it.
///
/// An adapter supplies data: the paths that it owns, the Tree-sitter grammar
/// with its highlight query, the comment tokens, and the indent rule. The
/// analysis above the trait knows no language, so a later release adds a
/// language by registering one more adapter.
///
/// The trait stays object-safe, because the registry holds trait objects. A
/// later asynchronous method must return a boxed future for the same reason.
pub trait LanguageAdapter: Send + Sync {
    /// Returns the stable adapter identifier.
    fn id(&self) -> &'static str;

    /// Returns the analysis implementation version.
    fn version(&self) -> &'static str;

    /// Returns the file extensions that this adapter owns.
    ///
    /// The extensions are case-sensitive. An adapter for a language that names
    /// files without an extension overrides
    /// [`LanguageAdapter::supports_path`] instead.
    fn extensions(&self) -> &'static [&'static str];

    /// Returns the comment tokens of the language.
    fn comment(&self) -> CommentStyle;

    /// Returns the Tree-sitter grammar and highlight query of the language.
    fn grammar(&self) -> Grammar;

    /// Returns the indent rule of the language.
    fn indent_rule(&self) -> IndentRule;

    /// Returns the language server of this language, when it declares one.
    ///
    /// The declaration is data: the program, its arguments, the protocol
    /// language identifier, and the initialization options. The session sends
    /// what the declaration names, so a new language server needs only this
    /// method and no change above the adapter boundary.
    ///
    /// The default answer is `None`. A language without a server stays a
    /// normal, fully editable buffer without diagnostics.
    fn language_server(&self) -> Option<LanguageServerDeclaration> {
        None
    }

    /// Reports whether this adapter owns one path.
    ///
    /// The default rule matches the extensions of the adapter.
    fn supports_path(&self, path: &Path) -> bool {
        path.extension().is_some_and(|extension| {
            self.extensions()
                .iter()
                .any(|supported| extension == OsStr::new(supported))
        })
    }

    /// Parses the source and collects bounded highlight spans.
    ///
    /// The job runs on the bounded worker service. It checks the cancellation
    /// token before and after the parse. The default implementation serves
    /// every language through its grammar data.
    ///
    /// # Errors
    ///
    /// Returns [`AnalysisError`] for a cancelled request, a failed parse, a
    /// malformed span, or a source that passes one bound.
    fn analyze(
        &self,
        input: &AnalysisInput,
        cancellation: &CancellationToken,
    ) -> Result<Analysis, AnalysisError> {
        analysis::analyze(self, input, cancellation)
    }
}

/// The language adapters of this editor build.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{AnalysisError, LanguageRegistry};
///
/// let registry = LanguageRegistry::first_release();
/// assert_eq!(registry.adapter(Path::new("lib.rs")).unwrap().id(), "rust");
/// // The first release supports no other language, and the match is
/// // case-sensitive.
/// assert_eq!(
///     registry.adapter(Path::new("notes.txt")).err(),
///     Some(AnalysisError::UnsupportedPath),
/// );
/// assert_eq!(
///     registry.adapter(Path::new("LIB.RS")).err(),
///     Some(AnalysisError::UnsupportedPath),
/// );
/// ```
#[derive(Clone, Copy)]
pub struct LanguageRegistry {
    adapters: &'static [&'static dyn LanguageAdapter],
}

/// The one adapter of the first release.
static RUST: RustAdapter = RustAdapter::new();

/// The registered languages of this editor build.
///
/// This table and the adapter file beside it are the only places that name a
/// language. A later release adds a language by adding one adapter file and one
/// entry here.
static ADAPTERS: [&dyn LanguageAdapter; 1] = [&RUST];

impl LanguageRegistry {
    /// Returns the registry of the first release, which holds one adapter.
    ///
    /// Multi-language support is deferred. A later release adds a language by
    /// registering one more adapter in the table that this constructor names,
    /// or by building a registry with [`LanguageRegistry::new`].
    #[must_use]
    pub const fn first_release() -> Self {
        Self::new(&ADAPTERS)
    }

    /// Creates a registry over an explicit adapter table.
    ///
    /// The table is the one place that names the supported languages.
    #[must_use]
    pub const fn new(adapters: &'static [&'static dyn LanguageAdapter]) -> Self {
        Self { adapters }
    }

    /// Returns the adapter that owns one path.
    ///
    /// # Errors
    ///
    /// Returns [`AnalysisError::UnsupportedPath`] when no adapter owns the
    /// path, and [`AnalysisError::AmbiguousPath`] when more than one does.
    pub fn adapter(&self, path: &Path) -> Result<&'static dyn LanguageAdapter, AnalysisError> {
        let mut found = None;
        for adapter in self.adapters {
            if !adapter.supports_path(path) {
                continue;
            }
            if found.is_some() {
                return Err(AnalysisError::AmbiguousPath);
            }
            found = Some(*adapter);
        }
        found.ok_or(AnalysisError::UnsupportedPath)
    }
}

/// Whether one result reached the visible state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Publication {
    /// The result matched the current buffer version and replaced the cache.
    Accepted,
    /// The result was obsolete. It changed nothing and entered no cache.
    Rejected,
}

/// The newest accepted analysis of one buffer.
///
/// The holder is the publication gate of the language module. An obsolete
/// result never reaches it, so no obsolete tree serves a later query.
///
/// # Examples
///
/// ```
/// use kvim_language::{BufferSyntax, Publication};
///
/// let syntax = BufferSyntax::new();
/// // A buffer without an accepted result renders plain text.
/// assert!(syntax.highlights().is_empty());
/// ```
#[derive(Clone, Debug, Default)]
pub struct BufferSyntax {
    accepted: Option<Analysis>,
}

impl BufferSyntax {
    /// Creates a holder without an accepted result.
    #[must_use]
    pub const fn new() -> Self {
        Self { accepted: None }
    }

    /// Publishes one result while its buffer version is still current.
    ///
    /// A result for an obsolete buffer version changes nothing and enters no
    /// cache, which `docs/responsiveness.md` requires.
    pub fn accept(&mut self, current: BufferVersion, analysis: Analysis) -> Publication {
        if analysis.version() != current {
            return Publication::Rejected;
        }
        self.accepted = Some(analysis);
        Publication::Accepted
    }

    /// Returns the newest accepted result.
    #[must_use]
    pub const fn analysis(&self) -> Option<&Analysis> {
        self.accepted.as_ref()
    }

    /// Returns the highlight spans of the newest accepted result.
    ///
    /// Highlighting is decoration, so the spans of the previous version stay
    /// visible while the next analysis runs.
    #[must_use]
    pub fn highlights(&self) -> &[HighlightSpan] {
        self.accepted.as_ref().map_or(&[][..], Analysis::highlights)
    }

    /// Returns the indent level for a new line at one byte offset.
    ///
    /// The result answers only for the current buffer version. A caller that
    /// receives `None` uses the previous-line fallback instead of waiting for a
    /// parse result, which keeps the terminal event loop free.
    #[must_use]
    pub fn indent_level(&self, current: BufferVersion, byte: usize) -> Option<IndentLevel> {
        let analysis = self.accepted.as_ref()?;
        if analysis.version() != current {
            return None;
        }
        analysis.indent_level(byte).ok()
    }
}

/// Builds one analysis result from parsed parts.
fn analysis(
    input: &AnalysisInput,
    tree: Tree,
    highlights: Vec<HighlightSpan>,
    indent: IndentRule,
) -> Analysis {
    Analysis {
        version: input.version,
        source: Arc::clone(&input.source),
        tree: SyntaxTree(tree),
        highlights,
        indent,
    }
}

/// Returns the reuse tree of one input.
fn previous_tree(input: &AnalysisInput) -> Option<&Tree> {
    input.previous.as_ref().map(|previous| &previous.0)
}

/// Rejects a value above one limit.
fn enforce_count(actual: usize, limit: usize, measure: BoundMeasure) -> Result<(), AnalysisError> {
    if actual > limit {
        return Err(AnalysisError::Bounds {
            measure,
            limit,
            actual,
        });
    }
    Ok(())
}

/// Rejects a source that passes the byte bound or the line bound.
fn validate_source(source: &str) -> Result<(), AnalysisError> {
    enforce_count(source.len(), ANALYSIS_SOURCE_BYTES_MAX, BoundMeasure::Bytes)?;
    let lines = source
        .bytes()
        .filter(|byte| *byte == b'\n')
        .take(ANALYSIS_SOURCE_LINES_MAX + 1)
        .count()
        + usize::from(!source.is_empty());
    enforce_count(lines, ANALYSIS_SOURCE_LINES_MAX, BoundMeasure::Lines)
}

/// Converts one change into the edit that moves the tree.
fn input_edit(before: &TextBuffer, change: &TextChange) -> InputEdit {
    let start = change.range().start();
    let end = change.range().end();
    let start_byte = before.char_to_byte(start).get();
    let start_position = point(before, start);
    let replacement = change.replacement();
    InputEdit {
        start_byte,
        old_end_byte: before.char_to_byte(end).get(),
        new_end_byte: start_byte + replacement.len(),
        start_position,
        old_end_position: point(before, end),
        new_end_position: advanced(start_position, replacement),
    }
}

/// Returns the row and the byte column of one character position.
fn point(buffer: &TextBuffer, position: CharPosition) -> Point {
    let line = buffer.char_to_line(position);
    let line_start = buffer.char_to_byte(buffer.line_start(line)).get();
    Point::new(line.get(), buffer.char_to_byte(position).get() - line_start)
}

/// Returns the position that follows inserted text.
fn advanced(start: Point, text: &str) -> Point {
    match text.rfind('\n') {
        Some(index) => Point::new(
            start.row + text.bytes().filter(|byte| *byte == b'\n').count(),
            text.len() - index - 1,
        ),
        None => Point::new(start.row, start.column + text.len()),
    }
}
