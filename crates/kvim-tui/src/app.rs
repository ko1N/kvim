//! The terminal event loop.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! The loop is the imperative shell of the editor. It owns the terminal, reads
//! normalized events, applies pure transitions to one [`Session`], and renders
//! after a visible state change. It runs no unconditional frame loop, and it
//! performs no filesystem, process, or language work. See
//! `docs/responsiveness.md`.

use std::io::{self, stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use thiserror::Error;
use tokio::time::sleep;

use kvim_input::Mode;
use kvim_language::{
    ANALYSIS_DEADLINE, FormattedDocument, FormatterFailure, LanguageEvent, LanguageRegistry,
    LanguageServices, LspError,
};
use kvim_runtime::{
    FileWatcher, ProcessOutput, PublicationGate, RequestSlot, Runtime, RuntimeError, RuntimeEvent,
    SubmitError, WORKER_DEADLINE_DEFAULT, WatchBatch,
};
use kvim_settings::EditorSettings;
use kvim_terminal::{
    CrosstermControl, CursorShape, EventSource, TerminalControl, TerminalError, TerminalEvent,
    TerminalSession, TerminationSource,
};
use kvim_workspace::{
    BUFFERS_MAX, FileResult, GitStatusFailure, GitStatusSnapshot, PickerResult, PickerSlot,
    WorkspaceResult,
};

use super::clipboard::{SessionClipboard, command_failure, refused_submission};
use super::language::{LANGUAGE_OUTBOX_MAX, send_request};
use super::picker::PickerFailure;
use super::session::{
    AnalysisResult, FileRequestFailure, JOB_ANALYSIS, JOB_OBSOLETE, JOB_REFUSED, JOB_WALK,
    MessageLevel, Redraw, RunState, Session,
};
use super::tree::GENERATED_NAMES;

/// The number of consecutive terminal read failures that ends the editor.
///
/// One failure keeps the source usable, so the loop reports it and reads again.
/// A run of failures means the terminal is gone, and the bound keeps the loop
/// from spinning forever.
pub const EVENT_ERRORS_MAX: usize = 8;

/// The publication slot of every file operation.
///
/// The editor runs one file operation at a time, so one slot holds every open
/// and every save. A newer request cancels the older request in this slot.
const FILE_SLOT: RequestSlot = RequestSlot::new(1);

/// The publication slot of every buffer analysis.
///
/// One slot holds the analysis of the active buffer, so a newer buffer version
/// cancels the parse of the version that it replaced.
const ANALYSIS_SLOT: RequestSlot = RequestSlot::new(2);

/// The publication slot of every workspace operation.
///
/// The file tree runs one directory read or one mutation at a time, so one slot
/// holds every workspace operation.
const WORKSPACE_SLOT: RequestSlot = RequestSlot::new(3);

/// The publication slot of the candidates of the open picker.
///
/// A newer query cancels the search that it replaces, so the obsolete `rg`
/// process stops and its result never reaches the screen.
const PICKER_SLOT: RequestSlot = RequestSlot::new(4);

/// The publication slot of the preview of the open picker.
///
/// A newer selection cancels the preview that it replaces.
const PREVIEW_SLOT: RequestSlot = RequestSlot::new(5);

/// The publication slot of every system clipboard command.
///
/// The session runs one clipboard operation at a time, so one slot holds every
/// write and every read. A newer operation cancels the command that it
/// replaces. See `docs/clipboard.md`.
const CLIPBOARD_SLOT: RequestSlot = RequestSlot::new(6);

/// The publication slot of the Git status of the workspace.
///
/// The file tree runs one status read at a time, so a newer trigger cancels the
/// read that it replaces and the gate rejects the obsolete result. See
/// `docs/git.md`.
const GIT_SLOT: RequestSlot = RequestSlot::new(7);

/// The publication slot of the external formatter of one buffer.
///
/// A save waits for its formatter answer, and the session starts no second
/// format while one runs, so one slot holds every formatter run. See
/// `docs/language-services.md`.
const FORMAT_SLOT: RequestSlot = RequestSlot::new(8);

/// The publication slot of the workspace walk of the command-line completion.
///
/// One open command line asks for one walk, so a newer command line cancels the
/// walk of the line that it replaces and the gate rejects the obsolete result.
/// See `docs/files.md`.
const COMPLETION_SLOT: RequestSlot = RequestSlot::new(9);

/// The picker requests that one loop iteration submits.
///
/// One transition produces at most one candidate request and one preview
/// request, so the bound covers every request that it can produce.
const PICKER_DISPATCH_MAX: usize = 2;

/// The workspace requests that one loop iteration submits.
///
/// The file tree runs one workspace operation at a time, so one transition
/// produces at most one directory read or one mutation. The bound keeps the
/// submission loop finite even if a later change let the tree offer the same
/// read again, so a defect of that shape can never hang the event loop. See
/// `docs/responsiveness.md`.
const WORKSPACE_DISPATCH_MAX: usize = 1;

/// The language requests that one loop iteration sends.
///
/// The session holds a bounded outbox and one fresh open for each loaded
/// buffer, so this bound covers every request that one transition can produce.
const LANGUAGE_DISPATCH_MAX: usize = LANGUAGE_OUTBOX_MAX + BUFFERS_MAX;

/// The submission passes that one loop iteration runs.
///
/// One pass can queue the work of another owner: a formatting request that no
/// language server accepts completes the save that waited for it, and that save
/// must reach the worker service inside the same iteration. A single pass would
/// leave the save in its outbox until the next terminal event, so the write and
/// its report would follow the next key instead of the command. Two passes
/// cover that chain, because the second pass only reports a refusal on the
/// message line and queues no further work. The bound keeps the pass finite, so
/// a request that its service offers again can never hold the loop. See
/// `docs/responsiveness.md`.
const DISPATCH_PASSES_MAX: usize = 2;

/// One completed background operation of the editor.
///
/// The runtime is generic over its result, and the editor submits both file work
/// and language work, so one value names both.
enum WorkResult {
    /// One file operation finished.
    File(FileResult),
    /// One buffer analysis finished.
    Analysis(AnalysisResult),
    /// One workspace operation of the file tree finished.
    Workspace(WorkspaceResult),
    /// One picker operation finished.
    Picker(PickerResult),
    /// The workspace walk of the command-line completion finished.
    Completion(PickerResult),
    /// One system clipboard command finished.
    Clipboard(ProcessOutput),
    /// One Git status read of the workspace finished.
    Git(Result<GitStatusSnapshot, GitStatusFailure>),
    /// One run of the external formatter of one buffer finished.
    Format(Result<Option<FormattedDocument>, FormatterFailure>),
}

/// A failure that ends the editor.
#[derive(Debug, Error)]
pub enum EditorError {
    /// A terminal setup or restore step failed.
    #[error("the terminal control step failed")]
    Terminal(#[source] TerminalError),
    /// Writing one frame to the terminal failed.
    #[error("the terminal draw failed")]
    Draw(#[source] io::Error),
    /// The terminal event stream failed repeatedly.
    #[error("the terminal event stream failed {EVENT_ERRORS_MAX} times in a row")]
    EventStream(#[source] TerminalError),
}

/// Whether the editor panics on purpose after its first frame.
///
/// The probe proves that the panic hook of the terminal session restores the
/// terminal, because a panic aborts without running a destructor on some
/// platforms. It is a diagnostic, not an editor feature. See
/// `docs/architecture.md`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PanicProbe {
    /// Run the editor normally.
    #[default]
    Disabled,
    /// Panic after the first frame reaches the terminal.
    AfterFirstFrame,
}

/// The outcome of one loop iteration.
enum Step {
    /// The iteration applied a transition.
    Handled(Redraw),
    /// The editor must leave its event loop, because the terminal event stream
    /// ended or the process received a termination signal.
    Stop,
    /// The terminal event stream reported one failure.
    Failed(TerminalError),
}

/// Runs the editor until it closes its last window.
///
/// The terminal returns to its original state on every exit path, including a
/// panic and a termination signal. The panic hook of the terminal session owns
/// the panic path, because a panic aborts without running a destructor on some
/// platforms. A termination signal leaves the event loop instead, so the
/// restore below writes the same steps as an ordinary exit.
///
/// The caller resolves the workspace root, because the language services
/// perform no filesystem lookup. The root is the containment boundary of every
/// document that a language server sees. A root that is not absolute leaves the
/// editor fully usable with no language services. See
/// `docs/language-services.md`.
///
/// # Errors
///
/// Returns [`EditorError`] when a terminal control step fails, when a draw
/// fails, or when the event stream fails [`EVENT_ERRORS_MAX`] times in a row.
pub async fn run(
    settings: EditorSettings,
    root: PathBuf,
    path: Option<PathBuf>,
    probe: PanicProbe,
) -> Result<(), EditorError> {
    let mut terminal =
        TerminalSession::enter(CrosstermControl::new()).map_err(EditorError::Terminal)?;
    // The listener registers the process signal handlers, so it belongs beside
    // the terminal setup and not inside the loop.
    let terminations = TerminationSource::from_process();
    let outcome = drive(&mut terminal, terminations, settings, root, path, probe).await;
    terminal.restore().map_err(EditorError::Terminal)?;
    outcome
}

/// Returns the cursor shape that one editor mode shows.
///
/// Insert mode shows a vertical bar, and every other mode shows a block. See
/// `docs/windows.md`.
const fn cursor_shape(mode: Mode) -> CursorShape {
    match mode {
        Mode::Insert => CursorShape::Bar,
        Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock => CursorShape::Block,
    }
}

/// Drives one editor session over the process terminal.
///
/// The loop leaves on the first termination request of `terminations`, so a
/// `SIGTERM`, a `SIGINT`, or a `SIGHUP` ends the editor through the ordinary
/// shutdown and the ordinary restore. See `docs/responsiveness.md`.
async fn drive<C: TerminalControl>(
    session: &mut TerminalSession<C>,
    mut terminations: TerminationSource,
    settings: EditorSettings,
    root: PathBuf,
    path: Option<PathBuf>,
    probe: PanicProbe,
) -> Result<(), EditorError> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout())).map_err(EditorError::Draw)?;
    let size = terminal.size().map_err(EditorError::Draw)?;
    // The clipboard selection reads the platform and the executable search path
    // once, here at the start, so no operation ever guesses. The editor and the
    // registers name no platform, no command, and no selection. See
    // `docs/clipboard.md`.
    let mut editor = Session::new(
        Rect::new(0, 0, size.width, size.height),
        settings,
        root.clone(),
    )
    .with_clipboard(SessionClipboard::detect());
    let mut events = EventSource::from_terminal();
    // The file operations and the buffer analysis run on the bounded worker
    // service, so the loop below performs no filesystem work and no parsing.
    // See `docs/responsiveness.md`.
    let (runtime, mut results) = Runtime::<WorkResult>::new();
    // The language sessions run as background tasks of this runtime context, so
    // the loop below never reads, writes, or waits for a server. A root that
    // this constructor refuses leaves the editor fully usable without them.
    let mut language =
        LanguageServices::new(LanguageRegistry::first_release(), root.clone(), settings).ok();
    // The watcher runs its platform callback and its coalescing task beside this
    // loop, so no filesystem event ever reaches it directly. It ignores the
    // generated directory names of the file tree, so it watches no build output
    // directory and one build writes no event.
    // The start places no watch and reads no directory. The coalescing task
    // walks the workspace after the first frame, so a large workspace delays no
    // frame. That task then reports the window that no watch covered, and the
    // burst that reports it reads the workspace again.
    // A host that refuses the watch leaves the editor fully usable with the
    // refresh command. A registration that covers a part of the workspace
    // reports that state with the burst that opens the stream.
    // See `docs/files.md` and `docs/responsiveness.md`.
    let mut watcher = FileWatcher::start(root, &GENERATED_NAMES).ok();
    if watcher.is_none() {
        let _ = editor.report_watch_unavailable();
    }
    let gate = PublicationGate::default();
    let start = Instant::now();
    let mut errors = 0;

    if let Some(path) = path {
        editor.open_path(path);
    }
    // The first frame follows this dispatch unconditionally, so its report needs
    // no redraw request of its own.
    let _ = dispatch(&mut editor, &runtime, &gate, &mut language);
    // The shape follows the mode, so the editor writes it once at the start and
    // then only after a mode change. The sequence is decoration: a terminal that
    // ignores it still shows its own cursor.
    let mut shape = cursor_shape(editor.mode());
    let _ = session.set_cursor_shape(shape);
    terminal
        .draw(|frame| editor.render(frame))
        .map_err(EditorError::Draw)?;
    assert!(
        probe == PanicProbe::Disabled,
        "the panic probe of the environment asks for this panic"
    );
    // The elapsed time alone drives one state change, so the loop runs one
    // catch-up transition for each deadline that already passed. It records that
    // deadline: a transition that leaves the same deadline behind must not run
    // again, or the loop would never await a terminal event and the editor would
    // stop serving input. See `docs/responsiveness.md`.
    let mut caught_up: Option<Duration> = None;
    while editor.run_state() == RunState::Running {
        let now = start.elapsed();
        let step = match editor.next_deadline() {
            Some(deadline) if deadline > now => {
                tokio::select! {
                    event = events.next_event() => apply(&mut editor, event, start.elapsed()),
                    result = results.recv() => complete(&mut editor, &gate, result, start.elapsed()),
                    outcome = next_language_event(&mut language) => {
                        publish(&mut editor, outcome, start.elapsed())
                    }
                    batch = next_watch_batch(&mut watcher) => {
                        publish_watch(&mut editor, batch.as_ref(), start.elapsed())
                    }
                    _ = terminations.recv() => Step::Stop,
                    () = sleep(deadline - now) => Step::Handled(editor.tick(start.elapsed())),
                }
            }
            // The deadline already passed, so the transition runs before the
            // loop waits for another event.
            Some(deadline) if caught_up != Some(deadline) => {
                caught_up = Some(deadline);
                Step::Handled(editor.tick(now))
            }
            // The deadline stayed, so no further transition follows from the
            // elapsed time alone. The loop waits for an event instead.
            Some(_) | None => {
                tokio::select! {
                    event = events.next_event() => apply(&mut editor, event, start.elapsed()),
                    result = results.recv() => complete(&mut editor, &gate, result, start.elapsed()),
                    outcome = next_language_event(&mut language) => {
                        publish(&mut editor, outcome, start.elapsed())
                    }
                    batch = next_watch_batch(&mut watcher) => {
                        publish_watch(&mut editor, batch.as_ref(), start.elapsed())
                    }
                    _ = terminations.recv() => Step::Stop,
                }
            }
        };
        // A refused submission reports its state on the message line, so the
        // dispatch owns a visible change of its own and the frame must follow
        // it as well as the transition above.
        let dispatched = dispatch(&mut editor, &runtime, &gate, &mut language);
        let next_shape = cursor_shape(editor.mode());
        if next_shape != shape {
            shape = next_shape;
            let _ = session.set_cursor_shape(shape);
        }
        match step {
            Step::Handled(handled) => {
                errors = 0;
                if handled.or(dispatched) == Redraw::Needed {
                    terminal
                        .draw(|frame| editor.render(frame))
                        .map_err(EditorError::Draw)?;
                }
            }
            Step::Stop => break,
            Step::Failed(error) => {
                errors += 1;
                if errors >= EVENT_ERRORS_MAX {
                    shutdown(runtime, language, watcher).await;
                    return Err(EditorError::EventStream(error));
                }
            }
        }
    }
    shutdown(runtime, language, watcher).await;
    Ok(())
}

