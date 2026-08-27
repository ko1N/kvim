//! The visible region of one buffer inside one window.
//!
//! A viewport is a pure value. One window owns one viewport, so the editor holds
//! no global scroll position. A viewport change never changes buffer content.
//! See `docs/windows.md`.

use std::num::NonZeroU16;

use kvim_core::TextBuffer;
use kvim_settings::DisplaySettings;

use super::cursor::Cursor;

/// The number of lines that stay visible across one full-page move.
///
/// The overlap keeps the reader oriented. A window that cannot hold the overlap
/// moves one line instead.
pub const FULL_PAGE_OVERLAP_ROWS: usize = 2;

/// The explicit alignment commands `zz`, `zt`, and `zb`.
///
/// An explicit alignment overrides the scroll margin for that command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewportAlignment {
    /// Center the cursor line in the window.
    Center,
    /// Align the cursor line to the window top.
    Top,
    /// Align the cursor line to the window bottom.
    Bottom,
}

/// The first visible line and the horizontal offset of one window.
///
/// The horizontal offset counts source columns. `core` defines the terminal-cell
/// column, but only the terminal boundary measures cell width, so the offset
/// stays a source column until rendering maps it. See `docs/text-model.md`.
///
/// # Examples
///
/// ```
/// use std::num::NonZeroU16;
///
/// use kvim_core::TextBuffer;
/// use kvim_editor::{ColumnLimit, Cursor, Viewport};
/// use kvim_settings::{DisplaySettings, FileSettings};
///
/// let text = "line\n".repeat(100);
/// let buffer = TextBuffer::from_text(&text, kvim_core::BufferBytesMax::default())
///     .expect("the text is small");
/// let rows = NonZeroU16::new(10).expect("the literal 10 is not zero");
/// let cells = NonZeroU16::new(80).expect("the literal 80 is not zero");
///
/// // The cursor moves below the window, so the viewport follows it and keeps
/// // the two-row scroll margin.
/// let cursor = Cursor::clamped(&buffer, 40, 0, ColumnLimit::LastCharacter);
/// let viewport = Viewport::new(rows, cells)
///     .reconciled(&buffer, cursor, &DisplaySettings::default());
/// assert_eq!(viewport.first_line(), 33);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Viewport {
    first_line: usize,
    left_column: usize,
    height_rows: NonZeroU16,
    width_cells: NonZeroU16,
}

impl Viewport {
    /// Creates a viewport at the start of the buffer.
    #[must_use]
    pub const fn new(height_rows: NonZeroU16, width_cells: NonZeroU16) -> Self {
        Self {
            first_line: 0,
            left_column: 0,
            height_rows,
            width_cells,
        }
    }

    /// Returns the first visible line.
    #[must_use]
    pub const fn first_line(self) -> usize {
        self.first_line
    }

    /// Returns the first visible source column.
    #[must_use]
    pub const fn left_column(self) -> usize {
        self.left_column
    }

    /// Returns the window height, in rows.
    #[must_use]
    pub const fn height_rows(self) -> NonZeroU16 {
        self.height_rows
    }

    /// Returns the window width, in cells.
    #[must_use]
    pub const fn width_cells(self) -> NonZeroU16 {
        self.width_cells
    }

    /// Returns the number of lines that `Ctrl-D` and `Ctrl-U` move.
    #[must_use]
    pub const fn half_page_rows(self) -> usize {
        let rows = self.rows();
        if rows > 1 { rows / 2 } else { 1 }
    }

    /// Returns the number of lines that `Ctrl-F` and `Ctrl-B` move.
    ///
    /// The value is the window height less `FULL_PAGE_OVERLAP_ROWS`, so two
    /// lines of the previous view stay visible. The overlap keeps the reader
    /// oriented across a page move. Vim uses the same rule.
    #[must_use]
    pub const fn full_page_rows(self) -> usize {
        let rows = self.rows();
        if rows > FULL_PAGE_OVERLAP_ROWS + 1 {
            rows - FULL_PAGE_OVERLAP_ROWS
        } else {
            1
        }
    }

    /// Returns the viewport at a new window size and keeps both offsets.
    ///
    /// A layout change moves the window edges. It does not move the reader, so
    /// a split, a close, and a terminal resize keep the same first visible line
    /// and the same first visible column.
    ///
    /// A viewport offset has no upper limit of its own, because the buffer owns
    /// the last line and the last column. The caller therefore calls
    /// [`Viewport::reconciled`] after the resize. That call pulls both offsets
    /// back to the buffer and the cursor.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::num::NonZeroU16;
    ///
    /// use kvim_core::TextBuffer;
    /// use kvim_editor::{ColumnLimit, Cursor, Viewport};
    /// use kvim_settings::{DisplaySettings, FileSettings};
    ///
    /// let text = "line\n".repeat(100);
    /// let buffer = TextBuffer::from_text(&text, kvim_core::BufferBytesMax::default())
    ///     .expect("the text is small");
    /// let rows = NonZeroU16::new(10).expect("the literal 10 is not zero");
    /// let cells = NonZeroU16::new(80).expect("the literal 80 is not zero");
    ///
    /// let cursor = Cursor::clamped(&buffer, 40, 0, ColumnLimit::LastCharacter);
    /// let viewport = Viewport::new(rows, cells)
    ///     .reconciled(&buffer, cursor, &DisplaySettings::default());
    /// assert_eq!(viewport.first_line(), 33);
    ///
    /// // A narrower and shorter window keeps the reader on the same line.
    /// let rows = NonZeroU16::new(6).expect("the literal 6 is not zero");
    /// let cells = NonZeroU16::new(40).expect("the literal 40 is not zero");
    /// let viewport = viewport.resized(rows, cells);
    /// assert_eq!(viewport.first_line(), 33);
    /// assert_eq!(viewport.height_rows(), rows);
    /// assert_eq!(viewport.width_cells(), cells);
    /// ```
    #[must_use]
    pub const fn resized(self, height_rows: NonZeroU16, width_cells: NonZeroU16) -> Self {
        Self {
            first_line: self.first_line,
            left_column: self.left_column,
            height_rows,
            width_cells,
        }
    }

