//! The generic which-key overlay: bounded hints, column layout, and rendering.
//!
//! The module is deterministic and pure. It reads no clock, no filesystem, and
//! no terminal. It holds no binding table: the caller derives its hints from
//! the one shared registry, for example through the which-key view of a
//! `kvim-keymap` resolver, and hands the widget the final key text, the final
//! label, an optional icon, an optional key style, one row marker, and its own
//! styles.
//!
//! A row beside a pending prefix can carry two independent facts: the table
//! that holds the key, and whether pressing the key continues the pending
//! sequence or abandons it. [`WhichKeyOverlayRow::icon`] carries the first fact,
//! because the table is a caller value that this crate cannot enumerate. The
//! caller supplies one icon for each table it draws, exactly as it already
//! does for a command group. [`WhichKeyOverlayRow::key_style`] carries the second
//! fact as a style override for the key text, because the widget selects no
//! color of its own and a caller therefore paints the two senses apart with
//! two styles of its choosing. The two fields are independent: a row sets
//! either, both, or neither.
//!
//! One hint row reads `key marker icon label`. The key stands first, so every
//! key of every column forms one narrow left-aligned run that a reader scans
//! down. The marker column holds one caller glyph, for example an arrow, that
//! points from the key to what it reaches. A row whose icon is absent keeps the
//! marker and the alignment, because the icon column reserves the same cells in
//! every row.
//!
//! The overlay covers the bottom of one body band. The content of the hints
//! decides how many columns the band holds, and the columns then divide that
//! band evenly: every column takes the same slot, the first slot starts at the
//! left margin, and the last slot ends at the right margin. The keys and the
//! labels of all columns therefore align, and a short label leaves the free
//! cells of its slot blank. The overlay bounds its own height. One blank row
//! opens the overlay, one blank row closes its hints, and the footer holds its
//! last row.
//!
//! The footer holds three parts: the breadcrumb of the keys that the reader
//! already pressed at the left, the legend of the navigation keys at the right,
//! and the note that counts the hints behind the drawn page left of the legend.
//! A row that cannot hold every part drops the note first and the legend
//! second, because the breadcrumb names where the reader stands.
//!
//! A list that outgrows the frame holds one page for each frame of columns.
//! [`WhichKeyOverlay::at_page`] names the page, and
//! [`WhichKeyOverlay::render`] reports the page it drew, so a host binds one
//! key that steps through the list and paints the position it reads back.
//!
//! [`WhichKeyOverlay::placement_for`] answers that same report before any
//! paint. A host that writes the page count into the footer it is about to
//! draw reads the count there first, from a shared reference and with no
//! paint, instead of rendering the band once to learn the count and once more
//! to draw the footer.
//!
//! The overlay holds a page rather than a [`ListViewport`](crate::ListViewport)
//! because the two answer different questions. The viewport moves one window of
//! lines until it shows a selected item, and the overlay holds no selection: a
//! host names the position itself. The viewport also stops its window at the
//! last line, so two neighbouring windows overlap, while a reader who steps
//! through a hint list must meet every hint exactly once. The layout is
//! column-major as well, so a window that slid by one row would move every hint
//! into another column on every step.
//!
//! `examples/which_key.rs` builds one registry, feeds it a pending key, derives
//! the hints, and prints the rendered buffer. It then steps through a list that
//! one frame cannot hold.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use thiserror::Error;

use crate::cells::text_cells;
use crate::layout::fits;

/// The largest number of hints that one overlay accepts.
///
/// One level of a binding table names far fewer next keys than this bound. The
/// bound keeps the measurement, the layout, and the painting of one frame
/// finite for a registry that binds a very large prefix.
pub const WHICH_KEY_HINTS_MAX: usize = 256;

/// The largest number of characters that one overlay text accepts.
///
/// The bound covers every footer text, the key text, and the label of every
/// hint. A longer text carries no visible information, because one column of
/// one terminal shows far fewer cells.
pub const WHICH_KEY_TEXT_CHARS_MAX: usize = 128;

/// The largest number of legend entries that one footer accepts.
///
/// The legend names the keys that navigate the overlay itself, and a reader
/// keeps only a handful of them in view. The bound keeps the footer
/// measurement finite for a caller that supplies a longer list.
pub const WHICH_KEY_LEGEND_ENTRIES_MAX: usize = 8;

/// The largest number of hint rows that one overlay column holds.
///
/// The bound keeps the overlay short for a prefix that reaches many commands,
/// even in a tall terminal. A list that outgrows one frame of columns holds
/// several pages, and the overlay reports the page it drew.
pub const WHICH_KEY_COLUMN_ROWS_MAX: usize = 10;

/// The share of the body band that the overlay may cover.
///
/// The overlay answers a pending key while the reader still needs the text
/// around the cursor, so it never covers more than one part of the body out of
/// this many. The value two therefore keeps at least half of the body visible,
/// footer row included.
pub const WHICH_KEY_BODY_SHARE: u16 = 2;

/// The number of rows that the overlay footer occupies.
const FOOTER_ROWS: u16 = 1;

/// The number of blank rows above the first hint row.
const TOP_PAD_ROWS: u16 = 1;

/// The number of blank rows between the last hint row and the footer row.
const BOTTOM_PAD_ROWS: u16 = 1;

/// The number of rows that the overlay keeps beside its hint rows.
///
/// The chrome is the padding row above the hints, the padding row below them,
/// and the footer row. A body band that cannot hold the chrome and one hint
/// paints nothing.
const CHROME_ROWS: u16 = TOP_PAD_ROWS + BOTTOM_PAD_ROWS + FOOTER_ROWS;

/// The number of cells between one legend key and its action word.
const LEGEND_KEY_GAP_CELLS: usize = 1;

/// The number of cells between two legend entries.
const LEGEND_ENTRY_GAP_CELLS: usize = 2;

/// The number of cells between the key column and the marker column.
const KEY_GAP_CELLS: usize = 1;

