//! The language adapter registry and the language-neutral Tree-sitter analysis.
//! Adapted from ReviewGraph (MIT), src/analysis.rs.
//!
//! The adapter boundary is the multi-language extension point of kvim. An
//! adapter supplies data: one [`LanguageCatalogEntry`], the comment tokens, the
//! indent rule, the language servers, and the external formatter. Nothing above
//! the trait names a language, so a release adds a language by registering one
//! more adapter. This build registers 25 adapters, which
//! `docs/language-services.md` names.
//!
//! The catalog entry owns what selects and parses one language: the language
//! names, the file extensions, the complete file names, and the Tree-sitter
//! grammar with its queries. The adapter owns what a grammar cannot answer, so
//! indentation, formatter, server, and editor-version behavior stays with the
//! adapter and no lookup table exists twice.
//!
//! Only an adapter can select a path by language, by file extension, or by file
//! name, and only an adapter answers to the name of a language. Generic
//! `kvim-core`, `kvim-editor`, `kvim-runtime`, `kvim-terminal`, `kvim-tui`, and
//! `kvim-workspace` code passes a path and exact buffer content, and never
//! inspects a lookup key.
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
//! holds one persistent session for each declared server of the workspace, and
//! it delivers every result as a [`LanguageEvent`] that names its server. The
//! client speaks the protocol only. An adapter declares its servers through
//! [`LanguageAdapter::language_servers`], so no code above the adapter boundary
//! names a server product. One adapter may declare several servers, and
//! `docs/language-services.md` owns the rules that merge their answers.
//!
//! An adapter also declares the external formatter of its language through
//! [`LanguageAdapter::external_formatter`]. That program takes precedence over
//! a formatting server, so [`LanguageAdapter::formatter`] names the one path
//! that a format-on-save runs. See `docs/language-services.md`.
//!
//! A server answer may carry markdown. [`MarkupDocument`] reads that text into
//! blocks of styled text, and it answers a [`MarkupRole`] for each stretch of
//! it. The parse is pure, and it names no color, no glyph, and no terminal
//! cell, because `kvim-tui` owns every one of them. The code of one fence
//! carries the [`SyntaxRole`] values of the one highlighter, so one text
//! carries one set of roles in a fence and in a buffer. See
//! `docs/language-services.md`.
//!
//! [`LanguageAdapter`] stays object-safe, so the registry holds one adapter for
//! each language. A later asynchronous method must return a boxed future
//! instead of using `async fn`, which would break object safety.
//!
//! # Examples
//!
//! ```
//! use std::path::Path;
//! use std::sync::{Arc, OnceLock};
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
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tree_sitter::{InputEdit, Point, Tree};

use kvim_core::{BufferVersion, CharPosition, EditTransaction, TextBuffer, TextChange};

mod analysis;
#[cfg(feature = "grammar-asm")]
mod asm;
#[cfg(feature = "grammar-bash")]
mod bash;
#[cfg(feature = "grammar-c")]
mod c;
#[cfg(feature = "grammar-cpp")]
mod cpp;
#[cfg(feature = "grammar-css")]
mod css;
mod document;
#[cfg(any(
    feature = "grammar-javascript",
    feature = "grammar-tsx",
    feature = "grammar-typescript",
))]
mod ecma;
mod encoding;
#[cfg(feature = "grammar-fish")]
mod fish;
mod formatter;
#[cfg(feature = "grammar-glsl")]
mod glsl;
#[cfg(feature = "grammar-go")]
mod go;
#[cfg(feature = "grammar-html")]
mod html;
#[cfg(feature = "grammar-javascript")]
mod javascript;
#[cfg(feature = "grammar-json")]
mod json;
#[cfg(feature = "grammar-lua")]
mod lua;
#[cfg(feature = "grammar-markdown")]
mod markdown;
mod markup;
#[cfg(feature = "grammar-nix")]
mod nix;
mod progress;
mod protocol;
#[cfg(feature = "grammar-python")]
mod python;
#[cfg(feature = "grammar-rust")]
mod rust;
#[cfg(feature = "grammar-scss")]
mod scss;
mod server;
mod services;
mod session;
#[cfg(feature = "grammar-sql")]
mod sql;
#[cfg(feature = "grammar-terraform")]
mod terraform;
#[cfg(feature = "grammar-toml")]
mod toml;
#[cfg(feature = "grammar-tsx")]
mod tsx;
#[cfg(feature = "grammar-typescript")]
mod typescript;
#[cfg(feature = "grammar-xml")]
mod xml;
#[cfg(feature = "grammar-yaml")]
mod yaml;
#[cfg(feature = "grammar-zig")]
mod zig;

