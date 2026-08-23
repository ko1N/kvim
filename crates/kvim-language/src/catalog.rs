//! The grammar catalog of the supported languages.
//!
//! One catalog entry owns every value that selects a language and every value
//! that parses it: the language names, the path keys, the Tree-sitter grammar,
//! and its queries. An adapter refers to its entry instead of repeating a
//! lookup table, so two tables of one language can never disagree, and the
//! indentation, formatter, server, and editor-version behavior of that language
//! stays with the adapter. See `docs/language-services.md`.
//!
//! The entry also owns the compiled highlight configuration of its grammar.
//! Compiling one highlight query costs more than one parse, and the result
//! never changes, so the entry compiles it at most once and every later
//! analysis of that language reads the same value. The entry owns the value, so
//! the catalog releases it and no allocation outlives the catalog.

use std::sync::OnceLock;

use tree_sitter::Language;
use tree_sitter_highlight::HighlightConfiguration;

use super::AnalysisError;
use super::analysis::highlight_role;

/// The Tree-sitter grammar and the queries of one language.
///
/// The value is catalog data. The analysis reads it and knows no language.
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
/// the complete file names whose extension does not name their format. It also
/// answers the grammar that parses the language.
///
/// The grammar arrives through a function, because several languages compose
/// their highlight query from more than one crate and build that text once at
/// run time.
///
/// # Examples
///
/// ```
/// use kvim_language::{LanguageAdapter, RustAdapter};
///
/// let entry = RustAdapter::new().catalog();
/// assert_eq!(entry.id(), "rust");
/// assert!(entry.extensions().contains(&"rs"));
/// assert!(entry.language_names().contains(&"rust"));
/// ```
pub struct LanguageCatalogEntry {
    id: &'static str,
    language_names: &'static [&'static str],
    extensions: &'static [&'static str],
    file_names: &'static [&'static str],
    grammar: fn() -> Grammar,
    /// The compiled highlight configuration, after the first analysis built it.
    highlights: OnceLock<HighlightConfiguration>,
}

impl LanguageCatalogEntry {
    /// Creates the catalog entry of one language.
    ///
    /// `id` is the stable identifier of the language, and it also names the
    /// grammar in every parser failure.
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
            highlights: OnceLock::new(),
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

    /// Returns the compiled highlight configuration of this grammar.
    ///
    /// The first call compiles the query. Two callers that race compile it
    /// twice and keep one value; the other one is dropped, so the entry retains
    /// exactly one configuration.
    ///
    /// # Errors
    ///
    /// Returns [`AnalysisError::ParserSetup`] when the grammar or its highlight
    /// query does not compile.
    pub(super) fn highlight_configuration(&self) -> Result<&HighlightConfiguration, AnalysisError> {
        if let Some(configuration) = self.highlights.get() {
            return Ok(configuration);
        }
        let configuration = self.compile_highlights()?;
        Ok(self.highlights.get_or_init(|| configuration))
    }

    /// Compiles the highlight query of this grammar.
    fn compile_highlights(&self) -> Result<HighlightConfiguration, AnalysisError> {
        let grammar = self.grammar();
        let mut configuration = HighlightConfiguration::new(
            (grammar.language)(),
            self.id,
            grammar.highlights_query,
            grammar.injections_query,
            grammar.locals_query,
        )
        .map_err(|_| AnalysisError::ParserSetup)?;
        disable_captures_without_a_role(&mut configuration);
        // The identity mapping keeps every capture name, so the role lookup
        // reads the name that the query of the grammar defines.
        let names: Vec<String> = configuration
            .names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        configuration.configure(&names);
        Ok(configuration)
    }
}

/// Turns off every capture of one query that carries no role.
///
/// The highlighter keeps the last capture of one node and reads the role of
/// that capture alone. Several grammars mark one node twice, for example
/// `(comment) @comment @spell`, where the second name is a decoration marker of
/// another editor. The marker would take the place of the role and leave the
/// node plain, so the configuration turns every such capture off. A turned-off
/// capture never reaches a match, and the capture indices keep their order.
///
/// The function keeps the injection and the local captures, because the
/// highlighter reads those names itself and never asks for their role.
fn disable_captures_without_a_role(configuration: &mut HighlightConfiguration) {
    let disabled: Vec<String> = configuration
        .query
        .capture_names()
        .iter()
        .filter(|name| {
            let owner = name.split('.').next().unwrap_or(name);
            !matches!(owner, "injection" | "local")
                && highlight_role(name, &[]).is_none()
                && !name.is_empty()
        })
        .map(|name| (*name).to_owned())
        .collect();
    for name in &disabled {
        configuration.query.disable_capture(name);
    }
}
