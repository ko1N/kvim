//! The generic which-key overlay: bounded hints, column layout, and rendering.
//!
//! The module is deterministic and pure. It reads no clock, no filesystem, and
//! no terminal. It holds no binding table: the caller derives its hints from
//! the one shared registry, for example through the which-key view of a
//! `kvim-keymap` resolver, and hands the widget the final key text, the final
//! label, an optional icon, an optional key style, and its own styles.
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
//! The overlay covers the bottom of one body band. It fills the width with
//! columns of equal width, so the keys and the labels of all columns align. It
//! bounds its own height, and its title row reports every hint that follows the
//! page it drew.
//!
//! A list that outgrows the frame holds one page for each frame of columns.
//! [`WhichKeyOverlay::at_page`] names the page, and
//! [`WhichKeyOverlay::render`] reports the page it drew, so a host binds one
//! key that steps through the list and paints the position it reads back.
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
use unicode_width::UnicodeWidthChar;

use crate::layout::fits;

/// The largest number of hints that one overlay accepts.
///
/// One level of a binding table names far fewer next keys than this bound. The
/// bound keeps the measurement, the layout, and the painting of one frame
/// finite for a registry that binds a very large prefix.
pub const WHICH_KEY_HINTS_MAX: usize = 256;

/// The largest number of characters that one overlay text accepts.
///
/// The bound covers the title, the key text, and the label of every hint. A
/// longer text carries no visible information, because one column of one
/// terminal shows far fewer cells.
pub const WHICH_KEY_TEXT_CHARS_MAX: usize = 128;

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
/// title row included.
pub const WHICH_KEY_BODY_SHARE: u16 = 2;

/// The number of rows that the overlay title occupies.
const TITLE_ROWS: u16 = 1;

/// The number of cells between the key column and the label column.
const KEY_GAP_CELLS: usize = 2;

/// The number of cells that the overlay keeps left of its first column.
const LEFT_PAD_CELLS: usize = 1;

/// The number of cells between two overlay columns.
const COLUMN_GAP_CELLS: usize = 2;

/// The number of cells between one icon and the key beside it.
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

    /// Returns the hint with one icon beside its key.
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

/// The styles that the caller gives one overlay.
///
/// The widget carries no palette. It paints the surface, the title row, and the
/// keys in the three styles below, and every icon in its own style.
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
///     title: accent,
///     key: accent,
/// };
/// assert_eq!(styles.key, accent);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WhichKeyStyles {
    /// The style of the overlay background, of every label, and of every gap.
    pub surface: Style,
    /// The style of the title row and of the note that counts the hints
    /// behind the drawn page.
    pub title: Style,
    /// The style of every key text.
    pub key: Style,
}

/// The page that one render drew, and the size of the complete hint list.
///
/// A frame holds a whole number of columns of a bounded height, so a hint list
/// that outgrows one frame holds several pages. Every hint belongs to exactly
/// one page: the pages never overlap, and together they cover the list. The
/// value reports which of them the render drew, so a host paints the position
/// and knows whether a further step reaches another page.
///
/// A frame that holds no hint reports no page at all. An empty hint list and a
/// body band that cannot hold the title row with one hint both report zero
/// pages and an empty range.
///
/// # Examples
///
/// ```
/// use ratatui::buffer::Buffer;
/// use ratatui::layout::Rect;
///
/// use kvim_ui::{WhichKeyOverlay, WhichKeyOverlayRow, WhichKeyStyles};
///
/// let body = Rect::new(0, 0, 24, 8);
/// let mut target = Buffer::empty(body);
/// let hints = [
///     WhichKeyOverlayRow::new("f", "Find"),
///     WhichKeyOverlayRow::new("q", "Quit"),
/// ];
///
/// let overlay = WhichKeyOverlay::new(" Which Key ", &hints, WhichKeyStyles::default())?;
/// let drawn = overlay.render(&mut target, body)?;
/// assert_eq!(drawn.drawn(), 0..2);
/// assert_eq!(drawn.total(), 2);
/// assert_eq!(drawn.pages(), 1);
/// assert!(!drawn.has_next_page());
/// # Ok::<(), kvim_ui::WhichKeyError>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WhichKeyPlacement {
    page: usize,
    pages: usize,
    first: usize,
    shown: usize,
    total: usize,
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
        }
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
/// The value borrows the hints and the title, so the caller keeps the final
/// texts. Construction checks every bound of the content once, so the render
/// then checks its geometry alone.
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
/// use kvim_ui::{WhichKeyOverlay, WhichKeyOverlayRow, WhichKeyStyles};
///
/// let body = Rect::new(0, 0, 30, 8);
/// let mut target = Buffer::empty(body);
/// let hints = [
///     WhichKeyOverlayRow::new("f", "+3 commands"),
///     WhichKeyOverlayRow::new("q", "Close the window"),
/// ];
///
/// let overlay = WhichKeyOverlay::new(" Which Key ", &hints, WhichKeyStyles::default())?;
/// overlay.render(&mut target, body)?;
/// assert_eq!(target.cell((1, 5)).map(|cell| cell.symbol()), Some("W"));
/// assert_eq!(target.cell((1, 6)).map(|cell| cell.symbol()), Some("f"));
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
/// use kvim_ui::{WhichKeyOverlay, WhichKeyOverlayRow, WhichKeyStyles};
///
/// // Forty keys, and a narrow band that holds one short column of them.
/// let keys: Vec<String> = (0..40).map(|index| format!("k{index}")).collect();
/// let hints: Vec<WhichKeyOverlayRow<'_>> = keys
///     .iter()
///     .map(|key| WhichKeyOverlayRow::new(key, "Run the command"))
///     .collect();
/// let body = Rect::new(0, 0, 30, 10);
/// let overlay = WhichKeyOverlay::new(" Which Key ", &hints, WhichKeyStyles::default())?;
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
    title: &'a str,
    hints: &'a [WhichKeyOverlayRow<'a>],
    styles: WhichKeyStyles,
    page: usize,
}

