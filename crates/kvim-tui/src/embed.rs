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
//! [`EmbeddedEditor`] is the facade of this contract. It builds the model and
//! the driver of one instance together, so a host names one root, one
//! rectangle, and one named [`EditorCapacity`] and gets one independent
//! editor. `crates/kvim-embed/examples/worktree_editor.rs` is the complete host
//! of one such editor: it owns the input, the cell buffer, the spawner, the
//! task supervision, and the cancellation.
//!
//! [`Session`]: super::session::Session

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroU32;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect};
use thiserror::Error;

use kvim_input::{BindingScope, Command, InputContextSnapshot, Mode, PasteText};
use kvim_language::LanguageServices;
use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{EventReceiver, FileWatcher, Runtime, RuntimeLimits};
use kvim_settings::{EditorSettings, SettingsError};
use kvim_terminal::TerminalEvent;
use kvim_ui::Direction;
use kvim_workspace::FileOperation;

use super::clipboard::ClipboardAccess;
use super::driver::{Completed, DriverApplyError, EditorDriver, EditorWork, ShutdownDrain};
use super::file_sidebar::{FileRow, FileSidebarInput, FileSidebarOutcome};
use super::session::{Redraw, RunState, Session};

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
/// use kvim_tui::__private::EditorInstanceId;
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
    /// use kvim_tui::__private::EditorInstanceId;
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
/// use kvim_tui::__private::EditorAccess;
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
/// use kvim_tui::__private::Refusal;
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
/// use kvim_tui::__private::{Direction, EditorEvent, InputRequest};
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
/// use kvim_tui::__private::{Direction, EditorEvent};
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
    /// One save reached a point where its durable state needs reconciliation.
    SaveReconciliationRequired {
        /// The contained path that a reload must check.
        path: WorktreeRelativePath,
    },
    /// One workspace mutation reached an uncertain durable state.
    WorkspaceReconciliationRequired {
        /// The operation whose affected paths need reconciliation.
        operation: FileOperation,
    },
    /// The reader activated one file of the file sidebar.
    ///
    /// The editor opened no buffer. The fact reaches the host through the
    /// [`FileSidebarOutcome`] of the input that produced it, and
    /// [`FileSidebarOutcome::event`] converts it for a host that keeps one
    /// uniform event stream.
    ///
    /// [`FileSidebarOutcome`]: super::file_sidebar::FileSidebarOutcome
    /// [`FileSidebarOutcome::event`]: super::file_sidebar::FileSidebarOutcome::event
    FileActivated {
        /// The contained path of the activated file.
        path: WorktreeRelativePath,
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
        assert_eq!(
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
        assert_eq!(
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
/// use kvim_tui::__private::CursorShape;
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

/// One embedded editor could not be constructed.
#[derive(Debug, Error)]
pub enum EditorOpenError {
    /// The supplied settings are invalid.
    #[error("invalid editor settings")]
    Settings(#[from] SettingsError),
    /// The editor rectangle is invalid.
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    /// The language service root differs from the editor root.
    #[error("the language service root {language:?} differs from the editor root {editor:?}")]
    LanguageRootMismatch {
        /// The editor worktree root.
        editor: PathBuf,
        /// The language service root.
        language: PathBuf,
    },
}

/// A completed value belongs to another editor instance.
#[derive(Debug)]
pub struct EditorApplyError {
    source: DriverApplyError,
}

impl EditorApplyError {
    /// Recovers the unapplied completion for routing to its owner.
    pub fn into_completed(self) -> Completed {
        self.source.into_completed()
    }
}

impl fmt::Display for EditorApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the completion belongs to another editor instance")
    }
}

impl std::error::Error for EditorApplyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<DriverApplyError> for EditorApplyError {
    fn from(source: DriverApplyError) -> Self {
        Self { source }
    }
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

/// The largest number of events that one complete drain returns.
///
/// The bounded queue holds every mandatory fact, and the two coalesced latches
/// sit beside it, so this bound covers every event that one editor can still
/// hold.
const DRAINED_EVENTS_MAX: usize = EDITOR_EVENTS_MAX + 2;

/// Takes every event that one editor still holds.
///
/// The bound above covers the queue and both latches, so the loop always ends.
fn drain_published(editor: &mut Session) -> Vec<PublishedEvent> {
    let mut events = Vec::new();
    for _ in 0..DRAINED_EVENTS_MAX {
        let Some(event) = editor.take_event() else {
            return events;
        };
        events.push(event);
    }
    debug_assert!(
        false,
        "the bounded outbox and its two latches hold at most DRAINED_EVENTS_MAX events"
    );
    events
}

/// Where one embedded editor takes its background capacity from.
///
/// Capacity belongs to one instance unless this value names a shared pool, so
/// a saturated editor consumes no worker permit, no result slot, and no
/// cancellation namespace of another editor. See `docs/embedding.md`.
///
/// # Examples
///
/// ```
/// use kvim_runtime::RuntimeLimits;
/// use kvim_tui::__private::EditorCapacity;
///
/// let limits = RuntimeLimits::new(32, 2, 2).expect("every capacity is nonzero");
/// assert!(matches!(
///     EditorCapacity::Isolated(limits),
///     EditorCapacity::Isolated(_)
/// ));
/// assert!(matches!(
///     EditorCapacity::default(),
///     EditorCapacity::SharedProcessPool
/// ));
/// ```
#[derive(Default)]
pub enum EditorCapacity {
    /// The editor owns its worker permits and its result queue, and it shares
    /// the one external-process pool of this program.
    ///
    /// A second editor of this kind adds no process capacity, so a program
    /// that runs many editors keeps one bound on its child processes.
    #[default]
    SharedProcessPool,
    /// The editor owns every permit and its result queue alone.
    ///
    /// Use this choice for an editor that must not wait for the processes of
    /// another editor.
    ///
    /// Give the limits more than two worker permits. One directory read and
    /// one buffer analysis hold two permits together, and a save that finds no
    /// free permit returns a saturation refusal instead of writing the file.
    Isolated(RuntimeLimits),
    /// The host built the spawner, so the host chose the capacity.
    Supplied {
        /// The bounded spawner that every request of this editor leaves
        /// through.
        spawner: Runtime<EditorWork>,
        /// The result stream of that spawner.
        results: EventReceiver<EditorWork>,
    },
}

impl fmt::Debug for EditorCapacity {
    /// Names the choice without naming the spawner, which holds no printable
    /// state.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SharedProcessPool => formatter.write_str("SharedProcessPool"),
            Self::Isolated(limits) => formatter.debug_tuple("Isolated").field(limits).finish(),
            Self::Supplied { .. } => formatter.write_str("Supplied"),
        }
    }
}

impl EditorCapacity {
    /// Returns the spawner and the result stream that this choice names.
    fn realize(self) -> (Runtime<EditorWork>, EventReceiver<EditorWork>) {
        match self {
            Self::SharedProcessPool => Runtime::new(),
            Self::Isolated(limits) => Runtime::with_limits(limits),
            Self::Supplied { spawner, results } => (spawner, results),
        }
    }
}

/// The construction of one embedded editor.
///
/// The root and the rectangle are required, because the root bounds every file
/// that the editor reaches and the rectangle bounds every cell that it writes.
/// Every other setting has a default. See `docs/embedding.md`.
///
/// # Examples
///
/// ```
/// use ratatui::layout::Rect;
///
/// use kvim_tui::__private::{EditorAccess, EmbeddedEditor};
///
/// let root = std::sync::Arc::new(
///     kvim_path::WorktreeRoot::open(
///         std::env::current_dir().expect("the process holds a working directory"),
///     )
///     .expect("the working directory is a worktree"),
/// );
/// let editor = EmbeddedEditor::builder(root, Rect::new(0, 0, 80, 24))
///     .access(EditorAccess::ViewOnly)
///     .open()
///     .expect("the rectangle holds cells");
/// assert_eq!(editor.area(), Rect::new(0, 0, 80, 24));
/// ```
pub struct EmbeddedEditorBuilder {
    root: Arc<WorktreeRoot>,
    area: Rect,
    settings: EditorSettings,
    access: EditorAccess,
    clipboard: ClipboardAccess,
    capacity: EditorCapacity,
    language: Option<LanguageServices>,
    watcher: Option<FileWatcher>,
    watcher_unavailable: bool,
    git_status: bool,
}

impl EmbeddedEditorBuilder {
    /// Sets every adjustable behavior of this editor.
    #[must_use]
    pub fn settings(mut self, settings: EditorSettings) -> Self {
        self.settings = settings;
        self
    }

    /// Sets what the host grants this editor.
    #[must_use]
    pub fn access(mut self, access: EditorAccess) -> Self {
        self.access = access;
        self
    }

    /// Sets what this editor may reach of the system clipboard.
    ///
    /// The default is [`ClipboardAccess::None`], so an embedded editor keeps
    /// every yank and every put inside its own registers until the host grants
    /// more. [`ClipboardAccess::System`] performs the platform selection once,
    /// inside [`EmbeddedEditorBuilder::open`]. See `docs/clipboard.md`.
    #[must_use]
    pub fn clipboard(mut self, clipboard: ClipboardAccess) -> Self {
        self.clipboard = clipboard;
        self
    }

    /// Sets where this editor takes its background capacity from.
    #[must_use]
    pub fn capacity(mut self, capacity: EditorCapacity) -> Self {
        self.capacity = capacity;
        self
    }

    /// Adds the language services of this editor.
    ///
    /// The services are optional. An editor without them stays fully usable,
    /// with no diagnostics, no completion, and no external formatter.
    #[must_use]
    pub fn language(mut self, language: LanguageServices) -> Self {
        self.language = Some(language);
        self
    }

    /// Adds the workspace watcher of this editor.
    ///
    /// The watcher is optional. An editor without it stays fully usable, and
    /// the refresh command reads the workspace by hand.
    #[must_use]
    pub fn watcher(mut self, watcher: FileWatcher) -> Self {
        self.watcher = Some(watcher);
        self
    }

    /// Reports that a requested watcher could not start.
    #[doc(hidden)]
    #[must_use]
    pub fn watcher_unavailable(mut self) -> Self {
        self.watcher_unavailable = true;
        self
    }

    /// Sets whether this editor requests Git status.
    ///
    /// The compatibility facade enables Git status by default. A higher-level
    /// facade can disable it until its host grants that optional capability.
    #[doc(hidden)]
    #[must_use]
    pub fn git_status(mut self, enabled: bool) -> Self {
        self.git_status = enabled;
        self
    }

    /// Builds the model and the driver of one independent editor.
    ///
    /// # Errors
    ///
    /// Returns [`EditorOpenError`] when geometry, settings, or optional
    /// language services do not match the editor boundary.
    pub fn open(self) -> Result<EmbeddedEditor, EditorOpenError> {
        let Self {
            root,
            area,
            settings,
            access,
            clipboard,
            capacity,
            language,
            watcher,
            watcher_unavailable,
            git_status,
        } = self;
        if area.width == 0 || area.height == 0 {
            return Err(GeometryError::Empty { area }.into());
        }
        let settings = settings.realize()?;
        if let Some(services) = language.as_ref()
            && services.root() != root.as_path()
        {
            return Err(EditorOpenError::LanguageRootMismatch {
                editor: root.as_path().to_path_buf(),
                language: services.root().to_path_buf(),
            });
        }
        let mut editor = Session::new(area, settings, root)
            .with_access(access)
            .with_clipboard(clipboard)
            .with_git_status(git_status);
        if watcher_unavailable {
            let _ = editor.report_watch_unavailable();
        }
        let (spawner, results) = capacity.realize();
        let mut driver = EditorDriver::new(editor.instance(), spawner, results);
        if let Some(language) = language {
            driver = driver.with_language(language);
        }
        if let Some(watcher) = watcher {
            driver = driver.with_watcher(watcher);
        }
        Ok(EmbeddedEditor { editor, driver })
    }
}

/// One complete editor instance that a host owns.
///
/// The value holds the visible state and the external services of one editor.
/// It owns no terminal, no event loop, and no asynchronous runtime. The host
/// supplies the resolved commands, the literal text, the elapsed time, the
/// rectangle, and the cell buffer, and it decides the effect of every
/// published event. See `docs/embedding.md`.
///
/// The editor runs no second key-sequence resolver.
/// [`EmbeddedEditor::command`] accepts the command that the shared resolver
/// already produced, and [`EmbeddedEditor::insert_literal`] accepts the text
/// fallback of the focused scope.
///
/// `crates/kvim-embed/examples/worktree_editor.rs` is one complete host of one
/// such editor.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use ratatui::buffer::Buffer;
/// use ratatui::layout::Rect;
///
/// use kvim_input::Command;
/// use kvim_tui::__private::{EditorShutdown, EmbeddedEditor};
///
/// # let host_runtime = tokio::runtime::Builder::new_current_thread()
/// #     .enable_all()
/// #     .build()
/// #     .expect("the example builds one runtime");
/// # host_runtime.block_on(async {
/// let root = std::sync::Arc::new(
///     kvim_path::WorktreeRoot::open(
///         std::env::current_dir().expect("the process holds a working directory"),
///     )
///     .expect("the working directory is a worktree"),
/// );
/// let area = Rect::new(0, 0, 80, 24);
/// let mut editor = EmbeddedEditor::builder(root, area)
///     .open()
///     .expect("the rectangle holds cells");
///
/// editor.command(Command::InsertBeforeCursor, None, None, Duration::ZERO);
/// editor.insert_literal("hello", Duration::ZERO);
///
/// let mut cells = Buffer::empty(area);
/// let cursor = editor.draw(&mut cells, area).expect("the rectangle fits");
/// assert!(cursor.position.is_some());
///
/// let shutdown = editor.shutdown(Duration::from_secs(5)).await;
/// assert!(matches!(shutdown, EditorShutdown::Finished { .. }));
/// # });
/// ```
pub struct EmbeddedEditor {
    editor: Session,
    driver: EditorDriver,
}

impl fmt::Debug for EmbeddedEditor {
    /// Names the instance and its rectangle, because the visible state and the
    /// tracked tasks hold no printable form.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmbeddedEditor")
            .field("instance", &self.editor.instance())
            .field("area", &self.editor.area())
            .finish_non_exhaustive()
    }
}

