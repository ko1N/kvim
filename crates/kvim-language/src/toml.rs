//! The TOML language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, and the indent rule. See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use serde_json::{Value, json};

use kvim_settings::LanguageSettings;

use super::{
    CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule, IndentScope,
    LanguageAdapter, LanguageCatalogEntry, LanguageServerDeclaration, ServerFormatting,
};

/// The node kinds whose content takes one more indent level in TOML.
///
/// A table header starts at the left margin, so only the two bracketed values
/// nest.
const TOML_INDENT_SCOPES: [IndentScope; 2] = [
    IndentScope::whole("array"),
    IndentScope::whole("inline_table"),
];

/// The number of columns that one TOML indent level takes.
const TOML_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close a TOML indent scope.
const TOML_CLOSING_DELIMITERS: [char; 2] = [']', '}'];

/// Returns the initialization options of `taplo`.
///
/// `taplo` needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn taplo_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the TOML adapter declares, in declaration order.
const TOML_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "taplo",
    program: "taplo",
    args: &["lsp", "stdio"],
    language_id: "toml",
    formatting: ServerFormatting::Enabled,
    // The server fits every workspace that holds a TOML file, so no marker
    // gates its start.
    root_markers: &[],
    initialization_options: taplo_options,
    workspace_settings: None,
}];

/// The external formatter of the TOML adapter.
///
/// `taplo` formats the document that the hyphen names, which is the standard
/// input, and it writes the formatted document on standard output.
const TOML_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "taplo",
    args: &[
        FormatterArgument::Literal("fmt"),
        FormatterArgument::Literal("-"),
    ],
};

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
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("toml").expect("the grammar-toml feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("#"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &TOML_INDENT_SCOPES,
            width: TOML_INDENT_WIDTH,
            closing_delimiters: &TOML_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &TOML_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&TOML_FORMATTER)
    }
}
