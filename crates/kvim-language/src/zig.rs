//! The Zig language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, and the language servers. See
//! `docs/language-services.md`.

use std::sync::OnceLock;

use serde_json::{Value, json};

use kvim_settings::LanguageSettings;

use super::{
    CommentStyle, IndentRule, LanguageAdapter, LanguageCatalogEntry, LanguageServerDeclaration,
    ServerFormatting,
};

/// The node kinds whose content takes one more indent level in Zig.
///
/// `block` holds the statements of a function, of a loop, and of a condition.
/// The container declarations hold the fields and the declarations of a braced
/// type. `initializer_list` holds the values of every struct initializer, so
/// the two initializer nodes above it stay out of the table and count no level
/// twice.
const ZIG_INDENT_SCOPES: [&str; 11] = [
    "arguments",
    "block",
    "enum_declaration",
    "error_set_declaration",
    "initializer_list",
    "opaque_declaration",
    "parameters",
    "parenthesized_expression",
    "struct_declaration",
    "switch_expression",
    "union_declaration",
];

/// The characters that close a Zig indent scope.
const ZIG_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the initialization options of `zls`.
///
/// `zls` needs no option from the language-neutral settings, so the function
/// returns the empty object and reads nothing from `settings`.
fn zls_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the Zig adapter declares, in declaration order.
const ZIG_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "zls",
    program: "zls",
    args: &[],
    language_id: "zig",
    // Zig declares no external formatter, so this server formats a Zig buffer.
    formatting: ServerFormatting::Enabled,
    // The server serves a single file as well as a build tree, so no marker
    // gates its start.
    root_markers: &[],
    initialization_options: zls_options,
    workspace_settings: None,
}];

/// The language adapter for Zig source paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, ZigAdapter};
///
/// let adapter = ZigAdapter::new();
/// assert!(adapter.supports_path(Path::new("src/main.zig")));
/// assert_eq!(adapter.comment().line_token(), Some("//"));
/// // Zig defines no block comment.
/// assert_eq!(adapter.comment().block(), None);
/// ```
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
            closing_delimiters: &ZIG_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &ZIG_SERVERS
    }
}