/// Cancels every background service and waits for its cleanup.
///
/// Every operation consumes its owner, so no caller can submit after them. The
/// watcher stops first, because a stopped watch queues no further directory
/// read for the services below it. See `docs/responsiveness.md`.
async fn shutdown(
    runtime: Runtime<WorkResult>,
    language: Option<LanguageServices>,
    watcher: Option<FileWatcher>,
) {
    if let Some(watcher) = watcher {
        watcher.shutdown().await;
    }
    if let Some(language) = language {
        language.shutdown().await;
    }
    runtime.shutdown().await;
}

/// Waits for the next result of the language services.
///
/// The future never completes while the editor runs without language services,
/// so the loop then waits for a terminal event alone.
async fn next_language_event(language: &mut Option<LanguageServices>) -> LanguageEvent {
    match language {
        Some(language) => match language.recv().await {
            Some(event) => event,
            // The services hold their own sender, so the queue never closes.
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

/// Waits for the next coalesced burst of the workspace watcher.
///
/// The future never completes while the editor runs without a watcher, so the
/// loop then waits for its other events alone.
///
/// Returns `None` once when the watch ended, which happens when the platform
/// refused the deferred registration. The call drops the ended watcher, so the
/// loop reports that state once and then waits for its other events alone.
async fn next_watch_batch(watcher: &mut Option<FileWatcher>) -> Option<WatchBatch> {
    let Some(active) = watcher else {
        return std::future::pending().await;
    };
    match active.recv().await {
        Some(batch) => Some(batch),
        None => {
            // The coalescing task ended, so no further burst can arrive. It
            // dropped the platform watcher before it closed this stream, so no
            // callback thread outlives this value.
            *watcher = None;
            None
        }
    }
}

/// Applies one coalesced burst of workspace filesystem changes.
///
/// A burst that never arrived reports that no watcher observes the workspace,
/// because the deferred registration failed. The editor stays fully usable and
/// the refresh command reads the workspace by hand. See `docs/files.md`.
fn publish_watch(editor: &mut Session, batch: Option<&WatchBatch>, now: Duration) -> Step {
    editor.advance_clock(now);
    match batch {
        Some(batch) => Step::Handled(editor.apply_watch_batch(batch)),
        None => Step::Handled(editor.report_watch_unavailable()),
    }
}

/// Applies one typed result of the language services.
///
/// The loop reports the elapsed time first, because a progress report and a
/// message both need it and neither carries a time of its own.
fn publish(editor: &mut Session, event: LanguageEvent, now: Duration) -> Step {
    editor.advance_clock(now);
    Step::Handled(editor.apply_language_event(event))
}

/// Hands the queued language requests to the language services.
///
/// Every call returns at once. The services own the process, the deadlines, and
/// the protocol bounds, so the loop never reads, writes, or waits for a server.
///
/// A refused request reaches the session as a typed failure, which reports the
/// state on the message line and completes a save that waited for a formatter.
/// The returned value carries that visible change to the caller, so the frame
/// follows the dispatch and not the next key.
fn submit_language_work(
    editor: &mut Session,
    mut language: Option<&mut LanguageServices>,
) -> Redraw {
    let mut redraw = Redraw::Skipped;
    for _ in 0..LANGUAGE_DISPATCH_MAX {
        let Some(request) = editor.take_language_request() else {
            return redraw;
        };
        let kind = request.kind();
        let result = match language.as_deref_mut() {
            // One language can run several servers, so the request reaches
            // every running session of its path.
            Some(language) => language
                .sessions(request.path())
                .and_then(|handles| send_request(&handles, &request)),
            // A workspace root that the services refused leaves the editor
            // usable with no language service at all.
            None => Err(LspError::NoServerDeclared),
        };
        redraw = redraw.or(editor.apply_language_dispatch(kind, result));
    }
    debug_assert!(
        editor.take_language_request().is_none(),
        "one transition produces fewer requests than the dispatch bound"
    );
    redraw
}

/// Hands every queued request of one iteration to the service that runs it.
///
/// A pass can queue the work of another owner, so the dispatch repeats inside
/// [`DISPATCH_PASSES_MAX`]. The returned value reports every visible change that
/// a refused submission produced, because a refusal names its state on the
/// message line and the frame must follow that report.
fn dispatch(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
    language: &mut Option<LanguageServices>,
) -> Redraw {
    let mut redraw = Redraw::Skipped;
    for _ in 0..DISPATCH_PASSES_MAX {
        redraw = redraw.or(submit_background_work(editor, runtime, gate));
        redraw = redraw.or(submit_language_work(editor, language.as_mut()));
    }
    redraw
}

/// Hands the queued file and analysis jobs to the bounded worker service.
///
/// A refused submission reaches the session as a typed failure, so the editor
/// keeps its previous visible state and reports the refusal.
fn submit_background_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) -> Redraw {
    submit_file_work(editor, runtime, gate)
        .or(submit_analysis_work(editor, runtime, gate))
        .or(submit_workspace_work(editor, runtime, gate))
        .or(submit_picker_work(editor, runtime, gate))
        .or(submit_completion_work(editor, runtime, gate))
        .or(submit_clipboard_work(editor, runtime, gate))
        .or(submit_git_work(editor, runtime, gate))
        .or(submit_format_work(editor, runtime, gate))
}

/// Hands the queued formatter run to the bounded process service.
///
/// The program reads the buffer and writes the formatted document, so it never
/// runs on this loop. A refused submission returns to the session as a typed
/// failure, which saves the unformatted content. See
/// `docs/language-services.md`.
fn submit_format_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_format_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(FORMAT_SLOT, &runtime.cancellation_root());
    let command = request.command();
    let submitted = runtime.submit_process(handle, command, move |output| {
        WorkResult::Format(request.publish(&output))
    });
    if submitted.is_err() {
        return editor.apply_format_result(Err(FormatterFailure::Unavailable));
    }
    Redraw::Skipped
}

/// Hands the queued Git status read to the bounded process service.
///
/// The command reads the repository, so it never runs on this loop. A refused
/// submission returns to the session as a typed failure, which keeps the marks
/// of the last successful read. See `docs/git.md`.
fn submit_git_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_git_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(GIT_SLOT, &runtime.cancellation_root());
    let command = request.command();
    let submitted = runtime.submit_process(handle, command, move |output| {
        WorkResult::Git(request.publish(&output))
    });
    if submitted.is_err() {
        return editor.apply_git_result(Err(GitStatusFailure::Unavailable));
    }
    Redraw::Skipped
}

