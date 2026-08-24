//! The Rust language adapter.
//!
//! The adapter supplies data only: the file extensions, the Tree-sitter
//! grammar with its highlight query, the comment tokens, and the indent rule.
//! The analysis itself is language-neutral, so a later adapter adds a language
//! by supplying the same kinds of data. See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use serde_json::{Value, json};

use kvim_settings::{CheckDepth, LanguageSettings};

use super::{
    BlockComment, CommentStyle, IndentRule, IndentScope, LanguageAdapter, LanguageCatalogEntry,
    LanguageServerDeclaration, ServerFormatting,
};

/// The node kinds whose content takes one more indent level in Rust.
const RUST_INDENT_SCOPES: [IndentScope; 16] = [
    IndentScope::whole("arguments"),
    IndentScope::whole("array_expression"),
    IndentScope::whole("block"),
    IndentScope::whole("declaration_list"),
    IndentScope::whole("enum_variant_list"),
    IndentScope::whole("field_declaration_list"),
    IndentScope::whole("field_initializer_list"),
    IndentScope::whole("match_block"),
    IndentScope::whole("ordered_field_declaration_list"),
    IndentScope::whole("parameters"),
    IndentScope::whole("token_tree"),
    IndentScope::whole("tuple_expression"),
    IndentScope::whole("tuple_pattern"),
    IndentScope::whole("type_arguments"),
    IndentScope::whole("type_parameters"),
    IndentScope::whole("use_list"),
];

/// The number of columns that one Rust indent level takes.
const RUST_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// The characters that close a Rust indent scope.
const RUST_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// The language servers that the Rust adapter declares, in declaration order.
const RUST_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "rust_analyzer",
    program: "rust-analyzer",
    args: &[],
    language_id: "rust",
    formatting: ServerFormatting::Enabled,
    // The server fits every Rust workspace, so no marker gates its start.
    root_markers: &[],
    initialization_options: rust_analyzer_options,
    workspace_settings: None,
}];

/// Maps the language-neutral settings onto the rust-analyzer options.
///
/// This function is the one place in kvim that names a setting of one concrete
/// server. Everything above the adapter boundary passes the returned value on
/// without reading it.
fn rust_analyzer_options(settings: LanguageSettings) -> Value {
    let command = match settings.check_depth {
        CheckDepth::Compile => "check",
        CheckDepth::Lints => "clippy",
    };
    json!({ "check": { "command": command } })
}

/// The language adapter for case-sensitive Rust source paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, RustAdapter};
///
/// let adapter = RustAdapter::new();
/// assert!(adapter.supports_path(Path::new("src/main.rs")));
/// assert!(!adapter.supports_path(Path::new("src/main.RS")));
/// assert_eq!(adapter.comment().line_token(), Some("//"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RustAdapter;

impl RustAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for RustAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("rust").expect("the grammar-rust feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")))
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &RUST_INDENT_SCOPES,
            width: RUST_INDENT_WIDTH,
            closing_delimiters: &RUST_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &RUST_SERVERS
    }
}