#[cfg(any(test, feature = "test-support"))]
pub mod mock;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod tests;

#[cfg(feature = "grammar-asm")]
pub use asm::AsmAdapter;
#[cfg(feature = "grammar-bash")]
pub use bash::BashAdapter;
#[cfg(feature = "grammar-c")]
pub use c::CAdapter;
#[cfg(feature = "grammar-cpp")]
pub use cpp::CppAdapter;
#[cfg(feature = "grammar-css")]
pub use css::CssAdapter;
pub use document::{
    ContentChange, Diagnostic, DiagnosticSet, DiagnosticSeverity, FormatEdits, MarkupKind,
    MarkupText, SourceLocation, TextEdit,
};
#[cfg(feature = "grammar-fish")]
pub use fish::FishAdapter;
pub use formatter::{
    FORMATTER_ARGS_MAX, FORMATTER_DEADLINE, FORMATTER_OUTPUT_BYTES_MAX, FormattedDocument,
    FormatterArgument, FormatterDeclaration, FormatterFailure, FormatterRequest, LanguageFormatter,
};
#[cfg(feature = "grammar-glsl")]
pub use glsl::GlslAdapter;
#[cfg(feature = "grammar-go")]
pub use go::GoAdapter;
#[cfg(feature = "grammar-html")]
pub use html::HtmlAdapter;
#[cfg(feature = "grammar-javascript")]
pub use javascript::JavascriptAdapter;
#[cfg(feature = "grammar-json")]
pub use json::JsonAdapter;
pub use kvim_syntax::{
    Grammar, HighlightLimits, HighlightSpan, Highlighted, LanguageCatalogEntry, LimitKind,
    SyntaxHighlighter, SyntaxRole, Truncation,
};
#[cfg(feature = "grammar-lua")]
pub use lua::LuaAdapter;
#[cfg(feature = "grammar-markdown")]
pub use markdown::MarkdownAdapter;
pub use markup::{
    MARKUP_BLOCKS_MAX, MARKUP_FENCE_SOURCE_BYTES_MAX, MARKUP_FENCE_SPANS_MAX, MARKUP_FENCES_MAX,
    MARKUP_NESTING_DEPTH_MAX, MARKUP_PIECES_MAX, MARKUP_SOURCE_BYTES_MAX, MarkupBlock, MarkupBody,
    MarkupContainer, MarkupDocument, MarkupMarker, MarkupRole, StyledMarkup,
};
#[cfg(feature = "grammar-nix")]
pub use nix::NixAdapter;
pub use progress::{
    LSP_PROGRESS_CHARS_MAX, ProgressPercentage, ProgressReport, ProgressStage, ProgressToken,
    SessionGeneration,
};
pub use protocol::{
    DocumentPosition, LSP_HEADER_BYTES_MAX, LSP_INPUT_BYTES_MAX, LSP_MESSAGE_BYTES_MAX,
    LSP_MESSAGES_MAX, LSP_OUTPUT_BYTES_MAX, LSP_REQUESTS_MAX, LspBound, LspError, SourceSpan,
    WorkspaceRoot,
};
#[cfg(feature = "grammar-python")]
pub use python::PythonAdapter;
#[cfg(feature = "grammar-rust")]
pub use rust::RustAdapter;
#[cfg(feature = "grammar-scss")]
pub use scss::ScssAdapter;
pub use server::{
    LANGUAGE_ROOT_MARKERS_MAX, LANGUAGE_SERVERS_MAX, LanguageServerDeclaration, LanguageServerId,
    ServerFormatting,
};
pub use services::{LSP_SESSIONS_MAX, LanguageServices};
pub use session::{
    LSP_CONFIGURATION_ITEMS_MAX, LSP_CONTENT_CHANGES_MAX, LSP_DIAGNOSTIC_DEADLINE,
    LSP_DIAGNOSTIC_PULL_DELAY, LSP_DIAGNOSTICS_MAX, LSP_EVENT_QUEUE_CAPACITY, LSP_FORMAT_DEADLINE,
    LSP_FORMAT_EDITS_MAX, LSP_HOVER_BYTES_MAX, LSP_INITIALIZE_DEADLINE, LSP_LOCATIONS_MAX,
    LSP_OPEN_DOCUMENTS_MAX, LSP_PENDING_REQUESTS_MAX, LSP_REQUEST_DEADLINE,
    LSP_REQUEST_QUEUE_CAPACITY, LSP_RESTARTS_MAX, LSP_RESULT_ID_BYTES_MAX, LSP_SHUTDOWN_DEADLINE,
    LSP_STDERR_BYTES_MAX, LSP_STDERR_LINE_BYTES_MAX, LanguageEvent, LanguageOutcome,
    LanguageRequestId, LanguageServerHandle, ServerReport,
};
#[cfg(feature = "grammar-sql")]
pub use sql::SqlAdapter;
#[cfg(feature = "grammar-terraform")]
pub use terraform::TerraformAdapter;
#[cfg(feature = "grammar-toml")]
pub use toml::TomlAdapter;
#[cfg(feature = "grammar-tsx")]
pub use tsx::TsxAdapter;
#[cfg(feature = "grammar-typescript")]
pub use typescript::TypescriptAdapter;
#[cfg(feature = "grammar-xml")]
pub use xml::XmlAdapter;
#[cfg(feature = "grammar-yaml")]
pub use yaml::YamlAdapter;
#[cfg(feature = "grammar-zig")]
pub use zig::ZigAdapter;

