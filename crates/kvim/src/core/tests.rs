//! Behavior tests for the buffer, its coordinates, and its bounded history.

use super::{
    CharRange, CoordinateError, EditError, EditTransaction, LineEnding, LoadError, TextBuffer,
    TextChange, UNDO_HISTORY_BYTES_MAX, UNDO_HISTORY_ENTRIES_MAX,
};
use crate::settings::FileSettings;

fn buffer(text: &str) -> TextBuffer {
    TextBuffer::from_text(text, &FileSettings::default()).expect("the test text is small")
}

fn range(buffer: &TextBuffer, start: usize, end: usize) -> CharRange {
    let start = buffer.char_position(start).expect("the start exists");
    let end = buffer.char_position(end).expect("the end exists");
    CharRange::new(start, end).expect("the test range ascends")
}

fn replace(buffer: &mut TextBuffer, start: usize, end: usize, text: &str) {
    let cursor = buffer.char_position(start).expect("the cursor exists");
    let change = TextChange::replace(range(buffer, start, end), text);
    buffer
        .apply(EditTransaction::single(cursor, change))
        .expect("the test range fits the buffer");
}

#[test]
fn an_edit_sequence_produces_deterministic_text() {
    let mut buffer = buffer("let value = 1;\n");
    replace(&mut buffer, 12, 13, "42");
    replace(&mut buffer, 0, 0, "// note\n");
    replace(&mut buffer, 8, 12, "");
    assert_eq!(buffer.to_string(), "// note\nvalue = 42;\n");
    assert_eq!(buffer.version().get(), 3);
}

#[test]
fn one_transaction_changes_several_lines_and_reverses_as_one_step() {
    let mut buffer = buffer("one\ntwo\nthree\n");
    let cursor = buffer.char_position(0).expect("the cursor exists");
    let changes = vec![
        TextChange::insert(buffer.char_position(0).expect("the position exists"), "> "),
        TextChange::delete(range(&buffer, 4, 7)),
        TextChange::replace(range(&buffer, 8, 13), "3"),
    ];
    let transaction = EditTransaction::new(cursor, changes).expect("the changes ascend");
    buffer
        .apply(transaction)
        .expect("the ranges fit the buffer");
    assert_eq!(buffer.to_string(), "> one\n\n3\n");

    assert_eq!(buffer.undo(), Some(cursor));
    assert_eq!(buffer.to_string(), "one\ntwo\nthree\n");
}

#[test]
fn a_rejected_transaction_leaves_the_buffer_unchanged() {
    let mut buffer = buffer("abc");
    let short = buffer.char_position(3).expect("the position exists");
    let long = TextBuffer::from_text("abcdefgh", &FileSettings::default())
        .expect("the text is small")
        .char_position(8)
        .expect("the position exists");
    let change = TextChange::delete(CharRange::new(short, long).expect("the range ascends"));

    assert_eq!(
        buffer.apply(EditTransaction::single(short, change)),
        Err(EditError::RangeOutOfBounds {
            start: 3,
            end: 8,
            len_chars: 3,
        })
    );
    assert_eq!(
        buffer.apply(EditTransaction::single(
            long,
            TextChange::insert(short, "x")
        )),
        Err(EditError::CursorOutOfBounds {
            position: 8,
            len_chars: 3,
        })
    );
    assert_eq!(buffer.to_string(), "abc");
    assert_eq!(buffer.version().get(), 0);
    assert!(!buffer.is_modified());
}

#[test]
fn a_byte_offset_inside_a_character_is_a_typed_error() {
    let buffer = buffer("aé漢");
    assert_eq!(buffer.len_bytes(), 6);
    assert_eq!(buffer.len_chars(), 3);

    for boundary in [0, 1, 3, 6] {
        assert!(buffer.byte_offset(boundary).is_ok());
    }
    for split in [2, 4, 5] {
        assert_eq!(
            buffer.byte_offset(split),
            Err(CoordinateError::ByteSplitsCharacter { offset: split })
        );
    }
    assert_eq!(
        buffer.byte_offset(7),
        Err(CoordinateError::ByteOutOfBounds {
            offset: 7,
            len_bytes: 6,
        })
    );
}

