use super::{CharRange, EditTransaction, TextChange, TransactionError};
use crate::{BufferBytesMax, CharPosition, TextBuffer};

fn positions(text: &str, wanted: &[usize]) -> Vec<CharPosition> {
    let buffer =
        TextBuffer::from_text(text, BufferBytesMax::default()).expect("the test text is small");
    wanted
        .iter()
        .map(|position| {
            buffer
                .char_position(*position)
                .expect("the test position exists")
        })
        .collect()
}

#[test]
fn a_reversed_range_is_rejected() {
    let bounds = positions("hello", &[1, 4]);
    assert_eq!(
        CharRange::new(bounds[1], bounds[0]),
        Err(TransactionError::ReversedRange { start: 4, end: 1 })
    );
}

#[test]
fn an_empty_transaction_is_rejected() {
    let cursor = positions("hello", &[0]);
    assert_eq!(
        EditTransaction::new(cursor[0], Vec::new()),
        Err(TransactionError::Empty)
    );
}

#[test]
fn overlapping_changes_are_rejected() {
    let bounds = positions("hello world", &[0, 5, 3, 8]);
    let first = CharRange::new(bounds[0], bounds[1]).expect("the range ascends");
    let second = CharRange::new(bounds[2], bounds[3]).expect("the range ascends");
    assert_eq!(
        EditTransaction::new(
            bounds[0],
            vec![TextChange::delete(first), TextChange::delete(second)],
        ),
        Err(TransactionError::OverlappingChanges {
            start: 3,
            previous_end: 5,
        })
    );
}

#[test]
fn descending_changes_are_rejected() {
    let bounds = positions("hello world", &[6, 0]);
    assert_eq!(
        EditTransaction::new(
            bounds[1],
            vec![
                TextChange::insert(bounds[0], "a"),
                TextChange::insert(bounds[1], "b"),
            ],
        ),
        Err(TransactionError::OverlappingChanges {
            start: 0,
            previous_end: 6,
        })
    );
}

#[test]
fn two_insertions_at_one_position_stay_valid() {
    let bounds = positions("hello", &[2]);
    let transaction = EditTransaction::new(
        bounds[0],
        vec![
            TextChange::insert(bounds[0], "a"),
            TextChange::insert(bounds[0], "b"),
        ],
    )
    .expect("empty ranges at one position do not overlap");
    assert_eq!(transaction.changes().len(), 2);
}
