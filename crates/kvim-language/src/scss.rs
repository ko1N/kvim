//! The SCSS language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::sync::OnceLock;

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, Grammar, IndentRule,
    LanguageAdapter, LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the SCSS adapter owns.
const SCSS_EXTENSIONS: [&str; 1] = ["scss"];

/// The language names that the SCSS adapter answers to.
const SCSS_LANGUAGE_NAMES: [&str; 1] = ["scss"];

/// The node kinds whose content takes one more indent level in SCSS.
///
/// SCSS adds the `parameters` node of a mixin and of a function to the two
/// bracketed nodes of CSS. Each one carries its own opening and closing
/// character, so each one behaves exactly as the equivalent node of a brace
/// language.
const SCSS_INDENT_SCOPES: [&str; 3] = ["arguments", "block", "parameters"];

/// The characters that close an SCSS indent scope.
const SCSS_CLOSING_DELIMITERS: [char; 2] = [')', '}'];

/// Returns the SCSS grammar of the bundled parser.
///
/// The crate predates the `LanguageFn` convention of the newer grammar crates,
/// so it exports a function that returns the grammar and no `LANGUAGE`
/// constant.
fn scss_language() -> Language {
    tree_sitter_scss::language()
}

/// The joined highlight query of the SCSS adapter.
///
/// The crate ships the SCSS patterns alone, because the upstream query inherits
/// the CSS patterns. kvim resolves no query inheritance, so the adapter joins
/// the two texts once. The CSS patterns come first, so an SCSS pattern of the
/// same node takes precedence. The SCSS grammar is a superset of the CSS
/// grammar, so every CSS pattern names a node kind that the SCSS grammar holds.
static SCSS_HIGHLIGHTS_QUERY: OnceLock<String> = OnceLock::new();

/// Returns the joined highlight query of the SCSS adapter.
fn scss_highlights_query() -> &'static str {
    SCSS_HIGHLIGHTS_QUERY.get_or_init(|| {
        let mut query = String::with_capacity(
            tree_sitter_css::HIGHLIGHTS_QUERY.len() + tree_sitter_scss::HIGHLIGHTS_QUERY.len() + 1,
        );
        query.push_str(tree_sitter_css::HIGHLIGHTS_QUERY);
        query.push('\n');
        query.push_str(tree_sitter_scss::HIGHLIGHTS_QUERY);
        query
    })
}

/// Returns the initialization options of `vscode-css-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn css_ls_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the SCSS adapter declares, in declaration order.
///
/// The CSS adapter declares the same program. Two adapters that name one
/// program start one session for each adapter, exactly as C and C++ do with
/// `clangd`.
const SCSS_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "cssls",
    program: "vscode-css-language-server",
    args: &["--stdio"],
    language_id: "scss",
    // The server supplies document formatting, and `prettier` formats every
    // buffer of this language.
    formatting: ServerFormatting::Disabled,
    // The server reads a single stylesheet as well as a complete project, so no
    // marker gates its start.
    root_markers: &[],
    initialization_options: css_ls_options,
    workspace_settings: None,
}];

/// The external formatter of the SCSS adapter.
///
/// `prettier` takes the document on standard input, and `--stdin-filepath`
/// carries the path that selects its parser.
const SCSS_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
        FormatterArgument::Literal("--stdin-filepath"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for SCSS stylesheet paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, ScssAdapter};
///
/// let adapter = ScssAdapter::new();
/// assert!(adapter.supports_path(Path::new("styles/site.scss")));
/// // SCSS adds the line comment that plain CSS does not define.
/// assert_eq!(adapter.comment().line_token(), Some("//"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScssAdapter;

impl ScssAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for ScssAdapter {
    fn id(&self) -> &'static str {
        "scss"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &SCSS_EXTENSIONS
    }

    fn language_names(&self) -> &'static [&'static str] {
        &SCSS_LANGUAGE_NAMES
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "scss",
            language: scss_language,
            highlights_query: scss_highlights_query(),
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &SCSS_INDENT_SCOPES,
            closing_delimiters: &SCSS_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &SCSS_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&SCSS_FORMATTER)
    }
}