/// Returns the typed Git failure of one runtime failure.
///
/// A command that cannot start is a normal state: the editor names it once and
/// stays usable without the repository state.
const fn git_failure(error: &RuntimeError) -> GitStatusFailure {
    match error {
        RuntimeError::ProcessSpawn(_) => GitStatusFailure::CommandMissing,
        RuntimeError::Cancelled
        | RuntimeError::Timeout
        | RuntimeError::WorkerFailure(_)
        | RuntimeError::ProcessRead(_)
        | RuntimeError::ProcessWrite(_)
        | RuntimeError::OutputLimit { .. } => GitStatusFailure::Unavailable,
    }
}

/// Hands the queued clipboard command to the bounded process service.
///
/// The command reaches the system clipboard, so it never runs on this loop. A
/// refused submission returns to the session as a typed failure, which keeps
/// the unnamed register and lets a deferred paste fall back to it. See
/// `docs/clipboard.md`.
fn submit_clipboard_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(command) = editor.take_clipboard_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(CLIPBOARD_SLOT, &runtime.cancellation_root());
    let submitted = runtime.submit_process(handle, command, WorkResult::Clipboard);
    if let Err(error) = submitted {
        return editor.apply_clipboard_result(Err(refused_submission(error)));
    }
    Redraw::Skipped
}

