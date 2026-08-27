//! The terminal event loop of the standalone editor.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! This module is the imperative shell of the standalone editor. It alone owns
//! raw mode, the alternate screen, terminal input and output, process signals,
//! panic restoration, cursor application, redraw scheduling, and shutdown
//! order. `WorktreeEditor` owns visible editor state and bounded background
//! execution, but it owns no terminal and no process event loop.
//!
//! The loop forwards normalized input to one `WorktreeEditor`, routes opaque
//! completions back to that editor, and draws only after a visible change. It
//! performs no filesystem, process, Git, language-server, formatter, or syntax
//! work. See `docs/responsiveness.md` and `docs/embedding.md`.

use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use thiserror::Error;
use tokio::time::sleep;

use kvim_embed::{
    ServicePolicy, WorktreeCapabilities, WorktreeCursorShape, WorktreeEditor, WorktreeEvent,
    WorktreeInput, WorktreeRunState, WorktreeShutdown, WorktreeUpdate,
};
use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_settings::EditorSettings;
use kvim_terminal::{
    CrosstermControl, CursorShape, EventSource, TerminalControl, TerminalError, TerminalEvent,
    TerminalSession, TerminationSource,
};

/// Consecutive terminal read failures that end the editor.
pub const EVENT_ERRORS_MAX: usize = 8;
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
    /// The editor facade could not open.
    #[error("the worktree editor could not open")]
    Open(#[source] kvim_embed::WorktreeOpenError),
    /// The initial path is outside the worktree.
    #[error("the initial path is outside the worktree")]
    InitialPath,
}

/// Whether the editor panics on purpose after its first frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PanicProbe {
    /// Run normally.
    #[default]
    Disabled,
    /// Panic after the first frame.
    AfterFirstFrame,
}

enum Step {
    Handled(WorktreeUpdate),
    Input(TerminalEvent, Duration),
    Completed(kvim_embed::WorktreeCompletion),
    Stop,
    Failed(TerminalError),
}

/// Runs the editor until it closes its last window.
pub async fn run(
    settings: EditorSettings,
    root: WorktreeRoot,
    path: Option<PathBuf>,
    probe: PanicProbe,
) -> Result<(), EditorError> {
    let mut terminal =
        TerminalSession::enter(CrosstermControl::new()).map_err(EditorError::Terminal)?;
    let terminations = TerminationSource::from_process();
    let outcome = drive(&mut terminal, terminations, settings, root, path, probe).await;
    terminal.restore().map_err(EditorError::Terminal)?;
    outcome
}

const fn terminal_shape(shape: WorktreeCursorShape) -> CursorShape {
    match shape {
        WorktreeCursorShape::Bar => CursorShape::Bar,
        WorktreeCursorShape::Block => CursorShape::Block,
    }
}

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
    let area = Rect::new(0, 0, size.width, size.height);
    let root_path = root.as_path().to_path_buf();
    let capabilities = WorktreeCapabilities {
        git: ServicePolicy::BuiltIn,
        watcher: ServicePolicy::BestEffortBuiltIn,
        language: ServicePolicy::BestEffortBuiltIn,
        clipboard: ServicePolicy::BuiltIn,
    };
    let mut editor = WorktreeEditor::builder(&root_path, area)
        .settings(settings)
        .capabilities(capabilities)
        .open()
        .map_err(EditorError::Open)?;
    if let Some(path) = path {
        let relative = initial_path(&root_path, &path).ok_or(EditorError::InitialPath)?;
        let _ = editor.open_file(relative);
    }

    let mut events = EventSource::from_terminal();
    let start = Instant::now();
    let mut errors = 0;
    let _ = dispatch(&mut editor);
    let mut shape = draw(&mut terminal, session, &editor)?;
    assert!(
        probe == PanicProbe::Disabled,
        "the panic probe of the environment asks for this panic"
    );

    let mut caught_up = None;
    while editor.run_state() == WorktreeRunState::Running {
        let now = start.elapsed();
        let step = match editor.next_deadline() {
            Some(deadline) if deadline > now => tokio::select! {
                event = events.next_event() => apply_event(event, start.elapsed()),
                completed = editor.ready() => Step::Completed(completed),
                _ = terminations.recv() => Step::Stop,
                () = sleep(deadline - now) => Step::Handled(editor.tick(start.elapsed())),
            },
            Some(deadline) if caught_up != Some(deadline) => {
                caught_up = Some(deadline);
                Step::Handled(editor.tick(now))
            }
            Some(_) | None => tokio::select! {
                event = events.next_event() => apply_event(event, start.elapsed()),
                completed = editor.ready() => Step::Completed(completed),
                _ = terminations.recv() => Step::Stop,
            },
        };
        let step = match step {
            Step::Input(event, now) => Step::Handled(
                editor
                    .input(facade_input(event), now)
                    .expect("the standalone editor owns its physical resolver"),
            ),
            Step::Completed(completed) => Step::Handled(
                editor
                    .apply(completed, start.elapsed())
                    .expect("the standalone loop routes its facade completion"),
            ),
            other => other,
        };
        let dispatched = dispatch(&mut editor);
        match step {
            Step::Handled(update) => {
                errors = 0;
                if update == WorktreeUpdate::Redraw || dispatched == WorktreeUpdate::Redraw {
                    shape = draw(&mut terminal, session, &editor)?;
                }
                let _ = shape;
            }
            Step::Stop => break,
            Step::Failed(error) => {
                errors += 1;
                if errors >= EVENT_ERRORS_MAX {
                    shutdown(editor).await;
                    return Err(EditorError::EventStream(error));
                }
            }
            Step::Input(_, _) | Step::Completed(_) => {
                unreachable!("input and completed work are applied above")
            }
        }
    }
    shutdown(editor).await;
    Ok(())
}

