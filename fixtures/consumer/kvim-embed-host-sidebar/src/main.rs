use std::{error::Error, fs, time::Duration};

use kvim_embed::{
    FileSidebarCommand, SurfaceOwnership, WorktreeEditor, WorktreePresentation, WorktreeShutdown,
};
use ratatui::layout::Rect;

const STEPS_MAX: usize = 64;

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("kvim-sidebar-consumer-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root)?;
    fs::write(root.join("note.txt"), "sidebar\n")?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let presentation =
            WorktreePresentation::standalone().file_sidebar(SurfaceOwnership::HostOwned);
        let mut editor = WorktreeEditor::builder(&root, Rect::new(20, 0, 48, 8))
            .presentation(presentation)
            .open()?;
        for _ in 0..STEPS_MAX {
            editor.dispatch();
            if editor
                .file_sidebar_snapshot()
                .is_some_and(|snapshot| !snapshot.rows().is_empty())
            {
                break;
            }
            let completion =
                tokio::time::timeout(Duration::from_secs(5), editor.ready()).await?;
            editor.apply(completion, Duration::ZERO)?;
        }
        let snapshot = editor.file_sidebar_snapshot().expect("host-owned sidebar");
        assert_eq!(snapshot.instance(), editor.instance());
        assert!(snapshot.rows().iter().any(|row| row.label() == "note.txt"));
        let outcome = editor.file_sidebar_command(FileSidebarCommand::MoveDown);
        assert!(matches!(
            outcome,
            kvim_embed::FileSidebarOutcome::Applied(_)
        ));
        match editor.shutdown(Duration::from_secs(5)).await {
            WorktreeShutdown::Finished { .. } => {}
            WorktreeShutdown::Draining(drain) => {
                tokio::time::timeout(Duration::from_secs(5), drain.complete()).await?;
            }
        }
        Ok::<(), Box<dyn Error>>(())
    })?;
    fs::remove_dir_all(root)?;
    Ok(())
}