/// The largest source that one analysis reads, in bytes.
pub const ANALYSIS_SOURCE_BYTES_MAX: usize = 4 * 1024 * 1024;

/// The largest source that one analysis reads, in lines.
pub const ANALYSIS_SOURCE_LINES_MAX: usize = 100_000;

/// The largest syntax tree that one analysis publishes, in nodes.
pub const ANALYSIS_NODES_MAX: usize = 1_000_000;

/// The largest number of ancestors that the indent query walks.
pub const ANALYSIS_DEPTH_MAX: usize = 128;

/// The largest number of highlight spans that one analysis publishes.
///
/// The densest measured real source produces one span for each 5.8 bytes, so
/// [`ANALYSIS_SOURCE_BYTES_MAX`] produces about 727000 spans. One span holds 16
/// bytes, so this bound retains 12 MB for one buffer.
pub const ANALYSIS_HIGHLIGHT_SPANS_MAX: usize = 750_000;

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
    /// The complete result exceeds one bound. kvim publishes no partial result.
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

impl From<kvim_syntax::HighlightFailure> for AnalysisError {
    /// Maps one highlighter outcome onto the analysis vocabulary.
    ///
    /// The editor keeps its own failure names, because a buffer analysis also
    /// parses and reads an indent rule, which the highlighter never does.
    fn from(failure: kvim_syntax::HighlightFailure) -> Self {
        match failure {
            kvim_syntax::HighlightFailure::UnsupportedLanguage => Self::UnsupportedPath,
            kvim_syntax::HighlightFailure::SourceTooLarge { bytes, max_bytes } => Self::Bounds {
                measure: BoundMeasure::Bytes,
                limit: max_bytes,
                actual: bytes,
            },
            kvim_syntax::HighlightFailure::GrammarSetup => Self::ParserSetup,
            kvim_syntax::HighlightFailure::ParseFailure => Self::ParseFailure,
            kvim_syntax::HighlightFailure::Cancelled => Self::Cancelled,
            kvim_syntax::HighlightFailure::MalformedRanges => Self::MalformedOutput,
        }
    }
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
/// language. A language that defines no comment uses [`CommentStyle::none`],
/// and the comment toggle then stays disabled and reports the reason, which is
/// the same path that a file without an adapter takes. See
/// `docs/language-services.md`.
///
/// # Examples
///
/// ```
/// use kvim_language::{BlockComment, CommentStyle};
///
/// let rust = CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")));
/// assert_eq!(rust.line_token(), Some("//"));
/// assert_eq!(rust.block().map(|block| block.close), Some("*/"));
///
/// let json = CommentStyle::none();
/// assert_eq!(json.line_token(), None);
/// assert_eq!(json.block(), None);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommentStyle {
    line: Option<&'static str>,
    block: Option<BlockComment>,
}

