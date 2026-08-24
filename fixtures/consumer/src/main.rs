//! One external consumer of every public kvim package.
//!
//! The program is not an example of one feature. It exists to prove that an
//! outside repository can name the public facades through a revision-pinned Git
//! dependency, without a shared parent workspace and without a test seam.
//!
//! It compiles and runs under every combination of the public feature matrix,
//! including the default build, which bundles no grammar at all.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use kvim_keymap::{
    Binding, CommandMetadata, Dispatch, DispatchContext, Input, InputContextSnapshot, Key, KeyCode,
    Registry, Resolver, Scope,
};
use kvim_lsp::{DiagnosticsLimits, DocumentRevision, ManagerLimits, WaitPolicy};
use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_syntax::{HighlightLimits, NeverCancelled, SyntaxHighlighter};
use kvim_tui::{EditorAccess, EditorCapacity, EditorEvent};
use kvim_ui::{ChildSide, Orientation, WindowLimits, WindowTree};

/// The host area that this consumer paints.
const HOST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

/// The longest key sequence that the table below binds.
const KEYS_MAX: u8 = 2;

/// The wait before a which-key overlay would appear.
const WHICH_KEY_DELAY: Duration = Duration::from_millis(500);

/// The commands that this host owns.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum HostCommand {
    Quit,
    SplitRight,
}

impl fmt::Display for HostCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for HostCommand {
    fn id(&self) -> &str {
        match self {
            Self::Quit => "quit",
            Self::SplitRight => "split-right",
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Quit => "Quit",
            Self::SplitRight => "Split right",
        }
    }
}

/// The one scope that this host owns.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Global;

impl fmt::Display for Global {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Global")
    }
}

impl Scope for Global {
    const COUNT: usize = 1;
}

/// The opaque surface identity that this host gives one window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SurfaceId(u16);

fn main() {
    check_path();
    check_syntax();
    check_keymap();
    check_ui();
    check_lsp();
    check_embedded_editor();
    println!("every public kvim facade compiles and answers.");
}

/// Names one worktree root and one safe relative path.
fn check_path() {
    let directory = std::env::temp_dir();
    let root = WorktreeRoot::open(&directory).expect("the temporary directory exists");
    let relative = WorktreeRelativePath::new("notes/todo.md").expect("the path stays inside");
    println!("root {} holds {}", root.as_path().display(), relative.as_path().display());
}

/// Highlights one fragment when the build bundles the Rust grammar.
///
/// The default build bundles no grammar, so the lookup answers `None` and the
/// consumer stays correct without a parser.
fn check_syntax() {
    let mut highlighter = SyntaxHighlighter::new();
    match kvim_syntax::language("rust") {
        Some(entry) => {
            let highlighted = highlighter
                .highlight(
                    entry,
                    "fn main() {}\n",
                    &HighlightLimits::default(),
                    &NeverCancelled,
                )
                .expect("the fragment stays inside every bound");
            println!("the Rust grammar returned {} spans", highlighted.spans().len());
        }
        None => println!("this build bundles no Rust grammar, so it highlights nothing"),
    }
}

/// Resolves one pending key sequence through one shared registry.
fn check_keymap() {
    let leader = Key::plain(KeyCode::Char(' '));
    let bindings = [
        Binding::host(Global, &[leader, Key::plain(KeyCode::Char('q'))], HostCommand::Quit),
        Binding::host(
            Global,
            &[leader, Key::plain(KeyCode::Char('v'))],
            HostCommand::SplitRight,
        ),
    ];
    let registry = Registry::from_bindings(&bindings, KEYS_MAX).expect("the table validates");
    let mut resolver = Resolver::new(Arc::new(registry), KEYS_MAX, WHICH_KEY_DELAY);
    let context = DispatchContext::focused(InputContextSnapshot::idle(Global));
    // This consumer draws no which-key overlay and holds no clock, so it
    // supplies no elapsed time and arms no timer.
    let pending = resolver.dispatch(&context, Input::Key(leader), None);
    assert_eq!(pending, Dispatch::Pending, "the leader opens a sequence");
    println!("the leader key answers {pending:?}");
}

/// Splits one host area between two caller-owned surfaces.
fn check_ui() {
    let mut tree = WindowTree::new(SurfaceId(1), HOST_AREA, WindowLimits::default());
    let right = tree
        .split(Orientation::Vertical, ChildSide::Second)
        .expect("the host area is wide enough for two windows");
    tree.replace_surface(right, SurfaceId(2))
        .expect("the split returned this window");

    let mut cells = Buffer::empty(HOST_AREA);
    let area = tree.layout().area(right).expect("the window is visible");
    cells.set_string(area.x, area.y, "right", ratatui::style::Style::default());
    println!("the right window sits at {area:?}");
}

/// Names the bounded values that one changed-file request carries.
fn check_lsp() {
    let limits = DiagnosticsLimits::default();
    let manager = ManagerLimits::default();
    let revision = DocumentRevision::new(1);
    let wait = WaitPolicy::Immediate;
    println!("diagnostics {limits:?} manager {manager:?} revision {revision:?} wait {wait:?}");
}

/// Names the embedded editor facade without starting a runtime.
///
/// The consumer of this facade supplies its own asynchronous runtime and its
/// own bounded spawner. This check proves that the values compile and that the
/// event vocabulary stays reachable.
fn check_embedded_editor() {
    let access = EditorAccess::ViewOnly;
    let capacity = EditorCapacity::default();
    println!("an embedded editor accepts {access:?} with {capacity:?}");
    println!("a redraw request names {}", event_name(&EditorEvent::RedrawRequested));
}

/// Returns the stable name of one editor event.
fn event_name(event: &EditorEvent) -> &'static str {
    match event {
        EditorEvent::ActiveFileChanged { .. } => "active-file-changed",
        EditorEvent::FileWritten { .. } => "file-written",
        EditorEvent::WorkspaceChanged { .. } => "workspace-changed",
        EditorEvent::RedrawRequested => "redraw-requested",
        EditorEvent::FocusBoundary(_) => "focus-boundary",
        EditorEvent::CloseRequested => "close-requested",
    }
}
