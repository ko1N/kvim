//! Opens a worktree editor with host-owned command and status chrome.
//!
//! Run with:
//!
//! ```text
//! cargo run -p kvim-embed --example host_owned_chrome --features worktree
//! ```

use std::{error::Error, fs, time::Duration};

use kvim_embed::{
    SurfaceOwnership, WorktreeCommandSurface, WorktreeEditor, WorktreePresentation,
    WorktreeShutdown,
};
use ratatui::{buffer::Buffer, layout::Rect};

struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("kvim-chrome-example-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root)?;
    let _cleanup = Cleanup(root.clone());

    let area = Rect::new(0, 0, 60, 10);
    let presentation = WorktreePresentation::standalone()
        .command_line(SurfaceOwnership::HostOwned)
        .statusline(SurfaceOwnership::HostOwned);
    let editor = WorktreeEditor::builder(&root, area)
        .presentation(presentation)
        .command_surface(WorktreeCommandSurface::new())
        .open()?;

    // The host uses semantic status fields to draw its own statusline.
    let status = editor.status();
    println!(
        "instance={:?} mode={:?} modified={}",
        status.instance(),
        status.mode(),
        status.is_modified(),
    );
    editor.render(&mut Buffer::empty(area))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        match editor.shutdown(Duration::from_secs(5)).await {
            WorktreeShutdown::Finished { .. } => {}
            WorktreeShutdown::Draining(drain) => {
                tokio::time::timeout(Duration::from_secs(5), drain.complete()).await?;
            }
        }
        Ok::<(), tokio::time::error::Elapsed>(())
    })?;
    fs::remove_dir_all(root)?;
    Ok(())
}