impl EmbeddedEditor {
    /// Starts the construction of one editor over one validated worktree root.
    ///
    /// The root is the containment boundary of every file that this editor
    /// reads, writes, or shows.
    #[must_use]
    pub fn builder(root: Arc<WorktreeRoot>, area: Rect) -> EmbeddedEditorBuilder {
        EmbeddedEditorBuilder {
            root,
            area,
            settings: EditorSettings::default(),
            access: EditorAccess::default(),
            clipboard: ClipboardAccess::default(),
            capacity: EditorCapacity::default(),
            language: None,
            watcher: None,
            watcher_unavailable: false,
            git_status: true,
        }
    }

    /// Returns the identity that every event and every result of this editor
    /// carries.
    #[inline]
    #[must_use]
    pub const fn instance(&self) -> EditorInstanceId {
        self.editor.instance()
    }

    /// Returns the rectangle that this editor accepted.
    #[inline]
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.editor.area()
    }

    /// Accepts one new rectangle for this editor.
    ///
    /// # Errors
    ///
    /// Returns [`GeometryError::Empty`] for a rectangle without a cell. The
    /// editor keeps the rectangle that it accepted before.
    pub fn set_area(&mut self, area: Rect) -> Result<Redraw, GeometryError> {
        self.editor.set_area(area)
    }

    /// Opens one file of this worktree.
    ///
    /// The path is relative to the root, so the editor reaches no file outside
    /// it. The open leaves the editor as one queued file request, which
    /// [`EmbeddedEditor::dispatch`] hands to the spawner.
    pub fn open_file(&mut self, path: WorktreeRelativePath) -> Redraw {
        self.editor.open(path)
    }

    /// Returns the file-sidebar rows that the host draws beside this editor.
    ///
    /// The editor owns one lazy file tree over its worktree root. This call
    /// copies the loaded state of that tree and reads no directory, so it
    /// performs no filesystem work on the host event loop. A directory that
    /// holds no listing yet reports
    /// [`FileRowKind::LoadingDirectory`](super::file_sidebar::FileRowKind::LoadingDirectory)
    /// until [`EmbeddedEditor::dispatch`] hands its read to the spawner and
    /// [`EmbeddedEditor::apply`] hands the listing back.
    ///
    /// The list holds at most
    /// [`FILE_SIDEBAR_ROWS_MAX`](super::file_sidebar::FILE_SIDEBAR_ROWS_MAX)
    /// rows. The host keeps one copy between frames and reads it again after
    /// one [`EditorEvent::RedrawRequested`].
    ///
    /// `crates/kvim-embed/examples/worktree_editor.rs` draws these rows.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_tui::__private::EmbeddedEditor;
    ///
    /// # let host_runtime = tokio::runtime::Builder::new_current_thread()
    /// #     .enable_all()
    /// #     .build()
    /// #     .expect("the example builds one runtime");
    /// # host_runtime.block_on(async {
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let mut editor = EmbeddedEditor::builder(root, Rect::new(0, 0, 80, 24))
    ///     .open()
    ///     .expect("the rectangle holds cells");
    ///
    /// // The tree reads no directory here. The read leaves as one unit of
    /// // work, and the host hands the finished unit back.
    /// for _ in 0..32 {
    ///     let _redraw = editor.dispatch();
    ///     if !editor.file_rows().is_empty() {
    ///         break;
    ///     }
    ///     let waited = tokio::time::timeout(Duration::from_secs(10), editor.recv()).await;
    ///     let Ok(completed) = waited else { break };
    ///     let _redraw = editor.apply(completed, Duration::ZERO);
    /// }
    ///
    /// let rows = editor.file_rows();
    /// assert!(!rows.is_empty(), "the worktree root holds entries");
    /// assert!(rows.iter().all(|row| row.depth() == 0));
    /// # let _shutdown = editor.shutdown(Duration::from_secs(10)).await;
    /// # });
    /// ```
    #[must_use]
    pub fn file_rows(&self) -> Vec<FileRow> {
        self.editor.file_rows()
    }

    /// Returns the worktree root as the header of the file sidebar shows it.
    ///
    /// The label shortens the home directory of the user to `~`, exactly as the
    /// header row of the sidebar of kvim shows it.
    #[must_use]
    pub fn file_root_label(&self) -> String {
        self.editor.file_root_label()
    }

    /// Applies one input of the host to the file sidebar of this editor.
    ///
    /// The reduction moves the selection, opens one directory, or closes one
    /// directory. It reads no directory itself: an expansion queues the listing
    /// that the next [`EmbeddedEditor::dispatch`] hands to the spawner.
    ///
    /// The sidebar opens no buffer. An activated file returns as
    /// [`FileSidebarOutcome::Activated`](super::file_sidebar::FileSidebarOutcome::Activated),
    /// and the host decides whether to call [`EmbeddedEditor::open_file`] with
    /// that path. The reduction latches [`EditorEvent::RedrawRequested`], so
    /// the host draws one frame after it.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_tui::__private::{EmbeddedEditor, FileSidebarInput, FileSidebarOutcome, ListMotion};
    ///
    /// let root = std::sync::Arc::new(
    ///     kvim_path::WorktreeRoot::open(
    ///         std::env::current_dir().expect("the process holds a working directory"),
    ///     )
    ///     .expect("the working directory is a worktree"),
    /// );
    /// let mut editor = EmbeddedEditor::builder(root, Rect::new(0, 0, 80, 24))
    ///     .open()
    ///     .expect("the rectangle holds cells");
    ///
    /// // The first listing has not arrived, so the tree shows no row and the
    /// // move changes nothing. The call still touches no filesystem.
    /// assert_eq!(
    ///     editor.file_sidebar(FileSidebarInput::Move(ListMotion::Down(1))),
    ///     FileSidebarOutcome::Applied,
    /// );
    /// ```
    #[must_use]
    pub fn file_sidebar(&mut self, input: FileSidebarInput) -> FileSidebarOutcome {
        self.editor.reduce_file_sidebar(input)
    }

    /// Applies one normalized terminal event through the internal standalone resolver.
    ///
    /// This method is an implementation seam for `kvim-embed`. It is not a
    /// supported `kvim-tui` host contract.
    #[doc(hidden)]
    pub fn input(&mut self, event: TerminalEvent, now: Duration) -> Redraw {
        self.editor.handle_event(event, now)
    }

    /// Applies one resolved editor command.
    ///
    /// The host owns the key-sequence resolver, so it supplies the command, its
    /// count, and the register that the operation names. `None` names the
    /// unnamed register, which every operation without a `"` prefix uses. See
    /// `docs/embedding.md`.
    #[must_use]
    pub fn command(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
        register: Option<char>,
        now: Duration,
    ) -> Reduction {
        self.editor.apply_command(command, count, register, now)
    }

    /// Inserts one run of literal text.
    #[must_use]
    pub fn insert_literal(&mut self, text: &str, now: Duration) -> Reduction {
        self.editor.insert_literal(text, now)
    }

    /// Applies one bounded paste as literal text.
    #[must_use]
    pub fn paste(&mut self, text: &PasteText, now: Duration) -> Reduction {
        self.editor.paste(text, now)
    }

    /// Returns the editing mode of this editor.
    ///
    /// A host names the mode in a band of its own, and the mode answers
    /// whatever reads the next key. [`EmbeddedEditor::input_context`] reports
    /// the scope that owns the keys instead, so it names a prompt, a sidebar,
    /// or a picker while one of those holds them. A host that wants the mode
    /// to stay on its statusline through a prompt reads this, exactly as the
    /// standalone editor does. See `docs/embedding.md`.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.editor.mode()
    }

    /// Returns the input context that this editor publishes.
    ///
    /// A host that composes several surfaces supplies this value to the shared
    /// resolver of its workspace after every input that this editor reduced.
    /// See `docs/embedding.md`.
    #[must_use]
    pub fn input_context(&self) -> InputContextSnapshot<BindingScope> {
        self.editor.input_context()
    }

    /// Cancels every pending semantic phase of this editor.
    ///
    /// A workspace composer proposes this addressed effect before it moves
    /// focus or overlay ownership. The call resets the count, the operator, the
    /// register, the text object, and the prompt, so the next
    /// [`EmbeddedEditor::input_context`] carries an idle context with a new
    /// generation. The host then resumes the proposed transition.
    #[must_use]
    pub fn cancel_pending(&mut self, now: Duration) -> Reduction {
        self.editor.cancel_pending(now)
    }

    /// Returns the next elapsed time at which this editor changes by itself.
    ///
    /// A host that composes several editors waits for the earliest deadline of
    /// its editors and calls [`EmbeddedEditor::tick`] on the editor that owns
    /// it.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        self.editor.next_deadline()
    }

    /// Applies the state change that the elapsed time alone produces.
    pub fn tick(&mut self, now: Duration) -> Redraw {
        self.editor.tick(now)
    }

    /// Reports whether this editor still serves input.
    #[inline]
    #[must_use]
    pub const fn run_state(&self) -> RunState {
        self.editor.run_state()
    }

    /// Renders one frame into the cells that the host owns.
    ///
    /// The editor writes only inside `area` and returns the cursor that the
    /// frame asks for. The host decides whether to apply that request.
    ///
    /// # Errors
    ///
    /// Returns [`GeometryError::Empty`] for a rectangle without a cell,
    /// [`GeometryError::OutsideBuffer`] for a rectangle that leaves `cells`,
    /// and [`GeometryError::Unreconciled`] for a rectangle that
    /// [`EmbeddedEditor::set_area`] never accepted. Every error leaves every
    /// cell unchanged.
    pub fn draw(&self, cells: &mut CellBuffer, area: Rect) -> Result<CursorRequest, GeometryError> {
        self.editor.draw(cells, area)
    }

    /// Takes the next fact or request of this editor.
    ///
    /// The host must read these events, because a full queue refuses the next
    /// durable operation.
    #[must_use]
    pub fn take_event(&mut self) -> Option<PublishedEvent> {
        self.editor.take_event()
    }

    /// Reports whether this editor may request Git status.
    ///
    /// This is an internal adapter seam for facades with explicit Git policy.
    #[doc(hidden)]
    #[must_use]
    pub fn git_status_enabled(&self) -> bool {
        self.editor.git_status_enabled()
    }

    /// Reports whether a Git status request waits for dispatch.
    ///
    /// This is an internal adapter seam for facades with explicit Git policy.
    #[doc(hidden)]
    #[must_use]
    pub fn git_request_queued(&self) -> bool {
        self.editor.git_request_queued()
    }

    /// Hands every queued request of this editor to its spawner.
    ///
    /// The call returns at once and starts no detached task. The host calls it
    /// after every input, every tick, and every applied result.
    pub fn dispatch(&mut self) -> Redraw {
        self.driver
            .dispatch(&mut self.editor)
            .expect("the facade constructs one driver with its owned session")
    }

    /// Waits for the next finished unit of background work of this editor.
    ///
    /// The future installs no terminal, no signal handler, no panic hook, and
    /// no other process-global owner, so a host can hold one of these futures
    /// for every editor that it runs. Every branch is cancellation safe.
    pub async fn recv(&mut self) -> Completed {
        self.driver.recv().await
    }

    /// Applies one finished unit of work as one editor transition.
    ///
    /// Returns [`EditorApplyError`] before any state change when `completed`
    /// belongs to another editor.
    #[allow(clippy::result_large_err)]
    pub fn apply(
        &mut self,
        completed: Completed,
        now: Duration,
    ) -> Result<Redraw, EditorApplyError> {
        self.driver
            .apply(&mut self.editor, completed, now)
            .map_err(Into::into)
    }

    /// Ends every background service of this editor.
    ///
    /// The operation consumes the editor, so no caller can submit after it. It
    /// cancels every request that has not committed yet, waits for every task
    /// that can still commit, and returns the remaining events.
    pub async fn shutdown(self, deadline: Duration) -> EditorShutdown {
        let Self { mut editor, driver } = self;
        match driver
            .shutdown(&mut editor, deadline)
            .await
            .expect("the facade constructs one driver with its owned session")
        {
            None => EditorShutdown::Finished {
                events: drain_published(&mut editor),
            },
            Some(drain) => EditorShutdown::Draining(Box::new(EditorDrain { editor, drain })),
        }
    }
}

