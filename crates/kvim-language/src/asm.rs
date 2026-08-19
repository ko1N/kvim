//! The assembly language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, and the language servers. See
//! `docs/language-services.md`.

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    BlockComment, CommentStyle, Grammar, IndentRule, LanguageAdapter, LanguageServerDeclaration,
    ServerFormatting,
};

/// The file extensions that the assembly adapter owns.
///
/// The two source extensions are distinct files by convention: the C
/// preprocessor runs over `S` and never over `s`. The match is case-sensitive,
/// so the adapter names both.
const ASM_EXTENSIONS: [&str; 3] = ["S", "asm", "s"];

/// Returns the assembly grammar of the bundled parser.
fn asm_language() -> Language {
    tree_sitter_asm::LANGUAGE.into()
}

/// Returns the initialization options of `asm-lsp`.
///
/// `asm-lsp` needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn asm_lsp_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the assembly adapter declares, in declaration
/// order.
const ASM_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "asm_lsp",
    program: "asm-lsp",
    args: &[],
    language_id: "asm",
    // The server supplies no document formatting, so it never receives a
    // formatting request. Assembly declares no external formatter either, so an
    // assembly buffer shows no format-on-save state.
    formatting: ServerFormatting::Disabled,
    // The server reads its own configuration file when the workspace holds one,
    // and it falls back to its built-in instruction data otherwise. It
    // therefore serves every workspace that holds an assembly file, so no
    // marker gates its start.
    root_markers: &[],
    initialization_options: asm_lsp_options,
}];

/// The language adapter for assembly source paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{AsmAdapter, LanguageAdapter};
///
/// let adapter = AsmAdapter::new();
/// assert!(adapter.supports_path(Path::new("boot.s")));
/// // The preprocessed source carries the uppercase extension.
/// assert!(adapter.supports_path(Path::new("boot.S")));
/// assert_eq!(adapter.comment().line_token(), Some("#"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AsmAdapter;

impl AsmAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for AsmAdapter {
    fn id(&self) -> &'static str {
        "asm"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &ASM_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        // The grammar accepts `#`, `//`, and `;` as a line comment, because it
        // serves several assembler dialects. Kvim targets macOS and Linux,
        // where the GNU assembler reads the file, so the toggle writes the `#`
        // token of that assembler. The same assembler reads the C block
        // comment on every target.
        CommentStyle::new(Some("#"), Some(BlockComment::new("/*", "*/")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "asm",
            language: asm_language,
            highlights_query: tree_sitter_asm::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        // Assembly nests through no bracketed node. The grammar holds one node
        // for each line, so no node kind adds an indent level and no character
        // closes one.
        IndentRule {
            scopes: &[],
            closing_delimiters: &[],
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &ASM_SERVERS
    }
}
