//! Normalized terminal events and the bounded terminal event source.

use std::io;

use crossterm::event::{Event as CrosstermEvent, EventStream};
use futures_util::stream::{Stream, StreamExt};

use super::{Key, TerminalError};

/// The maximum number of consecutive terminal events without a normalized form
/// that one read attempt skips. The bound keeps a read attempt finite when a
/// terminal sends unsupported events continuously.
pub const UNMAPPED_EVENT_SKIP_MAX: usize = 64;

/// The terminal focus transition that the terminal reported.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FocusChange {
    /// The terminal window became the focused window.
    Gained,
    /// The terminal window lost the focus.
    Lost,
}

/// One normalized terminal event.
///
/// The event loop consumes these values. It never inspects a crossterm event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TerminalEvent {
    /// A key press or a key repeat.
    Key(Key),
    /// The terminal window changed size.
    Resize {
        /// The new terminal width in cells.
        columns: u16,
        /// The new terminal height in cells.
        rows: u16,
    },
    /// The terminal window gained or lost the focus.
    Focus(FocusChange),
}

impl TerminalEvent {
    /// Normalizes one crossterm event.
    ///
    /// The function returns `None` for a mouse event, a paste event, a key
    /// release, and any key that Kvim does not use.
    ///
    /// ```
    /// use crossterm::event::Event;
    /// use kvim_terminal::TerminalEvent;
    ///
    /// let resize = Event::Resize(120, 40);
    /// assert_eq!(
    ///     TerminalEvent::from_crossterm(resize),
    ///     Some(TerminalEvent::Resize { columns: 120, rows: 40 }),
    /// );
    /// ```
    pub fn from_crossterm(event: CrosstermEvent) -> Option<Self> {
        match event {
            CrosstermEvent::Key(key) => Key::from_key_event(key).map(Self::Key),
            CrosstermEvent::Resize(columns, rows) => Some(Self::Resize { columns, rows }),
            CrosstermEvent::FocusGained => Some(Self::Focus(FocusChange::Gained)),
            CrosstermEvent::FocusLost => Some(Self::Focus(FocusChange::Lost)),
            CrosstermEvent::Mouse(_) | CrosstermEvent::Paste(_) => None,
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
    /// The function returns `None` after the stream ends. It skips an event
    /// without a normalized form, up to [`UNMAPPED_EVENT_SKIP_MAX`] events, and
    /// then reports [`TerminalError::UnmappedEventBurst`]. The source stays
    /// usable after either error, so the caller may read again.
    pub async fn next_event(&mut self) -> Option<Result<TerminalEvent, TerminalError>> {
        for _ in 0..UNMAPPED_EVENT_SKIP_MAX {
            let event = match self.events.next().await? {
                Ok(event) => event,
                Err(error) => return Some(Err(TerminalError::Read(error))),
            };
            if let Some(normalized) = TerminalEvent::from_crossterm(event) {
                return Some(Ok(normalized));
            }
        }
        Some(Err(TerminalError::UnmappedEventBurst))
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode as CrosstermKeyCode, KeyEvent, KeyModifiers, MouseEvent};
    use futures_util::stream;

    use super::*;
    use crate::KeyCode;

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
    async fn the_source_skips_events_without_a_normalized_form() {
        let key = KeyEvent::new(CrosstermKeyCode::Esc, KeyModifiers::NONE);
        let mut source = source(vec![
            mouse_event(),
            CrosstermEvent::Paste("text".to_owned()),
            CrosstermEvent::Key(key),
        ]);

        let event = source.next_event().await;

        assert!(matches!(
            event,
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
}
