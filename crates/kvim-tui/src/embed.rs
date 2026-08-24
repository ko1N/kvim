//! The embedding contract of one editor instance.
//!
//! A host owns the worktree set, the surfaces, the terminal, and the
//! asynchronous runtime. It composes one [`Session`] as the visible state of
//! one editor and decides the effect of every fact that the editor publishes.
//! See `docs/embedding.md`.
//!
//! [`EditorAccess`] names what the host granted. [`EditorAccess::ViewOnly`]
//! refuses every text change, every save, every format, and every workspace
//! mutation before that change reaches the buffer or the filesystem.
//!
//! [`EditorEvent`] names one fact or one request. The bounded [`EditorOutbox`]
//! reserves one slot before a durable operation starts, so a completed write
//! and a completed workspace mutation always own the slot of their mandatory
//! event. A full queue refuses the operation before its side effect and never
//! drops a published event.
//!
//! [`Session`]: super::session::Session

use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::layout::{Position, Rect};
use thiserror::Error;

use kvim_path::WorktreeRelativePath;
use kvim_ui::Direction;
use kvim_workspace::FileOperation;

/// The largest number of editor facts that one instance queues at a time.
///
/// A reserved slot counts against this bound, so a saturated outbox refuses
/// the next durable operation instead of losing its mandatory event.
pub const EDITOR_EVENTS_MAX: usize = 64;

/// The next instance number that [`EditorInstanceId::allocate`] hands out.
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

/// The identity of one editor instance.
///
/// Every published fact carries this identity, so a host that runs several
/// editors on one root or on different roots can route each fact back to the
/// editor that produced it. See `docs/embedding.md`.
///
/// # Examples
///
/// ```
/// use kvim_tui::EditorInstanceId;
///
/// let first = EditorInstanceId::allocate();
/// let second = EditorInstanceId::allocate();
/// assert_ne!(first, second);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EditorInstanceId(NonZeroU64);

impl EditorInstanceId {
    /// Returns one identity that no earlier call of this process returned.
    ///
    /// The counter starts at one and only grows, so two live editors never
    /// share an identity.
    ///
    /// # Panics
    ///
    /// Panics after the counter passed every value of a 64-bit number. One
    /// allocation for every nanosecond of six hundred years reaches that
    /// point, so the panic reports a corrupted counter instead.
    #[must_use]
    pub fn allocate() -> Self {
        let value = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
        let value = NonZeroU64::new(value).expect("the instance counter starts at one and grows");
        Self(value)
    }

    /// Returns the instance number.
    ///
    /// ```
    /// use kvim_tui::EditorInstanceId;
    ///
    /// assert!(EditorInstanceId::allocate().get() >= 1);
    /// ```
    #[inline]
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// What the host granted one editor instance.
///
/// The access is a property of the instance. A host can show one worktree in
/// two editors and grant write access to one of them only.
///
/// # Examples
///
/// ```
/// use kvim_tui::EditorAccess;
///
/// assert_eq!(EditorAccess::default(), EditorAccess::ReadWrite);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EditorAccess {
    /// Normal editing and bounded workspace writes.
    #[default]
    ReadWrite,
    /// Reading only. Every text change, save, format, and workspace mutation
    /// is refused before it reaches the buffer or the filesystem.
    ViewOnly,
}

/// Why the editor refused one input.
///
/// # Examples
///
/// ```
/// use kvim_tui::Refusal;
///
/// assert_eq!(Refusal::ViewOnly.note(), "the host granted read-only access");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The host granted [`EditorAccess::ViewOnly`].
    ViewOnly,
    /// The bounded event queue holds no slot for the mandatory event of the
    /// operation, so the editor refused it before its side effect started.
    Saturated,
}

impl Refusal {
    /// Returns the report that the message line shows.
    #[inline]
    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::ViewOnly => "the host granted read-only access",
            Self::Saturated => "the editor event queue is full; read the events first",
        }
    }
}

/// The bounded event queue of one instance holds no free slot.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("the editor event queue is full")]
pub struct Saturated;

/// One request that a synchronous input reduction hands to the host.
///
/// The host owns focus policy and the surface lifetime, so the editor names
/// the boundary that the input reached and changes nothing else. See
/// `docs/embedding.md`.
///
/// # Examples
///
/// ```
/// use kvim_tui::{Direction, EditorEvent, InputRequest};
///
/// let request = InputRequest::FocusBoundary(Direction::Left);
/// assert_eq!(request.event(), EditorEvent::FocusBoundary(Direction::Left));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputRequest {
    /// A focus move reached the outer edge of this editor.
    FocusBoundary(Direction),
    /// The editor closed its last window and asks the host to close it.
    CloseRequested,
}

impl InputRequest {
    /// Returns the request as one editor event.
    ///
    /// A host that keeps one uniform event stream converts the synchronous
    /// request with this method.
    #[inline]
    #[must_use]
    pub const fn event(self) -> EditorEvent {
        match self {
            Self::FocusBoundary(direction) => EditorEvent::FocusBoundary(direction),
            Self::CloseRequested => EditorEvent::CloseRequested,
        }
    }
}

