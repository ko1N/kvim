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
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use thiserror::Error;
use tokio::time::sleep;

use kvim_language::{LanguageRegistry, LanguageServices};
use kvim_path::WorktreeRoot;
use kvim_runtime::{FileWatcher, Runtime};
use kvim_settings::EditorSettings;
use kvim_terminal::{
    CrosstermControl, CursorShape, EventSource, TerminalControl, TerminalError, TerminalEvent,
    TerminalSession, TerminationSource,
};

use super::clipboard::SessionClipboard;
use super::driver::{Completed, EditorDriver, EditorWork};
use super::embed::CursorShape as EditorCursorShape;
use super::session::{Redraw, RunState, Session};
use super::tree::GENERATED_NAMES;

/// The number of consecutive terminal read failures that ends the editor.
///
/// One failure keeps the source usable, so the loop reports it and reads again.
/// A run of failures means the terminal is gone, and the bound keeps the loop
/// from spinning forever.
pub const EVENT_ERRORS_MAX: usize = 8;

/// The time that the standalone editor waits for its background work at exit.
///
/// The worker deadline, the process deadline, and the language server shutdown
/// deadline each bound one tracked task, so this value covers the longest of
/// them. A deadline that still expires leaves the drain, and the binary then
/// waits for that drain before it restores the terminal. See
/// `docs/responsiveness.md`.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(15);

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
    /// One unit of background work finished and still needs its transition.
    Completed(Completed),
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
/// The caller supplies one validated workspace root. The root is the
/// containment boundary of every file operation and every document that a
/// language server sees. See `docs/language-services.md`.
///
/// # Errors
///
/// Returns [`EditorError`] when a terminal control step fails, when a draw
/// fails, or when the event stream fails [`EVENT_ERRORS_MAX`] times in a row.
pub async fn run(
    settings: EditorSettings,
    root: WorktreeRoot,
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

/// Returns the terminal sequence of one editor cursor request.
///
/// The editor names the shape that its mode asks for, and this adapter is the
/// one place that turns that request into a terminal sequence. See
/// `docs/embedding.md`.
const fn terminal_shape(shape: EditorCursorShape) -> CursorShape {
    match shape {
        EditorCursorShape::Bar => CursorShape::Bar,
        EditorCursorShape::Block => CursorShape::Block,
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
    root: WorktreeRoot,
    path: Option<PathBuf>,
    probe: PanicProbe,
) -> Result<(), EditorError> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout())).map_err(EditorError::Draw)?;
    let size = terminal.size().map_err(EditorError::Draw)?;
    // The clipboard selection reads the platform and the executable search path
    // once, here at the start, so no operation ever guesses. The editor and the
    // registers name no platform, no command, and no selection. See
    // `docs/clipboard.md`.
    let root = Arc::new(root);
    let root_path = root.as_path().to_path_buf();
    let mut editor = Session::new(
        Rect::new(0, 0, size.width, size.height),
        settings,
        Arc::clone(&root),
    )
    .with_clipboard(SessionClipboard::detect());
    let mut events = EventSource::from_terminal();
    // The binary is the host of its own editor, so it builds the bounded
    // spawner and the driver itself. The file operations, the buffer analysis,
    // the external commands, and the language sessions all leave this loop
    // through that spawner, so the loop below performs no filesystem work, no
    // process work, and no parsing. See `docs/embedding.md`.
    let (spawner, results) = Runtime::<EditorWork>::new();
    let mut driver = EditorDriver::new(editor.instance(), spawner, results);
    // The language sessions run as background tasks of this runtime context, so
    // the loop below never reads, writes, or waits for a server. A root that
    // this constructor refuses leaves the editor fully usable without them.
    if let Ok(language) =
        LanguageServices::new(LanguageRegistry::first_release(), root_path, settings)
    {
        driver = driver.with_language(language);
    }
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
    match FileWatcher::start(Arc::clone(&root), &GENERATED_NAMES) {
        Ok(watcher) => driver = driver.with_watcher(watcher),
        Err(_refused) => {
            let _ = editor.report_watch_unavailable();
        }
    }
    let start = Instant::now();
    let mut errors = 0;

    if let Some(path) = path {
        editor.open_path(path);
    }
    // The first frame follows this dispatch unconditionally, so its report needs
    // no redraw request of its own.
    let _ = dispatch(&mut driver, &mut editor);
    // The shape follows the mode, so the editor writes it once at the start and
    // then only after a mode change. The sequence is decoration: a terminal that
    // ignores it still shows its own cursor.
    let mut shape = terminal_shape(editor.cursor_shape());
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
                    completed = driver.recv() => Step::Completed(completed),
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
                    completed = driver.recv() => Step::Completed(completed),
                    _ = terminations.recv() => Step::Stop,
                }
            }
        };
        // The driver owns the result routing, so the finished work reaches the
        // editor after the wait ended and not inside it.
        let step = match step {
            Step::Completed(completed) => {
                Step::Handled(driver.apply(&mut editor, completed, start.elapsed()))
            }
            other => other,
        };
        // A refused submission reports its state on the message line, so the
        // dispatch owns a visible change of its own and the frame must follow
        // it as well as the transition above.
        let dispatched = dispatch(&mut driver, &mut editor);
        let next_shape = terminal_shape(editor.cursor_shape());
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
            Step::Completed(_) => {
                debug_assert!(false, "the match above turned every completion into a step");
            }
            Step::Stop => break,
            Step::Failed(error) => {
                errors += 1;
                if errors >= EVENT_ERRORS_MAX {
                    shutdown(driver, &mut editor).await;
                    return Err(EditorError::EventStream(error));
                }
            }
        }
    }
    shutdown(driver, &mut editor).await;
    Ok(())
}

/// Hands every queued request to its service and drains the published facts.
///
/// The standalone binary is the host of its own editor. It owns the terminal,
/// the file tree, and the shutdown order itself, so it reads the published facts
/// and keeps the bounded outbox free for the next durable operation. See
/// `docs/embedding.md`.
fn dispatch(driver: &mut EditorDriver, editor: &mut Session) -> Redraw {
    let redraw = driver.dispatch(editor);
    while editor.take_event().is_some() {}
    redraw
}

/// Ends every background service of the editor.
///
/// The binary owns the asynchronous runtime, so it waits for the drain that an
/// expired deadline returns. No task of the editor outlives this call. See
/// `docs/responsiveness.md`.
async fn shutdown(driver: EditorDriver, editor: &mut Session) {
    if let Some(drain) = driver.shutdown(editor, SHUTDOWN_DEADLINE).await {
        let _redraw = drain.complete(editor).await;
    }
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
