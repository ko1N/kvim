//! The JSON language adapter.
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

/// The node kinds whose content takes one more indent level in JSON.
const JSON_INDENT_SCOPES: [IndentScope; 2] =
    [IndentScope::whole("array"), IndentScope::whole("object")];

/// The number of columns that one JSON indent level takes.
const JSON_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close a JSON indent scope.
const JSON_CLOSING_DELIMITERS: [char; 2] = [']', '}'];

/// Returns the initialization options of `vscode-json-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn vscode_json_language_server_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the JSON adapter declares, in declaration order.
const JSON_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "jsonls",
    program: "vscode-json-language-server",
    args: &["--stdio"],
    language_id: "json",
    formatting: ServerFormatting::Enabled,
    // The server fits every workspace that holds a JSON file, so no marker
    // gates its start.
    root_markers: &[],
    initialization_options: vscode_json_language_server_options,
    workspace_settings: None,
}];

/// The external formatter of the JSON adapter.
///
/// `prettier` reads the document on standard input, and it selects its parser
/// from the extension of the file name. The adapter also owns a lock file whose
/// extension names no format, so `prettier` infers no parser for it and refuses
/// the document. `--parser json` therefore names the format that every path of
/// this adapter holds.
///
/// The declaration keeps the document path beside `--stdin-filepath`, because
/// `prettier` reads the configuration of the project from that path.
const JSON_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
        FormatterArgument::Literal("--parser"),
        FormatterArgument::Literal("json"),
        FormatterArgument::Literal("--stdin-filepath"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for JSON documents.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{JsonAdapter, LanguageAdapter};
///
/// let adapter = JsonAdapter::new();
/// assert!(adapter.supports_path(Path::new("package.json")));
/// // A lock file in the JSON format reaches the adapter by name.
/// assert!(adapter.supports_path(Path::new("flake.lock")));
/// // JSON defines no comment, so the comment toggle stays disabled.
/// assert_eq!(adapter.comment().line_token(), None);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JsonAdapter;

impl JsonAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for JsonAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("json").expect("the grammar-json feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // The JSON grammar accepts a comment, but the format defines none, so
        // kvim writes none.
        CommentStyle::none()
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &JSON_INDENT_SCOPES,
            width: JSON_INDENT_WIDTH,
            closing_delimiters: &JSON_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &JSON_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&JSON_FORMATTER)
    }
}
