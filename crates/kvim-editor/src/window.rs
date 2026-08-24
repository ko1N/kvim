//! The view of one window into one buffer.
//!
//! A window owns its cursor, its selection anchor, and its viewport. Two windows
//! that show one buffer therefore move and scroll independently: they share the
//! text and nothing else. The mode is global, as it is in Vim, so
//! [`EditingState`](super::EditingState) keeps it. See `docs/windows.md`.

use std::num::NonZeroU16;

use kvim_core::TextBuffer;
use kvim_settings::DisplaySettings;

use super::cursor::Cursor;
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
/// let mut buffer = TextBuffer::from_text(&text, &FileSettings::default())
///     .expect("the text is small");
/// let settings = EditorSettings::default();
/// let mut registers = Registers::default();
/// let mut context = EditContext {
///     buffer: &mut buffer,
///     settings: &settings,
///     search: None,
///     language_indent_width: None,
///     registers: &mut registers,
///     applied: Vec::new(),
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
