//! The Terraform language adapter.
//!
//! The grammar and its vendored highlight query live in `kvim-syntax`.
//!
//! The adapter supplies data only, exactly as the other adapters do: its
//! catalog entry, the comment metadata, the indent rule, the language servers,
//! and the external formatter. See `docs/language-services.md`.

use std::sync::OnceLock;

use serde_json::{Value, json};

use kvim_settings::LanguageSettings;

use super::{
    BlockComment, CommentStyle, FormatterArgument, FormatterDeclaration, IndentRule,
    LanguageAdapter, LanguageCatalogEntry, LanguageServerDeclaration, ServerFormatting,
};

/// The node kinds whose content takes one more indent level in Terraform.
///
/// A `block` node spans the braces of a named block, an `object` node spans the
/// braces of an object value, and a `tuple` node spans the brackets of a list
/// value. A `function_call` node spans the name and the parentheses of a call.
/// Each one carries its own opening and closing character, so each one behaves
/// exactly as the equivalent node of a brace language.
const TERRAFORM_INDENT_SCOPES: [&str; 4] = ["block", "function_call", "object", "tuple"];

/// The characters that close a Terraform indent scope.
const TERRAFORM_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the initialization options of `tofu-ls`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn tofu_ls_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the Terraform adapter declares, in declaration
/// order.
const TERRAFORM_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "tofu_ls",
    program: "tofu-ls",
    args: &["serve"],
    language_id: "terraform",
    // The server supplies document formatting, and `tofu fmt` formats every
    // buffer of this language.
    formatting: ServerFormatting::Disabled,
    // The server reads a single configuration as well as a complete project, so
    // no marker gates its start.
    root_markers: &[],
    initialization_options: tofu_ls_options,
    workspace_settings: None,
}];

/// The external formatter of the Terraform adapter.
///
/// `tofu fmt` formats a directory by default. The `-` argument makes it read
/// the document from standard input and write the result to standard output.
const TERRAFORM_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "tofu",
    args: &[
        FormatterArgument::Literal("fmt"),
        FormatterArgument::Literal("-"),
    ],
};

/// The language adapter for Terraform configuration paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{LanguageAdapter, TerraformAdapter};
///
/// let adapter = TerraformAdapter::new();
/// assert!(adapter.supports_path(Path::new("infra/main.tf")));
/// assert_eq!(adapter.comment().line_token(), Some("#"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerraformAdapter;

impl TerraformAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for TerraformAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("terraform")
                .expect("the grammar-terraform feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // The language reads `#` and `//` as a line comment. `tofu fmt` writes
        // `#`, so the adapter names that token.
        CommentStyle::new(Some("#"), Some(BlockComment::new("/*", "*/")))
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &TERRAFORM_INDENT_SCOPES,
            closing_delimiters: &TERRAFORM_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &TERRAFORM_SERVERS
    }

    fn external_formatter(&self) -> Option<&'static FormatterDeclaration> {
        Some(&TERRAFORM_FORMATTER)
    }
}