/// The number of cells between the marker column and the icon column.
const MARKER_GAP_CELLS: usize = 1;

/// The number of cells that the overlay keeps left of its first column.
const LEFT_PAD_CELLS: usize = 4;

/// The number of cells between two overlay columns.
const COLUMN_GAP_CELLS: usize = 2;

/// The number of cells between one icon and the label beside it.
const ICON_GAP_CELLS: usize = 1;

/// The reason that the overlay refused one hint list.
///
/// Every variant names the bound that the value passed, so the caller repairs
/// the input instead of reading a clipped or a partial overlay.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WhichKeyError {
    /// The hint list holds more hints than [`WHICH_KEY_HINTS_MAX`].
    #[error("the overlay holds at most {max} hints, and the caller supplied {hints}")]
    Hints {
        /// The number of hints that the caller supplied.
        hints: usize,
        /// The bound that the hint list passed.
        max: usize,
    },
    /// One text holds more characters than [`WHICH_KEY_TEXT_CHARS_MAX`].
    #[error("an overlay text holds at most {max} characters, and the caller supplied {chars}")]
    Text {
        /// The number of characters that the caller supplied.
        chars: usize,
        /// The bound that the text passed.
        max: usize,
    },
    /// The footer legend holds more entries than
    /// [`WHICH_KEY_LEGEND_ENTRIES_MAX`].
    #[error("the footer legend holds at most {max} entries, and the caller supplied {entries}")]
    Legend {
        /// The number of legend entries that the caller supplied.
        entries: usize,
        /// The bound that the legend passed.
        max: usize,
    },
    /// The body band names cells that the supplied buffer does not hold.
    #[error("the overlay band {body:?} names cells outside the buffer {buffer:?}")]
    Area {
        /// The body band that the caller supplied.
        body: Rect,
        /// The rectangle that the supplied buffer covers.
        buffer: Rect,
    },
}

/// The icon of one hint row.
///
/// The caller owns the glyph and its style, so the widget selects no glyph and
/// no color of its own. Every icon of one overlay should occupy the same number
/// of cells, because the overlay reserves the width of the widest one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WhichKeyIcon<'a> {
    /// The text that the icon cell shows.
    pub glyph: &'a str,
    /// The style of the icon cell.
    pub style: Style,
}

/// The marker between the key and the icon of every hint row.
///
/// The marker points from the key to what the key reaches, for example with an
/// arrow. It is host text, exactly as the footer breadcrumb is, so the widget
/// names no glyph and no color of its own.
///
/// The glyph and its style are one fact, so they travel in one value. A caller
/// therefore cannot name a marker style without a marker glyph, and the widget
/// cannot paint a styled cell that holds no text. The default marker holds an
/// empty glyph, so the marker column then reserves no cell, exactly as the icon
/// column reserves no cell while no hint carries an icon.
///
/// # Examples
///
/// ```
/// use ratatui::style::{Color, Style};
///
/// use kvim_ui::WhichKeyMarker;
///
/// let marker = WhichKeyMarker {
///     glyph: "\u{2192}",
///     style: Style::default().fg(Color::DarkGray),
/// };
/// assert_eq!(marker.glyph, "\u{2192}");
/// assert_eq!(WhichKeyMarker::default().glyph, "");
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WhichKeyMarker<'a> {
    /// The text that the marker column shows in every hint row.
    pub glyph: &'a str,
    /// The style of the marker cells.
    pub style: Style,
}

/// One row of the overlay: the next key, its final label, its icon, and the
/// style of its key text.
///
/// The widget shows one level at a time, so the key text names the single key
/// that may follow the pending sequence, never a complete sequence. Both texts
/// are final: the caller resolved the key into its help form and the command
/// into its label before it built the hint.
///
/// A row beside a pending prefix can carry two independent facts: which table
/// holds the key, and whether the key continues the pending sequence or
/// abandons it. [`WhichKeyOverlayRow::icon`] marks the first fact and
/// [`WhichKeyOverlayRow::key_style`] marks the second, so a row loses neither. A row
/// that sets neither field draws exactly as a row of a context with one table
/// and no abandoning key draws.
///
/// # Examples
///
/// ```
/// use kvim_ui::WhichKeyOverlayRow;
///
/// let hint = WhichKeyOverlayRow::new("f", "+3 commands");
/// assert_eq!(hint.key, "f");
/// assert!(hint.icon.is_none());
/// assert!(hint.key_style.is_none());
/// ```
///
/// A row beside a pending prefix carries both facts at once: an icon names
/// the table, and a key style marks the key as one that abandons the pending
/// sequence.
///
/// ```
/// use ratatui::style::{Color, Style};
///
/// use kvim_ui::{WhichKeyIcon, WhichKeyOverlayRow};
///
/// let icon = WhichKeyIcon {
///     glyph: "#",
///     style: Style::default().fg(Color::Cyan),
/// };
/// let abandons = Style::default().fg(Color::Red);
/// let hint = WhichKeyOverlayRow::new("C-e", "Leave to chat")
///     .with_icon(icon)
///     .with_key_style(abandons);
/// assert_eq!(hint.icon, Some(icon));
/// assert_eq!(hint.key_style, Some(abandons));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WhichKeyOverlayRow<'a> {
    /// The key that follows the pending sequence, in its help form.
    pub key: &'a str,
    /// The label of the command, or the marker of a group of commands.
    pub label: &'a str,
    /// The icon of the row, or `None` while the caller shows no icon.
    pub icon: Option<WhichKeyIcon<'a>>,
    /// The style of the key text, or `None` while the row keeps the overlay's
    /// own key style.
    pub key_style: Option<Style>,
}

impl<'a> WhichKeyOverlayRow<'a> {
    /// Builds one hint without an icon and without a key style.
    #[inline]
    #[must_use]
    pub const fn new(key: &'a str, label: &'a str) -> Self {
        Self {
            key,
            label,
            icon: None,
            key_style: None,
        }
    }

