//! Presents bounded annotations over one worktree source file.
//!
//! Run with:
//!
//! ```text
//! cargo run -p kvim-embed --example source_presentation --features worktree
//! ```

use std::{error::Error, fs, time::Duration};

use kvim_embed::{
    SourceAnnotation, SourceLineRange, SourcePresentation, SourcePresentationOutcome,
    WorktreeEditor,
};
use kvim_path::WorktreeRelativePath;
use ratatui::{buffer::Buffer, layout::Rect};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("kvim-presentation-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root)?;
    fs::write(root.join("sample.rs"), "fn first() {}\nfn second() {}\n")?;

    let mut editor = WorktreeEditor::builder(&root, Rect::new(0, 0, 48, 8)).open()?;
    let request = SourcePresentation::new(
        WorktreeRelativePath::new("sample.rs")?,
        vec![
            SourceAnnotation::new(SourceLineRange::new(1, 1)?, "First declaration")?,
            SourceAnnotation::new(SourceLineRange::new(2, 2)?, "Second declaration")?,
        ],
    )?;
    assert_eq!(
        editor.present_source(request)?,
        SourcePresentationOutcome::Queued
    );
    for _ in 0..64 {
        let _ = editor.dispatch();
        if let Some(result) = editor.take_source_presentation_result() {
            result?;
            break;
        }
        let completion = editor.ready().await;
        let _ = editor.apply(completion, Duration::ZERO)?;
    }
    assert!(
        editor.source_presentation().is_some(),
        "the bounded open completed"
    );
    let _ = editor.next_source_annotation()?;

    let mut cells = Buffer::empty(Rect::new(0, 0, 48, 8));
    let _ = editor.render(&mut cells)?;
    let snapshot = editor
        .source_presentation()
        .expect("presentation is visible");
    println!("{}: {}", snapshot.range().first(), snapshot.message());

    let _ = editor.shutdown(Duration::from_secs(5)).await;
    fs::remove_dir_all(root)?;
    Ok(())
}
