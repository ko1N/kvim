//! Terminal lifecycle ownership with exact and repeatable restoration.
//!
//! Restoration must not depend on unwinding. On macOS 26.5.1 a panic cannot
//! unwind: the process reports `failed to initiate panic, error 5` and aborts,
//! so no [`Drop`] runs. The behavior belongs to the operating system, not to
//! one toolchain. A panic hook is therefore the primary restoration path, and
//! [`Drop`] is the secondary one. See `docs/architecture.md`.

use std::fmt;
use std::io::{self, stdout};
use std::panic::{self, PanicHookInfo};
use std::sync::Arc;
use std::thread;

use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};

use super::TerminalError;

/// The enhanced keyboard reporting flags that kvim requests.
///
/// The flags keep the `Ctrl-Alt`, `Ctrl-Enter`, `Ctrl-\`, and modified arrow
/// chords distinct from their unmodified keys.
/// See `docs/input-actions.md` for the bindings that depend on them.
const KEYBOARD_ENHANCEMENT_FLAGS: KeyboardEnhancementFlags =
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        .union(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES)
        .union(KeyboardEnhancementFlags::REPORT_EVENT_TYPES);

/// The shape that the terminal draws for its own cursor.
///
/// The editor shows the terminal cursor instead of a painted cell, because a
/// cell grid cannot draw half a cell. See `docs/windows.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorShape {
    /// A steady block that covers one complete cell.
    Block,
    /// A steady vertical bar at the left edge of one cell.
    Bar,
}

impl CursorShape {
    /// Returns the crossterm style that draws the shape.
    const fn style(self) -> SetCursorStyle {
        match self {
            Self::Block => SetCursorStyle::SteadyBlock,
            Self::Bar => SetCursorStyle::SteadyBar,
        }
    }
}

/// One terminal state change that restoration undoes.
///
/// The order of [`RestoreStep::ALL`] is the restoration order. Both the
/// ordinary restore and the panic hook follow it, so the terminal returns to
/// its original state through one step list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestoreStep {
    /// Return the cursor to the shape that the user configured.
    CursorShape,
    /// Pop the enhanced keyboard reporting flags.
    KeyboardEnhancement,
    /// Stop bracketed paste reporting.
    BracketedPaste,
    /// Stop mouse capture before the terminal leaves the alternate screen.
    MouseCapture,
    /// Show the cursor and leave the alternate screen.
    AlternateScreen,
    /// Leave raw mode.
    RawMode,
}

impl RestoreStep {
    /// Every step, in restoration order.
    pub const ALL: [Self; 6] = [
        Self::CursorShape,
        Self::KeyboardEnhancement,
        Self::BracketedPaste,
        Self::MouseCapture,
        Self::AlternateScreen,
        Self::RawMode,
    ];

    /// The number of steps that restoration undoes.
    pub const COUNT: usize = Self::ALL.len();

    /// Returns the record index of the step.
    ///
    /// The index is the declaration order of [`RestoreStep::ALL`], so it stays
    /// inside [`RestoreStep::COUNT`] by construction.
    const fn index(self) -> usize {
        self as usize
    }

    /// Writes the step to the process terminal.
    fn apply(self) -> io::Result<()> {
        match self {
            Self::CursorShape => execute!(stdout(), SetCursorStyle::DefaultUserShape),
            Self::KeyboardEnhancement => execute!(stdout(), PopKeyboardEnhancementFlags),
            Self::BracketedPaste => execute!(stdout(), DisableBracketedPaste),
            Self::MouseCapture => execute!(stdout(), DisableMouseCapture),
            Self::AlternateScreen => execute!(stdout(), Show, LeaveAlternateScreen),
            Self::RawMode => disable_raw_mode(),
        }
    }
}

/// The steps that the panic hook writes.
///
/// The hook cannot read which steps one control applied, because a panic gives
/// it no access to that state. It therefore writes every step. A step that the
/// session never applied writes a sequence that the terminal ignores.
const PANIC_RESTORE_STEPS: [RestoreStep; RestoreStep::COUNT] = RestoreStep::ALL;

/// The terminal writes that one installed panic hook performs.
///
/// The action is a function pointer, so the hook holds no captured state that a
/// panic could have left unusable.
type RestoreAction = fn();

