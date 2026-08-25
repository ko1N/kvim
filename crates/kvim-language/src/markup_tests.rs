use std::path::Path;
use std::sync::Arc;

use kvim_core::TextBuffer;
use kvim_settings::FileSettings;
use tokio_util::sync::CancellationToken;

use super::{
    MARKUP_BLOCKS_MAX, MARKUP_FENCE_SOURCE_BYTES_MAX, MARKUP_FENCE_SPANS_MAX, MARKUP_FENCES_MAX,
    MARKUP_NESTING_DEPTH_MAX, MARKUP_PIECES_MAX, MARKUP_SOURCE_BYTES_MAX, MarkupBlock, MarkupBody,
    MarkupContainer, MarkupDocument, MarkupMarker, MarkupRole, fence_language,
};
use crate::{AnalysisInput, HighlightSpan, LanguageRegistry, SyntaxHighlighter};

/// The answer that rust-analyzer sends for one function of kvim.
///
/// The shape is the common one: one fence that names the module path, one
/// fence that holds the signature, one thematic break, and the first line
/// of the document comment.
const RUST_ANALYZER_HOVER: &str = "\n```rust\nkvim_language::session\n```\n\n```rust\nfn \
                                       hover_markup(result: &RawValue) -> Result<Option<MarkupText>, \
                                       LspError>\n```\n\n---\n\nReturns the bounded text of one \
                                       hover answer and the markup that covers it.";

/// Returns the text of one block, without its containers.
fn content(block: &MarkupBlock) -> String {
    match block.body() {
        MarkupBody::Prose(styled) => styled.text().to_owned(),
        MarkupBody::Heading { text, .. } => text.text().to_owned(),
        MarkupBody::Code { lines, .. } => lines.join("\n"),
        MarkupBody::Rule => String::new(),
    }
}

/// Returns the pieces of one prose block, with their roles.
fn pieces(document: &MarkupDocument, index: usize) -> Vec<(String, MarkupRole)> {
    match document.blocks()[index].body() {
        MarkupBody::Prose(styled) | MarkupBody::Heading { text: styled, .. } => styled
            .pieces()
            .into_iter()
            .map(|(text, role)| (text.to_owned(), role))
            .collect(),
        MarkupBody::Code { .. } => panic!("block {index} is a code block"),
        MarkupBody::Rule => panic!("block {index} is a thematic break"),
    }
}

/// Returns the document of one source with the code of each fence named.
fn highlighted(source: &str) -> MarkupDocument {
    MarkupDocument::parse(source).highlighted(
        LanguageRegistry::first_release(),
        &mut SyntaxHighlighter::new(),
    )
}

/// Returns the spans of the code block at one index.
fn spans(document: &MarkupDocument, index: usize) -> &[HighlightSpan] {
    match document.blocks()[index].body() {
        MarkupBody::Code { highlights, .. } => highlights,
        other => panic!("block {index} is no code block: {other:?}"),
    }
}

/// Returns the spans that one text carries as the buffer of a Rust file.
///
/// The path selects the adapter, so this helper takes the selection that
/// an open file takes and never the one that a fence takes.
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

#[test]
fn a_rust_fence_carries_the_roles_of_the_same_buffer() {
    let path = "kvim_language::session";
    let signature = "fn hover_markup(result: &RawValue) -> Result<Option<MarkupText>, LspError>";
    let document = highlighted(RUST_ANALYZER_HOVER);

    assert!(
        !spans(&document, 1).is_empty(),
        "the signature of the answer carries roles"
    );
    assert_eq!(
        spans(&document, 1),
        buffer_spans(signature),
        "one text carries one set of roles in a fence and in a buffer"
    );
    assert_eq!(
        spans(&document, 0),
        buffer_spans(path),
        "the module path of the answer carries the roles of the same buffer text"
    );
}

#[test]
fn a_fence_of_many_lines_addresses_the_lines_of_its_own_source() {
    let source = "fn main() {\n    let value = 1;\n}";
    let document = highlighted(&format!("```rust\n{source}\n```"));

    assert_eq!(
        spans(&document, 0),
        buffer_spans(source),
        "a span of a fence addresses the line and the byte as a span of a buffer does"
    );
    assert_eq!(
        spans(&document, 0)
            .iter()
            .map(|span| span.line)
            .max()
            .expect("the fence carries roles"),
        2,
        "the last line of the fence is the third one"
    );
}

