//! The list viewport: one window over a bounded list of items.
//!
//! Every bounded list of kvim scrolls by the same rule. The viewport owns a
//! height, a scroll margin, and the first visible line, and it keeps the
//! selected item inside the window without scrolling past the end of the list.
//! It reads the height of each item, so a list of one line for each item is
//! the simple case of the same rule, not a second rule. See
//! `docs/windows.md`.
//!
//! The module is pure and deterministic. It reads no clock, no filesystem, and
//! no terminal. It stores no item, because the caller owns every item value:
//! [`SidebarState`](crate::SidebarState) owns its rows, and a picker owns its
//! candidates. Each call hands the viewport the measure of each item, and the
//! viewport hands back the visible part of each item that the window shows.

use std::num::NonZeroU16;

use ratatui::layout::Rect;

/// The largest number of terminal lines that one list viewport holds.
///
/// The bound keeps the total line count, the first visible line, and every sum
/// of the offset rule inside [`u32`]. Every caller bounds its own list first,
/// and both present bounds stay well below this one:
/// [`SIDEBAR_ROWS_MAX`](crate::SIDEBAR_ROWS_MAX) rows of
/// [`SIDEBAR_ROW_LINES_MAX`](crate::SIDEBAR_ROW_LINES_MAX) lines each reach
/// 262144 lines, and [`SELECTOR_CANDIDATES_MAX`](crate::SELECTOR_CANDIDATES_MAX)
/// rows of one line reach 4096.
pub const LIST_VIEWPORT_LINES_MAX: u32 = 1_048_576;

/// The measure of one list item: the lines it occupies and whether it shows.
///
/// The viewport reads the measure only. The item value, its identity, its
/// style, and its text all stay with the caller.
///
/// A hidden item occupies no line of the window, and no motion and no
/// placement reach it. A collapsed subtree and a collapsed section of a
/// sidebar both hide their items this way, so the position of every item in
/// the list stays the same while a subtree opens and closes.
///
/// # Examples
///
/// ```
/// use std::num::NonZeroU16;
///
/// use kvim_ui::ListItem;
///
/// let row = ListItem::single();
/// assert_eq!(row.lines(), 1);
/// assert!(row.is_visible());
///
/// let two_lines = ListItem::new(NonZeroU16::new(2).expect("the literal 2 is not zero"));
/// assert_eq!(two_lines.lines(), 2);
///
/// let collapsed = ListItem::single().with_visible(false);
/// assert!(!collapsed.is_visible());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListItem {
    lines: NonZeroU16,
    visible: bool,
}

impl ListItem {
    /// Creates one visible item of the named height, in terminal lines.
    #[must_use]
    pub const fn new(lines: NonZeroU16) -> Self {
        Self {
            lines,
            visible: true,
        }
    }

    /// Creates one visible item of one terminal line.
    ///
    /// A list of one line for each item builds every item this way.
    #[must_use]
    pub const fn single() -> Self {
        Self::new(NonZeroU16::MIN)
    }

    /// Returns the item with the named visibility.
    #[must_use]
    pub const fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Returns the height of the item, in terminal lines.
    #[must_use]
    pub const fn lines(&self) -> u16 {
        self.lines.get()
    }

    /// Reports whether the item occupies a line of the list.
    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.visible
    }
}

/// The visible part of one item, in the coordinates of the list rectangle.
///
/// The first and the last placement of one window may show a part of an item.
/// [`ListPlacement::first_line`] names the first visible line of the item, and
/// [`ListPlacement::lines`] names how many of them the window shows.
///
/// # Examples
///
/// ```
/// use ratatui::layout::Rect;
///
/// use kvim_ui::{ListItem, ListViewport};
///
/// let mut viewport = ListViewport::new(3);
/// viewport.reconcile(std::iter::repeat_n(ListItem::single(), 2), Some(1));
///
/// let placement = viewport.placements()[1];
/// assert_eq!(placement.index(), 1);
/// assert_eq!(placement.area(Rect::new(4, 2, 20, 3)), Rect::new(4, 3, 20, 1));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListPlacement {
    index: usize,
    first_line: u16,
    lines: NonZeroU16,
    top_row: u16,
}

impl ListPlacement {
    /// Returns the position of the item in the list.
    ///
    /// The position counts every item that the caller supplied, hidden items
    /// included, so it indexes the caller's own list directly.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Returns the first visible line of the item.
    ///
    /// The value is above zero only when the window clips the top of the first
    /// visible item.
    #[must_use]
    pub const fn first_line(&self) -> u16 {
        self.first_line
    }

    /// Returns the number of visible lines of the item.
    #[must_use]
    pub const fn lines(&self) -> u16 {
        self.lines.get()
    }

