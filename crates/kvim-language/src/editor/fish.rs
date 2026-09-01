//! The fish language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, and the language servers. See
//! `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{CommentStyle, IndentRule, IndentScope, LanguageAdapter, LanguageCatalogEntry};

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
const FISH_INDENT_SCOPES: [IndentScope; 8] = [
    IndentScope::whole("begin_statement"),
    IndentScope::whole("case_clause"),
    IndentScope::whole("command_substitution"),
    IndentScope::whole("for_statement"),
    IndentScope::whole("function_definition"),
    IndentScope::whole("if_statement"),
    IndentScope::whole("switch_statement"),
    IndentScope::whole("while_statement"),
];

/// The number of columns that one fish indent level takes.
const FISH_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close a fish indent scope.
///
/// A parenthesis closes a command substitution. Every other scope closes with
/// the `end` keyword, which this rule cannot name.
const FISH_CLOSING_DELIMITERS: [char; 1] = [')'];

/// The grammar-backed editor adapter for fish.
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
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::FISH_PROFILE
    }

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
            width: FISH_INDENT_WIDTH,
            closing_delimiters: &FISH_CLOSING_DELIMITERS,
        }
    }
}
