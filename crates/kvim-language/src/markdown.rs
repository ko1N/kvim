//! The Markdown language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, and the indent rule. See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use serde_json::{Value, json};

use kvim_settings::LanguageSettings;

use super::{
    CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule, LanguageAdapter,
    LanguageCatalogEntry, LanguageServerDeclaration, ServerFormatting,
};

/// The number of columns that one Markdown indent level takes.
const MARKDOWN_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

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
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("markdown")
                .expect("the grammar-markdown feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // Markdown defines no comment of its own. An HTML comment is HTML, and
        // the block toggle is deferred, so the adapter carries no token.
        CommentStyle::none()
    }

    fn indent_rule(&self) -> IndentRule {
        // Markdown nests through marker text and leading spaces, never through
        // a bracketed node, so no node kind adds an indent level and no
        // character closes one.
        IndentRule {
            scopes: &[],
            width: MARKDOWN_INDENT_WIDTH,
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
