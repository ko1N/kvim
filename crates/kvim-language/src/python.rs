//! The Python language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use serde_json::{Value, json};

use kvim_settings::LanguageSettings;

use super::{
    CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule, IndentScope,
    LanguageAdapter, LanguageCatalogEntry, LanguageServerDeclaration, ServerFormatting,
};

/// The node kinds whose content takes one more indent level in Python.
///
/// Python closes a suite with indentation alone, so the grammar has no node
/// that carries both a suite and its closing delimiter. The `block` node of the
/// grammar starts at the first token of the suite and ends at its last token,
/// so no node spans the header line and the body together. The table therefore
/// names the compound statement that owns each suite, and never the `block`
/// node itself. Each of these nine kinds uses the undelimited-body indent span
/// of its named field, so its scope indents from the end of the header, which
/// is the `:` token before the field, through the last byte of the node. A
/// one-line suite opens no indented block, so its scope then holds no
/// position.
///
/// `case_clause` stands beside `match_statement`, because a case label takes one
/// more level than its match statement. `elif_clause`, `else_clause`,
/// `except_clause`, and `finally_clause` stay out of the table, because each one
/// starts at the level of the statement that holds it.
///
/// The remaining twelve kinds are the bracketed expressions. Each one carries
/// its own opening and closing character, so each one keeps the whole indent
/// span and behaves exactly as the equivalent node of a brace language.
const PYTHON_INDENT_SCOPES: [IndentScope; 21] = [
    IndentScope::whole("argument_list"),
    IndentScope::undelimited_body("case_clause", "consequence"),
    IndentScope::undelimited_body("class_definition", "body"),
    IndentScope::whole("dictionary"),
    IndentScope::whole("dictionary_comprehension"),
    IndentScope::undelimited_body("for_statement", "body"),
    IndentScope::undelimited_body("function_definition", "body"),
    IndentScope::whole("generator_expression"),
    IndentScope::undelimited_body("if_statement", "consequence"),
    IndentScope::whole("list"),
    IndentScope::whole("list_comprehension"),
    IndentScope::undelimited_body("match_statement", "body"),
    IndentScope::whole("parameters"),
    IndentScope::whole("parenthesized_expression"),
    IndentScope::whole("set"),
    IndentScope::whole("set_comprehension"),
    IndentScope::whole("subscript"),
    IndentScope::undelimited_body("try_statement", "body"),
    IndentScope::whole("tuple"),
    IndentScope::undelimited_body("while_statement", "body"),
    IndentScope::undelimited_body("with_statement", "body"),
];

/// The number of columns that one Python indent level takes.
const PYTHON_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// The characters that close a Python indent scope.
///
/// A suite closes with no character, so these three characters close the
/// bracketed expressions of the table above alone.
const PYTHON_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

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
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("python")
                .expect("the grammar-python feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // Python defines no block comment. A triple-quoted text is a string
        // expression, so the metadata carries the line token alone.
        CommentStyle::new(Some("#"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &PYTHON_INDENT_SCOPES,
            width: PYTHON_INDENT_WIDTH,
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
