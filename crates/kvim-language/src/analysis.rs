//! The language-neutral Tree-sitter analysis.
//! Adapted from ReviewGraph (MIT), src/analysis.rs.
//!
//! Nothing in this file names one language. The parse, the highlight walk, and
//! the indent query all read the [`Grammar`](super::Grammar) and the
//! [`IndentRule`](super::IndentRule) that one adapter supplies as data. A new
//! language therefore needs one new adapter, and no change here.

use std::sync::Mutex;

use tokio_util::sync::CancellationToken;
use tree_sitter::{Node, ParseOptions, ParseState, Parser, Tree};
use tree_sitter_highlight::{
    Error as HighlightError, HighlightConfiguration, HighlightEvent, Highlighter,
};

use super::{
    ANALYSIS_DEPTH_MAX, ANALYSIS_HIGHLIGHT_SPANS_MAX, ANALYSIS_NODES_MAX, Analysis, AnalysisError,
    AnalysisInput, BoundMeasure, Grammar, HighlightSpan, IndentLevel, IndentRule, LanguageAdapter,
    SyntaxRole, analysis, enforce_count, previous_tree, validate_source,
};

/// The compiled highlight configuration of each grammar that ran once.
///
/// Compiling one highlight query costs more than one parse, and the result is
/// immutable after setup, so every analysis of one language shares one value.
/// The table holds at most one entry for each registered adapter, so the
/// retained memory stays bounded by the registry.
static CONFIGURATIONS: Mutex<Vec<(&'static str, &'static HighlightConfiguration)>> =
    Mutex::new(Vec::new());

/// Parses one buffer version and collects its bounded highlight spans.
///
/// The parse reuses the moved tree of the previous version when the caller
/// supplies one, so a small change does not reparse the complete buffer.
pub(super) fn analyze<A>(
    adapter: &A,
    input: &AnalysisInput,
    cancellation: &CancellationToken,
) -> Result<Analysis, AnalysisError>
where
    A: LanguageAdapter + ?Sized,
{
    let source = input.source();
    validate_source(source)?;
    if cancellation.is_cancelled() {
        return Err(AnalysisError::Cancelled);
    }

    let grammar = adapter.grammar();
    let tree = parse(grammar, source, previous_tree(input), cancellation)?;
    if cancellation.is_cancelled() {
        return Err(AnalysisError::Cancelled);
    }
    enforce_count(
        tree.root_node().descendant_count(),
        ANALYSIS_NODES_MAX,
        BoundMeasure::Nodes,
    )?;

    let highlights = collect_highlights(grammar, source, cancellation)?;
    Ok(analysis(input, tree, highlights, adapter.indent_rule()))
}

