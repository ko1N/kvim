//! Draws the file sidebar of one embedded kvim editor inside a host.
//!
//! The example is one complete embedding host of one file sidebar. It needs no
//! terminal, no network, and no checkout of its own. It creates one temporary
//! worktree, builds the bounded spawner itself, drives the tree of one
//! [`EmbeddedEditor`] with [`FileSidebarInput`] values, draws every row into
//! cells that it owns, and ends the editor through one shutdown.
//!
//! The run proves five facts of `docs/embedding.md`:
//!
//! - the host draws the tree over one worktree root without naming one type of
//!   `kvim-workspace`, which is no supported package;
//! - the editor reads no directory on the host loop: every listing leaves as
//!   one unit of work through the one channel that the host already drives for
//!   the editor, and returns through `apply`;
//! - the host moves the selection, opens one directory, and closes it again;
//! - one activated file returns from the input that produced it, and the host
//!   decides whether to open it;
//! - the host owns the look: kvim publishes the text, the indent guides, the
//!   depth, and the state of one row, and no color and no cell.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p kvim-tui --example embedded_file_sidebar
//! ```

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use tokio::time::timeout;

use kvim_path::WorktreeRoot;
use kvim_runtime::{Runtime, RuntimeLimits, WORKER_CONCURRENCY_LIMIT_MAX};
use kvim_settings::EditorSettings;
use kvim_tui::{
    EditorCapacity, EditorShutdown, EditorWork, EmbeddedEditor, FileRow, FileRowKind,
    FileSidebarInput, FileSidebarOutcome, ListMotion,
};
use kvim_workspace::temp::TempDir;

/// The directory of the temporary worktree that the run opens.
const PACKAGE: &str = "src";

/// The files that the temporary worktree holds.
const FILES: [(&str, &str); 3] = [
    ("src/main.rs", "fn main() {}\n"),
    ("src/config.rs", "pub const TIMEOUT: u32 = 30;\n"),
    ("readme.md", "One bounded example.\n"),
];

/// The rectangle that the host gives the sidebar inside its own cells.
const SIDEBAR_AREA: Rect = Rect {
    x: 1,
    y: 0,
    width: 34,
    height: 12,
};

/// The rectangle of the cells that the host owns.
const HOST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 36,
    height: 12,
};

/// The elapsed time that the host stamps on every applied result.
///
/// The sidebar reads no clock, so the host owns this value. One constant keeps
/// the run deterministic.
const NOW: Duration = Duration::ZERO;

/// The results that the spawner of this editor holds.
const RESULT_QUEUE: usize = 64;

/// The external processes of this editor that run at the same time.
const PROCESSES: usize = 4;

/// The steps that the host loop runs before it reports a defect.
///
/// One step hands every queued read to the spawner and applies one result, so
/// this bound covers every chain that one expansion of this run can start.
const DRIVE_STEPS_MAX: usize = 64;

/// The time that one step of the host loop waits for a result.
const STEP_DEADLINE: Duration = Duration::from_secs(10);

/// The time that the host gives the background work of the editor at exit.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

/// The marker that the host paints on the selected row.
///
/// kvim publishes the selection as one fact of the row. The glyph is the
/// host's own choice, because the host owns the look of its sidebar.
const SELECTION_MARK: &str = "▌";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let workspace = TempDir::new("embedded-file-sidebar");
    for (path, text) in FILES {
        workspace.file(path, text);
    }
    let root = Arc::new(WorktreeRoot::open(&workspace.path)?);

    // The host builds the spawner, so every directory read of the tree belongs
    // to the host and the editor detaches no task of its own.
    let limits = RuntimeLimits::new(RESULT_QUEUE, WORKER_CONCURRENCY_LIMIT_MAX, PROCESSES)?;
    let (spawner, results) = Runtime::<EditorWork>::with_limits(limits);

    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;

    let mut editor = EmbeddedEditor::builder(Arc::clone(&root), HOST_AREA)
        .settings(settings)
        .capacity(EditorCapacity::Supplied { spawner, results })
        .open()?;
    println!("editor instance {}", editor.instance().get());
    println!("worktree {}", editor.file_root_label());

    // The tree opens with one pending read, and that read never runs here. The
    // host hands it to the spawner and applies the listing that comes back.
    if editor.file_rows().is_empty() {
        println!("the tree shows no row yet, because the root listing is still work");
    }
    read_until(&mut editor, |editor| !editor.file_rows().is_empty()).await?;

    let mut cells = CellBuffer::empty(HOST_AREA);
    draw(&mut cells, &editor);
    println!("--- the worktree root ---");
    print_frame(&cells);

    // The host moves the selection with the bounded motions of `kvim-ui`. The
    // move never wraps and never rests on a row that reports about a directory.
    let _outcome = editor.file_sidebar(FileSidebarInput::Move(ListMotion::ToRow(0)));
    println!("selected {}", selected_label(&editor));

    // The open changes the tree and reads nothing. The directory reports that
    // it waits for its listing until the host hands that listing back.
    let _outcome = editor.file_sidebar(FileSidebarInput::Open);
    if let Some(row) = selected_row(&editor) {
        println!("{} is now {:?}", row.label(), row.kind());
    }
    read_until(&mut editor, |editor| {
        selected_row(editor).is_some_and(|row| row.kind() == FileRowKind::OpenDirectory)
    })
    .await?;

    draw(&mut cells, &editor);
    println!("--- {PACKAGE} is open ---");
    print_frame(&cells);

    // The reader steps onto one file and activates it. The sidebar opens no
    // buffer: the activation returns from the input that produced it, and the
    // host decides the effect.
    let _outcome = editor.file_sidebar(FileSidebarInput::Move(ListMotion::Down(1)));
    let outcome = editor.file_sidebar(FileSidebarInput::Activate);
    match outcome {
        FileSidebarOutcome::Activated { path } => {
            println!("the reader activated {}", path.as_path().display());
            // A host that shows the file asks for it here. kvim assigns the
            // activation no meaning of its own.
            let _redraw = editor.open_file(path);
        }
        FileSidebarOutcome::Applied => return Err("the selected row names one file".into()),
    }

    // The close hides every entry of the directory again, so two of these
    // inputs take a file to its directory and then close that directory.
    let _outcome = editor.file_sidebar(FileSidebarInput::Close);
    let _outcome = editor.file_sidebar(FileSidebarInput::Close);
    draw(&mut cells, &editor);
    println!("--- {PACKAGE} is closed again ---");
    print_frame(&cells);

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

