//! The C language adapter.
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

/// The node kinds whose content takes one more indent level in C.
///
/// `compound_statement` is the braced block of a function, of a loop, and of a
/// condition. The list nodes hold the arguments, the parameters, the fields,
/// the enumerators, and the initializers of a declaration.
const C_INDENT_SCOPES: [IndentScope; 7] = [
    IndentScope::whole("argument_list"),
    IndentScope::whole("compound_statement"),
    IndentScope::whole("enumerator_list"),
    IndentScope::whole("field_declaration_list"),
    IndentScope::whole("initializer_list"),
    IndentScope::whole("parameter_list"),
    IndentScope::whole("parenthesized_expression"),
];

/// The number of columns that one C indent level takes.
const C_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// The characters that close a C indent scope.
const C_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// The external formatter command for this language.
const C_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "clang-format",
    args: &[
        FormatterArgument::Literal("--assume-filename"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for C source and header paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{CAdapter, LanguageAdapter};
///
/// let adapter = CAdapter::new();
/// assert!(adapter.supports_path(Path::new("src/main.c")));
/// assert!(adapter.supports_path(Path::new("include/api.h")));
/// assert_eq!(adapter.comment().line_token(), Some("//"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CAdapter;

impl CAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for CAdapter {
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::C_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("c").expect("the grammar-c feature bundles this language")
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
            scopes: &C_INDENT_SCOPES,
            width: C_INDENT_WIDTH,
            closing_delimiters: &C_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&C_FORMATTER)
    }
}