#[test]
fn a_compound_info_string_selects_the_language_of_its_first_word() {
    // A writer may add an attribute after the name, and rust-analyzer
    // sends `rust` alone. The match folds ASCII case.
    for info in ["rust", "rust,ignore", "rust title=\"one\"", "Rust", "RS"] {
        let document = highlighted(&format!("```{info}\nfn main() {{}}\n```"));

        assert_eq!(
            spans(&document, 0),
            buffer_spans("fn main() {}"),
            "the info string {info:?} names Rust"
        );
    }

    assert_eq!(fence_language("rust,ignore"), "rust");
    assert_eq!(fence_language("rust title=\"one\""), "rust");
    assert_eq!(fence_language(""), "");
}

#[test]
fn a_fence_of_an_unknown_language_carries_no_role() {
    for info in ["console", "mermaid", "haskell"] {
        let document = highlighted(&format!("```{info}\nfn main() {{}}\n```"));

        assert!(
            spans(&document, 0).is_empty(),
            "no adapter answers to {info:?}, and that is no failure"
        );
        assert_eq!(
            content(&document.blocks()[0]),
            "fn main() {}",
            "the fence keeps every line of {info:?}"
        );
    }
}

#[test]
fn a_hostile_info_string_selects_no_language() {
    // The info string is server text. The reader passes one complete name,
    // and every comparison rejects a length that no declared name holds.
    let info = "r".repeat(16 * 1024);
    let document = highlighted(&format!("```{info}\nfn main() {{}}\n```"));

    assert!(spans(&document, 0).is_empty());
}

#[test]
fn a_fence_that_names_no_language_carries_no_role() {
    // The first source is a fence without an info string, and the second
    // one is an indented code block, which names no language at all.
    for source in ["```\nfn main() {}\n```", "    fn main() {}"] {
        let document = highlighted(source);

        assert!(
            spans(&document, 0).is_empty(),
            "{source:?} names no language"
        );
        assert_eq!(content(&document.blocks()[0]), "fn main() {}");
    }
}

#[test]
fn a_fence_above_the_source_bound_carries_no_role() {
    let line = "fn main() {}\n";
    let lines = MARKUP_FENCE_SOURCE_BYTES_MAX / line.len();
    let inside = line.repeat(lines);
    let outside = line.repeat(lines + 1);
    assert!(inside.len() <= MARKUP_FENCE_SOURCE_BYTES_MAX);
    assert!(outside.len() > MARKUP_FENCE_SOURCE_BYTES_MAX);

    let document = highlighted(&format!("```rust\n{inside}```"));
    assert!(
        !spans(&document, 0).is_empty(),
        "a fence at the bound still carries roles"
    );

    let document = highlighted(&format!("```rust\n{outside}```"));
    assert!(
        spans(&document, 0).is_empty(),
        "a fence above the bound costs no highlight"
    );
    assert_eq!(
        document.blocks()[0].body(),
        &MarkupBody::Code {
            info: "rust".to_owned(),
            lines: outside.lines().map(str::to_owned).collect(),
            highlights: Vec::new(),
        },
        "the fence above the bound keeps every line"
    );
}

#[test]
fn a_fence_above_the_span_bound_carries_no_role() {
    // Each keyword, each name, and each bracket is one span, so this
    // source produces about one span for each 1.5 bytes and reaches the
    // span bound below the source bound.
    let line = "fn a(){}\n";
    let source = line.repeat(MARKUP_FENCE_SOURCE_BYTES_MAX / line.len());
    assert!(source.len() <= MARKUP_FENCE_SOURCE_BYTES_MAX);
    assert!(
        buffer_spans(&source).len() > MARKUP_FENCE_SPANS_MAX,
        "the same buffer text passes the fence span bound"
    );

    let document = highlighted(&format!("```rust\n{source}```"));

    assert!(
        spans(&document, 0).is_empty(),
        "kvim publishes no partial result, so the fence carries no span at all"
    );
}

#[test]
fn the_fence_bound_holds_over_a_document_of_many_fences() {
    let document = highlighted(&"```rust\nfn main() {}\n```\n\n".repeat(MARKUP_FENCES_MAX * 2));

    let named = document
            .blocks()
            .iter()
            .filter(|block| !matches!(block.body(), MarkupBody::Code { highlights, .. } if highlights.is_empty()))
            .count();
    assert_eq!(named, MARKUP_FENCES_MAX, "the pass reads no further fence");
    assert!(
        document
            .blocks()
            .iter()
            .all(|block| content(block) == "fn main() {}"),
        "a fence above the bound keeps every line"
    );
}