    /// Moves the viewport until the cursor keeps the configured scroll margins.
    ///
    /// The viewport moves as little as possible, so a cursor that already keeps
    /// both margins leaves the viewport unchanged. A window that is smaller than
    /// twice the margin reduces the margin, so the cursor line always stays
    /// visible.
    #[must_use]
    pub fn reconciled(
        self,
        buffer: &TextBuffer,
        cursor: Cursor,
        display: &DisplaySettings,
    ) -> Self {
        let last_line = buffer.line_count() - 1;
        let first_line = self.reconciled_first_row(
            self.first_line,
            cursor.line().get(),
            last_line,
            usize::from(display.scrolloff_rows),
        );
        let last_column = buffer.line_len_chars(cursor.line());
        let left_column = reconcile_axis(
            self.left_column,
            cursor.column().get(),
            last_column,
            self.cells(),
            usize::from(display.sidescrolloff_cells),
        );
        Self {
            first_line,
            left_column,
            ..self
        }
    }

    /// Returns the first visible row that keeps one row inside the margin.
    ///
    /// The viewport owns the vertical scroll-margin rule, so every region that
    /// shows a list of rows reads it here. [`Viewport::reconciled`] follows the
    /// cursor line of a buffer through this entry point, and the file-tree
    /// sidebar follows its selected row through it, so the two cannot diverge.
    ///
    /// `first_row` is the offset that the caller holds, `row` is the row that
    /// must stay visible, and `last_row` is the largest row that the list
    /// holds. The offset moves as little as possible, so a row that already
    /// keeps the margin returns the offset unchanged. A viewport that is
    /// shorter than twice the margin reduces the margin, and the margin stops
    /// at `last_row`, so the offset never passes the end of the list to satisfy
    /// a margin that no content can fill.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::num::NonZeroU16;
    ///
    /// use kvim_editor::Viewport;
    ///
    /// let rows = NonZeroU16::new(10).expect("the literal 10 is not zero");
    /// let cells = NonZeroU16::new(80).expect("the literal 80 is not zero");
    /// let viewport = Viewport::new(rows, cells);
    ///
    /// // Row 5 keeps the two-row margin inside the first ten rows already.
    /// assert_eq!(viewport.reconciled_first_row(0, 5, 99, 2), 0);
    /// // Row 8 needs two rows below it, so the offset moves by one.
    /// assert_eq!(viewport.reconciled_first_row(0, 8, 99, 2), 1);
    /// // The margin stops at the last row, so the end of the list stays put.
    /// assert_eq!(viewport.reconciled_first_row(0, 9, 9, 2), 0);
    /// ```
    #[must_use]
    pub fn reconciled_first_row(
        self,
        first_row: usize,
        row: usize,
        last_row: usize,
        margin_rows: usize,
    ) -> usize {
        reconcile_axis(first_row, row, last_row, self.rows(), margin_rows)
    }

    /// Aligns the cursor line in the window and ignores the scroll margin.
    #[must_use]
    pub fn aligned(self, cursor: Cursor, alignment: ViewportAlignment) -> Self {
        let rows = self.rows();
        let line = cursor.line().get();
        let first_line = match alignment {
            ViewportAlignment::Top => line,
            ViewportAlignment::Center => line.saturating_sub(rows / 2),
            ViewportAlignment::Bottom => line.saturating_sub(rows - 1),
        };
        Self { first_line, ..self }
    }

    const fn rows(self) -> usize {
        self.height_rows.get() as usize
    }

    const fn cells(self) -> usize {
        self.width_cells.get() as usize
    }
}

/// Moves one viewport offset until the cursor keeps the margin on both sides.
///
/// `last` is the largest position that the axis holds: the last buffer line for
/// the vertical axis, and the line length for the horizontal axis. The margin
/// stops at the axis limit, so the viewport does not scroll past the buffer end
/// to satisfy a margin that no content can fill.
fn reconcile_axis(
    offset: usize,
    position: usize,
    last: usize,
    size: usize,
    margin: usize,
) -> usize {
    debug_assert!(size > 0, "a window size is a non-zero value");
    let margin = margin.min((size - 1) / 2);
    let low = position.saturating_sub(margin);
    let high = position.saturating_add(margin).min(last);
    let reconciled = offset.min(low).max((high + 1).saturating_sub(size));
    debug_assert!(
        reconciled <= position && position < reconciled + size,
        "the reconciled offset always keeps the cursor position visible"
    );
    reconciled
}
