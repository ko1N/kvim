//! Embeds one kvim editor in one host that owns everything around it.
//!
//! The example is one complete embedding host. It needs no terminal, no
//! network, and no checkout of its own. It creates one temporary worktree,
//! builds the bounded spawner itself, drives one [`EmbeddedEditor`] with
//! resolved commands and literal text, renders into cells that it owns, and
//! ends the editor through one shutdown that returns the remaining events.
//!
//! The run proves five facts of `docs/embedding.md`:
//!
//! - the host names its capacity, so the editor shares no queue and no
//!   cancellation namespace with another editor;
//! - the host supplies resolved commands and literal text, and the editor runs
//!   no second key-sequence resolver;
//! - the editor writes only inside the rectangle that the host accepted, and
//!   it returns the cursor request instead of moving a terminal cursor;
//! - one completed write publishes its mandatory `FileWritten` fact;
//! - the host owns the cancellation of its own loop, and the shutdown owns the
//!   cancellation of the pre-commit work of the editor.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p kvim-tui --example embedded_editor
//! ```

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use kvim_input::Command;
use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{Runtime, RuntimeLimits, WORKER_CONCURRENCY_LIMIT_MAX};
use kvim_settings::EditorSettings;
use kvim_tui::{
    EditorCapacity, EditorEvent, EditorShutdown, EditorWork, EmbeddedEditor, PublishedEvent,
};
use kvim_workspace::temp::TempDir;

/// The file that the host opens and saves.
const DOCUMENT: &str = "src/config.rs";

/// The exact text that the temporary worktree holds at the start.
const ORIGINAL_TEXT: &str = "pub const TIMEOUT: u32 = 30;\n";

/// The text that the host types into the first line.
const TYPED_TEXT: &str = "//! One bounded setting.";

/// The rectangle that the host gives the editor inside its own cells.
///
/// The origin is not zero, so the run also proves that the editor writes no
/// cell outside the rectangle that the host accepted.
const EDITOR_AREA: Rect = Rect {
    x: 2,
    y: 1,
    width: 76,
    height: 20,
};

/// The rectangle of the cells that the host owns.
const HOST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

/// The elapsed time that the host stamps on every input of this run.
///
/// The editor reads no clock, so the host owns this value. One constant keeps
/// the run deterministic.
const NOW: Duration = Duration::ZERO;

/// The results that the spawner of this editor holds.
const RESULT_QUEUE: usize = 64;

/// The worker jobs of this editor that run at the same time.
///
/// One transition can leave a directory read, a buffer analysis, and a write
/// in flight together, so a host that gives the editor fewer permits refuses
/// the operation that finds no permit. The bound of the crate is the safe
/// choice for one editor.
const WORKERS: usize = WORKER_CONCURRENCY_LIMIT_MAX;

/// The external processes of this editor that run at the same time.
///
/// One editor runs the Git read, the search, and the external formatter as
/// processes, so this bound covers every process that one transition starts.
const PROCESSES: usize = 4;

/// The steps that one host loop runs before it reports a defect.
///
/// One step dispatches every queued request and applies one result, so this
/// bound covers every chain that one command of this run can start.
const DRIVE_STEPS_MAX: usize = 64;

/// The time that one step of the host loop waits for a result.
const STEP_DEADLINE: Duration = Duration::from_secs(10);

/// The time that the host gives the background work of the editor at exit.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

/// The rows of the frame that the run prints.
const PRINTED_ROWS: u16 = 4;

/// What one run of the host loop produced.
enum DriveOutcome {
    /// The wanted event arrived. The value holds every event of this run, in
    /// publication order.
    Found(Vec<PublishedEvent>),
    /// The host cancelled its own loop.
    Cancelled,
    /// One step waited longer than the host allows.
    TimedOut,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let workspace = TempDir::new("embedded-editor");
    workspace.file(DOCUMENT, ORIGINAL_TEXT);
    let root = Arc::new(WorktreeRoot::open(&workspace.path)?);

    // The host builds the spawner, so every background task of the editor
    // belongs to the host and no editor detaches a task of its own.
    let limits = RuntimeLimits::new(RESULT_QUEUE, WORKERS, PROCESSES)?;
    let (spawner, results) = Runtime::<EditorWork>::with_limits(limits);

    // The undo file would write one more entry into the worktree, and this run
    // reports the exact workspace facts, so the host turns it off.
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;

    let mut editor = EmbeddedEditor::builder(Arc::clone(&root), EDITOR_AREA)
        .settings(settings)
        .capacity(EditorCapacity::Supplied { spawner, results })
        .open()?;
    println!("editor instance {}", editor.instance().get());

    // The host owns this token. It ends the loop below and never reaches the
    // editor, which owns the cancellation of its own submitted work.
    let cancellation = CancellationToken::new();

    let document = WorktreeRelativePath::new(DOCUMENT)?;
    let _redraw = editor.open_file(document);
    let opened = run_until(&mut editor, &cancellation, |event| {
        matches!(event, EditorEvent::ActiveFileChanged { .. })
    })
    .await;
    let DriveOutcome::Found(events) = opened else {
        return Err("the open publishes one active-file change".into());
    };
    report(&events);

    // The host resolved these commands itself. The editor accepts the command
    // and the literal text, and it resolves no key sequence of its own.
    let _reduction = editor.command(Command::InsertBeforeCursor, None, None, NOW);
    let _reduction = editor.insert_literal(TYPED_TEXT, NOW);
    let _reduction = editor.command(Command::InsertLineBreak, None, None, NOW);
    let _reduction = editor.command(Command::ReturnToNormal, None, None, NOW);

