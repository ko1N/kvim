use crossterm::event::{
    Event as CrosstermEvent, KeyCode as CrosstermKeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use futures_util::stream;

use kvim_keymap::{KeyCode, PASTE_BYTES_MAX};

use super::*;

fn source(
    events: Vec<CrosstermEvent>,
) -> EventSource<impl Stream<Item = io::Result<CrosstermEvent>> + Unpin> {
    EventSource::new(stream::iter(events.into_iter().map(Ok)))
}

fn mouse(kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) -> CrosstermEvent {
    CrosstermEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
}

#[tokio::test]
async fn the_source_skips_shift_modified_pointer_input() {
    let key = KeyEvent::new(CrosstermKeyCode::Esc, KeyModifiers::NONE);
    let mut source = source(vec![
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            4,
            2,
            KeyModifiers::SHIFT,
        ),
        CrosstermEvent::Key(key),
    ]);

    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Key(key))) if key == Key::plain(KeyCode::Esc)
    ));
}

#[tokio::test]
async fn the_source_normalizes_pointer_actions_without_crossterm_values() {
    let mut source = source(vec![
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            4,
            2,
            KeyModifiers::CONTROL,
        ),
        mouse(
            MouseEventKind::Up(MouseButton::Right),
            5,
            3,
            KeyModifiers::ALT,
        ),
        mouse(
            MouseEventKind::Drag(MouseButton::Middle),
            6,
            4,
            KeyModifiers::SUPER,
        ),
    ]);

    let press = source.next_event().await;
    let release = source.next_event().await;
    let drag = source.next_event().await;

    assert!(matches!(
        press,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.position() == CellPosition::new(4, 2)
                && event.modifiers().control()
                && event.action() == PointerAction::Press(PointerButton::Left)
    ));
    assert!(matches!(
        release,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.action() == PointerAction::Release(PointerButton::Right)
                && event.modifiers().alt()
    ));
    assert!(matches!(
        drag,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.action() == PointerAction::Drag(PointerButton::Middle)
                && event.modifiers().super_key()
    ));
}

#[tokio::test]
async fn the_source_coalesces_immediately_ready_consecutive_wheel_events() {
    let mut source = source(vec![
        mouse(MouseEventKind::ScrollDown, 4, 2, KeyModifiers::NONE),
        mouse(MouseEventKind::ScrollDown, 4, 2, KeyModifiers::NONE),
        mouse(MouseEventKind::ScrollDown, 4, 2, KeyModifiers::NONE),
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            5,
            2,
            KeyModifiers::NONE,
        ),
    ]);

    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.position() == CellPosition::new(4, 2)
                && event.action()
                    == PointerAction::Wheel(
                        PointerWheel::new(PointerWheelDirection::Down, 3)
                            .expect("three ticks are within the coalescing bound"),
                    )
    ));
    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.action() == PointerAction::Press(PointerButton::Left)
    ));
}

#[test]
fn pointer_wheel_rejects_invalid_tick_counts() {
    assert_eq!(
        PointerWheel::new(PointerWheelDirection::Up, 0),
        Err(PointerWheelError::ZeroTicks)
    );
    assert_eq!(
        PointerWheel::new(PointerWheelDirection::Up, POINTER_EVENTS_COALESCE_MAX + 1,),
        Err(PointerWheelError::TooManyTicks {
            ticks: POINTER_EVENTS_COALESCE_MAX + 1,
        })
    );
}

#[test]
fn hosts_can_construct_terminal_neutral_pointer_events() {
    let modifiers = PointerModifiers::new(true, false, true);
    let wheel = PointerWheel::new(PointerWheelDirection::Left, 2)
        .expect("two ticks are within the coalescing bound");
    let event = PointerEvent::new(
        CellPosition::new(4, 2),
        modifiers,
        PointerAction::Wheel(wheel),
    );

    assert_eq!(event.position(), CellPosition::new(4, 2));
    assert_eq!(event.modifiers(), modifiers);
    assert_eq!(event.action(), PointerAction::Wheel(wheel));
}

#[tokio::test]
async fn the_source_keeps_a_changed_position_wheel_event_pending() {
    let mut source = source(vec![
        mouse(MouseEventKind::ScrollDown, 4, 2, KeyModifiers::NONE),
        mouse(MouseEventKind::ScrollDown, 5, 2, KeyModifiers::NONE),
    ]);

    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.position() == CellPosition::new(4, 2)
                && event.action()
                    == PointerAction::Wheel(
                        PointerWheel::new(PointerWheelDirection::Down, 1)
                            .expect("one tick is within the coalescing bound"),
                    )
    ));
    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.position() == CellPosition::new(5, 2)
                && event.action()
                    == PointerAction::Wheel(
                        PointerWheel::new(PointerWheelDirection::Down, 1)
                            .expect("one tick is within the coalescing bound"),
                    )
    ));
}

