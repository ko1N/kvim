//! The generic sidebar: bounded rows, selection, scrolling, events, rendering.
//!
//! The module is deterministic and pure. It reads no clock, no filesystem, and
//! no terminal. [`SidebarState`] stores one opaque row identity for each row,
//! the terminal rows that each row occupies, the selection, and the viewport.
//! The host owns every row value, every style, every label, and the meaning of
//! every action.
//!
//! Selection and scrolling both work in terminal rows, so one row that occupies
//! several terminal rows scrolls like the lines that it holds. The first and
//! the last visible row may show a part of a row, and [`SidebarState::render`]
//! hands the host callback exactly the visible part.
//!
//! `examples/sidebar.rs` builds one sidebar of two-line rows with state markers
//! and prints the rendered buffer.

use std::num::NonZeroU16;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use thiserror::Error;

use crate::layout::fits;

/// The largest number of rows that one sidebar holds.
///
/// The bound stops a host that reads a very large directory, a very large
/// result list, or a very long log from building an unbounded row list. It
/// stays above the row count of a host that shows one entry and one notice for
/// every entry of its own bounded model.
pub const SIDEBAR_ROWS_MAX: usize = 32_768;

/// The largest number of terminal rows that one sidebar row occupies.
///
/// The bound keeps one row readable beside its neighbors, and it keeps the
/// product of the row count and the row height inside [`u32`].
pub const SIDEBAR_ROW_LINES_MAX: u16 = 8;

/// The largest depth that one sidebar row holds.
///
/// The bound matches `TREE_DEPTH_MAX` of `kvim-workspace` and
/// [`SPLIT_DEPTH_MAX`](crate::SPLIT_DEPTH_MAX) of this crate, so a host tree
/// that already respects those bounds never exceeds this one. It also keeps
/// the guide string of one row a bounded number of cells.
pub const SIDEBAR_ROW_DEPTH_MAX: usize = 16;

/// The largest number of sections that one sidebar holds.
///
/// A section is a second axis over the same flat row list: a collapsible
/// task list above a worktree tree is one section beside another. The bound
/// stops a host from building an unbounded section list, the way
/// [`SIDEBAR_ROWS_MAX`] stops an unbounded row list, and it stays well above
/// the number of collapsible groups that one sidebar plausibly shows.
pub const SIDEBAR_SECTIONS_MAX: usize = 64;

/// The largest number of characters that one drawn text accepts.
///
/// The bound stops a host callback from handing a whole file to the cell
/// buffer. The sidebar clips the text at its own width, so a longer text also
/// carries no visible information.
pub const SIDEBAR_LABEL_CHARS_MAX: usize = 512;

/// The largest number of characters that one action name accepts.
pub const SIDEBAR_ACTION_CHARS_MAX: usize = 32;

/// The largest number of draw calls that one row callback issues.
///
/// The bound holds the visible output of one row to a fixed cost, so no host
/// callback turns one frame into unbounded work.
pub const SIDEBAR_ROW_DRAWS_MAX: usize = 64;

/// The reason that the sidebar refused one row list, one action, or one draw.
///
/// Every variant names the bound that the value passed, so the host repairs the
/// input instead of reading a clipped or a partial result.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SidebarError {
    /// The row list holds more rows than [`SIDEBAR_ROWS_MAX`].
    #[error("the sidebar holds at most {max} rows, and the host supplied {rows}")]
    Rows {
        /// The number of rows that the host supplied.
        rows: usize,
        /// The bound that the row list passed.
        max: usize,
    },
    /// One row occupies more terminal rows than [`SIDEBAR_ROW_LINES_MAX`].
    #[error("row {index} occupies {height} terminal rows, and the bound is {max}")]
    RowHeight {
        /// The position of the row in the supplied list.
        index: usize,
        /// The height that the host supplied, in terminal rows.
        height: u16,
        /// The bound that the height passed.
        max: u16,
    },
    /// One row named a depth deeper than [`SIDEBAR_ROW_DEPTH_MAX`].
    #[error("row {index} holds depth {depth}, and the bound is {max}")]
    Depth {
        /// The position of the row in the supplied list.
        index: usize,
        /// The depth that the host supplied.
        depth: usize,
        /// The bound that the depth passed.
        max: usize,
    },
    /// The section list holds more sections than [`SIDEBAR_SECTIONS_MAX`].
    #[error("the sidebar holds at most {max} sections, and the host supplied {sections}")]
    Sections {
        /// The number of sections that the host supplied.
        sections: usize,
        /// The bound that the section list passed.
        max: usize,
    },
    /// The draw named a line outside the visible part of the row.
    #[error("the visible row holds {lines} lines, so line {line} is outside it")]
    Line {
        /// The line that the callback named.
        line: u16,
        /// The number of visible lines of the row.
        lines: u16,
    },
    /// The draw named a cell outside the sidebar width.
    #[error("the sidebar is {width} cells wide, so column {column} is outside it")]
    Cell {
        /// The column that the callback named.
        column: u16,
        /// The width of the sidebar, in cells.
        width: u16,
    },
    /// The drawn text holds more characters than [`SIDEBAR_LABEL_CHARS_MAX`].
    #[error("a sidebar text holds at most {max} characters, and the host supplied {chars}")]
    Label {
        /// The number of characters that the host supplied.
        chars: usize,
        /// The bound that the text passed.
        max: usize,
    },
    /// The action name is empty, too long, or holds a control character.
    #[error(
        "an action name holds 1 to {max} characters without a control character, and the host supplied {chars}"
    )]
    Action {
        /// The number of characters that the host supplied.
        chars: usize,
        /// The bound that the name passed.
        max: usize,
    },
    /// One row callback issued more draws than [`SIDEBAR_ROW_DRAWS_MAX`].
    #[error("one sidebar row accepts at most {max} draw calls")]
    VisibleOutput {
        /// The bound that the callback passed.
        max: usize,
    },
    /// The sidebar rectangle names cells that the supplied buffer does not hold.
    #[error("the sidebar rectangle {area:?} names cells outside the buffer {buffer:?}")]
    Area {
        /// The rectangle that the host supplied.
        area: Rect,
        /// The rectangle that the supplied buffer covers.
        buffer: Rect,
    },
}

