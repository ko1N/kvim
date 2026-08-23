//! The XML language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, Grammar, IndentRule,
    LanguageAdapter, LanguageCatalogEntry, LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the XML adapter owns.
///
/// The table names the extensions that `lemminx` serves. Each one carries an
/// XML document, so one grammar reads all of them.
const XML_EXTENSIONS: [&str; 5] = ["svg", "xml", "xsd", "xsl", "xslt"];

/// The language names that the XML adapter answers to.
const XML_LANGUAGE_NAMES: [&str; 1] = ["xml"];

/// The node kinds whose content takes one more indent level in XML.
///
/// An `element` node spans the start tag, the content, and the end tag, so it
/// supplies the level of its own content, exactly as the HTML node of the same
/// name does.
const XML_INDENT_SCOPES: [&str; 1] = ["element"];

/// The characters that close an XML indent scope.
///
/// An end tag opens with the same `<` character as a start tag, so no single
/// character separates the two. A line that holds an end tag therefore reports
/// one indent level too many, which is the same limit that HTML carries.
const XML_CLOSING_DELIMITERS: [char; 0] = [];

/// Returns the XML grammar of the bundled parser.
///
/// The crate ships a second grammar for a standalone document type definition.
/// One adapter carries one grammar, and no registered extension names such a
/// file, so this build compiles the document grammar alone.
fn xml_language() -> Language {
    tree_sitter_xml::LANGUAGE_XML.into()
}

/// Returns the initialization options of `lemminx`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn lemminx_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the XML adapter declares, in declaration order.
const XML_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "lemminx",
    program: "lemminx",
    args: &[],
    language_id: "xml",
    // The server supplies document formatting, and `xmlformat` formats every
    // buffer of this language.
    formatting: ServerFormatting::Disabled,
    // The server reads a single document as well as a complete project, so no
    // marker gates its start.
    root_markers: &[],
    initialization_options: lemminx_options,
    workspace_settings: None,
}];

/// The external formatter of the XML adapter.
///
/// The reference configuration names the `xmlformatter` package, whose command
/// is `xmlformat`. The `-` argument makes the program read the document from
/// standard input and write the result to standard output.
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

/// The catalog entry of the xml language.
///
/// The entry owns the lookup keys and the grammar of this language, so the
/// adapter below names each of them once.
static XML_CATALOG: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "xml",
    &XML_LANGUAGE_NAMES,
    &XML_EXTENSIONS,
    &[],
    xml_grammar,
);

/// Returns the Tree-sitter grammar and the queries of xml.
fn xml_grammar() -> Grammar {
    Grammar {
        language: xml_language,
        highlights_query: tree_sitter_xml::XML_HIGHLIGHT_QUERY,
        injections_query: "",
        locals_query: "",
    }
}

impl LanguageAdapter for XmlAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        &XML_CATALOG
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
            closing_delimiters: &XML_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &XML_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&XML_FORMATTER)
    }
}
