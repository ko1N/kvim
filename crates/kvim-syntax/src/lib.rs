//! Bounded syntax highlighting for one fragment of source.
//!
//! The crate answers one question: which ranges of this text carry which
//! meaning? It names no editor, no language server, no terminal, and no
//! runtime. A consumer selects a language, calls the highlighter, and paints
//! the returned [`SyntaxRole`] values with a palette of its own.
//!
//! # Grammars
//!
//! No grammar is bundled by default. Each language sits behind a Cargo feature
//! named `grammar-<language>`, and `all-grammars` enables every one of them, so
//! a consumer compiles only what it needs.
//!
//! # Bounds and cancellation
//!
//! Every request carries [`HighlightLimits`]. A walk that reaches a bound stops
//! and reports [`Truncation`] with the bound that stopped it, so no result is
//! silently short. The caller also supplies a [`CancellationSignal`], which the
//! highlighter asks during parser and query work.
//!
//! [`SyntaxHighlighter::highlight`] is synchronous processor work. It creates
//! no task and reads no clock. A consumer submits it to a bounded worker of its
//! own, and that scheduler owns the deadline and the cancellation signal. No
//! host event loop calls it directly.
//!
//! # Examples
//!
//! The `highlight` example is one complete consumer that needs no editor and no
//! language server:
//!
//! ```text
//! cargo run -p kvim-syntax --example highlight \
//!     --no-default-features --features grammar-rust
//! ```
//!
//! ```
//! # #[cfg(feature = "grammar-rust")] {
//! use kvim_syntax::{HighlightLimits, NeverCancelled, SyntaxHighlighter};
//!
//! let entry = kvim_syntax::language("rust").expect("the feature bundles Rust");
//! let mut highlighter = SyntaxHighlighter::new();
//! let highlighted = highlighter
//!     .highlight(entry, "fn main() {}\n", &HighlightLimits::default(), &NeverCancelled)
//!     .expect("the fragment stays inside every bound");
//!
//! assert!(!highlighted.spans().is_empty());
//! assert!(highlighted.errors().is_empty());
//! # }
//! ```

use std::path::Path;

mod catalog;
mod grammars;
mod highlight;
mod limits;
mod role;

#[cfg(test)]
mod tests;

pub use catalog::{Grammar, LanguageCatalogEntry};
pub use grammars::{BUNDLED_LANGUAGES_MAX, bundled};
pub use highlight::{
    CancellationSignal, HIGHLIGHT_CACHE_ENTRIES_MAX, HighlightFailure, Highlighted, NeverCancelled,
    SyntaxError, SyntaxHighlighter, Truncation,
};
pub use limits::{
    CAPTURE_DEPTH_DEFAULT, HighlightLimits, LimitKind, PARSE_DEPTH_DEFAULT, PARSER_WORK_DEFAULT,
    SOURCE_BYTES_DEFAULT, SPANS_DEFAULT, SYNTAX_ERRORS_DEFAULT,
};
pub use role::{HighlightSpan, SyntaxRole};

/// Returns the bundled language that answers to one name.
///
/// The match folds ASCII case, because the name is prose that an author or a
/// server writes. A name that this build bundles no grammar for returns `None`,
/// which is no failure: the consumer paints the fragment as plain text.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "grammar-rust")] {
/// assert_eq!(kvim_syntax::language("Rust").map(|entry| entry.id()), Some("rust"));
/// # }
/// assert!(kvim_syntax::language("no-such-language").is_none());
/// ```
#[must_use]
pub fn language(name: &str) -> Option<&'static LanguageCatalogEntry> {
    bundled()
        .iter()
        .copied()
        .find(|entry| entry.answers_to(name))
}

/// Returns the bundled language that owns one path.
///
/// The match is case-sensitive, because a path is a filesystem entity where the
/// case names a different file. The lookup reads the extension of the path and
/// its complete file name.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "grammar-rust")] {
/// use std::path::Path;
///
/// let entry = kvim_syntax::language_of_path(Path::new("src/main.rs"));
/// assert_eq!(entry.map(|entry| entry.id()), Some("rust"));
/// // The match is case-sensitive, so an uppercase extension names no language.
/// assert!(kvim_syntax::language_of_path(Path::new("MAIN.RS")).is_none());
/// # }
/// ```
#[must_use]
pub fn language_of_path(path: &Path) -> Option<&'static LanguageCatalogEntry> {
    bundled()
        .iter()
        .copied()
        .find(|entry| entry.owns_path(path))
}
