//! Tests of the line that one prompt edit edits.

use super::*;

/// The bound that these tests state, in characters.
const CHARS_MAX: usize = 32;

/// Returns one line over `text`, with the cursor after it.
fn line(text: &str) -> EditedLine {
    EditedLine::opened(text.to_owned(), CHARS_MAX).expect("the test seed meets the limit")
}

/// Reports whether the cursor names a character boundary inside the text.
fn holds_the_cursor_rule(line: &EditedLine) -> bool {
    let chars = line.text().chars().count();
    line.cursor() <= chars && line.text().is_char_boundary(line.cursor_offset())
}

#[test]
fn a_host_applies_every_edit_and_reads_the_text_and_the_cursor() {
    let mut line = line("one two");
    assert_eq!(line.cursor(), 7, "the line opens after its text");

    assert_eq!(line.apply(PromptEdit::Insert('!')), LineChange::TextChanged);
    assert_eq!(line.text(), "one two!");
    assert_eq!(line.cursor(), 8);

    assert_eq!(
        line.apply(PromptEdit::DeleteBackward),
        LineChange::TextChanged
    );
    assert_eq!(line.text(), "one two");
    assert_eq!(line.cursor(), 7);

    assert_eq!(line.apply(PromptEdit::CursorLeft), LineChange::CursorMoved);
    assert_eq!(line.cursor(), 6);
    assert_eq!(line.apply(PromptEdit::CursorRight), LineChange::CursorMoved);
    assert_eq!(line.cursor(), 7);

    assert_eq!(
        line.apply(PromptEdit::CursorWordBackward),
        LineChange::CursorMoved
    );
    assert_eq!(line.cursor(), 4, "the motion lands where the word starts");
    assert_eq!(
        line.apply(PromptEdit::CursorWordForward),
        LineChange::CursorMoved
    );
    assert_eq!(line.cursor(), 7, "no word follows, so the motion stops");

    assert_eq!(
        line.apply(PromptEdit::CursorLineStart),
        LineChange::CursorMoved
    );
    assert_eq!(line.cursor(), 0);
    assert_eq!(
        line.apply(PromptEdit::CursorLineStart),
        LineChange::Unchanged,
        "a motion at the end that it names changes nothing"
    );
    assert_eq!(
        line.apply(PromptEdit::DeleteBackward),
        LineChange::Unchanged,
        "no character stands before the start of the line"
    );

    assert_eq!(
        line.apply(PromptEdit::CursorLineEnd),
        LineChange::CursorMoved
    );
    assert_eq!(line.cursor(), 7);
    assert_eq!(
        line.apply(PromptEdit::DeleteWordBackward),
        LineChange::TextChanged
    );
    assert_eq!(line.text(), "one ");
    assert_eq!(line.cursor(), 4);

    // The completion, the accept, and the cancel end a prompt that the caller
    // owns, so the line changes nothing and reports them back.
    for edit in [
        PromptEdit::CompleteNext,
        PromptEdit::CompletePrevious,
        PromptEdit::Accept,
        PromptEdit::Cancel,
    ] {
        assert_eq!(line.apply(edit), LineChange::Deferred, "{edit:?}");
        assert_eq!(line.text(), "one ");
        assert_eq!(line.cursor(), 4);
    }
}

#[test]
fn every_edit_leaves_the_cursor_on_a_character_boundary() {
    // Every character of this text encodes in more than one byte, so a cursor
    // that counted bytes would land inside one of them.
    let mut line = line("äö 語彙");
    assert!(holds_the_cursor_rule(&line));

    for edit in [
        PromptEdit::CursorLineStart,
        PromptEdit::CursorRight,
        PromptEdit::Insert('ß'),
        PromptEdit::CursorWordForward,
        PromptEdit::CursorWordBackward,
        PromptEdit::CursorLineEnd,
        PromptEdit::DeleteBackward,
        PromptEdit::DeleteWordBackward,
        PromptEdit::CursorLeft,
    ] {
        let _ = line.apply(edit);
        assert!(holds_the_cursor_rule(&line), "{edit:?}");
    }
}

#[test]
fn the_word_delete_stops_at_the_cursor_and_keeps_the_text_after_it() {
    let mut line = EditedLine::opened_at(String::from("one two three"), 7, CHARS_MAX)
        .expect("the seed meets the limit");
    assert_eq!(
        line.apply(PromptEdit::DeleteWordBackward),
        LineChange::TextChanged
    );
    assert_eq!(
        line.text(),
        "one  three",
        "the blank after the cursor stays"
    );
    assert_eq!(line.cursor(), 4, "the cursor steps back over every removal");
}

#[test]
fn the_bound_refuses_an_insert_instead_of_cutting_the_line() {
    let mut line = EditedLine::opened(String::from("abc"), 4).expect("the seed meets the limit");
    assert_eq!(line.apply(PromptEdit::Insert('d')), LineChange::TextChanged);
    assert_eq!(
        line.apply(PromptEdit::Insert('e')),
        LineChange::Unchanged,
        "the line stands at its bound"
    );
    assert_eq!(line.text(), "abcd", "the refusal drops no character");
    assert_eq!(line.cursor(), 4);

    // The bound counts the whole line, so an insert in the middle meets it too.
    assert_eq!(
        line.apply(PromptEdit::CursorLineStart),
        LineChange::CursorMoved
    );
    assert_eq!(line.apply(PromptEdit::Insert('z')), LineChange::Unchanged);
    assert_eq!(line.text(), "abcd");
}

#[test]
fn a_written_line_above_the_bound_changes_nothing() {
    let mut line = EditedLine::opened(String::from("ab"), 4).expect("the seed meets the limit");
    assert_eq!(line.write(String::from("abcde")), LineChange::Unchanged);
    assert_eq!(line.text(), "ab", "the bound refuses rather than cuts");
    assert_eq!(line.cursor(), 2);

    assert_eq!(line.write(String::from("abcd")), LineChange::TextChanged);
    assert_eq!(line.text(), "abcd");
    assert_eq!(
        line.cursor(),
        4,
        "the reader continues after the whole line"
    );
}

#[test]
fn a_seeded_cursor_never_passes_the_characters_of_its_text() {
    let line = EditedLine::opened_at(String::from("näme"), 99, CHARS_MAX)
        .expect("the seed meets the limit");
    assert_eq!(line.cursor(), 4);
    assert!(holds_the_cursor_rule(&line));
}

#[test]
fn public_open_rejects_zero_limits_and_oversized_seeds() {
    assert_eq!(
        EditedLine::opened(String::new(), 0),
        Err(EditedLineError::ZeroLimit)
    );
    assert_eq!(
        EditedLine::opened_at(String::from("three"), 2, 4),
        Err(EditedLineError::SeedTooLong {
            chars: 5,
            chars_max: 4,
        })
    );
}
