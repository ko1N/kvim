//! Compose opaque host-owned surfaces through one shared key resolver.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use kvim_keymap::{
    Binding, CommandMetadata, Input, InputContextSnapshot, Key, KeyCode, Registry, Resolver, Scope,
};
use kvim_ui::{Composition, WindowLimits, WorkspaceComposer};
use ratatui::layout::Rect;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Command {
    Send,
}

impl fmt::Display for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for Command {
    fn id(&self) -> &str {
        "send"
    }

    fn label(&self) -> &str {
        "Send the message"
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Chat;

impl fmt::Display for Chat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Chat")
    }
}

impl Scope for Chat {
    const COUNT: usize = 1;
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let enter = Key::plain(KeyCode::Enter);
    let registry = Registry::from_bindings(&[Binding::surface(Chat, &[enter], Command::Send)], 2)?;
    let mut composer = WorkspaceComposer::new(
        "chat",
        InputContextSnapshot::idle(Chat),
        Rect::new(0, 0, 80, 24),
        WindowLimits::default(),
        Resolver::new(Arc::new(registry), 2, Duration::from_millis(500)),
    );

    assert_eq!(
        composer.reduce(Input::Key(enter), Some(Duration::ZERO)),
        Composition::Surface {
            surface: "chat",
            command: Command::Send,
        }
    );
    assert_eq!(composer.layout().surfaces().len(), 1);
    Ok(())
}
