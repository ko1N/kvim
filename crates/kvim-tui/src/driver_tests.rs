use std::ops::Not;
use std::path::{Path, PathBuf};

use ratatui::layout::Rect;
use tokio::time::timeout;

use kvim_runtime::{EVENT_QUEUE_CAPACITY, RuntimeLimits};
use kvim_settings::EditorSettings;
use kvim_terminal::{Key, KeyCode, TerminalEvent};
use kvim_workspace::temp::TempDir;

use super::*;
use crate::embed::EditorEvent;
use crate::session::test_root;
use crate::tree::GENERATED_NAMES;

/// The publication slot of the parked job of one shutdown test.
const PARKED_SLOT: RequestSlot = RequestSlot::new(99);

/// The deadline of the parked job of one shutdown test.
///
/// The test releases the job, so the deadline only bounds a failed run.
const PARKED_DEADLINE: Duration = Duration::from_secs(30);

/// The text that the fixture file holds before one save.
const ORIGINAL: &str = "one\n";

/// The elapsed time that every transition of these tests reports.
const NOW: Duration = Duration::ZERO;

/// The time that one test waits for the refused registration.
const REGISTRATION_WAIT: Duration = Duration::from_secs(5);

/// The time that one test waits for a future that must never complete.
const PARKED_WAIT: Duration = Duration::from_millis(50);

/// The report of a workspace that no watcher observes.
const WATCH_MISSING_NOTE: &str =
    "the workspace watcher could not start; the file tree updates on a refresh";

#[tokio::test]
async fn recovery_uses_capacity_independent_from_a_saturated_normal_worker_lane() {
    let directory = TempDir::new("driver-recovery-lane");
    let path = directory.write("main.rs", ORIGINAL);
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    let mut editor = Session::new(
        Rect::new(0, 0, 80, 24),
        settings,
        test_root(directory.path.clone()),
    )
    .with_recovery_state_directory(directory.join("state"));
    let _ = editor.open_path(path);
    let request = editor.take_file_request().unwrap();
    let _ = editor.apply_file_result(request.run());
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('i'))), NOW);
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('a'))), NOW);

    let limits = RuntimeLimits::new(2, 1, 1).unwrap();
    let (runtime, results) = Runtime::<EditorWork>::with_limits(limits);
    let mut driver = EditorDriver::new(editor.instance(), runtime, results);
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let parked = driver
        .gate
        .begin(PARKED_SLOT, &driver.spawner.cancellation_root());
    driver
        .spawner
        .submit_committing_worker(parked, PARKED_DEADLINE, move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            EditorWork(WorkResult::HostReport(String::new()))
        })
        .unwrap();
    entered_rx.await.unwrap();

    let _ = driver.dispatch(&mut editor).unwrap();
    let completed = timeout(REGISTRATION_WAIT, driver.recv())
        .await
        .expect("the independent recovery lane finishes");
    let _ = driver.apply(&mut editor, completed, NOW).unwrap();

    release_tx.send(()).unwrap();
    let _ = driver
        .shutdown(&mut editor, REGISTRATION_WAIT)
        .await
        .unwrap();
}

#[tokio::test]
async fn one_dispatch_hands_a_refused_format_and_the_save_behind_it_to_their_services() {
    let directory = TempDir::new("driver-dispatch-save");
    let path = directory.write("main.rs", "one\n");
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    let root = path
        .parent()
        .expect("the temporary file holds a parent directory")
        .to_path_buf();
    let mut editor = Session::new(Rect::new(0, 0, 80, 24), settings, test_root(root));
    let _ = editor.open_path(path);
    let request = editor
        .take_file_request()
        .expect("the open queued one file request");
    let _ = editor.apply_file_result(request.run());
    // One typed character leaves the buffer with an unsaved change.
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('i'))), NOW);
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('a'))), NOW);
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Esc)), NOW);
    assert!(editor.buffer().is_modified());

    let (runtime, _results) = Runtime::<EditorWork>::new();
    let (recovery_runtime, _recovery_results) = Runtime::<EditorWork>::new();
    let gate = PublicationGate::default();
    let recovery_gate = PublicationGate::default();
    // The editor runs without language services, so the formatter request of
    // the save reaches no server.
    let mut language = None;
    let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    let redraw = dispatch(
        &mut editor,
        &runtime,
        &gate,
        &recovery_runtime,
        &recovery_gate,
        &mut language,
    );

    assert_eq!(
        redraw,
        Redraw::Needed,
        "the refused formatter request names its state on the message line"
    );
    assert!(
        editor.take_file_request().is_none(),
        "the dispatch hands the save to the worker service inside one iteration, \
             so the write never waits for the next terminal event"
    );
    recovery_runtime.shutdown().await;
    runtime.shutdown().await;
}

