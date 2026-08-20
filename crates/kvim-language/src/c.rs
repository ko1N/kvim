//! The C language adapter.
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
    LanguageAdapter, LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the C adapter owns.
///
/// The adapter owns the header extension `h`, because a C header is the common
/// meaning of that extension. The C++ adapter therefore owns the explicit C++
/// header extensions alone. Exactly one adapter owns each extension, because
/// two owners make every path of that extension an ambiguous failure.
const C_EXTENSIONS: [&str; 2] = ["c", "h"];

/// The language names that the C adapter answers to.
const C_LANGUAGE_NAMES: [&str; 1] = ["c"];

/// The node kinds whose content takes one more indent level in C.
///
/// `compound_statement` is the braced block of a function, of a loop, and of a
/// condition. The list nodes hold the arguments, the parameters, the fields,
/// the enumerators, and the initializers of a declaration.
const C_INDENT_SCOPES: [&str; 7] = [
    "argument_list",
    "compound_statement",
    "enumerator_list",
    "field_declaration_list",
    "initializer_list",
    "parameter_list",
    "parenthesized_expression",
];

/// The characters that close a C indent scope.
const C_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the C grammar of the bundled parser.
fn c_language() -> Language {
    tree_sitter_c::LANGUAGE.into()
}

/// Returns the initialization options of `clangd`.
///
/// `clangd` needs no option from the language-neutral settings, so the function
/// returns the empty object and reads nothing from `settings`.
fn clangd_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the C adapter declares, in declaration order.
const C_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "clangd",
    program: "clangd",
    args: &[],
    language_id: "c",
    formatting: ServerFormatting::Enabled,
    // The server reads a compilation database when the workspace holds one,
    // and it falls back to default flags otherwise. It therefore serves every
    // workspace that holds a C file, so no marker gates its start.
    root_markers: &[],
    initialization_options: clangd_options,
    workspace_settings: None,
}];

/// The external formatter of the C adapter.
///
/// `clang-format` reads the document on standard input, and it selects its
/// style and its language from the file name. The declaration therefore names
/// the place of the document path beside the flag that carries it.
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
    fn id(&self) -> &'static str {
        "c"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &C_EXTENSIONS
    }

    fn language_names(&self) -> &'static [&'static str] {
        &C_LANGUAGE_NAMES
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "c",
            language: c_language,
            // The crate names the query in the singular.
            highlights_query: tree_sitter_c::HIGHLIGHT_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &C_INDENT_SCOPES,
            closing_delimiters: &C_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &C_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&C_FORMATTER)
    }
}