    /// Returns the hint with one icon before its label.
    #[inline]
    #[must_use]
    pub const fn with_icon(self, icon: WhichKeyIcon<'a>) -> Self {
        Self {
            icon: Some(icon),
            ..self
        }
    }

    /// Returns the hint with one style over its key text.
    ///
    /// The style replaces [`WhichKeyStyles::key`] for this row alone, so a
    /// caller marks a row that abandons a pending sequence apart from a row
    /// that continues it, without touching the icon that names the row's
    /// table.
    #[inline]
    #[must_use]
    pub const fn with_key_style(self, style: Style) -> Self {
        Self {
            key_style: Some(style),
            ..self
        }
    }
}

/// One navigation key of the footer legend.
///
/// The legend names the keys that navigate the overlay itself, such as the key
/// that closes it and the key that steps back one level. The two texts stay
/// apart because the key glyph and the action word carry two styles, so the
/// caller supplies the pair instead of one formatted text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WhichKeyLegendEntry<'a> {
    /// The glyph of the key, for example `ESC`.
    pub key: &'a str,
    /// The action that the key performs, for example `close`.
    pub action: &'a str,
}

/// The footer row of one overlay.
///
/// The widget owns no text of its own: the caller names the breadcrumb of the
/// keys it already read and the legend of the keys that navigate the overlay.
/// The note that counts the hints behind the drawn page is the third part of
/// the row, and the overlay derives it from the drawn page alone.
///
/// # Examples
///
/// ```
/// use kvim_ui::{WhichKeyFooter, WhichKeyLegendEntry};
///
/// let legend = [
///     WhichKeyLegendEntry { key: "ESC", action: "close" },
///     WhichKeyLegendEntry { key: "BS", action: "back" },
/// ];
/// let footer = WhichKeyFooter {
///     breadcrumb: "SPC \u{bb} w",
///     legend: &legend,
/// };
/// assert_eq!(footer.legend.len(), 2);
///
/// // An idle overlay names no pressed key and carries no legend.
/// assert_eq!(WhichKeyFooter::default().breadcrumb, "");
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WhichKeyFooter<'a> {
    /// The keys that the reader already pressed, in their final form.
    pub breadcrumb: &'a str,
    /// The keys that navigate the overlay itself.
    pub legend: &'a [WhichKeyLegendEntry<'a>],
}

/// The styles that the caller gives one overlay.
///
/// The widget carries no palette. It paints the surface, the keys, and the
/// three parts of the footer row in the styles below, and every icon in its own
/// style.
///
/// # Examples
///
/// ```
/// use ratatui::style::{Color, Style};
///
/// use kvim_ui::WhichKeyStyles;
///
/// let accent = Style::default().fg(Color::Yellow);
/// let styles = WhichKeyStyles {
///     surface: Style::default().bg(Color::Black),
///     key: accent,
///     note: accent,
///     breadcrumb: accent,
///     legend_key: accent,
///     legend_action: Style::default().fg(Color::Gray),
/// };
/// assert_eq!(styles.key, accent);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WhichKeyStyles {
    /// The style of the overlay background, of every label, and of every gap.
    pub surface: Style,
    /// The style of every key text.
    pub key: Style,
    /// The style of the note that counts the hints behind the drawn page.
    pub note: Style,
    /// The style of the breadcrumb of the keys that the reader pressed.
    pub breadcrumb: Style,
    /// The style of the key glyph of every legend entry.
    pub legend_key: Style,
    /// The style of the action word of every legend entry.
    pub legend_action: Style,
}

/// The exact visible rectangle and source index of one which-key row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WhichKeyRowPlacement {
    /// The index of this row in the hint slice supplied to
    /// [`WhichKeyOverlay::new`].
    pub index: usize,
    /// The exact visible cells of this row's column, clipped to the overlay.
    ///
    /// The rectangle covers the whole slot of the row's column, up to the first
    /// cell of the next column, and the last column reaches the right margin.
    /// Every visible cell of that slot therefore answers the same hint, the
    /// cells beside a short label included. It uses ratatui half-open
    /// containment: its right and bottom edges are outside it.
    pub area: Rect,
}

/// The page that one render drew, the visible row placements, and the size of
/// the complete hint list.
///
/// A host can use [`WhichKeyPlacement::rows`] and
/// [`WhichKeyPlacement::row_at`] to answer a pointer event without repeating
/// the overlay layout. The rows use the same geometry that
/// [`WhichKeyOverlay::render`] paints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhichKeyPlacement {
    page: usize,
    pages: usize,
    first: usize,
    shown: usize,
    total: usize,
    rows: Vec<WhichKeyRowPlacement>,
}

impl WhichKeyPlacement {
    /// The report of a render that drew no hint.
    const fn empty(total: usize) -> Self {
        Self {
            page: 0,
            pages: 0,
            first: 0,
            shown: 0,
            total,
            rows: Vec::new(),
        }
    }

    /// Returns the exact visible placement of every drawn hint row.
    ///
    /// Each row identifies an item in the hint slice supplied to
    /// [`WhichKeyOverlay::new`]. The rectangles stay within the overlay and
    /// body band. An empty or too-small body returns no rows.
    #[must_use]
    pub fn rows(&self) -> &[WhichKeyRowPlacement] {
        &self.rows
    }

    /// Returns the visible row that contains `cell`.
    ///
    /// The lookup uses ratatui half-open rectangles. A cell on the right or
    /// bottom edge of a row therefore selects no row.
    #[must_use]
    pub fn row_at(&self, cell: crate::Cell) -> Option<&WhichKeyRowPlacement> {
        self.rows
            .iter()
            .find(|row| crate::contains_cell(row.area, cell))
    }

    /// Returns the positions of the drawn hints in the supplied hint list.
    ///
    /// The range indexes the slice that the caller gave
    /// [`WhichKeyOverlay::new`], so the caller reads the drawn hints directly.
    #[must_use]
    pub const fn drawn(&self) -> Range<usize> {
        self.first..self.first + self.shown
    }

