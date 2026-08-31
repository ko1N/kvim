//! The HTML language adapter.
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

/// The node kinds whose content takes one more indent level in HTML.
///
/// An `element` node spans the start tag, the content, and the end tag, so one
/// entry names the whole construct. A `script_element` and a `style_element`
/// carry raw text between the same two tags, and each one indents that text the
/// same way.
const HTML_INDENT_SCOPES: [IndentScope; 3] = [
    IndentScope::whole("element"),
    IndentScope::whole("script_element"),
    IndentScope::whole("style_element"),
];

/// The number of columns that one HTML indent level takes.
const HTML_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close an HTML indent scope.
///
/// An end tag opens with the same `<` character as a start tag, so no single
/// character separates the two. The table therefore stays empty, and a line
/// that holds an end tag reports one indent level too many. The user corrects
/// that line, exactly as the documented limit of the Python indent rule works.
const HTML_CLOSING_DELIMITERS: [char; 0] = [];

/// Returns the initialization options of `vscode-html-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.

const HTML_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "prettier",
    args: &[
        FormatterArgument::Literal("--stdin-filepath"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for HTML document paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{HtmlAdapter, LanguageAdapter};
///
/// let adapter = HtmlAdapter::new();
/// assert!(adapter.supports_path(Path::new("public/index.html")));
/// // HTML defines no line comment, so the comment toggle stays disabled.
/// assert_eq!(adapter.comment().line_token(), None);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HtmlAdapter;

impl HtmlAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for HtmlAdapter {
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::HTML_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("html").expect("the grammar-html feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // HTML defines a block comment alone, so the metadata carries no line
        // token and the first-release toggle stays disabled.
        CommentStyle::new(None, Some(BlockComment::new("<!--", "-->")))
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &HTML_INDENT_SCOPES,
            width: HTML_INDENT_WIDTH,
            closing_delimiters: &HTML_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&HTML_FORMATTER)
    }
}
