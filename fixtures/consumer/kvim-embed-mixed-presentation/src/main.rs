use std::{error::Error, fs};

use kvim_embed::{
    SurfaceOwnership, WorktreeBindingMode, WorktreeCommandSurface, WorktreeEditor,
    WorktreePresentation, WorktreeShutdown,
};
use kvim_input::{Key, KeyCode};
use ratatui::{buffer::Buffer, layout::Rect};

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("kvim-mixed-consumer-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root)?;

    let area = Rect::new(0, 0, 48, 8);
    let presentation = WorktreePresentation::standalone()
        .command_line(SurfaceOwnership::HostOwned)
        .which_key(SurfaceOwnership::HostOwned);
    let editor = WorktreeEditor::builder(&root, area)
        .binding_mode(WorktreeBindingMode::HostResolved {
            reserved_escape: Key::ctrl(KeyCode::Char(']')),
        })
        .presentation(presentation)
        .command_surface(WorktreeCommandSurface::new())
        .open()?;
    assert!(editor.binding_context().is_some());
    assert_eq!(editor.status().instance(), editor.instance());
    editor.render(&mut Buffer::empty(area))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        match editor.shutdown(std::time::Duration::from_secs(5)).await {
            WorktreeShutdown::Finished { .. } => {}
            WorktreeShutdown::Draining(drain) => {
                tokio::time::timeout(std::time::Duration::from_secs(5), drain.complete()).await?;
            }
        }
        Ok::<(), tokio::time::error::Elapsed>(())
    })?;
    fs::remove_dir_all(root)?;
    Ok(())
}
