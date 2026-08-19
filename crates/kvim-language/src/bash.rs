//! The Bash language adapter.
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

/// The file extensions that the Bash adapter owns.
///
/// The grammar reads the POSIX shell language and the Bash extensions of it, so
/// one adapter owns both extensions.
const BASH_EXTENSIONS: [&str; 2] = ["bash", "sh"];

/// The complete file names that the Bash adapter owns.
///
/// Each name is a startup script of an interactive or a login shell. The name
/// carries no extension, so the file-name key is the only key that selects it.
const BASH_FILE_NAMES: [&str; 4] = [".bash_logout", ".bash_profile", ".bashrc", ".profile"];

/// The node kinds whose content takes one more indent level in Bash.
///
/// Every compound statement of the shell carries its own terminator: `fi` ends
/// an `if` statement, `done` ends a `do` group, `esac` ends a `case` statement,
/// and `;;` ends a case item. Each such node therefore spans its complete body,
/// exactly as a braced block of a C-family language does, and one entry names
/// the whole construct.
///
/// `for_statement` and `while_statement` stay out of the table, because each one
/// holds a `do_group` that already carries the body. `elif_clause` and
/// `else_clause` stay out of the table, because each one starts at the level of
/// the `if` statement that holds it.
const BASH_INDENT_SCOPES: [&str; 8] = [
    "array",
    "case_item",
    "case_statement",
    "command_substitution",
    "compound_statement",
    "do_group",
    "if_statement",
    "subshell",
];

/// The characters that close a Bash indent scope.
///
/// A parenthesis closes an array, a subshell, and a command substitution. A
/// brace closes a compound statement. The remaining scopes close with a keyword,
/// which this rule cannot name.
const BASH_CLOSING_DELIMITERS: [char; 2] = [')', '}'];

/// Returns the Bash grammar of the bundled parser.
fn bash_language() -> Language {
    tree_sitter_bash::LANGUAGE.into()
}

/// Returns the initialization options of `bash-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn bash_language_server_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the Bash adapter declares, in declaration order.
const BASH_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "bashls",
    program: "bash-language-server",
    args: &["start"],
    language_id: "shellscript",
    formatting: ServerFormatting::Enabled,
    // The server analyzes a single script as well as a complete workspace, so
    // no marker gates its start.
    root_markers: &[],
    initialization_options: bash_language_server_options,
}];

/// The external formatter of the Bash adapter.
///
/// `shfmt` reads the document on standard input and writes the formatted
/// document on standard output. `--filename` carries the document path, which
/// selects the dialect of the parser and finds the `.editorconfig` of the
/// project.
const BASH_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "shfmt",
    args: &[
        FormatterArgument::Literal("--filename"),
        FormatterArgument::DocumentPath,
    ],
};

/// The language adapter for shell script paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{BashAdapter, LanguageAdapter};
///
/// let adapter = BashAdapter::new();
/// assert!(adapter.supports_path(Path::new("scripts/build.sh")));
/// assert!(adapter.supports_path(Path::new("home/.bashrc")));
/// assert_eq!(adapter.comment().line_token(), Some("#"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BashAdapter;

impl BashAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for BashAdapter {
    fn id(&self) -> &'static str {
        "bash"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &BASH_EXTENSIONS
    }

    fn file_names(&self) -> &'static [&'static str] {
        &BASH_FILE_NAMES
    }

    fn comment(&self) -> CommentStyle {
        // The shell defines no block comment, so the metadata carries the line
        // token alone.
        CommentStyle::new(Some("#"), None)
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "bash",
            language: bash_language,
            // The crate names the query in the singular.
            highlights_query: tree_sitter_bash::HIGHLIGHT_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &BASH_INDENT_SCOPES,
            closing_delimiters: &BASH_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &BASH_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&BASH_FORMATTER)
    }
}
