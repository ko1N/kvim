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

use crate::input::Mode;
use crate::language::{
    ANALYSIS_DEADLINE, LanguageEvent, LanguageRegistry, LanguageServices, LspError,
};
use crate::runtime::{
    PublicationGate, RequestSlot, Runtime, RuntimeError, RuntimeEvent, SubmitError,
    WORKER_DEADLINE_DEFAULT,
};
use crate::settings::EditorSettings;
use crate::terminal::{
    CrosstermControl, CursorShape, EventSource, TerminalControl, TerminalError, TerminalEvent,
    TerminalSession,
};
use crate::workspace::{BUFFERS_MAX, FileResult, WorkspaceResult};

use super::language::{LANGUAGE_OUTBOX_MAX, send_request};
use super::session::{AnalysisResult, FileRequestFailure, Redraw, RunState, Session};

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

/// The language requests that one loop iteration sends.
///
/// The session holds a bounded outbox and one fresh open for each loaded
/// buffer, so this bound covers every request that one transition can produce.
const LANGUAGE_DISPATCH_MAX: usize = LANGUAGE_OUTBOX_MAX + BUFFERS_MAX;

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
    /// The terminal event stream ended.
    Stop,
    /// The terminal event stream reported one failure.
    Failed(TerminalError),
}

/// Runs the editor until it closes its last window.
///
/// The terminal returns to its original state on every exit path, including a
/// panic. The panic hook of the terminal session owns that path, because a
/// panic aborts without running a destructor on some platforms.
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
    let outcome = drive(&mut terminal, settings, root, path, probe).await;
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
async fn drive<C: TerminalControl>(
    session: &mut TerminalSession<C>,
    settings: EditorSettings,
    root: PathBuf,
    path: Option<PathBuf>,
    probe: PanicProbe,
) -> Result<(), EditorError> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout())).map_err(EditorError::Draw)?;
    let size = terminal.size().map_err(EditorError::Draw)?;
    let mut editor = Session::new(
        Rect::new(0, 0, size.width, size.height),
        settings,
        root.clone(),
    );
    let mut events = EventSource::from_terminal();
    // The file operations and the buffer analysis run on the bounded worker
    // service, so the loop below performs no filesystem work and no parsing.
    // See `docs/responsiveness.md`.
    let (runtime, mut results) = Runtime::<WorkResult>::new();
    // The language sessions run as background tasks of this runtime context, so
    // the loop below never reads, writes, or waits for a server. A root that
    // this constructor refuses leaves the editor fully usable without them.
    let mut language =
        LanguageServices::new(LanguageRegistry::first_release(), root, settings).ok();
    let gate = PublicationGate::default();
    let start = Instant::now();
    let mut errors = 0;

    if let Some(path) = path {
        editor.open_path(path);
    }
    submit_background_work(&mut editor, &runtime, &gate);
    submit_language_work(&mut editor, language.as_mut());
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
    while editor.run_state() == RunState::Running {
        let now = start.elapsed();
        let step = match editor.next_deadline() {
            Some(deadline) if deadline > now => {
                tokio::select! {
                    event = events.next_event() => apply(&mut editor, event, start.elapsed()),
                    result = results.recv() => complete(&mut editor, &gate, result),
                    outcome = next_language_event(&mut language) => publish(&mut editor, outcome),
                    () = sleep(deadline - now) => Step::Handled(editor.tick(start.elapsed())),
                }
            }
            // The deadline already passed, so the transition runs before the
            // loop waits for another event.
            Some(_) => Step::Handled(editor.tick(now)),
            None => {
                tokio::select! {
                    event = events.next_event() => apply(&mut editor, event, start.elapsed()),
                    result = results.recv() => complete(&mut editor, &gate, result),
                    outcome = next_language_event(&mut language) => publish(&mut editor, outcome),
                }
            }
        };
        submit_background_work(&mut editor, &runtime, &gate);
        submit_language_work(&mut editor, language.as_mut());
        let next_shape = cursor_shape(editor.mode());
        if next_shape != shape {
            shape = next_shape;
            let _ = session.set_cursor_shape(shape);
        }
        match step {
            Step::Handled(Redraw::Needed) => {
                errors = 0;
                terminal
                    .draw(|frame| editor.render(frame))
                    .map_err(EditorError::Draw)?;
            }
            Step::Handled(Redraw::Skipped) => errors = 0,
            Step::Stop => break,
            Step::Failed(error) => {
                errors += 1;
                if errors >= EVENT_ERRORS_MAX {
                    shutdown(runtime, language).await;
                    return Err(EditorError::EventStream(error));
                }
            }
        }
    }
    shutdown(runtime, language).await;
    Ok(())
}

