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

use crate::language::ANALYSIS_DEADLINE;
use crate::runtime::{
    PublicationGate, RequestSlot, Runtime, RuntimeError, RuntimeEvent, SubmitError,
    WORKER_DEADLINE_DEFAULT,
};
use crate::settings::EditorSettings;
use crate::terminal::{
    CrosstermControl, EventSource, TerminalError, TerminalEvent, TerminalSession,
};
use crate::workspace::FileResult;

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

/// One completed background operation of the editor.
///
/// The runtime is generic over its result, and the editor submits both file work
/// and language work, so one value names both.
enum WorkResult {
    /// One file operation finished.
    File(FileResult),
    /// One buffer analysis finished.
    Analysis(AnalysisResult),
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
/// panic unwind.
///
/// # Errors
///
/// Returns [`EditorError`] when a terminal control step fails, when a draw
/// fails, or when the event stream fails [`EVENT_ERRORS_MAX`] times in a row.
pub async fn run(settings: EditorSettings, path: Option<PathBuf>) -> Result<(), EditorError> {
    let terminal =
        TerminalSession::enter(CrosstermControl::new()).map_err(EditorError::Terminal)?;
    let outcome = drive(settings, path).await;
    terminal.restore().map_err(EditorError::Terminal)?;
    outcome
}

/// Drives one editor session over the process terminal.
async fn drive(settings: EditorSettings, path: Option<PathBuf>) -> Result<(), EditorError> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout())).map_err(EditorError::Draw)?;
    let size = terminal.size().map_err(EditorError::Draw)?;
    let mut editor = Session::new(Rect::new(0, 0, size.width, size.height), settings);
    let mut events = EventSource::from_terminal();
    // The file operations and the buffer analysis run on the bounded worker
    // service, so the loop below performs no filesystem work and no parsing.
    // See `docs/responsiveness.md`.
    let (runtime, mut results) = Runtime::<WorkResult>::new();
    let gate = PublicationGate::default();
    let start = Instant::now();
    let mut errors = 0;

    if let Some(path) = path {
        editor.open_path(path);
    }
    submit_background_work(&mut editor, &runtime, &gate);
    terminal
        .draw(|frame| editor.render(frame))
        .map_err(EditorError::Draw)?;
    while editor.run_state() == RunState::Running {
        let now = start.elapsed();
        let step = match editor.next_deadline() {
            Some(deadline) if deadline > now => {
                tokio::select! {
                    event = events.next_event() => apply(&mut editor, event, start.elapsed()),
                    result = results.recv() => complete(&mut editor, &gate, result),
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
                }
            }
        };
        submit_background_work(&mut editor, &runtime, &gate);
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
                    runtime.shutdown().await;
                    return Err(EditorError::EventStream(error));
                }
            }
        }
    }
    runtime.shutdown().await;
    Ok(())
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
    Step::Handled(match event.result {
        Ok(WorkResult::File(result)) => editor.apply_file_result(result),
        Ok(WorkResult::Analysis(result)) => editor.apply_analysis_result(result),
        // An analysis that fails, times out, or is cancelled renders plain text
        // and reports nothing, because highlighting is decoration.
        Err(_) if analysis => {
            editor.abandon_analysis_request();
            Redraw::Skipped
        }
        Err(RuntimeError::Timeout) => editor.abandon_file_request(FileRequestFailure::Timeout),
        // A cancelled request and a failed worker both leave the buffer
        // unchanged, so the editor stays usable and the user can try again.
        Err(
            RuntimeError::Cancelled
            | RuntimeError::WorkerFailure(_)
            | RuntimeError::ProcessSpawn(_)
            | RuntimeError::ProcessRead(_)
            | RuntimeError::ProcessWrite(_)
            | RuntimeError::OutputLimit { .. },
        ) => editor.abandon_file_request(FileRequestFailure::Cancelled),
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
