//! The bounds that one highlight request carries.
//!
//! The caller owns every bound, because a buffer and a chat fragment hold a
//! different quantity of text. Each bound has a documented default that suits
//! one editor buffer.

/// The quantity that one bound measures.
///
/// A truncated result names the bound that stopped the walk, so a consumer can
/// report which limit it reached instead of guessing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LimitKind {
    /// The fragment size, in bytes.
    SourceBytes,
    /// The depth of the syntax tree that the error walk descends.
    ParseDepth,
    /// The number of parser progress steps.
    ParserWork,
    /// The depth of the active capture stack.
    CaptureDepth,
    /// The number of highlight spans.
    Spans,
    /// The number of reported syntax errors.
    SyntaxErrors,
}

/// The default fragment bound, in bytes.
pub const SOURCE_BYTES_DEFAULT: usize = 4 * 1024 * 1024;

/// The default syntax-tree depth bound of the error walk.
pub const PARSE_DEPTH_DEFAULT: usize = 128;

/// The default parser progress bound, in steps.
///
/// The parser reports progress while it works, so a fragment that makes the
/// grammar run without an end stops here instead of holding its worker.
pub const PARSER_WORK_DEFAULT: usize = 100_000;

/// The default capture-stack depth bound.
pub const CAPTURE_DEPTH_DEFAULT: usize = 128;

/// The default highlight-span bound.
///
/// The densest measured real source produces one span for each 5.8 bytes, so
/// [`SOURCE_BYTES_DEFAULT`] produces about 727000 spans. One span holds 16
/// bytes, so this bound retains 12 MB for one fragment.
pub const SPANS_DEFAULT: usize = 750_000;

/// The default syntax-error bound.
pub const SYNTAX_ERRORS_DEFAULT: usize = 1_000;

/// The bounds of one highlight request.
///
/// Every bound is non-zero. The builder methods return the value, so a caller
/// composes only the bounds that it needs and keeps the defaults for the rest.
///
/// # Examples
///
/// ```
/// use kvim_syntax::HighlightLimits;
///
/// // One chat fragment is small, so its bounds are small.
/// let limits = HighlightLimits::default()
///     .with_source_bytes_max(64 * 1024)
///     .with_spans_max(4_000);
///
/// assert_eq!(limits.source_bytes_max(), 64 * 1024);
/// assert_eq!(limits.spans_max(), 4_000);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighlightLimits {
    source_bytes_max: usize,
    parse_depth_max: usize,
    parser_work_max: usize,
    capture_depth_max: usize,
    spans_max: usize,
    syntax_errors_max: usize,
}

impl Default for HighlightLimits {
    fn default() -> Self {
        Self {
            source_bytes_max: SOURCE_BYTES_DEFAULT,
            parse_depth_max: PARSE_DEPTH_DEFAULT,
            parser_work_max: PARSER_WORK_DEFAULT,
            capture_depth_max: CAPTURE_DEPTH_DEFAULT,
            spans_max: SPANS_DEFAULT,
            syntax_errors_max: SYNTAX_ERRORS_DEFAULT,
        }
    }
}

/// Returns one bound, or one when the caller passed zero.
///
/// A bound of zero would refuse every fragment, which no caller can mean, so
/// the smallest useful bound takes its place.
const fn at_least_one(value: usize) -> usize {
    if value == 0 { 1 } else { value }
}

impl HighlightLimits {
    /// Sets the largest fragment that one request reads, in bytes.
    #[must_use]
    pub const fn with_source_bytes_max(mut self, value: usize) -> Self {
        self.source_bytes_max = at_least_one(value);
        self
    }

    /// Sets the largest syntax-tree depth that the error walk descends.
    #[must_use]
    pub const fn with_parse_depth_max(mut self, value: usize) -> Self {
        self.parse_depth_max = at_least_one(value);
        self
    }

    /// Sets the largest number of parser progress steps.
    #[must_use]
    pub const fn with_parser_work_max(mut self, value: usize) -> Self {
        self.parser_work_max = at_least_one(value);
        self
    }

    /// Sets the largest capture-stack depth.
    #[must_use]
    pub const fn with_capture_depth_max(mut self, value: usize) -> Self {
        self.capture_depth_max = at_least_one(value);
        self
    }

    /// Sets the largest number of highlight spans.
    #[must_use]
    pub const fn with_spans_max(mut self, value: usize) -> Self {
        self.spans_max = at_least_one(value);
        self
    }

    /// Sets the largest number of reported syntax errors.
    #[must_use]
    pub const fn with_syntax_errors_max(mut self, value: usize) -> Self {
        self.syntax_errors_max = at_least_one(value);
        self
    }

    /// Returns the largest fragment that one request reads, in bytes.
    #[must_use]
    pub const fn source_bytes_max(self) -> usize {
        self.source_bytes_max
    }

    /// Returns the largest syntax-tree depth that the error walk descends.
    #[must_use]
    pub const fn parse_depth_max(self) -> usize {
        self.parse_depth_max
    }

    /// Returns the largest number of parser progress steps.
    #[must_use]
    pub const fn parser_work_max(self) -> usize {
        self.parser_work_max
    }

    /// Returns the largest capture-stack depth.
    #[must_use]
    pub const fn capture_depth_max(self) -> usize {
        self.capture_depth_max
    }

    /// Returns the largest number of highlight spans.
    #[must_use]
    pub const fn spans_max(self) -> usize {
        self.spans_max
    }

    /// Returns the largest number of reported syntax errors.
    #[must_use]
    pub const fn syntax_errors_max(self) -> usize {
        self.syntax_errors_max
    }
}