/// What one input reduction produced.
///
/// A refused input performs no side effect, so it names no request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReductionOutcome {
    /// The editor applied the input and asks the host for nothing.
    Applied,
    /// The editor applied the input and asks the host to act.
    Request(InputRequest),
    /// The editor refused the input and changed no durable state.
    Refused(Refusal),
}

/// The complete answer of one synchronous input reduction.
///
/// The redraw request and every durable fact leave the editor through
/// [`Session::take_event`], so this value names only the instance and the one
/// outcome of the input.
///
/// [`Session::take_event`]: super::session::Session::take_event
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reduction {
    /// The editor that reduced the input.
    pub instance: EditorInstanceId,
    /// What the input produced.
    pub outcome: ReductionOutcome,
}

impl Reduction {
    /// Returns the request that the host must answer, if the input named one.
    #[inline]
    #[must_use]
    pub const fn request(&self) -> Option<InputRequest> {
        match self.outcome {
            ReductionOutcome::Request(request) => Some(request),
            ReductionOutcome::Applied | ReductionOutcome::Refused(_) => None,
        }
    }

    /// Returns the reason that the editor refused the input, if it refused it.
    #[inline]
    #[must_use]
    pub const fn refusal(&self) -> Option<Refusal> {
        match self.outcome {
            ReductionOutcome::Refused(refusal) => Some(refusal),
            ReductionOutcome::Applied | ReductionOutcome::Request(_) => None,
        }
    }
}

/// One fact or one request of an editor instance.
///
/// The host decides the effect of every event. kvim assigns no host meaning to
/// a written file or to a changed workspace. A review surface publishes its own
/// typed `ReviewEvent` values, which stay separate from this enumeration. See
/// `docs/embedding.md`.
///
/// # Examples
///
/// ```
/// use kvim_tui::{Direction, EditorEvent};
///
/// let event = EditorEvent::FocusBoundary(Direction::Right);
/// assert_eq!(event, EditorEvent::FocusBoundary(Direction::Right));
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditorEvent {
    /// The editor shows another file, or a buffer without a file.
    ActiveFileChanged {
        /// The contained path of the new active file, or `None` for a buffer
        /// that no file backs.
        path: Option<WorktreeRelativePath>,
    },
    /// One buffer reached the filesystem as a completed write.
    FileWritten {
        /// The contained path that the write produced.
        path: WorktreeRelativePath,
    },
    /// One workspace mutation completed.
    WorkspaceChanged {
        /// The operation that the workspace performed.
        operation: FileOperation,
    },
    /// The visible state changed and the host must draw one frame.
    ///
    /// The outbox coalesces this request, so a burst of changes publishes it
    /// once.
    RedrawRequested,
    /// A focus move reached the outer edge of this editor.
    FocusBoundary(Direction),
    /// The editor closed its last window and asks the host to close it.
    CloseRequested,
}

/// One event and the identity of the editor that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedEvent {
    /// The editor that produced the event.
    pub instance: EditorInstanceId,
    /// The fact or the request.
    pub event: EditorEvent,
}

/// One outbox slot that a durable operation owns until it publishes or fails.
///
/// The token exists only while the operation runs, so a completed write can
/// never find the queue full. See `docs/embedding.md`.
#[must_use = "a reserved slot must publish its event or release the slot"]
#[derive(Debug)]
pub(super) struct EventReservation {
    instance: EditorInstanceId,
}

/// The bounded event queue of one editor instance.
///
/// The queue holds the mandatory facts of the durable operations: one
/// completed write and one completed workspace mutation. Every such operation
/// reserves its slot before the side effect starts.
///
/// The redraw request and the active file are coalesced latches beside the
/// queue. Both report the current state instead of a history, so a burst
/// consumes no slot and can never saturate the queue.
#[derive(Debug)]
pub(super) struct EditorOutbox {
    instance: EditorInstanceId,
    queued: VecDeque<EditorEvent>,
    /// The number of slots that running durable operations hold.
    reserved: usize,
    /// Reports whether the host still owes one frame.
    redraw: bool,
    /// The active file that the host has not read yet.
    ///
    /// The outer option reports whether a change waits. The inner option is
    /// the path of the active buffer, which a scratch buffer leaves empty.
    active_file: Option<Option<WorktreeRelativePath>>,
}

impl EditorOutbox {
    /// Creates the empty outbox of one instance.
    pub(super) fn new(instance: EditorInstanceId) -> Self {
        Self {
            instance,
            queued: VecDeque::new(),
            reserved: 0,
            redraw: false,
            active_file: None,
        }
    }

    /// Returns the number of slots that neither a queued event nor a running
    /// operation holds.
    fn free(&self) -> usize {
        EDITOR_EVENTS_MAX
            .saturating_sub(self.queued.len())
            .saturating_sub(self.reserved)
    }

    /// Reports whether one durable operation can still reserve a slot.
    pub(super) fn has_free_slot(&self) -> bool {
        self.free() > 0
    }

