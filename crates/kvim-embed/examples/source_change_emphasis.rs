//! Follows one settled source change without replacing diagnostics or presentation.
//!
//! Run with:
//!
//! ```text
//! cargo run -p kvim-embed --example source_change_emphasis --features worktree
//! ```

use std::{error::Error, fs, time::Duration};

use kvim_embed::{
    SourceChangeEmphasis, SourceChangeEmphasisOutcome, SourceLineRange, WorktreeEditor,
};
use kvim_path::WorktreeRelativePath;
use ratatui::{buffer::Buffer, layout::Rect};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("kvim-source-change-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root)?;
    fs::write(root.join("sample.rs"), "fn before() {}\nfn after() {}\n")?;

    let mut editor = WorktreeEditor::builder(&root, Rect::new(0, 0, 48, 8)).open()?;
    let request = SourceChangeEmphasis::new(
        WorktreeRelativePath::new("sample.rs")?,
        vec![SourceLineRange::new(2, 2)?],
    )?;
    assert_eq!(
        editor.emphasize_source_change(request)?,
        SourceChangeEmphasisOutcome::Queued
    );
    for _ in 0..64 {
        let _ = editor.dispatch();
        if let Some(result) = editor.take_source_change_emphasis_result() {
            result?;
            break;
        }
        let completion = editor.ready().await;
        let _ = editor.apply(completion, Duration::ZERO)?;
    }

    let emphasis = editor
        .source_change_emphasis()
        .expect("source change is visible");
    assert_eq!(emphasis.range(0), Some(SourceLineRange::new(2, 2)?));
    let mut cells = Buffer::empty(Rect::new(0, 0, 48, 8));
    let _ = editor.render(&mut cells)?;

    let _ = editor.shutdown(Duration::from_secs(5)).await;
    fs::remove_dir_all(root)?;
    Ok(())
}
