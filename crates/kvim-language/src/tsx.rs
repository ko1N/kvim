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

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::ecma::{ESLINT_ROOT_MARKERS, eslint_options, eslint_workspace_settings, ts_ls_options};
use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule, IndentScope,
    LanguageAdapter, LanguageCatalogEntry, LanguageServerDeclaration, ServerFormatting,
};

/// The node kinds whose content takes one more indent level in TSX.
///
/// The table holds the TypeScript nodes and the two nodes of the JSX syntax. A
/// `jsx_element` node spans the opening element, the content, and the closing
/// element, so one entry names the whole construct. A `jsx_expression` node
/// spans the braces of an expression inside such an element.
const TSX_INDENT_SCOPES: [IndentScope; 21] = [
    IndentScope::whole("arguments"),
    IndentScope::whole("array"),
    IndentScope::whole("array_pattern"),
    IndentScope::whole("class_body"),
    IndentScope::whole("enum_body"),
    IndentScope::whole("formal_parameters"),
    IndentScope::whole("interface_body"),
    IndentScope::whole("jsx_element"),
    IndentScope::whole("jsx_expression"),
    IndentScope::whole("named_imports"),
    IndentScope::whole("object"),
    IndentScope::whole("object_pattern"),
    IndentScope::whole("object_type"),
    IndentScope::whole("parenthesized_expression"),
    IndentScope::whole("statement_block"),
    IndentScope::whole("switch_body"),
    IndentScope::whole("switch_case"),
    IndentScope::whole("switch_default"),
    IndentScope::whole("template_substitution"),
    IndentScope::whole("type_arguments"),
    IndentScope::whole("type_parameters"),
];

/// The number of columns that one TSX indent level takes.
const TSX_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close a TSX indent scope.
///
/// A closing element opens with the same `<` character as an opening element,
/// so no single character separates the two. A line that holds a closing
/// element therefore reports one indent level too many, exactly as the HTML
/// adapter reports an end tag. The three characters below close every other
/// scope of the table.
const TSX_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

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
        workspace_settings: Some(eslint_workspace_settings),
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
        workspace_settings: None,
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
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("tsx").expect("the grammar-tsx feature bundles this language")
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
            scopes: &TSX_INDENT_SCOPES,
            width: TSX_INDENT_WIDTH,
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
