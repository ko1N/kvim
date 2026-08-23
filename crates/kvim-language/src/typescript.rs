//! The TypeScript language adapter.
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

/// The file extensions that the TypeScript adapter owns.
///
/// The `tsx` extension belongs to the TSX adapter, because the crate ships a
/// second grammar for it and one adapter carries one grammar.
const TYPESCRIPT_EXTENSIONS: [&str; 3] = ["cts", "mts", "ts"];

/// The language names that the TypeScript adapter answers to.
///
/// `ts` is the short form that a fence carries beside `typescript`.
const TYPESCRIPT_LANGUAGE_NAMES: [&str; 2] = ["ts", "typescript"];

/// The node kinds whose content takes one more indent level in TypeScript.
///
/// The table holds the JavaScript nodes and the five bracketed nodes that the
/// type syntax adds. Every entry carries its own opening and closing character,
/// so each one behaves exactly as the equivalent node of a brace language.
/// `switch_case` and `switch_default` stand beside `switch_body`, because a
/// case label takes one more level than the body that holds it.
const TYPESCRIPT_INDENT_SCOPES: [&str; 19] = [
    "arguments",
    "array",
    "array_pattern",
    "class_body",
    "enum_body",
    "formal_parameters",
    "interface_body",
    "named_imports",
    "object",
    "object_pattern",
    "object_type",
    "parenthesized_expression",
    "statement_block",
    "switch_body",
    "switch_case",
    "switch_default",
    "template_substitution",
    "type_arguments",
    "type_parameters",
];

/// The characters that close a TypeScript indent scope.
const TYPESCRIPT_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the TypeScript grammar of the bundled parser.
///
/// The crate ships two grammars. This one reads the plain type syntax, and it
/// rejects the JSX syntax that the TSX adapter owns.
fn typescript_language() -> Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

/// The joined highlight query of the TypeScript adapter.
///
/// The crate ships the type patterns alone, because the upstream query inherits
/// the JavaScript patterns. kvim resolves no query inheritance, so the adapter
/// joins the two texts once. The JavaScript patterns come first, so a
/// TypeScript pattern of the same node takes precedence. The TypeScript grammar
/// is a superset of the JavaScript grammar, so every JavaScript pattern names a
/// node kind that the TypeScript grammar holds.
static TYPESCRIPT_HIGHLIGHTS_QUERY: OnceLock<String> = OnceLock::new();

/// Returns the joined highlight query of the TypeScript adapter.
fn typescript_highlights_query() -> &'static str {
    TYPESCRIPT_HIGHLIGHTS_QUERY.get_or_init(|| {
        let mut query = String::with_capacity(
            tree_sitter_javascript::HIGHLIGHT_QUERY.len()
                + tree_sitter_typescript::HIGHLIGHTS_QUERY.len()
                + 1,
        );
        // The JavaScript crate names its query in the singular.
        query.push_str(tree_sitter_javascript::HIGHLIGHT_QUERY);
        query.push('\n');
        query.push_str(tree_sitter_typescript::HIGHLIGHTS_QUERY);
        query
    })
}

/// The language servers that the TypeScript adapter declares, in declaration
/// order.
///
/// `eslint` stands first, so its lint message survives a merge with an
/// identical message of `ts_ls`. The linter names the rule that produced the
/// report, and the type checker does not.
const TYPESCRIPT_SERVERS: [LanguageServerDeclaration; 2] = [
    LanguageServerDeclaration {
        id: "eslint",
        program: "vscode-eslint-language-server",
        args: &["--stdio"],
        language_id: "typescript",
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
        language_id: "typescript",
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

/// The external formatter of the TypeScript adapter.
///
/// `prettier` takes the document on standard input, and `--stdin-filepath`
/// carries the path that selects its parser.
const TYPESCRIPT_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
        FormatterArgument::Literal("--stdin-filepath"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for TypeScript source paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, TypescriptAdapter};
///
/// let adapter = TypescriptAdapter::new();
/// assert!(adapter.supports_path(Path::new("src/main.ts")));
/// assert_eq!(adapter.comment().line_token(), Some("//"));
/// // The adapter runs a linter beside a type checker.
/// assert_eq!(adapter.language_servers().len(), 2);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TypescriptAdapter;

impl TypescriptAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// The catalog entry of the typescript language.
///
/// The entry owns the lookup keys and the grammar of this language, so the
/// adapter below names each of them once.
static TYPESCRIPT_CATALOG: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "typescript",
    &TYPESCRIPT_LANGUAGE_NAMES,
    &TYPESCRIPT_EXTENSIONS,
    &[],
    typescript_grammar,
);

/// Returns the Tree-sitter grammar and the queries of typescript.
fn typescript_grammar() -> Grammar {
    Grammar {
        language: typescript_language,
        highlights_query: typescript_highlights_query(),
        injections_query: "",
        locals_query: "",
    }
}

impl LanguageAdapter for TypescriptAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        &TYPESCRIPT_CATALOG
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")))
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &TYPESCRIPT_INDENT_SCOPES,
            closing_delimiters: &TYPESCRIPT_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &TYPESCRIPT_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&TYPESCRIPT_FORMATTER)
    }
}
