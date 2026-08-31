//! The language catalog: stable grammar identity and parser data.

use tree_sitter::Language;

/// The Tree-sitter grammar and queries of one language.
#[derive(Clone, Copy)]
pub struct Grammar {
    /// The compiled grammar entry point.
    pub language: fn() -> Language,
    /// The highlight query.
    pub highlights_query: &'static str,
    /// The injection query, or empty text.
    pub injections_query: &'static str,
    /// The local-variable query, or empty text.
    pub locals_query: &'static str,
}

/// One bundled language grammar.
///
/// The stable identifier selects the grammar. Language aliases and path
/// selectors belong to `kvim-language`.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "grammar-rust")] {
/// let entry = kvim_syntax::language("rust").unwrap();
/// assert_eq!(entry.id(), "rust");
/// # }
/// ```
pub struct LanguageCatalogEntry {
    id: &'static str,
    grammar: fn() -> Grammar,
}

impl LanguageCatalogEntry {
    /// Creates one catalog entry from its stable identity and grammar data.
    #[must_use]
    pub const fn new(id: &'static str, grammar: fn() -> Grammar) -> Self {
        Self { id, grammar }
    }
    /// Returns the stable language identifier.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }
    /// Returns the grammar and queries.
    #[must_use]
    pub fn grammar(&self) -> Grammar {
        (self.grammar)()
    }
}

impl core::fmt::Debug for LanguageCatalogEntry {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LanguageCatalogEntry")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
