//! Opens, edits, renders, saves, and shuts down a worktree editor.
//!
//! Run with:
//!
//! ```text
//! cargo run -p kvim-embed --example worktree_editor --features worktree
//! ```

use std::error::Error;
use std::fs;
use std::time::Duration;

use kvim_embed::{WorktreeEditor, WorktreeEvent, WorktreeShutdown};
use kvim_input::Command;
use kvim_path::WorktreeRelativePath;
use ratatui::{buffer::Buffer, layout::Rect};

const STEPS_MAX: usize = 64;

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("kvim-worktree-example-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root)?;
    fs::write(root.join("note.txt"), "hello\n")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let area = Rect::new(0, 0, 40, 8);
        let mut editor = WorktreeEditor::builder(&root, area).open()?;
        editor.open_file(WorktreeRelativePath::new("note.txt")?);
        drive_until(&mut editor, |event| {
            matches!(event, WorktreeEvent::ActiveFileChanged { .. })
        })
        .await?;

        editor.command(Command::InsertBeforeCursor, None, None, Duration::ZERO)?;
        editor.literal("saved ", Duration::ZERO);
        editor.command(Command::ReturnToNormal, None, None, Duration::ZERO)?;
        let mut cells = Buffer::empty(area);
        let cursor = editor.render(&mut cells)?;
        println!("cursor: {:?}", cursor.position);

        editor.command(Command::SaveBuffer, None, None, Duration::ZERO)?;
        drive_until(&mut editor, |event| {
            matches!(event, WorktreeEvent::FileWritten { .. })
        })
        .await?;
        match editor.shutdown(Duration::from_secs(5)).await {
            WorktreeShutdown::Finished { events } => println!("shutdown events: {}", events.len()),
            WorktreeShutdown::Draining(drain) => {
                println!("drain events: {}", drain.complete().await.len())
            }
        }
        Ok::<(), Box<dyn Error>>(())
    })?;
    fs::remove_dir_all(root)?;
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
        let completed = editor.ready().await;
        editor.apply(completed, Duration::ZERO);
    }
    Err("the bounded editor drive did not produce the expected event".into())
}
