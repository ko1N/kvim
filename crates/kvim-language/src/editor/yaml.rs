//! The YAML language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{
    CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule, IndentScope,
    LanguageAdapter, LanguageCatalogEntry,
};

/// The node kinds whose content takes one more indent level in YAML.
///
/// YAML closes a block collection with indentation alone, so no node spans the
/// key line and the nested block together. The table therefore names the entry
/// that owns each nested collection and gives it the undelimited-body span of
/// its `value` field, exactly as the Python table names the compound statement
/// that owns each suite. The span reaches the last byte of the entry, so the
/// last line of a nested collection keeps the level of that collection. A
/// `block_sequence_item` node names no field, so it keeps the whole span, and
/// the entry that holds the sequence supplies the level of every item. A
/// `flow_mapping` node and a `flow_sequence` node carry their own brackets, so
/// both keep the whole span and supply their own level, and the entry that
/// holds one adds no second level. A block scalar carries no scope of its own,
/// so it takes its level from the entry that holds it.
const YAML_INDENT_SCOPES: [IndentScope; 4] = [
    IndentScope::undelimited_body("block_mapping_pair", "value"),
    IndentScope::whole("block_sequence_item"),
    IndentScope::whole("flow_mapping"),
    IndentScope::whole("flow_sequence"),
];

/// The number of columns that one YAML indent level takes.
const YAML_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close a YAML indent scope.
///
/// A block collection closes with indentation alone, so the table names the
/// brackets of the flow collections.
const YAML_CLOSING_DELIMITERS: [char; 2] = [']', '}'];

/// The external formatter command for this language.
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
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::YAML_PROFILE
    }

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
            width: YAML_INDENT_WIDTH,
            closing_delimiters: &YAML_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&YAML_FORMATTER)
    }
}
