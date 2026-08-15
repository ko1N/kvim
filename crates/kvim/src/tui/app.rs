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

use crate::runtime::{
    PublicationGate, RequestSlot, Runtime, RuntimeError, RuntimeEvent, SubmitError,
    WORKER_DEADLINE_DEFAULT,
};
use crate::settings::EditorSettings;
use crate::terminal::{
    CrosstermControl, EventSource, TerminalError, TerminalEvent, TerminalSession,
};
use crate::workspace::FileResult;

use super::session::{FileRequestFailure, Redraw, RunState, Session};

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
    // The file operations run on the bounded worker service, so the loop below
    // performs no filesystem work. See `docs/responsiveness.md`.
    let (runtime, mut results) = Runtime::<FileResult>::new();
    let gate = PublicationGate::default();
    let start = Instant::now();
    let mut errors = 0;

    if let Some(path) = path {
        editor.open_path(path);
    }
    submit_file_work(&mut editor, &runtime, &gate);
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
        submit_file_work(&mut editor, &runtime, &gate);
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

/// Hands the queued file request to the bounded worker service.
///
/// A refused submission reaches the session as a typed failure, so the editor
/// keeps its previous visible state.
fn submit_file_work(editor: &mut Session, runtime: &Runtime<FileResult>, gate: &PublicationGate) {
    let Some(request) = editor.take_file_request() else {
        return;
    };
    let handle = gate.begin(FILE_SLOT, &runtime.cancellation_root());
    let submitted = runtime.submit_worker(handle, WORKER_DEADLINE_DEFAULT, |_cancellation| {
        request.run()
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

/// Applies one result of the bounded worker service.
fn complete(
    editor: &mut Session,
    gate: &PublicationGate,
    event: Option<RuntimeEvent<FileResult>>,
) -> Step {
    let Some(event) = event else {
        // The runtime is gone, so no further file result can arrive.
        return Step::Handled(Redraw::Skipped);
    };
    if !gate.accepts(&event.request) {
        // A newer request owns the slot, so this result is obsolete.
        return Step::Handled(Redraw::Skipped);
    }
    Step::Handled(match event.result {
        Ok(result) => editor.apply_file_result(result),
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
