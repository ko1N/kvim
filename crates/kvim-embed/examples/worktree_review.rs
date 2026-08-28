//! Captures and renders a standalone review directly from a worktree.
//!
//! Run with:
//!
//! ```text
//! cargo run -p kvim-embed --example worktree_review --features worktree
//! ```

use std::{error::Error, fs, process::Command, time::Duration};

use kvim_embed::{ReviewConfig, ReviewEvent, ReviewShutdown, ReviewSurface};
use ratatui::{buffer::Buffer, layout::Rect};

struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const STEPS_MAX: usize = 128;

fn git(root: &std::path::Path, arguments: &[&str]) -> Result<(), Box<dyn Error>> {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .status()?;
    if !status.success() {
        return Err("example Git command failed".into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("kvim-review-example-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root)?;
    let _cleanup = Cleanup(root.clone());
    git(&root, &["init", "-q"])?;
    git(&root, &["config", "user.email", "review@example.invalid"])?;
    git(&root, &["config", "user.name", "Review Example"])?;
    fs::write(root.join("note.txt"), "one\n")?;
    git(&root, &["add", "note.txt"])?;
    git(&root, &["commit", "-qm", "initial"])?;
    fs::write(root.join("note.txt"), "one\ntwo\n")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let area = Rect::new(0, 0, 72, 16);
        let mut review = ReviewSurface::for_worktree(&root, ReviewConfig::new(area))?;
        let mut finished = false;
        for _ in 0..STEPS_MAX {
            review.dispatch()?;
            let completion = tokio::time::timeout(Duration::from_secs(5), review.ready()).await??;
            review.apply(completion)?;
            while let Some(event) = review.event() {
                if matches!(event, ReviewEvent::CaptureFinished { .. }) {
                    finished = true;
                }
            }
            if finished {
                break;
            }
        }
        assert!(finished, "bounded capture completes");
        review.render(&mut Buffer::empty(area))?;
        match review.shutdown(Duration::from_secs(5)).await? {
            ReviewShutdown::Finished { .. } => {}
            ReviewShutdown::Draining(drain) => {
                tokio::time::timeout(Duration::from_secs(5), drain.complete()).await?;
            }
        }
        Ok::<(), Box<dyn Error>>(())
    })?;
    fs::remove_dir_all(root)?;
    Ok(())
}