/// The hook value that the process holds.
type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

/// Writes every restore step and ignores each failure.
///
/// The hook runs while the process ends, so a failure has no report path.
fn write_panic_restore() {
    for step in PANIC_RESTORE_STEPS {
        let _ = step.apply();
    }
}

/// The panic hook of one terminal session.
///
/// The hook exists exactly while the terminal holds the setup steps. It writes
/// the restore steps first, so the shell is usable, and then calls the hook
/// that it replaced, so the normal panic message still reaches the user.
#[derive(Default)]
struct PanicRestore {
    /// The hook that this hook replaced, shared with the installed hook.
    previous: Option<Arc<PanicHook>>,
}

impl PanicRestore {
    /// Creates a record that holds no installed hook.
    const fn new() -> Self {
        Self { previous: None }
    }

    /// Reports whether one hook of this record is installed.
    const fn is_installed(&self) -> bool {
        self.previous.is_some()
    }

    /// Installs the hook that writes `restore` before the previous hook runs.
    ///
    /// A second call performs no work, so the record never chains one hook
    /// onto itself.
    fn install(&mut self, restore: RestoreAction) {
        if self.is_installed() {
            return;
        }
        let previous = Arc::new(panic::take_hook());
        let chained = Arc::clone(&previous);
        panic::set_hook(Box::new(move |info| {
            // The body allocates nothing and locks no editor state, because a
            // panic can leave both unusable. It writes the terminal steps only.
            restore();
            (*chained)(info);
        }));
        self.previous = Some(previous);
    }

    /// Removes the hook and installs the hook that it replaced.
    ///
    /// The operation is repeatable. A second call performs no work. A call
    /// from a panicking thread performs no work either: [`panic::take_hook`]
    /// panics there, and a panic inside a [`Drop`] that an unwind runs ends the
    /// process without the message of the first panic. The hook has already
    /// written every restore step by then, so it stays installed for the rest
    /// of the unwind and the terminal is usable in any case.
    fn remove(&mut self) {
        if thread::panicking() {
            return;
        }
        let Some(previous) = self.previous.take() else {
            return;
        };
        // Dropping the installed hook releases the second owner of the previous
        // hook, so the previous hook moves back without a copy.
        drop(panic::take_hook());
        match Arc::try_unwrap(previous) {
            Ok(hook) => panic::set_hook(hook),
            // Another owner still holds the previous hook, so it moves back
            // behind one further call instead.
            Err(shared) => panic::set_hook(Box::new(move |info| (*shared)(info))),
        }
    }
}

impl fmt::Debug for PanicRestore {
    /// Writes whether one hook of this record is installed.
    ///
    /// A hook value holds no readable content, so the record reports its state
    /// instead.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PanicRestore")
            .field("installed", &self.is_installed())
            .finish()
    }
}

/// The terminal setup and restore steps.
///
/// The trait separates the steps from [`TerminalSession`], so a test drives
/// every failure path without a real terminal.
pub trait TerminalControl {
    /// Applies every setup step.
    ///
    /// An implementation must undo each completed step before it returns an
    /// error, so a failed setup leaves the terminal in its original state.
    fn setup(&mut self) -> io::Result<()>;

    /// Undoes every completed setup step.
    ///
    /// The operation is repeatable. A second call performs no work.
    fn restore(&mut self) -> io::Result<()>;

    /// Requests one cursor shape from the terminal.
    ///
    /// The shape is decoration. A terminal that ignores the sequence still
    /// shows its own cursor, so the caller keeps running after a failure.
    fn set_cursor_shape(&mut self, shape: CursorShape) -> io::Result<()>;

    /// Installs the panic hook that repeats the restore steps.
    ///
    /// Terminal restoration must not depend on unwinding, because a panic
    /// aborts without running [`Drop`] on some platforms. The default
    /// implementation installs nothing, so a control that changes no
    /// process-wide state stays inert.
    fn install_panic_hook(&mut self) {}

    /// Removes the panic hook that [`TerminalControl::install_panic_hook`]
    /// installed.
    ///
    /// The operation is repeatable. A second call performs no work.
    fn remove_panic_hook(&mut self) {}
}

