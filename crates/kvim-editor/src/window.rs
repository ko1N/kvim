//! The view of one window into one buffer.
//!
//! A window owns its cursor, its selection anchor, and its viewport. Two windows
//! that show one buffer therefore move and scroll independently: they share the
//! text and nothing else. The mode is global, as it is in Vim, so
//! [`EditingState`](super::EditingState) keeps it. See `docs/windows.md`.

use std::num::NonZeroU16;

use kvim_core::TextBuffer;
use kvim_settings::DisplaySettings;

use super::cursor::{ColumnLimit, Cursor};
use super::selection::AnchorPoint;
use super::viewport::Viewport;

/// The cursor, the selection anchor, and the viewport of one window.
///
/// The value is pure. It holds no buffer text, so the window tree stores it
/// beside the buffer identity of the window.
///
/// # Examples
///
/// ```
/// use std::num::NonZeroU16;
///
/// use kvim_core::TextBuffer;
/// use kvim_editor::{EditContext, EditingState, Registers, Viewport, WindowState};
/// use kvim_input::Command;
/// use kvim_settings::{EditorSettings, FileSettings};
///
/// let text = "line\n".repeat(100);
/// let mut buffer = TextBuffer::from_text(&text, kvim_core::BufferBytesMax::default())
///     .expect("the text is small");
/// let settings = EditorSettings::default();
/// let mut registers = Registers::default();
/// let mut context = EditContext {
///     buffer: &mut buffer,
///     settings: &settings,
///     search: None,
///     language_indent_width: None,
///     registers: &mut registers,
/// };
///
/// let rows = NonZeroU16::new(10).expect("the literal 10 is not zero");
/// let cells = NonZeroU16::new(80).expect("the literal 80 is not zero");
/// let viewport = Viewport::new(rows, cells);
///
/// // Two windows show one buffer. Both start at the first line.
/// let left = WindowState::new(viewport);
/// let mut right = WindowState::new(viewport);
/// let mut editing = EditingState::new();
///
/// // A move in the right window scrolls the right window alone.
/// editing.apply(&mut context, &mut right, Command::MoveLastLine, None);
/// assert!(right.first_line() > 0);
/// assert_eq!(left.first_line(), 0);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowState {
    pub(super) cursor: Cursor,
    /// The point where the Visual selection of this window started.
    ///
    /// The value is `Some` exactly while a Visual mode is active, because every
    /// mode transition writes the mode and the anchor together.
    pub(super) anchor: Option<AnchorPoint>,
    pub(super) viewport: Viewport,
}

impl WindowState {
    /// Creates the state of a window that shows the start of a buffer.
    ///
    /// The value needs no buffer, because [`Cursor::ORIGIN`] is valid for every
    /// buffer.
    #[must_use]
    pub const fn new(viewport: Viewport) -> Self {
        Self {
            cursor: Cursor::ORIGIN,
            anchor: None,
            viewport,
        }
    }

    /// Returns the cursor of the window.
    #[must_use]
    pub const fn cursor(self) -> Cursor {
        self.cursor
    }

    /// Returns the viewport of the window.
    #[must_use]
    pub const fn viewport(self) -> Viewport {
        self.viewport
    }

    /// Returns the first visible line of the window.
    #[must_use]
    pub const fn first_line(self) -> usize {
        self.viewport.first_line()
    }

    /// Returns the first visible source column of the window.
    #[must_use]
    pub const fn left_column(self) -> usize {
        self.viewport.left_column()
    }

    /// Returns the viewport with a new first visible source column.
    ///
    /// A terminal renderer uses this after it maps source columns to terminal
    /// cells. The column must not follow the cursor column.
    #[must_use]
    pub fn with_left_column(self, left_column: usize) -> Self {
        debug_assert!(
            left_column <= self.cursor.column().get(),
            "a reconciled viewport starts at or before its cursor"
        );
        Self {
            viewport: self.viewport.with_left_column(left_column),
            ..self
        }
    }

