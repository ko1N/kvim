//! The Nix language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, and the indent rule. See `docs/language-services.md`.

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    BlockComment, CommentStyle, FormatterDeclaration, Grammar, IndentRule, LanguageAdapter,
    LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the Nix adapter owns.
const NIX_EXTENSIONS: [&str; 1] = ["nix"];

/// The node kinds whose content takes one more indent level in Nix.
///
/// The binding set of an attribute set is not listed, because every node that
/// holds one already nests its content. Listing both would count one level
/// twice. A `let` expression holds its bindings and its body, so the body also
/// takes the level of the bindings.
const NIX_INDENT_SCOPES: [&str; 7] = [
    "attrset_expression",
    "formals",
    "let_attrset_expression",
    "let_expression",
    "list_expression",
    "parenthesized_expression",
    "rec_attrset_expression",
];

/// The characters that close a Nix indent scope.
const NIX_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the Nix grammar of the bundled parser.
fn nix_language() -> Language {
    tree_sitter_nix::LANGUAGE.into()
}

/// Returns the initialization options of `nil`.
///
/// `nil` needs no option from the language-neutral settings, so the function
/// returns the empty object and reads nothing from `settings`.
fn nil_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the Nix adapter declares, in declaration order.
const NIX_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "nil_ls",
    program: "nil",
    args: &[],
    language_id: "nix",
    formatting: ServerFormatting::Enabled,
    // The server fits every workspace that holds a Nix file, so no marker
    // gates its start.
    root_markers: &[],
    initialization_options: nil_options,
    workspace_settings: None,
}];

/// The external formatter of the Nix adapter.
///
/// `nixfmt` reads the document on standard input and writes the formatted
/// document on standard output, so the declaration names no argument.
const NIX_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "nixfmt",
    args: &[],
};

/// The language adapter for Nix expressions.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, NixAdapter};
///
/// let adapter = NixAdapter::new();
/// assert!(adapter.supports_path(Path::new("flake.nix")));
/// assert_eq!(adapter.comment().line_token(), Some("#"));
/// assert_eq!(adapter.comment().block().map(|block| block.open), Some("/*"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NixAdapter;

impl NixAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for NixAdapter {
    fn id(&self) -> &'static str {
        "nix"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &NIX_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("#"), Some(BlockComment::new("/*", "*/")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "nix",
            language: nix_language,
            highlights_query: tree_sitter_nix::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &NIX_INDENT_SCOPES,
            closing_delimiters: &NIX_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &NIX_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&NIX_FORMATTER)
    }
}
