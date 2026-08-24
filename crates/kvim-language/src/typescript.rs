//! The TypeScript language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::sync::OnceLock;

use super::ecma::{ESLINT_ROOT_MARKERS, eslint_options, eslint_workspace_settings, ts_ls_options};
use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule,
    LanguageAdapter, LanguageCatalogEntry, LanguageServerDeclaration, ServerFormatting,
};

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

impl LanguageAdapter for TypescriptAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("typescript")
                .expect("the grammar-typescript feature bundles this language")
        })
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
