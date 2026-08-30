//! Normalized terminal events and the bounded terminal event source.

use std::io;

use crossterm::event::{
    Event as CrosstermEvent, EventStream, KeyModifiers, MouseButton as CrosstermMouseButton,
    MouseEvent, MouseEventKind,
};
use futures_util::FutureExt;
use futures_util::stream::{Stream, StreamExt};

use kvim_keymap::{Key, PasteError, PasteText};
use thiserror::Error;

use super::TerminalError;
use crate::key::{KeyRejection, normalize_key_event};

/// The maximum number of consecutive terminal events without a normalized form
/// that one read attempt skips. The bound keeps a read attempt finite when a
/// terminal sends unsupported events continuously.
pub const UNMAPPED_EVENT_SKIP_MAX: usize = 64;
/// The maximum number of immediately-ready pointer motion or wheel events that
/// one returned event may coalesce.
pub const POINTER_EVENTS_COALESCE_MAX: u8 = 32;

/// A terminal cell position.
///
/// This position is distinct from a source-text character or byte position.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CellPosition {
    column: u16,
    row: u16,
}

impl CellPosition {
    /// Creates a position from zero-based terminal cell coordinates.
    #[must_use]
    pub const fn new(column: u16, row: u16) -> Self {
        Self { column, row }
    }

    /// Returns the zero-based terminal cell column.
    #[must_use]
    pub const fn column(self) -> u16 {
        self.column
    }

    /// Returns the zero-based terminal cell row.
    #[must_use]
    pub const fn row(self) -> u16 {
        self.row
    }
}

/// Non-Shift modifiers reported with a pointer event.
///
/// Shift-modified pointer input is omitted before it reaches this type so the
/// terminal emulator can own its native selection behavior.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PointerModifiers {
    control: bool,
    alt: bool,
    super_key: bool,
}

impl PointerModifiers {
    /// Creates modifiers from terminal-neutral modifier states.
    #[must_use]
    pub const fn new(control: bool, alt: bool, super_key: bool) -> Self {
        Self {
            control,
            alt,
            super_key,
        }
    }

    const fn from_crossterm(modifiers: KeyModifiers) -> Self {
        Self::new(
            modifiers.contains(KeyModifiers::CONTROL),
            modifiers.contains(KeyModifiers::ALT),
            modifiers.contains(KeyModifiers::SUPER),
        )
    }

    /// Returns whether Control was held.
    #[must_use]
    pub const fn control(self) -> bool {
        self.control
    }

    /// Returns whether Alt was held.
    #[must_use]
    pub const fn alt(self) -> bool {
        self.alt
    }

    /// Returns whether Super was held.
    #[must_use]
    pub const fn super_key(self) -> bool {
        self.super_key
    }
}

/// A pointer button with a supported terminal-neutral identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PointerButton {
    /// The primary button.
    Left,
    /// The secondary button.
    Right,
    /// The middle button.
    Middle,
}

/// A wheel direction in terminal-cell coordinates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PointerWheelDirection {
    /// Scroll toward smaller row values.
    Up,
    /// Scroll toward larger row values.
    Down,
    /// Scroll toward smaller column values.
    Left,
    /// Scroll toward larger column values.
    Right,
}

/// The reason a wheel tick count is invalid.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PointerWheelError {
    /// The wheel action carries no raw tick.
    #[error("the pointer wheel action must contain at least one tick")]
    ZeroTicks,
    /// The wheel action exceeds the published coalescing bound.
    #[error(
        "the pointer wheel action has {ticks} ticks, above the maximum of {POINTER_EVENTS_COALESCE_MAX}"
    )]
    TooManyTicks { ticks: u8 },
}

/// A bounded coalesced wheel action.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PointerWheel {
    direction: PointerWheelDirection,
    ticks: u8,
}

impl PointerWheel {
    /// Creates a wheel action with a bounded nonzero tick count.
    ///
    /// # Errors
    ///
    /// Returns [`PointerWheelError`] when `ticks` is zero or above
    /// [`POINTER_EVENTS_COALESCE_MAX`].
    pub const fn new(
        direction: PointerWheelDirection,
        ticks: u8,
    ) -> Result<Self, PointerWheelError> {
        if ticks == 0 {
            return Err(PointerWheelError::ZeroTicks);
        }
        if ticks > POINTER_EVENTS_COALESCE_MAX {
            return Err(PointerWheelError::TooManyTicks { ticks });
        }
        Ok(Self { direction, ticks })
    }

    const fn one(direction: PointerWheelDirection) -> Self {
        Self {
            direction,
            ticks: 1,
        }
    }