/// Hands the queued picker work to the bounded worker and process services.
///
/// A workspace walk and a preview read are worker jobs. A ripgrep search is an
/// external command, so it reaches the process service instead. Both slots
/// cancel the request that a newer one replaces.
fn submit_picker_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) -> Redraw {
    let mut redraw = Redraw::Skipped;
    for _ in 0..PICKER_DISPATCH_MAX {
        let Some(request) = editor.take_picker_request() else {
            return redraw;
        };
        let slot = request.slot();
        let deadline = request.deadline();
        let handle = gate.begin(publication_slot(slot), &runtime.cancellation_root());
        let submitted = match request.command() {
            Some(command) => runtime.submit_process(handle, command, move |output| {
                WorkResult::Picker(request.publish(&output))
            }),
            None => runtime.submit_worker(handle, deadline, move |cancellation| {
                WorkResult::Picker(request.run(&cancellation))
            }),
        };
        if let Err(error) = submitted {
            redraw = redraw.or(editor.abandon_picker_request(
                slot,
                match error {
                    SubmitError::Saturated(_) => PickerFailure::Saturated,
                    SubmitError::InvalidLimits
                    | SubmitError::ProcessBounds
                    | SubmitError::ShuttingDown => PickerFailure::Cancelled,
                },
            ));
        }
    }
    debug_assert!(
        editor.take_picker_request().is_none(),
        "one transition produces fewer picker requests than the dispatch bound"
    );
    redraw
}

