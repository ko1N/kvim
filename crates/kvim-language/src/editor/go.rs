//! The Go language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{
    BlockComment, CommentStyle, FormatterDeclaration, IndentRule, IndentScope, LanguageAdapter,
    LanguageCatalogEntry,
};

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
const GO_INDENT_SCOPES: [IndentScope; 13] = [
    IndentScope::whole("argument_list"),
    IndentScope::whole("block"),
    IndentScope::whole("communication_case"),
    IndentScope::whole("default_case"),
    IndentScope::whole("expression_case"),
    IndentScope::whole("field_declaration_list"),
    IndentScope::whole("import_spec_list"),
    IndentScope::whole("interface_type"),
    IndentScope::whole("literal_value"),
    IndentScope::whole("parameter_list"),
    IndentScope::whole("type_case"),
    IndentScope::whole("type_parameter_list"),
    IndentScope::whole("var_spec_list"),
];

/// The number of columns that one Go indent level takes.
///
/// `gofmt` indents with one hard tab. The rule declares a column count only, so
/// Go receives four columns of spaces until a rule can declare a tab.
const GO_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// The characters that close a Go indent scope.
const GO_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// The external formatter command for this language.
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
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::GO_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("go").expect("the grammar-go feature bundles this language")
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
            scopes: &GO_INDENT_SCOPES,
            width: GO_INDENT_WIDTH,
            closing_delimiters: &GO_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&GO_FORMATTER)
    }
}
