//! The fish language adapter.
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

/// The node kinds whose content takes one more indent level in fish.
///
/// Every compound statement of fish ends with the `end` keyword, so each node
/// spans its complete body exactly as a braced block of a C-family language
/// does. One entry therefore names the whole construct.
///
/// `case_clause` stands beside `switch_statement`, because a case label takes
/// one more level than its switch statement. `else_clause` and `else_if_clause`
/// stay out of the table, because each one starts at the level of the `if`
/// statement that holds it.
const FISH_INDENT_SCOPES: [&str; 8] = [
    "begin_statement",
    "case_clause",
    "command_substitution",
    "for_statement",
    "function_definition",
    "if_statement",
    "switch_statement",
    "while_statement",
];

/// The characters that close a fish indent scope.
///
/// A parenthesis closes a command substitution. Every other scope closes with
/// the `end` keyword, which this rule cannot name.
const FISH_CLOSING_DELIMITERS: [char; 1] = [')'];

/// Returns the initialization options of `fish-lsp`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn fish_lsp_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the fish adapter declares, in declaration order.
const FISH_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "fish_lsp",
    program: "fish-lsp",
    args: &["start"],
    language_id: "fish",
    // The adapter declares no external formatter, so this server formats every
    // buffer of the language.
    formatting: ServerFormatting::Enabled,
    // The server analyzes a single script as well as a complete configuration
    // directory, so no marker gates its start.
    root_markers: &[],
    initialization_options: fish_lsp_options,
    workspace_settings: None,
}];

/// The language adapter for fish script paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{FishAdapter, LanguageAdapter};
///
/// let adapter = FishAdapter::new();
/// assert!(adapter.supports_path(Path::new("functions/greet.fish")));
/// assert_eq!(adapter.comment().line_token(), Some("#"));
/// // The reference configuration declares no formatter, so the server formats.
/// assert!(adapter.external_formatter().is_none());
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FishAdapter;

impl FishAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for FishAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("fish").expect("the grammar-fish feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // fish defines no block comment, so the metadata carries the line token
        // alone.
        CommentStyle::new(Some("#"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &FISH_INDENT_SCOPES,
            closing_delimiters: &FISH_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &FISH_SERVERS
    }
}