#[tokio::test]
async fn one_dispatch_runs_the_external_formatter_and_the_save_behind_it() {
    let directory = TempDir::new("driver-dispatch-format");
    // The Nix adapter declares an external formatter, so the save reaches
    // the bounded process service instead of a language server.
    let path = directory.write("flake.nix", "{  }\n");
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    let root = path
        .parent()
        .expect("the temporary file holds a parent directory")
        .to_path_buf();
    let mut editor = Session::new(Rect::new(0, 0, 80, 24), settings, test_root(root));
    let _ = editor.open_path(path);
    let request = editor
        .take_file_request()
        .expect("the open queued one file request");
    let _ = editor.apply_file_result(request.run());
    // One typed character leaves the buffer with an unsaved change, so the
    // save behind the formatter writes the file.
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('i'))), NOW);
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char(' '))), NOW);
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Esc)), NOW);

    let (runtime, mut results) = Runtime::<EditorWork>::new();
    let (recovery_runtime, _recovery_results) = Runtime::<EditorWork>::new();
    let gate = PublicationGate::default();
    let recovery_gate = PublicationGate::default();
    let mut language = None;
    let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    let _ = dispatch(
        &mut editor,
        &runtime,
        &gate,
        &recovery_runtime,
        &recovery_gate,
        &mut language,
    );

    assert!(
        editor.take_file_request().is_none(),
        "the save waits for the formatter answer"
    );
    // The same dispatch also started the directory read of the file tree,
    // so the loop applies every result until the formatter answers.
    let mut answered = false;
    for _ in 0..DISPATCH_PASSES_MAX + PICKER_DISPATCH_MAX {
        let event = results
            .recv()
            .await
            .expect("every accepted request produces one result");
        answered |= event.request.slot() == FORMAT_SLOT;
        let _ = complete(&mut editor, &gate, Some(event));
        if answered {
            break;
        }
    }
    assert!(
        answered,
        "the dispatch handed the run to the process service"
    );

    // A host without the program answers a typed failure, and a host with
    // it answers a document. The save follows either answer.
    assert!(
        editor.take_file_request().is_some(),
        "the formatter answer completes the save that waited for it"
    );
    recovery_runtime.shutdown().await;
    runtime.shutdown().await;
}

#[tokio::test]
async fn a_registration_that_fails_reports_that_no_watcher_runs() {
    // The start places no watch, so it accepts the root that the deferred
    // registration then refuses.
    let directory = TempDir::new("driver-missing-watch-root");
    let root = test_root(directory.path.clone());
    std::fs::remove_dir_all(&directory.path)
        .expect("the fixture root exists before watcher registration");
    // The gate holds every test of this binary that builds one platform
    // watcher, so one deadline measures the watcher and not the suite.
    let _platform_watcher = crate::session::PLATFORM_WATCHER.lock().await;
    let mut watcher = FileWatcher::start(root, &GENERATED_NAMES).ok();
    assert!(
        watcher.is_some(),
        "the start defers every platform call, so it refuses no root"
    );

    let batch = timeout(REGISTRATION_WAIT, next_watch_batch(&mut watcher))
        .await
        .expect("the refused registration ends the published stream");

    assert!(batch.is_none(), "the ended stream publishes no burst");
    assert!(
        watcher.is_none(),
        "the loop drops the ended watch instead of reading it again"
    );
    assert!(
        timeout(PARKED_WAIT, next_watch_batch(&mut watcher))
            .await
            .is_err(),
        "the loop then waits for its other events alone"
    );

    let mut editor = Session::new(
        Rect::new(0, 0, 80, 24),
        EditorSettings::default(),
        test_root(std::env::current_dir().expect("the test process holds a working directory")),
    );
    let redraw = publish_watch(&mut editor, batch.as_ref());

    assert_eq!(
        redraw,
        Redraw::Needed,
        "the report changes the message line, so one frame follows it"
    );
    assert_eq!(
        editor
            .message()
            .map_or_else(String::new, |message| message.text().to_owned()),
        WATCH_MISSING_NOTE,
    );
}

/// The steps that one test runs before it gives up on a result.
///
/// One open queues the read, the directory listing, the Git status, and the
/// analysis, so the bound covers every result of that chain.
const DRIVER_STEPS_MAX: usize = 16;

/// The time that one test waits for one result of the spawner.
const STEP_WAIT: Duration = Duration::from_secs(10);