#[test]
fn a_combining_mark_keeps_its_own_character_position() {
    // The text holds one base character and one combining acute accent.
    let mut buffer = buffer("e\u{301}x");
    assert_eq!(buffer.len_chars(), 3);
    assert_eq!(buffer.len_bytes(), 4);

    assert!(buffer.byte_offset(1).is_ok());
    assert_eq!(
        buffer.byte_offset(2),
        Err(CoordinateError::ByteSplitsCharacter { offset: 2 })
    );

    // An edit between the base character and the mark keeps valid text.
    replace(&mut buffer, 1, 1, "\u{302}");
    assert_eq!(buffer.to_string(), "e\u{302}\u{301}x");
    assert_eq!(buffer.undo().map(|cursor| cursor.get()), Some(1));
    assert_eq!(buffer.to_string(), "e\u{301}x");
}

#[test]
fn every_coordinate_kind_rejects_its_invalid_value() {
    let buffer = buffer("one\ntwo\n");
    assert_eq!(
        buffer.char_position(9),
        Err(CoordinateError::CharOutOfBounds {
            position: 9,
            len_chars: 8,
        })
    );
    assert_eq!(
        buffer.line_index(3),
        Err(CoordinateError::LineOutOfBounds {
            index: 3,
            line_count: 3,
        })
    );
    let line = buffer.line_index(0).expect("the first line exists");
    assert_eq!(
        buffer.source_column(line, 4),
        Err(CoordinateError::ColumnOutOfBounds {
            column: 4,
            line_len_chars: 3,
        })
    );
    assert!(buffer.source_column(line, 3).is_ok());
}

#[test]
fn coordinates_convert_across_a_multi_byte_line() {
    let buffer = buffer("héllo\nwörld\n");
    let line = buffer.line_index(1).expect("the second line exists");
    let start = buffer.line_start(line);
    assert_eq!(start.get(), 6);
    assert_eq!(buffer.char_to_byte(start).get(), 7);

    let column = buffer.source_column(line, 3).expect("the column exists");
    let position = buffer.column_to_char(line, column);
    assert_eq!(position.get(), 9);
    assert_eq!(buffer.char_to_line(position), line);
    assert_eq!(buffer.char_to_column(position), column);

    let offset = buffer.byte_offset(11).expect("the offset is a boundary");
    assert_eq!(buffer.byte_to_char(offset).get(), 9);
}

#[test]
fn both_line_endings_survive_load_and_line_access() {
    let unix = buffer("one\ntwo\n");
    assert_eq!(unix.line_ending(), LineEnding::Lf);
    assert_eq!(unix.line_count(), 3);
    assert_eq!(
        unix.line_text(unix.line_index(1).expect("the line exists")),
        "two"
    );

    let windows = buffer("one\r\ntwo\r\n");
    assert_eq!(windows.line_ending(), LineEnding::Crlf);
    assert_eq!(windows.line_count(), 3);
    let line = windows.line_index(0).expect("the line exists");
    assert_eq!(windows.line_text(line), "one");
    assert_eq!(windows.line_len_chars(line), 3);
    assert_eq!(windows.to_string(), "one\r\ntwo\r\n");
}

#[test]
fn a_mixed_file_keeps_its_first_line_ending_for_new_lines() {
    let mut buffer = buffer("one\r\ntwo\n");
    assert_eq!(buffer.line_ending(), LineEnding::Crlf);
    let ending = buffer.line_ending().as_str().to_owned();
    let end = buffer.len_chars();
    replace(&mut buffer, end, end, &ending);
    assert_eq!(buffer.to_string(), "one\r\ntwo\n\r\n");
    assert_eq!(
        buffer.line_text(buffer.line_index(1).expect("the line exists")),
        "two"
    );
}

#[test]
fn undo_and_redo_walk_the_history_in_both_directions() {
    let mut buffer = buffer("");
    replace(&mut buffer, 0, 0, "one");
    replace(&mut buffer, 3, 3, " two");
    assert_eq!(buffer.to_string(), "one two");

    assert!(buffer.undo().is_some());
    assert_eq!(buffer.to_string(), "one");
    assert!(buffer.undo().is_some());
    assert_eq!(buffer.to_string(), "");
    assert_eq!(buffer.undo(), None);

    assert!(buffer.redo().is_some());
    assert_eq!(buffer.to_string(), "one");
    assert_eq!(buffer.redo().map(|cursor| cursor.get()), Some(7));
    assert_eq!(buffer.to_string(), "one two");
    assert_eq!(buffer.redo(), None);
}

