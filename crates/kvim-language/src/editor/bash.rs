//! The Bash language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, the language servers, and the external formatter.
//! See `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{
    CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule, IndentScope,
    LanguageAdapter, LanguageCatalogEntry,
};

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
const BASH_INDENT_SCOPES: [IndentScope; 8] = [
    IndentScope::whole("array"),
    IndentScope::whole("case_item"),
    IndentScope::whole("case_statement"),
    IndentScope::whole("command_substitution"),
    IndentScope::whole("compound_statement"),
    IndentScope::whole("do_group"),
    IndentScope::whole("if_statement"),
    IndentScope::whole("subshell"),
];

/// The number of columns that one Bash indent level takes.
const BASH_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(2).expect("the literal 2 is not zero");

/// The characters that close a Bash indent scope.
///
/// A parenthesis closes an array, a subshell, and a command substitution. A
/// brace closes a compound statement. The remaining scopes close with a keyword,
/// which this rule cannot name.
const BASH_CLOSING_DELIMITERS: [char; 2] = [')', '}'];

/// Returns the initialization options of `bash-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.

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
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::BASH_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("bash").expect("the grammar-bash feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // The shell defines no block comment, so the metadata carries the line
        // token alone.
        CommentStyle::new(Some("#"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &BASH_INDENT_SCOPES,
            width: BASH_INDENT_WIDTH,
            closing_delimiters: &BASH_CLOSING_DELIMITERS,
        }
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&BASH_FORMATTER)
    }
}