    /// Returns the offset of the item from the top of the window, in rows.
    #[must_use]
    pub const fn top_row(&self) -> u16 {
        self.top_row
    }

    /// Returns the rectangle of the visible part inside one list rectangle.
    ///
    /// The rectangle never reaches outside the list, so a placement of a
    /// taller window still writes inside a shorter rectangle.
    #[must_use]
    pub fn area(&self, list: Rect) -> Rect {
        if self.top_row >= list.height {
            return Rect::new(list.x, list.bottom(), list.width, 0);
        }
        let height = self.lines.get().min(list.height - self.top_row);
        Rect::new(
            list.x,
            list.y.saturating_add(self.top_row),
            list.width,
            height,
        )
    }
}

/// One bounded move of a list selection, measured in rows of the receiving
/// list's own row space.
///
/// The move stops at the first and the last row of that space, so it never
/// wraps. [`SidebarState`](crate::SidebarState) and
/// [`Selector`](crate::Selector) both answer every variant, but each names a
/// different row space. `SidebarState` counts every row of its complete flat
/// row list, hidden rows included. `Selector` counts only the rows of
/// [`Selector::matches`](crate::Selector::matches), the rows that the current
/// query keeps. The same index value can therefore name two different rows,
/// or no row at all, across the two lists. See [`ListMotion::ToRow`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListMotion {
    /// Move down the given number of rows.
    Down(usize),
    /// Move up the given number of rows.
    Up(usize),
    /// Move to the row of the given index, in the row space of the list that
    /// receives the motion.
    ///
    /// [`SidebarState`](crate::SidebarState) indexes its complete flat row
    /// list, hidden rows included. A hidden target resolves like an inert
    /// row: to the nearest selectable row in the direction of travel, then to
    /// the nearest one behind it. [`Selector`](crate::Selector) indexes
    /// [`Selector::matches`](crate::Selector::matches) instead, so it never
    /// resolves to a row that the current query does not keep.
    ToRow(usize),
    /// Move to the last row.
    LastRow,
}

/// One window over a bounded list: a height, a scroll margin, and an offset.
///
/// The viewport holds no item. [`ListViewport::reconcile`] takes the measure
/// of every item and the position of the selected one, moves the window until
/// it shows the selection, and places every item that the window shows.
///
/// Call [`ListViewport::reconcile`] after every change of the items, the
/// selection, the height, or the scroll margin. The placements describe the
/// state of the last reconciliation, so a caller that skips one reads a stale
/// window.
///
/// # Examples
///
/// ```
/// use kvim_ui::{ListItem, ListViewport};
///
/// // Ten items of one line each, inside a window of four rows.
/// let mut viewport = ListViewport::new(4);
/// let items = || std::iter::repeat_n(ListItem::single(), 10);
///
/// viewport.reconcile(items(), Some(0));
/// assert_eq!(viewport.total_lines(), 10);
/// assert_eq!(viewport.first_line(), 0);
///
/// // The window follows the selection down the list.
/// viewport.reconcile(items(), Some(6));
/// assert_eq!(viewport.first_line(), 3);
///
/// // The window stops at the end of the list instead of scrolling past it.
/// viewport.reconcile(items(), Some(9));
/// assert_eq!(viewport.first_line(), 6);
/// assert_eq!(viewport.placements().len(), 4);
/// assert_eq!(viewport.placements()[3].index(), 9);
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ListViewport {
    total_lines: u32,
    first_line: u32,
    height_rows: u16,
    margin_rows: u16,
    placements: Vec<ListPlacement>,
}

impl ListViewport {
    /// Creates one empty window of the named height, in terminal rows.
    #[must_use]
    pub fn new(height_rows: u16) -> Self {
        Self {
            height_rows,
            ..Self::default()
        }
    }

    /// Returns the height of the window, in terminal rows.
    #[must_use]
    pub const fn height_rows(&self) -> u16 {
        self.height_rows
    }

    /// Sets the height of the window, in terminal rows.
    ///
    /// Call [`ListViewport::reconcile`] afterward, so the window scrolls the
    /// selected item back inside itself.
    pub const fn set_height_rows(&mut self, height_rows: u16) {
        self.height_rows = height_rows;
    }

    /// Returns the number of rows that the selection keeps above and below
    /// itself.
    #[must_use]
    pub const fn scroll_margin(&self) -> u16 {
        self.margin_rows
    }

    /// Sets the number of rows that the selection keeps above and below
    /// itself.
    ///
    /// The margin stops at half the window, so a short window still shows the
    /// selected item. Call [`ListViewport::reconcile`] afterward.
    pub const fn set_scroll_margin(&mut self, margin_rows: u16) {
        self.margin_rows = margin_rows;
    }