impl CommentStyle {
    /// Creates the comment metadata of one language.
    ///
    /// Pass the line token that the language defines. A language that defines
    /// no comment uses [`CommentStyle::none`], because an empty token would
    /// comment a line out with nothing and would still enable the toggle.
    #[must_use]
    pub const fn new(line: Option<&'static str>, block: Option<BlockComment>) -> Self {
        debug_assert!(
            !matches!(line, Some(token) if token.is_empty()),
            "an empty line token cannot comment a line out, so a language without a comment uses CommentStyle::none"
        );
        Self { line, block }
    }

    /// Creates the comment metadata of a language that defines no comment.
    ///
    /// The comment toggle then stays disabled and reports the reason. JSON and
    /// Markdown define no comment of their own, so neither one can carry a
    /// token.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            line: None,
            block: None,
        }
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

/// Reports whether one case-sensitive lookup key stands in a selection table.
fn owns(keys: &[&'static str], value: &OsStr) -> bool {
    keys.iter().any(|key| value == OsStr::new(key))
}

/// One language and everything that kvim needs to serve it.
///
/// An adapter supplies data: the paths that it owns, the Tree-sitter grammar
/// with its highlight query, the comment tokens, and the indent rule. The
/// analysis above the trait knows no language, so a later release adds a
/// language by registering one more adapter.
///
/// The trait stays object-safe, because the registry holds trait objects. A
/// later asynchronous method must return a boxed future for the same reason.
pub trait LanguageAdapter: Send + Sync {
    /// Returns the catalog entry of the language.
    ///
    /// The entry owns the lookup keys and the grammar, so an adapter names each
    /// of them once. The adapter itself owns only what a catalog entry cannot
    /// answer: the comment tokens, the indent rule, the language servers, the
    /// external formatter, and the analysis version.
    fn catalog(&self) -> &'static LanguageCatalogEntry;

    /// Returns the analysis implementation version.
    fn version(&self) -> &'static str;

    /// Returns the stable adapter identifier.
    ///
    /// The identifier is the one that the catalog entry carries, so an adapter
    /// and its grammar can never answer to two names.
    fn id(&self) -> &'static str {
        self.catalog().id()
    }

    /// Returns the file extensions that this adapter owns.
    ///
    /// The extensions are case-sensitive.
    fn extensions(&self) -> &'static [&'static str] {
        self.catalog().extensions()
    }

    /// Returns the complete file names that this adapter owns.
    ///
    /// A file name is the second lookup key of the same selection, for a file
    /// whose extension does not name its format. `flake.lock` holds JSON, so
    /// the JSON catalog entry names that file. The names are case-sensitive,
    /// and they carry no directory.
    fn file_names(&self) -> &'static [&'static str] {
        self.catalog().file_names()
    }

    /// Returns the names of the language that this adapter answers to.
    ///
    /// A language name is the third lookup key of the adapter, and it needs no
    /// path. A markdown fence names its language in an info string, so a fence
    /// reaches its adapter through this key alone. The table holds the name of
    /// the language and the aliases that an author or a server writes for it,
    /// for example `rs` beside `rust`. Every name stands in lower case, and
    /// exactly one adapter of the registry owns each name. See
    /// `docs/language-services.md`.
    fn language_names(&self) -> &'static [&'static str] {
        self.catalog().language_names()
    }

    /// Returns the comment tokens of the language.
    fn comment(&self) -> CommentStyle;

    /// Returns the Tree-sitter grammar and highlight query of the language.
    fn grammar(&self) -> Grammar {
        self.catalog().grammar()
    }

    /// Returns the indent rule of the language.
    fn indent_rule(&self) -> IndentRule;

    /// Returns the language servers of this language, in declaration order.
    ///
    /// A declaration is data: the identifier, the program, its arguments, the
    /// protocol language identifier, the formatting role, the workspace root
    /// markers, and the initialization options. The session sends what the
    /// declaration names, so a new language server needs only this method and
    /// no change above the adapter boundary.
    ///
    /// The table holds at most [`LANGUAGE_SERVERS_MAX`] declarations, every
    /// identifier is unique inside the adapter, and at most one declaration
    /// formats. Each declaration names at most [`LANGUAGE_ROOT_MARKERS_MAX`]
    /// root markers. The default answer is the empty table. A language without
    /// a server stays a normal, fully editable buffer without diagnostics.
    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &[]
    }

    /// Returns the external formatter of this language.
    ///
    /// A declaration is data: the program, and its arguments in command order.
    /// The editor runs what the declaration names, so a new formatter needs
    /// only this method and no change above the adapter boundary. The default
    /// answer is none, which leaves the formatting server of the language in
    /// charge.
    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        None
    }

    /// Returns the formatter that formats the buffers of this language.
    ///
    /// An external formatter takes precedence. The declared server formats only
    /// while the adapter names no program, so `ServerFormatting::Enabled` is the
    /// fallback role of its adapter. At most one declaration carries that role,
    /// so the answer names one formatter or none. See
    /// `docs/language-services.md`.
    fn formatter(&self) -> Option<LanguageFormatter> {
        if let Some(declaration) = self.external_formatter() {
            return Some(LanguageFormatter::External(declaration));
        }
        self.language_servers()
            .iter()
            .find(|declaration| declaration.formatting == ServerFormatting::Enabled)
            .map(LanguageFormatter::Server)
    }

    /// Reports whether this adapter owns one path.
    ///
    /// The rule reads the two lookup keys of one selection: the extension of
    /// the path and its complete file name. One selection path therefore
    /// serves both keys, and no caller above the boundary learns either one.
    fn supports_path(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| owns(self.extensions(), extension))
            || path
                .file_name()
                .is_some_and(|name| owns(self.file_names(), name))
    }

    /// Reports whether this adapter answers to one language name.
    ///
    /// The rule reads the third lookup key, which carries no path. The match
    /// folds ASCII case, because the name is prose that a server writes, and a
    /// path is a filesystem entity where the case names a different file. A
    /// name that no adapter declares matches nothing, which is no failure.
    ///
    /// The caller passes one complete name. A CommonMark info string may carry
    /// an attribute after the name, and the reader of the fence extracts the
    /// name. A longer text therefore matches nothing, because every comparison
    /// rejects a length that no declared name holds.
    fn supports_language(&self, language: &str) -> bool {
        self.language_names()
            .iter()
            .any(|name| name.eq_ignore_ascii_case(language))
    }

    /// Parses the source and collects bounded highlight spans.
    ///
    /// The job runs on the bounded worker service. It checks the cancellation
    /// token before and after the parse. The default implementation serves
    /// every language through its catalog entry.
    ///
    /// The caller owns `highlighter`, which keeps the compiled query of each
    /// language that it served and releases every one of them when it drops.
    ///
    /// # Errors
    ///
    /// Returns [`AnalysisError`] for a cancelled request, a failed parse, a
    /// malformed span, or a source that passes one bound.
    fn analyze(
        &self,
        input: &AnalysisInput,
        highlighter: &mut SyntaxHighlighter,
        cancellation: &CancellationToken,
    ) -> Result<Analysis, AnalysisError> {
        analysis::analyze(self, input, highlighter, cancellation)
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
/// // A file name selects an adapter as an extension does.
/// assert_eq!(registry.adapter(Path::new("flake.lock")).unwrap().id(), "json");
/// // A registered language is the only match, and the match is
/// // case-sensitive.
/// assert_eq!(
///     registry.adapter(Path::new("notes.txt")).err(),
///     Some(AnalysisError::UnsupportedPath),
/// );
/// assert_eq!(
///     registry.adapter(Path::new("LIB.RS")).err(),
///     Some(AnalysisError::UnsupportedPath),
/// );
/// // A language name selects an adapter without a path, and that match folds
/// // ASCII case.
/// assert_eq!(registry.adapter_of_language("Rust").unwrap().id(), "rust");
/// // A name that no adapter declares selects nothing, which is no failure.
/// assert!(registry.adapter_of_language("console").is_none());
/// ```
#[derive(Clone, Copy)]
pub struct LanguageRegistry {
    adapters: &'static [&'static dyn LanguageAdapter],
}

