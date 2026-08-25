//! Render one sidebar of two-line rows with state markers and print the cells.
//!
//! The host owns every row value, every style, and the meaning of every action.
//! The sidebar stores the row identity, the height of the row in terminal rows,
//! the depth of the row inside its tree, its collapsed flag, its section, the
//! selection, and the viewport. One callback draws the cells, so this example
//! needs no editor, no filesystem, and no terminal. It paints into a ratatui
//! test buffer and prints that buffer.
//!
//! The rows below hold two sections over one flat list: a task section above a
//! worktree tree. A collapsed directory hides its children, and a collapsed
//! section hides every row that it holds. Both hidden kinds take no selection
//! and contribute no line.
//!
//! The indent guides come from `sidebar_guides`, which draws one segment for
//! each level from 1 to the depth of the row. A top-level row therefore carries
//! no guide. This host draws no header row above its top level, so it prepends
//! no `SIDEBAR_GUIDE_BLANK` of its own. The kvim file tree does prepend one,
//! because its workspace-root header is no sibling of the rows below it.
//!
//! Run it with:
//!
//! ```sh
//! cargo run -p kvim-ui --example sidebar
//! ```

use std::num::NonZeroU16;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use kvim_ui::{
    RowKind, SIDEBAR_GUIDE_ELBOW, SIDEBAR_GUIDE_TRUNK, SidebarAction, SidebarCanvas, SidebarEvent,
    SidebarInput, SidebarMotion, SidebarPlacement, SidebarRow, SidebarState, sidebar_guides,
};

/// The identity of one host row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EntryId(u32);

/// What the host knows about one entry. The sidebar reads none of it.
struct Entry {
    id: EntryId,
    name: &'static str,
    detail: &'static str,
    state: EntryState,
    /// The depth of the entry below the root of its own tree.
    depth: usize,
    /// Whether the entry hides the deeper entries below it.
    collapsed: bool,
    /// The section that holds the entry.
    section: usize,
}

/// The semantic state of one entry, as the host defines it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryState {
    /// The entry holds no change.
    Clean,
    /// The entry holds a change that the reader made.
    Changed,
    /// The entry reports a failure.
    Failed,
}

impl EntryState {
    /// Returns the marker that the host paints at the right edge.
    const fn marker(self) -> &'static str {
        match self {
            Self::Clean => " ",
            Self::Changed => "●",
            Self::Failed => "▲",
        }
    }

    /// Returns the color that the host gives the marker and the name.
    const fn color(self) -> Color {
        match self {
            Self::Clean => Color::Gray,
            Self::Changed => Color::Yellow,
            Self::Failed => Color::Red,
        }
    }
}

/// The rectangle that the host gives the sidebar.
const SIDEBAR_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 26,
    height: 7,
};

/// The number of terminal rows that one entry occupies.
const ROW_LINES: u16 = 2;

/// The cells that the state marker reserves at the right edge.
const MARKER_CELLS: u16 = 1;

/// The section that holds the tasks of the reader.
const TASKS: usize = 0;

/// The section that holds the worktree tree.
const WORKTREE: usize = 1;

/// The name that this host gives each section, in section order.
const SECTION_NAMES: [&str; 2] = ["Tasks", "Worktree"];

