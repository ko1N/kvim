//! The C++ language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::sync::OnceLock;

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, Grammar, IndentRule,
    LanguageAdapter, LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the C++ adapter owns.
///
/// The adapter owns the explicit C++ header extensions alone. The C adapter
/// owns `h`, because a C header is the common meaning of that extension.
/// Exactly one adapter owns each extension, because two owners make every path
/// of that extension an ambiguous failure.
const CPP_EXTENSIONS: [&str; 6] = ["cc", "cpp", "cxx", "hh", "hpp", "hxx"];

/// The node kinds whose content takes one more indent level in C++.
///
/// The table extends the C table. `declaration_list` is the braced body of a
/// namespace and of a linkage specification. `field_initializer_list` holds the
/// member initializers of a constructor. The two template lists hold the
/// parameters and the arguments of a template.
const CPP_INDENT_SCOPES: [&str; 11] = [
    "argument_list",
    "compound_statement",
    "declaration_list",
    "enumerator_list",
    "field_declaration_list",
    "field_initializer_list",
    "initializer_list",
    "parameter_list",
    "parenthesized_expression",
    "template_argument_list",
    "template_parameter_list",
];

/// The characters that close a C++ indent scope.
///
/// The table names the three brackets alone. A template list closes with `>`,
/// and a formatter never puts that character on its own line, so the table
/// keeps `>` out and never dedents a shifted or compared expression.
const CPP_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the C++ grammar of the bundled parser.
fn cpp_language() -> Language {
    tree_sitter_cpp::LANGUAGE.into()
}

/// The joined highlight query of the C++ adapter.
///
/// The crate ships the C++ patterns alone, because the upstream query inherits
/// the C patterns. Kvim resolves no query inheritance, so the adapter joins the
/// two texts once. The C patterns come first, so a C++ pattern of the same node
/// takes precedence. The C++ grammar is a superset of the C grammar, so every
/// C pattern names a node kind that the C++ grammar holds.
static CPP_HIGHLIGHTS_QUERY: OnceLock<String> = OnceLock::new();

/// Returns the joined highlight query of the C++ adapter.
fn cpp_highlights_query() -> &'static str {
    CPP_HIGHLIGHTS_QUERY.get_or_init(|| {
        let mut query = String::with_capacity(
            tree_sitter_c::HIGHLIGHT_QUERY.len() + tree_sitter_cpp::HIGHLIGHT_QUERY.len() + 1,
        );
        // The crates name both queries in the singular.
        query.push_str(tree_sitter_c::HIGHLIGHT_QUERY);
        query.push('\n');
        query.push_str(tree_sitter_cpp::HIGHLIGHT_QUERY);
        query
    })
}

/// Returns the initialization options of `clangd`.
///
/// `clangd` needs no option from the language-neutral settings, so the function
/// returns the empty object and reads nothing from `settings`.
fn clangd_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the C++ adapter declares, in declaration order.
///
/// The C adapter declares the same program. Two adapters that name one program
/// start one session for each adapter, as `docs/language-services.md` states.
const CPP_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "clangd",
    program: "clangd",
    args: &[],
    language_id: "cpp",
    formatting: ServerFormatting::Enabled,
    // The server reads a compilation database when the workspace holds one,
    // and it falls back to default flags otherwise. It therefore serves every
    // workspace that holds a C++ file, so no marker gates its start.
    root_markers: &[],
    initialization_options: clangd_options,
}];

/// The external formatter of the C++ adapter.
///
/// `clang-format` reads the document on standard input, and it selects its
/// style and its language from the file name. The declaration therefore names
/// the place of the document path beside the flag that carries it.
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
    fn id(&self) -> &'static str {
        "cpp"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &CPP_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "cpp",
            language: cpp_language,
            highlights_query: cpp_highlights_query(),
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &CPP_INDENT_SCOPES,
            closing_delimiters: &CPP_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &CPP_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&CPP_FORMATTER)
    }
}