/// Whether one row accepts the selection.
///
/// A sidebar shows entries beside notices, headers, and separators. An inert
/// row occupies terminal rows and takes no selection, so a move never stops on
/// it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowKind {
    /// The row accepts the selection.
    Selectable,
    /// The row shows information and takes no selection.
    Inert,
}

/// One row of the sidebar: the host identity and the terminal rows it holds.
///
/// The sidebar reads the identity only to compare it, so the host chooses any
/// identity that names one row. Styles, labels, icons, and markers stay with
/// the host and reach the cells through the render callback.
///
/// # Examples
///
/// ```
/// use std::num::NonZeroU16;
///
/// use kvim_ui::{RowKind, SidebarRow};
///
/// let entry = SidebarRow::single(7_u32, RowKind::Selectable);
/// assert_eq!(entry.height_rows(), 1);
///
/// let two_lines = SidebarRow::new(
///     8_u32,
///     NonZeroU16::new(2).expect("the literal 2 is not zero"),
///     RowKind::Selectable,
/// );
/// assert_eq!(two_lines.height_rows(), 2);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SidebarRow<R> {
    id: R,
    height_rows: NonZeroU16,
    kind: RowKind,
    depth: usize,
    collapsed: bool,
    section: usize,
}

impl<R> SidebarRow<R> {
    /// Creates one row of the named height, at depth 0, not collapsed, and in
    /// section 0.
    ///
    /// Use [`SidebarRow::with_depth`], [`SidebarRow::with_collapsed`], and
    /// [`SidebarRow::with_section`] to place the row inside a tree and a
    /// section.
    #[must_use]
    pub const fn new(id: R, height_rows: NonZeroU16, kind: RowKind) -> Self {
        Self {
            id,
            height_rows,
            kind,
            depth: 0,
            collapsed: false,
            section: 0,
        }
    }

    /// Creates one row that occupies one terminal row, at depth 0, not
    /// collapsed, and in section 0.
    ///
    /// Use [`SidebarRow::with_depth`], [`SidebarRow::with_collapsed`], and
    /// [`SidebarRow::with_section`] to place the row inside a tree and a
    /// section.
    #[must_use]
    pub const fn single(id: R, kind: RowKind) -> Self {
        Self {
            id,
            height_rows: NonZeroU16::MIN,
            kind,
            depth: 0,
            collapsed: false,
            section: 0,
        }
    }

    /// Returns the row with the named depth below the root of its tree.
    ///
    /// The root row of a tree holds depth 0. [`SIDEBAR_ROW_DEPTH_MAX`] bounds
    /// the depth that [`SidebarState::set_rows`] accepts.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_ui::{RowKind, SidebarRow};
    ///
    /// let child = SidebarRow::single("src/main.rs", RowKind::Selectable).with_depth(1);
    /// assert_eq!(child.depth(), 1);
    /// ```
    #[must_use]
    pub const fn with_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// Returns the row with the named collapsed state.
    ///
    /// A collapsed row hides every row below it that carries a strictly
    /// greater depth, transitively, from every motion, from the placements,
    /// and from the total line count. See [`SidebarState`].
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_ui::{RowKind, SidebarInput, SidebarMotion, SidebarRow, SidebarState};
    ///
    /// // A collapsed directory hides the file below it.
    /// let mut sidebar = SidebarState::new(3);
    /// sidebar
    ///     .set_rows(vec![
    ///         SidebarRow::single("src", RowKind::Selectable).with_collapsed(true),
    ///         SidebarRow::single("src/main.rs", RowKind::Selectable).with_depth(1),
    ///         SidebarRow::single("tests", RowKind::Selectable),
    ///     ])
    ///     .expect("three rows stay inside every bound");
    ///
    /// // A downward move skips the hidden file and lands on the next visible row.
    /// sidebar.select(&"src");
    /// sidebar.reduce(&SidebarInput::Move(SidebarMotion::Down(1)));
    /// assert_eq!(sidebar.selected(), Some(&"tests"));
    /// // The collapsed subtree contributes no line to the scroll.
    /// assert_eq!(sidebar.total_lines(), 2);
    /// ```
    #[must_use]
    pub const fn with_collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    /// Returns the row in the named section.
    ///
    /// A section is a second axis over the same flat row list, not a nested
    /// container: it groups rows by section index instead of by tree depth.
    /// The row list stays ordered by section, so every row of section 0
    /// precedes every row of section 1, and the depth of a row still counts
    /// from the root of its own tree, inside its own section. Use
    /// [`SidebarState::set_sections`] to collapse a whole section at once.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_ui::{RowKind, SidebarRow};
    ///
    /// let task = SidebarRow::single("task one", RowKind::Selectable).with_section(0);
    /// assert_eq!(task.section(), 0);
    /// ```
    #[must_use]
    pub const fn with_section(mut self, section: usize) -> Self {
        self.section = section;
        self
    }

