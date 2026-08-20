//! The rows that one language float paints, and the layout of one markup
//! document.
//!
//! `kvim-language` answers one [`MarkupDocument`]: blocks, styled pieces,
//! roles, and the highlight spans of each fence. That value measures no
//! terminal cell and names no glyph, because
//! `unicode-width` and the palette both live in this crate. This module is
//! therefore the one place that turns the document into rows of one width. It
//! chooses every glyph, it measures every cell, and it names one role for each
//! piece. The theme answers the color of that role, so no color stands here.
//!
//! See `docs/language-services.md` for the row rules and `docs/windows.md` for
//! the roles.

use kvim_language::{
    DiagnosticSeverity, HighlightSpan, MarkupBlock, MarkupBody, MarkupContainer, MarkupDocument,
    MarkupMarker, MarkupRole, StyledMarkup, SyntaxRole,
};

use super::cells::{clip_cells, text_cells, wrap_ranges};
use super::language::FLOAT_ROWS_MAX;

/// The glyph that marks one item of an unordered list.
const BULLET_GLYPH: &str = "•";

/// The glyph that rails every row of one block quote, and its blank.
const QUOTE_RAIL: &str = "│ ";

/// The glyph that draws one thematic break.
const RULE_GLYPH: &str = "─";

/// The cells that the list field of one document holds at least.
///
/// The field holds one marker and one blank, and the narrowest marker of a list
/// is the bullet of one cell.
const FLOAT_LIST_FIELD_CELLS_MIN: usize = 2;

/// The cells that the list field of one document holds at most.
///
/// A list of more items than the field holds digits for indents every one of
/// its rows, and the float shows at most [`FLOAT_ROWS_MAX`] rows, so a wider
/// field would spend the width of the float on numbers that no row shows.
const FLOAT_LIST_FIELD_CELLS_MAX: usize = 6;

/// What one piece of one float row is.
///
/// The style names the meaning of the piece, never its color. The overlay maps
/// each one to a theme role, exactly as it maps a highlight role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FloatStyle {
    /// Text that the surface band of the float paints alone.
    Plain,
    /// The message of one diagnostic, in the severity of that diagnostic.
    Severity(DiagnosticSeverity),
    /// One stretch of a markup document, in the role of that stretch.
    Markup(MarkupRole),
    /// One range of the code of one fence, in the syntax role of that range.
    ///
    /// The role is the one that the same code carries in a buffer, so one text
    /// takes one color in a hover answer and in an open file.
    Syntax(SyntaxRole),
    /// One glyph that this module draws itself, such as a thematic break.
    Structure,
}

impl FloatStyle {
    /// Returns the style of one row that carries a severity, or none.
    pub(super) const fn of_severity(severity: Option<DiagnosticSeverity>) -> Self {
        match severity {
            Some(severity) => Self::Severity(severity),
            None => Self::Plain,
        }
    }
}

/// One piece of one float row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FloatSpan {
    /// The text of the piece, without a line feed.
    pub(super) text: String,
    /// The style of the piece.
    pub(super) style: FloatStyle,
}

impl FloatSpan {
    /// Creates one piece of text in one style.
    fn new(text: impl Into<String>, style: FloatStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// One painted row of one float.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct FloatLine {
    /// The pieces of the row, from left to right.
    pub(super) spans: Vec<FloatSpan>,
}

impl FloatLine {
    /// Creates one row of one text in one style.
    pub(super) fn new(text: impl Into<String>, style: FloatStyle) -> Self {
        Self {
            spans: vec![FloatSpan::new(text, style)],
        }
    }

    /// Returns the terminal cells that the row occupies.
    pub(super) fn cells(&self) -> usize {
        span_cells(&self.spans)
    }

