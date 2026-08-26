//! Derive which-key hints from one shared registry and render them.
//!
//! The example builds one binding table, feeds the resolver a pending leader
//! key, reads the hints of that pending prefix, and paints them into a ratatui
//! test buffer. The overlay holds no binding table of its own: every row comes
//! from the registry that dispatch reads, so a hint can never disagree with the
//! command that its key reaches.
//!
//! It then paints one row that carries both facts that a row beside a pending
//! prefix can carry: an icon names the scope, and a key style marks the row
//! as one that abandons the pending sequence instead of continuing it.
//!
//! The example then paints an idle list that one frame cannot hold. It steps
//! through the pages of that list, prints the position that each render
//! reports, and checks that the steps reach every key exactly once.
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
    Registry, Resolver, Scope, ScopedWhichKeyHint,
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

/// The number of keys of the idle list that the paging part of the example
/// paints. One measured host list holds this many.
const IDLE_KEYS: usize = 91;

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
        let reached: Vec<&str> = hint
            .hint()
            .commands()
            .iter()
            .map(CommandMetadata::id)
            .collect();
        println!(
            "  {:<4} {:<24} {}",
            hint.hint().key_label().to_string(),
            hint.hint().target().to_string(),
            reached.join(", ")
        );
    }

    // The widget takes final texts, so the host owns every label, icon, and
    // color that it paints.
    println!("{}", printable(&painted(&hints)));

    print_row_with_both_facts(&hints);
    step_through_a_long_list();
}

/// Paints one row that carries both facts beside the ordinary extensions.
///
/// A host that draws `WhichKeyView::hints` beside `WhichKeyView::interruptions`
/// marks two independent facts on a row: the scope that holds the key, through
/// `WhichKeyHint::icon`, and whether the key continues the pending sequence or
/// abandons it, through `WhichKeyHint::key_style`. This registry binds no
/// host-global scope, so the example stands in one interruption of its own: a
/// key that returns focus to an embedding host, styled apart from the
/// extensions above it.
fn print_row_with_both_facts(hints: &[ScopedWhichKeyHint<Command, Global>]) {
    let Some(extension) = hints.first() else {
        return;
    };
    let extension_group = Group::of(extension.hint().commands());
    let extension_key = extension.hint().key_label().to_string();
    let extension_target = extension.hint().target().to_string();
    let extension_row =
        WhichKeyHint::new(&extension_key, &extension_target).with_icon(WhichKeyIcon {
            glyph: extension_group.glyph(),
            style: Style::default().fg(extension_group.color()),
        });

    // The interruption keeps its own icon, for the host-global scope that
    // contributed it, and a key style that marks it apart from a row that
    // continues the pending sequence.
    let interruption_icon = WhichKeyIcon {
        glyph: "!",
        style: Style::default().fg(Color::Magenta),
    };
    let interruption_key_style = Style::default().fg(Color::Red);
    let interruption_row = WhichKeyHint::new("C-e", "Leave to chat")
        .with_icon(interruption_icon)
        .with_key_style(interruption_key_style);

    let rows = [extension_row, interruption_row];
    let styles = WhichKeyStyles {
        surface: Style::default().bg(Color::Black).fg(Color::Gray),
        title: Style::default().fg(Color::Yellow),
        key: Style::default().fg(Color::Yellow),
    };
    let mut target = Buffer::empty(BODY);
    WhichKeyOverlay::new(" Which Key ", &rows, styles)
        .expect("two rows stay inside every bound")
        .render(&mut target, BODY)
        .expect("the band covers the cell buffer of this example");
    println!("an extension beside an interruption, each keeping its own icon:");
    println!("{}", printable(&target));
}

/// Paints one idle list that one frame cannot hold, one page at a time.
///
/// A pending prefix names a handful of next keys, and an idle list names the
/// first key of every scope that answers. A measured host list holds 91 of
/// them, and a terminal of 24 rows shows only a part. The overlay therefore
/// holds one page for each frame of columns, and every render reports the page
/// it drew, so a host binds one key that steps through the list.
fn step_through_a_long_list() {
    let keys: Vec<String> = (0..IDLE_KEYS).map(|index| format!("g{index}")).collect();
    let labels: Vec<String> = (0..IDLE_KEYS)
        .map(|index| format!("Run command {index}"))
        .collect();
    let rows: Vec<WhichKeyHint<'_>> = keys
        .iter()
        .zip(&labels)
        .map(|(key, label)| WhichKeyHint::new(key, label))
        .collect();
    let overlay = WhichKeyOverlay::new(" Which Key ", &rows, WhichKeyStyles::default())
        .expect("the idle list stays inside every bound");

    let mut reached: Vec<usize> = Vec::new();
    let mut page = 0;
    loop {
        let mut target = Buffer::empty(BODY);
        let drawn = overlay
            .at_page(page)
            .render(&mut target, BODY)
            .expect("the band covers the cell buffer of this example");
        let range = drawn.drawn();
        // A host paints this position beside the overlay, in its own style.
        println!(
            "page {} of {}: keys {} to {} of {}",
            drawn.page() + 1,
            drawn.pages(),
            range.start + 1,
            range.end,
            drawn.total()
        );
        println!("{}", printable(&target));
        reached.extend(range);
        if !drawn.has_next_page() {
            break;
        }
        page += 1;
    }

    let mut once = reached.clone();
    once.sort_unstable();
    once.dedup();
    assert_eq!(once.len(), reached.len(), "no key appears on two pages");
    assert_eq!(once.len(), IDLE_KEYS, "the steps reach every key");
    println!("the {IDLE_KEYS} keys arrived over {} pages", page + 1);
}

/// Renders the derived hints into one cell buffer.
fn painted(hints: &[ScopedWhichKeyHint<Command, Global>]) -> Buffer {
    let texts: Vec<(String, String)> = hints
        .iter()
        .map(|hint| {
            (
                hint.hint().key_label().to_string(),
                hint.hint().target().to_string(),
            )
        })
        .collect();
    let rows: Vec<WhichKeyHint<'_>> = texts
        .iter()
        .zip(hints)
        .map(|((key, label), hint)| {
            let group = Group::of(hint.hint().commands());
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