    /// Returns the host identity of the row.
    #[must_use]
    pub const fn id(&self) -> &R {
        &self.id
    }

    /// Returns the height of the row, in terminal rows.
    #[must_use]
    pub const fn height_rows(&self) -> u16 {
        self.height_rows.get()
    }

    /// Reports whether the row accepts the selection.
    #[must_use]
    pub const fn kind(&self) -> RowKind {
        self.kind
    }

    /// Returns the depth of the row below the root of its tree.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Reports whether the row hides the rows below it that carry a strictly
    /// greater depth.
    #[must_use]
    pub const fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    /// Returns the section index of the row.
    #[must_use]
    pub const fn section(&self) -> usize {
        self.section
    }
}

/// Returns, for each row of `rows`, whether a collapsed ancestor or a
/// collapsed section hides it.
///
/// A row is hidden when a preceding row of strictly smaller depth carries
/// `collapsed == true` and no shallower row closes that ancestor first, or
/// when `sections` marks the row's own section index collapsed. This is the
/// one function that decides row visibility, so the depth rule and the
/// section rule never drift apart. The scan holds one stack of the depths of
/// the ancestors that are currently open and collapsed, so it costs one pass
/// over `rows`. A row that is itself collapsed stays visible; only the rows
/// below it, of strictly greater depth, are hidden. A section index past the
/// end of `sections` counts as not collapsed, so a row that carries no
/// section stays visible under the default, empty section list.
pub(crate) fn sidebar_visibility<R>(rows: &[SidebarRow<R>], sections: &[bool]) -> Vec<bool> {
    let mut visible = Vec::with_capacity(rows.len());
    let mut collapsed_ancestors: Vec<usize> = Vec::new();
    let mut section = None;
    for row in rows {
        // A section holds its own tree, so no row of one section is the
        // ancestor of a row of the next one. The stack therefore empties at
        // every section boundary, and a host that starts a section below
        // depth 0 still hides no row of it behind the previous section.
        if section != Some(row.section) {
            section = Some(row.section);
            collapsed_ancestors.clear();
        }
        // A row at or above the depth of the innermost open ancestor has left
        // its subtree, so that ancestor, and every one it closes with it, no
        // longer applies.
        while collapsed_ancestors
            .last()
            .is_some_and(|&depth| row.depth <= depth)
        {
            collapsed_ancestors.pop();
        }
        let section_collapsed = sections.get(row.section).copied().unwrap_or(false);
        visible.push(collapsed_ancestors.is_empty() && !section_collapsed);
        if row.collapsed {
            collapsed_ancestors.push(row.depth);
        }
    }
    visible
}

/// The visible part of one row, in the coordinates of the sidebar rectangle.
///
/// The first and the last placement of one viewport may show a part of a row.
/// [`SidebarPlacement::first_line`] names the first visible line of the row,
/// and [`SidebarPlacement::lines`] names how many of them the viewport shows.
///
/// # Examples
///
/// ```
/// use ratatui::layout::Rect;
///
/// use kvim_ui::{RowKind, SidebarRow, SidebarState};
///
/// let mut sidebar = SidebarState::new(3);
/// sidebar
///     .set_rows(vec![SidebarRow::single(1_u32, RowKind::Selectable)])
///     .expect("one row stays inside every bound");
///
/// let placement = &sidebar.placements()[0];
/// assert_eq!(placement.area(Rect::new(4, 2, 20, 3)), Rect::new(4, 2, 20, 1));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SidebarPlacement<R> {
    row: R,
    index: usize,
    first_line: u16,
    lines: NonZeroU16,
    top_row: u16,
}

impl<R> SidebarPlacement<R> {
    /// Returns the host identity of the row.
    #[must_use]
    pub const fn row(&self) -> &R {
        &self.row
    }

    /// Returns the position of the row in the row list.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Returns the first visible line of the row.
    ///
    /// The value is above zero only when the viewport clips the top of the
    /// first visible row.
    #[must_use]
    pub const fn first_line(&self) -> u16 {
        self.first_line
    }

    /// Returns the number of visible lines of the row.
    #[must_use]
    pub const fn lines(&self) -> u16 {
        self.lines.get()
    }

    /// Returns the offset of the row from the top of the sidebar, in rows.
    #[must_use]
    pub const fn top_row(&self) -> u16 {
        self.top_row
    }

    /// Returns the rectangle of the visible part inside one sidebar rectangle.
    ///
    /// The rectangle never reaches outside the sidebar, so a placement of a
    /// larger viewport still writes inside a smaller rectangle.
    #[must_use]
    pub fn area(&self, sidebar: Rect) -> Rect {
        if self.top_row >= sidebar.height {
            return Rect::new(sidebar.x, sidebar.bottom(), sidebar.width, 0);
        }
        let height = self.lines.get().min(sidebar.height - self.top_row);
        Rect::new(
            sidebar.x,
            sidebar.y.saturating_add(self.top_row),
            sidebar.width,
            height,
        )
    }
}

/// One bounded action name that the host gives its own meaning.
///
/// The sidebar never runs an action. It carries the name from the input to the
/// event, so the host keeps every command, every file operation, and every
/// permission rule.
///
/// # Examples
///
/// ```
/// use kvim_ui::{SIDEBAR_ACTION_CHARS_MAX, SidebarAction, SidebarError};
///
/// let action = SidebarAction::new("rename").expect("the name stays inside the bound");
/// assert_eq!(action.name(), "rename");
/// assert_eq!(
///     SidebarAction::new(""),
///     Err(SidebarError::Action {
///         chars: 0,
///         max: SIDEBAR_ACTION_CHARS_MAX,
///     })
/// );
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SidebarAction(String);