#[test]
fn the_parse_alone_names_no_role() {
    // The terminal event loop may run the parse, and it must run no
    // Tree-sitter work, so only the highlight pass produces one span.
    let document = MarkupDocument::parse(RUST_ANALYZER_HOVER);

    let mut fences = 0;
    for (index, block) in document.blocks().iter().enumerate() {
        let MarkupBody::Code { highlights, .. } = block.body() else {
            continue;
        };
        fences += 1;
        assert!(
            highlights.is_empty(),
            "block {index} carries a span that no highlight pass produced"
        );
    }
    assert_eq!(
        fences, 2,
        "the answer holds the two fences that this test reads"
    );
}

#[test]
fn a_rust_analyzer_hover_parses_into_its_blocks() {
    let document = MarkupDocument::parse(RUST_ANALYZER_HOVER);
    let blocks = document.blocks();

    assert_eq!(blocks.len(), 4, "{blocks:?}");
    assert!(!document.is_clipped());

    let MarkupBody::Code { info, lines, .. } = blocks[0].body() else {
        panic!("the module path stands in a fence: {:?}", blocks[0]);
    };
    assert_eq!(info, "rust", "the fence keeps its info string");
    assert_eq!(lines, &["kvim_language::session".to_owned()]);

    let MarkupBody::Code { info, lines, .. } = blocks[1].body() else {
        panic!("the signature stands in a fence: {:?}", blocks[1]);
    };
    assert_eq!(info, "rust", "the second fence keeps its info string");
    assert_eq!(
        lines,
        &["fn hover_markup(result: &RawValue) -> Result<Option<MarkupText>, LspError>".to_owned()]
    );

    assert_eq!(
        blocks[2].body(),
        &MarkupBody::Rule,
        "the thematic break is one block of its own"
    );

    assert_eq!(
        pieces(&document, 3),
        vec![(
            "Returns the bounded text of one hover answer and the markup that covers it."
                .to_owned(),
            MarkupRole::Text,
        )],
        "the prose keeps every character of the document comment"
    );

    // Every block but the first opens with one blank row, because the
    // source separated them with a blank line.
    assert_eq!(
        blocks
            .iter()
            .map(MarkupBlock::is_spaced)
            .collect::<Vec<_>>(),
        vec![false, true, true, true]
    );
}

#[test]
fn two_answers_join_into_one_document_and_one_blank_row() {
    // Two servers of one language answer on their own, and each answer
    // carries its own document, so the editor joins documents.
    let first = highlighted("```rust\nfn first() {}\n```");
    let second = highlighted("The second server *answers* as well.");
    let joined = first.clone().joined(&second);

    assert_eq!(
        joined
            .blocks()
            .iter()
            .map(|block| (content(block), block.is_spaced()))
            .collect::<Vec<_>>(),
        vec![
            ("fn first() {}".to_owned(), false),
            (
                "The second server answers as well.".to_owned(),
                // One blank row stands above the first block of the second
                // answer, and the first block of the join opens none.
                true,
            ),
        ]
    );
    assert_eq!(
        spans(&joined, 0),
        spans(&first, 0),
        "the join moves the spans of each fence unchanged"
    );
    assert!(!joined.is_clipped());
}

#[test]
fn the_first_answer_of_one_join_opens_no_blank_row() {
    // The join starts at the empty document, so the first answer keeps the
    // spacing that its own parse gave it.
    let answer = MarkupDocument::parse("one\n\ntwo");
    let joined = MarkupDocument::default().joined(&answer);

    assert_eq!(joined, answer, "the empty document adds nothing");
}

#[test]
fn one_clipped_answer_reports_the_join_as_clipped() {
    let clipped = MarkupDocument::parse(&"word ".repeat(MARKUP_SOURCE_BYTES_MAX));
    assert!(clipped.is_clipped());
    let complete = MarkupDocument::parse("a short answer");
    assert!(!complete.is_clipped());

    assert!(complete.clone().joined(&clipped).is_clipped());
    assert!(clipped.joined(&complete).is_clipped());
}

#[test]
fn the_block_bound_holds_over_one_join() {
    // Neither answer reaches the bound alone, and the two reach it
    // together, so the join is the step that stops.
    let answer = MarkupDocument::parse(&"a\n\n".repeat(MARKUP_BLOCKS_MAX * 3 / 4));
    assert!(!answer.is_clipped(), "one answer stays under the bound");

    let joined = answer.clone().joined(&answer);

    assert_eq!(
        joined.blocks().len(),
        MARKUP_BLOCKS_MAX,
        "the join appends no block above the bound"
    );
    assert!(joined.is_clipped(), "the join stopped at the block bound");
}

