//! The TOML language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, and the indent rule. See `docs/language-services.md`.

use tree_sitter::Language;

use super::{CommentStyle, Grammar, IndentRule, LanguageAdapter};

/// The file extensions that the TOML adapter owns.
const TOML_EXTENSIONS: [&str; 1] = ["toml"];

/// The node kinds whose content takes one more indent level in TOML.
///
/// A table header starts at the left margin, so only the two bracketed values
/// nest.
const TOML_INDENT_SCOPES: [&str; 2] = ["array", "inline_table"];

/// The characters that close a TOML indent scope.
const TOML_CLOSING_DELIMITERS: [char; 2] = [']', '}'];

/// Returns the TOML grammar of the bundled parser.
fn toml_language() -> Language {
    tree_sitter_toml_ng::LANGUAGE.into()
}

/// The language adapter for TOML documents.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, TomlAdapter};
///
/// let adapter = TomlAdapter::new();
/// assert!(adapter.supports_path(Path::new("Cargo.toml")));
/// assert_eq!(adapter.comment().line_token(), Some("#"));
/// // TOML defines no block comment.
/// assert_eq!(adapter.comment().block(), None);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TomlAdapter;

impl TomlAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for TomlAdapter {
    fn id(&self) -> &'static str {
        "toml"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &TOML_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("#"), None)
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "toml",
            language: toml_language,
            highlights_query: tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &TOML_INDENT_SCOPES,
            closing_delimiters: &TOML_CLOSING_DELIMITERS,
        }
    }
}
