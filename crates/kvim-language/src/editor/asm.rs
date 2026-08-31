//! The assembly language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, and the language servers. See
//! `docs/language-services.md`.

use std::num::NonZeroU8;
use std::sync::OnceLock;

use super::{BlockComment, CommentStyle, IndentRule, LanguageAdapter, LanguageCatalogEntry};

/// The number of columns that one assembly indent level takes.
const ASM_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// Returns the initialization options of `asm-lsp`.
///
/// `asm-lsp` needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.

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
    fn service_profile(&self) -> &'static crate::LanguageServiceProfile {
        &crate::profile::ASM_PROFILE
    }

    fn catalog(&self) -> &'static LanguageCatalogEntry {
        static ENTRY: OnceLock<&'static LanguageCatalogEntry> = OnceLock::new();
        ENTRY.get_or_init(|| {
            kvim_syntax::language("asm").expect("the grammar-asm feature bundles this language")
        })
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        // The grammar accepts `#`, `//`, and `;` as a line comment, because it
        // serves several assembler dialects. kvim targets macOS and Linux,
        // where the GNU assembler reads the file, so the toggle writes the `#`
        // token of that assembler. The same assembler reads the C block
        // comment on every target.
        CommentStyle::new(Some("#"), Some(BlockComment::new("/*", "*/")))
    }

    fn indent_rule(&self) -> IndentRule {
        // Assembly nests through no bracketed node. The grammar holds one node
        // for each line, so no node kind adds an indent level and no character
        // closes one.
        IndentRule {
            scopes: &[],
            width: ASM_INDENT_WIDTH,
            closing_delimiters: &[],
        }
    }
}
