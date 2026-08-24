//! The bounded fragment highlighter.
//! Adapted from ReviewGraph (MIT), src/analysis.rs.
//!
//! [`SyntaxHighlighter`] owns one parser and one compiled query for each
//! language that it has highlighted. The cache is bounded, and dropping the
//! highlighter releases every compiled query with it.
//!
//! [`SyntaxHighlighter::highlight`] is synchronous processor work. It creates
//! no task, reads no clock, and reaches no runtime. A consumer submits it to a
//! bounded worker of its own, and that scheduler owns the deadline and the
//! cancellation signal.
//!
//! `crates/kvim-syntax/examples/highlight.rs` is the dedicated example of this
//! feature. It highlights one Rust fragment and prints the role of each range.

use tree_sitter::{ParseOptions, ParseState, Parser, Tree};
use tree_sitter_highlight::{
    Error as HighlightError, HighlightConfiguration, HighlightEvent, Highlighter,
};

use crate::catalog::LanguageCatalogEntry;
use crate::limits::{HighlightLimits, LimitKind};
use crate::role::{HighlightSpan, SyntaxRole, highlight_role};

/// The largest number of compiled queries that one highlighter retains.
///
/// One entry belongs to one language. A consumer that highlights more languages
/// than this drops the least recently compiled query and compiles it again on
/// the next request, so the retained memory stays bounded by this value.
pub const HIGHLIGHT_CACHE_ENTRIES_MAX: usize = 32;

/// Whether the highlighter reported everything that it found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Truncation {
    /// Every span and every syntax error of the fragment is present.
    Complete,
    /// One bound stopped the walk, so the report omits later data.
    Truncated {
        /// The bound that stopped the walk.
        limit: LimitKind,
    },
}

/// One place where the grammar could not read the fragment.
///
/// A malformed fragment still carries spans, so a consumer paints what the
/// grammar did read and names the rest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxError {
    /// The zero-based line of the first byte that the grammar could not read.
    pub line: u32,
    /// The first byte of the range inside the line.
    pub start_byte: u32,
    /// The byte after the range inside the line.
    pub end_byte: u32,
}

/// The complete result of one highlight request.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Highlighted {
    spans: Vec<HighlightSpan>,
    errors: Vec<SyntaxError>,
    truncation: Truncation,
}

impl Default for Truncation {
    fn default() -> Self {
        Self::Complete
    }
}

impl Highlighted {
    /// Returns the bounded highlight spans, in ascending order.
    #[must_use]
    pub fn spans(&self) -> &[HighlightSpan] {
        &self.spans
    }

    /// Returns the places where the grammar could not read the fragment.
    ///
    /// The list is empty for a fragment that the grammar read completely.
    #[must_use]
    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    /// Returns whether one bound stopped the walk.
    #[must_use]
    pub const fn truncation(&self) -> Truncation {
        self.truncation
    }
}

/// A refused highlight request.
///
/// Every variant is a normal state. Highlighting is decoration, so a consumer
/// paints the fragment as plain text and keeps working.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HighlightFailure {
    /// This build bundles no grammar for the language.
    #[error("no bundled grammar supports the language")]
    UnsupportedLanguage,
    /// The fragment is larger than the source bound of the request.
    #[error("the fragment holds {bytes} bytes; the limit is {max_bytes} bytes")]
    SourceTooLarge {
        /// The size of the rejected fragment, in bytes.
        bytes: usize,
        /// The configured bound, in bytes.
        max_bytes: usize,
    },
    /// The grammar or its highlight query did not compile.
    #[error("the grammar could not be configured")]
    GrammarSetup,
    /// The parser returned no syntax tree.
    #[error("the parser did not return a syntax tree")]
    ParseFailure,
    /// The scheduler cancelled the request.
    #[error("the request was cancelled")]
    Cancelled,
    /// The grammar reported a range that the fragment does not hold.
    #[error("the grammar returned malformed ranges")]
    MalformedRanges,
}

/// A cancellation signal that the scheduler owns.
///
/// The highlighter asks the signal during parser and query work, so a
/// superseded request releases its worker early. The trait carries no runtime
/// and no clock, so a consumer can drive it from any scheduler.
///
/// # Examples
///
/// ```
/// use std::sync::atomic::{AtomicBool, Ordering};
///
/// use kvim_syntax::CancellationSignal;
///
/// let stop = AtomicBool::new(false);
/// let signal = || stop.load(Ordering::Relaxed);
/// assert!(!signal.is_cancelled());
/// stop.store(true, Ordering::Relaxed);
/// assert!(signal.is_cancelled());
/// ```
pub trait CancellationSignal {
    /// Reports whether the scheduler cancelled the request.
    fn is_cancelled(&self) -> bool;
}

