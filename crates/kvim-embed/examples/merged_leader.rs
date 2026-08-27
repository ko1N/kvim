//! Compose opaque Keel commands with kvim's embedded bindings and one which-key model.

use std::fmt;
use std::time::Duration;

use kvim_embed::{
    WorktreeBindingFocus, WorktreeBindingModel, WorktreeHostBinding, WorktreeHostBindingLayer,
    WorktreeHostCommand, WorktreeMergedCommand,
};
use kvim_input::{BindingProfile, BindingScope, InputContextSnapshot, Key, KeyCode, Mode};
use kvim_keymap::{CommandMetadata, Dispatch, Input, Resolver};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum KeelCommand {
    LeaveEditor,
    OpenSessions,
}

impl fmt::Display for KeelCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for KeelCommand {
    fn id(&self) -> &str {
        match self {
            Self::LeaveEditor => "keel-leave-editor",
            Self::OpenSessions => "keel-open-sessions",
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::LeaveEditor => "Leave editor",
            Self::OpenSessions => "Open sessions",
        }
    }
}

impl WorktreeHostCommand for KeelCommand {
    fn owner_label(&self) -> &str {
        "Keel"
    }

    fn group_label(&self) -> &str {
        "workspace"
    }
}

fn key(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = BindingProfile::Embedded.manifest()?;
    let host = [
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Global,
            &[Key::ctrl(KeyCode::Char('e'))],
            KeelCommand::LeaveEditor,
        )?,
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Editor),
            &[key(' '), key('s')],
            KeelCommand::OpenSessions,
        )?,
    ];
    let model = WorktreeBindingModel::compose(&manifest, &host)?;
    let context = WorktreeBindingModel::<KeelCommand>::editor_snapshot_context(
        InputContextSnapshot::idle(BindingScope::Mode(Mode::Normal)),
        None,
    );
    let mut resolver = Resolver::new(model.registry(), 4, Duration::ZERO);

    assert_eq!(
        resolver.dispatch(&context, Input::Key(key(' ')), Some(Duration::ZERO)),
        Dispatch::Pending
    );
    let view = resolver
        .which_key(Duration::ZERO)
        .expect("the zero delay publishes the one merged menu");
    let hints = view.hints();
    for scoped in &hints {
        for command in scoped.hint().commands() {
            println!(
                "{}  {:<12} {:<10} {}",
                scoped.hint().key_label(),
                command.owner_label(),
                command.group_label(),
                command.label(),
            );
        }
    }

    let sessions = hints
        .iter()
        .flat_map(|hint| hint.hint().commands())
        .find(|command| **command == WorktreeMergedCommand::Host(KeelCommand::OpenSessions))
        .expect("the merged host leader command is visible");
    println!(
        "dispatch: owner={} group={} command={}",
        sessions.owner_label(),
        sessions.group_label(),
        sessions.id(),
    );
    Ok(())
}
