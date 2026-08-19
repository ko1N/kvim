//! The HTML language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, Grammar, IndentRule,
    LanguageAdapter, LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the HTML adapter owns.
const HTML_EXTENSIONS: [&str; 2] = ["htm", "html"];

/// The node kinds whose content takes one more indent level in HTML.
///
/// An `element` node spans the start tag, the content, and the end tag, so one
/// entry names the whole construct. A `script_element` and a `style_element`
/// carry raw text between the same two tags, and each one indents that text the
/// same way.
const HTML_INDENT_SCOPES: [&str; 3] = ["element", "script_element", "style_element"];

/// The characters that close an HTML indent scope.
///
/// An end tag opens with the same `<` character as a start tag, so no single
/// character separates the two. The table therefore stays empty, and a line
/// that holds an end tag reports one indent level too many. The user corrects
/// that line, exactly as the documented limit of the Python indent rule works.
const HTML_CLOSING_DELIMITERS: [char; 0] = [];

/// Returns the HTML grammar of the bundled parser.
fn html_language() -> Language {
    tree_sitter_html::LANGUAGE.into()
}

/// Returns the initialization options of `vscode-html-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn html_ls_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the HTML adapter declares, in declaration order.
const HTML_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "html",
    program: "vscode-html-language-server",
    args: &["--stdio"],
    language_id: "html",
    // The server supplies document formatting, and `prettier` formats every
    // buffer of this language.
    formatting: ServerFormatting::Disabled,
    // The server reads a single document as well as a complete project, so no
    // marker gates its start.
    root_markers: &[],
    initialization_options: html_ls_options,
    workspace_settings: None,
}];

/// The external formatter of the HTML adapter.
///
/// `prettier` takes the document on standard input, and `--stdin-filepath`
/// carries the path that selects its parser.
const HTML_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
        FormatterArgument::Literal("--stdin-filepath"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for HTML document paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{HtmlAdapter, LanguageAdapter};
///
/// let adapter = HtmlAdapter::new();
/// assert!(adapter.supports_path(Path::new("public/index.html")));
/// // HTML defines no line comment, so the comment toggle stays disabled.
/// assert_eq!(adapter.comment().line_token(), None);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HtmlAdapter;

impl HtmlAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for HtmlAdapter {
    fn id(&self) -> &'static str {
        "html"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &HTML_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        // HTML defines a block comment alone, so the metadata carries no line
        // token and the first-release toggle stays disabled.
        CommentStyle::new(None, Some(BlockComment::new("<!--", "-->")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "html",
            language: html_language,
            highlights_query: tree_sitter_html::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &HTML_INDENT_SCOPES,
            closing_delimiters: &HTML_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &HTML_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&HTML_FORMATTER)
    }
}
