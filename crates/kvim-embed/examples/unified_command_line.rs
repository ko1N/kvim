//! Composes a host command with kvim's addressed editor command line.
//!
//! Run with:
//!
//! ```text
//! cargo run -p kvim-embed --example unified_command_line --features worktree
//! ```

use std::fs;
use std::time::Duration;

use kvim_embed::{
    EditorCommandId, EditorCommandRequestId, SurfaceOwnership, WorktreeCommandSurface,
    WorktreeEditor, WorktreeInputRequest, WorktreePresentation,
};
use kvim_input::Command;
use ratatui::layout::Rect;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("kvim-unified-command-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/main.rs"), "fn main() {}\n")?;

    let presentation = WorktreePresentation::standalone().command_line(SurfaceOwnership::HostOwned);
    let mut editor = WorktreeEditor::builder(&root, Rect::new(0, 0, 60, 10))
        .presentation(presentation)
        .command_surface(WorktreeCommandSurface::new())
        .open()?;

    let outcome = editor.command(Command::OpenCommandLine, None, None, Duration::ZERO)?;
    let kvim_embed::WorktreeInputOutcome::Request(WorktreeInputRequest::OpenCommandLine(session)) =
        outcome
    else {
        return Err("editor did not request the host command line".into());
    };

    // The host owns this text, cursor, selection, and history.
    let host_line = "edit src/m";
    let names = editor.command_catalog().complete_names("ed");
    println!(
        "editor names: {:?}; host command: host.help",
        names.candidates()
    );

    editor.request_command_completion(
        session,
        EditorCommandRequestId::new(1).expect("one is nonzero"),
        host_line,
    )?;
    let mut paths = None;
    for _ in 0..64 {
        editor.dispatch();
        let ready = editor.ready().await;
        editor.apply(ready, Duration::ZERO)?;
        paths = editor.take_command_completion();
        if paths.is_some() {
            break;
        }
    }
    let paths = paths.expect("the applied request publishes candidates");
    println!("path candidates: {:?}", paths.candidates());

    let addressed = editor.command_catalog().address(EditorCommandId::Edit);
    editor.execute_session_command(session, addressed, "edit src/main.rs")?;
    fs::remove_dir_all(root)?;
    Ok(())
}
