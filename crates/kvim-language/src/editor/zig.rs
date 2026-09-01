//! The Zig language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, and the language servers. See
//! `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{CommentStyle, IndentRule, IndentScope, LanguageAdapter, LanguageCatalogEntry};

/// The node kinds whose content takes one more indent level in Zig.
///
/// `block` holds the statements of a function, of a loop, and of a condition.
/// The container declarations hold the fields and the declarations of a braced
/// type. `initializer_list` holds the values of every struct initializer, so
/// the two initializer nodes above it stay out of the table and count no level
/// twice.
const ZIG_INDENT_SCOPES: [IndentScope; 11] = [
    IndentScope::whole("arguments"),
    IndentScope::whole("block"),
    IndentScope::whole("enum_declaration"),
    IndentScope::whole("error_set_declaration"),
    IndentScope::whole("initializer_list"),
    IndentScope::whole("opaque_declaration"),
    IndentScope::whole("parameters"),
    IndentScope::whole("parenthesized_expression"),
    IndentScope::whole("struct_declaration"),
    IndentScope::whole("switch_expression"),
    IndentScope::whole("union_declaration"),
];

/// The number of columns that one Zig indent level takes.
const ZIG_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// The characters that close a Zig indent scope.
const ZIG_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// The grammar-backed editor adapter for Zig.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ZigAdapter;

impl ZigAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for ZigAdapter {
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::ZIG_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("zig").expect("the grammar-zig feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // The Zig language defines no block comment, so the adapter carries the
        // line token alone.
        CommentStyle::new(Some("//"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &ZIG_INDENT_SCOPES,
            width: ZIG_INDENT_WIDTH,
            closing_delimiters: &ZIG_CLOSING_DELIMITERS,
        }
    }
}