fn initial_path(root: &Path, path: &Path) -> Option<WorktreeRelativePath> {
    let relative = if path.is_absolute() {
        path.strip_prefix(root).ok()?
    } else {
        path
    };
    WorktreeRelativePath::new(relative).ok()
}

fn dispatch(editor: &mut WorktreeEditor) -> WorktreeUpdate {
    let mut update = editor.dispatch();
    while let Some(event) = editor.take_event() {
        if matches!(event, WorktreeEvent::RedrawRequested) {
            update = WorktreeUpdate::Redraw;
        }
    }
    update
}

fn draw<C: TerminalControl>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    session: &mut TerminalSession<C>,
    editor: &WorktreeEditor,
) -> Result<CursorShape, EditorError> {
    let mut cursor = None;
    terminal
        .draw(|frame| {
            let rendered = editor
                .render(frame.buffer_mut())
                .expect("the terminal frame uses the accepted editor area");
            cursor = Some(rendered);
            if let Some(position) = rendered.position {
                frame.set_cursor_position(position);
            }
        })
        .map_err(EditorError::Draw)?;
    let shape = terminal_shape(cursor.expect("one draw renders one cursor").shape);
    let _ = session.set_cursor_shape(shape);
    Ok(shape)
}

async fn shutdown(editor: WorktreeEditor) {
    let events = match editor.shutdown(SHUTDOWN_DEADLINE).await {
        WorktreeShutdown::Finished { events } => events,
        WorktreeShutdown::Draining(drain) => drain.complete().await,
    };
    // Shutdown reconciles every committed side effect before it returns these
    // mandatory facts. The standalone process is exiting, so no observer can
    // use them after terminal restoration. Consume them deliberately here.
    for event in events {
        match event {
            WorktreeEvent::ActiveFileChanged { .. }
            | WorktreeEvent::FileWritten { .. }
            | WorktreeEvent::WorkspaceChanged { .. }
            | WorktreeEvent::SaveReconciliationRequired { .. }
            | WorktreeEvent::WorkspaceReconciliationRequired { .. }
            | WorktreeEvent::FileActivated { .. }
            | WorktreeEvent::RedrawRequested
            | WorktreeEvent::FocusBoundary(_)
            | WorktreeEvent::CloseRequested => {}
        }
    }
}

fn apply_event(event: Option<Result<TerminalEvent, TerminalError>>, now: Duration) -> Step {
    match event {
        Some(Ok(event)) => Step::Input(event, now),
        Some(Err(error)) => Step::Failed(error),
        None => Step::Stop,
    }
}

fn facade_input(event: TerminalEvent) -> WorktreeInput {
    match event {
        TerminalEvent::Key(key) => WorktreeInput::Key(key),
        TerminalEvent::Paste(text) => WorktreeInput::Paste(text),
        TerminalEvent::Resize { columns, rows } => WorktreeInput::Resize { columns, rows },
        TerminalEvent::Unsupported => WorktreeInput::Unsupported,
    }
}
