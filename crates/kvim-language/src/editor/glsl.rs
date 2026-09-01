//! The GLSL language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, and the language servers. See
//! `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{
    BlockComment, CommentStyle, IndentRule, IndentScope, LanguageAdapter, LanguageCatalogEntry,
};

/// The node kinds whose content takes one more indent level in GLSL.
///
/// The GLSL grammar extends the C grammar, so the node kinds are the node kinds
/// of C. The enumerator list stays out of the table, because the shading
/// language defines no enumeration.
const GLSL_INDENT_SCOPES: [IndentScope; 6] = [
    IndentScope::whole("argument_list"),
    IndentScope::whole("compound_statement"),
    IndentScope::whole("field_declaration_list"),
    IndentScope::whole("initializer_list"),
    IndentScope::whole("parameter_list"),
    IndentScope::whole("parenthesized_expression"),
];

/// The number of columns that one GLSL indent level takes.
const GLSL_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// The characters that close a GLSL indent scope.
const GLSL_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// The grammar-backed editor adapter for GLSL.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GlslAdapter;

impl GlslAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for GlslAdapter {
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::GLSL_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("glsl").expect("the grammar-glsl feature bundles this language")
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
            scopes: &GLSL_INDENT_SCOPES,
            width: GLSL_INDENT_WIDTH,
            closing_delimiters: &GLSL_CLOSING_DELIMITERS,
        }
    }
}
