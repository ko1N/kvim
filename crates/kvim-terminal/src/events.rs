//! Normalized terminal events and the bounded terminal event source.

use std::future::poll_fn;
use std::io;
use std::pin::Pin;
use std::task::Poll;

use crossterm::event::{
    Event as CrosstermEvent, EventStream, KeyModifiers, MouseButton as CrosstermMouseButton,
    MouseEvent, MouseEventKind,
};
use futures_util::stream::{Stream, StreamExt};

use kvim_keymap::{Key, PasteError, PasteText};
use thiserror::Error;

use super::TerminalError;
use crate::key::{KeyRejection, normalize_key_event};

/// The maximum number of consecutive terminal events without a normalized form
/// that one read attempt skips. The bound keeps a read attempt finite when a
/// terminal sends unsupported events continuously.
pub const UNMAPPED_EVENT_SKIP_MAX: usize = 64;
pub use kvim_keymap::{
    CellPosition, POINTER_EVENTS_COALESCE_MAX, PointerAction, PointerButton, PointerEvent,
    PointerModifiers, PointerWheel, PointerWheelDirection, PointerWheelError,
};

fn normalize_mouse(event: MouseEvent) -> Result<PointerEvent, EventRejection> {
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        return Err(EventRejection::MouseShift);
    }
    let action = match event.kind {
        MouseEventKind::Down(button) => PointerAction::Press(pointer_button(button)),
        MouseEventKind::Up(button) => PointerAction::Release(pointer_button(button)),
        MouseEventKind::Drag(button) => PointerAction::Drag(pointer_button(button)),
        MouseEventKind::Moved => PointerAction::Motion,
        MouseEventKind::ScrollUp => PointerAction::Wheel(pointer_wheel(PointerWheelDirection::Up)),
        MouseEventKind::ScrollDown => {
            PointerAction::Wheel(pointer_wheel(PointerWheelDirection::Down))
        }
        MouseEventKind::ScrollLeft => {
            PointerAction::Wheel(pointer_wheel(PointerWheelDirection::Left))
        }
        MouseEventKind::ScrollRight => {
            PointerAction::Wheel(pointer_wheel(PointerWheelDirection::Right))
        }
    };
    Ok(PointerEvent::new(
        CellPosition::new(event.column, event.row),
        PointerModifiers::new(
            event.modifiers.contains(KeyModifiers::CONTROL),
            event.modifiers.contains(KeyModifiers::ALT),
            event.modifiers.contains(KeyModifiers::SUPER),
        ),
        action,
    ))
}

fn pointer_wheel(direction: PointerWheelDirection) -> PointerWheel {
    PointerWheel::new(direction, 1).expect("one tick is inside the published pointer bound")
}

fn can_merge_pointer(left: PointerEvent, right: PointerEvent) -> bool {
    match (left.action(), right.action()) {
        (PointerAction::Motion, PointerAction::Motion) => left.modifiers() == right.modifiers(),
        (PointerAction::Wheel(left_wheel), PointerAction::Wheel(right_wheel)) => {
            left.modifiers() == right.modifiers()
                && left_wheel.direction() == right_wheel.direction()
                && right_wheel.ticks() <= POINTER_EVENTS_COALESCE_MAX - left_wheel.ticks()
        }
        _ => false,
    }
}

fn merge_pointer(left: &mut PointerEvent, right: PointerEvent) {
    debug_assert!(
        can_merge_pointer(*left, right),
        "the event source merges only consecutive motions or equal wheel directions"
    );
    match (left.action(), right.action()) {
        (PointerAction::Motion, PointerAction::Motion) => *left = right,
        (PointerAction::Wheel(left_wheel), PointerAction::Wheel(right_wheel)) => {
            let wheel = PointerWheel::new(
                left_wheel.direction(),
                left_wheel.ticks() + right_wheel.ticks(),
            )
            .expect("can_merge_pointer keeps the wheel inside its published bound");
            *left = PointerEvent::new(
                right.position(),
                left.modifiers(),
                PointerAction::Wheel(wheel),
            );
        }
        _ => unreachable!("can_merge_pointer validated the pointer action pair"),
    }
}