    /// Returns the wheel direction.
    #[must_use]
    pub const fn direction(self) -> PointerWheelDirection {
        self.direction
    }

    /// Returns the number of raw wheel ticks, from one through
    /// [`POINTER_EVENTS_COALESCE_MAX`].
    #[must_use]
    pub const fn ticks(self) -> u8 {
        self.ticks
    }

    fn can_merge(self, other: Self) -> bool {
        self.direction == other.direction && other.ticks <= POINTER_EVENTS_COALESCE_MAX - self.ticks
    }

    fn merge(&mut self, other: Self) {
        debug_assert!(
            self.can_merge(other),
            "the event source merges only equal wheel directions below the published coalescing limit"
        );
        self.ticks += other.ticks;
    }
}

/// One terminal-neutral pointer action.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PointerAction {
    /// A button press.
    Press(PointerButton),
    /// A button release.
    Release(PointerButton),
    /// A pointer movement while a button is held.
    Drag(PointerButton),
    /// A pointer movement without a reported button.
    Motion,
    /// One or more bounded wheel ticks.
    Wheel(PointerWheel),
}

/// One terminal-neutral pointer event.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PointerEvent {
    position: CellPosition,
    modifiers: PointerModifiers,
    action: PointerAction,
}

impl PointerEvent {
    /// Creates a terminal-neutral pointer event.
    #[must_use]
    pub const fn new(
        position: CellPosition,
        modifiers: PointerModifiers,
        action: PointerAction,
    ) -> Self {
        Self {
            position,
            modifiers,
            action,
        }
    }

    fn from_crossterm(event: MouseEvent) -> Result<Self, EventRejection> {
        if event.modifiers.contains(KeyModifiers::SHIFT) {
            return Err(EventRejection::MouseShift);
        }
        let action = match event.kind {
            MouseEventKind::Down(button) => PointerAction::Press(pointer_button(button)),
            MouseEventKind::Up(button) => PointerAction::Release(pointer_button(button)),
            MouseEventKind::Drag(button) => PointerAction::Drag(pointer_button(button)),
            MouseEventKind::Moved => PointerAction::Motion,
            MouseEventKind::ScrollUp => {
                PointerAction::Wheel(PointerWheel::one(PointerWheelDirection::Up))
            }
            MouseEventKind::ScrollDown => {
                PointerAction::Wheel(PointerWheel::one(PointerWheelDirection::Down))
            }
            MouseEventKind::ScrollLeft => {
                PointerAction::Wheel(PointerWheel::one(PointerWheelDirection::Left))
            }
            MouseEventKind::ScrollRight => {
                PointerAction::Wheel(PointerWheel::one(PointerWheelDirection::Right))
            }
        };
        Ok(Self::new(
            CellPosition::new(event.column, event.row),
            PointerModifiers::from_crossterm(event.modifiers),
            action,
        ))
    }

    /// Returns the reported terminal cell position.
    #[must_use]
    pub const fn position(self) -> CellPosition {
        self.position
    }

    /// Returns the non-Shift modifiers.
    #[must_use]
    pub const fn modifiers(self) -> PointerModifiers {
        self.modifiers
    }

    /// Returns the pointer action.
    #[must_use]
    pub const fn action(self) -> PointerAction {
        self.action
    }

    fn can_merge(self, other: Self) -> bool {
        match (self.action, other.action) {
            (PointerAction::Motion, PointerAction::Motion) => self.modifiers == other.modifiers,
            (PointerAction::Wheel(left), PointerAction::Wheel(right)) => {
                self.position == other.position
                    && self.modifiers == other.modifiers
                    && left.can_merge(right)
            }
            _ => false,
        }
    }

    fn merge(&mut self, other: Self) {
        debug_assert!(
            self.can_merge(other),
            "the event source merges only consecutive motions or equal wheel directions"
        );
        match (&mut self.action, other.action) {
            (PointerAction::Motion, PointerAction::Motion) => *self = other,
            (PointerAction::Wheel(left), PointerAction::Wheel(right)) => left.merge(right),
            _ => unreachable!("PointerEvent::can_merge validated the pointer action pair"),
        }
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
            CrosstermEvent::Mouse(mouse) => Ok(Self::Pointer(PointerEvent::from_crossterm(mouse)?)),
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

    fn coalesce_pointer(&mut self, mut pointer: PointerEvent) -> PointerEvent {
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
            let Some(next) = self.events.next().now_or_never() else {
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
                Ok(TerminalEvent::Pointer(next_pointer)) if pointer.can_merge(next_pointer) => {
                    pointer.merge(next_pointer);
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
                    return Some(Ok(TerminalEvent::Pointer(self.coalesce_pointer(pointer))));
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