    // The host owns the cells. The editor writes only inside `EDITOR_AREA` and
    // returns the cursor that the frame asks for, so the host decides whether
    // any cursor becomes visible.
    let mut cells = CellBuffer::empty(HOST_AREA);
    let cursor = editor.draw(&mut cells, EDITOR_AREA)?;
    match cursor.position {
        Some(position) => println!(
            "the frame asks for a {:?} cursor at column {} row {}",
            cursor.shape, position.x, position.y
        ),
        None => println!("the frame asks for no visible cursor"),
    }
    print_frame(&cells);

    // A save is one durable side effect, so it reserves its outbox slot before
    // the write starts and publishes `FileWritten` after the write succeeded.
    let _reduction = editor.command(Command::SaveBuffer, None, None, NOW);
    let written = run_until(&mut editor, &cancellation, |event| {
        matches!(event, EditorEvent::FileWritten { .. })
    })
    .await;
    let DriveOutcome::Found(events) = written else {
        return Err("one completed write publishes its mandatory fact".into());
    };
    report(&events);
    println!(
        "the worktree now holds {} bytes",
        std::fs::read(workspace.join(DOCUMENT))?.len()
    );

    // The host cancels its own loop. The loop leaves at once and waits for no
    // deadline of its own.
    cancellation.cancel();
    let outcome = run_until(&mut editor, &cancellation, |_| false).await;
    match outcome {
        DriveOutcome::Cancelled => println!("the host loop left on its own cancellation"),
        DriveOutcome::Found(_) | DriveOutcome::TimedOut => {
            return Err("a cancelled host loop leaves at once".into());
        }
    }

    // The shutdown consumes the editor. It cancels every request that has not
    // committed yet, waits for every task that can still commit, and hands the
    // remaining events back.
    match editor.shutdown(SHUTDOWN_DEADLINE).await {
        EditorShutdown::Finished { events } => {
            println!(
                "the shutdown finished with {} remaining events",
                events.len()
            );
        }
        EditorShutdown::Draining(drain) => {
            let events = drain.complete().await;
            println!("the drain delivered {} remaining events", events.len());
        }
    }

    Ok(())
}

/// Drives the editor until it publishes one event that the host waits for.
///
/// The host owns this loop. It hands every queued request to the spawner,
/// reads every published event, and waits for the next result beside its own
/// cancellation and its own deadline. The loop performs no filesystem work, no
/// process work, and no parsing of its own.
async fn run_until(
    editor: &mut EmbeddedEditor,
    cancellation: &CancellationToken,
    wanted: fn(&EditorEvent) -> bool,
) -> DriveOutcome {
    let mut seen = Vec::new();
    for _ in 0..DRIVE_STEPS_MAX {
        let _redraw = editor.dispatch();
        // The outbox is bounded, so this drain always ends.
        while let Some(published) = editor.take_event() {
            let found = wanted(&published.event);
            seen.push(published);
            if found {
                return DriveOutcome::Found(seen);
            }
        }
        tokio::select! {
            completed = editor.recv() => {
                let _redraw = editor.apply(completed, NOW);
            }
            () = cancellation.cancelled() => return DriveOutcome::Cancelled,
            () = sleep(STEP_DEADLINE) => return DriveOutcome::TimedOut,
        }
    }
    DriveOutcome::TimedOut
}

/// Prints every event of one run of the host loop.
///
/// kvim gives none of these facts a host meaning. The host decides the effect
/// of each one.
fn report(events: &[PublishedEvent]) {
    for published in events {
        match &published.event {
            EditorEvent::ActiveFileChanged { path } => match path {
                Some(path) => println!(
                    "editor {}: shows {}",
                    published.instance.get(),
                    path.as_path().display()
                ),
                None => println!("editor {}: shows no file", published.instance.get()),
            },
            EditorEvent::FileWritten { path } => println!(
                "editor {}: wrote {}",
                published.instance.get(),
                path.as_path().display()
            ),
            EditorEvent::WorkspaceChanged { operation } => {
                println!("editor {}: changed {operation:?}", published.instance.get());
            }
            EditorEvent::SaveReconciliationRequired { path } => println!(
                "editor {}: must reconcile save of {}",
                published.instance.get(),
                path.as_path().display()
            ),
            EditorEvent::WorkspaceReconciliationRequired { operation } => println!(
                "editor {}: must reconcile {operation:?}",
                published.instance.get()
            ),
            EditorEvent::FileActivated { path } => println!(
                "editor {}: the reader activated {}",
                published.instance.get(),
                path.as_path().display()
            ),
            EditorEvent::RedrawRequested => {
                println!("editor {}: asks for one frame", published.instance.get());
            }
            EditorEvent::FocusBoundary(direction) => println!(
                "editor {}: focus reached the {direction:?} edge",
                published.instance.get()
            ),
            EditorEvent::CloseRequested => {
                println!(
                    "editor {}: asks the host to close it",
                    published.instance.get()
                );
            }
        }
    }
}

/// Prints the first rows of the frame that the host owns.
///
/// The columns left of the editor rectangle stay empty, which shows that the
/// editor wrote no cell outside the rectangle that the host accepted.
fn print_frame(cells: &CellBuffer) {
    for row in 0..PRINTED_ROWS {
        let mut line = String::new();
        for column in 0..cells.area.width {
            line.push_str(cells[(column, row)].symbol());
        }
        println!("|{}|", line.trim_end());
    }
}