/// Parses the source with the grammar of one adapter.
fn parse(
    grammar: Grammar,
    source: &str,
    previous: Option<&Tree>,
    cancellation: &CancellationToken,
) -> Result<Tree, AnalysisError> {
    let mut parser = Parser::new();
    parser
        .set_language(&(grammar.language)())
        .map_err(|_| AnalysisError::ParserSetup)?;
    // The parser stops as soon as the callback reports cancellation, so a
    // superseded request releases its worker permit early.
    let mut progress = |_: &ParseState| cancellation.is_cancelled();
    let bytes = source.as_bytes();
    let mut read = |offset: usize, _| bytes.get(offset..).unwrap_or_default();
    parser
        .parse_with_options(
            &mut read,
            previous,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
        .ok_or_else(|| {
            if cancellation.is_cancelled() {
                AnalysisError::Cancelled
            } else {
                AnalysisError::ParseFailure
            }
        })
}

/// Returns the shared highlight configuration of one grammar.
fn highlight_configuration(
    grammar: Grammar,
) -> Result<&'static HighlightConfiguration, AnalysisError> {
    let mut cache = CONFIGURATIONS
        .lock()
        .expect("the cache mutex guards only local vector operations that cannot panic");
    if let Some((_, configuration)) = cache.iter().find(|(name, _)| *name == grammar.name) {
        return Ok(configuration);
    }

    let mut configuration = HighlightConfiguration::new(
        (grammar.language)(),
        grammar.name,
        grammar.highlights_query,
        grammar.injections_query,
        grammar.locals_query,
    )
    .map_err(|_| AnalysisError::ParserSetup)?;
    disable_captures_without_a_role(&mut configuration);
    // The identity mapping keeps every capture name, so the role lookup reads
    // the name that the query of the grammar defines.
    let names: Vec<String> = configuration
        .names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    configuration.configure(&names);
    let shared: &'static HighlightConfiguration = Box::leak(Box::new(configuration));
    cache.push((grammar.name, shared));
    Ok(shared)
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

/// Collects the bounded highlight spans of one source.
///
/// The highlighter reports one flat, ordered sequence of ranges with an active
/// capture stack, so the innermost capture decides the role of a range.
fn collect_highlights(
    grammar: Grammar,
    source: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<HighlightSpan>, AnalysisError> {
    let configuration = highlight_configuration(grammar)?;
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
            return Err(AnalysisError::Cancelled);
        }
        match event.map_err(map_highlight_error)? {
            HighlightEvent::HighlightStart(highlight) => {
                enforce_count(active.len() + 1, ANALYSIS_DEPTH_MAX, BoundMeasure::Depth)?;
                active.push(highlight.0);
            }
            HighlightEvent::HighlightEnd => {
                active.pop().ok_or(AnalysisError::MalformedOutput)?;
            }
            HighlightEvent::Source { start, end } => {
                if start > end || end > bytes.len() {
                    return Err(AnalysisError::MalformedOutput);
                }
                let Some(role) = active.iter().rev().find_map(|index| {
                    names
                        .get(*index)
                        .and_then(|name| highlight_role(name, &bytes[start..end]))
                }) else {
                    continue;
                };
                push_spans(bytes, &mut lines, start, end, role, &mut spans)?;
            }
        }
    }
    if !active.is_empty() {
        return Err(AnalysisError::MalformedOutput);
    }
    Ok(spans)
}

/// Maps one capture name of a highlight query to one syntax role.
///
/// Tree-sitter highlight queries share one capture vocabulary across grammars,
/// so the mapping stays language-neutral. The first component of a dotted name
/// carries the meaning. A constant that starts with a digit is a numeric
/// literal, which several queries capture as a constant.
fn highlight_role(name: &str, bytes: &[u8]) -> Option<SyntaxRole> {
    let mut parts = name.split('.');
    let prefix = parts.next()?;
    match prefix {
        "attribute" => Some(SyntaxRole::Attribute),
        "boolean" => Some(SyntaxRole::Boolean),
        // A character literal is a string of one character, so it takes the
        // string role.
        "character" => Some(SyntaxRole::String),
        "comment" => Some(SyntaxRole::Comment),
        "constant" if bytes.first().is_some_and(u8::is_ascii_digit) => Some(SyntaxRole::Number),
        "constant" => Some(SyntaxRole::Constant),
        "constructor" => Some(SyntaxRole::Constructor),
        // The C family names a comma and a semicolon `delimiter`, while the
        // other grammars name the same characters `punctuation.delimiter`.
        "delimiter" => Some(SyntaxRole::Delimiter),
        "escape" | "string" => Some(SyntaxRole::String),
        "function" if name.split('.').any(|part| part == "macro") => Some(SyntaxRole::Macro),
        "function" => Some(SyntaxRole::Function),
        "keyword" => Some(SyntaxRole::Keyword),
        "label" => Some(SyntaxRole::Statement),
        // A module name names a namespace of declarations, so it takes the type
        // role of that namespace.
        "module" => Some(SyntaxRole::Type),
        "number" => Some(SyntaxRole::Number),
        "operator" => Some(SyntaxRole::Operator),
        "preproc" => Some(SyntaxRole::Preprocessor),
        "property" => Some(SyntaxRole::Property),
        "punctuation" => match parts.next() {
            Some("bracket") => Some(SyntaxRole::Bracket),
            Some("delimiter") => Some(SyntaxRole::Delimiter),
            _ => Some(SyntaxRole::Operator),
        },
        // The `text` family belongs to the prose grammars of the same shared
        // vocabulary. Each name maps onto the role that carries the same
        // meaning, because the role set names source meaning and stays fixed.
        "text" => match parts.next() {
            Some("literal" | "uri") => Some(SyntaxRole::String),
            Some("reference") => Some(SyntaxRole::Constant),
            Some("title") => Some(SyntaxRole::Type),
            _ => None,
        },
        "type" => Some(SyntaxRole::Type),
        "variable" => match parts.next() {
            Some("parameter") => Some(SyntaxRole::Parameter),
            _ => Some(SyntaxRole::Variable),
        },
        _ => None,
    }
}

