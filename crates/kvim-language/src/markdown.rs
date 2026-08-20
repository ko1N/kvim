//! The Markdown language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, and the indent rule. See `docs/language-services.md`.

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    CommentStyle, FormatterArgument, FormatterDeclaration, Grammar, IndentRule, LanguageAdapter,
    LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the Markdown adapter owns.
const MARKDOWN_EXTENSIONS: [&str; 2] = ["markdown", "md"];

/// Returns the Markdown block grammar of the bundled parser.
///
/// The parser splits Markdown into a block grammar and an inline grammar. kvim
/// resolves no grammar injection yet, so the block grammar is the whole
/// analysis, and it carries every structural highlight of a document.
fn markdown_language() -> Language {
    tree_sitter_md::LANGUAGE.into()
}

/// Returns the initialization options of `marksman`.
///
/// `marksman` needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn marksman_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the Markdown adapter declares, in declaration order.
const MARKDOWN_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "marksman",
    program: "marksman",
    args: &["server"],
    language_id: "markdown",
    formatting: ServerFormatting::Enabled,
    // The server fits every workspace that holds a Markdown file, so no marker
    // gates its start.
    root_markers: &[],
    initialization_options: marksman_options,
    workspace_settings: None,
}];

/// The external formatter of the Markdown adapter.
///
/// `prettier` reads the document on standard input, and it selects its parser
/// from the file name. The declaration therefore names the place of the
/// document path beside the flag that carries it.
const MARKDOWN_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
        FormatterArgument::Literal("--stdin-filepath"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for Markdown documents.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, MarkdownAdapter};
///
/// let adapter = MarkdownAdapter::new();
/// assert!(adapter.supports_path(Path::new("README.md")));
/// assert!(adapter.supports_path(Path::new("notes.markdown")));
/// // Markdown defines no comment, so the comment toggle stays disabled.
/// assert_eq!(adapter.comment().line_token(), None);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MarkdownAdapter;

impl MarkdownAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for MarkdownAdapter {
    fn id(&self) -> &'static str {
        "markdown"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &MARKDOWN_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        // Markdown defines no comment of its own. An HTML comment is HTML, and
        // the block toggle is deferred, so the adapter carries no token.
        CommentStyle::none()
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "markdown",
            language: markdown_language,
            highlights_query: tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        // Markdown nests through marker text and leading spaces, never through
        // a bracketed node, so no node kind adds an indent level and no
        // character closes one.
        IndentRule {
            scopes: &[],
            closing_delimiters: &[],
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &MARKDOWN_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&MARKDOWN_FORMATTER)
    }
}
