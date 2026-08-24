//! Render one sidebar of two-line rows with state markers and print the cells.
//!
//! The host owns every row value, every style, and the meaning of every action.
//! The sidebar stores the row identity, the height of the row in terminal rows,
//! the selection, and the viewport. One callback draws the cells, so this
//! example needs no editor, no filesystem, and no terminal. It paints into a
//! ratatui test buffer and prints that buffer.
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
    RowKind, SidebarAction, SidebarCanvas, SidebarEvent, SidebarInput, SidebarMotion,
    SidebarPlacement, SidebarRow, SidebarState,
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

fn main() {
    let entries = [
        Entry {
            id: EntryId(1),
            name: "src/main.rs",
            detail: "12 KB · edited",
            state: EntryState::Changed,
        },
        Entry {
            id: EntryId(2),
            name: "src/lib.rs",
            detail: "3 KB",
            state: EntryState::Clean,
        },
        Entry {
            id: EntryId(3),
            name: "docs/plan.md",
            detail: "read failed",
            state: EntryState::Failed,
        },
        Entry {
            id: EntryId(4),
            name: "README.md",
            detail: "1 KB",
            state: EntryState::Clean,
        },
    ];

    // Every entry occupies two terminal rows: the name and one detail line.
    let lines = NonZeroU16::new(ROW_LINES).expect("the row height is not zero");
    let mut sidebar = SidebarState::new(SIDEBAR_AREA.height);
    sidebar
        .set_rows(
            entries
                .iter()
                .map(|entry| SidebarRow::new(entry.id, lines, RowKind::Selectable))
                .collect(),
        )
        .expect("four rows stay inside every bound");

    // The reduction moves the selection and reports what happened. It runs no
    // host command.
    let moved = sidebar.reduce(&SidebarInput::Move(SidebarMotion::ToRow(2)));
    println!("selection: {moved:?}");
    println!(
        "viewport:  {} lines of {} terminal rows, first line {}",
        SIDEBAR_AREA.height,
        sidebar.total_lines(),
        sidebar.first_line(),
    );

    let mut target = Buffer::empty(SIDEBAR_AREA);
    sidebar
        .render(&mut target, SIDEBAR_AREA, |canvas, placement| {
            let entry = entries
                .iter()
                .find(|entry| entry.id == *placement.row())
                .expect("the host built the rows from these entries");
            let current = sidebar.selected() == Some(&entry.id);
            draw_entry(canvas, placement, entry, current);
        })
        .expect("the callback stays inside every bound");
    println!("{}", printable(&target));

    // The host decides what an action means. The sidebar only names it.
    let action = SidebarAction::new("open").expect("the name stays inside the bound");
    match sidebar.reduce(&SidebarInput::Request(action)) {
        Some(SidebarEvent::ActionRequested { row, action }) => {
            println!("the host runs {} on {row:?}", action.name());
        }
        other => unreachable!("a selected row always answers an action: {other:?}"),
    }
}

/// Draws one entry into the visible part of its row.
///
/// The first line holds the selection mark, the name, and the state marker. The
/// second line holds the detail. The canvas covers the visible part of the row
/// only, so a clipped first or last row draws the lines that it shows.
fn draw_entry(
    canvas: &mut SidebarCanvas<'_>,
    placement: &SidebarPlacement<EntryId>,
    entry: &Entry,
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
        canvas.draw_clipped(line, 2, entry.name, names, background);
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