/// The assembly adapter of this build.
#[cfg(feature = "grammar-asm")]
static ASM: AsmAdapter = AsmAdapter::new();

/// The Bash adapter of this build.
#[cfg(feature = "grammar-bash")]
static BASH: BashAdapter = BashAdapter::new();

/// The C adapter of this build.
#[cfg(feature = "grammar-c")]
static C: CAdapter = CAdapter::new();

/// The C++ adapter of this build.
#[cfg(feature = "grammar-cpp")]
static CPP: CppAdapter = CppAdapter::new();

/// The CSS adapter of this build.
#[cfg(feature = "grammar-css")]
static CSS: CssAdapter = CssAdapter::new();

/// The fish adapter of this build.
#[cfg(feature = "grammar-fish")]
static FISH: FishAdapter = FishAdapter::new();

/// The GLSL adapter of this build.
#[cfg(feature = "grammar-glsl")]
static GLSL: GlslAdapter = GlslAdapter::new();

/// The Go adapter of this build.
#[cfg(feature = "grammar-go")]
static GO: GoAdapter = GoAdapter::new();

/// The HTML adapter of this build.
#[cfg(feature = "grammar-html")]
static HTML: HtmlAdapter = HtmlAdapter::new();

/// The JavaScript adapter of this build.
#[cfg(feature = "grammar-javascript")]
static JAVASCRIPT: JavascriptAdapter = JavascriptAdapter::new();