#[test]
fn a_new_transaction_discards_the_redo_entries() {
    let mut buffer = buffer("");
    replace(&mut buffer, 0, 0, "one");
    replace(&mut buffer, 3, 3, " two");
    assert!(buffer.undo().is_some());

    replace(&mut buffer, 3, 3, " three");
    assert_eq!(buffer.to_string(), "one three");
    assert_eq!(buffer.redo(), None);
    assert!(buffer.undo().is_some());
    assert_eq!(buffer.to_string(), "one");
}

#[test]
fn the_dirty_state_follows_the_saved_history_position() {
    let mut buffer = buffer("one\n");
    assert!(!buffer.is_modified());

    replace(&mut buffer, 4, 4, "two\n");
    assert!(buffer.is_modified());

    buffer.mark_saved();
    assert!(!buffer.is_modified());

    assert!(buffer.undo().is_some());
    assert!(buffer.is_modified());
    assert!(buffer.redo().is_some());
    assert!(!buffer.is_modified());

    assert!(buffer.undo().is_some());
    replace(&mut buffer, 4, 4, "three\n");
    // The discarded redo entry held the saved state.
    assert!(buffer.is_modified());
    assert!(buffer.undo().is_some());
    assert!(buffer.is_modified());
}

#[test]
fn the_history_keeps_at_most_the_entry_bound() {
    let overflow = 10;
    let mut buffer = buffer("");
    for _ in 0..UNDO_HISTORY_ENTRIES_MAX + overflow {
        replace(&mut buffer, 0, 0, "a");
    }

    let mut undone = 0;
    while buffer.undo().is_some() {
        undone += 1;
    }
    assert_eq!(undone, UNDO_HISTORY_ENTRIES_MAX);
    assert_eq!(buffer.len_chars(), overflow);
}

#[test]
fn the_history_keeps_at_most_the_retained_byte_bound() {
    let chunk_bytes = 1024 * 1024;
    let kept = UNDO_HISTORY_BYTES_MAX / chunk_bytes;
    let chunk = "a".repeat(chunk_bytes);

    let mut buffer = buffer("");
    for _ in 0..kept + 1 {
        replace(&mut buffer, 0, 0, &chunk);
    }

    let mut undone = 0;
    while buffer.undo().is_some() {
        undone += 1;
    }
    assert_eq!(undone, kept);
}

#[test]
fn an_oversized_text_never_becomes_a_buffer() {
    let files = FileSettings {
        max_file_bytes: 8,
        ..FileSettings::default()
    };
    assert_eq!(
        TextBuffer::from_text("123456789", &files).unwrap_err(),
        LoadError::TooLarge {
            bytes: 9,
            max_bytes: 8,
        }
    );
    assert!(TextBuffer::from_text("12345678", &files).is_ok());
}

#[test]
fn undo_restores_the_original_text_for_generated_edit_sequences() {
    const ORIGINAL: &str = "fn main() {\n    println!(\"héllo wörld\");\n}\n";
    const REPLACEMENTS: [&str; 4] = ["", "x", "é\n", "    "];

    for seed in 0..32u64 {
        let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize
        };

        let mut buffer = buffer(ORIGINAL);
        let mut applied = 0;
        for _ in 0..16 {
            let len = buffer.len_chars();
            let start = next() % (len + 1);
            let end = start + next() % (len + 1 - start).min(6);
            replace(&mut buffer, start, end, REPLACEMENTS[next() % 4]);
            applied += 1;
        }
        let edited = buffer.to_string();

        for _ in 0..applied {
            assert!(buffer.undo().is_some(), "every applied step reverses");
        }
        assert_eq!(buffer.to_string(), ORIGINAL);
        assert!(!buffer.is_modified());

        for _ in 0..applied {
            assert!(buffer.redo().is_some(), "every reversed step replays");
        }
        assert_eq!(buffer.to_string(), edited);
    }
}
