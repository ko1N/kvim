//! The JavaScript language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::sync::OnceLock;

use tree_sitter::Language;

use super::ecma::{ESLINT_ROOT_MARKERS, eslint_options, eslint_workspace_settings, ts_ls_options};
use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, Grammar, IndentRule,
    LanguageAdapter, LanguageCatalogEntry, LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the JavaScript adapter owns.
///
/// The grammar reads the JSX extension of the language, so the `jsx` extension
/// needs no grammar of its own and stands beside the three module extensions.
const JAVASCRIPT_EXTENSIONS: [&str; 4] = ["cjs", "js", "jsx", "mjs"];

/// The language names that the JavaScript adapter answers to.
///
/// One grammar reads the module syntax and the JSX syntax of the language, so
/// the adapter answers to `jsx` as well.
const JAVASCRIPT_LANGUAGE_NAMES: [&str; 3] = ["javascript", "js", "jsx"];

/// The node kinds whose content takes one more indent level in JavaScript.
///
/// Every entry carries its own opening and closing character, so each one
/// behaves exactly as the equivalent node of a brace language. `switch_case`
/// and `switch_default` stand beside `switch_body`, because a case label takes
/// one more level than the body that holds it.
const JAVASCRIPT_INDENT_SCOPES: [&str; 14] = [
    "arguments",
    "array",
    "array_pattern",
    "class_body",
    "formal_parameters",
    "named_imports",
    "object",
    "object_pattern",
    "parenthesized_expression",
    "statement_block",
    "switch_body",
    "switch_case",
    "switch_default",
    "template_substitution",
];

/// The characters that close a JavaScript indent scope.
const JAVASCRIPT_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the JavaScript grammar of the bundled parser.
fn javascript_language() -> Language {
    tree_sitter_javascript::LANGUAGE.into()
}

/// The joined highlight query of the JavaScript adapter.
///
/// The crate ships the JSX patterns in a second text, because another editor
/// selects them by file type. One grammar reads both dialects, so the adapter
/// joins the two texts once and highlights a JSX document with the same
/// configuration.
static JAVASCRIPT_HIGHLIGHTS_QUERY: OnceLock<String> = OnceLock::new();

/// Returns the joined highlight query of the JavaScript adapter.
fn javascript_highlights_query() -> &'static str {
    JAVASCRIPT_HIGHLIGHTS_QUERY.get_or_init(|| {
        let mut query = String::with_capacity(
            tree_sitter_javascript::HIGHLIGHT_QUERY.len()
                + tree_sitter_javascript::JSX_HIGHLIGHT_QUERY.len()
                + 1,
        );
        // The crate names the first query in the singular.
        query.push_str(tree_sitter_javascript::HIGHLIGHT_QUERY);
        query.push('\n');
        query.push_str(tree_sitter_javascript::JSX_HIGHLIGHT_QUERY);
        query
    })
}

/// The language servers that the JavaScript adapter declares, in declaration
/// order.
///
/// `eslint` stands first, so its lint message survives a merge with an
/// identical message of `ts_ls`. The linter names the rule that produced the
/// report, and the type checker does not.
const JAVASCRIPT_SERVERS: [LanguageServerDeclaration; 2] = [
    LanguageServerDeclaration {
        id: "eslint",
        program: "vscode-eslint-language-server",
        args: &["--stdio"],
        language_id: "javascript",
        // The server supplies no document formatting, and `prettier` formats
        // every buffer of this language.
        formatting: ServerFormatting::Disabled,
        // The linter needs a workspace configuration, so a marker gates it.
        root_markers: &ESLINT_ROOT_MARKERS,
        initialization_options: eslint_options,
        workspace_settings: Some(eslint_workspace_settings),
    },
    LanguageServerDeclaration {
        id: "ts_ls",
        program: "typescript-language-server",
        args: &["--stdio"],
        language_id: "javascript",
        // The server supplies document formatting, and `prettier` formats every
        // buffer of this language.
        formatting: ServerFormatting::Disabled,
        // The server checks a single file as well as a complete project, so no
        // marker gates its start.
        root_markers: &[],
        initialization_options: ts_ls_options,
        workspace_settings: None,
    },
];

/// The external formatter of the JavaScript adapter.
///
/// `prettier` takes the document on standard input, and `--stdin-filepath`
/// carries the path that selects its parser.
const JAVASCRIPT_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
        FormatterArgument::Literal("--stdin-filepath"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for JavaScript source paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{JavascriptAdapter, LanguageAdapter};
///
/// let adapter = JavascriptAdapter::new();
/// assert!(adapter.supports_path(Path::new("src/main.js")));
/// // One grammar reads the JSX extension, so one adapter owns both dialects.
/// assert!(adapter.supports_path(Path::new("src/App.jsx")));
/// assert_eq!(adapter.comment().line_token(), Some("//"));
/// // The adapter runs a linter beside a type checker.
/// assert_eq!(adapter.language_servers().len(), 2);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JavascriptAdapter;

impl JavascriptAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// The catalog entry of the javascript language.
///
/// The entry owns the lookup keys and the grammar of this language, so the
/// adapter below names each of them once.
static JAVASCRIPT_CATALOG: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "javascript",
    &JAVASCRIPT_LANGUAGE_NAMES,
    &JAVASCRIPT_EXTENSIONS,
    &[],
    javascript_grammar,
);

/// Returns the Tree-sitter grammar and the queries of javascript.
fn javascript_grammar() -> Grammar {
    Grammar {
        language: javascript_language,
        highlights_query: javascript_highlights_query(),
        injections_query: "",
        locals_query: "",
    }
}

impl LanguageAdapter for JavascriptAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        &JAVASCRIPT_CATALOG
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")))
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &JAVASCRIPT_INDENT_SCOPES,
            closing_delimiters: &JAVASCRIPT_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &JAVASCRIPT_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&JAVASCRIPT_FORMATTER)
    }
}