    /// Returns the first visible line of the list.
    #[must_use]
    pub const fn first_line(&self) -> u32 {
        self.first_line
    }

    /// Returns the number of terminal lines that every visible item occupies
    /// together.
    #[must_use]
    pub const fn total_lines(&self) -> u32 {
        self.total_lines
    }

    /// Returns the visible part of every item that the window shows, in list
    /// order.
    ///
    /// The placements cover the window from its first row without a gap while
    /// the items fill it. The first and the last placement may show a part of
    /// an item.
    #[must_use]
    pub fn placements(&self) -> &[ListPlacement] {
        &self.placements
    }

    /// Moves the window until it shows the selection, then places the items.
    ///
    /// `items` supplies the measure of every item of the list, in list order,
    /// and `selected` names the position of the selected item in that same
    /// list. The selected item is always one visible item of the list.
    ///
    /// The margin stops at half the window and at the last line of the list,
    /// so the window never scrolls past the items to satisfy a margin that no
    /// item can fill. An item that is taller than the window shows its first
    /// line.
    pub fn reconcile<I>(&mut self, items: I, selected: Option<usize>)
    where
        I: Iterator<Item = ListItem> + Clone,
    {
        self.placements.clear();
        let lines = list_lines(items.clone(), selected);
        self.total_lines = lines.total;
        if self.height_rows == 0 || lines.total == 0 {
            self.first_line = 0;
            return;
        }
        let height = u32::from(self.height_rows);
        let last_start = lines.total.saturating_sub(height);
        self.first_line = match lines.selected {
            None => self.first_line.min(last_start),
            Some((start, end)) => {
                let margin = u32::from(self.margin_rows).min((height - 1) / 2);
                let low = start.saturating_sub(margin);
                let high = (end + margin).min(lines.total - 1);
                self.first_line
                    .min(low)
                    .max((high + 1).saturating_sub(height))
                    .min(last_start)
                    .min(start)
            }
        };
        self.place(items);
        debug_assert!(
            selected.is_none_or(|index| {
                self.placements
                    .iter()
                    .any(|placement| placement.index == index)
            }),
            "the reconciled offset always shows the selected item"
        );
    }

    /// Places every visible item that the window shows and clips the two ends.
    ///
    /// A hidden item contributes no line, so it never reaches the loop body
    /// that turns a line range into one placement.
    fn place<I>(&mut self, items: I)
    where
        I: Iterator<Item = ListItem>,
    {
        let height = u32::from(self.height_rows);
        let mut line = 0_u32;
        for (index, item) in items.enumerate() {
            if !item.visible {
                continue;
            }
            let end = line + u32::from(item.lines());
            if end <= self.first_line {
                line = end;
                continue;
            }
            if line >= self.first_line + height {
                break;
            }
            let first_line = self.first_line.saturating_sub(line);
            let top_row = line.saturating_sub(self.first_line);
            let visible_lines = (u32::from(item.lines()) - first_line).min(height - top_row);
            let (Ok(first_line), Ok(top_row), Some(lines)) = (
                u16::try_from(first_line),
                u16::try_from(top_row),
                u16::try_from(visible_lines).ok().and_then(NonZeroU16::new),
            ) else {
                debug_assert!(false, "one visible part stays inside the window height");
                return;
            };
            self.placements.push(ListPlacement {
                index,
                first_line,
                lines,
                top_row,
            });
            line = end;
        }
    }
}

/// The line count of one list, and the line range of its selected item.
struct ListLines {
    /// The number of terminal lines that every visible item occupies together.
    total: u32,
    /// The first and the last line of the selected item, both inclusive.
    selected: Option<(u32, u32)>,
}

/// Measures one list in terminal lines and locates its selected item.
///
/// A hidden item contributes no line, so the line of an item counts the
/// visible items before it alone.
fn list_lines<I>(items: I, selected: Option<usize>) -> ListLines
where
    I: Iterator<Item = ListItem>,
{
    let mut total = 0_u32;
    let mut selected_lines = None;
    for (index, item) in items.enumerate() {
        if !item.visible {
            continue;
        }
        let start = total;
        total = total.saturating_add(u32::from(item.lines()));
        if selected == Some(index) {
            selected_lines = Some((start, total - 1));
        }
    }
    debug_assert!(
        total <= LIST_VIEWPORT_LINES_MAX,
        "every caller bounds its own list below LIST_VIEWPORT_LINES_MAX"
    );
    debug_assert!(
        selected.is_none() || selected_lines.is_some(),
        "the caller selects one visible item, so the walk always finds its lines"
    );
    ListLines {
        total,
        selected: selected_lines,
    }
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