const fn pointer_button(button: CrosstermMouseButton) -> PointerButton {
    match button {
        CrosstermMouseButton::Left => PointerButton::Left,
        CrosstermMouseButton::Right => PointerButton::Right,
        CrosstermMouseButton::Middle => PointerButton::Middle,
    }
}

/// The reason that one crossterm event carries no normalized form.
///
/// The reason stays typed up to the event source, so a rejected modifier never
/// looks like a missing event.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EventRejection {
    /// The key event carried no normalized key.
    #[error("the terminal key event carries no normalized key")]
    Key(#[from] KeyRejection),
    /// A focus event carries no normalized form because focus reporting is disabled.
    #[error("the terminal reported focus while focus reporting is disabled")]
    Focus,
    /// Shift-modified mouse input is reserved for native terminal selection.
    #[error("the terminal reported Shift-modified mouse input")]
    MouseShift,
    /// A bracketed paste carried no bounded, non-empty block.
    #[error("the terminal paste block carries no bounded input")]
    Paste(#[from] PasteError),
}

/// One normalized terminal event.
///
/// The event loop consumes these values. It never inspects a crossterm event.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum TerminalEvent {
    /// A key press or a key repeat.
    Key(Key),
    /// One bounded bracketed-paste block.
    ///
    /// The block is one input, so the editor applies it as one edit
    /// transaction and one undo unit. See `docs/input-actions.md`.
    Paste(PasteText),
    /// One terminal-neutral pointer event.
    Pointer(PointerEvent),
    /// The terminal window changed size.
    Resize {
        /// The new terminal width in cells.
        columns: u16,
        /// The new terminal height in cells.
        rows: u16,
    },
    /// The terminal reported input that no binding accepts.
    ///
    /// A key with an unsupported modifier and a paste block above
    /// [`PASTE_BYTES_MAX`](kvim_keymap::PASTE_BYTES_MAX) both reach this
    /// event. The editor resets its pending grammar instead of running the
    /// binding of the unmodified key or inserting part of the block.
    Unsupported,
}

impl TerminalEvent {
    /// Normalizes one crossterm event.
    ///
    /// # Errors
    ///
    /// Returns [`EventRejection`] for a Shift-modified pointer event, for a
    /// paste block that carries no bounded input, and for every key event
    /// without a normalized key. [`EventSource`] turns rejections that still
    /// name input into [`TerminalEvent::Unsupported`].
    ///
    /// ```
    /// use crossterm::event::Event;
    /// use kvim_terminal::TerminalEvent;
    ///
    /// let resize = Event::Resize(120, 40);
    /// assert_eq!(
    ///     TerminalEvent::from_crossterm(resize),
    ///     Ok(TerminalEvent::Resize { columns: 120, rows: 40 }),
    /// );
    /// ```
    pub fn from_crossterm(event: CrosstermEvent) -> Result<Self, EventRejection> {
        match event {
            CrosstermEvent::Key(key) => Ok(Self::Key(normalize_key_event(key)?)),
            CrosstermEvent::Paste(text) => Ok(Self::Paste(PasteText::new(&text)?)),
            CrosstermEvent::Resize(columns, rows) => Ok(Self::Resize { columns, rows }),
            CrosstermEvent::FocusGained | CrosstermEvent::FocusLost => Err(EventRejection::Focus),
            CrosstermEvent::Mouse(mouse) => Ok(Self::Pointer(normalize_mouse(mouse)?)),
        }
    }
}

/// A bounded source of normalized terminal events.
///
/// The source is generic over the crossterm event stream, so a test drives
/// normalization without a terminal. [`EventSource::from_terminal`] supplies the
/// process terminal.
pub struct EventSource<S> {
    events: S,
    pending: Option<io::Result<CrosstermEvent>>,
}

impl EventSource<EventStream> {
    /// Reads events from the process terminal.
    ///
    /// ```no_run
    /// use kvim_terminal::EventSource;
    ///
    /// # async fn read() {
    /// let mut source = EventSource::from_terminal();
    /// while let Some(event) = source.next_event().await {
    ///     let _ = event;
    /// }
    /// # }
    /// ```
    #[must_use]
    pub fn from_terminal() -> Self {
        Self {
            events: EventStream::new(),
            pending: None,
        }
    }
}

