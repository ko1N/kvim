//! The Nix language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, and the indent rule. See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{
    BlockComment, CommentStyle, FormatterDeclaration, IndentRule, IndentScope, LanguageAdapter,
    LanguageCatalogEntry,
};

/// The node kinds whose content takes one more indent level in Nix.
///
/// The binding set of an attribute set is not listed, because every node that
/// holds one already nests its content. Listing both would count one level
/// twice. A `let` expression spans its bindings and its `in` body, and the body
/// already carries its own level, so the `let` scope stops at the `body` field.
const NIX_INDENT_SCOPES: [IndentScope; 7] = [
    IndentScope::whole("attrset_expression"),
    IndentScope::whole("formals"),
    IndentScope::whole("let_attrset_expression"),
    IndentScope::until_body("let_expression", "body"),
    IndentScope::whole("list_expression"),
    IndentScope::whole("parenthesized_expression"),
    IndentScope::whole("rec_attrset_expression"),
];

/// The number of columns that one Nix indent level takes.
const NIX_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close a Nix indent scope.
const NIX_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the initialization options of `nil`.
///
/// `nil` needs no option from the language-neutral settings, so the function
/// returns the empty object and reads nothing from `settings`.

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
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::NIX_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("nix").expect("the grammar-nix feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("#"), Some(BlockComment::new("/*", "*/")))
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &NIX_INDENT_SCOPES,
            width: NIX_INDENT_WIDTH,
            closing_delimiters: &NIX_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&NIX_FORMATTER)
    }
}
