//! The JSON language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, and the indent rule. See `docs/language-services.md`.

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    CommentStyle, FormatterArgument, FormatterDeclaration, Grammar, IndentRule, LanguageAdapter,
    LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the JSON adapter owns.
const JSON_EXTENSIONS: [&str; 1] = ["json"];

/// The file names that the JSON adapter owns.
///
/// A lock file in the JSON format carries the extension of the tool that wrote
/// it, not the extension of its format, so the adapter names the file itself.
/// `flake.lock` is the lock file of this repository.
const JSON_FILE_NAMES: [&str; 1] = ["flake.lock"];

/// The node kinds whose content takes one more indent level in JSON.
const JSON_INDENT_SCOPES: [&str; 2] = ["array", "object"];

/// The characters that close a JSON indent scope.
const JSON_CLOSING_DELIMITERS: [char; 2] = [']', '}'];

/// Returns the JSON grammar of the bundled parser.
fn json_language() -> Language {
    tree_sitter_json::LANGUAGE.into()
}

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
/// from the file name. The declaration therefore names the place of the
/// document path beside the flag that carries it.
const JSON_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
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
    fn id(&self) -> &'static str {
        "json"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &JSON_EXTENSIONS
    }

    fn file_names(&self) -> &'static [&'static str] {
        &JSON_FILE_NAMES
    }

    fn comment(&self) -> CommentStyle {
        // The JSON grammar accepts a comment, but the format defines none, so
        // Kvim writes none.
        CommentStyle::none()
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "json",
            language: json_language,
            highlights_query: tree_sitter_json::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &JSON_INDENT_SCOPES,
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