/// Hands the workspace walk of the command-line completion to the worker.
///
/// The walk is the same job that the file picker submits, so one walk serves
/// both. The command line reads no directory and waits for no result, so a
/// refused submission leaves it without a path list and reports nothing. See
/// `docs/files.md`.
fn submit_completion_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_completion_request() else {
        return Redraw::Skipped;
    };
    let deadline = request.deadline();
    let handle = gate.begin(COMPLETION_SLOT, &runtime.cancellation_root());
    // A refusal leaves the completion in the state that it already holds, so
    // the editor has nothing to clear and nothing to report.
    let _refused = runtime.submit_worker(handle, deadline, move |cancellation| {
        WorkResult::Completion(request.run(&cancellation))
    });
    Redraw::Skipped
}

/// Returns the publication slot of one picker operation.
const fn publication_slot(slot: PickerSlot) -> RequestSlot {
    match slot {
        PickerSlot::Candidates => PICKER_SLOT,
        PickerSlot::Preview => PREVIEW_SLOT,
    }
}

/// Returns the typed picker failure of one runtime failure.
///
/// A command that cannot start is a normal state: the editor reports it and
/// stays usable without the search picker.
const fn picker_failure(error: &RuntimeError) -> PickerFailure {
    match error {
        RuntimeError::Timeout => PickerFailure::Timeout,
        RuntimeError::ProcessSpawn(_) => PickerFailure::CommandMissing,
        RuntimeError::Cancelled
        | RuntimeError::WorkerFailure(_)
        | RuntimeError::ProcessRead(_)
        | RuntimeError::ProcessWrite(_)
        | RuntimeError::OutputLimit { .. } => PickerFailure::Cancelled,
    }
}