/// Cancels every background service and waits for its cleanup.
///
/// Both operations consume their owner, so no caller can submit after them. See
/// `docs/responsiveness.md`.
async fn shutdown(runtime: Runtime<WorkResult>, language: Option<LanguageServices>) {
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

/// Applies one typed result of the language services.
fn publish(editor: &mut Session, event: LanguageEvent) -> Step {
    Step::Handled(editor.apply_language_event(event))
}

/// Hands the queued language requests to the language services.
///
/// Every call returns at once. The services own the process, the deadlines, and
/// the protocol bounds, so the loop never reads, writes, or waits for a server.
fn submit_language_work(editor: &mut Session, mut language: Option<&mut LanguageServices>) {
    for _ in 0..LANGUAGE_DISPATCH_MAX {
        let Some(request) = editor.take_language_request() else {
            return;
        };
        let kind = request.kind();
        let result = match language.as_deref_mut() {
            Some(language) => language
                .session(request.path())
                .and_then(|handle| send_request(handle, request)),
            // A workspace root that the services refused leaves the editor
            // usable with no language service at all.
            None => Err(LspError::NoServerDeclared),
        };
        editor.apply_language_dispatch(kind, result);
    }
    debug_assert!(
        editor.take_language_request().is_none(),
        "one transition produces fewer requests than the dispatch bound"
    );
}

/// Hands the queued file and analysis jobs to the bounded worker service.
///
/// A refused submission reaches the session as a typed failure, so the editor
/// keeps its previous visible state.
fn submit_background_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) {
    submit_file_work(editor, runtime, gate);
    submit_analysis_work(editor, runtime, gate);
    submit_workspace_work(editor, runtime, gate);
}

/// Hands the queued directory read or mutation to the bounded worker service.
///
/// A refused submission reaches the session as a typed failure, so the file
/// tree keeps the state that it held before the request.
fn submit_workspace_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) {
    let Some(request) = editor.take_workspace_request() else {
        return;
    };
    let handle = gate.begin(WORKSPACE_SLOT, &runtime.cancellation_root());
    let submitted = runtime.submit_worker(handle, WORKER_DEADLINE_DEFAULT, |_cancellation| {
        WorkResult::Workspace(request.run())
    });
    if let Err(error) = submitted {
        editor.abandon_workspace_request(match error {
            SubmitError::Saturated(_) => FileRequestFailure::Saturated,
            SubmitError::InvalidLimits | SubmitError::ProcessBounds | SubmitError::ShuttingDown => {
                FileRequestFailure::Cancelled
            }
        });
    }
}

/// Hands the queued file request to the bounded worker service.
fn submit_file_work(editor: &mut Session, runtime: &Runtime<WorkResult>, gate: &PublicationGate) {
    let Some(request) = editor.take_file_request() else {
        return;
    };
    let handle = gate.begin(FILE_SLOT, &runtime.cancellation_root());
    let submitted = runtime.submit_worker(handle, WORKER_DEADLINE_DEFAULT, |_cancellation| {
        WorkResult::File(request.run())
    });
    if let Err(error) = submitted {
        editor.abandon_file_request(match error {
            SubmitError::Saturated(_) => FileRequestFailure::Saturated,
            SubmitError::InvalidLimits | SubmitError::ProcessBounds | SubmitError::ShuttingDown => {
                FileRequestFailure::Cancelled
            }
        });
    }
}

/// Hands the analysis of the active buffer to the bounded worker service.
///
/// Highlighting is decoration, so a refused submission only frees the request
/// again. The next transition asks for it once more.
fn submit_analysis_work(
    editor: &mut Session,
    runtime: &Runtime<WorkResult>,
    gate: &PublicationGate,
) {
    let Some(request) = editor.take_analysis_request() else {
        return;
    };
    let handle = gate.begin(ANALYSIS_SLOT, &runtime.cancellation_root());
    let submitted = runtime.submit_worker(handle, ANALYSIS_DEADLINE, move |cancellation| {
        WorkResult::Analysis(request.run(&cancellation))
    });
    if submitted.is_err() {
        editor.abandon_analysis_request();
    }
}

/// Applies one result of the bounded worker service.
fn complete(
    editor: &mut Session,
    gate: &PublicationGate,
    event: Option<RuntimeEvent<WorkResult>>,
) -> Step {
    let Some(event) = event else {
        // The runtime is gone, so no further result can arrive.
        return Step::Handled(Redraw::Skipped);
    };
    if !gate.accepts(&event.request) {
        // A newer request owns the slot, so this result is obsolete.
        return Step::Handled(Redraw::Skipped);
    }
    let analysis = event.request.slot() == ANALYSIS_SLOT;
    let workspace = event.request.slot() == WORKSPACE_SLOT;
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
    Step::Handled(match event.result {
        Ok(WorkResult::File(result)) => editor.apply_file_result(result),
        Ok(WorkResult::Analysis(result)) => editor.apply_analysis_result(result),
        Ok(WorkResult::Workspace(result)) => editor.apply_workspace_result(result),
        // An analysis that fails, times out, or is cancelled renders plain text
        // and reports nothing, because highlighting is decoration.
        Err(_) if analysis => {
            editor.abandon_analysis_request();
            Redraw::Skipped
        }
        Err(error) if workspace => editor.abandon_workspace_request(failure(&error)),
        Err(error) => editor.abandon_file_request(failure(&error)),
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