    /// Returns the number of hints of the complete list.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.total
    }

    /// Returns the page that the render drew.
    ///
    /// The value is the page that [`WhichKeyOverlay::at_page`] named, clamped
    /// to the last page of this frame.
    #[must_use]
    pub const fn page(&self) -> usize {
        self.page
    }

    /// Returns the number of pages that the hint list holds in this frame.
    ///
    /// The count depends on the body band, because a wider or a taller frame
    /// holds more hints on one page.
    #[must_use]
    pub const fn pages(&self) -> usize {
        self.pages
    }

    /// Reports whether one further step reaches another page.
    #[must_use]
    pub const fn has_next_page(&self) -> bool {
        self.page + 1 < self.pages
    }

    /// Reports whether one step back reaches another page.
    #[must_use]
    pub const fn has_previous_page(&self) -> bool {
        self.page > 0
    }
}

/// One validated which-key overlay.
///
/// The value borrows the hints and the footer texts, so the caller keeps the
/// final texts. Construction checks every bound of the content once, so the
/// render then checks its geometry alone.
///
/// The overlay also holds the page of its own hint list. The page travels with
/// the hints it indexes, because [`WhichKeyOverlay::at_page`] returns the
/// overlay that owns both, so no render reads a page of an earlier list.
///
/// # Examples
///
/// ```
/// use ratatui::buffer::Buffer;
/// use ratatui::layout::Rect;
///
/// use kvim_ui::{
///     WhichKeyFooter, WhichKeyMarker, WhichKeyOverlay, WhichKeyOverlayRow, WhichKeyStyles,
/// };
///
/// let body = Rect::new(0, 0, 30, 10);
/// let mut target = Buffer::empty(body);
/// let hints = [
///     WhichKeyOverlayRow::new("f", "+3 commands"),
///     WhichKeyOverlayRow::new("q", "Close the window"),
/// ];
/// let footer = WhichKeyFooter { breadcrumb: "SPC", legend: &[] };
/// let marker = WhichKeyMarker { glyph: "\u{2192}", ..WhichKeyMarker::default() };
///
/// let overlay = WhichKeyOverlay::new(footer, &hints, marker, WhichKeyStyles::default())?;
/// overlay.render(&mut target, body)?;
/// // One blank row opens the overlay, one blank row closes its hints, and the
/// // footer holds its last row.
/// assert_eq!(target.cell((4, 6)).map(|cell| cell.symbol()), Some("f"));
/// assert_eq!(target.cell((6, 6)).map(|cell| cell.symbol()), Some("\u{2192}"));
/// assert_eq!(target.cell((4, 7)).map(|cell| cell.symbol()), Some("q"));
/// assert_eq!(target.cell((4, 9)).map(|cell| cell.symbol()), Some("S"));
/// # Ok::<(), kvim_ui::WhichKeyError>(())
/// ```
///
/// A list that outgrows the frame holds several pages. A host steps through
/// them and meets every hint exactly once:
///
/// ```
/// use ratatui::buffer::Buffer;
/// use ratatui::layout::Rect;
///
/// use kvim_ui::{
///     WhichKeyFooter, WhichKeyMarker, WhichKeyOverlay, WhichKeyOverlayRow, WhichKeyStyles,
/// };
///
/// // Forty keys, and a narrow band that holds one short column of them.
/// let keys: Vec<String> = (0..40).map(|index| format!("k{index}")).collect();
/// let hints: Vec<WhichKeyOverlayRow<'_>> = keys
///     .iter()
///     .map(|key| WhichKeyOverlayRow::new(key, "Run the command"))
///     .collect();
/// let body = Rect::new(0, 0, 30, 10);
/// let footer = WhichKeyFooter::default();
/// let marker = WhichKeyMarker { glyph: "\u{2192}", ..WhichKeyMarker::default() };
/// let overlay = WhichKeyOverlay::new(footer, &hints, marker, WhichKeyStyles::default())?;
///
/// let mut page = 0;
/// let mut reached = 0;
/// loop {
///     let mut target = Buffer::empty(body);
///     let drawn = overlay.at_page(page).render(&mut target, body)?;
///     assert_eq!(drawn.drawn().start, reached, "the pages leave no gap");
///     assert_eq!(drawn.total(), 40);
///     reached = drawn.drawn().end;
///     if !drawn.has_next_page() {
///         break;
///     }
///     page += 1;
/// }
/// assert_eq!(reached, 40, "the steps reach every hint");
/// assert!(page > 0, "one frame does not hold forty hints");
/// # Ok::<(), kvim_ui::WhichKeyError>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub struct WhichKeyOverlay<'a> {
    footer: WhichKeyFooter<'a>,
    hints: &'a [WhichKeyOverlayRow<'a>],
    marker: WhichKeyMarker<'a>,
    styles: WhichKeyStyles,
    page: usize,
}