impl<S> EventSource<S>
where
    S: Stream<Item = io::Result<CrosstermEvent>> + Unpin,
{
    /// Reads events from the supplied crossterm event stream.
    pub const fn new(events: S) -> Self {
        Self {
            events,
            pending: None,
        }
    }

    async fn next_raw(&mut self) -> Option<io::Result<CrosstermEvent>> {
        match self.pending.take() {
            Some(event) => Some(event),
            None => self.events.next().await,
        }
    }

    async fn coalesce_pointer(&mut self, mut pointer: PointerEvent) -> PointerEvent {
        let mut events_coalesced = 1;
        while events_coalesced < POINTER_EVENTS_COALESCE_MAX
            && matches!(
                pointer.action(),
                PointerAction::Motion | PointerAction::Wheel(_)
            )
            && match pointer.action() {
                PointerAction::Wheel(wheel) => wheel.ticks() < POINTER_EVENTS_COALESCE_MAX,
                PointerAction::Motion => true,
                _ => false,
            }
        {
            // Poll with this task's waker. EventStream keeps one wake task active,
            // so a no-op waker here can prevent the following read from waking.
            let Some(next) = poll_fn(|context| {
                Poll::Ready(match Pin::new(&mut self.events).poll_next(context) {
                    Poll::Ready(next) => Some(next),
                    Poll::Pending => None,
                })
            })
            .await
            else {
                break;
            };
            let Some(next) = next else {
                break;
            };
            let Ok(raw) = next else {
                self.pending = Some(next);
                break;
            };
            match TerminalEvent::from_crossterm(raw.clone()) {
                Ok(TerminalEvent::Pointer(next_pointer))
                    if can_merge_pointer(pointer, next_pointer) =>
                {
                    merge_pointer(&mut pointer, next_pointer);
                    events_coalesced += 1;
                }
                _ => {
                    self.pending = Some(Ok(raw));
                    break;
                }
            }
        }
        pointer
    }

    /// Returns the next normalized terminal event.
    ///
    /// The function returns `None` after the stream ends. A rejection that
    /// still names input becomes [`TerminalEvent::Unsupported`], so a key with
    /// an unsupported modifier never degrades into the binding of the
    /// unmodified key, and an over-long paste block never inserts a part of
    /// itself. A Shift-modified pointer event is skipped so the terminal owns
    /// native selection. An empty paste block or key release names no input
    /// and is skipped.
    ///
    /// The function coalesces immediately-ready compatible motions or wheel
    /// events. It retains the first incompatible event for the next call.
    ///
    /// The function skips up to [`UNMAPPED_EVENT_SKIP_MAX`] such events and
    /// then reports [`TerminalError::UnmappedEventBurst`]. The source stays
    /// usable after either error, so the caller may read again.
    pub async fn next_event(&mut self) -> Option<Result<TerminalEvent, TerminalError>> {
        for _ in 0..UNMAPPED_EVENT_SKIP_MAX {
            let event = match self.next_raw().await? {
                Ok(event) => event,
                Err(error) => return Some(Err(TerminalError::Read(error))),
            };
            match TerminalEvent::from_crossterm(event) {
                Ok(TerminalEvent::Pointer(pointer)) => {
                    return Some(Ok(TerminalEvent::Pointer(
                        self.coalesce_pointer(pointer).await,
                    )));
                }
                Ok(normalized) => return Some(Ok(normalized)),
                // A key release is the other half of a press that already
                // reported its input. `REPORT_EVENT_TYPES` makes a terminal
                // send one after every key, so reporting it as unsupported
                // would clear the pending sequence between the two presses of
                // every multi-key binding.
                Err(
                    EventRejection::Key(KeyRejection::Release)
                    | EventRejection::Focus
                    | EventRejection::MouseShift
                    | EventRejection::Paste(PasteError::Empty),
                ) => continue,
                Err(EventRejection::Key(_) | EventRejection::Paste(PasteError::TooLong { .. })) => {
                    return Some(Ok(TerminalEvent::Unsupported));
                }
            }
        }
        Some(Err(TerminalError::UnmappedEventBurst))
    }
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