impl<'a> WhichKeyOverlay<'a> {
    /// Validates one hint list and its title, and opens it at its first page.
    ///
    /// The bound refuses a list above [`WHICH_KEY_HINTS_MAX`] rather than
    /// cutting it. Paging changes nothing about that bound: it reaches the
    /// hints of an accepted list, and a caller still bounds its own list first.
    ///
    /// # Errors
    ///
    /// Returns [`WhichKeyError::Hints`] for a list above
    /// [`WHICH_KEY_HINTS_MAX`], and [`WhichKeyError::Text`] for a title, a key
    /// text, or a label above [`WHICH_KEY_TEXT_CHARS_MAX`].
    pub fn new(
        title: &'a str,
        hints: &'a [WhichKeyOverlayRow<'a>],
        styles: WhichKeyStyles,
    ) -> Result<Self, WhichKeyError> {
        if hints.len() > WHICH_KEY_HINTS_MAX {
            return Err(WhichKeyError::Hints {
                hints: hints.len(),
                max: WHICH_KEY_HINTS_MAX,
            });
        }
        check_text(title)?;
        for hint in hints {
            check_text(hint.key)?;
            check_text(hint.label)?;
            if let Some(icon) = hint.icon {
                check_text(icon.glyph)?;
            }
        }
        Ok(Self {
            title,
            hints,
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

    /// Paints one page of the overlay over the bottom of one body band.
    ///
    /// The overlay covers the text behind it, so it blanks its rectangle first.
    /// It spreads the hints over columns of equal width, and every column keeps
    /// the width of the widest hint of the complete list, so the keys and the
    /// labels of all columns and of every page align. A body band that cannot
    /// hold the title row and one hint over its own share paints nothing, which
    /// keeps the text behind it visible.
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
        let total = self.hints.len();
        if body.is_empty() || self.hints.is_empty() {
            return Ok(WhichKeyPlacement::empty(total));
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
        let key_cells = self.widest(|hint| hint.key);
        let label_cells = self.widest(|hint| hint.label);
        let column_cells = icon_cells + key_cells + KEY_GAP_CELLS + label_cells + COLUMN_GAP_CELLS;

        // The height bound keeps the overlay over one part of the body only, so
        // the text around the cursor stays visible while the reader chooses.
        let rows_max = usize::from((body.height / WHICH_KEY_BODY_SHARE).saturating_sub(TITLE_ROWS))
            .min(WHICH_KEY_COLUMN_ROWS_MAX);
        let cells = usize::from(body.width).saturating_sub(LEFT_PAD_CELLS);
        // One page holds one full frame of columns. The pages therefore cover
        // the list without a gap and without an overlap, so a reader who steps
        // through them meets every hint exactly once.
        let capacity = column_layout(total, column_cells, cells, rows_max).shown(total);
        if capacity == 0 {
            return Ok(WhichKeyPlacement::empty(total));
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
        let Ok(height) = u16::try_from(layout.rows_per_column) else {
            debug_assert!(false, "the row bound keeps the overlay height small");
            return Ok(WhichKeyPlacement::empty(total));
        };
        let height = height.saturating_add(TITLE_ROWS);
        let area = Rect::new(body.x, body.bottom() - height, body.width, height);
        fill(target, area, " ");
        target.set_style(area, self.styles.surface);
        self.render_title(target, area, total - first - shown);

        for (index, hint) in hints.iter().enumerate() {
            let column = index / layout.rows_per_column;
            let offset = index % layout.rows_per_column;
            let (Some(x), Ok(offset)) = (
                column_start(area, column, column_cells),
                u16::try_from(offset),
            ) else {
                // A column that starts outside the body paints nothing, which
                // the one-column minimum only reaches on a terminal narrower
                // than one column.
                continue;
            };
            let y = area.y + TITLE_ROWS + offset;
            let mut cursor = x;
            if icon_cells > 0 {
                let glyph = hint.icon.map_or(0, |icon| text_cells(icon.glyph));
                if let Some(icon) = hint.icon {
                    write_cells(target, area, &mut cursor, y, icon.glyph, icon.style);
                }
                let padding = " ".repeat(icon_cells - glyph);
                write_cells(target, area, &mut cursor, y, &padding, self.styles.surface);
            }
            let key_style = hint.key_style.unwrap_or(self.styles.key);
            write_cells(target, area, &mut cursor, y, hint.key, key_style);
            let padding = " ".repeat(key_cells - text_cells(hint.key) + KEY_GAP_CELLS);
            write_cells(target, area, &mut cursor, y, &padding, self.styles.surface);
            write_cells(
                target,
                area,
                &mut cursor,
                y,
                hint.label,
                self.styles.surface,
            );
        }
        Ok(WhichKeyPlacement {
            page,
            pages,
            first,
            shown,
            total,
        })
    }

    /// Renders the title row of the overlay.
    ///
    /// A prefix that reaches more hints than one page holds keeps the rest on
    /// the pages behind it. The title row names how many hints follow the drawn
    /// page, so a reader never believes an incomplete list. The reader reaches
    /// those commands by typing the next key, or by stepping to the next page
    /// where the host binds that step.
    fn render_title(&self, target: &mut Buffer, area: Rect, following: usize) {
        target.set_stringn(
            area.x,
            area.y,
            self.title,
            usize::from(area.width),
            self.styles.title,
        );
        if following == 0 {
            return;
        }
        let note = format!("+{following} more ");
        let width = usize::from(area.width);
        // The note never covers the title, so a narrow overlay keeps its name
        // and drops the count instead.
        if text_cells(self.title) + text_cells(&note) > width {
            return;
        }
        let Ok(offset) = u16::try_from(width - text_cells(&note)) else {
            debug_assert!(false, "the terminal width fits into a u16");
            return;
        };
        target.set_stringn(
            area.x.saturating_add(offset),
            area.y,
            &note,
            text_cells(&note),
            self.styles.title,
        );
    }

    /// Returns the cells of the widest text that one accessor reads.
    fn widest(&self, text: impl Fn(&WhichKeyOverlayRow<'a>) -> &'a str) -> usize {
        self.hints
            .iter()
            .map(|hint| text_cells(text(hint)))
            .max()
            .unwrap_or(0)
    }
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

/// Returns the first cell of one overlay column, or `None` outside the body.
fn column_start(area: Rect, column: usize, column_cells: usize) -> Option<u16> {
    let offset = LEFT_PAD_CELLS.checked_add(column.checked_mul(column_cells)?)?;
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

/// Returns the number of terminal cells that one text occupies.
///
/// The measurement never counts bytes and never counts characters: a wide
/// character occupies two cells, a combining mark occupies none, and a control
/// character occupies one blank cell, because writing it would move the
/// terminal cursor.
///
/// Every measured text is one text that [`WhichKeyOverlay::new`] accepted, or
/// padding that the overlay derived from such a text, so the scan is finite.
fn text_cells(text: &str) -> usize {
    text.chars().map(|value| value.width().unwrap_or(1)).sum()
}

#[cfg(test)]
#[path = "which_key_tests.rs"]
mod tests;