impl<'a> WhichKeyOverlay<'a> {
    /// Validates one hint list and its footer, and opens it at its first page.
    ///
    /// The bound refuses a list above [`WHICH_KEY_HINTS_MAX`] rather than
    /// cutting it. Paging changes nothing about that bound: it reaches the
    /// hints of an accepted list, and a caller still bounds its own list first.
    ///
    /// # Errors
    ///
    /// Returns [`WhichKeyError::Hints`] for a list above
    /// [`WHICH_KEY_HINTS_MAX`], [`WhichKeyError::Legend`] for a legend above
    /// [`WHICH_KEY_LEGEND_ENTRIES_MAX`], and [`WhichKeyError::Text`] for a
    /// footer text, a marker glyph, a key text, or a label above
    /// [`WHICH_KEY_TEXT_CHARS_MAX`].
    pub fn new(
        footer: WhichKeyFooter<'a>,
        hints: &'a [WhichKeyOverlayRow<'a>],
        marker: WhichKeyMarker<'a>,
        styles: WhichKeyStyles,
    ) -> Result<Self, WhichKeyError> {
        if hints.len() > WHICH_KEY_HINTS_MAX {
            return Err(WhichKeyError::Hints {
                hints: hints.len(),
                max: WHICH_KEY_HINTS_MAX,
            });
        }
        if footer.legend.len() > WHICH_KEY_LEGEND_ENTRIES_MAX {
            return Err(WhichKeyError::Legend {
                entries: footer.legend.len(),
                max: WHICH_KEY_LEGEND_ENTRIES_MAX,
            });
        }
        check_text(footer.breadcrumb)?;
        check_text(marker.glyph)?;
        for entry in footer.legend {
            check_text(entry.key)?;
            check_text(entry.action)?;
        }
        for hint in hints {
            check_text(hint.key)?;
            check_text(hint.label)?;
            if let Some(icon) = hint.icon {
                check_text(icon.glyph)?;
            }
        }
        Ok(Self {
            footer,
            hints,
            marker,
            styles,
            page: 0,
        })
    }

    /// Returns the overlay opened at the named page of its own hint list.
    ///
    /// The page counts frames of columns, from zero. A page above the last one
    /// draws the last page instead, because the number of pages depends on the
    /// body band and a caller learns it from the render alone. The returned
    /// value owns both the hints and the page, so a page of an earlier list
    /// never reaches a render.
    #[must_use]
    pub const fn at_page(mut self, page: usize) -> Self {
        self.page = page;
        self
    }

    /// Answers the placement of the overlay's own page, without painting it.
    ///
    /// The answer names the drawn hints, the size of the complete list, the
    /// drawn page, and the number of pages of `body`, exactly as
    /// [`WhichKeyOverlay::render`] would report them for the same body band,
    /// because `render` calls this same rule. A host that must write the page
    /// count into the footer it is about to draw reads the count here first,
    /// through a shared reference and with no paint, instead of rendering the
    /// band once to learn the count and once more to draw the footer.
    ///
    /// The answer covers the page that [`WhichKeyOverlay::at_page`] set. A
    /// host that wants the count of a different page opens the overlay at
    /// that page first: the drawn hints and the drawn page depend on it, while
    /// the number of pages does not, because the number of pages depends on
    /// the hint list and the body band alone.
    ///
    /// A body band that cannot hold the chrome rows and one hint over its own
    /// share, or an empty hint list, both answer zero pages and an empty
    /// range, exactly as [`WhichKeyOverlay::render`] paints nothing for them.
    ///
    /// # Examples
    ///
    /// A host reads the page count before it draws the footer that reports it.
    ///
    /// ```
    /// use ratatui::layout::Rect;
    ///
    /// use kvim_ui::{
    ///     WhichKeyFooter, WhichKeyMarker, WhichKeyOverlay, WhichKeyOverlayRow, WhichKeyStyles,
    /// };
    ///
    /// let body = Rect::new(0, 0, 24, 8);
    /// let hints = [
    ///     WhichKeyOverlayRow::new("f", "Find"),
    ///     WhichKeyOverlayRow::new("q", "Quit"),
    /// ];
    /// let footer = WhichKeyFooter { breadcrumb: "SPC", legend: &[] };
    /// let marker = WhichKeyMarker { glyph: "\u{2192}", ..WhichKeyMarker::default() };
    /// let overlay = WhichKeyOverlay::new(footer, &hints, marker, WhichKeyStyles::default())?;
    ///
    /// let placement = overlay.placement_for(body);
    /// let breadcrumb = format!("SPC (page {} of {})", placement.page() + 1, placement.pages());
    /// assert_eq!(breadcrumb, "SPC (page 1 of 1)");
    /// # Ok::<(), kvim_ui::WhichKeyError>(())
    /// ```
    #[must_use]
    pub fn placement_for(&self, body: Rect) -> WhichKeyPlacement {
        self.page_geometry(body).placement
    }

    /// Paints one page of the overlay over the bottom of one body band.
    ///
    /// The overlay covers the text behind it, so it blanks its rectangle first.
    /// It spreads the hints over columns of equal width, and every column
    /// reserves the width of the widest hint of the complete list, so the keys
    /// and the labels of all columns and of every page align. The columns then
    /// divide the band evenly, so the last one ends at the right margin and a
    /// short label leaves free cells behind it. Every row reads
    /// `key marker icon label`. A body band that cannot hold the chrome rows
    /// and one hint over its own share paints nothing, which keeps the text
    /// behind it visible.
    ///
    /// [`WhichKeyOverlayRow::key_style`] repaints the key text in place. It adds no
    /// cell of its own, so a page never changes width because a marked row
    /// moved onto it or off it.
    ///
    /// The render writes no cell outside `body` and performs no input and no
    /// output beyond the cell buffer.
    ///
    /// The returned [`WhichKeyPlacement`] names the drawn hints, the size of
    /// the complete list, and the page among the pages of this frame. A host
    /// paints that position and steps to the next page from it.
    ///
    /// # Errors
    ///
    /// Returns [`WhichKeyError::Area`] when `body` names one cell that `target`
    /// does not hold. The buffer keeps every cell in that case, so a host that
    /// supplies a stale rectangle reads no partial overlay. An empty band names
    /// no cell, so every buffer accepts it and the render paints nothing.
    pub fn render(
        &self,
        target: &mut Buffer,
        body: Rect,
    ) -> Result<WhichKeyPlacement, WhichKeyError> {
        let buffer = *target.area();
        if !fits(body, buffer) {
            return Err(WhichKeyError::Area { body, buffer });
        }
        // `page_geometry` is the one capacity rule: it answers the same
        // placement that `WhichKeyOverlay::placement_for` reports, and this
        // render paints from it instead of computing the rule a second time.
        let geometry = self.page_geometry(body);
        let placement = geometry.placement;
        let Some(paint) = geometry.paint else {
            return Ok(placement);
        };
        debug_assert_eq!(
            placement.rows.len(),
            placement.shown,
            "one placement exists for every row that the shared layout draws"
        );
        let area = paint.area;
        fill(target, area, " ");
        target.set_style(area, self.styles.surface);
        self.render_footer(
            target,
            area,
            placement.total - placement.first - placement.shown,
        );

        let hints = &self.hints[placement.drawn()];
        for (hint, row) in hints.iter().zip(&placement.rows) {
            let mut cursor = row.area.x;
            let y = row.area.y;
            let key_style = hint.key_style.unwrap_or(self.styles.key);
            write_cells(target, area, &mut cursor, y, hint.key, key_style);
            let padding = " ".repeat(paint.key_cells - text_cells(hint.key) + KEY_GAP_CELLS);
            write_cells(target, area, &mut cursor, y, &padding, self.styles.surface);
            if paint.marker_cells > 0 {
                write_cells(
                    target,
                    area,
                    &mut cursor,
                    y,
                    self.marker.glyph,
                    self.marker.style,
                );
                let padding = " ".repeat(MARKER_GAP_CELLS);
                write_cells(target, area, &mut cursor, y, &padding, self.styles.surface);
            }
            if paint.icon_cells > 0 {
                let glyph = hint.icon.map_or(0, |icon| text_cells(icon.glyph));
                if let Some(icon) = hint.icon {
                    write_cells(target, area, &mut cursor, y, icon.glyph, icon.style);
                }
                // A row without an icon keeps the reserved cells blank, so its
                // key, its marker, and its label stay in line with every other
                // row.
                let padding = " ".repeat(paint.icon_cells - glyph);
                write_cells(target, area, &mut cursor, y, &padding, self.styles.surface);
            }
            write_cells(
                target,
                area,
                &mut cursor,
                y,
                hint.label,
                self.styles.surface,
            );
        }
        Ok(placement)
    }

    /// Renders the footer row of the overlay.
    ///
    /// The row holds the breadcrumb of the pressed keys at the left, the
    /// legend of the navigation keys at the right, and the note that counts
    /// the hints behind the drawn page left of the legend.
    ///
    /// A prefix that reaches more hints than one page holds keeps the rest on
    /// the pages behind it. The note names how many hints follow the drawn
    /// page, so a reader never believes an incomplete list. The reader reaches
    /// those commands by typing the next key, or by stepping to the next page
    /// where the host binds that step.
    ///
    /// A row that cannot hold every part drops the note first and the legend
    /// second. The breadcrumb answers where the reader stands, so it survives
    /// every narrow band.
    fn render_footer(&self, target: &mut Buffer, area: Rect, following: usize) {
        let y = area.bottom().saturating_sub(FOOTER_ROWS);
        let width = usize::from(area.width);
        // The breadcrumb starts at the first cell of the first hint column, so
        // it stands under the keys that it leads to. The spread never moves
        // that column, so the footer reads the left margin directly.
        let Some(start) = band_cell(area, LEFT_PAD_CELLS) else {
            return;
        };
        let mut cursor = start;
        write_cells(
            target,
            area,
            &mut cursor,
            y,
            self.footer.breadcrumb,
            self.styles.breadcrumb,
        );
        // An empty breadcrumb reserves no cell, so the legend then centers over
        // the whole row.
        let breadcrumb_cells = if self.footer.breadcrumb.is_empty() {
            0
        } else {
            usize::from(cursor.saturating_sub(area.x))
        };

        // The trailing space of the note holds it off the legend that follows
        // it, and off the right margin while the row carries no legend.
        let note = (following > 0).then(|| format!("+{following} more "));
        let note_cells = note.as_deref().map_or(0, text_cells);
        let legend_cells = legend_row_cells(self.footer.legend);
        // The legend ends at the right margin, and the note ends where the
        // legend starts. The note is the first part that a narrow row drops,
        // and the legend the second, because the breadcrumb is the most
        // valuable of the three.
        let legend_start = (legend_cells > 0 && breadcrumb_cells + legend_cells <= width)
            .then(|| width - legend_cells);
        let note_end = legend_start.unwrap_or(width);

        // The note never covers the breadcrumb, so a narrow overlay keeps the
        // keys it already read and drops the count instead.
        if note_cells > 0
            && breadcrumb_cells + note_cells <= note_end
            && let (Some(note), Ok(offset)) = (note, u16::try_from(note_end - note_cells))
        {
            target.set_stringn(
                area.x.saturating_add(offset),
                y,
                &note,
                note_cells,
                self.styles.note,
            );
        }

        let Some(legend_start) = legend_start else {
            return;
        };
        let Ok(offset) = u16::try_from(legend_start) else {
            debug_assert!(false, "the terminal width fits into a u16");
            return;
        };
        let mut cursor = area.x.saturating_add(offset);
        for (index, entry) in self.footer.legend.iter().enumerate() {
            if index > 0 {
                let gap = " ".repeat(LEGEND_ENTRY_GAP_CELLS);
                write_cells(target, area, &mut cursor, y, &gap, self.styles.surface);
            }
            write_cells(
                target,
                area,
                &mut cursor,
                y,
                entry.key,
                self.styles.legend_key,
            );
            let gap = " ".repeat(LEGEND_KEY_GAP_CELLS);
            write_cells(target, area, &mut cursor, y, &gap, self.styles.surface);
            write_cells(
                target,
                area,
                &mut cursor,
                y,
                entry.action,
                self.styles.legend_action,
            );
        }
    }

    /// Returns the cells of the widest text that one accessor reads.
    fn widest(&self, text: impl Fn(&WhichKeyOverlayRow<'a>) -> &'a str) -> usize {
        self.hints
            .iter()
            .map(|hint| text_cells(text(hint)))
            .max()
            .unwrap_or(0)
    }

    /// Answers the geometry of the overlay's own page over `body`, without
    /// painting it.
    ///
    /// This is the one capacity rule of the overlay.
    /// [`WhichKeyOverlay::render`] and [`WhichKeyOverlay::placement_for`] both
    /// call it, so the drawn answer and the pure answer cannot disagree. The
    /// result names the placement that either caller reports, and, when the
    /// page holds a hint, the measurements that painting needs and does not
    /// answer twice.
    fn page_geometry(&self, body: Rect) -> PageGeometry {
        let total = self.hints.len();
        if body.is_empty() || self.hints.is_empty() {
            return PageGeometry::empty(total);
        }
        // Every column keeps the width of the widest hint. A hidden icon
        // reserves no cell, which keeps that alignment without a patched font.
        // The measure reads the complete list, so one hint keeps its cells
        // while the reader steps from one page to the next.
        let icon_cells = self
            .hints
            .iter()
            .filter_map(|hint| hint.icon)
            .map(|icon| text_cells(icon.glyph))
            .max()
            .map_or(0, |glyph| glyph + ICON_GAP_CELLS);
        // The marker column reserves the same cells in every row, so a row
        // whose icon is absent keeps it. An empty glyph reserves no cell, which
        // is the same rule that the icon column follows.
        let marker_cells = match text_cells(self.marker.glyph) {
            0 => 0,
            glyph => glyph + MARKER_GAP_CELLS,
        };
        let key_cells = self.widest(|hint| hint.key);
        let label_cells = self.widest(|hint| hint.label);
        let column_cells =
            key_cells + KEY_GAP_CELLS + marker_cells + icon_cells + label_cells + COLUMN_GAP_CELLS;

        // The height bound keeps the overlay over one part of the body only, so
        // the text around the cursor stays visible while the reader chooses.
        let rows_max =
            usize::from((body.height / WHICH_KEY_BODY_SHARE).saturating_sub(CHROME_ROWS))
                .min(WHICH_KEY_COLUMN_ROWS_MAX);
        let cells = usize::from(body.width).saturating_sub(LEFT_PAD_CELLS);
        // One page holds one full frame of columns. The pages therefore cover
        // the list without a gap and without an overlap, so a reader who steps
        // through them meets every hint exactly once.
        let capacity = column_layout(total, column_cells, cells, rows_max).shown(total);
        if capacity == 0 {
            return PageGeometry::empty(total);
        }
        let pages = total.div_ceil(capacity);
        let page = self.page.min(pages - 1);
        let first = page * capacity;
        let hints = &self.hints[first..(first + capacity).min(total)];

        // A last page that holds fewer hints than one frame spreads them over
        // its own columns, so it never paints an empty column or an empty row.
        let layout = column_layout(hints.len(), column_cells, cells, rows_max);
        let shown = layout.shown(hints.len());
        debug_assert!(
            shown == hints.len(),
            "one page never holds more hints than one frame of columns"
        );
        let height = u16::try_from(layout.rows_per_column)
            .expect("the row bound keeps the overlay height small")
            .saturating_add(CHROME_ROWS);
        let area = Rect::new(body.x, body.bottom() - height, body.width, height);
        // The column count above stays the capacity rule. The spread divides
        // the band over those same columns and changes no count.
        let spread = ColumnSpread::new(layout.columns, column_cells, cells);
        let mut rows = Vec::with_capacity(shown);
        for index in 0..shown {
            let column = index / layout.rows_per_column;
            let offset = u16::try_from(index % layout.rows_per_column)
                .expect("the row bound keeps the row offset small");
            let x = band_cell(area, spread.start(column))
                .expect("the shared column layout keeps every drawn row inside the overlay");
            rows.push(WhichKeyRowPlacement {
                index: first + index,
                area: Rect::new(
                    x,
                    // One blank row opens the overlay, so the first hint row
                    // stands below it. The blank row below the hints and the
                    // footer row close the overlay, and neither names a hint.
                    area.y + TOP_PAD_ROWS + offset,
                    u16::try_from(spread.width(column))
                        .expect("the terminal width bounds one column")
                        .min(area.right() - x),
                    1,
                ),
            });
        }
        PageGeometry {
            placement: WhichKeyPlacement {
                page,
                pages,
                first,
                shown,
                total,
                rows,
            },
            paint: Some(PagePaint {
                area,
                key_cells,
                marker_cells,
                icon_cells,
            }),
        }
    }
}