/// The crossterm implementation of [`TerminalControl`].
///
/// The record holds one entry for each completed setup step. Cleanup undoes
/// only the steps that succeeded, so restoration stays exact and repeatable. A
/// step that fails to restore keeps its entry, so a later call retries that
/// step alone.
#[derive(Debug, Default)]
pub struct CrosstermControl {
    completed: [bool; RestoreStep::COUNT],
    panic_restore: PanicRestore,
}

impl CrosstermControl {
    /// Creates a control that has completed no setup step.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            completed: [false; RestoreStep::COUNT],
            panic_restore: PanicRestore::new(),
        }
    }

    /// Records that one setup step succeeded, or that its restore failed.
    fn record(&mut self, step: RestoreStep, completed: bool) {
        self.completed[step.index()] = completed;
    }

    /// Returns the completed steps in restoration order.
    fn pending_steps(&self) -> Vec<RestoreStep> {
        RestoreStep::ALL
            .into_iter()
            .filter(|step| self.completed[step.index()])
            .collect()
    }

    /// Undoes the completed steps and reports the first failure.
    ///
    /// Every step runs, so one failure does not block the others.
    fn cleanup(&mut self) -> io::Result<()> {
        let mut outcome = Ok(());
        for step in self.pending_steps() {
            let result = step.apply();
            // A step that could not restore keeps its record, so a later call
            // retries that step alone.
            self.record(step, result.is_err());
            if outcome.is_ok() {
                outcome = result;
            }
        }
        outcome
    }
}

impl TerminalControl for CrosstermControl {
    fn setup(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        self.record(RestoreStep::RawMode, true);
        if matches!(supports_keyboard_enhancement(), Ok(true)) {
            if let Err(error) = execute!(
                stdout(),
                PushKeyboardEnhancementFlags(KEYBOARD_ENHANCEMENT_FLAGS)
            ) {
                let _ = self.cleanup();
                return Err(error);
            }
            self.record(RestoreStep::KeyboardEnhancement, true);
        }
        // Bracketed paste turns one paste into one event instead of a run of
        // key presses, so one paste becomes one edit transaction and one undo
        // unit. A terminal that ignores the sequence sends the key run, which
        // the editor still accepts. See `docs/input-actions.md`.
        self.record(RestoreStep::BracketedPaste, true);
        if let Err(error) = execute!(stdout(), EnableBracketedPaste) {
            let _ = self.cleanup();
            return Err(error);
        }
        // Mouse capture is session state. Record it before the write because a
        // terminal can apply the sequence and then fail to flush it.
        self.record(RestoreStep::MouseCapture, true);
        if let Err(error) = execute!(stdout(), EnableMouseCapture) {
            let _ = self.cleanup();
            return Err(error);
        }
        // The editor shows the terminal cursor itself, so the alternate screen
        // keeps it visible. See `docs/windows.md`.
        self.record(RestoreStep::AlternateScreen, true);
        if let Err(error) = execute!(stdout(), EnterAlternateScreen) {
            let _ = self.cleanup();
            return Err(error);
        }
        Ok(())
    }

    fn restore(&mut self) -> io::Result<()> {
        self.cleanup()
    }

    fn set_cursor_shape(&mut self, shape: CursorShape) -> io::Result<()> {
        // Record the step before the call, because a terminal can apply the
        // sequence and then fail to flush it.
        self.record(RestoreStep::CursorShape, true);
        execute!(stdout(), shape.style())
    }

    fn install_panic_hook(&mut self) {
        self.panic_restore.install(write_panic_restore);
    }

    fn remove_panic_hook(&mut self) {
        self.panic_restore.remove();
    }
}

impl Drop for CrosstermControl {
    fn drop(&mut self) {
        let _ = self.cleanup();
        // The hook writes the steps of this control, so it leaves with it.
        self.panic_restore.remove();
    }
}

/// Whether the setup steps are applied to the terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionState {
    /// The setup steps are applied.
    Active,
    /// The setup steps are undone. The session can resume.
    Suspended,
}

