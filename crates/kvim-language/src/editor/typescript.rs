//! The TypeScript language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule, IndentScope,
    LanguageAdapter, LanguageCatalogEntry,
};

/// The node kinds whose content takes one more indent level in TypeScript.
///
/// The table holds the JavaScript nodes and the five bracketed nodes that the
/// type syntax adds. Every entry carries its own opening and closing character,
/// so each one behaves exactly as the equivalent node of a brace language.
/// `switch_case` and `switch_default` stand beside `switch_body`, because a
/// case label takes one more level than the body that holds it.
const TYPESCRIPT_INDENT_SCOPES: [IndentScope; 19] = [
    IndentScope::whole("arguments"),
    IndentScope::whole("array"),
    IndentScope::whole("array_pattern"),
    IndentScope::whole("class_body"),
    IndentScope::whole("enum_body"),
    IndentScope::whole("formal_parameters"),
    IndentScope::whole("interface_body"),
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

/// The number of columns that one TypeScript indent level takes.
const TYPESCRIPT_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close a TypeScript indent scope.
const TYPESCRIPT_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

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
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::TYPESCRIPT_PROFILE
    }

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
            width: TYPESCRIPT_INDENT_WIDTH,
            closing_delimiters: &TYPESCRIPT_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&TYPESCRIPT_FORMATTER)
    }
}
