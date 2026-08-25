use crossterm::event::{
    KeyCode as CrosstermKeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
};
use futures_util::stream;

use kvim_keymap::{KeyCode, PASTE_BYTES_MAX};

use super::*;

fn source(
    events: Vec<CrosstermEvent>,
) -> EventSource<impl Stream<Item = io::Result<CrosstermEvent>> + Unpin> {
    EventSource::new(stream::iter(events.into_iter().map(Ok)))
}

fn mouse_event() -> CrosstermEvent {
    CrosstermEvent::Mouse(MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    })
}

#[tokio::test]
async fn the_source_skips_an_event_that_names_no_input() {
    // A mouse event, an empty paste block, and a key release carry nothing
    // to report, so the pending grammar of the editor must not see them at
    // all. A release that reached the editor would clear the pending
    // sequence between the two presses of `gg`, because
    // `REPORT_EVENT_TYPES` makes the terminal send one after every key.
    let released = KeyEvent::new_with_kind(
        CrosstermKeyCode::Char('g'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );
    let key = KeyEvent::new(CrosstermKeyCode::Esc, KeyModifiers::NONE);
    let mut source = source(vec![
        mouse_event(),
        CrosstermEvent::Paste(String::new()),
        CrosstermEvent::Key(released),
        CrosstermEvent::Key(key),
    ]);

    let event = source.next_event().await;

    assert!(matches!(
        event,
        Some(Ok(TerminalEvent::Key(key))) if key == Key::plain(KeyCode::Esc)
    ));
}

#[tokio::test]
async fn the_source_normalizes_one_bounded_paste_block() {
    let mut source = source(vec![CrosstermEvent::Paste("two words".to_owned())]);

    let event = source.next_event().await;

    assert_eq!(
        event.map(|event| event.expect("the block is bounded")),
        Some(TerminalEvent::Paste(
            PasteText::new("two words").expect("the block is bounded")
        ))
    );
}

#[tokio::test]
async fn a_rejected_key_reaches_the_editor_as_unsupported_input() {
    // A rejected chord must never degrade into the binding of its
    // unmodified key, so the editor resets its pending grammar instead.
    let rejected = KeyEvent::new(
        CrosstermKeyCode::Char('d'),
        KeyModifiers::SUPER | KeyModifiers::CONTROL,
    );
    let mut source = source(vec![CrosstermEvent::Key(rejected)]);

    let event = source.next_event().await;

    assert_eq!(
        event.map(|event| event.expect("a rejected key names input")),
        Some(TerminalEvent::Unsupported)
    );
}

#[tokio::test]
async fn a_paste_block_above_the_bound_reaches_the_editor_as_unsupported_input() {
    let long = "x".repeat(PASTE_BYTES_MAX + 1);
    let mut source = source(vec![CrosstermEvent::Paste(long)]);

    let event = source.next_event().await;

    assert_eq!(
        event.map(|event| event.expect("an over-long block names input")),
        Some(TerminalEvent::Unsupported)
    );
}

#[tokio::test]
async fn the_source_ends_with_the_stream() {
    let mut source = source(Vec::new());

    assert!(source.next_event().await.is_none());
}

#[tokio::test]
async fn the_source_reports_a_burst_of_unmapped_events() {
    let unmapped = (0..=UNMAPPED_EVENT_SKIP_MAX)
        .map(|_| mouse_event())
        .collect();
    let mut source = source(unmapped);

    let event = source.next_event().await;

    assert!(matches!(
        event,
        Some(Err(TerminalError::UnmappedEventBurst))
    ));
}

#[tokio::test]
async fn the_source_reports_a_stream_failure() {
    let failure = io::Error::other("terminal read failed");
    let mut source = EventSource::new(stream::iter(vec![Err(failure)]));

    let event = source.next_event().await;

    assert!(matches!(event, Some(Err(TerminalError::Read(_)))));
}