impl SidebarAction {
    /// Creates one action name.
    ///
    /// The name holds 1 to [`SIDEBAR_ACTION_CHARS_MAX`] characters and no
    /// control character, so it stays printable in a hint row and in a log.
    ///
    /// # Errors
    ///
    /// Returns [`SidebarError::Action`] when the name passes the bound, when it
    /// is empty, or when it holds a control character.
    pub fn new(name: &str) -> Result<Self, SidebarError> {
        let chars = name.chars().count();
        let printable = !name.chars().any(char::is_control);
        if chars == 0 || chars > SIDEBAR_ACTION_CHARS_MAX || !printable {
            return Err(SidebarError::Action {
                chars,
                max: SIDEBAR_ACTION_CHARS_MAX,
            });
        }
        Ok(Self(name.to_owned()))
    }

    /// Returns the action name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }
}

/// One bounded move of the sidebar selection, measured in rows.
///
/// The move stops at the first and the last row, so it never wraps. An inert
/// row takes no selection, so the move takes the nearest selectable row in the
/// direction of travel, and the nearest one behind it when that direction holds
/// none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SidebarMotion {
    /// Move down the given number of rows.
    Down(usize),
    /// Move up the given number of rows.
    Up(usize),
    /// Move to the row of the given index.
    ToRow(usize),
    /// Move to the last row.
    LastRow,
}

/// One input that the sidebar reduces into at most one event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SidebarInput {
    /// Move the selection.
    Move(SidebarMotion),
    /// Act on the selected row, as a double click or `Enter` does.
    Activate,
    /// Ask the host for one named action on the selected row.
    Request(SidebarAction),
}

/// One event that the reduction of an input produced.
///
/// The sidebar runs no host command. It reports what the input means, and the
/// host decides what happens next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SidebarEvent<R> {
    /// The selection moved to another row.
    SelectionChanged {
        /// The identity of the newly selected row.
        row: R,
    },
    /// The host activated the selected row.
    Activated {
        /// The identity of the selected row.
        row: R,
    },
    /// The host asked for one named action on the selected row.
    ActionRequested {
        /// The identity of the selected row.
        row: R,
        /// The name that the host gave the action.
        action: SidebarAction,
    },
}

/// The rows, the selection, and the viewport of one sidebar.
///
/// The state owns identities and the viewport. Rows, heights, styles, labels,
/// and the meaning of every action stay with the host.
///
/// Every operation that changes the rows, the selection, or the viewport
/// recomputes the placements, so [`SidebarState::placements`] always describes
/// the current state.
///
/// `examples/sidebar.rs` renders two-line rows with state markers.
///
/// # Examples
///
/// ```
/// use kvim_ui::{RowKind, SidebarEvent, SidebarInput, SidebarMotion, SidebarRow, SidebarState};
///
/// // The host names its own rows. The sidebar copies the identity only.
/// let mut sidebar = SidebarState::new(2);
/// sidebar
///     .set_rows(vec![
///         SidebarRow::single("src", RowKind::Selectable),
///         SidebarRow::single("read only", RowKind::Inert),
///         SidebarRow::single("tests", RowKind::Selectable),
///     ])
///     .expect("three rows stay inside every bound");
///
/// // The first move selects a row and reports the change.
/// assert_eq!(
///     sidebar.reduce(&SidebarInput::Move(SidebarMotion::ToRow(0))),
///     Some(SidebarEvent::SelectionChanged { row: "src" }),
/// );
/// // The move skips the inert row and scrolls it into the viewport.
/// assert_eq!(
///     sidebar.reduce(&SidebarInput::Move(SidebarMotion::Down(1))),
///     Some(SidebarEvent::SelectionChanged { row: "tests" }),
/// );
/// assert_eq!(sidebar.first_line(), 1);
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SidebarState<R> {
    rows: Vec<SidebarRow<R>>,
    /// The collapsed flag of each section, in section order. A row whose
    /// section index falls outside this list counts as not collapsed, so a
    /// sidebar that never calls [`Self::set_sections`] hides no row through
    /// this axis. [`Self::set_sections`] replaces the whole list.
    sections: Vec<bool>,
    /// Whether each row of `rows` is visible, at the same position. A
    /// collapsed ancestor or a collapsed section hides a row from every
    /// motion, from the placements, and from the total line count.
    /// [`Self::set_rows`] and [`Self::set_sections`] both recompute this and
    /// every later read uses the stored result.
    visible: Vec<bool>,
    total_lines: u32,
    selected: Option<usize>,
    first_line: u32,
    height_rows: u16,
    margin_rows: u16,
    placements: Vec<SidebarPlacement<R>>,
}

impl<R> Default for SidebarState<R> {
    /// Returns one sidebar without a row and without a viewport.
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            sections: Vec::new(),
            visible: Vec::new(),
            total_lines: 0,
            selected: None,
            first_line: 0,
            height_rows: 0,
            margin_rows: 0,
            placements: Vec::new(),
        }
    }
}

impl<R: Clone + Eq> SidebarState<R> {
    /// Creates one empty sidebar of the named viewport height.
    #[must_use]
    pub fn new(height_rows: u16) -> Self {
        Self {
            height_rows,
            ..Self::default()
        }
    }

    /// Returns the current rows.
    ///
    /// The list stays the complete flat list that the host supplied, hidden
    /// rows included, because the host indexes into it and needs those
    /// indexes to stay stable. Use [`SidebarState::placements`] for the
    /// visible rows alone.
    #[must_use]
    pub fn rows(&self) -> &[SidebarRow<R>] {
        &self.rows
    }