#[tokio::test]
async fn the_source_coalesces_compatible_motion_to_the_latest_position() {
    let mut source = source(vec![
        mouse(MouseEventKind::Moved, 4, 2, KeyModifiers::CONTROL),
        mouse(MouseEventKind::Moved, 6, 3, KeyModifiers::CONTROL),
    ]);

    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.position() == CellPosition::new(6, 3)
                && event.modifiers() == PointerModifiers::new(true, false, false)
                && event.action() == PointerAction::Motion
    ));
}

#[tokio::test]
async fn the_source_keeps_a_motion_modifier_transition_pending() {
    let mut source = source(vec![
        mouse(MouseEventKind::Moved, 4, 2, KeyModifiers::NONE),
        mouse(MouseEventKind::Moved, 6, 3, KeyModifiers::CONTROL),
    ]);

    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.position() == CellPosition::new(4, 2)
                && event.modifiers() == PointerModifiers::default()
                && event.action() == PointerAction::Motion
    ));
    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.position() == CellPosition::new(6, 3)
                && event.modifiers() == PointerModifiers::new(true, false, false)
                && event.action() == PointerAction::Motion
    ));
}

#[tokio::test]
async fn the_source_keeps_the_first_nonmatching_event_pending() {
    let mut source = source(vec![
        mouse(MouseEventKind::Moved, 4, 2, KeyModifiers::NONE),
        CrosstermEvent::Resize(120, 40),
    ]);

    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Pointer(_)))
    ));
    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Resize {
            columns: 120,
            rows: 40
        }))
    ));
}

#[tokio::test]
async fn the_source_stops_wheel_coalescing_at_the_published_bound() {
    let mut events = Vec::with_capacity(usize::from(POINTER_EVENTS_COALESCE_MAX) + 1);
    for _ in 0..=POINTER_EVENTS_COALESCE_MAX {
        events.push(mouse(MouseEventKind::ScrollUp, 0, 0, KeyModifiers::NONE));
    }
    let mut source = source(events);

    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.action()
                == PointerAction::Wheel(
                    PointerWheel::new(PointerWheelDirection::Up, POINTER_EVENTS_COALESCE_MAX)
                        .expect("the published coalescing bound is valid"),
                )
    ));
    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Pointer(event)))
            if event.action()
                == PointerAction::Wheel(
                    PointerWheel::new(PointerWheelDirection::Up, 1)
                        .expect("one tick is within the coalescing bound"),
                )
    ));
}

#[tokio::test]
async fn a_rejected_key_reaches_the_editor_as_unsupported_input() {
    let rejected = KeyEvent::new(
        CrosstermKeyCode::Char('d'),
        KeyModifiers::SUPER | KeyModifiers::CONTROL,
    );
    let mut source = source(vec![CrosstermEvent::Key(rejected)]);

    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Unsupported))
    ));
}

#[tokio::test]
async fn a_paste_block_above_the_bound_reaches_the_editor_as_unsupported_input() {
    let mut source = source(vec![CrosstermEvent::Paste("x".repeat(PASTE_BYTES_MAX + 1))]);

    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Unsupported))
    ));
}

#[tokio::test]
async fn the_source_skips_key_releases_and_empty_paste_blocks() {
    let released = KeyEvent::new_with_kind(
        CrosstermKeyCode::Char('g'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );
    let key = KeyEvent::new(CrosstermKeyCode::Esc, KeyModifiers::NONE);
    let mut source = source(vec![
        CrosstermEvent::Paste(String::new()),
        CrosstermEvent::Key(released),
        CrosstermEvent::Key(key),
    ]);

    assert!(matches!(
        source.next_event().await,
        Some(Ok(TerminalEvent::Key(key))) if key == Key::plain(KeyCode::Esc)
    ));
}

#[tokio::test]
async fn the_source_ends_with_the_stream() {
    let mut source = source(Vec::new());
    assert!(source.next_event().await.is_none());
}

#[tokio::test]
async fn the_source_reports_a_burst_of_unmapped_events() {
    let unmapped = (0..=UNMAPPED_EVENT_SKIP_MAX)
        .map(|_| CrosstermEvent::FocusGained)
        .collect();
    let mut source = source(unmapped);

    assert!(matches!(
        source.next_event().await,
        Some(Err(TerminalError::UnmappedEventBurst))
    ));
}

#[tokio::test]
async fn the_source_reports_a_stream_failure() {
    let failure = io::Error::other("terminal read failed");
    let mut source = EventSource::new(stream::iter(vec![Err(failure)]));
    assert!(matches!(
        source.next_event().await,
        Some(Err(TerminalError::Read(_)))
    ));
}
