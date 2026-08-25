//! Builds one binding registry, resolves a key sequence, and reads its hints.
//!
//! This is the whole keymap workflow that a host starts from. The crate holds
//! no terminal and no renderer, so the example prints plain lines instead.
//!
//! Run it with `cargo run -p kvim-keymap --example dispatch_keys`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use kvim_keymap::{
    Binding, CommandMetadata, Dispatch, DispatchContext, Input, InputContextSnapshot, Key, KeyCode,
    Registry, RegistryError, Resolver, Scope,
};

/// The pending-key limit of this host.
const KEYS_MAX: u8 = 4;

/// The wait before the which-key overlay first appears.
const WHICH_KEY_DELAY: Duration = Duration::from_millis(500);

/// The commands that this host binds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Action {
    FirstLine,
    LastLine,
    Save,
}

impl fmt::Display for Action {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for Action {
    fn id(&self) -> &str {
        match self {
            Self::FirstLine => "first_line",
            Self::LastLine => "last_line",
            Self::Save => "save",
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::FirstLine => "Go to the first line",
            Self::LastLine => "Go to the last line",
            Self::Save => "Save the buffer",
        }
    }
}

/// The one binding scope of this host.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Editor;

impl fmt::Display for Editor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Editor")
    }
}

impl Scope for Editor {
    const COUNT: usize = 1;
}

/// Returns one plain character key.
fn key(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

fn main() -> Result<(), RegistryError<Action, Editor>> {
    // 1. The registry validates the whole contribution list once. A duplicate
    //    sequence or an ambiguous prefix pair fails here, not at dispatch time.
    let registry = Registry::from_bindings(
        &[
            Binding::surface(Editor, &[key('g'), key('g')], Action::FirstLine),
            Binding::surface(Editor, &[key('g'), key('e')], Action::LastLine),
            Binding::host(Editor, &[key('s')], Action::Save),
        ],
        KEYS_MAX,
    )?;

    // 2. One resolver reads that one table, so no presentation layer holds a
    //    second one. It reads no clock: the caller supplies the elapsed time.
    let mut resolver = Resolver::new(Arc::new(registry), KEYS_MAX, WHICH_KEY_DELAY);
    let context = DispatchContext::focused(InputContextSnapshot::idle(Editor));

    // 3. A one-key binding of the host side reaches the host.
    report(resolver.dispatch(&context, Input::Key(key('s')), Some(Duration::ZERO)));

    // 4. A prefix stays pending until the sequence completes.
    report(resolver.dispatch(&context, Input::Key(key('g')), Some(Duration::ZERO)));

    // 5. While the sequence is pending, the hints come from the same table.
    if let Some(view) = resolver.which_key(WHICH_KEY_DELAY) {
        println!("which-key after `g`:");
        for hint in view.hints() {
            println!("  {}  {}", hint.key_label(), hint.commands()[0].label());
        }
    }

    // 6. The next key completes the sequence and reaches the focused surface.
    report(resolver.dispatch(&context, Input::Key(key('g')), Some(WHICH_KEY_DELAY)));

    // 7. Input that no binding accepts is reported, never degraded.
    report(resolver.dispatch(&context, Input::Key(key('q')), Some(Duration::ZERO)));
    report(resolver.dispatch(&context, Input::Unsupported, Some(Duration::ZERO)));

    // 8. A conflicting table is refused at composition time.
    let conflict = Registry::from_bindings(
        &[
            Binding::surface(Editor, &[key('g')], Action::FirstLine),
            Binding::surface(Editor, &[key('g'), key('e')], Action::LastLine),
            Binding::host(Editor, &[key('s')], Action::Save),
        ],
        KEYS_MAX,
    );
    match conflict {
        Ok(_) => panic!("a strict prefix pair must not compose"),
        Err(error) => println!("refused the conflicting table: {error}"),
    }

    Ok(())
}

/// Prints one dispatch outcome in its plain form.
fn report(dispatch: Dispatch<Action>) {
    match dispatch {
        Dispatch::Host { command } => println!("host runs `{}`", command.label()),
        Dispatch::Surface { command } => println!("surface runs `{}`", command.label()),
        Dispatch::Text { owner, text } => println!("{owner} takes text {text:?}"),
        Dispatch::Pending => println!("the sequence is pending"),
        Dispatch::Unsupported => println!("the terminal reported unsupported input"),
        Dispatch::Unbound => println!("no binding and no text fallback took the input"),
    }
}
