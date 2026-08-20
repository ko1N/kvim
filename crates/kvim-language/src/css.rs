//! The CSS language adapter.
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

/// The file extensions that the CSS adapter owns.
const CSS_EXTENSIONS: [&str; 1] = ["css"];

/// The language names that the CSS adapter answers to.
const CSS_LANGUAGE_NAMES: [&str; 1] = ["css"];

/// The node kinds whose content takes one more indent level in CSS.
///
/// A `block` node spans the braces of a rule set and of an at-rule, and an
/// `arguments` node spans the parentheses of a function call. Each one carries
/// its own opening and closing character, so each one behaves exactly as the
/// equivalent node of a brace language.
const CSS_INDENT_SCOPES: [&str; 2] = ["arguments", "block"];

/// The characters that close a CSS indent scope.
const CSS_CLOSING_DELIMITERS: [char; 2] = [')', '}'];

/// Returns the CSS grammar of the bundled parser.
fn css_language() -> Language {
    tree_sitter_css::LANGUAGE.into()
}

/// Returns the initialization options of `vscode-css-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn css_ls_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the CSS adapter declares, in declaration order.
const CSS_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "cssls",
    program: "vscode-css-language-server",
    args: &["--stdio"],
    language_id: "css",
    // The server supplies document formatting, and `prettier` formats every
    // buffer of this language.
    formatting: ServerFormatting::Disabled,
    // The server reads a single stylesheet as well as a complete project, so no
    // marker gates its start.
    root_markers: &[],
    initialization_options: css_ls_options,
    workspace_settings: None,
}];

/// The external formatter of the CSS adapter.
///
/// `prettier` takes the document on standard input, and `--stdin-filepath`
/// carries the path that selects its parser.
const CSS_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
        FormatterArgument::Literal("--stdin-filepath"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for CSS stylesheet paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{CssAdapter, LanguageAdapter};
///
/// let adapter = CssAdapter::new();
/// assert!(adapter.supports_path(Path::new("assets/site.css")));
/// // CSS defines no line comment, so the comment toggle stays disabled.
/// assert_eq!(adapter.comment().line_token(), None);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CssAdapter;

impl CssAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for CssAdapter {
    fn id(&self) -> &'static str {
        "css"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &CSS_EXTENSIONS
    }

    fn language_names(&self) -> &'static [&'static str] {
        &CSS_LANGUAGE_NAMES
    }

    fn comment(&self) -> CommentStyle {
        // CSS defines a block comment alone, so the metadata carries no line
        // token and the first-release toggle stays disabled.
        CommentStyle::new(None, Some(BlockComment::new("/*", "*/")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "css",
            language: css_language,
            highlights_query: tree_sitter_css::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &CSS_INDENT_SCOPES,
            closing_delimiters: &CSS_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &CSS_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&CSS_FORMATTER)
    }
}
