//! The Python language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    CommentStyle, FormatterArgument, FormatterDeclaration, Grammar, IndentRule, LanguageAdapter,
    LanguageServerDeclaration, ServerFormatting,
};

/// The file extensions that the Python adapter owns.
///
/// A stub file carries the `pyi` extension and the same grammar, so one adapter
/// owns both extensions.
const PYTHON_EXTENSIONS: [&str; 2] = ["py", "pyi"];

/// The node kinds whose content takes one more indent level in Python.
///
/// Python closes a suite with indentation alone, so the grammar has no node
/// that carries both a suite and its closing delimiter. The `block` node of the
/// grammar starts at the first token of the suite and ends at its last token,
/// so a new line at the end of the header line stands before that node, and a
/// new line at the end of the last statement stands behind it. The table
/// therefore names the compound statement that owns each suite, and never the
/// `block` node itself. One statement then supplies the level of its own body.
///
/// `case_clause` stands beside `match_statement`, because a case label takes one
/// more level than its match statement. `elif_clause`, `else_clause`,
/// `except_clause`, and `finally_clause` stay out of the table, because each one
/// starts at the level of the statement that holds it.
///
/// The remaining kinds are the bracketed expressions. Each one carries its own
/// opening and closing character, so each one behaves exactly as the equivalent
/// node of a brace language.
///
/// Two limits follow from this model, and the user corrects each affected line:
///
/// - The last line of a suite reports one level too few, because the compound
///   statement ends at that line and no delimiter follows it.
/// - A compound statement whose header spans several lines reports one level too
///   many, because the statement already supplies the level of its own body.
const PYTHON_INDENT_SCOPES: [&str; 21] = [
    "argument_list",
    "case_clause",
    "class_definition",
    "dictionary",
    "dictionary_comprehension",
    "for_statement",
    "function_definition",
    "generator_expression",
    "if_statement",
    "list",
    "list_comprehension",
    "match_statement",
    "parameters",
    "parenthesized_expression",
    "set",
    "set_comprehension",
    "subscript",
    "try_statement",
    "tuple",
    "while_statement",
    "with_statement",
];

/// The characters that close a Python indent scope.
///
/// A suite closes with no character, so these three characters close the
/// bracketed expressions of the table above alone.
const PYTHON_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the Python grammar of the bundled parser.
fn python_language() -> Language {
    tree_sitter_python::LANGUAGE.into()
}

/// Returns the initialization options of `pyright-langserver`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn pyright_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the Python adapter declares, in declaration order.
const PYTHON_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "pyright",
    program: "pyright-langserver",
    args: &["--stdio"],
    language_id: "python",
    // The server supplies no document formatting, and `black` formats every
    // buffer of this language.
    formatting: ServerFormatting::Disabled,
    // The server type checks a single file as well as a complete project, so no
    // marker gates its start.
    root_markers: &[],
    initialization_options: pyright_options,
    workspace_settings: None,
}];

/// The external formatter of the Python adapter.
///
/// `black` reads the document on standard input only when the command names the
/// dash argument. `--stdin-filename` carries the document path, which selects
/// the stub rules for a `pyi` document and finds the `pyproject.toml` of the
/// project. `--quiet` stops the report that the program writes to standard
/// error after each run.
const PYTHON_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "black",
    args: &[
        FormatterArgument::Literal("--stdin-filename"),
        FormatterArgument::DocumentPath,
        FormatterArgument::Literal("--quiet"),
        FormatterArgument::Literal("-"),
    ],
};

/// The language adapter for Python source paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, PythonAdapter};
///
/// let adapter = PythonAdapter::new();
/// assert!(adapter.supports_path(Path::new("src/main.py")));
/// assert!(adapter.supports_path(Path::new("src/api.pyi")));
/// assert_eq!(adapter.comment().line_token(), Some("#"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PythonAdapter;

impl PythonAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for PythonAdapter {
    fn id(&self) -> &'static str {
        "python"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &PYTHON_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        // Python defines no block comment. A triple-quoted text is a string
        // expression, so the metadata carries the line token alone.
        CommentStyle::new(Some("#"), None)
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "python",
            language: python_language,
            highlights_query: tree_sitter_python::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &PYTHON_INDENT_SCOPES,
            closing_delimiters: &PYTHON_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &PYTHON_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&PYTHON_FORMATTER)
    }
}
