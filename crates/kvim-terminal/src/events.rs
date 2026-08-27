//! Normalized terminal events and the bounded terminal event source.

use std::io;

use crossterm::event::{Event as CrosstermEvent, EventStream};
use futures_util::stream::{Stream, StreamExt};

use kvim_keymap::{Key, PasteError, PasteText};
use thiserror::Error;

use super::TerminalError;
use crate::key::{KeyRejection, normalize_key_event};

/// The maximum number of consecutive terminal events without a normalized form
/// that one read attempt skips. The bound keeps a read attempt finite when a
/// terminal sends unsupported events continuously.
pub const UNMAPPED_EVENT_SKIP_MAX: usize = 64;

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
    /// A mouse event carries no normalized form.
    #[error("the terminal reported a mouse event, which kvim does not read")]
    Mouse,
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
    /// Returns [`EventRejection`] for a mouse event, for a paste block that
    /// carries no bounded input, and for every key event without a normalized
    /// key. [`EventSource`] turns the rejections that still name input into
    /// [`TerminalEvent::Unsupported`].
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
            CrosstermEvent::Mouse(_) => Err(EventRejection::Mouse),
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
        }
    }
}

impl<S> EventSource<S>
where
    S: Stream<Item = io::Result<CrosstermEvent>> + Unpin,
{
    /// Reads events from the supplied crossterm event stream.
    pub const fn new(events: S) -> Self {
        Self { events }
    }

    /// Returns the next normalized terminal event.
    ///
    /// The function returns `None` after the stream ends. A rejection that
    /// still names input becomes [`TerminalEvent::Unsupported`], so a key with
    /// an unsupported modifier never degrades into the binding of the
    /// unmodified key, and an over-long paste block never inserts a part of
    /// itself. A rejection that names no input at all, such as a mouse event,
    /// an empty paste block, or a key release, carries nothing to report and
    /// is skipped.
    ///
    /// The function skips up to [`UNMAPPED_EVENT_SKIP_MAX`] such events and
    /// then reports [`TerminalError::UnmappedEventBurst`]. The source stays
    /// usable after either error, so the caller may read again.
    pub async fn next_event(&mut self) -> Option<Result<TerminalEvent, TerminalError>> {
        for _ in 0..UNMAPPED_EVENT_SKIP_MAX {
            let event = match self.events.next().await? {
                Ok(event) => event,
                Err(error) => return Some(Err(TerminalError::Read(error))),
            };
            match TerminalEvent::from_crossterm(event) {
                Ok(normalized) => return Some(Ok(normalized)),
                // A key release is the other half of a press that already
                // reported its input, so it names no input of its own.
                // `REPORT_EVENT_TYPES` makes a terminal send one after every
                // key, so reporting it as unsupported would clear the pending
                // sequence between the two presses of every multi-key binding.
                Err(
                    EventRejection::Key(KeyRejection::Release)
                    | EventRejection::Focus
                    | EventRejection::Mouse
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
