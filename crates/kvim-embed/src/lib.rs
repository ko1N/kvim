//! Rendered in-memory modal editing.
//!
//! [`MemoryEditor`] owns bounded text and modal state. It performs no input or
//! output and requests no filesystem, process, Git, watcher, or language work.
//! `crates/kvim-embed/examples/in_memory_editor.rs` demonstrates its complete
//! lifecycle.
//!
//! The default feature set enables no worktree or grammar. Grammar features
//! imply `worktree` and add only their named language adapters. A worktree with
//! no grammar remains usable; its language registry is empty, so path-based
//! language work reports typed unsupported outcomes and fenced markup stays
//! plain.

#![deny(missing_docs)]

#[cfg(feature = "worktree")]
mod worktree;

#[cfg(feature = "worktree")]
pub use worktree::{
    COMPLETION_CAPACITY_MAX, CancelPendingProposal, CancelPendingResume, CapacityError,
    EVENT_CAPACITY_MAX, PROCESS_CAPACITY_MAX, ServicePolicy, WORKER_CAPACITY_MAX,
    WORKSPACE_OPERATION_PATHS_MAX, WorkspaceEntryKind, WorkspaceOperation, WorkspaceOperationKind,
    WorkspaceTransfer, WorktreeAccess, WorktreeApplyError, WorktreeApplyErrorKind,
    WorktreeBindingContext, WorktreeBindingMode, WorktreeCapabilities, WorktreeCapacity,
    WorktreeCommandError, WorktreeCompletion, WorktreeCursor, WorktreeCursorShape,
    WorktreeDispatchDecision, WorktreeDispatchError, WorktreeDispatchOutcome, WorktreeDrain,
    WorktreeEditor, WorktreeEditorBuilder, WorktreeEvent, WorktreeGeometryError,
    WorktreeHostReportRequest, WorktreeHostWorkspace, WorktreeInput, WorktreeInputError,
    WorktreeInputOutcome, WorktreeInputRequest, WorktreeInstanceId, WorktreeOpenError,
    WorktreeOpenErrorKind, WorktreeRefusal, WorktreeRunState, WorktreeSemanticDispatch,
    WorktreeShutdown, WorktreeUpdate,
};

use std::num::{NonZeroU16, NonZeroU32};
use std::time::Duration;

use kvim_core::{BufferBytesMax, LoadError, TextBuffer};
use kvim_editor::{CommandOutcome, EditContext, EditingState, Registers, Viewport, WindowState};
use kvim_input::{Command, Mode, is_register_name};
use kvim_settings::{EditorSettings, SettingsError};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use thiserror::Error;
use unicode_width::UnicodeWidthChar;

/// A failure while creating or resizing an in-memory editor.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MemoryEditorError {
    /// The supplied text exceeds its byte limit.
    #[error(transparent)]
    Text(#[from] LoadError),
    /// The supplied settings do not satisfy their published bounds.
    #[error(transparent)]
    Settings(#[from] SettingsError),
    /// A host supplied a character that cannot name a register.
    #[error("{name:?} is not a valid register name")]
    InvalidRegisterName {
        /// The rejected character.
        name: char,
    },
    /// The rectangle has no cells.
    #[error("the editor rectangle must have nonzero width and height")]
    EmptyGeometry,
    /// The rectangle extends beyond the caller-owned cell buffer.
    #[error("the editor rectangle {editor:?} is outside the cell buffer {buffer:?}")]
    GeometryOutsideBuffer {
        /// The editor rectangle.
        editor: Rect,
        /// The cell-buffer rectangle.
        buffer: Rect,
    },
}

/// The result of literal text input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiteralOutcome {
    /// The text changed the buffer.
    Changed,
    /// Insert mode was not active, so the text was refused.
    Refused,
    /// A configured bound refused the text.
    Rejected,
}

/// The result of advancing host-owned elapsed time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TickOutcome {
    /// In-memory modal state has no time-driven transition.
    Unchanged,
}

/// The placement facts produced by one render.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderOutcome {
    cursor: Position,
}

impl RenderOutcome {
    /// Returns the cursor cell selected by the editor.
    #[must_use]
    pub const fn cursor(self) -> Position {
        self.cursor
    }
}

/// A rendered modal editor over caller-supplied text.
///
/// The editor owns only deterministic in-memory state. The host owns terminal
/// lifecycle and passes a ratatui cell buffer to [`MemoryEditor::render`].
///
/// # Examples
///
/// ```
/// use kvim_embed::{LiteralOutcome, MemoryEditor};
/// use kvim_input::Command;
/// use kvim_settings::EditorSettings;
/// use ratatui::{buffer::Buffer, layout::Rect};
///
/// let area = Rect::new(0, 0, 24, 4);
/// let settings = EditorSettings::default();
/// let mut editor = MemoryEditor::open("hello\n", settings, area)?;
/// editor.command(Command::InsertAtLineEnd, None, None)?;
/// assert_eq!(editor.literal(" world"), LiteralOutcome::Changed);
/// editor.command(Command::ReturnToNormal, None, None)?;
///
/// let mut cells = Buffer::empty(area);
/// let rendered = editor.render(&mut cells)?;
/// assert_eq!(editor.text(), "hello world\n");
/// assert!(area.contains(rendered.cursor()));
/// # Ok::<(), kvim_embed::MemoryEditorError>(())
/// ```
#[derive(Debug)]
pub struct MemoryEditor {
    buffer: TextBuffer,
    settings: EditorSettings,
    editing: EditingState,
    registers: Registers,
    window: WindowState,
    area: Rect,
}