    /// Returns the text of the row, without its styles.
    #[cfg(test)]
    pub(super) fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// Returns the terminal cells that a sequence of pieces occupies.
fn span_cells(spans: &[FloatSpan]) -> usize {
    spans.iter().map(|span| text_cells(&span.text)).sum()
}

/// Returns the rows that one markup document paints at one width.
///
/// `cells` names the cells that one row of the float holds. The result holds at
/// most one row beyond [`FLOAT_ROWS_MAX`], because the caller needs to see only
/// that further rows exist.
pub(super) fn markup_lines(document: &MarkupDocument, cells: usize) -> Vec<FloatLine> {
    debug_assert!(cells >= 1, "the caller reserves at least one cell for text");
    let field = list_field(document);
    let mut lines: Vec<FloatLine> = Vec::new();
    let mut rules: Vec<usize> = Vec::new();

    for block in document.blocks() {
        if lines.len() > FLOAT_ROWS_MAX {
            break;
        }
        // The source separated two blocks with a blank line, and the first row
        // of the float needs no blank row above it.
        if block.is_spaced() && !lines.is_empty() {
            lines.push(FloatLine::default());
        }
        let prefix = Prefix::of(block.containers(), field);
        block_lines(block.body(), &prefix, cells, &mut lines, &mut rules);
    }
    draw_rules(&mut lines, &rules, cells);

    lines
}

/// Appends the rows of one block body.
fn block_lines(
    body: &MarkupBody,
    prefix: &Prefix,
    cells: usize,
    lines: &mut Vec<FloatLine>,
    rules: &mut Vec<usize>,
) {
    match body {
        MarkupBody::Prose(styled) => styled_lines(styled, prefix, cells, lines),
        MarkupBody::Heading { level, text } => {
            // The rank of a heading indents it by one cell for each rank below
            // the first, so no marker of the source reaches the screen and the
            // reader still sees which heading holds which.
            let prefix = prefix.indented(usize::from(*level).saturating_sub(1));
            styled_lines(text, &prefix, cells, lines);
        }
        MarkupBody::Code {
            lines: source,
            highlights,
            ..
        } => {
            let width = body_width(prefix, cells);
            for (index, line) in source.iter().enumerate() {
                if lines.len() > FLOAT_ROWS_MAX {
                    return;
                }
                let mut row = prefix.row(index);
                // A code line must not wrap, because a wrap would move its rest
                // under its own indentation. It loses its end instead.
                row.spans
                    .extend(code_spans(clip_cells(line, width), highlights, index));
                lines.push(row);
            }
        }
        // The width of a break follows every other row, so the caller draws it
        // after the last block.
        MarkupBody::Rule => {
            rules.push(lines.len());
            lines.push(prefix.row(0));
        }
    }
}

/// Returns the pieces of one row of one code block.
///
/// `line` names the row inside its own block, and one span of that row
/// addresses the text by its bytes, exactly as one span of a buffer line does.
/// The pieces partition the text: a range that one span names takes the syntax
/// role of that span, and every other range takes the code span role. A block
/// without a span therefore paints in one role, as every fence did before the
/// highlight.
///
/// The caller passes the text that the row paints, which the clip already
/// shortened. A span behind that cut adds no piece, and a span across it ends
/// at the cut, so the pieces stay aligned to what the row shows. The clip
/// counts terminal cells and never splits a character, so every kept range
/// still addresses a character boundary.
fn code_spans(text: &str, highlights: &[HighlightSpan], line: usize) -> Vec<FloatSpan> {
    let code = FloatStyle::Markup(MarkupRole::InlineCode);
    let Ok(line) = u32::try_from(line) else {
        debug_assert!(false, "one code block holds fewer lines than u32 counts");
        return vec![FloatSpan::new(text, code)];
    };

    let first = highlights.partition_point(|span| span.line < line);
    let mut spans: Vec<FloatSpan> = Vec::new();
    let mut painted = 0;

    for span in highlights[first..]
        .iter()
        .take_while(|span| span.line == line)
    {
        // A malformed span never breaks the partition: the range starts at the
        // end of the piece before it and stops at the end of the text.
        let start = (span.start_byte as usize).max(painted).min(text.len());
        let end = (span.end_byte as usize).min(text.len());
        if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        if painted < start {
            spans.push(FloatSpan::new(&text[painted..start], code));
        }
        spans.push(FloatSpan::new(
            &text[start..end],
            FloatStyle::Syntax(span.role),
        ));
        painted = end;
    }
    if painted < text.len() {
        spans.push(FloatSpan::new(&text[painted..], code));
    }

    debug_assert_eq!(
        spans.iter().map(|span| span.text.len()).sum::<usize>(),
        text.len(),
        "the pieces of one row partition the text of that row"
    );
    spans
}

/// Appends the wrapped rows of one styled text.
///
/// The wrap measures terminal cells, so it never splits a wide character, and
/// it answers byte ranges of the text, so every role survives a break on both
/// sides of it.
fn styled_lines(styled: &StyledMarkup, prefix: &Prefix, cells: usize, lines: &mut Vec<FloatLine>) {
    let text = styled.text();
    for (index, range) in wrap_ranges(text, body_width(prefix, cells))
        .into_iter()
        .enumerate()
    {
        if lines.len() > FLOAT_ROWS_MAX {
            return;
        }
        let mut row = prefix.row(index);
        row.spans.extend(
            styled
                .pieces_in(range.start, range.end)
                .into_iter()
                .map(|(piece, role)| FloatSpan::new(piece, FloatStyle::Markup(role))),
        );
        lines.push(row);
    }
}

/// Draws every thematic break after each other row is known.
///
/// A break is as wide as the widest other row, so a short answer keeps a narrow
/// float. A document that holds breaks alone has no other row, and its breaks
/// then take the whole width.
fn draw_rules(lines: &mut [FloatLine], rules: &[usize], cells: usize) {
    if rules.is_empty() {
        return;
    }
    let widest = lines
        .iter()
        .enumerate()
        .filter(|(index, _)| !rules.contains(index))
        .map(|(_, line)| line.cells())
        .max()
        .unwrap_or(0);
    let widest = if widest == 0 {
        cells
    } else {
        widest.min(cells)
    };

    for index in rules {
        let Some(line) = lines.get_mut(*index) else {
            debug_assert!(false, "every recorded index names one row of this list");
            continue;
        };
        let width = widest.saturating_sub(line.cells());
        line.spans.push(FloatSpan::new(
            RULE_GLYPH.repeat(width),
            FloatStyle::Structure,
        ));
    }
}

/// Returns the cells that the body of one block holds.
///
/// A prefix that consumes the whole width still leaves one cell for text, so a
/// deeply nested block still reaches the screen and the float clips the row.
fn body_width(prefix: &Prefix, cells: usize) -> usize {
    cells.saturating_sub(prefix.cells).max(1)
}

/// Returns the cells that every list container of one document occupies.
///
/// The widest marker of the document decides the field, so a block that
/// continues an item stands under the text of that item although it names no
/// marker of its own. [`FLOAT_LIST_FIELD_CELLS_MAX`] bounds the field.
fn list_field(document: &MarkupDocument) -> usize {
    let widest = document
        .blocks()
        .iter()
        .flat_map(MarkupBlock::containers)
        .filter_map(|container| match container {
            MarkupContainer::List {
                marker: Some(marker),
            } => Some(text_cells(&marker_glyph(*marker))),
            MarkupContainer::List { marker: None } | MarkupContainer::Quote => None,
        })
        .max()
        .unwrap_or(0);

    widest
        .saturating_add(1)
        .clamp(FLOAT_LIST_FIELD_CELLS_MIN, FLOAT_LIST_FIELD_CELLS_MAX)
}

/// Returns the glyph of one list marker.
fn marker_glyph(marker: MarkupMarker) -> String {
    match marker {
        MarkupMarker::Bullet => BULLET_GLYPH.to_owned(),
        MarkupMarker::Ordered(number) => format!("{number}."),
    }
}

/// Returns the text of one list marker inside the field of one document.
///
/// The marker stands at the right of the field, and one blank separates it from
/// the text of the item. A block that continues an item names no marker and
/// takes blanks of the same width, so every row of one item keeps one left
/// edge. A marker that is wider than the field loses its end for the same
/// reason.
fn marker_text(marker: Option<MarkupMarker>, field: usize) -> String {
    debug_assert!(
        field >= FLOAT_LIST_FIELD_CELLS_MIN,
        "the field holds one marker and one blank"
    );
    let Some(marker) = marker else {
        return " ".repeat(field);
    };

    let glyph = marker_glyph(marker);
    let glyph = clip_cells(&glyph, field - 1);
    let pad = field - 1 - text_cells(glyph);
    format!("{}{glyph} ", " ".repeat(pad))
}

/// The cells that stand left of the body of one block.
///
/// A quote rails every row of its block, and a list item marks its first row
/// alone. Both prefixes occupy one width, so every row of one block keeps one
/// left edge.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Prefix {
    /// The pieces of the first row of the block.
    first: Vec<FloatSpan>,
    /// The pieces of every later row of the block.
    rest: Vec<FloatSpan>,
    /// The cells that both occupy.
    cells: usize,
}

impl Prefix {
    /// Returns the prefix of the containers around one block.
    fn of(containers: &[MarkupContainer], field: usize) -> Self {
        let mut first = Vec::new();
        let mut rest = Vec::new();

        for container in containers {
            match container {
                MarkupContainer::Quote => {
                    let rail = FloatSpan::new(QUOTE_RAIL, FloatStyle::Markup(MarkupRole::Quote));
                    first.push(rail.clone());
                    rest.push(rail);
                }
                MarkupContainer::List { marker } => {
                    first.push(FloatSpan::new(
                        marker_text(*marker, field),
                        FloatStyle::Structure,
                    ));
                    rest.push(FloatSpan::new(" ".repeat(field), FloatStyle::Plain));
                }
            }
        }

        let cells = span_cells(&first);
        debug_assert_eq!(
            cells,
            span_cells(&rest),
            "a marker and the blanks that replace it occupy one width"
        );
        Self { first, rest, cells }
    }