/// The shutdown deadline that every ordinary test gives the driver.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);

/// The reads that one test performs before it gives up on a write.
const COMMIT_POLLS_MAX: usize = 1000;

/// The time between two reads of one running save.
const COMMIT_POLL: Duration = Duration::from_millis(10);

/// The modules that the structural check reads.
fn source_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Returns every production module of this crate except the driver itself.
///
/// A `tests.rs` or `*_tests.rs` sibling holds test code, so it names a
/// submission or a terminal owner to drive one. The driver is the module under
/// test, so it owns both.
fn production_modules() -> Vec<PathBuf> {
    let mut modules = Vec::new();
    for entry in
        std::fs::read_dir(source_directory()).expect("the crate holds its own source directory")
    {
        let path = entry
            .expect("the source directory lists its entries")
            .path();
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let skip = path.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name == "driver.rs" || name == "tests.rs" || name.ends_with("_tests.rs")
        });
        if !skip {
            modules.push(path);
        }
    }
    modules
}

/// Creates one editor over one temporary workspace.
fn editor_at(root: &Path) -> Session {
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    Session::new(
        Rect::new(0, 0, 80, 24),
        settings,
        test_root(root.to_path_buf()),
    )
}

/// Runs one host step: one dispatch, one wait, and one transition.
async fn step(driver: &mut EditorDriver, editor: &mut Session) {
    let _ = driver.dispatch(editor);
    let completed = timeout(STEP_WAIT, driver.recv())
        .await
        .expect("every accepted request produces one result");
    let _ = driver.apply(editor, completed, NOW);
}

/// Loads one file through the driver, like the host loop does.
async fn open_through(driver: &mut EditorDriver, editor: &mut Session, path: PathBuf) {
    editor.open_path(path);
    for _ in 0..DRIVER_STEPS_MAX {
        step(driver, editor).await;
        if editor.active_buffer().path().is_some() {
            return;
        }
    }
    panic!("one open queues fewer results than the step bound");
}

/// Leaves the active buffer with one unsaved change.
fn type_one_character(editor: &mut Session) {
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('i'))), NOW);
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('a'))), NOW);
    let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Esc)), NOW);
    assert!(editor.buffer().is_modified());
}

/// Waits until the running save reached the file.
///
/// A masked job starts its blocking work before it writes, so a changed file
/// proves that the job entered its commit. No cancellation can drop its
/// result after that point, which is the seam that the two shutdown tests
/// below need. See `docs/embedding.md`.
async fn wait_for_commit(path: &Path, before: &str) {
    for _ in 0..COMMIT_POLLS_MAX {
        if std::fs::read_to_string(path).is_ok_and(|text| text != before) {
            return;
        }
        tokio::time::sleep(COMMIT_POLL).await;
    }
    panic!("one accepted save reaches its file inside the poll bound");
}

/// Reports whether the editor published the fact of one completed write.
fn published_a_write(editor: &mut Session) -> bool {
    let mut written = false;
    while let Some(published) = editor.take_event() {
        written |= matches!(published.event, EditorEvent::FileWritten { .. });
    }
    written
}

