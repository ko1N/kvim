//! The Rust language adapter.
//!
//! The adapter supplies data only: the file extensions, the Tree-sitter
//! grammar with its highlight query, the comment tokens, and the indent rule.
//! The analysis itself is language-neutral, so a later adapter adds a language
//! by supplying the same kinds of data. See `docs/language-services.md`.

use tree_sitter::Language;

use super::{BlockComment, CommentStyle, Grammar, IndentRule, LanguageAdapter};

/// The file extensions that the Rust adapter owns.
///
/// The match is case-sensitive, because a Rust source file uses a lowercase
/// extension.
const RUST_EXTENSIONS: [&str; 1] = ["rs"];

/// The node kinds whose content takes one more indent level in Rust.
const RUST_INDENT_SCOPES: [&str; 16] = [
    "arguments",
    "array_expression",
    "block",
    "declaration_list",
    "enum_variant_list",
    "field_declaration_list",
    "field_initializer_list",
    "match_block",
    "ordered_field_declaration_list",
    "parameters",
    "token_tree",
    "tuple_expression",
    "tuple_pattern",
    "type_arguments",
    "type_parameters",
    "use_list",
];

/// The characters that close a Rust indent scope.
const RUST_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the Rust grammar of the bundled parser.
fn rust_language() -> Language {
    tree_sitter_rust::LANGUAGE.into()
}

/// The language adapter for case-sensitive Rust source paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim::language::{LanguageAdapter, RustAdapter};
///
/// let adapter = RustAdapter::new();
/// assert!(adapter.supports_path(Path::new("src/main.rs")));
/// assert!(!adapter.supports_path(Path::new("src/main.RS")));
/// assert_eq!(adapter.comment().line_token(), Some("//"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RustAdapter;

impl RustAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for RustAdapter {
    fn id(&self) -> &'static str {
        "rust"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &RUST_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "rust",
            language: rust_language,
            highlights_query: tree_sitter_rust::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &RUST_INDENT_SCOPES,
            closing_delimiters: &RUST_CLOSING_DELIMITERS,
        }
    }
}