/// Hands the queued directory read or mutation to the bounded worker service.
///
/// A refused submission reaches the session as a typed failure, so the file
/// tree keeps the state that it held before the request.
fn submit_workspace_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) -> Redraw {
    let mut redraw = Redraw::Skipped;
    for _ in 0..WORKSPACE_DISPATCH_MAX {
        let Some(request) = editor.take_workspace_request() else {
            return redraw;
        };
        let handle = gate.begin(WORKSPACE_SLOT, &runtime.cancellation_root());
        let submitted = runtime.submit_worker(handle, WORKER_DEADLINE_DEFAULT, |_cancellation| {
            WorkResult::Workspace(request.run())
        });
        // A refused submission clears the pending state of the tree, so the
        // next transition offers the read again instead of waiting for a result
        // that never arrives.
        if let Err(error) = submitted {
            redraw = redraw.or(editor.abandon_workspace_request(match error {
                SubmitError::Saturated(_) => FileRequestFailure::Saturated,
                SubmitError::InvalidLimits
                | SubmitError::ProcessBounds
                | SubmitError::ShuttingDown => FileRequestFailure::Cancelled,
            }));
        }
    }
    redraw
}

/// Hands the queued file request to the bounded worker service.
fn submit_file_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_file_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(FILE_SLOT, &runtime.cancellation_root());
    let submitted = runtime.submit_worker(handle, WORKER_DEADLINE_DEFAULT, |_cancellation| {
        WorkResult::File(request.run())
    });
    if let Err(error) = submitted {
        return editor.abandon_file_request(match error {
            SubmitError::Saturated(_) => FileRequestFailure::Saturated,
            SubmitError::InvalidLimits | SubmitError::ProcessBounds | SubmitError::ShuttingDown => {
                FileRequestFailure::Cancelled
            }
        });
    }
    Redraw::Skipped
}

/// Hands the analysis of the active buffer to the bounded worker service.
///
/// Highlighting is decoration, so a refused submission only frees the request
/// again and paints nothing. The next transition asks for it once more.
fn submit_analysis_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_analysis_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(ANALYSIS_SLOT, &runtime.cancellation_root());
    let submitted = runtime.submit_worker(handle, ANALYSIS_DEADLINE, move |cancellation| {
        WorkResult::Analysis(request.run(&cancellation))
    });
    if submitted.is_err() {
        // The refusal paints nothing, so the log is the one place that holds
        // it. See `docs/responsiveness.md`.
        editor.record_job(JOB_ANALYSIS, MessageLevel::Warning, JOB_REFUSED);
        editor.abandon_analysis_request();
    }
    Redraw::Skipped
}

