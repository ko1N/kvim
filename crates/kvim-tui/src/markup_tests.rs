use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use kvim_core::TextBuffer;
use kvim_language::{
    AnalysisInput, HighlightSpan, LanguageRegistry, MarkupDocument, MarkupRole, SyntaxHighlighter,
    SyntaxRole,
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
    let mut highlighter = SyntaxHighlighter::new();
    let document = MarkupDocument::parse(source)
        .highlighted(LanguageRegistry::first_release(), &mut highlighter);
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
            &mut SyntaxHighlighter::new(),
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
            "fn hover_markup(result: &RawValue) -> Result<Option<MarkupText>, LspError>".to_owned(),
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