#[test]
fn the_piece_bound_holds_over_one_join() {
    // One line of a code block is one piece, and the lines of that block
    // join the count when the block closes, so this answer holds the
    // complete bound in one block.
    let answer = MarkupDocument::parse(&format!("```\n{}```", "line\n".repeat(MARKUP_PIECES_MAX)));
    assert!(!answer.is_clipped(), "the parse reached no bound");
    assert_eq!(
        answer
            .blocks()
            .iter()
            .map(MarkupBlock::pieces)
            .sum::<usize>(),
        MARKUP_PIECES_MAX
    );

    let joined = answer.joined(&MarkupDocument::parse("a second answer"));

    assert_eq!(
        joined.blocks().len(),
        1,
        "the join appends no block after the count reached the bound"
    );
    assert!(joined.is_clipped(), "the join stopped at the piece bound");
}

#[test]
fn an_empty_source_holds_no_block() {
    for source in ["", "   ", "\n\n"] {
        let document = MarkupDocument::parse(source);

        assert!(document.is_empty(), "{source:?} holds no block");
        assert!(!document.is_clipped(), "{source:?} reached no bound");
    }
}

#[test]
fn a_text_without_markup_stays_one_paragraph() {
    let message = "expected type usize, found type u32";
    let document = MarkupDocument::parse(message);

    assert_eq!(document.blocks().len(), 1);
    assert_eq!(
        pieces(&document, 0),
        vec![(message.to_owned(), MarkupRole::Text)],
        "a text that holds no markup keeps every character in one role"
    );
}

#[test]
fn a_source_above_the_bound_reports_the_bound() {
    let source = "word ".repeat(MARKUP_SOURCE_BYTES_MAX);
    let document = MarkupDocument::parse(&source);

    assert!(document.is_clipped(), "the source stands above the bound");
    assert!(document.blocks().len() <= MARKUP_BLOCKS_MAX + 1);

    let kept: usize = document
        .blocks()
        .iter()
        .map(|block| content(block).len())
        .sum();
    assert!(
        kept <= MARKUP_SOURCE_BYTES_MAX,
        "the value never grows past the source bound: {kept}"
    );
}

#[test]
fn a_source_above_the_bound_splits_no_character() {
    // Each character occupies four bytes, so the bound falls inside one of
    // them and the parse must step back to its start.
    let source = "𝄞".repeat(MARKUP_SOURCE_BYTES_MAX);
    let document = MarkupDocument::parse(&source);

    assert!(document.is_clipped());
    assert!(
        content(&document.blocks()[0])
            .chars()
            .all(|value| value == '𝄞'),
        "the parse cut the source at a character boundary"
    );
}

#[test]
fn the_block_bound_holds_over_a_source_of_many_blocks() {
    let source = "a\n\n".repeat(MARKUP_BLOCKS_MAX * 2);
    let document = MarkupDocument::parse(&source);

    assert!(document.blocks().len() <= MARKUP_BLOCKS_MAX + 1);
    assert!(document.is_clipped(), "the parse stopped at the bound");
}

#[test]
fn the_piece_bound_holds_over_one_text_of_many_pieces() {
    // Each emphasis and each blank between two of them is one piece. The
    // source stays far below the source bound, so the piece bound is the
    // one that stops this parse.
    let source = "*a* ".repeat(MARKUP_PIECES_MAX);
    assert!(source.len() < MARKUP_SOURCE_BYTES_MAX);

    let document = MarkupDocument::parse(&source);

    assert!(
        document.is_clipped(),
        "the parse stopped at the piece bound"
    );
    let pieces: usize = document
        .blocks()
        .iter()
        .map(|block| match block.body() {
            MarkupBody::Prose(styled) | MarkupBody::Heading { text: styled, .. } => {
                styled.pieces().len()
            }
            MarkupBody::Code { lines, .. } => lines.len(),
            MarkupBody::Rule => 1,
        })
        .sum();
    assert!(
        pieces <= MARKUP_PIECES_MAX,
        "the value holds no more pieces than the bound: {pieces}"
    );
}

#[test]
fn each_inline_marker_takes_its_own_role() {
    let document =
        MarkupDocument::parse("plain *soft* **hard** `code` and [the manual](https://kvim)");

    assert_eq!(
        pieces(&document, 0),
        vec![
            ("plain ".to_owned(), MarkupRole::Text),
            ("soft".to_owned(), MarkupRole::Emphasis),
            (" ".to_owned(), MarkupRole::Text),
            ("hard".to_owned(), MarkupRole::Strong),
            (" ".to_owned(), MarkupRole::Text),
            ("code".to_owned(), MarkupRole::InlineCode),
            (" and ".to_owned(), MarkupRole::Text),
            ("the manual".to_owned(), MarkupRole::Link),
        ],
        "no marker of the source reaches the text, and the destination never paints"
    );
}

