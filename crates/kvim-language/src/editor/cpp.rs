//! The C++ language adapter.
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

/// The node kinds whose content takes one more indent level in C++.
///
/// The table extends the C table. `declaration_list` is the braced body of a
/// namespace and of a linkage specification. `field_initializer_list` holds the
/// member initializers of a constructor. The two template lists hold the
/// parameters and the arguments of a template.
const CPP_INDENT_SCOPES: [IndentScope; 11] = [
    IndentScope::whole("argument_list"),
    IndentScope::whole("compound_statement"),
    IndentScope::whole("declaration_list"),
    IndentScope::whole("enumerator_list"),
    IndentScope::whole("field_declaration_list"),
    IndentScope::whole("field_initializer_list"),
    IndentScope::whole("initializer_list"),
    IndentScope::whole("parameter_list"),
    IndentScope::whole("parenthesized_expression"),
    IndentScope::whole("template_argument_list"),
    IndentScope::whole("template_parameter_list"),
];

/// The number of columns that one C++ indent level takes.
const CPP_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// The characters that close a C++ indent scope.
///
/// The table names the three brackets alone. A template list closes with `>`,
/// and a formatter never puts that character on its own line, so the table
/// keeps `>` out and never dedents a shifted or compared expression.
const CPP_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// The external formatter command for this language.
const CPP_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "clang-format",
    args: &[
        FormatterArgument::Literal("--assume-filename"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for C++ source and header paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{CppAdapter, LanguageAdapter};
///
/// let adapter = CppAdapter::new();
/// assert!(adapter.supports_path(Path::new("src/main.cpp")));
/// assert!(adapter.supports_path(Path::new("include/api.hpp")));
/// // The C adapter owns the plain header extension.
/// assert!(!adapter.supports_path(Path::new("include/api.h")));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CppAdapter;

impl CppAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for CppAdapter {
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::CPP_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("cpp").expect("the grammar-cpp feature bundles this language")
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
            scopes: &CPP_INDENT_SCOPES,
            width: CPP_INDENT_WIDTH,
            closing_delimiters: &CPP_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&CPP_FORMATTER)
    }
}