/// The geometry of one page, before any cell is painted.
///
/// [`WhichKeyOverlay::page_geometry`] is the one capacity rule of the module.
/// [`WhichKeyOverlay::render`] and [`WhichKeyOverlay::placement_for`] both
/// read the result instead of computing a page's geometry a second time.
struct PageGeometry {
    /// The report that both callers answer: the drawn hints, the size of the
    /// complete list, the drawn page, and the number of pages.
    placement: WhichKeyPlacement,
    /// The measurements that painting needs, or `None` when the page holds no
    /// hint, because an empty band or an empty list has nothing to paint.
    paint: Option<PagePaint>,
}

impl PageGeometry {
    /// The geometry of a page that holds no hint.
    const fn empty(total: usize) -> Self {
        Self {
            placement: WhichKeyPlacement::empty(total),
            paint: None,
        }
    }
}

/// The measurements that painting one page needs, beyond its placement.
struct PagePaint {
    /// The exact overlay rectangle that the page layout selected.
    area: Rect,
    /// The width of the widest key of the complete list, in terminal cells.
    key_cells: usize,
    /// The width that the marker column reserves, or zero while the caller
    /// names an empty marker glyph.
    marker_cells: usize,
    /// The width that the icon column reserves, or zero while no hint of the
    /// complete list carries an icon.
    icon_cells: usize,
}

