//! The CSS language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule, IndentScope,
    LanguageAdapter, LanguageCatalogEntry,
};

/// The node kinds whose content takes one more indent level in CSS.
///
/// A `block` node spans the braces of a rule set and of an at-rule, and an
/// `arguments` node spans the parentheses of a function call. Each one carries
/// its own opening and closing character, so each one behaves exactly as the
/// equivalent node of a brace language.
const CSS_INDENT_SCOPES: [IndentScope; 2] =
    [IndentScope::whole("arguments"), IndentScope::whole("block")];

/// The number of columns that one CSS indent level takes.
const CSS_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close a CSS indent scope.
const CSS_CLOSING_DELIMITERS: [char; 2] = [')', '}'];

/// The external formatter command for this language.
const CSS_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
        FormatterArgument::Literal("--stdin-filepath"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for CSS stylesheet paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{CssAdapter, LanguageAdapter};
///
/// let adapter = CssAdapter::new();
/// assert!(adapter.supports_path(Path::new("assets/site.css")));
/// // CSS defines no line comment, so the comment toggle stays disabled.
/// assert_eq!(adapter.comment().line_token(), None);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CssAdapter;

impl CssAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for CssAdapter {
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::CSS_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("css").expect("the grammar-css feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // CSS defines a block comment alone, so the metadata carries no line
        // token and the first-release toggle stays disabled.
        CommentStyle::new(None, Some(BlockComment::new("/*", "*/")))
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &CSS_INDENT_SCOPES,
            width: CSS_INDENT_WIDTH,
            closing_delimiters: &CSS_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&CSS_FORMATTER)
    }
}
