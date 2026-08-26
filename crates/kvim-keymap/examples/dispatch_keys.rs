//! Builds one binding registry, resolves a key sequence, and reads its hints.
//!
//! This is the whole keymap workflow that a host starts from. The crate holds
//! no terminal and no renderer, so the example prints plain lines instead.
//!
//! The example runs the same registry through two contexts. A context with one
//! scope is the standalone shape, and it reaches no interruption. A context
//! that also names a host-global scope is the embedded shape, and there a
//! host-global key cancels a pending editor sequence and runs.
//!
//! Run it with `cargo run -p kvim-keymap --example dispatch_keys`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use kvim_keymap::{
    Binding, CommandMetadata, CommandOwner, Dispatch, DispatchContext, Input, InputContextSnapshot,
    Key, KeyCode, Registry, RegistryError, Resolver, Scope,
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
    LeaveToHost,
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
            Self::LeaveToHost => "leave_to_host",
            Self::Save => "save",
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::FirstLine => "Go to the first line",
            Self::LastLine => "Go to the last line",
            Self::LeaveToHost => "Leave the editor for the host surface",
            Self::Save => "Save the buffer",
        }
    }
}

/// The binding scopes of this host.
///
/// The order of the variants is the order that the resolver walks. The
/// host-global scope therefore precedes the editor scope.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Table {
    Global,
    Editor,
}

impl fmt::Display for Table {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Global => "Global",
            Self::Editor => "Editor",
        })
    }
}

impl Scope for Table {
    const COUNT: usize = 2;
}

/// Returns one plain character key.
fn key(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

fn main() -> Result<(), RegistryError<Action, Table>> {
    // 1. The registry validates the whole contribution list once. A duplicate
    //    sequence or an ambiguous prefix pair fails here, not at dispatch time.
    let leave = Key::ctrl(KeyCode::Char('e'));
    let registry = Registry::from_bindings(
        &[
            Binding::host(Table::Global, &[leave], Action::LeaveToHost),
            Binding::surface(Table::Editor, &[key('g'), key('g')], Action::FirstLine),
            Binding::surface(Table::Editor, &[key('g'), key('e')], Action::LastLine),
            Binding::host(Table::Editor, &[key('s')], Action::Save),
        ],
        KEYS_MAX,
    )?;

    // 2. One resolver reads that one table, so no presentation layer holds a
    //    second one. It reads no clock: the caller supplies the elapsed time.
    let registry = Arc::new(registry);
    let mut resolver = Resolver::new(Arc::clone(&registry), KEYS_MAX, WHICH_KEY_DELAY);
    let context = DispatchContext::focused(InputContextSnapshot::idle(Table::Editor));

    // 3. A one-key binding of the host side reaches the host.
    report(&resolver.dispatch(&context, Input::Key(key('s')), Some(Duration::ZERO)));

    // 4. A prefix stays pending until the sequence completes.
    report(&resolver.dispatch(&context, Input::Key(key('g')), Some(Duration::ZERO)));

    // 5. While the sequence is pending, the hints come from the same table.
    if let Some(view) = resolver.which_key(WHICH_KEY_DELAY) {
        println!("which-key after `g`:");
        for scoped in view.hints() {
            let hint = scoped.hint();
            println!("  {}  {}", hint.key_label(), hint.commands()[0].label());
        }
    }

    // 6. The next key completes the sequence and reaches the focused surface.
    report(&resolver.dispatch(&context, Input::Key(key('g')), Some(WHICH_KEY_DELAY)));

    // 7. Input that no binding accepts is reported, never degraded.
    report(&resolver.dispatch(&context, Input::Key(key('q')), Some(Duration::ZERO)));
    report(&resolver.dispatch(&context, Input::Unsupported, Some(Duration::ZERO)));

    // 8. The context above names one scope, so no scope precedes the editor.
    //    A host-global key therefore only breaks the pending sequence there.
    report(&resolver.dispatch(&context, Input::Key(key('g')), Some(Duration::ZERO)));
    let standalone = resolver.dispatch(&context, Input::Key(leave), Some(Duration::ZERO));
    report(&standalone);
    assert_eq!(
        standalone,
        Dispatch::Unbound,
        "a context with one scope holds no preceding scope, so it reaches no interruption"
    );

    // 9. An embedded host names its own scope beside the focused one. That
    //    scope precedes the editor, so its complete one-key binding cancels a
    //    pending editor sequence and runs. The host resets the semantic state
    //    of the named surface before it runs the command, because the count,
    //    the operator, and the register of the cancelled sequence still sit
    //    there.
    let mut embedded = Resolver::new(Arc::clone(&registry), KEYS_MAX, WHICH_KEY_DELAY);
    let composed = DispatchContext {
        overlay: None,
        global: Some(Table::Global),
        focus: InputContextSnapshot::idle(Table::Editor),
    };
    report(&embedded.dispatch(&composed, Input::Key(key('g')), Some(Duration::ZERO)));

    // 10. The overlay of a pending prefix answers two lists. The hints
    //     continue the sequence, and the interruptions abandon it. Every
    //     published key runs at this moment.
    if let Some(view) = embedded.which_key(WHICH_KEY_DELAY) {
        println!("interruptions after `g` in the composed context:");
        for scoped in view.interruptions() {
            let hint = scoped.hint();
            println!(
                "  {}  {}  ({})",
                hint.key_label(),
                hint.commands()[0].label(),
                scoped.scope()
            );
        }
    }

    let interrupted = embedded.dispatch(&composed, Input::Key(leave), Some(WHICH_KEY_DELAY));
    report(&interrupted);
    assert_eq!(
        interrupted,
        Dispatch::Interrupted {
            owner: CommandOwner::Host,
            command: Action::LeaveToHost,
        },
        "the host-global scope precedes the editor scope, so its key cancels the sequence"
    );
    assert!(
        embedded.pending_keys().is_empty(),
        "the interruption leaves no prefix behind"
    );

    // 11. A conflicting table is refused at composition time.
    let conflict = Registry::from_bindings(
        &[
            Binding::surface(Table::Editor, &[key('g')], Action::FirstLine),
            Binding::surface(Table::Editor, &[key('g'), key('e')], Action::LastLine),
            Binding::host(Table::Editor, &[key('s')], Action::Save),
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
fn report(dispatch: &Dispatch<Action>) {
    match dispatch {
        Dispatch::Host { command } => println!("host runs `{}`", command.label()),
        Dispatch::Surface { command } => println!("surface runs `{}`", command.label()),
        Dispatch::Interrupted { owner, command } => println!(
            "{owner} runs `{}`, and the pending sequence is cancelled",
            command.label()
        ),
        Dispatch::Text { owner, text } => println!("{owner} takes text {text:?}"),
        Dispatch::Pending => println!("the sequence is pending"),
        Dispatch::Unsupported => println!("the terminal reported unsupported input"),
        Dispatch::Unbound => println!("no binding and no text fallback took the input"),
        Dispatch::Cancelled => println!("the input cancelled the focused scope"),
    }
}
