//! Holds one prompt line of a host and applies every edit that a key names.
//!
//! Run it with `cargo run -p kvim-input --example edited_line`.
//!
//! The example holds no terminal and no editor. It shows the whole workflow
//! that a host performs: open one line over its own seed, apply the edit that
//! the resolver reports, read the text and the cursor for its own drawing, and
//! answer the edits that the line reports back.

use kvim_input::{EditedLine, LineChange, PromptEdit};

/// The largest number of characters that this host accepts on its line.
///
/// The bound belongs to the host, because only the host knows what its own
/// command names. The line refuses an insert above it rather than cutting the
/// text.
const CHARS_MAX: usize = 24;

/// The keys of this run, in the order that a reader presses them.
const EDITS: &[PromptEdit] = &[
    PromptEdit::Insert('r'),
    PromptEdit::Insert('e'),
    PromptEdit::Insert('n'),
    PromptEdit::Insert('a'),
    PromptEdit::Insert('m'),
    PromptEdit::Insert('e'),
    PromptEdit::Insert(' '),
    PromptEdit::CursorWordBackward,
    PromptEdit::CursorLineStart,
    PromptEdit::Insert(':'),
    PromptEdit::CursorLineEnd,
    PromptEdit::Insert('o'),
    PromptEdit::Insert('l'),
    PromptEdit::Insert('d'),
    PromptEdit::DeleteBackward,
    PromptEdit::DeleteWordBackward,
    PromptEdit::Accept,
];

fn main() {
    // The host seeds the line and places the cursor itself. `EditedLine::opened`
    // places it after the whole seed instead.
    let mut line =
        EditedLine::opened_at(String::new(), 0, CHARS_MAX).expect("the empty seed meets the limit");

    for &edit in EDITS {
        match line.apply(edit) {
            // The text changed, so a host that ranks candidates below the line
            // ranks them again here.
            LineChange::TextChanged => report("text", &line),
            // The cursor moved and the text stayed, so only the drawn cursor
            // moves.
            LineChange::CursorMoved => report("cursor", &line),
            // The edit reached the line and changed nothing, so the frame stays
            // as it is.
            LineChange::Unchanged => report("unchanged", &line),
            // The line holds no candidate list and no prompt, so the host owns
            // the completion keys, the accept, and the cancel.
            LineChange::Deferred => {
                println!("the host runs {:?} over {:?}", edit, line.text());
            }
        }
    }

    // The bound refuses rather than cuts.
    let mut full =
        EditedLine::opened(String::from("12345678"), 8).expect("the seed meets the limit");
    assert_eq!(full.apply(PromptEdit::Insert('9')), LineChange::Unchanged);
    println!("the bound of 8 characters keeps {:?}", full.text());
}

/// Prints the line as a host draws it, with a marker at the cursor.
fn report(change: &str, line: &EditedLine) {
    let offset = line.cursor_offset();
    println!(
        "{change:>9}  {}|{}  cursor {}",
        &line.text()[..offset],
        &line.text()[offset..],
        line.cursor()
    );
}