    /// Returns the prefix that indents this one by `cells` further cells.
    fn indented(&self, cells: usize) -> Self {
        let mut indented = self.clone();
        if cells > 0 {
            let blanks = FloatSpan::new(" ".repeat(cells), FloatStyle::Plain);
            indented.first.push(blanks.clone());
            indented.rest.push(blanks);
            indented.cells += cells;
        }
        indented
    }

    /// Returns the row that starts with this prefix, at one row of the block.
    fn row(&self, index: usize) -> FloatLine {
        FloatLine {
            spans: if index == 0 {
                self.first.clone()
            } else {
                self.rest.clone()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use kvim_core::TextBuffer;
    use kvim_language::{
        AnalysisInput, HighlightSpan, LanguageRegistry, MarkupDocument, MarkupRole, SyntaxRole,
    };
    use kvim_settings::FileSettings;

    use super::{FloatLine, FloatStyle, markup_lines};

    /// The style of one part of a code line that no highlight span names.
    const CODE: FloatStyle = FloatStyle::Markup(MarkupRole::InlineCode);

    /// The answer that rust-analyzer sends for one function of kvim.
    const RUST_ANALYZER_HOVER: &str = "\n```rust\nkvim_language::session\n```\n\n```rust\nfn \
                                       hover_markup(result: &RawValue) -> \
                                       Result<Option<MarkupText>, LspError>\n```\n\n---\n\nReturns \
                                       the bounded text of one hover answer.";

    /// Returns the rows of one text at one width.
    ///
    /// The language-server task names the code of each fence before the answer
    /// reaches this crate, so the helper takes the document that the float
    /// paints and never the parse alone.
    fn rows(source: &str, cells: usize) -> Vec<FloatLine> {
        let document = MarkupDocument::parse(source).highlighted(LanguageRegistry::first_release());
        markup_lines(&document, cells)
    }

    /// Returns the pieces of one row, as text and style.
    fn pieces(row: &FloatLine) -> Vec<(String, FloatStyle)> {
        row.spans
            .iter()
            .map(|span| (span.text.clone(), span.style))
            .collect()
    }

    /// Returns the spans that one text carries as the buffer of a Rust file.
    ///
    /// The path selects the adapter, so the helper takes the selection that an
    /// open file takes and never the one that a fence takes.
    fn buffer_spans(source: &str) -> Vec<HighlightSpan> {
        let version = TextBuffer::from_text("", &FileSettings::default())
            .expect("the empty text is small")
            .version();
        LanguageRegistry::first_release()
            .adapter(Path::new("hover.rs"))
            .expect("the Rust adapter owns a .rs path")
            .analyze(
                &AnalysisInput::new(version, Arc::from(source)),
                &CancellationToken::new(),
            )
            .expect("the test source stays inside every bound")
            .highlights()
            .to_vec()
    }

    /// Returns the pieces that one line of a Rust buffer paints.
    ///
    /// The helper states the painting rule of `docs/language-services.md`: the
    /// range of each span takes the syntax role of that span, and every other
    /// range takes the code span role.
    fn buffer_pieces(source: &str) -> Vec<(String, FloatStyle)> {
        let mut pieces = Vec::new();
        let mut painted = 0;

        for span in buffer_spans(source)
            .iter()
            .filter(|span| span.line == 0 && span.start_byte < span.end_byte)
        {
            let start = span.start_byte as usize;
            let end = span.end_byte as usize;
            if painted < start {
                pieces.push((source[painted..start].to_owned(), CODE));
            }
            pieces.push((source[start..end].to_owned(), FloatStyle::Syntax(span.role)));
            painted = end;
        }
        if painted < source.len() {
            pieces.push((source[painted..].to_owned(), CODE));
        }

        pieces
    }

    /// Returns the text of every row of one text at one width.
    fn texts(source: &str, cells: usize) -> Vec<String> {
        rows(source, cells)
            .iter()
            .map(FloatLine::text)
            .collect::<Vec<_>>()
    }

    #[test]
    fn a_rust_analyzer_hover_shows_no_marker_of_its_source() {
        let painted = texts(RUST_ANALYZER_HOVER, 80);

        assert_eq!(
            painted,
            vec![
                "kvim_language::session".to_owned(),
                String::new(),
                "fn hover_markup(result: &RawValue) -> Result<Option<MarkupText>, LspError>"
                    .to_owned(),
                String::new(),
                "─".repeat(74),
                String::new(),
                "Returns the bounded text of one hover answer.".to_owned(),
            ],
            "no fence, no backtick, and no dash of the source reaches one row",
        );
    }

    #[test]
    fn a_code_block_and_a_thematic_break_carry_their_own_styles() {
        let painted = rows(RUST_ANALYZER_HOVER, 80);

        assert_eq!(
            pieces(&painted[0]),
            buffer_pieces("kvim_language::session"),
            "a code block paints the roles that its code carries in a buffer",
        );
        assert_eq!(
            painted[4].spans[0].style,
            FloatStyle::Structure,
            "the float draws the thematic break itself",
        );
    }

    #[test]
    fn a_rust_fence_paints_each_token_in_the_role_of_the_same_buffer() {
        let source = "fn hover(&self) -> Vec<&MarkupText>";
        let painted = rows(&format!("```rust\n{source}\n```"), 80);

        assert_eq!(painted.len(), 1, "one source line paints one row");
        assert_eq!(
            pieces(&painted[0]),
            buffer_pieces(source),
            "one text takes one set of roles in a hover answer and in a buffer",
        );
        assert_eq!(
            pieces(&painted[0])[0],
            ("fn".to_owned(), FloatStyle::Syntax(SyntaxRole::Keyword)),
            "the reader sees the keyword of the signature in the keyword role",
        );
        assert!(
            pieces(&painted[0])
                .iter()
                .filter(|(_, style)| matches!(style, FloatStyle::Syntax(_)))
                .count()
                >= 3,
            "the signature paints several roles and not one flat color: {:?}",
            pieces(&painted[0]),
        );
    }

    #[test]
    fn a_fence_of_an_unknown_language_paints_one_flat_color() {
        // A server may write any info string, and no adapter answers to these
        // names, so the fence reads as plain code.
        for info in ["", "console", "mermaid"] {
            let painted = rows(&format!("```{info}\nfn main() {{}}\n```"), 80);

            assert_eq!(
                pieces(&painted[0]),
                vec![("fn main() {}".to_owned(), CODE)],
                "the fence of {info:?} paints one piece in the code role",
            );
        }
    }

    #[test]
    fn each_inline_marker_carries_its_role_into_one_row() {
        let painted = rows("plain *soft* **hard** `code` [manual](https://kvim)", 80);

        assert_eq!(
            painted[0]
                .spans
                .iter()
                .map(|span| (span.text.as_str(), span.style))
                .collect::<Vec<_>>(),
            vec![
                ("plain ", FloatStyle::Markup(MarkupRole::Text)),
                ("soft", FloatStyle::Markup(MarkupRole::Emphasis)),
                (" ", FloatStyle::Markup(MarkupRole::Text)),
                ("hard", FloatStyle::Markup(MarkupRole::Strong)),
                (" ", FloatStyle::Markup(MarkupRole::Text)),
                ("code", FloatStyle::Markup(MarkupRole::InlineCode)),
                (" ", FloatStyle::Markup(MarkupRole::Text)),
                ("manual", FloatStyle::Markup(MarkupRole::Link)),
            ],
        );
    }

    #[test]
    fn a_wrapped_list_item_keeps_its_left_edge() {
        let painted = texts("- one item that wraps at this width\n- second", 14);

        assert_eq!(
            painted,
            vec![
                "• one item".to_owned(),
                "  that wraps".to_owned(),
                "  at this".to_owned(),
                "  width".to_owned(),
                "• second".to_owned(),
            ],
            "every row after the marker stands under the text of its item",
        );
    }

    #[test]
    fn every_list_container_of_one_document_holds_one_field() {
        // The widest marker decides the field, so the block that continues an
        // item stands under the text of that item.
        let painted = texts("10. ten\n11. eleven  \n    more", 20);

        assert_eq!(
            painted,
            vec![
                "10. ten".to_owned(),
                "11. eleven".to_owned(),
                "    more".to_owned(),
            ],
        );
    }

    #[test]
    fn a_quote_rails_every_row_that_it_holds() {
        let painted = texts("> a remark that wraps here", 12);

        assert_eq!(
            painted,
            vec![
                "│ a remark".to_owned(),
                "│ that".to_owned(),
                "│ wraps here".to_owned(),
            ],
        );
    }

    #[test]
    fn a_heading_indents_by_its_rank_and_shows_no_hash() {
        let painted = texts("# first\n\n### third", 20);

        assert_eq!(
            painted,
            vec!["first".to_owned(), String::new(), "  third".to_owned(),],
        );
    }

    #[test]
    fn no_row_overflows_the_width_and_no_wide_character_splits() {
        // Each character occupies two cells, so a wrap that counted characters
        // would paint rows of twice the width.
        let painted = rows("漢字漢字漢字 and a word", 7);

        for row in &painted {
            assert!(row.cells() <= 7, "{row:?} fits the width");
        }
        assert_eq!(
            painted.iter().map(FloatLine::text).collect::<Vec<_>>(),
            vec![
                "漢字漢".to_owned(),
                "字漢字".to_owned(),
                "and a".to_owned(),
                "word".to_owned(),
            ],
            "a wide character stands whole in one row, and only a blank of one \
             break disappears",
        );
    }

    #[test]
    fn a_code_line_that_is_wider_than_the_float_loses_its_end() {
        // A wrap would move the rest of the line under its own indentation, so
        // a reader could no longer read the line as source text.
        let painted = texts("```rust\nfn wide(argument: usize) -> usize\n```", 12);

        assert_eq!(painted, vec!["fn wide(argu".to_owned()]);
    }

    #[test]
    fn a_clipped_code_line_keeps_the_spans_of_what_it_paints() {
        let painted = rows("```rust\nfn wide(argument: usize) -> usize\n```", 12);
        let row = &painted[0];

        assert_eq!(row.text(), "fn wide(argu", "the row lost its end");
        assert_eq!(
            pieces(row),
            vec![
                ("fn".to_owned(), FloatStyle::Syntax(SyntaxRole::Keyword)),
                (" ".to_owned(), CODE),
                ("wide".to_owned(), FloatStyle::Syntax(SyntaxRole::Function)),
                ("(".to_owned(), FloatStyle::Syntax(SyntaxRole::Bracket)),
                ("argu".to_owned(), FloatStyle::Syntax(SyntaxRole::Parameter)),
            ],
            "the pieces of the row stay aligned to the text that it paints",
        );

        // The row that keeps its end holds the same pieces, and the piece that
        // the cut crosses keeps the part that the narrow row paints.
        let whole = pieces(&rows("```rust\nfn wide(argument: usize) -> usize\n```", 80)[0]);
        let clipped = pieces(row);
        assert_eq!(clipped[..4], whole[..4], "a piece before the cut survives");
        assert_eq!(
            clipped[4].1, whole[4].1,
            "the piece across the cut keeps its role"
        );
        assert!(
            whole[4].0.starts_with(&clipped[4].0),
            "and it keeps the start of its text: {:?}",
            whole[4].0
        );
    }

    #[test]
    fn a_clipped_code_line_splits_no_wide_character() {
        // Each character of the literal occupies two cells, so a clip that
        // counted bytes would cut one of them in half.
        let painted = rows("```rust\nlet name = \"漢字漢字\";\n```", 14);
        let row = &painted[0];

        assert_eq!(
            row.text(),
            "let name = \"漢",
            "the wide character stands whole"
        );
        assert!(row.cells() <= 14, "{row:?} fits the width");
        assert_eq!(
            pieces(row).last().expect("the row holds one piece").clone(),
            ("\"漢".to_owned(), FloatStyle::Syntax(SyntaxRole::String)),
            "the clipped literal keeps the role of a string",
        );
    }
}
