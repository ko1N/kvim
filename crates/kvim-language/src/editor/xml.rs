//! The XML language adapter.
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

/// The node kinds whose content takes one more indent level in XML.
///
/// An `element` node spans the start tag, the content, and the end tag, so it
/// supplies the level of its own content, exactly as the HTML node of the same
/// name does.
const XML_INDENT_SCOPES: [IndentScope; 1] = [IndentScope::whole("element")];

/// The number of columns that one XML indent level takes.
const XML_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close an XML indent scope.
///
/// An end tag opens with the same `<` character as a start tag, so no single
/// character separates the two. A line that holds an end tag therefore reports
/// one indent level too many, which is the same limit that HTML carries.
const XML_CLOSING_DELIMITERS: [char; 0] = [];

/// The external formatter command for this language.
const XML_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "xmlformat",
    args: &[FormatterArgument::Literal("-")],
};

/// The language adapter for XML document paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, XmlAdapter};
///
/// let adapter = XmlAdapter::new();
/// assert!(adapter.supports_path(Path::new("build/pom.xml")));
/// // XML defines a block comment alone, so the comment toggle stays disabled.
/// assert_eq!(adapter.comment().line_token(), None);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct XmlAdapter;

impl XmlAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for XmlAdapter {
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::XML_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("xml").expect("the grammar-xml feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // XML defines a block comment alone, so the metadata carries no line
        // token and the first-release toggle stays disabled.
        CommentStyle::new(None, Some(BlockComment::new("<!--", "-->")))
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &XML_INDENT_SCOPES,
            width: XML_INDENT_WIDTH,
            closing_delimiters: &XML_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&XML_FORMATTER)
    }
}
