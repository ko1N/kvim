//! The Lua language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    BlockComment, CommentStyle, FormatterDeclaration, Grammar, IndentRule, LanguageAdapter,
    LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the Lua adapter owns.
const LUA_EXTENSIONS: [&str; 1] = ["lua"];

/// The language names that the Lua adapter answers to.
const LUA_LANGUAGE_NAMES: [&str; 1] = ["lua"];

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
const LUA_INDENT_SCOPES: [&str; 11] = [
    "arguments",
    "do_statement",
    "for_statement",
    "function_declaration",
    "function_definition",
    "if_statement",
    "parameters",
    "parenthesized_expression",
    "repeat_statement",
    "table_constructor",
    "while_statement",
];

/// The characters that close a Lua indent scope.
///
/// A parenthesis closes an argument list, a parameter list, and a parenthesized
/// expression. A brace closes a table constructor. Every other scope closes with
/// a keyword, which this rule cannot name.
const LUA_CLOSING_DELIMITERS: [char; 2] = [')', '}'];

/// Returns the Lua grammar of the bundled parser.
fn lua_language() -> Language {
    tree_sitter_lua::LANGUAGE.into()
}

/// Returns the initialization options of `lua-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn lua_language_server_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the Lua adapter declares, in declaration order.
const LUA_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "lua_ls",
    program: "lua-language-server",
    args: &[],
    language_id: "lua",
    formatting: ServerFormatting::Enabled,
    // The server analyzes a single file as well as a complete workspace, so no
    // marker gates its start.
    root_markers: &[],
    initialization_options: lua_language_server_options,
    workspace_settings: None,
}];

/// The external formatter of the Lua adapter.
///
/// `lua-format` reads the document on standard input and writes the formatted
/// document on standard output, so the declaration names no argument.
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
    fn id(&self) -> &'static str {
        "lua"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &LUA_EXTENSIONS
    }

    fn language_names(&self) -> &'static [&'static str] {
        &LUA_LANGUAGE_NAMES
    }

    fn comment(&self) -> CommentStyle {
        // A long comment opens with two dashes and a long bracket, and it closes
        // with the matching long bracket.
        CommentStyle::new(Some("--"), Some(BlockComment::new("--[[", "]]")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "lua",
            language: lua_language,
            highlights_query: tree_sitter_lua::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &LUA_INDENT_SCOPES,
            closing_delimiters: &LUA_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &LUA_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&LUA_FORMATTER)
    }
}