#[tokio::test]
async fn wrong_instance_completion_is_rejected_before_clock_or_result_mutation() {
    let first_root = TempDir::new("driver-owner-first");
    let second_root = TempDir::new("driver-owner-second");
    let mut first_editor = editor_at(&first_root.path);
    let mut second_editor = editor_at(&second_root.path);
    let (first_spawner, first_results) = Runtime::<EditorWork>::new();
    let (second_spawner, second_results) = Runtime::<EditorWork>::new();
    let mut first_driver = EditorDriver::new(first_editor.instance(), first_spawner, first_results);
    let mut second_driver =
        EditorDriver::new(second_editor.instance(), second_spawner, second_results);

    let _ = first_driver.dispatch(&mut first_editor).unwrap();
    let _ = second_driver.dispatch(&mut second_editor).unwrap();
    let completion = timeout(STEP_WAIT, first_driver.recv())
        .await
        .expect("the first matching local request completes");
    let second_completion = timeout(STEP_WAIT, second_driver.recv())
        .await
        .expect("the second matching local request completes");
    while second_editor.take_event().is_some() {}
    assert_eq!(second_editor.clock(), Duration::ZERO);

    let error = second_driver
        .apply(&mut second_editor, completion, Duration::from_secs(60))
        .expect_err("release builds reject another driver's completion");
    assert_eq!(second_editor.clock(), Duration::ZERO);
    assert!(second_editor.take_event().is_none());
    second_driver
        .apply(&mut second_editor, second_completion, Duration::ZERO)
        .expect("rejection leaves the receiver's request state available");
    first_driver
        .apply(&mut first_editor, error.into_completed(), Duration::ZERO)
        .expect("the owner accepts its recovered completion");

    assert!(
        first_driver
            .shutdown(&mut first_editor, SHUTDOWN_WAIT)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        second_driver
            .shutdown(&mut second_editor, SHUTDOWN_WAIT)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_driver_submits_every_syntax_request_through_the_caller_spawner() {
    let directory = TempDir::new("driver-analysis");
    let path = directory.write("main.rs", "fn main() {}\n");
    let mut editor = editor_at(&directory.path);
    let (spawner, results) = Runtime::<EditorWork>::new();
    let mut driver = EditorDriver::new(editor.instance(), spawner, results);

    editor.open_path(path);
    let mut analyses = 0;
    for _ in 0..DRIVER_STEPS_MAX {
        let _ = driver.dispatch(&mut editor);
        assert!(
            editor.take_analysis_request().is_none(),
            "the dispatch hands every syntax request to the spawner, so no \
                 request stays behind for this loop to run"
        );
        let completed = timeout(STEP_WAIT, driver.recv())
            .await
            .expect("every accepted request produces one result");
        if let Outcome::Work(event) = &completed.outcome
            && let Some(event) = event.as_ref()
            && event.request.slot() == ANALYSIS_SLOT
        {
            analyses += 1;
        }
        let _ = driver.apply(&mut editor, completed, NOW);
        if analyses > 0 {
            break;
        }
    }

    assert_eq!(
        analyses, 1,
        "the parse and the highlight query ran on the worker service"
    );
    let drain = driver
        .shutdown(&mut editor, SHUTDOWN_WAIT)
        .await
        .expect("the driver owns this editor");
    assert!(drain.is_none(), "every task of this editor finished");
}

#[tokio::test]
async fn shutdown_drains_a_checkpoint_and_its_ordered_cleanup_successor() {
    let directory = TempDir::new("driver-shutdown-recovery-drain");
    let path = directory.write("main.rs", ORIGINAL);
    let mut editor =
        editor_at(&directory.path).with_recovery_state_directory(directory.join("state"));
    let (spawner, results) = Runtime::<EditorWork>::new();
    let mut driver = EditorDriver::new(editor.instance(), spawner, results);
    open_through(&mut driver, &mut editor, path).await;
    type_one_character(&mut editor);
    let _ = driver.dispatch(&mut editor);
    let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    let _ = driver.dispatch(&mut editor);

    let drain = driver
        .shutdown(&mut editor, SHUTDOWN_WAIT)
        .await
        .expect("the driver owns this editor");

    assert!(drain.is_none());
    assert!(!editor.has_queued_recovery_work());
    assert!(!editor.has_submitted_recovery_work());
}

#[tokio::test]
async fn a_shutdown_waits_for_the_committed_write_and_publishes_its_fact() {
    let directory = TempDir::new("driver-shutdown-write");
    let path = directory.write("main.rs", ORIGINAL);
    let saved = path.clone();
    let mut editor = editor_at(&directory.path);
    let (spawner, results) = Runtime::<EditorWork>::new();
    let mut driver = EditorDriver::new(editor.instance(), spawner, results);
    open_through(&mut driver, &mut editor, path).await;
    type_one_character(&mut editor);

    let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    // The one dispatch refuses the formatter and hands the save behind it to
    // the worker service inside the same iteration.
    let _ = driver.dispatch(&mut editor);
    wait_for_commit(&saved, ORIGINAL).await;
    assert!(published_a_write(&mut editor).not(), "the write still runs");

    // The shutdown cancels every pre-commit request, and the write masks
    // that cancellation, so its reserved slot still publishes its fact.
    let drain = driver
        .shutdown(&mut editor, SHUTDOWN_WAIT)
        .await
        .expect("the driver owns this editor");

    assert!(drain.is_none(), "the write finished inside the deadline");
    assert!(
        published_a_write(&mut editor),
        "a completed write always publishes its mandatory fact"
    );
}

#[tokio::test]
async fn an_expired_deadline_returns_the_drain_that_publishes_the_write() {
    let directory = TempDir::new("driver-shutdown-drain");
    let path = directory.write("main.rs", ORIGINAL);
    let saved = path.clone();
    let mut editor = editor_at(&directory.path);
    let limits = RuntimeLimits::new(EVENT_QUEUE_CAPACITY, 4, 4)
        .expect("every capacity of this fixture is above zero");
    let (spawner, results) = Runtime::<EditorWork>::with_limits(limits);
    // One parked job holds the tracked work past the deadline, so the
    // shutdown below reaches its expiry with certainty.
    let (release, parked) = std::sync::mpsc::channel::<()>();
    let gate = PublicationGate::default();
    let handle = gate.begin(PARKED_SLOT, &spawner.cancellation_root());
    spawner
        .submit_worker(handle, PARKED_DEADLINE, move |_cancellation| {
            let _released = parked.recv();
            EditorWork(WorkResult::HostReport(String::new()))
        })
        .expect("the fresh spawner holds worker capacity");
    let mut driver = EditorDriver::new(editor.instance(), spawner, results);
    open_through(&mut driver, &mut editor, path).await;
    type_one_character(&mut editor);

    let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
    let _ = driver.dispatch(&mut editor);
    wait_for_commit(&saved, ORIGINAL).await;
    let drain = driver
        .shutdown(&mut editor, Duration::ZERO)
        .await
        .expect("the driver owns this editor")
        .expect("the parked job holds the tracked work past the deadline");

    assert!(
        published_a_write(&mut editor).not(),
        "the drain still owns the fact of the write"
    );
    release
        .send(())
        .expect("the parked job still waits for its release");
    let _redraw = drain
        .complete(&mut editor)
        .await
        .expect("the drain owns this editor");

    assert!(
        published_a_write(&mut editor),
        "the resumed drain publishes the mandatory fact before the host \
             stops its runtime"
    );
}

#[test]
fn no_module_beside_the_driver_hands_work_to_a_spawner() {
    // The driver owns every submission, so no other module can start
    // filesystem, process, Git, formatter, or Tree-sitter work. See
    // `docs/responsiveness.md`.
    const SUBMISSIONS: [&str; 3] = [
        "submit_worker(",
        "submit_committing_worker(",
        "submit_process(",
    ];
    let mut checked = 0;
    for path in production_modules() {
        let source = std::fs::read_to_string(&path).expect("every module is readable text");
        for call in SUBMISSIONS {
            assert!(
                !source.contains(call),
                "{} calls {call}, but the driver owns every submission",
                path.display()
            );
        }
        checked += 1;
    }
    assert!(checked > 1, "the check read the modules of this crate");
}

#[test]
fn no_module_of_this_crate_owns_the_terminal() {
    // The `kvim` binary is the only terminal owner. This crate holds the
    // visible state and the presentation of one editor, so it never enters
    // raw mode, never enters the alternate screen, never reads the terminal
    // event stream, never installs a signal handler or a panic hook, and
    // never writes to standard output. See `docs/architecture.md` and
    // `docs/embedding.md`.
    const TERMINAL_OWNERS: [&str; 8] = [
        "TerminalSession",
        "CrosstermControl",
        "TerminationSource",
        "EventSource",
        "CrosstermBackend",
        "enable_raw_mode",
        "EnterAlternateScreen",
        "set_hook",
    ];
    let mut checked = 0;
    for path in production_modules() {
        let source = std::fs::read_to_string(&path).expect("every module is readable text");
        for owner in TERMINAL_OWNERS {
            assert!(
                !source.contains(owner),
                "{} names {owner}, but the binary owns the terminal",
                path.display()
            );
        }
        checked += 1;
    }
    assert!(checked > 1, "the check read the modules of this crate");
}

#[test]
fn the_session_builds_the_analysis_request_and_never_runs_it() {
    // `session.rs` owns the inert `AnalysisRequest` value and calls the
    // adapter once, inside `AnalysisRequest::run`. The worker service is the
    // only caller of that method, and the check above proves that the driver
    // owns every submission. The terminal event loop of the `kvim` binary
    // therefore names no syntax value at all, and its own guard proves that.
    let owner = std::fs::read_to_string(source_directory().join("session.rs"))
        .expect("the session module is readable text");
    assert_eq!(
        owner.matches(".analyze(").count(),
        1,
        "one call of the adapter exists, and `AnalysisRequest::run` owns it"
    );
    assert_eq!(
        owner.matches("AnalysisRequest::run").count(),
        0,
        "the session builds the request and never runs it"
    );
}

#[test]
fn the_two_review_captures_hold_their_own_publication_slots() {
    // One capture takes several commands, so the two halves run at the same
    // time. One shared slot would cancel the half that started first, and the
    // review would then publish one half alone and open one tab.
    assert_ne!(
        DIFF_STAGED_SLOT, DIFF_UNSTAGED_SLOT,
        "each half of the review needs its own slot"
    );
}
