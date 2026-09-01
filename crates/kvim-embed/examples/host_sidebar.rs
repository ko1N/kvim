//! Draws a host tree and kvim's tree as two independent host-owned regions.
//!
//! Run with:
//!
//! ```text
//! cargo run -p kvim-embed --example host_sidebar --features worktree
//! ```

use std::error::Error;
use std::fs;
use std::time::Duration;

use kvim_embed::{
    FileSidebarCommand, SurfaceOwnership, WorktreeEditor, WorktreePresentation, WorktreeShutdown,
};
use ratatui::layout::Rect;

const STEPS_MAX: usize = 64;

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("kvim-sidebar-example-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(root.join("README.md"), "host sidebar\n")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let presentation =
            WorktreePresentation::standalone().file_sidebar(SurfaceOwnership::HostOwned);
        let mut editor = WorktreeEditor::builder(&root, Rect::new(20, 0, 60, 12))
            .presentation(presentation)
            .open()?;

        // This host tree remains separate. Kvim never receives or merges it.
        let host_rows = ["sessions", "agents"];
        for _ in 0..STEPS_MAX {
            editor.dispatch();
            if editor
                .file_sidebar_snapshot()
                .is_some_and(|snapshot| !snapshot.rows().is_empty())
            {
                break;
            }
            let completion = tokio::time::timeout(Duration::from_secs(5), editor.ready()).await?;
            editor.apply(completion, Duration::ZERO)?;
        }

        println!("host tree: {}", host_rows.join(", "));
        let snapshot = editor
            .file_sidebar_snapshot()
            .expect("the host owns the sidebar");
        for row in snapshot.rows() {
            println!(
                "kvim tree: {:?} icon={:?} dimming={:?} notice={:?} match={:?} {}",
                row.id(),
                row.icon_glyph(),
                row.dimming(),
                row.notice_kind(),
                row.matched_characters(),
                row.label()
            );
        }
        let _ = editor.file_sidebar_command(FileSidebarCommand::MoveDown);

        match editor.shutdown(Duration::from_secs(5)).await {
            WorktreeShutdown::Finished { .. } => {}
            WorktreeShutdown::Draining(drain) => {
                let _ = drain.complete().await;
            }
        }
        Ok::<(), Box<dyn Error>>(())
    })?;
    fs::remove_dir_all(root)?;
    Ok(())
}