/// Drives the work channel of the editor until one condition holds.
///
/// The host owns this loop. It hands every queued read to the spawner and
/// waits for the next finished unit beside its own deadline. The loop performs
/// no filesystem work of its own.
async fn read_until(
    editor: &mut EmbeddedEditor,
    settled: fn(&EmbeddedEditor) -> bool,
) -> Result<(), Box<dyn Error>> {
    for _ in 0..DRIVE_STEPS_MAX {
        let _redraw = editor.dispatch();
        // The outbox is bounded, so this drain always ends.
        while let Some(_published) = editor.take_event() {}
        if settled(editor) {
            return Ok(());
        }
        let completed = timeout(STEP_DEADLINE, editor.recv()).await?;
        let _redraw = editor.apply(completed, NOW);
    }
    Err("one listing of this worktree arrives inside the step bound".into())
}

/// Returns the row that the selection rests on.
fn selected_row(editor: &EmbeddedEditor) -> Option<FileRow> {
    editor.file_rows().into_iter().find(FileRow::is_selected)
}

/// Returns the label of the selected row, or a report that none is selected.
fn selected_label(editor: &EmbeddedEditor) -> String {
    selected_row(editor).map_or_else(|| "no row".to_owned(), |row| row.label().to_owned())
}

/// Draws every published row into the cells that the host owns.
///
/// kvim publishes the text, the indent guides, the depth, and the state of one
/// row. The host owns every glyph and every color, so this function is the
/// complete look of this sidebar.
fn draw(cells: &mut CellBuffer, editor: &EmbeddedEditor) {
    cells.reset();
    for (line, row) in editor.file_rows().iter().enumerate() {
        let Ok(line) = u16::try_from(line) else {
            break;
        };
        if line >= SIDEBAR_AREA.height {
            break;
        }
        let mark = if row.is_selected() {
            SELECTION_MARK
        } else {
            " "
        };
        // The guides already hold the leading blank of the workspace-root
        // header, so the host draws them exactly as kvim publishes them.
        let text = format!("{mark}{}{}{}", row.guides(), glyph(row.kind()), row.label());
        cells.set_stringn(
            SIDEBAR_AREA.x,
            SIDEBAR_AREA.y + line,
            &text,
            usize::from(SIDEBAR_AREA.width),
            Style::default(),
        );
    }
}

/// Returns the glyph that this host paints for one row state.
///
/// kvim names the state and paints no glyph, so this table belongs to the host.
const fn glyph(kind: FileRowKind) -> &'static str {
    match kind {
        FileRowKind::File => "  ",
        FileRowKind::ClosedDirectory => "▸ ",
        FileRowKind::OpenDirectory => "▾ ",
        FileRowKind::LoadingDirectory => "… ",
        FileRowKind::Note => "· ",
    }
}

/// Prints the cells that the host owns.
///
/// The column left of the sidebar rectangle stays empty, which shows that the
/// host wrote no cell outside the rectangle that it gave the sidebar.
fn print_frame(cells: &CellBuffer) {
    for row in 0..cells.area.height {
        let mut line = String::new();
        for column in 0..cells.area.width {
            line.push_str(cells[(column, row)].symbol());
        }
        if line.trim().is_empty() {
            continue;
        }
        println!("|{}|", line.trim_end());
    }
}
