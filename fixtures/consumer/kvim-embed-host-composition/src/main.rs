use std::{error::Error, fmt, time::Duration};

use kvim_embed::{
    WorktreeBindingFocus, WorktreeBindingModel, WorktreeHostBinding, WorktreeHostBindingLayer,
    WorktreeHostCommand, WorktreeMergedCommand,
};
use kvim_input::{BindingProfile, BindingScope, InputContextSnapshot, Key, KeyCode, Mode};
use kvim_keymap::{CommandMetadata, Dispatch, Input, Resolver};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum HostCommand {
    LeaveEditor,
    OpenSessions,
}

impl fmt::Display for HostCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for HostCommand {
    fn id(&self) -> &str {
        match self {
            Self::LeaveEditor => "host-leave-editor",
            Self::OpenSessions => "host-open-sessions",
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::LeaveEditor => "Leave editor",
            Self::OpenSessions => "Open sessions",
        }
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

fn key(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = BindingProfile::Embedded.manifest()?;
    let host = [
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Global,
            &[Key::ctrl(KeyCode::Char('e'))],
            HostCommand::LeaveEditor,
        )?,
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Editor),
            &[key(' '), key('s')],
            HostCommand::OpenSessions,
        )?,
    ];
    let model = WorktreeBindingModel::compose(&manifest, &host)?;
    let context = WorktreeBindingModel::<HostCommand>::editor_snapshot_context(
        InputContextSnapshot::idle(BindingScope::Mode(Mode::Normal)),
        None,
    );
    let mut resolver = Resolver::new(model.registry(), 4, Duration::ZERO);
    assert_eq!(
        resolver.dispatch(&context, Input::Key(key(' ')), Some(Duration::ZERO)),
        Dispatch::Pending,
    );
    let which_key = resolver.which_key(Duration::ZERO).expect("merged menu");
    assert!(which_key.hints().iter().any(|hint| {
        hint.hint()
            .commands()
            .contains(&WorktreeMergedCommand::Host(HostCommand::OpenSessions))
    }));
    assert_eq!(
        resolver.dispatch(&context, Input::Key(key('s')), Some(Duration::ZERO)),
        Dispatch::Host {
            command: WorktreeMergedCommand::Host(HostCommand::OpenSessions),
        },
    );
    Ok(())
}
