//! The Rust language adapter.
//!
//! The adapter supplies data only: the file extensions, the Tree-sitter
//! grammar with its highlight query, the comment tokens, and the indent rule.
//! The analysis itself is language-neutral, so a later adapter adds a language
//! by supplying the same kinds of data. See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{
    BlockComment, CommentStyle, IndentRule, IndentScope, LanguageAdapter, LanguageCatalogEntry,
};

/// The node kinds whose content takes one more indent level in Rust.
const RUST_INDENT_SCOPES: [IndentScope; 16] = [
    IndentScope::whole("arguments"),
    IndentScope::whole("array_expression"),
    IndentScope::whole("block"),
    IndentScope::whole("declaration_list"),
    IndentScope::whole("enum_variant_list"),
    IndentScope::whole("field_declaration_list"),
    IndentScope::whole("field_initializer_list"),
    IndentScope::whole("match_block"),
    IndentScope::whole("ordered_field_declaration_list"),
    IndentScope::whole("parameters"),
    IndentScope::whole("token_tree"),
    IndentScope::whole("tuple_expression"),
    IndentScope::whole("tuple_pattern"),
    IndentScope::whole("type_arguments"),
    IndentScope::whole("type_parameters"),
    IndentScope::whole("use_list"),
];

/// The number of columns that one Rust indent level takes.
const RUST_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// The characters that close a Rust indent scope.
const RUST_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// The grammar-backed editor adapter for Rust.
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
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::RUST_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("rust").expect("the grammar-rust feature bundles this language")
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
            scopes: &RUST_INDENT_SCOPES,
            width: RUST_INDENT_WIDTH,
            closing_delimiters: &RUST_CLOSING_DELIMITERS,
        }
    }
}