    /// Returns the collapsed flag of each section, in section order.
    ///
    /// A row whose section index falls outside this list counts as not
    /// collapsed. See [`Self::set_sections`].
    #[must_use]
    pub fn sections(&self) -> &[bool] {
        &self.sections
    }

    /// Returns the number of terminal rows that every row occupies together.
    #[must_use]
    pub const fn total_lines(&self) -> u32 {
        self.total_lines
    }

    /// Returns the identity of the selected row.
    #[must_use]
    pub fn selected(&self) -> Option<&R> {
        self.selected.map(|index| &self.rows[index].id)
    }

    /// Returns the position of the selected row in the row list.
    #[must_use]
    pub const fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Returns the first visible terminal row of the row list.
    #[must_use]
    pub const fn first_line(&self) -> u32 {
        self.first_line
    }

    /// Returns the viewport height, in terminal rows.
    #[must_use]
    pub const fn height_rows(&self) -> u16 {
        self.height_rows
    }

    /// Returns the visible part of every visible row, in layout order.
    ///
    /// The placements cover the viewport from its first row without a gap while
    /// the rows fill it. The first and the last placement may show a part of a
    /// row.
    #[must_use]
    pub fn placements(&self) -> &[SidebarPlacement<R>] {
        &self.placements
    }

    /// Replaces every row and keeps the selected identity while it remains.
    ///
    /// A selected identity that stays selectable keeps the selection at its new
    /// position. A removed identity moves the selection to the nearest
    /// selectable row at or after its old position, and to the nearest one
    /// before it while the rows behind hold none. A sidebar without a selection
    /// stays without one.
    ///
    /// The call validates the complete list before it replaces anything, so a
    /// refused list leaves the previous rows and the previous selection in
    /// place.
    ///
    /// # Errors
    ///
    /// Returns [`SidebarError::Rows`] when the list passes
    /// [`SIDEBAR_ROWS_MAX`], [`SidebarError::RowHeight`] when one row passes
    /// [`SIDEBAR_ROW_LINES_MAX`], and [`SidebarError::Depth`] when one row
    /// passes [`SIDEBAR_ROW_DEPTH_MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_ui::{RowKind, SidebarRow, SidebarState};
    ///
    /// let mut sidebar = SidebarState::new(4);
    /// sidebar
    ///     .set_rows(vec![
    ///         SidebarRow::single("a", RowKind::Selectable),
    ///         SidebarRow::single("b", RowKind::Selectable),
    ///     ])
    ///     .expect("two rows stay inside every bound");
    /// sidebar.select(&"b");
    ///
    /// // The reader keeps the entry that they selected, at its new position.
    /// sidebar
    ///     .set_rows(vec![
    ///         SidebarRow::single("b", RowKind::Selectable),
    ///         SidebarRow::single("c", RowKind::Selectable),
    ///     ])
    ///     .expect("two rows stay inside every bound");
    /// assert_eq!(sidebar.selected(), Some(&"b"));
    /// ```
    pub fn set_rows(&mut self, rows: Vec<SidebarRow<R>>) -> Result<(), SidebarError> {
        if rows.len() > SIDEBAR_ROWS_MAX {
            return Err(SidebarError::Rows {
                rows: rows.len(),
                max: SIDEBAR_ROWS_MAX,
            });
        }
        for (index, row) in rows.iter().enumerate() {
            if row.height_rows() > SIDEBAR_ROW_LINES_MAX {
                return Err(SidebarError::RowHeight {
                    index,
                    height: row.height_rows(),
                    max: SIDEBAR_ROW_LINES_MAX,
                });
            }
            if row.depth > SIDEBAR_ROW_DEPTH_MAX {
                return Err(SidebarError::Depth {
                    index,
                    depth: row.depth,
                    max: SIDEBAR_ROW_DEPTH_MAX,
                });
            }
        }
        let previous = self
            .selected
            .map(|index| (index, self.rows[index].id.clone()));
        // The list passed every bound, so the replacement commits in one step.
        self.rows = rows;
        self.recompute_visibility(previous);
        Ok(())
    }

    /// Replaces the collapsed flag of every section.
    ///
    /// `sections[i]` is the collapsed flag of section `i`. A collapsed
    /// section hides every row of that section from every motion, from the
    /// placements, and from the total line count, exactly as a collapsed
    /// tree row hides its subtree. A section index that no row carries still
    /// counts toward the bound, because the host may add a row of that
    /// section later.
    ///
    /// The call validates the list before it replaces anything, so a refused
    /// list leaves the previous sections and the previous selection in
    /// place.
    ///
    /// # Errors
    ///
    /// Returns [`SidebarError::Sections`] when the list passes
    /// [`SIDEBAR_SECTIONS_MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_ui::{RowKind, SidebarEvent, SidebarInput, SidebarMotion, SidebarRow, SidebarState};
    ///
    /// // A task section sits above a worktree section.
    /// let mut sidebar = SidebarState::new(4);
    /// sidebar
    ///     .set_rows(vec![
    ///         SidebarRow::single("task one", RowKind::Selectable).with_section(0),
    ///         SidebarRow::single("task two", RowKind::Selectable).with_section(0),
    ///         SidebarRow::single("src", RowKind::Selectable).with_section(1),
    ///     ])
    ///     .expect("three rows stay inside every bound");
    ///
    /// // Collapsing the task section hides every row it holds and contributes
    /// // no line to the scroll.
    /// sidebar
    ///     .set_sections(vec![true, false])
    ///     .expect("two sections stay inside the bound");
    /// assert_eq!(sidebar.total_lines(), 1);
    ///
    /// // A downward move from no selection skips both hidden tasks in one
    /// // step and lands on the worktree row.
    /// assert_eq!(
    ///     sidebar.reduce(&SidebarInput::Move(SidebarMotion::Down(1))),
    ///     Some(SidebarEvent::SelectionChanged { row: "src" }),
    /// );
    /// ```
    pub fn set_sections(&mut self, sections: Vec<bool>) -> Result<(), SidebarError> {
        if sections.len() > SIDEBAR_SECTIONS_MAX {
            return Err(SidebarError::Sections {
                sections: sections.len(),
                max: SIDEBAR_SECTIONS_MAX,
            });
        }
        let previous = self
            .selected
            .map(|index| (index, self.rows[index].id.clone()));
        // The list passed the bound, so the replacement commits in one step.
        self.sections = sections;
        self.recompute_visibility(previous);
        Ok(())
    }

