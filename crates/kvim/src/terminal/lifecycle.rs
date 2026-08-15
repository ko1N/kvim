//! Terminal lifecycle ownership with exact and repeatable restoration.

use std::io::{self, stdout};

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
}

/// The crossterm implementation of [`TerminalControl`].
///
/// Each field records one completed setup step. Cleanup undoes only the steps
/// that succeeded, so restoration stays exact and repeatable. A step that fails
/// to restore keeps its record, so a later call retries that step alone.
#[derive(Debug, Default)]
pub struct CrosstermControl {
    keyboard_enhancement_pushed: bool,
    alternate_screen_entered: bool,
    raw_mode_enabled: bool,
    cursor_shape_set: bool,
}

impl CrosstermControl {
    /// Creates a control that has completed no setup step.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            keyboard_enhancement_pushed: false,
            alternate_screen_entered: false,
            raw_mode_enabled: false,
            cursor_shape_set: false,
        }
    }

    /// Undoes the completed steps in reverse order and reports the first
    /// failure. Every step runs, so one failure does not block the others.
    fn cleanup(&mut self) -> io::Result<()> {
        // The editor changed the cursor shape, so the terminal returns to the
        // shape that its user configured.
        let cursor = if self.cursor_shape_set {
            let result = execute!(stdout(), SetCursorStyle::DefaultUserShape);
            self.cursor_shape_set = result.is_err();
            result
        } else {
            Ok(())
        };
        let keyboard = if self.keyboard_enhancement_pushed {
            let result = execute!(stdout(), PopKeyboardEnhancementFlags);
            self.keyboard_enhancement_pushed = result.is_err();
            result
        } else {
            Ok(())
        };
        let screen = if self.alternate_screen_entered {
            let result = execute!(stdout(), Show, LeaveAlternateScreen);
            self.alternate_screen_entered = result.is_err();
            result
        } else {
            Ok(())
        };
        let raw = if self.raw_mode_enabled {
            let result = disable_raw_mode();
            self.raw_mode_enabled = result.is_err();
            result
        } else {
            Ok(())
        };
        cursor.and(keyboard).and(screen).and(raw)
    }
}

impl TerminalControl for CrosstermControl {
    fn setup(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        self.raw_mode_enabled = true;
        if matches!(supports_keyboard_enhancement(), Ok(true)) {
            if let Err(error) = execute!(
                stdout(),
                PushKeyboardEnhancementFlags(KEYBOARD_ENHANCEMENT_FLAGS)
            ) {
                let _ = self.cleanup();
                return Err(error);
            }
            self.keyboard_enhancement_pushed = true;
        }
        // The editor shows the terminal cursor itself, so the alternate screen
        // keeps it visible. See `docs/windows.md`.
        self.alternate_screen_entered = true;
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
        self.cursor_shape_set = true;
        execute!(stdout(), shape.style())
    }
}

impl Drop for CrosstermControl {
    fn drop(&mut self) {
        let _ = self.cleanup();
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
/// The session restores the terminal on a normal return, on an error, and while
/// the process unwinds from a panic. [`TerminalSession::restore`] consumes the
/// session, so the caller observes a restore failure. [`Drop`] stays a safety
/// net for every other exit path.
///
/// ```
/// use std::io;
///
/// use kvim::terminal::{CursorShape, TerminalControl, TerminalSession};
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
/// # Ok::<(), kvim::terminal::TerminalError>(())
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
        Ok(Self {
            control,
            state: SessionState::Active,
        })
    }

    /// Undoes the setup steps and keeps the session for a later resume.
    ///
    /// The call performs no work when the session is already suspended. A
    /// failed restore keeps the session active, so [`Drop`] retries it.
    pub fn suspend(&mut self) -> Result<(), TerminalError> {
        if self.state == SessionState::Suspended {
            return Ok(());
        }
        self.control.restore().map_err(TerminalError::Control)?;
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
        self.control.restore().map_err(TerminalError::Control)
    }
}

impl<C: TerminalControl> Drop for TerminalSession<C> {
    fn drop(&mut self) {
        if self.state == SessionState::Active {
            let _ = self.control.restore();
            self.state = SessionState::Suspended;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ControlStep {
        Setup,
        Restore,
        Shape(CursorShape),
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

        assert_eq!(steps(&log), vec![ControlStep::Setup, ControlStep::Restore]);
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
            vec![ControlStep::Setup, ControlStep::Restore, ControlStep::Setup,]
        );
    }

    #[test]
    fn drop_after_a_suspend_does_not_restore_again() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut session = TerminalSession::enter(FakeControl::new(&log)).expect("setup succeeds");

        session.suspend().expect("restore succeeds");
        drop(session);

        assert_eq!(steps(&log), vec![ControlStep::Setup, ControlStep::Restore]);
    }

    #[test]
    fn an_explicit_restore_replaces_the_drop_safety_net() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let session = TerminalSession::enter(FakeControl::new(&log)).expect("setup succeeds");

        session.restore().expect("restore succeeds");

        assert_eq!(steps(&log), vec![ControlStep::Setup, ControlStep::Restore]);
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
                ControlStep::Restore,
                ControlStep::Restore,
            ],
            "an active session must retry the restore while it drops"
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
            vec![ControlStep::Setup, ControlStep::Restore, ControlStep::Setup,],
            "a suspended session must not restore an unchanged terminal"
        );
    }
}
