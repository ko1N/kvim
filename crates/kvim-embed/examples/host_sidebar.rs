//! Draws a host tree and kvim's tree as two independent host-owned regions.
//!
//! Run with:
//!
//! ```text
//! cargo run -p kvim-embed --example host_sidebar --features worktree
//! ```

use std::error::Error;
use std::fs;
use std::num::NonZeroU16;
use std::time::Duration;

use kvim_embed::{
    FileSidebarCommand, FileSidebarLabelMatch, SurfaceOwnership, WorktreeEditor,
    WorktreePresentation, WorktreeShutdown,
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

        // Record only the visible body. Do not include host tabs or other chrome.
        editor.record_file_sidebar_viewport(
            NonZeroU16::new(10).expect("the visible body has rows"),
            NonZeroU16::new(30).expect("the visible body has cells"),
        )?;
        let search = editor.begin_file_sidebar_search()?;
        editor.accept_file_sidebar_search(search, "README")?;

        println!("host tree: {}", host_rows.join(", "));
        let snapshot = editor
            .file_sidebar_snapshot()
            .expect("the host owns the sidebar");
        for row in snapshot.rows() {
            println!(
                "kvim tree: {:?} icon={:?} dimming={:?} notice={:?} {}",
                row.id(),
                row.icon_glyph(),
                row.dimming(),
                row.notice_kind(),
                render_label_match(row.label(), row.matched_characters())
            );
        }
        let selected = snapshot
            .rows()
            .iter()
            .find(|row| !matches!(row.kind(), kvim_embed::FileSidebarRowKind::Notice(_)))
            .expect("the worktree contains selectable rows");
        let _ = editor.file_sidebar_command(FileSidebarCommand::Select(selected.id().clone()));

        let _ = editor.next_file_sidebar_match(search)?;
        let _ = editor.previous_file_sidebar_match(search)?;
        editor.move_file_sidebar_half_page_down()?;
        editor.move_file_sidebar_half_page_up()?;
        editor.move_file_sidebar_full_page_down()?;
        editor.move_file_sidebar_full_page_up()?;
        editor.end_file_sidebar_search(search)?;

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

fn render_label_match(label: &str, matched: Option<FileSidebarLabelMatch>) -> String {
    let Some(matched) = matched else {
        return label.to_owned();
    };
    let start = matched.start();
    let end = start.saturating_add(matched.len());
    let mut rendered = String::with_capacity(label.len().saturating_add(2));
    for (index, character) in label.chars().enumerate() {
        if index == start {
            rendered.push('[');
        }
        rendered.push(character);
        if index.saturating_add(1) == end {
            rendered.push(']');
        }
    }
    rendered
}
