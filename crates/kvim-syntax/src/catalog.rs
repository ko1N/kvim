//! The language catalog: what selects a language and what parses it.
//!
//! One catalog entry owns the language names, the file extensions, the complete
//! file names, and the Tree-sitter grammar with its queries. A consumer selects
//! an entry by language name or by path, and hands it to the highlighter.

use std::path::Path;

use tree_sitter::Language;

/// The Tree-sitter grammar and the queries of one language.
///
/// The value is catalog data. The highlighter reads it and names no language.
#[derive(Clone, Copy)]
pub struct Grammar {
    /// The entry point of the compiled grammar.
    pub language: fn() -> Language,
    /// The highlight query of the grammar.
    pub highlights_query: &'static str,
    /// The injection query, or the empty text when the grammar has none.
    pub injections_query: &'static str,
    /// The local-variable query, or the empty text when the grammar has none.
    pub locals_query: &'static str,
}

/// The catalog entry of one language.
///
/// The entry answers the three lookup keys of a selection: the language names
/// that a fence or a server writes, the file extensions of the language, and
/// the complete file names whose extension does not name their format.
///
/// The grammar arrives through a function, because several languages compose
/// their highlight query from more than one crate and build that text once at
/// run time.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "grammar-rust")] {
/// use std::path::Path;
///
/// let entry = kvim_syntax::language("rust").expect("the feature bundles Rust");
/// assert_eq!(entry.id(), "rust");
/// assert!(entry.owns_path(Path::new("src/main.rs")));
/// # }
/// ```
pub struct LanguageCatalogEntry {
    id: &'static str,
    language_names: &'static [&'static str],
    extensions: &'static [&'static str],
    file_names: &'static [&'static str],
    grammar: fn() -> Grammar,
}

impl LanguageCatalogEntry {
    /// Creates the catalog entry of one language.
    ///
    /// `id` is the stable identifier of the language. It also names the grammar
    /// in a compilation failure.
    #[must_use]
    pub const fn new(
        id: &'static str,
        language_names: &'static [&'static str],
        extensions: &'static [&'static str],
        file_names: &'static [&'static str],
        grammar: fn() -> Grammar,
    ) -> Self {
        Self {
            id,
            language_names,
            extensions,
            file_names,
            grammar,
        }
    }

    /// Returns the stable identifier of the language.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// Returns the names that this language answers to, in lower case.
    #[must_use]
    pub const fn language_names(&self) -> &'static [&'static str] {
        self.language_names
    }

    /// Returns the case-sensitive file extensions of this language.
    #[must_use]
    pub const fn extensions(&self) -> &'static [&'static str] {
        self.extensions
    }

    /// Returns the case-sensitive complete file names of this language.
    #[must_use]
    pub const fn file_names(&self) -> &'static [&'static str] {
        self.file_names
    }

    /// Returns the Tree-sitter grammar and the queries of this language.
    #[must_use]
    pub fn grammar(&self) -> Grammar {
        (self.grammar)()
    }

    /// Reports whether this language answers to one name.
    ///
    /// The match folds ASCII case, because the name is prose that an author or
    /// a server writes. The caller passes one complete name: a CommonMark info
    /// string may carry an attribute after the name, and the reader of the
    /// fence extracts the name before it asks.
    #[must_use]
    pub fn answers_to(&self, language: &str) -> bool {
        self.language_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(language))
    }

    /// Reports whether this language owns one path.
    ///
    /// The match is case-sensitive, because a path is a filesystem entity where
    /// the case names a different file. The rule reads the extension of the
    /// path and its complete file name, so one selection serves both keys.
    #[must_use]
    pub fn owns_path(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| self.extensions.iter().any(|known| *known == extension))
            || path
                .file_name()
                .is_some_and(|name| self.file_names.iter().any(|known| *known == name))
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