    /// Recomputes visibility from the current rows and sections, then
    /// restores the selection and reconciles the viewport.
    ///
    /// [`Self::set_rows`] and [`Self::set_sections`] both change one input of
    /// [`sidebar_visibility`] and share this recovery, so the rule that turns
    /// a lost selection into the nearest visible row lives once.
    fn recompute_visibility(&mut self, previous: Option<(usize, R)>) {
        let visible = sidebar_visibility(&self.rows, &self.sections);
        self.total_lines = self
            .rows
            .iter()
            .zip(&visible)
            .filter(|&(_, &visible)| visible)
            .map(|(row, _)| u32::from(row.height_rows()))
            .sum::<u32>();
        self.visible = visible;
        self.selected = previous.and_then(|(index, id)| {
            self.index_of(&id)
                .or_else(|| self.nearest_selectable(index, Travel::Forward))
        });
        self.reconcile();
    }

    /// Sets the viewport height, in terminal rows.
    ///
    /// The sidebar scrolls the selected row back into the viewport, so a resize
    /// never hides it.
    pub fn set_height_rows(&mut self, height_rows: u16) {
        self.height_rows = height_rows;
        self.reconcile();
    }

    /// Sets the number of rows that the selection keeps above and below itself.
    ///
    /// The margin stops at half the viewport, so a small viewport still shows
    /// the selected row.
    pub fn set_scroll_margin(&mut self, margin_rows: u16) {
        self.margin_rows = margin_rows;
        self.reconcile();
    }

    /// Selects the named row and returns the event of a changed selection.
    ///
    /// An unknown identity and an inert row both leave the selection where it
    /// was.
    pub fn select(&mut self, id: &R) -> Option<SidebarEvent<R>> {
        let index = self.index_of(id)?;
        self.commit_selection(Some(index))
    }

    /// Removes the selection.
    ///
    /// The sidebar keeps its scroll offset, so the visible rows stay where the
    /// reader last saw them.
    pub fn clear_selection(&mut self) {
        self.selected = None;
        self.reconcile();
    }

    /// Reduces one input into at most one event.
    ///
    /// The reduction changes the selection and the viewport only. It runs no
    /// host command, so the host decides what an activation and an action mean.
    /// An input that reaches no selectable row produces no event.
    pub fn reduce(&mut self, input: &SidebarInput) -> Option<SidebarEvent<R>> {
        match input {
            SidebarInput::Move(motion) => self.move_selection(*motion),
            SidebarInput::Activate => self
                .selected()
                .cloned()
                .map(|row| SidebarEvent::Activated { row }),
            SidebarInput::Request(action) => {
                self.selected()
                    .cloned()
                    .map(|row| SidebarEvent::ActionRequested {
                        row,
                        action: action.clone(),
                    })
            }
        }
    }