fn main() {
    let entries = [
        Entry {
            id: EntryId(1),
            name: "Rename the constants",
            detail: "in progress",
            state: EntryState::Changed,
            depth: 0,
            collapsed: false,
            section: TASKS,
        },
        Entry {
            id: EntryId(2),
            name: "Triage the crashes",
            detail: "blocked",
            state: EntryState::Failed,
            depth: 0,
            collapsed: false,
            section: TASKS,
        },
        Entry {
            id: EntryId(3),
            name: "src",
            detail: "2 entries",
            state: EntryState::Clean,
            depth: 0,
            collapsed: true,
            section: WORKTREE,
        },
        Entry {
            id: EntryId(4),
            name: "main.rs",
            detail: "12 KB · edited",
            state: EntryState::Changed,
            depth: 1,
            collapsed: false,
            section: WORKTREE,
        },
        Entry {
            id: EntryId(5),
            name: "lib.rs",
            detail: "3 KB",
            state: EntryState::Clean,
            depth: 1,
            collapsed: false,
            section: WORKTREE,
        },
        Entry {
            id: EntryId(6),
            name: "docs",
            detail: "2 entries",
            state: EntryState::Clean,
            depth: 0,
            collapsed: false,
            section: WORKTREE,
        },
        Entry {
            id: EntryId(7),
            name: "plan.md",
            detail: "read failed",
            state: EntryState::Failed,
            depth: 1,
            collapsed: false,
            section: WORKTREE,
        },
        Entry {
            id: EntryId(8),
            name: "windows.md",
            detail: "4 KB",
            state: EntryState::Clean,
            depth: 1,
            collapsed: false,
            section: WORKTREE,
        },
    ];

    // Every entry occupies two terminal rows: the name and one detail line. The
    // depth, the collapsed flag, and the section arrive through the builders, so
    // the host names the tree shape beside the height and the kind.
    let lines = NonZeroU16::new(ROW_LINES).expect("the row height is not zero");
    let mut sidebar = SidebarState::new(SIDEBAR_AREA.height);
    sidebar
        .set_rows(
            entries
                .iter()
                .map(|entry| {
                    SidebarRow::new(entry.id, lines, RowKind::Selectable)
                        .with_depth(entry.depth)
                        .with_collapsed(entry.collapsed)
                        .with_section(entry.section)
                })
                .collect(),
        )
        .expect("eight rows stay inside every bound");

    // The default section list is empty, which collapses no section. This host
    // states both flags, so it can collapse one of them later.
    sidebar
        .set_sections(vec![false, false])
        .expect("two sections stay inside the bound");

    // The two children of the collapsed directory stay in the row list, so the
    // host keeps its own indexes, and they contribute no line to the scroll.
    let hidden_lines = u32::from(ROW_LINES) * 2;
    assert_eq!(sidebar.rows().len(), entries.len());
    assert_eq!(
        sidebar.total_lines(),
        u32::try_from(entries.len()).expect("eight rows fit") * u32::from(ROW_LINES) - hidden_lines,
        "the collapsed directory contributes no line for its two children"
    );

    // A hidden row takes no selection at all, so a host cannot land on a row
    // that the reader cannot see.
    assert!(
        sidebar.select(&EntryId(4)).is_none(),
        "the collapsed directory hides this row"
    );

    // A downward move crosses the collapsed subtree in one step.
    sidebar.select(&EntryId(3));
    let moved = sidebar.reduce(&SidebarInput::Move(SidebarMotion::Down(1)));
    println!("selection: {moved:?}");
    assert_eq!(
        sidebar.selected(),
        Some(&EntryId(6)),
        "the move skips both hidden children"
    );

    // The guides start at depth 1, so a top-level row carries none. A level
    // that holds a further row below draws a trunk, and the last row of a level
    // closes it with an elbow.
    assert_eq!(guides_of(&sidebar, &EntryId(3)), "");
    assert_eq!(guides_of(&sidebar, &EntryId(7)), SIDEBAR_GUIDE_TRUNK);
    assert_eq!(guides_of(&sidebar, &EntryId(8)), SIDEBAR_GUIDE_ELBOW);

    // The viewport shows a part of the list, so the reader travels to the last
    // visible row. The sidebar scrolls the deeper rows into view, and the drawn
    // cells below hold their guides.
    sidebar.reduce(&SidebarInput::Move(SidebarMotion::LastRow));
    assert_eq!(sidebar.selected(), Some(&EntryId(8)));
    println!(
        "viewport:  {} lines of {} terminal rows, first line {}",
        SIDEBAR_AREA.height,
        sidebar.total_lines(),
        sidebar.first_line(),
    );
    for placement in sidebar.placements() {
        println!(
            "  row {} {:?} guides {:?}",
            placement.index(),
            placement.row(),
            sidebar_guides(sidebar.rows(), placement.index()),
        );
    }
    println!("{}", draw(&sidebar, &entries));

    // Collapsing one section hides every row that it holds, exactly as a
    // collapsed directory hides its subtree.
    sidebar
        .set_sections(vec![false, true])
        .expect("two sections stay inside the bound");
    println!("the host collapsed the {} section", SECTION_NAMES[WORKTREE]);
    assert_eq!(
        sidebar.total_lines(),
        u32::from(ROW_LINES) * 2,
        "the two task rows are the only rows that remain"
    );
    assert!(
        sidebar
            .placements()
            .iter()
            .all(|placement| entry_of(&entries, placement.row()).section == TASKS),
        "the collapsed section shows no row"
    );
    assert!(
        sidebar.select(&EntryId(3)).is_none(),
        "the collapsed section hides its own top-level row"
    );
    // The selected row went with its section, so the sidebar moved the
    // selection to the nearest visible row instead of losing it.
    assert_eq!(sidebar.selected(), Some(&EntryId(2)));
    println!("{}", draw(&sidebar, &entries));

    // The host decides what an action means. The sidebar only names it.
    let action = SidebarAction::new("open").expect("the name stays inside the bound");
    match sidebar.reduce(&SidebarInput::Request(action)) {
        Some(SidebarEvent::ActionRequested { row, action }) => {
            println!("the host runs {} on {row:?}", action.name());
        }
        other => unreachable!("a selected row always answers an action: {other:?}"),
    }
}