#[test]
fn a_heading_keeps_its_rank_and_drops_its_marker() {
    let document = MarkupDocument::parse("### The report");

    let MarkupBody::Heading { level, text } = document.blocks()[0].body() else {
        panic!("the hashes open a heading");
    };
    assert_eq!(*level, 3);
    assert_eq!(text.text(), "The report");
    assert_eq!(text.pieces()[0].1, MarkupRole::Heading);
}

#[test]
fn a_list_names_the_marker_of_each_item_once() {
    let document = MarkupDocument::parse("1. one\n2. two\n   - inner\n3. three");
    let blocks = document.blocks();

    assert_eq!(
        blocks
            .iter()
            .map(|block| (block.containers().to_owned(), content(block)))
            .collect::<Vec<_>>(),
        vec![
            (
                vec![MarkupContainer::List {
                    marker: Some(MarkupMarker::Ordered(1))
                }],
                "one".to_owned()
            ),
            (
                vec![MarkupContainer::List {
                    marker: Some(MarkupMarker::Ordered(2))
                }],
                "two".to_owned()
            ),
            (
                vec![
                    MarkupContainer::List { marker: None },
                    MarkupContainer::List {
                        marker: Some(MarkupMarker::Bullet)
                    },
                ],
                "inner".to_owned()
            ),
            (
                vec![MarkupContainer::List {
                    marker: Some(MarkupMarker::Ordered(3))
                }],
                "three".to_owned()
            ),
        ],
        "the nested item indents under the item that holds it"
    );

    // The items of one list follow one another without a blank row.
    assert!(blocks.iter().all(|block| !block.is_spaced()));
}

#[test]
fn a_hard_break_continues_one_item_without_its_marker() {
    // Two blanks at the end of a line are one hard break of CommonMark.
    let document = MarkupDocument::parse("- first line  \n  second line");
    let blocks = document.blocks();

    assert_eq!(blocks.len(), 2, "{blocks:?}");
    assert_eq!(
        blocks[0].containers(),
        &[MarkupContainer::List {
            marker: Some(MarkupMarker::Bullet)
        }]
    );
    assert_eq!(content(&blocks[0]), "first line");
    assert_eq!(
        blocks[1].containers(),
        &[MarkupContainer::List { marker: None }],
        "one marker stands on one block only"
    );
    assert_eq!(content(&blocks[1]), "second line");
    assert!(!blocks[1].is_spaced(), "a hard break continues one item");
}

#[test]
fn a_block_quote_rails_its_block_and_names_its_text() {
    let document = MarkupDocument::parse("> a remark");

    assert_eq!(
        document.blocks()[0].containers(),
        &[MarkupContainer::Quote],
        "the quote rails every row of the block"
    );
    assert_eq!(
        pieces(&document, 0),
        vec![("a remark".to_owned(), MarkupRole::Quote)]
    );
}

#[test]
fn the_container_bound_holds_over_a_deep_nesting() {
    let source = format!("{}deep", "> ".repeat(MARKUP_NESTING_DEPTH_MAX * 2));
    let document = MarkupDocument::parse(&source);

    assert_eq!(
        document.blocks()[0].containers().len(),
        MARKUP_NESTING_DEPTH_MAX,
        "a container below the bound adds no prefix"
    );
    assert_eq!(content(&document.blocks()[0]), "deep");
}

#[test]
fn an_open_fence_already_reads_as_a_code_block() {
    // A server writes a fence and no closing fence when its answer ends
    // with one. CommonMark closes the fence at the end of the text.
    for source in ["```rust", "```rust\n", "```rust\nfn main() {"] {
        let document = MarkupDocument::parse(source);

        assert!(
            matches!(document.blocks()[0].body(), MarkupBody::Code { .. }),
            "{source:?} must already be a code block"
        );
    }
}

#[test]
fn a_table_arrives_as_the_text_that_the_server_wrote() {
    // The parse enables no extension, so no cell of the table disappears.
    let document = MarkupDocument::parse("| Tool | Use |\n| --- | --- |\n| read | a file |");

    let text: String = document
        .blocks()
        .iter()
        .map(content)
        .collect::<Vec<_>>()
        .join(" ");
    for cell in ["Tool", "Use", "read", "a file"] {
        assert!(text.contains(cell), "the text keeps {cell:?}: {text:?}");
    }
}