    /// Takes one slot for a durable operation that has not started yet.
    ///
    /// # Errors
    ///
    /// Returns [`Saturated`] while the queue and the running operations hold
    /// every slot. The caller must then refuse its operation before it starts
    /// the side effect.
    pub(super) fn reserve(&mut self) -> Result<EventReservation, Saturated> {
        if self.free() == 0 {
            return Err(Saturated);
        }
        self.reserved += 1;
        Ok(EventReservation {
            instance: self.instance,
        })
    }

    /// Publishes the mandatory event of one committed operation.
    ///
    /// The call cannot fail, because the operation reserved its slot before it
    /// started.
    pub(super) fn commit(&mut self, reservation: EventReservation, event: EditorEvent) {
        debug_assert_eq!(
            reservation.instance, self.instance,
            "a reservation belongs to the outbox that created it"
        );
        debug_assert!(
            self.reserved > 0,
            "every live reservation counts against the bound until it commits"
        );
        self.reserved = self.reserved.saturating_sub(1);
        debug_assert!(
            self.queued.len() < EDITOR_EVENTS_MAX,
            "a reserved slot keeps room for the event of its operation"
        );
        self.queued.push_back(event);
    }

    /// Releases the slot of one operation that produced no durable change.
    pub(super) fn release(&mut self, reservation: EventReservation) {
        debug_assert_eq!(
            reservation.instance, self.instance,
            "a reservation belongs to the outbox that created it"
        );
        debug_assert!(
            self.reserved > 0,
            "every live reservation counts against the bound until it is released"
        );
        self.reserved = self.reserved.saturating_sub(1);
    }

    /// Latches the request for one frame.
    pub(super) const fn request_redraw(&mut self) {
        self.redraw = true;
    }

    /// Latches the file that the editor now shows.
    ///
    /// The latch reports the current active file, so a second change replaces
    /// the first one instead of taking a slot of the bounded queue.
    pub(super) fn note_active_file(&mut self, path: Option<WorktreeRelativePath>) {
        self.active_file = Some(path);
    }

    /// Takes the next event of this instance.
    ///
    /// The mandatory facts leave first, in the order that they committed. The
    /// two latches follow, because each one reports the current state and not
    /// a history.
    pub(super) fn take(&mut self) -> Option<PublishedEvent> {
        let event = match self.queued.pop_front() {
            Some(event) => event,
            None => match self.active_file.take() {
                Some(path) => EditorEvent::ActiveFileChanged { path },
                None if self.redraw => {
                    self.redraw = false;
                    EditorEvent::RedrawRequested
                }
                None => return None,
            },
        };
        Some(PublishedEvent {
            instance: self.instance,
            event,
        })
    }
}

/// The cursor shape that one editor mode asks for.
///
/// The host owns the terminal, so it decides whether to apply the request.
///
/// # Examples
///
/// ```
/// use kvim_tui::CursorShape;
///
/// assert_ne!(CursorShape::Bar, CursorShape::Block);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CursorShape {
    /// A block over the character under the cursor.
    Block,
    /// A vertical bar before the character under the cursor.
    Bar,
}

/// The cursor that one frame asks the host to show.
///
/// A frame without a visible cursor names a position of `None` and still names
/// the shape of the current mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CursorRequest {
    /// The cell of the cursor inside the supplied buffer.
    pub position: Option<Position>,
    /// The shape that the mode asks for.
    pub shape: CursorShape,
}

/// A rectangle that no editor can use.
///
/// Every variant names the rectangle that the caller supplied, so a host can
/// report the exact geometry that it must correct.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GeometryError {
    /// The rectangle holds no cell.
    #[error("the editor rectangle {area:?} holds no cell")]
    Empty {
        /// The rectangle that the caller supplied.
        area: Rect,
    },
    /// The rectangle leaves the supplied cell buffer.
    #[error("the editor rectangle {area:?} leaves the buffer rectangle {buffer:?}")]
    OutsideBuffer {
        /// The rectangle that the caller supplied.
        area: Rect,
        /// The rectangle of the supplied cell buffer.
        buffer: Rect,
    },
    /// The rectangle differs from the area that the editor last accepted.
    ///
    /// The layout, the viewports, and the cursor all follow the accepted area,
    /// so a frame over another rectangle would report the wrong cursor cell.
    #[error("the editor rectangle {area:?} differs from the accepted rectangle {accepted:?}")]
    Unreconciled {
        /// The rectangle that the caller supplied.
        area: Rect,
        /// The rectangle that the editor accepted last.
        accepted: Rect,
    },
}

/// Reports whether the buffer rectangle holds the complete area.
pub(super) fn fits(area: Rect, buffer: Rect) -> bool {
    area.x >= buffer.x
        && area.y >= buffer.y
        && u32::from(area.x) + u32::from(area.width)
            <= u32::from(buffer.x) + u32::from(buffer.width)
        && u32::from(area.y) + u32::from(area.height)
            <= u32::from(buffer.y) + u32::from(buffer.height)
}
