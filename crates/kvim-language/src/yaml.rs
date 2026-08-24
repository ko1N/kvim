//! The YAML language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::sync::OnceLock;

use serde_json::{Value, json};

use kvim_settings::LanguageSettings;

use super::{
    CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule, LanguageAdapter,
    LanguageCatalogEntry, LanguageServerDeclaration, ServerFormatting,
};

/// The node kinds whose content takes one more indent level in YAML.
///
/// YAML closes a block collection with indentation alone, so no node spans the
/// key line and the nested block together. The table therefore names the entry
/// that owns each nested collection, exactly as the Python table names the
/// compound statement that owns each suite. A `flow_mapping` node and a
/// `flow_sequence` node carry their own brackets, so both behave exactly as
/// the equivalent node of a brace language.
const YAML_INDENT_SCOPES: [&str; 4] = [
    "block_mapping_pair",
    "block_sequence_item",
    "flow_mapping",
    "flow_sequence",
];

/// The characters that close a YAML indent scope.
///
/// A block collection closes with indentation alone, so the table names the
/// brackets of the flow collections.
const YAML_CLOSING_DELIMITERS: [char; 2] = [']', '}'];

/// Returns the initialization options of `yaml-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn yaml_ls_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the YAML adapter declares, in declaration order.
const YAML_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "yamlls",
    program: "yaml-language-server",
    args: &["--stdio"],
    language_id: "yaml",
    // The server supplies document formatting, and `yamlfmt` formats every
    // buffer of this language.
    formatting: ServerFormatting::Disabled,
    // The server reads a single document as well as a complete project, so no
    // marker gates its start.
    root_markers: &[],
    initialization_options: yaml_ls_options,
    workspace_settings: None,
}];

/// The external formatter of the YAML adapter.
///
/// `yamlfmt` formats a file path by default. The `-` argument makes it read
/// the document from standard input and write the result to standard output.
const YAML_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "yamlfmt",
    args: &[FormatterArgument::Literal("-")],
};

/// The language adapter for YAML document paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, YamlAdapter};
///
/// let adapter = YamlAdapter::new();
/// assert!(adapter.supports_path(Path::new("deploy/values.yaml")));
/// // A clang configuration holds YAML and carries no extension.
/// assert!(adapter.supports_path(Path::new(".clang-format")));
/// assert_eq!(adapter.comment().line_token(), Some("#"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct YamlAdapter;

impl YamlAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for YamlAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("yaml").expect("the grammar-yaml feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // YAML defines a line comment alone.
        CommentStyle::new(Some("#"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &YAML_INDENT_SCOPES,
            closing_delimiters: &YAML_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &YAML_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&YAML_FORMATTER)
    }
}
