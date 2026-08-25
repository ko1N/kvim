//! The grapheme cluster rule of every cursor column.
//!
//! `core` validates character boundaries alone, because a cluster boundary needs
//! a segmentation table that `core` does not hold. This module holds that table
//! for the editor charter, so every cursor column stands on a cluster boundary
//! of its line. A combining mark therefore never parts from its letter, and one
//! step and one delete both take the whole cluster. See `docs/text-model.md`.
//!
//! Every function reads one line. The file settings bound one buffer, so they
//! bound one line, and an ASCII line needs no segmentation at all.

use kvim_core::{LineIndex, TextBuffer};
use unicode_segmentation::UnicodeSegmentation;

/// Returns the largest cluster boundary at or before `column`.
///
/// A column that already stands on a boundary stays unchanged. A column inside a
/// cluster moves back to the start of that cluster, because the cluster is one
/// unit for the reader.
pub(super) fn snapped_column(buffer: &TextBuffer, line: LineIndex, column: usize) -> usize {
    if column == 0 || buffer.line_is_ascii(line) {
        return column;
    }
    let text = buffer.line_text(line);
    boundary_at(&text, cluster_index(&text, column))
}

/// Returns the column `count` clusters before `column`.
///
/// The walk stops at the first column of the line.
pub(super) fn column_left(
    buffer: &TextBuffer,
    line: LineIndex,
    column: usize,
    count: usize,
) -> usize {
    if buffer.line_is_ascii(line) {
        return column.saturating_sub(count);
    }
    let text = buffer.line_text(line);
    let index = cluster_index(&text, column);
    boundary_at(&text, index.saturating_sub(count))
}

/// Returns the column `count` clusters after `column`.
///
/// The result can pass the end of the line, exactly as a character step can.
/// The caller clamps it against the line and the mode, so Normal mode still
/// stops on the last cluster.
pub(super) fn column_right(
    buffer: &TextBuffer,
    line: LineIndex,
    column: usize,
    count: usize,
) -> usize {
    if buffer.line_is_ascii(line) {
        return column.saturating_add(count);
    }
    let text = buffer.line_text(line);
    let index = cluster_index(&text, column);
    boundary_at(&text, index.saturating_add(count))
}

/// Returns the number of complete clusters before `column`.
///
/// A column inside a cluster reports the clusters before that cluster, so the
/// index names the cluster that holds the column.
fn cluster_index(text: &str, column: usize) -> usize {
    let mut chars = 0;
    for (index, cluster) in text.graphemes(true).enumerate() {
        let next = chars + cluster.chars().count();
        if column < next {
            return index;
        }
        chars = next;
    }
    text.graphemes(true).count()
}

/// Returns the column where the cluster of `index` starts.
///
/// An index past the last cluster reports the end of the line, which is the
/// boundary that Insert mode stands on after the last character.
fn boundary_at(text: &str, index: usize) -> usize {
    let mut chars = 0;
    for (position, cluster) in text.graphemes(true).enumerate() {
        if position == index {
            return chars;
        }
        chars += cluster.chars().count();
    }
    chars
}

#[cfg(test)]
#[path = "grapheme_tests.rs"]
mod tests;