impl MemoryEditor {
    /// Opens supplied text at one initial geometry.
    ///
    /// The realized `settings.files.max_file_bytes` value limits the text.
    /// This constructor validates settings, text size, and geometry before it
    /// creates visible editor state.
    pub fn open(
        text: &str,
        settings: EditorSettings,
        area: Rect,
    ) -> Result<Self, MemoryEditorError> {
        validate_area(area)?;
        let settings = settings.realize()?;
        let bytes_max = BufferBytesMax::new(settings.files.max_file_bytes)
            .expect("realized settings validate files.max_file_bytes");
        let buffer = TextBuffer::from_text(text, bytes_max)?;
        let viewport = Viewport::new(
            NonZeroU16::new(area.height).expect("validated geometry has a nonzero height"),
            body_width(area, &buffer, &settings),
        );
        Ok(Self {
            buffer,
            settings,
            editing: EditingState::new(),
            registers: Registers::default(),
            window: WindowState::new(viewport),
            area,
        })
    }

    /// Returns the logical text, without a synthetic final line ending.
    #[must_use]
    pub fn text(&self) -> String {
        let mut text = self.buffer.to_string();
        if self.buffer.final_line_ending() == kvim_core::FinalLineEnding::Absent {
            text.truncate(self.buffer.logical_len_bytes());
        }
        text
    }

    /// Returns the active modal editing mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.editing.mode()
    }

    /// Applies one resolved semantic command.
    /// Returns [`MemoryEditorError::InvalidRegisterName`] without changing
    /// state when `register` is not an ASCII letter, digit, `"`, or `_`.
    pub fn command(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
        register: Option<char>,
    ) -> Result<CommandOutcome, MemoryEditorError> {
        if let Some(name) = register
            && !is_register_name(name)
        {
            return Err(MemoryEditorError::InvalidRegisterName { name });
        }
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: None,
            registers: &mut self.registers,
        };
        let outcome = self.editing.apply_with_register(
            &mut context,
            &mut self.window,
            command,
            count,
            register,
        );
        self.reconcile_window();
        Ok(outcome.outcome())
    }

    /// Applies literal text when Insert mode owns text input.
    pub fn literal(&mut self, text: &str) -> LiteralOutcome {
        if self.mode() != Mode::Insert {
            return LiteralOutcome::Refused;
        }
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: None,
            registers: &mut self.registers,
        };
        let outcome = match self
            .editing
            .insert_text(&mut context, &mut self.window, text)
            .outcome()
        {
            CommandOutcome::Changed => LiteralOutcome::Changed,
            CommandOutcome::Rejected => LiteralOutcome::Rejected,
            _ => LiteralOutcome::Refused,
        };
        self.reconcile_window();
        outcome
    }

    /// Accepts host-owned elapsed time.
    ///
    /// The in-memory editor has no timer-driven state, so elapsed time does not
    /// request a redraw. This method keeps clock ownership with the host.
    pub const fn tick(&mut self, _elapsed: Duration) -> TickOutcome {
        TickOutcome::Unchanged
    }

    /// Changes the accepted render rectangle.
    pub fn resize(&mut self, area: Rect) -> Result<(), MemoryEditorError> {
        validate_area(area)?;
        self.area = area;
        self.window = self.window.resized(
            NonZeroU16::new(area.height).expect("validated geometry has a nonzero height"),
            body_width(area, &self.buffer, &self.settings),
        );
        self.reconcile_window();
        Ok(())
    }

    /// Renders into the caller-owned ratatui cell buffer.
    ///
    /// The operation validates the complete rectangle before changing a cell.
    pub fn render(&self, cells: &mut Buffer) -> Result<RenderOutcome, MemoryEditorError> {
        if !contains_rect(cells.area, self.area) {
            return Err(MemoryEditorError::GeometryOutsideBuffer {
                editor: self.area,
                buffer: cells.area,
            });
        }
        render_editor(self, cells)
    }

    fn reconcile_window(&mut self) {
        let width = body_width(self.area, &self.buffer, &self.settings);
        self.window = self.window.resized(
            NonZeroU16::new(self.area.height).expect("validated geometry has a nonzero height"),
            width,
        );
        self.window = self.window.reconciled(&self.buffer, &self.settings.display);
        let cursor = self.window.cursor();
        let text = self.buffer.line_text(cursor.line());
        let left = reconcile_left_column(
            &text,
            self.window.left_column(),
            cursor.column().get(),
            usize::from(width.get()),
            usize::from(self.settings.display.sidescrolloff_cells),
        );
        self.window = self.window.with_left_column(left);
    }
}

