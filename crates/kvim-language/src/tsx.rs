//! The TSX language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.
//!
//! TSX carries the same language servers and the same formatter as TypeScript,
//! and it needs its own adapter because the grammar crate ships a second
//! grammar for it. One adapter carries one grammar entry point, so the `tsx`
//! extension cannot stand in the TypeScript table.

use std::sync::OnceLock;

use tree_sitter::Language;

use super::ecma::{ESLINT_ROOT_MARKERS, eslint_options, ts_ls_options};
use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, Grammar, IndentRule,
    LanguageAdapter, LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the TSX adapter owns.
const TSX_EXTENSIONS: [&str; 1] = ["tsx"];

/// The node kinds whose content takes one more indent level in TSX.
///
/// The table holds the TypeScript nodes and the two nodes of the JSX syntax. A
/// `jsx_element` node spans the opening element, the content, and the closing
/// element, so one entry names the whole construct. A `jsx_expression` node
/// spans the braces of an expression inside such an element.
const TSX_INDENT_SCOPES: [&str; 21] = [
    "arguments",
    "array",
    "array_pattern",
    "class_body",
    "enum_body",
    "formal_parameters",
    "interface_body",
    "jsx_element",
    "jsx_expression",
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

/// The characters that close a TSX indent scope.
///
/// A closing element opens with the same `<` character as an opening element,
/// so no single character separates the two. A line that holds a closing
/// element therefore reports one indent level too many, exactly as the HTML
/// adapter reports an end tag. The three characters below close every other
/// scope of the table.
const TSX_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the TSX grammar of the bundled parser.
///
/// The crate ships two grammars. This one reads the JSX syntax, and the plain
/// TypeScript grammar of the TypeScript adapter does not.
fn tsx_language() -> Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}

/// The joined highlight query of the TSX adapter.
///
/// TSX reads three dialects at once, and the two crates ship one text for each
/// of them. Kvim resolves no query inheritance, so the adapter joins the three
/// texts once. The JavaScript patterns come first, the JSX patterns follow
/// them, and the type patterns come last, so a later pattern of the same node
/// takes precedence.
static TSX_HIGHLIGHTS_QUERY: OnceLock<String> = OnceLock::new();

/// Returns the joined highlight query of the TSX adapter.
fn tsx_highlights_query() -> &'static str {
    TSX_HIGHLIGHTS_QUERY.get_or_init(|| {
        let mut query = String::with_capacity(
            tree_sitter_javascript::HIGHLIGHT_QUERY.len()
                + tree_sitter_javascript::JSX_HIGHLIGHT_QUERY.len()
                + tree_sitter_typescript::HIGHLIGHTS_QUERY.len()
                + 2,
        );
        // The JavaScript crate names its first query in the singular.
        query.push_str(tree_sitter_javascript::HIGHLIGHT_QUERY);
        query.push('\n');
        query.push_str(tree_sitter_javascript::JSX_HIGHLIGHT_QUERY);
        query.push('\n');
        query.push_str(tree_sitter_typescript::HIGHLIGHTS_QUERY);
        query
    })
}

/// The language servers that the TSX adapter declares, in declaration order.
///
/// `eslint` stands first, so its lint message survives a merge with an
/// identical message of `ts_ls`. The linter names the rule that produced the
/// report, and the type checker does not.
const TSX_SERVERS: [LanguageServerDeclaration; 2] = [
    LanguageServerDeclaration {
        id: "eslint",
        program: "vscode-eslint-language-server",
        args: &["--stdio"],
        language_id: "typescriptreact",
        // The server supplies no document formatting, and `prettier` formats
        // every buffer of this language.
        formatting: ServerFormatting::Disabled,
        // The linter needs a workspace configuration, so a marker gates it.
        root_markers: &ESLINT_ROOT_MARKERS,
        initialization_options: eslint_options,
    },
    LanguageServerDeclaration {
        id: "ts_ls",
        program: "typescript-language-server",
        args: &["--stdio"],
        language_id: "typescriptreact",
        // The server supplies document formatting, and `prettier` formats every
        // buffer of this language.
        formatting: ServerFormatting::Disabled,
        // The server checks a single file as well as a complete project, so no
        // marker gates its start.
        root_markers: &[],
        initialization_options: ts_ls_options,
    },
];

/// The external formatter of the TSX adapter.
///
/// `prettier` takes the document on standard input, and `--stdin-filepath`
/// carries the path that selects its parser.
const TSX_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
        FormatterArgument::Literal("--stdin-filepath"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for TSX source paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, TsxAdapter};
///
/// let adapter = TsxAdapter::new();
/// assert!(adapter.supports_path(Path::new("src/App.tsx")));
/// // The plain TypeScript grammar rejects the JSX syntax, so TSX owns the
/// // extension alone.
/// assert!(!adapter.supports_path(Path::new("src/main.ts")));
/// assert_eq!(adapter.comment().line_token(), Some("//"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TsxAdapter;

impl TsxAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for TsxAdapter {
    fn id(&self) -> &'static str {
        "tsx"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &TSX_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "tsx",
            language: tsx_language,
            highlights_query: tsx_highlights_query(),
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &TSX_INDENT_SCOPES,
            closing_delimiters: &TSX_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &TSX_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&TSX_FORMATTER)
    }
}
