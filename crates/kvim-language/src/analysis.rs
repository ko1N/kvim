//! The language-neutral Tree-sitter analysis.
//! Adapted from ReviewGraph (MIT), src/analysis.rs.
//!
//! Nothing in this file names one language. The parse and the indent query both
//! read the catalog entry and the [`IndentRule`](super::IndentRule) that one
//! adapter supplies as data. A new language therefore needs one new adapter,
//! and no change here.
//!
//! The highlight walk itself belongs to `kvim-syntax`, which owns the grammar
//! catalog and the bounded highlighter. This file keeps the parse, because the
//! editor also needs the syntax tree for the indent query and for the reuse
//! input of the next parse.

use tokio_util::sync::CancellationToken;
use tree_sitter::{Node, ParseOptions, ParseState, Parser, Tree};

use kvim_syntax::{
    Grammar, HighlightLimits, Highlighted, LimitKind, SyntaxHighlighter, Truncation,
};

use super::LanguageAdapter;
use super::{
    ANALYSIS_DEPTH_MAX, ANALYSIS_HIGHLIGHT_SPANS_MAX, ANALYSIS_NODES_MAX,
    ANALYSIS_SOURCE_BYTES_MAX, Analysis, AnalysisError, AnalysisInput, BoundMeasure, IndentLevel,
    IndentRule, IndentScope, analysis, enforce_count, previous_tree, validate_source,
};

/// Returns the bounds that one buffer analysis gives the highlighter.
///
/// The values repeat the analysis bounds of this crate, so a buffer keeps the
/// limits that `docs/language-services.md` records.
pub(super) fn buffer_limits() -> HighlightLimits {
    HighlightLimits::default()
        .with_source_bytes_max(ANALYSIS_SOURCE_BYTES_MAX)
        .with_parse_depth_max(ANALYSIS_DEPTH_MAX)
        .with_capture_depth_max(ANALYSIS_DEPTH_MAX)
        .with_spans_max(ANALYSIS_HIGHLIGHT_SPANS_MAX)
}

/// Parses one buffer version and collects its bounded highlight spans.
///
/// The parse reuses the moved tree of the previous version when the caller
/// supplies one, so a small change does not reparse the complete buffer.
///
/// The caller owns `highlighter`, which keeps the compiled query of each
/// language that it served. One analysis runs at a time, so one highlighter
/// serves the editor.
pub(super) fn analyze<A>(
    adapter: &A,
    input: &AnalysisInput,
    highlighter: &mut SyntaxHighlighter,
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

    let entry = adapter.catalog();
    let tree = parse(entry.grammar(), source, previous_tree(input), cancellation)?;
    if cancellation.is_cancelled() {
        return Err(AnalysisError::Cancelled);
    }
    enforce_count(
        tree.root_node().descendant_count(),
        ANALYSIS_NODES_MAX,
        BoundMeasure::Nodes,
    )?;

    let highlighted = highlighter
        .highlight(entry, source, &buffer_limits(), &|| {
            cancellation.is_cancelled()
        })
        .map_err(AnalysisError::from)?;
    // The public facade truncates and names the bound that stopped it. The
    // editor publishes no partial buffer analysis, so a truncated result
    // renders plain text instead of highlighting the head of a file and
    // leaving its tail bare. See `docs/language-services.md`.
    if let Some(bounds) = rejected_truncation(&highlighted) {
        return Err(bounds);
    }
    Ok(analysis(
        input,
        tree,
        highlighted.spans().to_vec(),
        adapter.indent_rule(),
    ))
}

/// Returns the bounds failure of one truncated buffer analysis.
///
/// A truncated syntax-error list changes no published value, because a buffer
/// analysis carries spans alone, so that bound alone keeps the result.
fn rejected_truncation(highlighted: &Highlighted) -> Option<AnalysisError> {
    let Truncation::Truncated { limit } = highlighted.truncation() else {
        return None;
    };
    let (measure, bound) = match limit {
        LimitKind::Spans => (BoundMeasure::HighlightSpans, ANALYSIS_HIGHLIGHT_SPANS_MAX),
        LimitKind::CaptureDepth | LimitKind::ParseDepth => {
            (BoundMeasure::Depth, ANALYSIS_DEPTH_MAX)
        }
        LimitKind::SourceBytes => (BoundMeasure::Bytes, ANALYSIS_SOURCE_BYTES_MAX),
        LimitKind::ParserWork | LimitKind::SyntaxErrors => return None,
        _ => return None,
    };
    Some(AnalysisError::Bounds {
        measure,
        limit: bound,
        // The walk stopped at the bound, so the kept count names what the
        // analysis would have published.
        actual: highlighted.spans().len(),
    })
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
    // A node that no delimiter closes ends at the last token of its content.
    // A new line at the end of that content names a byte that the node no
    // longer holds, so `descendant_for_byte_range(byte, byte)` answers with
    // an outer node and the walk never reaches the node itself. Starting one
    // byte earlier names the character that ends the current line, so the
    // walk reaches that node and every ancestor. That earlier byte can fall
    // inside one multi-byte character; Tree-sitter reads bytes, not
    // characters, so the range still names the right character.
    let mut node = tree
        .root_node()
        .descendant_for_byte_range(byte.saturating_sub(1), byte);
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
    rule.scopes
        .iter()
        .any(|&scope| scope.kind() == node.kind() && indent_range_holds(scope, node, byte))
}

/// Reports whether the indent range of one scope holds the position.
///
/// The range starts at the first byte of the node and ends where the body of
/// the scope starts. A scope that names no body ends at the node itself.
///
/// The body bound keeps a scope from indenting its own body a second time. A
/// Nix `let_expression` spans the attribute set after its `in` keyword, and
/// that attribute set is an indent scope of its own. A `let` that reached its
/// own end would therefore add one level to every line of that body, and the
/// last `};` of the file would take two levels instead of one.
fn indent_range_holds(scope: IndentScope, node: Node<'_>, byte: usize) -> bool {
    let body = scope
        .body()
        .and_then(|field| node.child_by_field_name(field));
    let end = match body {
        Some(body) => body.start_byte(),
        None => node.end_byte(),
    };
    node.start_byte() < byte && byte < end
}

/// Reports whether the new line starts with a closing delimiter.
fn closes_scope(rule: IndentRule, rest: &str) -> bool {
    rest.trim_start_matches([' ', '\t'])
        .starts_with(rule.closing_delimiters)
}
