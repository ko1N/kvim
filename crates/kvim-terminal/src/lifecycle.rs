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

use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};

use super::TerminalError;

/// The enhanced keyboard reporting flags that Kvim requests.
///
/// The flags keep the `Ctrl-Alt`, `Ctrl-Enter`, and `Ctrl-\` chords distinct.
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
    /// Show the cursor and leave the alternate screen.
    AlternateScreen,
    /// Leave raw mode.
    RawMode,
}

impl RestoreStep {
    /// Every step, in restoration order.
    pub const ALL: [Self; 4] = [
        Self::CursorShape,
        Self::KeyboardEnhancement,
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
    /// The operation is repeatable. A second call performs no work.
    fn remove(&mut self) {
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
mod tests {
    use std::cell::RefCell;
    use std::future;
    use std::rc::Rc;

    use super::*;
    use crate::{TerminationSignal, TerminationSource};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ControlStep {
        Setup,
        Restore,
        Shape(CursorShape),
        HookInstalled,
        HookRemoved,
    }

    #[derive(Default)]
    struct FakeControl {
        log: Rc<RefCell<Vec<ControlStep>>>,
        setup_fails: bool,
        restore_fails: bool,
    }

    impl FakeControl {
        fn new(log: &Rc<RefCell<Vec<ControlStep>>>) -> Self {
            Self {
                log: Rc::clone(log),
                setup_fails: false,
                restore_fails: false,
            }
        }

        fn record(&self, step: ControlStep, fails: bool) -> io::Result<()> {
            self.log.borrow_mut().push(step);
            if fails {
                return Err(io::Error::other("the terminal step failed"));
            }
            Ok(())
        }
    }

    impl TerminalControl for FakeControl {
        fn setup(&mut self) -> io::Result<()> {
            self.record(ControlStep::Setup, self.setup_fails)
        }

        fn restore(&mut self) -> io::Result<()> {
            self.record(ControlStep::Restore, self.restore_fails)
        }

        fn set_cursor_shape(&mut self, shape: CursorShape) -> io::Result<()> {
            self.record(ControlStep::Shape(shape), false)
        }

        fn install_panic_hook(&mut self) {
            let _ = self.record(ControlStep::HookInstalled, false);
        }

        fn remove_panic_hook(&mut self) {
            let _ = self.record(ControlStep::HookRemoved, false);
        }
    }

    fn steps(log: &Rc<RefCell<Vec<ControlStep>>>) -> Vec<ControlStep> {
        log.borrow().clone()
    }

    #[test]
    fn a_failed_setup_leaves_no_session() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut control = FakeControl::new(&log);
        control.setup_fails = true;

        let session = TerminalSession::enter(control);

        assert!(session.is_err());
        assert_eq!(steps(&log), vec![ControlStep::Setup]);
    }

    #[test]
    fn drop_restores_an_active_session_once() {
        let log = Rc::new(RefCell::new(Vec::new()));

        drop(TerminalSession::enter(FakeControl::new(&log)).expect("setup succeeds"));

        assert_eq!(
            steps(&log),
            vec![
                ControlStep::Setup,
                ControlStep::HookInstalled,
                ControlStep::Restore,
                ControlStep::HookRemoved,
            ]
        );
    }

    #[test]
    fn suspend_and_resume_apply_each_step_once() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut session = TerminalSession::enter(FakeControl::new(&log)).expect("setup succeeds");

        session.suspend().expect("restore succeeds");
        session
            .suspend()
            .expect("a repeated suspend performs no work");
        session.resume().expect("setup succeeds");
        session
            .resume()
            .expect("a repeated resume performs no work");

        assert_eq!(
            steps(&log),
            vec![
                ControlStep::Setup,
                ControlStep::HookInstalled,
                ControlStep::Restore,
                ControlStep::HookRemoved,
                ControlStep::Setup,
                ControlStep::HookInstalled,
            ],
            "the hook exists exactly while the terminal holds the setup steps"
        );
    }

    #[test]
    fn drop_after_a_suspend_does_not_restore_again() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut session = TerminalSession::enter(FakeControl::new(&log)).expect("setup succeeds");

        session.suspend().expect("restore succeeds");
        drop(session);

        assert_eq!(
            steps(&log),
            vec![
                ControlStep::Setup,
                ControlStep::HookInstalled,
                ControlStep::Restore,
                ControlStep::HookRemoved,
            ]
        );
    }

