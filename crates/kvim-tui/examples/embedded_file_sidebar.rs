//! Draws the file sidebar of one embedded kvim editor inside a host.
//!
//! The example is one complete embedding host of one file sidebar. It needs no
//! terminal, no network, and no checkout of its own. It creates one temporary
//! worktree, builds the bounded spawner itself, drives the tree of one
//! [`EmbeddedEditor`] with [`FileSidebarInput`] values, draws every row into
//! cells that it owns, and ends the editor through one shutdown.
//!
//! The run proves six facts of `docs/embedding.md`:
//!
//! - the host draws the tree over one worktree root without naming one type of
//!   `kvim-workspace`, which is no supported package;
//! - the editor reads no directory on the host loop: every listing leaves as
//!   one unit of work through the one channel that the host already drives for
//!   the editor, and returns through `apply`;
//! - the host moves the selection, opens one directory, and closes it again;
//! - one activated file returns from the input that produced it, and the host
//!   decides whether to open it;
//! - the host takes the look of kvim: `draw_file_row` paints one row exactly
//!   as kvim's own file tree paints it, and that tree paints through the same
//!   call, so no second appearance exists;
//! - kvim also publishes the Git state, the symbolic-link fact, and the icon
//!   role of one row as facts beside the drawn cells, so a host that wants a
//!   look of its own reads them and paints every cell itself.
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
use tokio::time::timeout;

use kvim_path::WorktreeRoot;
use kvim_runtime::{Runtime, RuntimeLimits, WORKER_CONCURRENCY_LIMIT_MAX};
use kvim_settings::{EditorSettings, FileTreeIcons};
use kvim_tui::{
    EditorCapacity, EditorShutdown, EditorWork, EmbeddedEditor, FileRow, FileRowGit, FileRowKind,
    FileSidebarInput, FileSidebarOutcome, ListMotion, RegionFocus, Theme, draw_file_row,
};
use kvim_ui::{RowKind, SidebarRow, SidebarState};
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

/// The icon setting that this host gives the painter.
///
/// Every icon glyph of kvim needs a patched font. This run prints its cells to
/// an ordinary terminal, so it hides them and takes the expansion markers that
/// the painter draws instead.
const ICONS: FileTreeIcons = FileTreeIcons::Hidden;

/// The focus that this host gives the painter.
///
/// The keys of this run reach the sidebar alone, so the sidebar holds the
/// focus and the painter marks the selected row at its left edge.
const FOCUS: RegionFocus = RegionFocus::Focused;

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

    let theme = Theme::new();
    let mut cells = CellBuffer::empty(HOST_AREA);
    draw(&mut cells, &editor, theme);
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
        print_facts(&row);
    }
    read_until(&mut editor, |editor| {
        selected_row(editor).is_some_and(|row| row.kind() == FileRowKind::OpenDirectory)
    })
    .await?;

    draw(&mut cells, &editor, theme);
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
    draw(&mut cells, &editor, theme);
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
/// The host owns the rectangle and the row geometry, so it holds one bounded
/// `SidebarState` of `kvim-ui` over the rows that the editor publishes. Every
/// visible row then reaches `draw_file_row`, which paints the mark cell, the
/// indent guides, the glyph cells, the label, the link suffix, and the Git
/// mark exactly as kvim's own file tree paints them. The host supplies the
/// palette, the icon setting, and the focus of its sidebar, and it already
/// holds all three.
///
/// This host gives the keys to the sidebar, so it reports
/// `RegionFocus::Focused` and the selected row takes the mark at its left
/// edge. A host whose keys reach another surface reports
/// `RegionFocus::Unfocused` instead, and the fill of the row alone reports the
/// selection.
///
/// The temporary worktree of this run holds no repository, so every row
/// carries no Git state and the mark column at the right edge stays blank.
fn draw(cells: &mut CellBuffer, editor: &EmbeddedEditor, theme: Theme) {
    cells.reset();
    let rows = editor.file_rows();
    let mut view = SidebarState::new(SIDEBAR_AREA.height);
    view.set_rows(
        (0..rows.len())
            .map(|index| SidebarRow::single(index, RowKind::Selectable))
            .collect(),
    )
    .expect("this worktree holds a handful of rows");
    let outcome = view.render(cells, SIDEBAR_AREA, |canvas, placement| {
        if let Some(row) = rows.get(placement.index()) {
            draw_file_row(canvas, row, theme, ICONS, FOCUS);
        }
    });
    outcome.expect("every row stays inside the rectangle of the sidebar");
}

/// Prints the facts that kvim publishes beside the drawn cells of one row.
///
/// A host that wants a look of its own reads these facts and paints every
/// cell itself, instead of calling the painter.
fn print_facts(row: &FileRow) {
    println!(
        "facts: label {:?}, kind {:?}, depth {}, guides {:?}, git {:?}, symlink {}, icon {:?}",
        row.label(),
        row.kind(),
        row.depth(),
        row.guides(),
        row.git().map(FileRowGit::glyph),
        row.is_symlink(),
        row.icon_role(),
    );
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
