//! Derive which-key hints from one shared registry and render them.
//!
//! The example builds one binding table, feeds the resolver a pending leader
//! key, reads the hints of that pending prefix, and paints them into a ratatui
//! test buffer. The overlay holds no binding table of its own: every row comes
//! from the registry that dispatch reads, so a hint can never disagree with the
//! command that its key reaches.
//!
//! The example needs no editor, no filesystem, and no terminal.
//!
//! Run it with:
//!
//! ```sh
//! cargo run -p kvim-ui --example which_key
//! ```

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use kvim_keymap::{
    Binding, CommandMetadata, Dispatch, DispatchContext, Input, InputContextSnapshot, Key, KeyCode,
    Registry, Resolver, Scope, WhichKeyHint as KeyHint,
};
use kvim_ui::{WhichKeyHint, WhichKeyIcon, WhichKeyOverlay, WhichKeyStyles};

/// The commands that the host binds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Command {
    FindFile,
    FindBuffer,
    FindText,
    SplitRight,
    SplitBelow,
    ToggleComment,
    Quit,
}

impl fmt::Display for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for Command {
    fn id(&self) -> &str {
        match self {
            Self::FindFile => "find-file",
            Self::FindBuffer => "find-buffer",
            Self::FindText => "find-text",
            Self::SplitRight => "split-right",
            Self::SplitBelow => "split-below",
            Self::ToggleComment => "toggle-comment",
            Self::Quit => "quit",
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::FindFile => "Open the file picker",
            Self::FindBuffer => "Open the buffer picker",
            Self::FindText => "Search the worktree",
            Self::SplitRight => "Split the window right",
            Self::SplitBelow => "Split the window below",
            Self::ToggleComment => "Toggle the comment",
            Self::Quit => "Quit",
        }
    }
}

impl Command {
    /// Returns the group that the host paints the icon of.
    const fn group(self) -> Group {
        match self {
            Self::FindFile | Self::FindBuffer | Self::FindText => Group::Search,
            Self::SplitRight | Self::SplitBelow => Group::Window,
            Self::ToggleComment | Self::Quit => Group::Other,
        }
    }
}

/// The command groups that the host gives its own icons.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Group {
    Search,
    Window,
    Other,
}

impl Group {
    /// Returns the icon glyph of the group.
    const fn glyph(self) -> &'static str {
        match self {
            Self::Search => "?",
            Self::Window => "#",
            Self::Other => "*",
        }
    }

    /// Returns the icon color of the group.
    const fn color(self) -> Color {
        match self {
            Self::Search => Color::Green,
            Self::Window => Color::Cyan,
            Self::Other => Color::Magenta,
        }
    }

    /// Returns the one group of every command behind one key.
    ///
    /// A key that reaches commands of several groups carries [`Group::Other`],
    /// so one row always names exactly one group.
    fn of(commands: &[Command]) -> Self {
        let mut groups = commands.iter().map(|command| command.group());
        let first = groups.next().unwrap_or(Self::Other);
        groups.fold(
            first,
            |merged, group| {
                if merged == group { merged } else { Self::Other }
            },
        )
    }
}

/// The one binding table of this example.
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

/// The longest sequence that the table binds.
const KEYS_MAX: u8 = 3;

/// The wait before the overlay first appears.
const WHICH_KEY_DELAY: Duration = Duration::from_millis(500);

/// The body band that the host gives the overlay.
const BODY: Rect = Rect {
    x: 0,
    y: 0,
    width: 46,
    height: 10,
};

fn key(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

fn main() {
    // The registry validates the complete table once, so a duplicate sequence
    // or an ambiguous prefix pair fails here and never at dispatch time.
    let leader = key(' ');
    let bindings = [
        (vec![leader, key('f'), key('f')], Command::FindFile),
        (vec![leader, key('f'), key('b')], Command::FindBuffer),
        (vec![leader, key('f'), key('t')], Command::FindText),
        (vec![leader, key('w'), key('v')], Command::SplitRight),
        (vec![leader, key('w'), key('s')], Command::SplitBelow),
        (vec![leader, key('/')], Command::ToggleComment),
        (vec![leader, key('q')], Command::Quit),
    ];
    let bindings: Vec<Binding<Command, Global>> = bindings
        .iter()
        .map(|(keys, command)| Binding::host(Global, keys, *command))
        .collect();
    let registry = Registry::from_bindings(&bindings, KEYS_MAX).expect("the table validates");

    // One pending key opens the overlay. The resolver reads no clock, so the
    // host supplies the elapsed time of every step.
    let mut resolver = Resolver::new(Arc::new(registry), KEYS_MAX, WHICH_KEY_DELAY);
    let context = DispatchContext::focused(InputContextSnapshot::idle(Global));
    let pending = resolver.dispatch(&context, Input::Key(leader), Some(Duration::ZERO));
    println!("the leader key answers: {pending:?}");
    assert_eq!(pending, Dispatch::Pending, "the leader opens a sequence");
    println!(
        "the overlay appears at {:?}",
        resolver.overlay_deadline().expect("input is pending")
    );

    // The hints come from the same registry and the same pending prefix that
    // dispatch reads.
    let view = resolver
        .which_key(WHICH_KEY_DELAY)
        .expect("the delay passed, so the overlay is visible");
    let hints = view.hints();
    for hint in &hints {
        let reached: Vec<&str> = hint.commands().iter().map(CommandMetadata::id).collect();
        println!(
            "  {:<4} {:<24} {}",
            hint.key_label().to_string(),
            hint.target().to_string(),
            reached.join(", ")
        );
    }

    // The widget takes final texts, so the host owns every label, icon, and
    // color that it paints.
    println!("{}", printable(&painted(&hints)));
}

/// Renders the derived hints into one cell buffer.
fn painted(hints: &[KeyHint<Command>]) -> Buffer {
    let texts: Vec<(String, String)> = hints
        .iter()
        .map(|hint| (hint.key_label().to_string(), hint.target().to_string()))
        .collect();
    let rows: Vec<WhichKeyHint<'_>> = texts
        .iter()
        .zip(hints)
        .map(|((key, label), hint)| {
            let group = Group::of(hint.commands());
            WhichKeyHint::new(key, label).with_icon(WhichKeyIcon {
                glyph: group.glyph(),
                style: Style::default().fg(group.color()),
            })
        })
        .collect();
    let accent = Style::default().fg(Color::Yellow);
    let styles = WhichKeyStyles {
        surface: Style::default().bg(Color::Black).fg(Color::Gray),
        title: accent,
        key: accent,
    };

    let mut target = Buffer::empty(BODY);
    WhichKeyOverlay::new(" Which Key ", &rows, styles)
        .expect("one level of hints stays inside every bound")
        .render(&mut target, BODY)
        .expect("the band covers the cell buffer of this example");
    target
}

/// Returns the printable rows of one cell buffer.
fn printable(target: &Buffer) -> String {
    let mut out = String::new();
    for y in target.area.top()..target.area.bottom() {
        for x in target.area.left()..target.area.right() {
            out.push_str(target.cell((x, y)).map_or(" ", |cell| cell.symbol()));
        }
        out.push('\n');
    }
    out
}
