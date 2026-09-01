//! The SQL language adapter.
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

/// The node kinds whose content takes one more indent level in SQL.
///
/// Each name spans a parenthesized construct: the column list of a table, a
/// call with its arguments, a value list, a parenthesized predicate, and a
/// nested query. Each one carries its own opening and closing character, so
/// each one behaves exactly as the equivalent node of a brace language.
const SQL_INDENT_SCOPES: [IndentScope; 5] = [
    IndentScope::whole("column_definitions"),
    IndentScope::whole("invocation"),
    IndentScope::whole("list"),
    IndentScope::whole("parenthesized_expression"),
    IndentScope::whole("subquery"),
];

/// The number of columns that one SQL indent level takes.
const SQL_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close an SQL indent scope.
const SQL_CLOSING_DELIMITERS: [char; 1] = [')'];

/// The external formatter command for this language.
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

impl LanguageAdapter for SqlAdapter {
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::SQL_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("sql").expect("the grammar-sql feature bundles this language")
        })
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
            width: SQL_INDENT_WIDTH,
            closing_delimiters: &SQL_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&SQL_FORMATTER)
    }
}