/// The JSON adapter of this build.
#[cfg(feature = "grammar-json")]
static JSON: JsonAdapter = JsonAdapter::new();

/// The Lua adapter of this build.
#[cfg(feature = "grammar-lua")]
static LUA: LuaAdapter = LuaAdapter::new();

/// The Markdown adapter of this build.
#[cfg(feature = "grammar-markdown")]
static MARKDOWN: MarkdownAdapter = MarkdownAdapter::new();

/// The Nix adapter of this build.
#[cfg(feature = "grammar-nix")]
static NIX: NixAdapter = NixAdapter::new();

/// The Python adapter of this build.
#[cfg(feature = "grammar-python")]
static PYTHON: PythonAdapter = PythonAdapter::new();

/// The Rust adapter of this build.
#[cfg(feature = "grammar-rust")]
static RUST: RustAdapter = RustAdapter::new();

/// The SCSS adapter of this build.
#[cfg(feature = "grammar-scss")]
static SCSS: ScssAdapter = ScssAdapter::new();

/// The SQL adapter of this build.
#[cfg(feature = "grammar-sql")]
static SQL: SqlAdapter = SqlAdapter::new();

/// The Terraform adapter of this build.
#[cfg(feature = "grammar-terraform")]
static TERRAFORM: TerraformAdapter = TerraformAdapter::new();

/// The TOML adapter of this build.
#[cfg(feature = "grammar-toml")]
static TOML: TomlAdapter = TomlAdapter::new();

/// The TSX adapter of this build.
#[cfg(feature = "grammar-tsx")]
static TSX: TsxAdapter = TsxAdapter::new();

/// The TypeScript adapter of this build.
#[cfg(feature = "grammar-typescript")]
static TYPESCRIPT: TypescriptAdapter = TypescriptAdapter::new();

/// The XML adapter of this build.
#[cfg(feature = "grammar-xml")]
static XML: XmlAdapter = XmlAdapter::new();

/// The YAML adapter of this build.
#[cfg(feature = "grammar-yaml")]
static YAML: YamlAdapter = YamlAdapter::new();

/// The Zig adapter of this build.
#[cfg(feature = "grammar-zig")]
static ZIG: ZigAdapter = ZigAdapter::new();