    /// Returns the state of a window that starts to show another buffer.
    ///
    /// The cursor and the anchor belong to the previous text, so both restart.
    /// The window size stays, because the layout did not change.
    #[must_use]
    pub const fn showing_new_buffer(self) -> Self {
        Self {
            cursor: Cursor::ORIGIN,
            anchor: None,
            viewport: self.viewport,
        }
    }

    /// Returns the state at a new window size and keeps both scroll offsets.
    #[must_use]
    pub const fn resized(self, height_rows: NonZeroU16, width_cells: NonZeroU16) -> Self {
        Self {
            viewport: self.viewport.resized(height_rows, width_cells),
            ..self
        }
    }

    /// Moves the viewport until the cursor of this window keeps the margins.
    ///
    /// Every window reconciles against its own cursor and its own buffer, so a
    /// scroll in one window moves no other window.
    #[must_use]
    pub fn reconciled(self, buffer: &TextBuffer, display: &DisplaySettings) -> Self {
        Self {
            viewport: self.viewport.reconciled(buffer, self.cursor, display),
            ..self
        }
    }

    /// Scrolls this window down and keeps its cursor inside the scroll margin.
    #[must_use]
    pub fn scrolled_down(
        self,
        buffer: &TextBuffer,
        rows: usize,
        limit: ColumnLimit,
        display: &DisplaySettings,
    ) -> Self {
        self.with_scrolled_viewport(
            buffer,
            self.viewport
                .scrolled_down(buffer.line_count().saturating_sub(1), rows),
            limit,
            display,
        )
    }

    /// Scrolls this window up and keeps its cursor inside the scroll margin.
    #[must_use]
    pub fn scrolled_up(
        self,
        buffer: &TextBuffer,
        rows: usize,
        limit: ColumnLimit,
        display: &DisplaySettings,
    ) -> Self {
        self.with_scrolled_viewport(buffer, self.viewport.scrolled_up(rows), limit, display)
    }

    fn with_scrolled_viewport(
        self,
        buffer: &TextBuffer,
        viewport: Viewport,
        limit: ColumnLimit,
        display: &DisplaySettings,
    ) -> Self {
        let first = viewport.first_line();
        let height = usize::from(viewport.height_rows().get());
        let buffer_last = buffer.line_count().saturating_sub(1);
        let last = first
            .saturating_add(height.saturating_sub(1))
            .min(buffer_last);
        let margin = usize::from(display.scrolloff_rows).min((height - 1) / 2);
        let first_cursor = if first == 0 {
            0
        } else {
            first.saturating_add(margin)
        };
        let last_cursor = if last == buffer_last {
            buffer_last
        } else {
            last.saturating_sub(margin)
        };
        debug_assert!(
            first_cursor <= last_cursor,
            "the reduced margin leaves at least one legal cursor row"
        );
        let line = self.cursor.line().get().clamp(first_cursor, last_cursor);
        Self {
            cursor: Cursor::clamped(buffer, line, self.cursor.column().get(), limit),
            viewport,
            ..self
        }
    }

    /// Returns the anchor of the window, clamped to the current buffer.
    ///
    /// A window without an anchor answers with the point of its cursor, which
    /// is where a new Visual selection starts. An edit can shorten the anchor
    /// line, so the column clamps to that line.
    pub(super) fn anchor_point(self, buffer: &TextBuffer) -> AnchorPoint {
        let (line, column) = self
            .anchor
            .map_or((self.cursor.line(), self.cursor.column()), |anchor| {
                (anchor.line, anchor.column)
            });
        let line = buffer
            .line_index(line.get().min(buffer.line_count() - 1))
            .expect("the clamp keeps the line index inside the buffer");
        let column = buffer
            .source_column(line, column.get().min(buffer.line_len_chars(line)))
            .expect("the clamp keeps the column inside the anchor line");
        AnchorPoint { line, column }
    }
}
