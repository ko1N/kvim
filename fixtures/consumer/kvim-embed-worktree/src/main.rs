use std::{error::Error, fs, time::Duration};

use kvim_embed::{WorktreeEditor, WorktreeEvent, WorktreeShutdown};
use kvim_input::Command;
use kvim_path::WorktreeRelativePath;
use ratatui::{buffer::Buffer, layout::Rect};

const STEPS_MAX: usize = 64;

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!(
        "kvim-external-worktree-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("main")
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root)?;
    fs::write(root.join("note.txt"), "hello\n")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(&root))?;
    assert_eq!(fs::read_to_string(root.join("note.txt"))?, "saved hello\n");
    fs::remove_dir_all(root)?;
    Ok(())
}

async fn run(root: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let area = Rect::new(0, 0, 40, 8);
    let mut editor = WorktreeEditor::builder(root, area).open()?;
    editor.render(&mut Buffer::empty(area))?;
    editor.open_file(WorktreeRelativePath::new("note.txt")?);
    drive_until(&mut editor, |event| {
        matches!(event, WorktreeEvent::ActiveFileChanged { .. })
    })
    .await?;

    editor.command(Command::InsertBeforeCursor, None, None, Duration::ZERO)?;
    editor.literal("saved ", Duration::ZERO);
    editor.command(Command::ReturnToNormal, None, None, Duration::ZERO)?;
    editor.render(&mut Buffer::empty(area))?;
    editor.command(Command::SaveBuffer, None, None, Duration::ZERO)?;
    drive_until(&mut editor, |event| {
        matches!(event, WorktreeEvent::FileWritten { .. })
    })
    .await?;

    match editor.shutdown(Duration::from_secs(5)).await {
        WorktreeShutdown::Finished { events } => drop(events),
        WorktreeShutdown::Draining(drain) => drop(drain.complete().await),
    }
    Ok(())
}

async fn drive_until(
    editor: &mut WorktreeEditor,
    wanted: impl Fn(&WorktreeEvent) -> bool,
) -> Result<(), Box<dyn Error>> {
    for _ in 0..STEPS_MAX {
        editor.dispatch();
        while let Some(event) = editor.take_event() {
            if wanted(&event) {
                return Ok(());
            }
        }
        let completion = editor.ready().await;
        editor
            .apply(completion, Duration::ZERO)
            .expect("ready returns this editor's completion");
    }
    Err("the bounded drive produced no expected event".into())
}