/// The line and the line start of the last visited byte offset.
///
/// The highlighter reports ascending ranges, so one forward walk over the
/// source converts every range. A scan from the start of the source for each
/// range would cost the square of the source length.
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
        debug_assert!(
            byte >= self.position,
            "the highlighter reports ascending ranges, so the cursor never moves back"
        );
        let end = byte.min(source.len());
        for (offset, value) in source[self.position..end].iter().enumerate() {
            if *value == b'\n' {
                self.line += 1;
                self.line_start = self.position + offset + 1;
            }
        }
        self.position = byte;
    }
}

/// Splits one source range into per-line spans with byte columns.
fn push_spans(
    source: &[u8],
    lines: &mut LineCursor,
    start: usize,
    end: usize,
    role: SyntaxRole,
    spans: &mut Vec<HighlightSpan>,
) -> Result<(), AnalysisError> {
    lines.advance_to(source, start);
    let mut segment_start = start;
    while segment_start < end {
        let segment_end = source[segment_start..end]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(end, |offset| segment_start + offset);
        if segment_start < segment_end {
            enforce_count(
                spans.len() + 1,
                ANALYSIS_HIGHLIGHT_SPANS_MAX,
                BoundMeasure::HighlightSpans,
            )?;
            spans.push(HighlightSpan {
                line: narrowed(lines.line)?,
                start_byte: narrowed(segment_start - lines.line_start)?,
                end_byte: narrowed(segment_end - lines.line_start)?,
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

/// Narrows one span coordinate to the published width.
fn narrowed(value: usize) -> Result<u32, AnalysisError> {
    u32::try_from(value).map_err(|_| AnalysisError::MalformedOutput)
}

/// Maps one highlighter failure to a typed analysis failure.
fn map_highlight_error(error: HighlightError) -> AnalysisError {
    match error {
        HighlightError::Cancelled => AnalysisError::Cancelled,
        HighlightError::InvalidLanguage => AnalysisError::ParserSetup,
        HighlightError::Unknown => AnalysisError::ParseFailure,
    }
}

/// Returns the indent level of a new line at one byte offset.
///
/// The walk counts the enclosing indent scopes of the position. A closing
/// delimiter that follows the position closes the innermost scope, so the new
/// line loses that level again. Both the scope kinds and the delimiters are
/// adapter data, so the rule stays language-neutral.
pub(super) fn indent_level(
    tree: &Tree,
    rule: IndentRule,
    source: &str,
    byte: usize,
) -> Result<IndentLevel, AnalysisError> {
    if byte > source.len() || !source.is_char_boundary(byte) {
        return Err(AnalysisError::MalformedOutput);
    }
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte);
    let mut levels: u16 = 0;
    let mut depth = 0;
    while let Some(current) = node {
        depth += 1;
        enforce_count(depth, ANALYSIS_DEPTH_MAX, BoundMeasure::Depth)?;
        if encloses(rule, current, byte) {
            levels = levels.saturating_add(1);
        }
        node = current.parent();
    }
    if closes_scope(rule, &source[byte..]) {
        levels = levels.saturating_sub(1);
    }
    Ok(IndentLevel::new(levels))
}

/// Reports whether one node is an indent scope that holds the position inside.
fn encloses(rule: IndentRule, node: Node<'_>, byte: usize) -> bool {
    rule.scopes.contains(&node.kind()) && node.start_byte() < byte && byte < node.end_byte()
}

/// Reports whether the new line starts with a closing delimiter.
fn closes_scope(rule: IndentRule, rest: &str) -> bool {
    rest.trim_start_matches([' ', '\t'])
        .starts_with(rule.closing_delimiters)
}
