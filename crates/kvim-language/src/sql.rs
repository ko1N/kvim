//! The SQL language adapter.
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
    LanguageCatalogEntry, LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the SQL adapter owns.
const SQL_EXTENSIONS: [&str; 1] = ["sql"];

/// The language names that the SQL adapter answers to.
const SQL_LANGUAGE_NAMES: [&str; 1] = ["sql"];

/// The node kinds whose content takes one more indent level in SQL.
///
/// Each name spans a parenthesized construct: the column list of a table, a
/// call with its arguments, a value list, a parenthesized predicate, and a
/// nested query. Each one carries its own opening and closing character, so
/// each one behaves exactly as the equivalent node of a brace language.
const SQL_INDENT_SCOPES: [&str; 5] = [
    "column_definitions",
    "invocation",
    "list",
    "parenthesized_expression",
    "subquery",
];

/// The characters that close an SQL indent scope.
const SQL_CLOSING_DELIMITERS: [char; 1] = [')'];

/// Returns the SQL grammar of the bundled parser.
fn sql_language() -> Language {
    tree_sitter_sequel::LANGUAGE.into()
}

/// Returns the initialization options of `sqls`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn sqls_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the SQL adapter declares, in declaration order.
const SQL_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "sqls",
    program: "sqls",
    args: &[],
    language_id: "sql",
    // The server supplies document formatting, and `sql-formatter` formats
    // every buffer of this language.
    formatting: ServerFormatting::Disabled,
    // The server reads a single statement file as well as a complete project,
    // so no marker gates its start.
    root_markers: &[],
    initialization_options: sqls_options,
    workspace_settings: None,
}];

/// The external formatter of the SQL adapter.
///
/// `sql-formatter` reads the document from standard input and writes the
/// result to standard output when it receives no file argument.
const SQL_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "sql-formatter",
    args: &[],
};

/// The language adapter for SQL statement paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, SqlAdapter};
///
/// let adapter = SqlAdapter::new();
/// assert!(adapter.supports_path(Path::new("migrations/001_users.sql")));
/// assert_eq!(adapter.comment().line_token(), Some("--"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SqlAdapter;

impl SqlAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// The catalog entry of the sql language.
///
/// The entry owns the lookup keys and the grammar of this language, so the
/// adapter below names each of them once.
static SQL_CATALOG: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "sql",
    &SQL_LANGUAGE_NAMES,
    &SQL_EXTENSIONS,
    &[],
    sql_grammar,
);

/// Returns the Tree-sitter grammar and the queries of sql.
fn sql_grammar() -> Grammar {
    Grammar {
        language: sql_language,
        highlights_query: tree_sitter_sequel::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    }
}

impl LanguageAdapter for SqlAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        &SQL_CATALOG
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("--"), Some(BlockComment::new("/*", "*/")))
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &SQL_INDENT_SCOPES,
            closing_delimiters: &SQL_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &SQL_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&SQL_FORMATTER)
    }
}