    /// Renders every visible row through one host callback.
    ///
    /// The callback receives one [`SidebarCanvas`] for each visible row. The
    /// canvas covers the visible part of that row only, and it clips every
    /// draw, so the render writes no cell outside `area`. The render performs
    /// no input and no output beyond the cell buffer.
    ///
    /// # Errors
    ///
    /// Returns [`SidebarError::Area`] when `area` names one cell that `target`
    /// does not hold. The buffer keeps every cell in that case, so a host that
    /// supplies a stale rectangle reads no partial sidebar.
    ///
    /// Returns otherwise the first bound that a callback passed. Every other
    /// draw of the same frame still reaches the buffer, so one refused draw
    /// hides no other row.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::buffer::Buffer;
    /// use ratatui::layout::Rect;
    /// use ratatui::style::Style;
    ///
    /// use kvim_ui::{RowKind, SidebarRow, SidebarState};
    ///
    /// let area = Rect::new(0, 0, 6, 2);
    /// let mut target = Buffer::empty(area);
    /// let mut sidebar = SidebarState::new(area.height);
    /// sidebar
    ///     .set_rows(vec![
    ///         SidebarRow::single("one", RowKind::Selectable),
    ///         SidebarRow::single("two", RowKind::Selectable),
    ///     ])
    ///     .expect("two rows stay inside every bound");
    ///
    /// sidebar
    ///     .render(&mut target, area, |canvas, placement| {
    ///         canvas.draw(0, 0, placement.row(), Style::default());
    ///     })
    ///     .expect("the callback stays inside every bound");
    /// assert_eq!(target.cell((0, 1)).map(|cell| cell.symbol()), Some("t"));
    /// ```
    pub fn render<F>(
        &self,
        target: &mut Buffer,
        area: Rect,
        mut draw: F,
    ) -> Result<(), SidebarError>
    where
        F: FnMut(&mut SidebarCanvas<'_>, &SidebarPlacement<R>),
    {
        let buffer = *target.area();
        if !fits(area, buffer) {
            return Err(SidebarError::Area { area, buffer });
        }
        let mut failure = None;
        if area.is_empty() {
            return Ok(());
        }
        for placement in &self.placements {
            let row_area = placement.area(area);
            if row_area.is_empty() {
                continue;
            }
            let mut canvas = SidebarCanvas {
                target,
                area: row_area,
                draws: 0,
                failure: None,
            };
            draw(&mut canvas, placement);
            failure = failure.or(canvas.failure);
        }
        failure.map_or(Ok(()), Err)
    }

    /// Moves the selection by one bounded row move.
    ///
    /// [`SidebarMotion::Down`] and [`SidebarMotion::Up`] count visible rows
    /// only, so a collapsed subtree is absent from the count and a move over
    /// one lands on the next visible row at or above the depth of the
    /// collapsed row. [`SidebarMotion::ToRow`] and [`SidebarMotion::LastRow`]
    /// address visible rows only, through [`Self::nearest_selectable`].
    fn move_selection(&mut self, motion: SidebarMotion) -> Option<SidebarEvent<R>> {
        let last = self.rows.len().checked_sub(1)?;
        let current = self.selected.unwrap_or(0);
        let (target, travel) = match motion {
            SidebarMotion::Down(step) => (
                self.step_visible(current, step, Travel::Forward),
                Travel::Forward,
            ),
            SidebarMotion::Up(step) => (
                self.step_visible(current, step, Travel::Backward),
                Travel::Backward,
            ),
            SidebarMotion::ToRow(row) => (row.min(last), Travel::Forward),
            SidebarMotion::LastRow => (last, Travel::Backward),
        };
        // Every row may report information instead of an entry, so the move
        // finds no row at all and the selection stays where it was.
        let found = self.nearest_selectable(target, travel)?;
        self.commit_selection(Some(found))
    }

    /// Returns the row position `step` visible rows away from `from`.
    ///
    /// A hidden row is absent from the count, so the walk never stops on one
    /// and never counts it toward `step`. The walk stops at the first or the
    /// last row instead of wrapping, so a `step` larger than the number of
    /// visible rows ahead lands on the row nearest that end.
    fn step_visible(&self, from: usize, step: usize, travel: Travel) -> usize {
        let last = self.rows.len().saturating_sub(1);
        let mut position = from.min(last);
        for _ in 0..step {
            let next = match travel {
                Travel::Forward => (position.saturating_add(1)..=last).find(|&i| self.visible[i]),
                Travel::Backward => (0..position).rev().find(|&i| self.visible[i]),
            };
            let Some(next) = next else {
                break;
            };
            position = next;
        }
        position
    }

    /// Selects one row and reports the change.
    fn commit_selection(&mut self, index: Option<usize>) -> Option<SidebarEvent<R>> {
        if index == self.selected {
            self.reconcile();
            return None;
        }
        self.selected = index;
        self.reconcile();
        self.selected()
            .cloned()
            .map(|row| SidebarEvent::SelectionChanged { row })
    }

    /// Returns the position of one visible, selectable row.
    fn index_of(&self, id: &R) -> Option<usize> {
        self.rows.iter().enumerate().position(|(index, row)| {
            row.kind == RowKind::Selectable && self.visible[index] && row.id == *id
        })
    }

    /// Returns the nearest visible, selectable row from one position.
    ///
    /// The search runs in the direction of travel first, and then behind it,
    /// so a block of hidden rows or inert rows never stops a move. Both
    /// conditions must hold for a row to match: it is visible, and its kind
    /// is [`RowKind::Selectable`].
    fn nearest_selectable(&self, from: usize, travel: Travel) -> Option<usize> {
        let last = self.rows.len().checked_sub(1)?;
        let from = from.min(last);
        let is_selectable =
            |index: usize| self.rows[index].kind == RowKind::Selectable && self.visible[index];
        let ahead = (from..=last).find(|&index| is_selectable(index));
        let behind = (0..=from).rev().find(|&index| is_selectable(index));
        match travel {
            Travel::Forward => ahead.or(behind),
            Travel::Backward => behind.or(ahead),
        }
    }

    /// Returns the first terminal row of one row of the list.
    ///
    /// A hidden row contributes no line, so it is absent from the sum.
    fn line_of(&self, index: usize) -> u32 {
        self.rows[..index]
            .iter()
            .zip(&self.visible)
            .filter(|&(_, &visible)| visible)
            .map(|(row, _)| u32::from(row.height_rows()))
            .sum()
    }

    /// Moves the viewport until it shows the selection, then places the rows.
    ///
    /// The margin stops at half the viewport and at the last terminal row, so
    /// the sidebar never scrolls past its rows to satisfy a margin that no row
    /// can fill. A row that is taller than the viewport shows its first line.
    fn reconcile(&mut self) {
        debug_assert_eq!(
            self.rows.len(),
            self.visible.len(),
            "set_rows always stores one visibility flag for every row"
        );
        self.placements.clear();
        if self.height_rows == 0 || self.rows.is_empty() {
            self.first_line = 0;
            return;
        }
        let height = u32::from(self.height_rows);
        let last_start = self.total_lines.saturating_sub(height);
        self.first_line = match self.selected {
            None => self.first_line.min(last_start),
            Some(index) => {
                let start = self.line_of(index);
                let end = start + u32::from(self.rows[index].height_rows()) - 1;
                let margin = u32::from(self.margin_rows).min((height - 1) / 2);
                let low = start.saturating_sub(margin);
                let high = (end + margin).min(self.total_lines - 1);
                self.first_line
                    .min(low)
                    .max((high + 1).saturating_sub(height))
                    .min(last_start)
                    .min(start)
            }
        };
        self.place_rows();
        debug_assert!(
            self.selected.is_none_or(|index| {
                self.placements
                    .iter()
                    .any(|placement| placement.index == index)
            }),
            "the reconciled offset always shows the selected row"
        );
    }

    /// Places every visible row that the viewport shows and clips the two
    /// ends.
    ///
    /// A hidden row contributes no line, so it never reaches the loop body
    /// that turns a line range into one placement.
    fn place_rows(&mut self) {
        let height = u32::from(self.height_rows);
        let mut line = 0_u32;
        for (index, row) in self.rows.iter().enumerate() {
            if !self.visible[index] {
                continue;
            }
            let end = line + u32::from(row.height_rows());
            if end <= self.first_line {
                line = end;
                continue;
            }
            if line >= self.first_line + height {
                break;
            }
            let first_line = self.first_line.saturating_sub(line);
            let top_row = line.saturating_sub(self.first_line);
            let visible_lines = (u32::from(row.height_rows()) - first_line).min(height - top_row);
            let (Ok(first_line), Ok(top_row), Some(lines)) = (
                u16::try_from(first_line),
                u16::try_from(top_row),
                u16::try_from(visible_lines).ok().and_then(NonZeroU16::new),
            ) else {
                debug_assert!(false, "one visible part stays inside the viewport height");
                return;
            };
            self.placements.push(SidebarPlacement {
                row: row.id.clone(),
                index,
                first_line,
                lines,
                top_row,
            });
            line = end;
        }
    }
}

/// The direction that one selection search takes first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Travel {
    /// Search toward the last row first.
    Forward,
    /// Search toward the first row first.
    Backward,
}

