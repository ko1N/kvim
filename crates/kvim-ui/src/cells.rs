//! Terminal-cell measurement for one rendered text.
//!
//! Every band, strip, and overlay of this crate measures cells, never bytes and
//! never characters. The module holds that one measurement, so no two callers
//! can disagree about the width of one text. See `docs/text-model.md`.

use unicode_width::UnicodeWidthChar;

/// Returns the number of terminal cells that one text occupies.
///
/// The measurement never counts bytes and never counts characters: a wide
/// character occupies two cells, a combining mark occupies none, and a control
/// character occupies one blank cell, because writing it would move the
/// terminal cursor.
///
/// Every measured text is one text that a bounded constructor accepted, or
/// padding that the caller derived from such a text, so the scan is finite.
pub(crate) fn text_cells(text: &str) -> usize {
    text.chars().map(|value| value.width().unwrap_or(1)).sum()
}