fn gutter_width(area: Rect, buffer: &TextBuffer, settings: &EditorSettings) -> usize {
    if settings.display.number || settings.display.relative_number {
        (buffer.line_count().to_string().len() + 1).min(usize::from(area.width - 1))
    } else {
        0
    }
}

fn body_width(area: Rect, buffer: &TextBuffer, settings: &EditorSettings) -> NonZeroU16 {
    let gutter = u16::try_from(gutter_width(area, buffer, settings))
        .expect("the gutter cannot exceed the u16 rectangle width");
    NonZeroU16::new(area.width - gutter).expect("the gutter always leaves one body cell")
}

fn char_cells(value: char) -> usize {
    value.width().unwrap_or(1)
}

fn clipped_cells(text: &str, cells: usize) -> &str {
    let mut used = 0;
    for (index, value) in text.char_indices() {
        let width = char_cells(value);
        if used + width > cells {
            return &text[..index];
        }
        used += width;
    }
    text
}

fn reconcile_left_column(
    text: &str,
    left: usize,
    cursor: usize,
    width: usize,
    margin: usize,
) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let margin = margin.min((width - 1) / 2);
    let mut left = left.min(cursor);
    let cells_before = |start: usize| {
        chars[start..cursor]
            .iter()
            .copied()
            .map(char_cells)
            .sum::<usize>()
    };
    let cursor_cells = chars.get(cursor).copied().map_or(1, char_cells);
    while left < cursor && cells_before(left) + cursor_cells + margin > width {
        left += 1;
    }
    while left > 0 && cells_before(left - 1) + cursor_cells + margin <= width {
        left -= 1;
    }
    left
}

fn validate_area(area: Rect) -> Result<(), MemoryEditorError> {
    if area.is_empty() {
        return Err(MemoryEditorError::EmptyGeometry);
    }
    Ok(())
}

fn contains_rect(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

fn render_editor(
    editor: &MemoryEditor,
    cells: &mut Buffer,
) -> Result<RenderOutcome, MemoryEditorError> {
    let area = editor.area;
    cells.set_style(area, Style::default());
    let viewport = editor.window.viewport();
    let cursor = editor.window.cursor();
    let gutter_width = gutter_width(area, &editor.buffer, &editor.settings);

    for row in 0..area.height {
        let line_number = viewport.first_line() + usize::from(row);
        if line_number >= editor.buffer.line_count() {
            break;
        }
        let y = area.y + row;
        if gutter_width > 0 {
            let shown =
                if editor.settings.display.relative_number && line_number != cursor.line().get() {
                    line_number.abs_diff(cursor.line().get()).to_string()
                } else {
                    (line_number + 1).to_string()
                };
            let number_width = gutter_width.saturating_sub(1);
            let label = format!("{shown:>number_width$} ");
            cells.set_stringn(
                area.x,
                y,
                label,
                gutter_width,
                Style::default().fg(Color::DarkGray),
            );
        }
        let text_x = area
            .x
            .saturating_add(u16::try_from(gutter_width).unwrap_or(area.width));
        let text_width = usize::from(area.right().saturating_sub(text_x));
        if text_width == 0 {
            continue;
        }
        let line = editor
            .buffer
            .line_index(line_number)
            .expect("the render loop bounds the line index");
        let text = editor.buffer.line_text(line);
        let visible: String = text.chars().skip(viewport.left_column()).collect();
        cells.set_stringn(
            text_x,
            y,
            clipped_cells(&visible, text_width),
            text_width,
            Style::default(),
        );
    }

    let cursor_y = area.y
        + u16::try_from(cursor.line().get().saturating_sub(viewport.first_line()))
            .unwrap_or(u16::MAX);
    let cursor_text = editor.buffer.line_text(cursor.line());
    let before: String = cursor_text
        .chars()
        .skip(viewport.left_column())
        .take(cursor.column().get().saturating_sub(viewport.left_column()))
        .collect();
    let cursor_offset = before.chars().map(char_cells).sum::<usize>();
    let text_x = area
        .x
        .saturating_add(u16::try_from(gutter_width).unwrap_or(area.width));
    let cursor_x = text_x.saturating_add(u16::try_from(cursor_offset).unwrap_or(u16::MAX));
    let cursor = Position::new(
        cursor_x.min(area.right() - 1),
        cursor_y.min(area.bottom() - 1),
    );
    cells[cursor].set_style(Style::default().add_modifier(Modifier::REVERSED));
    Ok(RenderOutcome { cursor })
}

#[cfg(test)]
#[path = "memory_editor_tests.rs"]
mod tests;