/// Returns the log outcome of one runtime failure.
///
/// Every outcome is one fixed text, so a job that fails the same way twice
/// collapses into one log entry. See `docs/windows.md`.
const fn job_outcome(error: &RuntimeError) -> &'static str {
    match error {
        RuntimeError::Timeout => "passed its deadline",
        RuntimeError::Cancelled => "was cancelled",
        RuntimeError::WorkerFailure(_) => "failed inside its worker",
        RuntimeError::ProcessSpawn(_) => "did not start",
        RuntimeError::ProcessRead(_) | RuntimeError::ProcessWrite(_) => "lost its pipe",
        RuntimeError::OutputLimit { .. } => "wrote more than its output limit",
    }
}

/// Returns the log severity of one runtime failure.
///
/// A newer request in the same slot cancels the older one, so a cancelled job
/// is a normal state. Every other failure needs attention.
const fn job_level(error: &RuntimeError) -> MessageLevel {
    match error {
        RuntimeError::Cancelled => MessageLevel::Info,
        RuntimeError::Timeout
        | RuntimeError::WorkerFailure(_)
        | RuntimeError::ProcessSpawn(_)
        | RuntimeError::ProcessRead(_)
        | RuntimeError::ProcessWrite(_)
        | RuntimeError::OutputLimit { .. } => MessageLevel::Warning,
    }
}

/// Applies one result of the bounded worker service.
fn complete(
    editor: &mut Session,
    gate: &PublicationGate,
    event: Option<RuntimeEvent<WorkResult>>,
    now: Duration,
) -> Step {
    editor.advance_clock(now);
    let Some(event) = event else {
        // The runtime is gone, so no further result can arrive.
        return Step::Handled(Redraw::Skipped);
    };
    if !gate.accepts(&event.request) {
        // A newer request owns the slot, so this result is obsolete. The log
        // records the analysis slot alone, because the log collapses one
        // repeated report and two obsolete kinds that alternate would collapse
        // into nothing. See `docs/responsiveness.md`.
        if event.request.slot() == ANALYSIS_SLOT {
            editor.record_job(JOB_ANALYSIS, MessageLevel::Info, JOB_OBSOLETE);
        }
        return Step::Handled(Redraw::Skipped);
    }
    let analysis = event.request.slot() == ANALYSIS_SLOT;
    let workspace = event.request.slot() == WORKSPACE_SLOT;
    let clipboard = event.request.slot() == CLIPBOARD_SLOT;
    let git = event.request.slot() == GIT_SLOT;
    let format = event.request.slot() == FORMAT_SLOT;
    let completion = event.request.slot() == COMPLETION_SLOT;
    let picker = if event.request.slot() == PICKER_SLOT {
        Some(PickerSlot::Candidates)
    } else if event.request.slot() == PREVIEW_SLOT {
        Some(PickerSlot::Preview)
    } else {
        None
    };
    let failure = |error: &RuntimeError| match error {
        RuntimeError::Timeout => FileRequestFailure::Timeout,
        // A cancelled request and a failed worker both leave the buffer
        // unchanged, so the editor stays usable and the user can try again.
        RuntimeError::Cancelled
        | RuntimeError::WorkerFailure(_)
        | RuntimeError::ProcessSpawn(_)
        | RuntimeError::ProcessRead(_)
        | RuntimeError::ProcessWrite(_)
        | RuntimeError::OutputLimit { .. } => FileRequestFailure::Cancelled,
    };
    Step::Handled(match (picker, event.result) {
        (_, Ok(WorkResult::File(result))) => editor.apply_file_result(result),
        (_, Ok(WorkResult::Analysis(result))) => editor.apply_analysis_result(result),
        (_, Ok(WorkResult::Workspace(result))) => editor.apply_workspace_result(result),
        (_, Ok(WorkResult::Picker(result))) => editor.apply_picker_result(result),
        (_, Ok(WorkResult::Completion(result))) => editor.apply_completion_result(result),
        (_, Ok(WorkResult::Clipboard(output))) => editor.apply_clipboard_result(Ok(output)),
        (_, Ok(WorkResult::Git(result))) => editor.apply_git_result(result),
        (_, Ok(WorkResult::Format(result))) => editor.apply_format_result(result),
        (Some(slot), Err(error)) => editor.abandon_picker_request(slot, picker_failure(&error)),
        // A clipboard command that fails, times out, or is cancelled keeps the
        // unnamed register, so the yank or the paste still holds its value.
        (None, Err(error)) if clipboard => {
            editor.apply_clipboard_result(Err(command_failure(&error)))
        }
        // An analysis that fails, times out, or is cancelled renders plain text
        // and reports nothing, because highlighting is decoration. The log
        // names the outcome instead.
        (None, Err(error)) if analysis => {
            editor.record_job(JOB_ANALYSIS, job_level(&error), job_outcome(&error));
            editor.abandon_analysis_request();
            Redraw::Skipped
        }
        // A status read that fails, times out, or is cancelled keeps the marks
        // of the last successful read, because they are decoration.
        (None, Err(error)) if git => editor.apply_git_result(Err(git_failure(&error))),
        // A formatter that fails, times out, or is cancelled leaves the buffer
        // as the user typed it, and the save that waited for it still runs.
        (None, Err(error)) if format => {
            editor.apply_format_result(Err(FormatterFailure::of(&error)))
        }
        // A walk that fails, times out, or is cancelled leaves the command line
        // without a path list. The user still types the path in full, so the
        // editor keeps nothing to clear and reports nothing. The log names the
        // outcome instead.
        (None, Err(error)) if completion => {
            editor.record_job(JOB_WALK, job_level(&error), job_outcome(&error));
            Redraw::Skipped
        }
        (None, Err(error)) if workspace => editor.abandon_workspace_request(failure(&error)),
        (None, Err(error)) => editor.abandon_file_request(failure(&error)),
    })
}

