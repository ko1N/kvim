//! The Go language adapter.
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

/// The file extensions that the Go adapter owns.
const GO_EXTENSIONS: [&str; 1] = ["go"];

/// The node kinds whose content takes one more indent level in Go.
///
/// `block` is the braced body of a function, of a loop, and of a condition. The
/// statement list inside a block stays out of the table, because the block
/// already holds it and two entries would count one level twice.
///
/// A switch statement stays out of the table, because `gofmt` writes every case
/// label at the level of its switch. Each case node holds the statements of one
/// case, so a case body still takes one more level. The brace that closes a
/// switch therefore loses one level too many, and the user corrects that one
/// line.
const GO_INDENT_SCOPES: [&str; 13] = [
    "argument_list",
    "block",
    "communication_case",
    "default_case",
    "expression_case",
    "field_declaration_list",
    "import_spec_list",
    "interface_type",
    "literal_value",
    "parameter_list",
    "type_case",
    "type_parameter_list",
    "var_spec_list",
];

/// The characters that close a Go indent scope.
const GO_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the Go grammar of the bundled parser.
fn go_language() -> Language {
    tree_sitter_go::LANGUAGE.into()
}

/// Returns the initialization options of `gopls`.
///
/// `gopls` needs no option from the language-neutral settings, so the function
/// returns the empty object and reads nothing from `settings`.
fn gopls_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the Go adapter declares, in declaration order.
const GO_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "gopls",
    program: "gopls",
    args: &[],
    language_id: "go",
    formatting: ServerFormatting::Enabled,
    // The server serves a module, a workspace, and a single file, so no marker
    // gates its start.
    root_markers: &[],
    initialization_options: gopls_options,
    workspace_settings: None,
}];

/// The external formatter of the Go adapter.
///
/// `goimports` reads the document on standard input and writes the formatted
/// document on standard output, so the declaration names no argument. The
/// program applies the `gofmt` rules and corrects the import block.
const GO_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "goimports",
    args: &[],
};

/// The language adapter for Go source paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{GoAdapter, LanguageAdapter};
///
/// let adapter = GoAdapter::new();
/// assert!(adapter.supports_path(Path::new("cmd/main.go")));
/// assert_eq!(adapter.comment().line_token(), Some("//"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GoAdapter;

impl GoAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for GoAdapter {
    fn id(&self) -> &'static str {
        "go"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &GO_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "go",
            language: go_language,
            highlights_query: tree_sitter_go::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &GO_INDENT_SCOPES,
            closing_delimiters: &GO_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &GO_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&GO_FORMATTER)
    }
}
