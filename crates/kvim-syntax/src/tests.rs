//! Tests for stable grammar selection, the bounded highlighter, and its outcomes.
//!
//! Every test runs with the grammar features that the build enables. The tests
//! that need one concrete grammar name the Rust feature, which the verification
//! command of this crate enables.

use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    BUNDLED_LANGUAGES_MAX, HighlightLimits, LimitKind, NeverCancelled, SyntaxHighlighter,
    Truncation, bundled,
};

#[test]
fn every_bundled_language_has_a_unique_stable_identifier() {
    for (index, entry) in bundled().iter().enumerate() {
        assert!(
            bundled()[index + 1..]
                .iter()
                .all(|other| other.id() != entry.id()),
            "the {} grammar identifier is unique",
            entry.id(),
        );
        assert_eq!(
            super::language(entry.id()).map(|selected| selected.id()),
            Some(entry.id()),
            "each bundled grammar selects by its stable identifier",
        );
    }
    assert!(bundled().len() <= BUNDLED_LANGUAGES_MAX);
}

#[test]
fn an_identifier_that_no_feature_bundles_selects_nothing() {
    assert!(super::language("no-such-language").is_none());
}

#[cfg(feature = "grammar-rust")]
mod rust {
    use super::{
        AtomicBool, HighlightLimits, LimitKind, NeverCancelled, Ordering, SyntaxHighlighter,
        Truncation,
    };
    use crate::{HighlightFailure, SyntaxRole};

    /// Returns the bundled Rust language.
    fn rust() -> &'static crate::LanguageCatalogEntry {
        crate::language("rust").expect("the test build enables the Rust feature")
    }

    #[test]
    fn a_grammar_selects_only_by_its_exact_stable_identifier() {
        assert_eq!(
            crate::language("rust").map(|entry| entry.id()),
            Some("rust")
        );
        assert!(crate::language("rs").is_none());
        assert!(crate::language("RUST").is_none());
    }

    #[test]
    fn one_fragment_carries_the_roles_of_its_source() {
        let mut highlighter = SyntaxHighlighter::new();
        let highlighted = highlighter
            .highlight(
                rust(),
                "fn main() {\n    let value = 1;\n}\n",
                &HighlightLimits::default(),
                &NeverCancelled,
            )
            .expect("the fragment stays inside every bound");

        assert_eq!(highlighted.truncation(), Truncation::Complete);
        assert!(highlighted.errors().is_empty());
        let roles: Vec<SyntaxRole> = highlighted.spans().iter().map(|span| span.role).collect();
        assert!(roles.contains(&SyntaxRole::Keyword), "{roles:?}");
        assert!(roles.contains(&SyntaxRole::Function), "{roles:?}");
        assert!(roles.contains(&SyntaxRole::Number), "{roles:?}");
        // Every span names a byte range inside its own line.
        for span in highlighted.spans() {
            assert!(span.start_byte < span.end_byte, "{span:?}");
        }
    }

    #[test]
    fn a_malformed_fragment_still_carries_spans_and_names_its_errors() {
        let mut highlighter = SyntaxHighlighter::new();
        let highlighted = highlighter
            .highlight(
                rust(),
                "fn main( {\n    let value = ;\n",
                &HighlightLimits::default(),
                &NeverCancelled,
            )
            .expect("a malformed fragment is no failure");

        assert!(
            !highlighted.spans().is_empty(),
            "the grammar reads what it can and the result keeps those spans",
        );
        assert!(
            !highlighted.errors().is_empty(),
            "the result names the places that the grammar could not read",
        );
    }

    #[test]
    fn the_highlighter_compiles_one_query_for_each_language_once() {
        let mut highlighter = SyntaxHighlighter::new();
        assert_eq!(highlighter.cached_languages(), 0);
        for _ in 0..3 {
            highlighter
                .highlight(
                    rust(),
                    "fn main() {}\n",
                    &HighlightLimits::default(),
                    &NeverCancelled,
                )
                .expect("the fragment stays inside every bound");
        }
        assert_eq!(
            highlighter.cached_languages(),
            1,
            "the cache holds one compiled query for the one language that ran",
        );
    }

    #[test]
    fn a_fragment_above_the_source_bound_is_refused_without_a_parse() {
        let mut highlighter = SyntaxHighlighter::new();
        let error = highlighter
            .highlight(
                rust(),
                "fn main() {}\n",
                &HighlightLimits::default().with_source_bytes_max(4),
                &NeverCancelled,
            )
            .expect_err("the fragment passes the source bound");

        assert_eq!(
            error,
            HighlightFailure::SourceTooLarge {
                bytes: 13,
                max_bytes: 4,
            },
        );
        assert_eq!(
            highlighter.cached_languages(),
            0,
            "a refused request compiles no query",
        );
    }

    #[test]
    fn the_span_bound_truncates_the_report_and_names_itself() {
        let mut highlighter = SyntaxHighlighter::new();
        let highlighted = highlighter
            .highlight(
                rust(),
                "fn main() {\n    let value = 1;\n}\n",
                &HighlightLimits::default().with_spans_max(2),
                &NeverCancelled,
            )
            .expect("a bound truncates the report instead of failing");

        assert_eq!(highlighted.spans().len(), 2);
        assert_eq!(
            highlighted.truncation(),
            Truncation::Truncated {
                limit: LimitKind::Spans,
            },
        );
    }

    #[test]
    fn a_cancelled_request_stops_the_parser_and_the_query_walk() {
        let mut highlighter = SyntaxHighlighter::new();
        let cancelled = AtomicBool::new(true);
        let signal = || cancelled.load(Ordering::Relaxed);

        let error = highlighter
            .highlight(
                rust(),
                "fn main() {}\n",
                &HighlightLimits::default(),
                &signal,
            )
            .expect_err("a cancelled request publishes nothing");
        assert_eq!(error, HighlightFailure::Cancelled);

        // The same highlighter serves the next request, so one cancellation
        // leaves no unusable value behind.
        cancelled.store(false, Ordering::Relaxed);
        let highlighted = highlighter
            .highlight(
                rust(),
                "fn main() {}\n",
                &HighlightLimits::default(),
                &signal,
            )
            .expect("the second request runs to its end");
        assert!(!highlighted.spans().is_empty());
    }

    #[test]
    fn the_parser_work_bound_stops_a_request_that_would_run_without_an_end() {
        let mut highlighter = SyntaxHighlighter::new();
        // One progress step is fewer than any real parse needs, so the parser
        // stops at the bound and returns no tree.
        let error = highlighter
            .highlight(
                rust(),
                &"fn main() {}\n".repeat(2_000),
                &HighlightLimits::default().with_parser_work_max(1),
                &NeverCancelled,
            )
            .expect_err("the parser stops at its work bound");

        assert_eq!(error, HighlightFailure::ParseFailure);
    }
}