/// Returns the cells that one footer legend occupies, its gaps included.
fn legend_row_cells(legend: &[WhichKeyLegendEntry<'_>]) -> usize {
    let entries: usize = legend
        .iter()
        .map(|entry| text_cells(entry.key) + LEGEND_KEY_GAP_CELLS + text_cells(entry.action))
        .sum();
    entries + LEGEND_ENTRY_GAP_CELLS * legend.len().saturating_sub(1)
}

/// Rejects one text above the character bound.
fn check_text(text: &str) -> Result<(), WhichKeyError> {
    let chars = text.chars().count();
    if chars > WHICH_KEY_TEXT_CHARS_MAX {
        return Err(WhichKeyError::Text {
            chars,
            max: WHICH_KEY_TEXT_CHARS_MAX,
        });
    }
    Ok(())
}

/// How the overlay spreads its hints over columns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ColumnLayout {
    /// The number of columns that the overlay paints.
    pub(crate) columns: usize,
    /// The number of rows that each column holds.
    ///
    /// Every column except the last one is full, because the overlay fills one
    /// column from top to bottom before it starts the next one.
    pub(crate) rows_per_column: usize,
}

impl ColumnLayout {
    /// The layout that paints nothing.
    const EMPTY: Self = Self {
        columns: 0,
        rows_per_column: 0,
    };

