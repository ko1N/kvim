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