/// The visible part of one sidebar row, as one host callback draws it.
///
/// The canvas covers the visible part of one row only. Every draw names a line
/// of that part and a column of the sidebar, and the canvas clips the result at
/// the right edge, so the callback writes no cell outside the row.
///
/// The canvas records the first bound that a draw passed and
/// [`SidebarState::render`] returns it. A refused draw writes nothing.
pub struct SidebarCanvas<'a> {
    target: &'a mut Buffer,
    area: Rect,
    draws: usize,
    failure: Option<SidebarError>,
}

impl SidebarCanvas<'_> {
    /// Returns the rectangle of the visible part of the row.
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
    }

    /// Returns the width of the sidebar, in cells.
    #[must_use]
    pub const fn width_cells(&self) -> u16 {
        self.area.width
    }

    /// Returns the number of visible lines of the row.
    #[must_use]
    pub const fn lines(&self) -> u16 {
        self.area.height
    }

    /// Paints the complete visible part of the row in one style.
    pub fn fill(&mut self, style: Style) {
        if !self.charge() {
            return;
        }
        self.target.set_style(self.area, style);
    }

    /// Paints one span of one line in one style.
    ///
    /// The span stops at the right edge of the sidebar.
    pub fn style_span(&mut self, line: u16, column: u16, cells: u16, style: Style) {
        let Some((x, y)) = self.origin(line, column) else {
            return;
        };
        if !self.charge() {
            return;
        }
        let width = cells.min(self.area.width - column);
        self.target.set_style(Rect::new(x, y, width, 1), style);
    }

    /// Draws one text into one line and returns the cells that it wrote.
    ///
    /// The text stops at `cells` and at the right edge of the sidebar.
    pub fn draw_clipped(&mut self, line: u16, column: u16, text: &str, cells: u16, style: Style) {
        let Some((x, y)) = self.origin(line, column) else {
            return;
        };
        let chars = text.chars().count();
        if chars > SIDEBAR_LABEL_CHARS_MAX {
            self.fail(SidebarError::Label {
                chars,
                max: SIDEBAR_LABEL_CHARS_MAX,
            });
            return;
        }
        if !self.charge() {
            return;
        }
        let width = usize::from(cells.min(self.area.width - column));
        self.target.set_stringn(x, y, text, width, style);
    }

    /// Draws one text into one line up to the right edge of the sidebar.
    pub fn draw(&mut self, line: u16, column: u16, text: &str, style: Style) {
        self.draw_clipped(line, column, text, u16::MAX, style);
    }

    /// Returns the cell of one line and one column of the visible part.
    fn origin(&mut self, line: u16, column: u16) -> Option<(u16, u16)> {
        if line >= self.area.height {
            self.fail(SidebarError::Line {
                line,
                lines: self.area.height,
            });
            return None;
        }
        if column >= self.area.width {
            self.fail(SidebarError::Cell {
                column,
                width: self.area.width,
            });
            return None;
        }
        Some((self.area.x + column, self.area.y + line))
    }

    /// Counts one draw against the visible-output bound.
    fn charge(&mut self) -> bool {
        if self.draws >= SIDEBAR_ROW_DRAWS_MAX {
            self.fail(SidebarError::VisibleOutput {
                max: SIDEBAR_ROW_DRAWS_MAX,
            });
            return false;
        }
        self.draws += 1;
        true
    }

    /// Records the first bound that one draw passed.
    fn fail(&mut self, error: SidebarError) {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
    }
}

#[cfg(test)]
#[path = "sidebar_tests.rs"]
mod tests;