/// Owns the terminal setup steps and restores them on every exit path.
///
/// The session restores the terminal on a normal return, on an error, and after
/// a panic. [`TerminalSession::restore`] consumes the session, so the caller
/// observes a restore failure. A panic hook holds the primary restoration path,
/// because a panic aborts without unwinding on some platforms, and [`Drop`]
/// stays the secondary one. The hook exists exactly while the terminal holds the
/// setup steps: entering installs it, and a successful restore removes it.
///
/// ```
/// use std::io;
///
/// use kvim_terminal::{CursorShape, TerminalControl, TerminalSession};
///
/// #[derive(Default)]
/// struct FakeControl {
///     setups: usize,
///     restores: usize,
///     shape: Option<CursorShape>,
/// }
///
/// impl TerminalControl for FakeControl {
///     fn setup(&mut self) -> io::Result<()> {
///         self.setups += 1;
///         Ok(())
///     }
///
///     fn restore(&mut self) -> io::Result<()> {
///         self.restores += 1;
///         Ok(())
///     }
///
///     fn set_cursor_shape(&mut self, shape: CursorShape) -> io::Result<()> {
///         self.shape = Some(shape);
///         Ok(())
///     }
/// }
///
/// let mut session = TerminalSession::enter(FakeControl::default())?;
/// session.set_cursor_shape(CursorShape::Bar)?;
/// session.suspend()?;
/// session.resume()?;
/// session.restore()?;
/// # Ok::<(), kvim_terminal::TerminalError>(())
/// ```
pub struct TerminalSession<C: TerminalControl> {
    control: C,
    state: SessionState,
}

impl<C: TerminalControl> TerminalSession<C> {
    /// Applies the setup steps and takes ownership of the terminal state.
    ///
    /// The session does not exist when setup fails, so no restore runs for a
    /// terminal that was never changed.
    pub fn enter(mut control: C) -> Result<Self, TerminalError> {
        control.setup().map_err(TerminalError::Control)?;
        control.install_panic_hook();
        Ok(Self {
            control,
            state: SessionState::Active,
        })
    }

    /// Undoes the setup steps and keeps the session for a later resume.
    ///
    /// The call performs no work when the session is already suspended. A
    /// failed restore keeps the session active and keeps the panic hook, so
    /// [`Drop`] retries the restore and the hook still covers the terminal.
    pub fn suspend(&mut self) -> Result<(), TerminalError> {
        if self.state == SessionState::Suspended {
            return Ok(());
        }
        self.control.restore().map_err(TerminalError::Control)?;
        self.control.remove_panic_hook();
        self.state = SessionState::Suspended;
        Ok(())
    }

    /// Requests one cursor shape from the terminal.
    ///
    /// The caller changes the shape only when the editor mode changes, because
    /// the shape is terminal state, not frame content.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalError`] when the write fails. The shape is
    /// decoration, so a caller may ignore that failure and keep running.
    pub fn set_cursor_shape(&mut self, shape: CursorShape) -> Result<(), TerminalError> {
        self.control
            .set_cursor_shape(shape)
            .map_err(TerminalError::Control)
    }

    /// Applies the setup steps again after a suspend.
    ///
    /// The call performs no work when the session is already active. A failed
    /// setup keeps the session suspended.
    pub fn resume(&mut self) -> Result<(), TerminalError> {
        if self.state == SessionState::Active {
            return Ok(());
        }
        self.control.setup().map_err(TerminalError::Control)?;
        self.control.install_panic_hook();
        self.state = SessionState::Active;
        Ok(())
    }

    /// Undoes the setup steps and consumes the session.
    ///
    /// Use this operation for a normal shutdown, because it reports a restore
    /// failure. A failure still marks the session suspended, because the
    /// control keeps the record of each step that it could not undo.
    pub fn restore(mut self) -> Result<(), TerminalError> {
        if self.state == SessionState::Suspended {
            return Ok(());
        }
        self.state = SessionState::Suspended;
        let outcome = self.control.restore().map_err(TerminalError::Control);
        // A failed restore keeps the hook, because the terminal still holds one
        // step that the hook writes again.
        if outcome.is_ok() {
            self.control.remove_panic_hook();
        }
        outcome
    }
}

impl<C: TerminalControl> Drop for TerminalSession<C> {
    fn drop(&mut self) {
        if self.state == SessionState::Active {
            let _ = self.control.restore();
            self.control.remove_panic_hook();
            self.state = SessionState::Suspended;
        }
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