/// Applies one read from the terminal event source.
fn apply(
    editor: &mut Session,
    event: Option<Result<TerminalEvent, TerminalError>>,
    now: Duration,
) -> Step {
    match event {
        Some(Ok(event)) => Step::Handled(editor.handle_event(event, now)),
        Some(Err(error)) => Step::Failed(error),
        None => Step::Stop,
    }
}

#[cfg(test)]
mod tests {
    use kvim_terminal::{Key, KeyCode, TerminalEvent};
    use kvim_workspace::temp::TempDir;
    use tokio::time::timeout;

    use super::*;

    /// The elapsed time that every transition of these tests reports.
    const NOW: Duration = Duration::ZERO;

    /// The absolute root that no host holds, so its registration always fails.
    const MISSING_ROOT: &str = "/kvim-app-root-that-never-exists";

    /// The time that one test waits for the refused registration.
    const REGISTRATION_WAIT: Duration = Duration::from_secs(5);

    /// The time that one test waits for a future that must never complete.
    const PARKED_WAIT: Duration = Duration::from_millis(50);

    /// The report of a workspace that no watcher observes.
    const WATCH_MISSING_NOTE: &str =
        "the workspace watcher could not start; the file tree updates on a refresh";

    #[tokio::test]
    async fn one_dispatch_hands_a_refused_format_and_the_save_behind_it_to_their_services() {
        let directory = TempDir::new("app-dispatch-save");
        let path = directory.write("main.rs", "one\n");
        let mut settings = EditorSettings::default();
        settings.files.undo_file = false;
        let root = path
            .parent()
            .expect("the temporary file holds a parent directory")
            .to_path_buf();
        let mut editor = Session::new(Rect::new(0, 0, 80, 24), settings, root);
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

        let (runtime, _results) = Runtime::<WorkResult>::new();
        let gate = PublicationGate::default();
        // The editor runs without language services, so the formatter request of
        // the save reaches no server.
        let mut language = None;
        let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
        let redraw = dispatch(&mut editor, &runtime, &gate, &mut language);

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
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn one_dispatch_runs_the_external_formatter_and_the_save_behind_it() {
        let directory = TempDir::new("app-dispatch-format");
        // The Nix adapter declares an external formatter, so the save reaches
        // the bounded process service instead of a language server.
        let path = directory.write("flake.nix", "{  }\n");
        let mut settings = EditorSettings::default();
        settings.files.undo_file = false;
        let root = path
            .parent()
            .expect("the temporary file holds a parent directory")
            .to_path_buf();
        let mut editor = Session::new(Rect::new(0, 0, 80, 24), settings, root);
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

        let (runtime, mut results) = Runtime::<WorkResult>::new();
        let gate = PublicationGate::default();
        let mut language = None;
        let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
        let _ = dispatch(&mut editor, &runtime, &gate, &mut language);

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
            let _ = complete(&mut editor, &gate, Some(event), NOW);
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
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn a_registration_that_fails_reports_that_no_watcher_runs() {
        // The start places no watch, so it accepts the root that the deferred
        // registration then refuses.
        let mut watcher = FileWatcher::start(PathBuf::from(MISSING_ROOT), &GENERATED_NAMES).ok();
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
            PathBuf::from("/workspace"),
        );
        let step = publish_watch(&mut editor, batch.as_ref(), NOW);

        assert!(
            matches!(step, Step::Handled(Redraw::Needed)),
            "the report changes the message line, so one frame follows it"
        );
        assert_eq!(
            editor
                .message()
                .map_or_else(String::new, |message| message.text().to_owned()),
            WATCH_MISSING_NOTE,
        );
    }
}
