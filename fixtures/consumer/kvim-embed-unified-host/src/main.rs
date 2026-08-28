use std::{error::Error, fmt, fs, time::Duration};

use kvim_embed::{
    WorktreeBindingFocus, WorktreeBindingMode, WorktreeBindingModel, WorktreeCommandSurface,
    WorktreeEditor, WorktreeHostBinding, WorktreeHostBindingLayer, WorktreeHostCommand,
    WorktreeInputRequest, WorktreePresentation, WorktreeShutdown,
};
use kvim_input::{Command, InputContextSnapshot, Key, KeyCode};
use kvim_keymap::{CommandMetadata, Dispatch, Input, Resolver};
use ratatui::{buffer::Buffer, layout::Rect};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum HostCommand {
    OpenSessions,
}

impl fmt::Display for HostCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for HostCommand {
    fn id(&self) -> &str {
        "host-open-sessions"
    }

    fn label(&self) -> &str {
        "Open sessions"
    }
}

impl WorktreeHostCommand for HostCommand {
    fn owner_label(&self) -> &str {
        "Host"
    }

    fn group_label(&self) -> &str {
        "workspace"
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("kvim-unified-consumer-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root)?;

    let mut editor = WorktreeEditor::builder(&root, Rect::new(0, 0, 48, 8))
        .binding_mode(WorktreeBindingMode::HostResolved {
            reserved_escape: Key::ctrl(KeyCode::Char(']')),
        })
        .presentation(WorktreePresentation::integrated_host())
        .command_surface(WorktreeCommandSurface::new())
        .open()?;
    let host = [WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Editor),
        &[Key::plain(KeyCode::Char(' ')), Key::plain(KeyCode::Char('s'))],
        HostCommand::OpenSessions,
    )?];
    let model = WorktreeBindingModel::compose(
        editor.binding_manifest().expect("host-resolved manifest"),
        &host,
    )?;
    let context = WorktreeBindingModel::<HostCommand>::editor_snapshot_context(
        InputContextSnapshot::idle(editor.input_context().scope),
        None,
    );
    let mut resolver = Resolver::new(model.registry(), 4, Duration::ZERO);
    assert_eq!(
        resolver.dispatch(
            &context,
            Input::Key(Key::plain(KeyCode::Char(' '))),
            Some(Duration::ZERO),
        ),
        Dispatch::Pending,
    );
    assert!(resolver.which_key(Duration::ZERO).is_some());
    assert_eq!(
        resolver.dispatch(
            &context,
            Input::Key(Key::plain(KeyCode::Char('s'))),
            Some(Duration::ZERO),
        ),
        Dispatch::Host {
            command: kvim_embed::WorktreeMergedCommand::Host(HostCommand::OpenSessions),
        },
    );
    assert!(!editor.command_catalog().descriptors().is_empty());
    assert_eq!(editor.status().instance(), editor.instance());

    let outcome = editor.command(Command::OpenCommandLine, None, None, Duration::ZERO)?;
    assert!(matches!(
        outcome,
        kvim_embed::WorktreeInputOutcome::Request(WorktreeInputRequest::OpenCommandLine(_))
    ));
    editor.render(&mut Buffer::empty(Rect::new(0, 0, 48, 8)))?;

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