impl<F> CancellationSignal for F
where
    F: Fn() -> bool,
{
    fn is_cancelled(&self) -> bool {
        self()
    }
}

/// A signal that never cancels.
///
/// A consumer without a scheduler passes this value, which keeps the bounds of
/// the request as the only stopping rule.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NeverCancelled;

impl CancellationSignal for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// One owned highlighter with a bounded query cache.
///
/// Compiling one highlight query costs more than one parse, and the result
/// never changes, so the highlighter keeps the compiled query of each language
/// that it served. Dropping the highlighter releases every one of them.
///
/// The value is not shareable across threads by itself. A consumer that
/// highlights from several workers gives each worker its own highlighter, or
/// guards one behind its own lock.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "grammar-rust")] {
/// use kvim_syntax::{HighlightLimits, NeverCancelled, SyntaxHighlighter};
///
/// let mut highlighter = SyntaxHighlighter::new();
/// let entry = kvim_syntax::language("rust").expect("the feature bundles Rust");
/// let highlighted = highlighter
///     .highlight(entry, "fn main() {}\n", &HighlightLimits::default(), &NeverCancelled)
///     .expect("the fragment stays inside every bound");
///
/// assert!(!highlighted.spans().is_empty());
/// # }
/// ```
#[derive(Default)]
pub struct SyntaxHighlighter {
    /// The compiled query of each language that this highlighter served.
    ///
    /// The newest entry stands last, so the eviction drops the first one.
    cache: Vec<(&'static str, HighlightConfiguration)>,
}

impl core::fmt::Debug for SyntaxHighlighter {
    /// Names the cached languages, because a compiled query prints nothing.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SyntaxHighlighter")
            .field(
                "cached",
                &self.cache.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl SyntaxHighlighter {
    /// Creates one highlighter with an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self { cache: Vec::new() }
    }

    /// Returns the number of compiled queries that the cache retains.
    #[must_use]
    pub fn cached_languages(&self) -> usize {
        self.cache.len()
    }

    /// Highlights one fragment of one language.
    ///
    /// The call is synchronous processor work. It creates no task and reads no
    /// clock. The caller runs it on a bounded worker of its own and owns the
    /// deadline and the cancellation signal.
    ///
    /// A malformed fragment is no failure: the grammar reads what it can, and
    /// the result carries the spans that it found beside the bounded syntax
    /// errors that name the rest.
    ///
    /// # Errors
    ///
    /// Returns [`HighlightFailure`] for a language that this build bundles no
    /// grammar for, a fragment above the source bound, a grammar that does not
    /// compile, a parse that returns no tree, a cancelled request, and a
    /// grammar that reports a range outside the fragment.
    pub fn highlight(
        &mut self,
        entry: &'static LanguageCatalogEntry,
        source: &str,
        limits: &HighlightLimits,
        cancellation: &dyn CancellationSignal,
    ) -> Result<Highlighted, HighlightFailure> {
        if source.len() > limits.source_bytes_max() {
            return Err(HighlightFailure::SourceTooLarge {
                bytes: source.len(),
                max_bytes: limits.source_bytes_max(),
            });
        }
        if cancellation.is_cancelled() {
            return Err(HighlightFailure::Cancelled);
        }

        let tree = parse(entry, source, limits, cancellation)?;
        if cancellation.is_cancelled() {
            return Err(HighlightFailure::Cancelled);
        }
        let (errors, error_truncation) = collect_errors(&tree, source, limits);

        let configuration = self.configuration(entry)?;
        let (spans, span_truncation) = collect_spans(configuration, source, limits, cancellation)?;

        Ok(Highlighted {
            spans,
            errors,
            // The span walk runs after the error walk, so a span bound reports
            // the later stop of the two.
            truncation: match (span_truncation, error_truncation) {
                (Truncation::Complete, other) | (other, _) => other,
            },
        })
    }

    /// Returns the compiled query of one language, compiling it when needed.
    fn configuration(
        &mut self,
        entry: &'static LanguageCatalogEntry,
    ) -> Result<&HighlightConfiguration, HighlightFailure> {
        if let Some(index) = self.cache.iter().position(|(id, _)| *id == entry.id()) {
            return Ok(&self.cache[index].1);
        }
        let configuration = compile(entry)?;
        if self.cache.len() >= HIGHLIGHT_CACHE_ENTRIES_MAX {
            // The cache is bounded, so the oldest entry leaves and its language
            // compiles again on a later request.
            self.cache.remove(0);
        }
        self.cache.push((entry.id(), configuration));
        let last = self
            .cache
            .last()
            .expect("the push above left one entry in the cache");
        Ok(&last.1)
    }
}

/// Compiles the highlight query of one language.
fn compile(entry: &LanguageCatalogEntry) -> Result<HighlightConfiguration, HighlightFailure> {
    let grammar = entry.grammar();
    let mut configuration = HighlightConfiguration::new(
        (grammar.language)(),
        entry.id(),
        grammar.highlights_query,
        grammar.injections_query,
        grammar.locals_query,
    )
    .map_err(|_| HighlightFailure::GrammarSetup)?;
    disable_captures_without_a_role(&mut configuration);
    // The identity mapping keeps every capture name, so the role lookup reads
    // the name that the query of the grammar defines.
    let names: Vec<String> = configuration
        .names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    configuration.configure(&names);
    Ok(configuration)
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

/// Parses one fragment under the parser bounds of the request.
fn parse(
    entry: &LanguageCatalogEntry,
    source: &str,
    limits: &HighlightLimits,
    cancellation: &dyn CancellationSignal,
) -> Result<Tree, HighlightFailure> {
    let mut parser = Parser::new();
    parser
        .set_language(&(entry.grammar().language)())
        .map_err(|_| HighlightFailure::GrammarSetup)?;
    // The parser stops as soon as the callback reports a stop, so a superseded
    // request releases its worker early and a pathological fragment cannot run
    // without a bound.
    let mut steps = 0_usize;
    let work_max = limits.parser_work_max();
    let mut progress = |_: &ParseState| {
        steps = steps.saturating_add(1);
        steps > work_max || cancellation.is_cancelled()
    };
    let bytes = source.as_bytes();
    let mut read = |offset: usize, _| bytes.get(offset..).unwrap_or_default();
    parser
        .parse_with_options(
            &mut read,
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
        .ok_or_else(|| {
            if cancellation.is_cancelled() {
                HighlightFailure::Cancelled
            } else {
                HighlightFailure::ParseFailure
            }
        })
}

/// Collects the bounded syntax errors of one parsed fragment.
///
/// The walk uses an explicit stack, so a deep tree never grows the call stack.
/// It descends only into a subtree that carries an error, so a fragment without
/// one costs a single node visit.
fn collect_errors(
    tree: &Tree,
    source: &str,
    limits: &HighlightLimits,
) -> (Vec<SyntaxError>, Truncation) {
    let mut errors = Vec::new();
    let bytes = source.as_bytes();
    let mut lines = LineCursor::default();
    let mut stack = vec![(tree.root_node(), 0_usize)];
    while let Some((node, depth)) = stack.pop() {
        if !node.has_error() {
            continue;
        }
        if depth > limits.parse_depth_max() {
            return (
                errors,
                Truncation::Truncated {
                    limit: LimitKind::ParseDepth,
                },
            );
        }
        if node.is_error() || node.is_missing() {
            if errors.len() >= limits.syntax_errors_max() {
                return (
                    errors,
                    Truncation::Truncated {
                        limit: LimitKind::SyntaxErrors,
                    },
                );
            }
            if let Some(error) = syntax_error(bytes, &mut lines, node.start_byte(), node.end_byte())
            {
                errors.push(error);
            }
            continue;
        }
        // The children push in reverse, so the walk reports the errors of one
        // fragment in ascending order.
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    (errors, Truncation::Complete)
}

/// Returns one syntax error with line-relative byte columns.
fn syntax_error(
    source: &[u8],
    lines: &mut LineCursor,
    start: usize,
    end: usize,
) -> Option<SyntaxError> {
    if start > end || end > source.len() {
        return None;
    }
    lines.advance_to(source, start);
    let line = u32::try_from(lines.line).ok()?;
    let start_byte = u32::try_from(start - lines.line_start).ok()?;
    // A grammar error can span several lines. The report names its first line
    // and clips the range to that line, so a consumer paints one row.
    let line_end = source[start..end]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(end, |offset| start + offset);
    let end_byte = u32::try_from(line_end - lines.line_start).ok()?;
    Some(SyntaxError {
        line,
        start_byte,
        end_byte,
    })
}

/// Collects the bounded highlight spans of one fragment.
///
/// The highlighter reports one flat, ordered sequence of ranges with an active
/// capture stack, so the innermost capture decides the role of a range.
fn collect_spans(
    configuration: &HighlightConfiguration,
    source: &str,
    limits: &HighlightLimits,
    cancellation: &dyn CancellationSignal,
) -> Result<(Vec<HighlightSpan>, Truncation), HighlightFailure> {
    let names = configuration.names();
    let bytes = source.as_bytes();
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(configuration, bytes, None, |_| None)
        .map_err(map_highlight_error)?;

    let mut spans = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    let mut lines = LineCursor::default();
    for event in events {
        if cancellation.is_cancelled() {
            return Err(HighlightFailure::Cancelled);
        }
        match event.map_err(map_highlight_error)? {
            HighlightEvent::HighlightStart(highlight) => {
                if active.len() + 1 > limits.capture_depth_max() {
                    return Ok((
                        spans,
                        Truncation::Truncated {
                            limit: LimitKind::CaptureDepth,
                        },
                    ));
                }
                active.push(highlight.0);
            }
            HighlightEvent::HighlightEnd => {
                active.pop().ok_or(HighlightFailure::MalformedRanges)?;
            }
            HighlightEvent::Source { start, end } => {
                if start > end || end > bytes.len() {
                    return Err(HighlightFailure::MalformedRanges);
                }
                let Some(role) = active.iter().rev().find_map(|index| {
                    names
                        .get(*index)
                        .and_then(|name| highlight_role(name, &bytes[start..end]))
                }) else {
                    continue;
                };
                if push_spans(bytes, &mut lines, start, end, role, limits, &mut spans).is_err() {
                    return Ok((
                        spans,
                        Truncation::Truncated {
                            limit: LimitKind::Spans,
                        },
                    ));
                }
            }
        }
    }
    if !active.is_empty() {
        return Err(HighlightFailure::MalformedRanges);
    }
    Ok((spans, Truncation::Complete))
}

/// Reports that one span bound stopped the walk.
struct SpanBoundReached;

/// Splits one source range into per-line spans with byte columns.
fn push_spans(
    source: &[u8],
    lines: &mut LineCursor,
    start: usize,
    end: usize,
    role: SyntaxRole,
    limits: &HighlightLimits,
    spans: &mut Vec<HighlightSpan>,
) -> Result<(), SpanBoundReached> {
    lines.advance_to(source, start);
    let mut segment_start = start;
    while segment_start < end {
        let segment_end = source[segment_start..end]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(end, |offset| segment_start + offset);
        if segment_start < segment_end {
            if spans.len() + 1 > limits.spans_max() {
                return Err(SpanBoundReached);
            }
            let (Ok(line), Ok(start_byte), Ok(end_byte)) = (
                u32::try_from(lines.line),
                u32::try_from(segment_start - lines.line_start),
                u32::try_from(segment_end - lines.line_start),
            ) else {
                return Err(SpanBoundReached);
            };
            spans.push(HighlightSpan {
                line,
                start_byte,
                end_byte,
                role,
            });
        }
        if segment_end == end {
            break;
        }
        segment_start = segment_end + 1;
        lines.advance_to(source, segment_start);
    }
    Ok(())
}

/// The line and the line start of the last visited byte offset.
///
/// The highlighter reports ascending ranges, so one forward walk over the
/// fragment converts every range. A scan from the start for each range would
/// cost the square of the fragment length.
#[derive(Debug, Default)]
struct LineCursor {
    /// The byte offset that the cursor already counted.
    position: usize,
    /// The zero-based line of [`LineCursor::position`].
    line: usize,
    /// The byte offset at which that line starts.
    line_start: usize,
}

impl LineCursor {
    /// Moves the cursor forward to one byte offset.
    fn advance_to(&mut self, source: &[u8], byte: usize) {
        let end = byte.min(source.len());
        if end < self.position {
            // An error walk and a span walk each start at the beginning, so a
            // cursor that a caller reuses restarts instead of moving back.
            self.position = 0;
            self.line = 0;
            self.line_start = 0;
        }
        for (offset, value) in source[self.position..end].iter().enumerate() {
            if *value == b'\n' {
                self.line += 1;
                self.line_start = self.position + offset + 1;
            }
        }
        self.position = end;
    }
}

/// Maps one highlighter failure onto a typed outcome.
fn map_highlight_error(error: HighlightError) -> HighlightFailure {
    match error {
        HighlightError::Cancelled => HighlightFailure::Cancelled,
        HighlightError::InvalidLanguage => HighlightFailure::GrammarSetup,
        HighlightError::Unknown => HighlightFailure::MalformedRanges,
    }
}