/// The registered languages of this editor build.
///
/// This table and the adapter files beside it are the only places that name a
/// language. A later release adds a language by adding one adapter file and one
/// entry here.
///
/// Exactly one adapter owns each extension and each file name. Two owners make
/// every path of that key an ambiguous failure, which leaves the buffer without
/// highlighting, without a server, and without a formatter. Exactly one adapter
/// owns each language name for the same reason.
fn registered_adapters() -> &'static [&'static dyn LanguageAdapter] {
    static ADAPTERS: OnceLock<Vec<&'static dyn LanguageAdapter>> = OnceLock::new();
    ADAPTERS.get_or_init(|| {
        #[allow(unused_mut)]
        let mut adapters: Vec<&'static dyn LanguageAdapter> = Vec::new();
        #[cfg(feature = "grammar-asm")]
        adapters.push(&ASM as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-bash")]
        adapters.push(&BASH as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-c")]
        adapters.push(&C as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-cpp")]
        adapters.push(&CPP as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-css")]
        adapters.push(&CSS as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-fish")]
        adapters.push(&FISH as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-glsl")]
        adapters.push(&GLSL as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-go")]
        adapters.push(&GO as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-html")]
        adapters.push(&HTML as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-javascript")]
        adapters.push(&JAVASCRIPT as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-json")]
        adapters.push(&JSON as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-lua")]
        adapters.push(&LUA as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-markdown")]
        adapters.push(&MARKDOWN as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-nix")]
        adapters.push(&NIX as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-python")]
        adapters.push(&PYTHON as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-rust")]
        adapters.push(&RUST as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-scss")]
        adapters.push(&SCSS as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-sql")]
        adapters.push(&SQL as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-terraform")]
        adapters.push(&TERRAFORM as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-toml")]
        adapters.push(&TOML as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-tsx")]
        adapters.push(&TSX as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-typescript")]
        adapters.push(&TYPESCRIPT as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-xml")]
        adapters.push(&XML as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-yaml")]
        adapters.push(&YAML as &dyn LanguageAdapter);
        #[cfg(feature = "grammar-zig")]
        adapters.push(&ZIG as &dyn LanguageAdapter);
        adapters
    })
}

impl LanguageRegistry {
    /// Returns the registry of this build.
    ///
    /// The table holds one adapter for assembly, Bash, C, C++, CSS, fish,
    /// GLSL, Go, HTML, JavaScript, JSON, Lua, Markdown, Nix, Python, Rust,
    /// SCSS, SQL, Terraform, TOML, TSX, TypeScript, XML, YAML, and Zig. A
    /// later release adds a language by registering one more adapter in the
    /// table that this constructor names, or by building a registry with
    /// [`LanguageRegistry::new`].
    #[must_use]
    pub fn first_release() -> Self {
        Self::new(registered_adapters())
    }

    /// Creates a registry over an explicit adapter table.
    ///
    /// The table is the one place that names the supported languages.
    #[must_use]
    pub const fn new(adapters: &'static [&'static dyn LanguageAdapter]) -> Self {
        Self { adapters }
    }

    /// Returns the adapter table of this registry.
    ///
    /// The workspace-root probe of [`LanguageServices`] reads the declared
    /// root markers of every adapter once, so it needs the complete table.
    #[must_use]
    pub const fn adapters(&self) -> &'static [&'static dyn LanguageAdapter] {
        self.adapters
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

    /// Returns the adapter that answers to one language name.
    ///
    /// The lookup reads the third key of the selection, so it needs no path. A
    /// markdown fence names its language in an info string, and the caller
    /// passes that name alone. The match folds ASCII case.
    ///
    /// A name that no adapter declares selects nothing. That answer is no
    /// failure, because a fence may name any language of the world, and such a
    /// fence renders as plain code. See `docs/language-services.md`.
    #[must_use]
    pub fn adapter_of_language(&self, language: &str) -> Option<&'static dyn LanguageAdapter> {
        let mut found = None;
        for adapter in self.adapters {
            if !adapter.supports_language(language) {
                continue;
            }
            debug_assert!(
                found.is_none(),
                "the registry table gives each language name one adapter, and the registry test proves it"
            );
            found = Some(*adapter);
        }
        found
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