    #[test]
    fn an_explicit_restore_replaces_the_drop_safety_net() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let session = TerminalSession::enter(FakeControl::new(&log)).expect("setup succeeds");

        session.restore().expect("restore succeeds");

        assert_eq!(
            steps(&log),
            vec![
                ControlStep::Setup,
                ControlStep::HookInstalled,
                ControlStep::Restore,
                ControlStep::HookRemoved,
            ]
        );
    }

    #[test]
    fn a_second_restore_of_a_suspended_session_is_harmless() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut session = TerminalSession::enter(FakeControl::new(&log)).expect("setup succeeds");

        session.suspend().expect("restore succeeds");
        session
            .restore()
            .expect("a repeated restore performs no work");

        assert_eq!(
            steps(&log),
            vec![
                ControlStep::Setup,
                ControlStep::HookInstalled,
                ControlStep::Restore,
                ControlStep::HookRemoved,
            ],
            "a restored terminal must not receive the steps a second time"
        );
    }

    #[test]
    fn a_failed_suspend_keeps_the_session_active_for_drop() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut control = FakeControl::new(&log);
        control.restore_fails = true;
        let mut session = TerminalSession::enter(control).expect("setup succeeds");

        session.suspend().expect_err("the restore step fails");
        drop(session);

        assert_eq!(
            steps(&log),
            vec![
                ControlStep::Setup,
                ControlStep::HookInstalled,
                ControlStep::Restore,
                ControlStep::Restore,
                ControlStep::HookRemoved,
            ],
            "an active session must retry the restore while it drops, and it keeps the hook until one restore succeeds"
        );
    }

    #[test]
    fn a_failed_resume_keeps_the_session_suspended() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut session = TerminalSession::enter(FakeControl::new(&log)).expect("setup succeeds");
        session.suspend().expect("restore succeeds");
        session.control.setup_fails = true;

        session.resume().expect_err("the setup step fails");
        drop(session);

        assert_eq!(
            steps(&log),
            vec![
                ControlStep::Setup,
                ControlStep::HookInstalled,
                ControlStep::Restore,
                ControlStep::HookRemoved,
                ControlStep::Setup,
            ],
            "a suspended session must not restore an unchanged terminal"
        );
    }

    #[tokio::test]
    async fn a_termination_signal_leaves_the_loop_and_restores_the_terminal() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let session = TerminalSession::enter(FakeControl::new(&log)).expect("setup succeeds");
        let (requests, mut terminations) = TerminationSource::channel();
        requests
            .send(TerminationSignal::Terminate)
            .await
            .expect("the source holds its receiver");

        // The editor waits for a terminal event and a termination request
        // together. The request must end the wait, so the loop leaves and the
        // caller restores exactly as it does after the last window closes.
        let terminated = tokio::select! {
            () = future::pending::<()>() => false,
            _ = terminations.recv() => true,
        };
        assert!(terminated, "a termination request must end the event wait");
        session.restore().expect("restore succeeds");

        assert_eq!(
            steps(&log),
            vec![
                ControlStep::Setup,
                ControlStep::HookInstalled,
                ControlStep::Restore,
                ControlStep::HookRemoved,
            ],
            "a terminated editor must never leave the terminal in raw mode"
        );
    }

    #[test]
    fn the_panic_hook_writes_the_steps_of_a_complete_restore() {
        let control = CrosstermControl {
            completed: [true; RestoreStep::COUNT],
            panic_restore: PanicRestore::new(),
        };

        assert_eq!(
            control.pending_steps(),
            PANIC_RESTORE_STEPS.to_vec(),
            "the hook must return the terminal exactly as an ordinary restore does"
        );
    }

    #[test]
    fn a_restored_control_writes_no_step_again() {
        let control = CrosstermControl::new();

        assert!(
            control.pending_steps().is_empty(),
            "a control that applied no setup step restores nothing"
        );
    }

    #[test]
    fn the_hook_record_installs_once_and_removes_repeatably() {
        // The hook of this test writes nothing, so the process terminal stays
        // unchanged while the record drives its own state.
        let mut record = PanicRestore::new();
        assert!(!record.is_installed());

        record.install(|| {});
        assert!(record.is_installed());
        record.install(|| {});
        assert!(
            record.is_installed(),
            "a second install must not chain the hook onto itself"
        );

        record.remove();
        assert!(!record.is_installed());
        record.remove();
        assert!(!record.is_installed(), "a second remove performs no work");
    }
}
