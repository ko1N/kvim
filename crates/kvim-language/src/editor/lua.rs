//! The Lua language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{
    BlockComment, CommentStyle, FormatterDeclaration, IndentRule, IndentScope, LanguageAdapter,
    LanguageCatalogEntry,
};

/// The node kinds whose content takes one more indent level in Lua.
///
/// Every compound statement of Lua ends with the `end` keyword, and a repeat
/// statement ends with the `until` keyword. Each node therefore spans its
/// complete body exactly as a braced block of a C-family language does, and one
/// entry names the whole construct.
///
/// The `block` node stays out of the table, because the compound statement that
/// holds it already carries the level. `else_statement`, `elseif_statement`,
/// `for_generic_clause`, and `for_numeric_clause` stay out of the table for the
/// same reason.
///
/// The remaining kinds are the bracketed constructs: the argument list of a
/// call, the parameter list of a function, a parenthesized expression, and a
/// table constructor.
const LUA_INDENT_SCOPES: [IndentScope; 11] = [
    IndentScope::whole("arguments"),
    IndentScope::whole("do_statement"),
    IndentScope::whole("for_statement"),
    IndentScope::whole("function_declaration"),
    IndentScope::whole("function_definition"),
    IndentScope::whole("if_statement"),
    IndentScope::whole("parameters"),
    IndentScope::whole("parenthesized_expression"),
    IndentScope::whole("repeat_statement"),
    IndentScope::whole("table_constructor"),
    IndentScope::whole("while_statement"),
];

/// The number of columns that one Lua indent level takes.
const LUA_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close a Lua indent scope.
///
/// A parenthesis closes an argument list, a parameter list, and a parenthesized
/// expression. A brace closes a table constructor. Every other scope closes with
/// a keyword, which this rule cannot name.
const LUA_CLOSING_DELIMITERS: [char; 2] = [')', '}'];

/// Returns the initialization options of `lua-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.

const LUA_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "lua-format",
    args: &[],
};

/// The language adapter for Lua source paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, LuaAdapter};
///
/// let adapter = LuaAdapter::new();
/// assert!(adapter.supports_path(Path::new("plugin/init.lua")));
/// assert_eq!(adapter.comment().line_token(), Some("--"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LuaAdapter;

impl LuaAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for LuaAdapter {
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::LUA_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("lua").expect("the grammar-lua feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // A long comment opens with two dashes and a long bracket, and it closes
        // with the matching long bracket.
        CommentStyle::new(Some("--"), Some(BlockComment::new("--[[", "]]")))
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &LUA_INDENT_SCOPES,
            width: LUA_INDENT_WIDTH,
            closing_delimiters: &LUA_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&LUA_FORMATTER)
    }
}