    /// Returns the number of rows that the layout shows out of `rows`.
    pub(crate) const fn shown(self, rows: usize) -> usize {
        let capacity = self.columns.saturating_mul(self.rows_per_column);
        if capacity < rows { capacity } else { rows }
    }
}

/// Returns the column layout of one which-key overlay.
///
/// The function is pure: `rows` counts the hints, `column_cells` is the width of
/// one column with its gap, `cells` is the width that the overlay may use, and
/// `rows_max` is the height bound of one column.
///
/// The overlay fills the width: it takes as many columns as `cells` holds, and
/// it then spreads the rows evenly over them, so no column stays empty. A
/// terminal that is narrower than one column still shows one column, which
/// clips at the body edge, because a single column is the readable minimum.
pub(crate) fn column_layout(
    rows: usize,
    column_cells: usize,
    cells: usize,
    rows_max: usize,
) -> ColumnLayout {
    debug_assert!(
        column_cells >= 1,
        "one column holds at least the key of one row"
    );
    if rows == 0 || rows_max == 0 || cells == 0 || column_cells == 0 {
        return ColumnLayout::EMPTY;
    }
    let fitting = (cells / column_cells).max(1);
    let columns = fitting.min(rows);
    let rows_per_column = rows.div_ceil(columns).min(rows_max);
    debug_assert!(rows_per_column >= 1, "a non-empty overlay holds one row");
    // An even spread can leave the last columns empty, for example four rows in
    // three fitting columns. The recount drops those columns.
    let columns = rows.div_ceil(rows_per_column).min(columns);
    ColumnLayout {
        columns,
        rows_per_column,
    }
}

/// Where the overlay paints the columns that [`column_layout`] counted.
///
/// The content decides how many columns the band holds, and nothing else. The
/// spread then divides the band evenly: every column takes the same slot, the
/// first slot starts at the left margin, and the last slot ends at the right
/// margin. One column therefore takes the whole band, and a column whose own
/// label is short keeps the free cells of its slot blank.
///
/// The band rarely divides evenly. The first `wide_slots` slots therefore take
/// one cell above `pitch`, so the columns keep whole cells and the last slot
/// still ends at the margin.
///
/// The slot never falls below the content width of one column: the column count
/// is at most `cells / column_cells`, so `cells / columns` is at least
/// `column_cells`. A band that is exactly full therefore paints as a left-packed
/// band paints. A band that is narrower than one column keeps one clipped
/// column, which is the readable minimum that [`column_layout`] names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ColumnSpread {
    /// The number of columns that the overlay paints.
    columns: usize,
    /// The cells of one column slot, before the remainder.
    pitch: usize,
    /// The number of leading slots that take one cell above `pitch`.
    wide_slots: usize,
}

impl ColumnSpread {
    /// Divides `cells` evenly over `columns` columns of `column_cells` cells.
    fn new(columns: usize, column_cells: usize, cells: usize) -> Self {
        debug_assert!(columns >= 1, "a page that paints a hint holds one column");
        let spread = Self {
            columns,
            pitch: cells / columns,
            wide_slots: cells % columns,
        };
        debug_assert!(
            spread.pitch >= column_cells || cells < column_cells,
            "the column count is at most `cells / column_cells`, so an even \
             division never narrows a slot below the content of one column"
        );
        spread
    }

    /// Returns the first cell of one column, counted from the band's left edge.
    ///
    /// The column after the last one names the right margin of the band, so
    /// [`ColumnSpread::width`] reads every slot from the same rule.
    fn start(self, column: usize) -> usize {
        debug_assert!(
            column <= self.columns,
            "the spread names the columns of the page and its right margin"
        );
        LEFT_PAD_CELLS + column * self.pitch + column.min(self.wide_slots)
    }

    /// Returns the cells of one column slot, up to the column that follows it.
    ///
    /// Every visible cell of one slot answers the same hint, so a pointer that
    /// stands beside a short label still selects the row of that column.
    fn width(self, column: usize) -> usize {
        debug_assert!(column < self.columns, "the page paints this column");
        self.start(column + 1) - self.start(column)
    }
}

/// Returns the cell of one offset inside the band, or `None` outside the body.
fn band_cell(area: Rect, offset: usize) -> Option<u16> {
    let offset = u16::try_from(offset).ok()?;
    let x = area.x.checked_add(offset)?;
    (x < area.right()).then_some(x)
}

/// Writes one text at the cursor and advances the cursor by the painted cells.
fn write_cells(
    target: &mut Buffer,
    area: Rect,
    cursor: &mut u16,
    y: u16,
    text: &str,
    style: Style,
) {
    if *cursor >= area.right() {
        return;
    }
    let remaining = usize::from(area.right() - *cursor);
    target.set_stringn(*cursor, y, text, remaining, style);
    let written = u16::try_from(text_cells(text).min(remaining)).unwrap_or(u16::MAX);
    *cursor = cursor.saturating_add(written);
}

/// Paints one symbol into every cell of the rectangle.
fn fill(target: &mut Buffer, area: Rect, symbol: &str) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = target.cell_mut((x, y)) {
                cell.set_symbol(symbol);
            }
        }
    }
}

#[cfg(test)]
#[path = "which_key_tests.rs"]
mod tests;
