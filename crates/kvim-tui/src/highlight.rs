//! The one rule that splits one line of text into its highlight pieces.
//!
//! `kvim-language` answers [`HighlightSpan`] values that address the bytes of
//! one line. Every place that paints those spans over text needs the same
//! partition: a range that one span names takes the role of that span, and
//! every other range takes no role. The language float and the picker preview
//! both paint spans over text that is not a buffer, so both read this rule.
//!
//! The buffer view paints the same spans over its own cell grid, because a
//! buffer row expands tabs and clips wide characters. See `docs/windows.md`.

use kvim_language::{HighlightSpan, SyntaxRole};

/// Returns the pieces of one line of text, in order.
///
/// `line` names the row inside the analyzed text, and `highlights` holds the
/// spans of that complete text in ascending line and byte order. The pieces
/// partition `text`: a range that one span names carries the role of that
/// span, and every other range carries none. A line without a span therefore
/// answers one piece without a role, which paints as plain text.
///
/// The caller passes the text that the row paints, which a clip may already
/// have shortened. A span behind that cut adds no piece, and a span across it
/// ends at the cut, so the pieces stay aligned to what the row shows. A clip
/// counts terminal cells and never splits a character, so every kept range
/// still addresses a character boundary.
pub(super) fn role_pieces<'a>(
    text: &'a str,
    highlights: &[HighlightSpan],
    line: usize,
) -> Vec<(&'a str, Option<SyntaxRole>)> {
    let Ok(line) = u32::try_from(line) else {
        debug_assert!(false, "one analyzed text holds fewer lines than u32 counts");
        return vec![(text, None)];
    };

    let first = highlights.partition_point(|span| span.line < line);
    let mut pieces: Vec<(&str, Option<SyntaxRole>)> = Vec::new();
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
            pieces.push((&text[painted..start], None));
        }
        pieces.push((&text[start..end], Some(span.role)));
        painted = end;
    }
    if painted < text.len() {
        pieces.push((&text[painted..], None));
    }

    debug_assert_eq!(
        pieces.iter().map(|(piece, _)| piece.len()).sum::<usize>(),
        text.len(),
        "the pieces of one row partition the text of that row"
    );
    pieces
}