/// Returns the entry that one row identity names.
fn entry_of<'a>(entries: &'a [Entry], id: &EntryId) -> &'a Entry {
    entries
        .iter()
        .find(|entry| entry.id == *id)
        .expect("the host built the rows from these entries")
}

/// Returns the indent guides of the named row.
fn guides_of(sidebar: &SidebarState<EntryId>, id: &EntryId) -> String {
    let index = sidebar
        .rows()
        .iter()
        .position(|row| row.id() == id)
        .expect("the host built the rows from these entries");
    sidebar_guides(sidebar.rows(), index)
}

/// Renders every visible row into one test buffer and returns its cells.
fn draw(sidebar: &SidebarState<EntryId>, entries: &[Entry]) -> String {
    let mut target = Buffer::empty(SIDEBAR_AREA);
    sidebar
        .render(&mut target, SIDEBAR_AREA, |canvas, placement| {
            let entry = entry_of(entries, placement.row());
            let current = sidebar.selected() == Some(&entry.id);
            let guides = sidebar_guides(sidebar.rows(), placement.index());
            draw_entry(canvas, placement, entry, &guides, current);
        })
        .expect("the callback stays inside every bound");
    printable(&target)
}

/// Draws one entry into the visible part of its row.
///
/// The first line holds the selection mark, the indent guides, the name, and
/// the state marker. The second line holds the detail. The canvas covers the
/// visible part of the row only, so a clipped first or last row draws the lines
/// that it shows.
fn draw_entry(
    canvas: &mut SidebarCanvas<'_>,
    placement: &SidebarPlacement<EntryId>,
    entry: &Entry,
    guides: &str,
    current: bool,
) {
    let base = Style::default().fg(entry.state.color());
    let background = if current {
        base.bg(Color::DarkGray)
    } else {
        base
    };
    canvas.fill(background);

    // The visible part starts at the first line of the row, so a clipped row
    // moves every line up by the lines that the viewport hides. A line that the
    // viewport clips at either end has no place on the canvas.
    let line_of = |line: u16| {
        line.checked_sub(placement.first_line())
            .filter(|line| *line < placement.lines())
    };
    let names = canvas.width_cells().saturating_sub(MARKER_CELLS + 2);
    if let Some(line) = line_of(0) {
        canvas.draw(line, 0, if current { "▌" } else { " " }, background);
        canvas.draw_clipped(
            line,
            2,
            &format!("{guides}{}", entry.name),
            names,
            background,
        );
        canvas.draw(
            line,
            canvas.width_cells() - MARKER_CELLS,
            entry.state.marker(),
            background.add_modifier(Modifier::BOLD),
        );
    }
    if let Some(line) = line_of(1) {
        canvas.draw_clipped(
            line,
            4,
            entry.detail,
            names,
            background.add_modifier(Modifier::DIM),
        );
    }
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