/// What one editor shutdown produced.
///
/// The value never reports a complete shutdown while a committed side effect
/// can still publish its mandatory event. See `docs/embedding.md`.
#[must_use = "an unfinished shutdown still owns the mandatory events of committed work"]
#[derive(Debug)]
pub enum EditorShutdown {
    /// Every tracked task finished inside the deadline.
    Finished {
        /// The events that the editor still held, in publication order.
        events: Vec<PublishedEvent>,
    },
    /// The deadline expired while a committed task can still publish.
    ///
    /// The host must keep its asynchronous runtime alive until
    /// [`EditorDrain::complete`] returns. The drain owns the complete visible
    /// state of the editor, so the box keeps this value small.
    Draining(Box<EditorDrain>),
}

/// The remaining work of one editor whose shutdown deadline expired.
///
/// The drain owns every task that can still commit a side effect and the
/// delivery of every mandatory event that such a task produces.
#[must_use = "the drain owns the mandatory events of every committed side effect"]
pub struct EditorDrain {
    editor: Session,
    drain: ShutdownDrain,
}

impl fmt::Debug for EditorDrain {
    /// Names the instance that owns the remaining events.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EditorDrain")
            .field("instance", &self.editor.instance())
            .finish_non_exhaustive()
    }
}

impl EditorDrain {
    /// Returns the editor that owns the remaining events.
    #[inline]
    #[must_use]
    pub const fn instance(&self) -> EditorInstanceId {
        self.editor.instance()
    }

    /// Waits for every tracked task and returns every remaining event.
    ///
    /// The wait is bounded by the deadlines of the submitted work alone, so it
    /// observes no further deadline of its own.
    #[must_use]
    pub async fn complete(self) -> Vec<PublishedEvent> {
        let Self { mut editor, drain } = self;
        let _redraw = drain
            .complete(&mut editor)
            .await
            .expect("the drain retains its facade-owned session");
        drain_published(&mut editor)
    }
}

#[cfg(test)]
#[path = "embed_tests.rs"]
mod tests;
