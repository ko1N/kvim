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
mod composition;
mod dialog;
#[cfg(feature = "review")]
mod review;
#[cfg(feature = "worktree")]
mod worktree;

pub use kvim_ui::{
    DIALOG_BODY_LINE_CHARS_MAX, DIALOG_BODY_LINES_MAX, DIALOG_CHOICE_LABEL_CHARS_MAX,
    DIALOG_CHOICES_MAX, DIALOG_DIRECT_KEYS_MAX, DIALOG_POPUP_COLUMNS_MAX, DIALOG_POPUP_ROWS_MAX,
    DIALOG_QUESTION_CHARS_MAX,
};

pub use dialog::{
    DialogAnswer, DialogChoice, DialogChoiceId, DialogChoicePlacement, DialogInput,
    DialogInputOutcome, DialogOpenError, DialogPlacement, DialogRequest, DialogSnapshot,
    DialogStyles,
};

#[cfg(feature = "review")]
pub use kvim_input::ReviewBindingProfile;
#[cfg(feature = "review")]
pub use review::{
    REVIEW_CANDIDATE_ID_BYTES_MAX, REVIEW_CANDIDATES_MAX, REVIEW_EVENTS_MAX, REVIEW_FILE_HUNKS_MAX,
    REVIEW_FILES_MAX, REVIEW_HUNK_LINES_MAX, REVIEW_PANEL_ROWS_MAX, REVIEW_ROOT_LABEL_BYTES_MAX,
    REVIEW_SNAPSHOT_ANCHORS_MAX, ReviewAnchor, ReviewCandidate, ReviewCandidateId, ReviewCommand,
    ReviewCommentBody, ReviewConfig, ReviewError, ReviewEvent, ReviewFile, ReviewFileChange,
    ReviewFocus, ReviewHunk, ReviewInput, ReviewLine, ReviewLineOrigin, ReviewPanelGitState,
    ReviewPanelHeading, ReviewPanelPlacement, ReviewPanelRow, ReviewPanelRowId, ReviewPanelSection,
    ReviewPanelSnapshot, ReviewRenderOutcome, ReviewSection, ReviewSnapshot, ReviewSurface,
    ReviewUpdate,
};
#[cfg(feature = "worktree")]
pub use review::{
    ReviewApplyError, ReviewApplyErrorKind, ReviewCaptureFailure, ReviewCompletion, ReviewDrain,
    ReviewInstanceId, ReviewOpenError, ReviewOpenErrorKind, ReviewRequestId, ReviewShutdown,
};

#[cfg(feature = "worktree")]
pub use composition::{
    WORKTREE_BINDING_OVERRIDES_MAX, WORKTREE_GROUP_LABEL_BYTES_MAX, WORKTREE_HOST_BINDINGS_MAX,
    WORKTREE_HOST_SCOPES_MAX, WORKTREE_OWNER_LABEL_BYTES_MAX, WorktreeAddressedCommand,
    WorktreeBindingCompositionError, WorktreeBindingConflictKind, WorktreeBindingContextError,
    WorktreeBindingFocus, WorktreeBindingModel, WorktreeBindingOverride,
    WorktreeBindingOverrideError, WorktreeHostBinding, WorktreeHostBindingError,
    WorktreeHostBindingLayer, WorktreeHostCommand, WorktreeHostScope, WorktreeHostScopeError,
    WorktreeMergedCommand, WorktreeMergedScope,
};

#[cfg(feature = "worktree")]
pub use worktree::{
    AddressedEditorCommand, COMPLETION_CAPACITY_MAX, CancelPendingProposal, CancelPendingResume,
    CapacityError, EDITOR_COMMAND_COMPLETION_CANDIDATES_MAX, EDITOR_COMMAND_DESCRIPTORS_MAX,
    EVENT_CAPACITY_MAX, EditorCommandArguments, EditorCommandAvailability, EditorCommandCatalog,
    EditorCommandCompletion, EditorCommandDescriptor, EditorCommandExecutionError, EditorCommandId,
    EditorCommandNameCompletion, EditorCommandPathCompletion, EditorCommandRequestId,
    EditorCommandSessionError, EditorCommandSessionId, EditorCursorPosition,
    EditorDiagnosticSummary, EditorFormatterState, EditorStatusSnapshot, FILE_SIDEBAR_DEPTH_MAX,
    FILE_SIDEBAR_LABEL_CHARS_MAX, FILE_SIDEBAR_ROOT_LABEL_BYTES_MAX, FILE_SIDEBAR_ROWS_MAX,
    FileSidebarCommand, FileSidebarGitState, FileSidebarIconRole, FileSidebarNoticeKind,
    FileSidebarOutcome, FileSidebarRow, FileSidebarRowId, FileSidebarRowKind, FileSidebarSnapshot,
    FileSidebarSymlinkState, PROCESS_CAPACITY_MAX, ServicePolicy, SurfaceOwnership,
    WORKER_CAPACITY_MAX, WORKSPACE_OPERATION_PATHS_MAX, WorkspaceEntryKind, WorkspaceOperation,
    WorkspaceOperationKind, WorkspaceTransfer, WorktreeAccess, WorktreeApplyError,
    WorktreeApplyErrorKind, WorktreeBindingContext, WorktreeBindingMode, WorktreeCapabilities,
    WorktreeCapacity, WorktreeCommandError, WorktreeCommandSurface, WorktreeCompletion,
    WorktreeCursor, WorktreeCursorShape, WorktreeDispatchDecision, WorktreeDispatchError,
    WorktreeDispatchOutcome, WorktreeDrain, WorktreeEditor, WorktreeEditorBuilder, WorktreeEvent,
    WorktreeGeometryError, WorktreeHostReportRequest, WorktreeHostWorkspace, WorktreeInput,
    WorktreeInputError, WorktreeInputOutcome, WorktreeInputRequest, WorktreeInstanceId,
    WorktreeOpenError, WorktreeOpenErrorKind, WorktreePresentation, WorktreeRecoveryDecision,
    WorktreeRecoveryError, WorktreeRecoveryId, WorktreeRecoveryOutcome, WorktreeRecoveryStatus,
    WorktreeRefusal, WorktreeRunState, WorktreeSemanticDispatch, WorktreeShutdown, WorktreeUpdate,
};

use std::num::{NonZeroU16, NonZeroU32};
use std::time::Duration;

use kvim_core::{BufferBytesMax, LoadError, TextBuffer};
use kvim_editor::{
    ColumnLimit, CommandOutcome, EditContext, EditingState, Registers, Viewport, WindowState,
};
use kvim_input::{Command, Mode, is_register_name};
pub use kvim_keymap::{
    CellPosition, POINTER_EVENTS_COALESCE_MAX, PointerAction, PointerButton, PointerEvent,
    PointerModifiers, PointerWheel, PointerWheelDirection, PointerWheelError,
};
use kvim_settings::{EditorSettings, SettingsError};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use thiserror::Error;
use unicode_width::UnicodeWidthChar;

const ROW_SCAN_CHARS_MAX: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PointerDragState {
    Idle,
    Dragging { anchor: SourcePosition },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourcePosition {
    line: usize,
    column: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowSymbol {
    Char(char),
    WideTail,
    Blank,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RowCell {
    symbol: RowSymbol,
    column: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TextSurfaceGeometry {
    content: Rect,
    gutter: usize,
    scrollbar_x: Option<u16>,
}

impl TextSurfaceGeometry {
    fn text_x(self) -> u16 {
        self.content
            .x
            .saturating_add(u16::try_from(self.gutter).unwrap_or(self.content.width))
    }

    fn text_width(self) -> NonZeroU16 {
        let gutter = u16::try_from(self.gutter).unwrap_or(self.content.width);
        NonZeroU16::new(self.content.width.saturating_sub(gutter))
            .expect("surface geometry always leaves one text cell")
    }

    fn is_text_column(self, column: u16) -> bool {
        column >= self.text_x() && column < self.content.right()
    }
}

fn text_surface_geometry(
    area: Rect,
    buffer: &TextBuffer,
    settings: &EditorSettings,
) -> TextSurfaceGeometry {
    let reserve_scrollbar = settings.display.scrollbar && area.height > 0 && area.width >= 2;
    let content = Rect {
        width: area.width.saturating_sub(u16::from(reserve_scrollbar)),
        ..area
    };
    let gutter = gutter_width_for(content.width, buffer, settings);
    TextSurfaceGeometry {
        content,
        gutter,
        scrollbar_x: reserve_scrollbar.then(|| area.right().saturating_sub(1)),
    }
}

fn scrollbar_thumb(track: u16, lines: usize, first_line: usize) -> Option<(u16, u16)> {
    if track == 0 || lines <= usize::from(track) {
        return None;
    }
    let track = u128::from(track);
    let lines = u128::try_from(lines.max(1)).unwrap_or(u128::MAX);
    let visible = track.min(lines);
    let max_first = lines.saturating_sub(visible);
    let thumb_len = (track.saturating_mul(visible) / lines).clamp(1, track);
    let thumb_start = if max_first == 0 {
        0
    } else {
        let first = u128::try_from(first_line)
            .unwrap_or(u128::MAX)
            .min(max_first);
        first.saturating_mul(track.saturating_sub(thumb_len)) / max_first
    };
    Some((
        u16::try_from(thumb_start).unwrap_or(u16::MAX),
        u16::try_from(thumb_len).unwrap_or(u16::MAX),
    ))
}

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

/// One facade-owned in-memory editor event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryEditorEvent {
    /// The user answered a host-opened action-agnostic dialog.
    DialogAnswered(DialogAnswer),
}

/// The result of literal text input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiteralOutcome {
    /// The text changed the buffer.
    Changed,
    /// The open dialog consumed the text without changing editor state.
    Consumed,
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

/// The result of one pointer transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerOutcome {
    /// Visible editor state changed.
    Changed,
    /// An open dialog consumed the pointer without changing visible state.
    Consumed,
    /// The pointer action did not apply to this surface.
    Ignored,
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
/// use kvim_embed::{
///     CellPosition, LiteralOutcome, MemoryEditor, PointerAction, PointerButton, PointerEvent,
///     PointerModifiers,
/// };
/// use kvim_input::Command;
/// use kvim_settings::EditorSettings;
/// use ratatui::{buffer::Buffer, layout::Rect};
///
/// let area = Rect::new(0, 0, 24, 4);
/// let settings = EditorSettings::default();
/// let mut editor = MemoryEditor::open("hello\n", settings, area)?;
/// editor.pointer(PointerEvent::new(
///     CellPosition::new(4, 0),
///     PointerModifiers::default(),
///     PointerAction::Press(PointerButton::Left),
/// ));
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
    pointer_drag: PointerDragState,
    dialog: dialog::DialogHost,
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
        let geometry = text_surface_geometry(area, &buffer, &settings);
        let viewport = Viewport::new(
            NonZeroU16::new(area.height).expect("validated geometry has a nonzero height"),
            geometry.text_width(),
        );
        Ok(Self {
            buffer,
            settings,
            editing: EditingState::new(),
            registers: Registers::default(),
            window: WindowState::new(viewport),
            area,
            pointer_drag: PointerDragState::Idle,
            dialog: dialog::DialogHost::new(),
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

    /// Returns whether a host dialog currently owns all input.
    #[must_use]
    pub fn dialog_is_open(&self) -> bool {
        self.dialog.is_open()
    }

    /// Opens one validated host dialog.
    ///
    /// ```
    /// use kvim_embed::{DialogChoice, DialogChoiceId, DialogRequest, DialogStyles, MemoryEditor};
    /// use kvim_settings::EditorSettings;
    /// use ratatui::{buffer::Buffer, layout::Rect};
    ///
    /// let area = Rect::new(0, 0, 40, 10);
    /// let cancel = DialogChoiceId::new(1);
    /// let mut editor = MemoryEditor::open("text", EditorSettings::default(), area)?;
    /// editor.open_dialog(DialogRequest::new(
    ///     "Continue?",
    ///     std::iter::empty::<&str>(),
    ///     [DialogChoice::new(cancel, "Cancel")],
    ///     cancel,
    ///     cancel,
    ///     area,
    ///     DialogStyles::default(),
    /// )?)?;
    /// let mut cells = Buffer::empty(area);
    /// editor.render(&mut cells)?;
    /// assert!(editor.dialog_snapshot().and_then(|value| value.placement().cloned()).is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_dialog(&mut self, request: DialogRequest) -> Result<(), DialogOpenError> {
        dialog::validate_dialog_body(&request, self.area)?;
        self.dialog.open(request)
    }

    /// Closes an open dialog without producing an answer event.
    ///
    /// Returns `true` when a dialog was closed. A queued answer remains queued.
    #[must_use]
    pub fn close_dialog(&mut self) -> bool {
        self.dialog.close()
    }

    /// Returns the current dialog snapshot and latest current placement.
    #[must_use]
    pub fn dialog_snapshot(&self) -> Option<DialogSnapshot> {
        self.dialog.snapshot()
    }

    /// Applies physical dialog input before any editor or host-global input.
    #[must_use]
    pub fn dialog_input(&mut self, input: DialogInput) -> DialogInputOutcome {
        self.dialog.input(input)
    }

    /// Takes the next facade-owned event.
    #[must_use]
    pub fn take_event(&mut self) -> Option<MemoryEditorEvent> {
        self.dialog
            .take_answer()
            .map(MemoryEditorEvent::DialogAnswered)
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
        if self.dialog.is_open() {
            return Ok(CommandOutcome::Applied);
        }
        self.pointer_drag = PointerDragState::Idle;
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
        if self.dialog.is_open() {
            return LiteralOutcome::Consumed;
        }
        self.pointer_drag = PointerDragState::Idle;
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

    /// Accepts one terminal-neutral pointer event.
    ///
    /// Pointer dispatch does not use key-binding arbitration. A left press
    /// places the cursor. A wheel scrolls this surface. A left drag creates a
    /// characterwise Visual selection from its press position.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_embed::{
    ///     CellPosition, MemoryEditor, PointerAction, PointerButton, PointerEvent,
    ///     PointerModifiers, PointerWheel, PointerWheelDirection,
    /// };
    /// use kvim_input::Mode;
    /// use kvim_settings::EditorSettings;
    /// use ratatui::layout::Rect;
    ///
    /// let mut editor = MemoryEditor::open(
    ///     "first\nsecond\nthird\n",
    ///     EditorSettings::default(),
    ///     Rect::new(0, 0, 12, 2),
    /// )?;
    /// let event = |column, row, action| PointerEvent::new(
    ///     CellPosition::new(column, row),
    ///     PointerModifiers::default(),
    ///     action,
    /// );
    /// editor.pointer(event(3, 0, PointerAction::Press(PointerButton::Left)));
    /// editor.pointer(event(5, 1, PointerAction::Drag(PointerButton::Left)));
    /// editor.pointer(event(5, 1, PointerAction::Release(PointerButton::Left)));
    /// assert_eq!(editor.mode(), Mode::Visual);
    /// editor.pointer(event(
    ///     5,
    ///     1,
    ///     PointerAction::Wheel(PointerWheel::new(PointerWheelDirection::Down, 1)?),
    /// ));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn pointer(&mut self, pointer: PointerEvent) -> PointerOutcome {
        if self.dialog.is_open() {
            let outcome = self.dialog.input(DialogInput::Pointer(pointer));
            return match outcome {
                DialogInputOutcome::Redraw | DialogInputOutcome::Answered => {
                    PointerOutcome::Changed
                }
                DialogInputOutcome::Consumed | DialogInputOutcome::NotOpen => {
                    PointerOutcome::Consumed
                }
            };
        }
        let position = pointer.position();
        let inside = position.column() >= self.area.x
            && position.column() < self.area.right()
            && position.row() >= self.area.y
            && position.row() < self.area.bottom();
        match pointer.action() {
            PointerAction::Wheel(wheel) if inside => {
                self.pointer_drag = PointerDragState::Idle;
                let rows = usize::from(self.settings.mouse.scroll_rows)
                    .saturating_mul(usize::from(wheel.ticks()));
                self.window = match wheel.direction() {
                    PointerWheelDirection::Up => self.window.scrolled_up(
                        &self.buffer,
                        rows,
                        ColumnLimit::LastCharacter,
                        &self.settings.display,
                    ),
                    PointerWheelDirection::Down => self.window.scrolled_down(
                        &self.buffer,
                        rows,
                        ColumnLimit::LastCharacter,
                        &self.settings.display,
                    ),
                    PointerWheelDirection::Left | PointerWheelDirection::Right => {
                        return PointerOutcome::Ignored;
                    }
                };
                PointerOutcome::Changed
            }
            PointerAction::Press(PointerButton::Left) if inside => {
                let geometry = text_surface_geometry(self.area, &self.buffer, &self.settings);
                if !geometry
                    .content
                    .contains(Position::new(position.column(), position.row()))
                {
                    self.pointer_drag = PointerDragState::Idle;
                    return PointerOutcome::Ignored;
                }
                let source = self.source_at_cell(position);
                if matches!(
                    self.editing.mode(),
                    Mode::Visual | Mode::VisualLine | Mode::VisualBlock
                ) {
                    self.editing
                        .enter_mode(&self.buffer, &mut self.window, Mode::Normal);
                }
                self.editing
                    .move_to(&self.buffer, &mut self.window, source.line, source.column);
                self.pointer_drag = if geometry.is_text_column(position.column()) {
                    PointerDragState::Dragging { anchor: source }
                } else {
                    PointerDragState::Idle
                };
                PointerOutcome::Changed
            }
            PointerAction::Drag(PointerButton::Left) => {
                let PointerDragState::Dragging { anchor } = self.pointer_drag else {
                    return PointerOutcome::Ignored;
                };
                let rows = usize::from(self.settings.mouse.scroll_rows);
                if position.row() < self.area.y {
                    self.window = self.window.scrolled_up(
                        &self.buffer,
                        rows,
                        ColumnLimit::LastCharacter,
                        &self.settings.display,
                    );
                } else if position.row() >= self.area.bottom() {
                    self.window = self.window.scrolled_down(
                        &self.buffer,
                        rows,
                        ColumnLimit::LastCharacter,
                        &self.settings.display,
                    );
                }
                let geometry = text_surface_geometry(self.area, &self.buffer, &self.settings);
                let text_left = geometry
                    .text_x()
                    .min(geometry.content.right().saturating_sub(1));
                let position = CellPosition::new(
                    position
                        .column()
                        .clamp(text_left, geometry.content.right().saturating_sub(1)),
                    position
                        .row()
                        .clamp(geometry.content.y, geometry.content.bottom() - 1),
                );
                let head = self.source_at_cell(position);
                self.editing
                    .move_to(&self.buffer, &mut self.window, anchor.line, anchor.column);
                self.editing
                    .enter_mode(&self.buffer, &mut self.window, Mode::Normal);
                self.editing
                    .enter_mode(&self.buffer, &mut self.window, Mode::Visual);
                self.editing
                    .move_to(&self.buffer, &mut self.window, head.line, head.column);
                self.reconcile_window();
                PointerOutcome::Changed
            }
            PointerAction::Release(PointerButton::Left) => {
                self.pointer_drag = PointerDragState::Idle;
                PointerOutcome::Ignored
            }
            PointerAction::Press(PointerButton::Right | PointerButton::Middle)
            | PointerAction::Release(PointerButton::Right | PointerButton::Middle)
            | PointerAction::Drag(PointerButton::Right | PointerButton::Middle)
            | PointerAction::Motion
            | PointerAction::Wheel(_)
            | PointerAction::Press(PointerButton::Left) => {
                self.pointer_drag = PointerDragState::Idle;
                PointerOutcome::Ignored
            }
        }
    }

    /// Accepts host-owned elapsed time.
    ///
    /// The in-memory editor has no timer-driven state, so elapsed time does not
    /// request a redraw. This method keeps clock ownership with the host.
    pub const fn tick(&mut self, _elapsed: Duration) -> TickOutcome {
        TickOutcome::Unchanged
    }

    /// Changes the accepted render rectangle.
    ///
    /// An accepted resize closes an open dialog without an answer when its
    /// fixed body rectangle no longer fits.
    pub fn resize(&mut self, area: Rect) -> Result<(), MemoryEditorError> {
        validate_area(area)?;
        if !self.dialog.body_fits(area) {
            let closed = self.dialog.close();
            debug_assert!(closed, "a non-fitting body implies an open dialog");
        } else {
            self.dialog.invalidate();
        }
        self.pointer_drag = PointerDragState::Idle;
        self.area = area;
        let geometry = text_surface_geometry(area, &self.buffer, &self.settings);
        self.window = self.window.resized(
            NonZeroU16::new(area.height).expect("validated geometry has a nonzero height"),
            geometry.text_width(),
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
        let outcome = render_editor(self, cells)?;
        self.dialog
            .render(cells)
            .expect("dialog request and accepted resize keep render geometry valid");
        Ok(outcome)
    }

    fn source_at_cell(&self, position: CellPosition) -> SourcePosition {
        let geometry = text_surface_geometry(self.area, &self.buffer, &self.settings);
        let line = self
            .window
            .first_line()
            .saturating_add(usize::from(position.row().saturating_sub(self.area.y)))
            .min(self.buffer.line_count().saturating_sub(1));
        let line_index = self
            .buffer
            .line_index(line)
            .expect("the line is clamped to the buffer");
        let text_x = geometry.text_x();
        let offset = usize::from(position.column().saturating_sub(text_x));
        let text = self.buffer.line_text(line_index);
        let tab_width = usize::from(self.settings.indent.tab_width.get());
        let first_cell = terminal_column(&text, tab_width, self.window.left_column());
        let column = layout_row(
            &text,
            tab_width,
            first_cell,
            usize::from(geometry.text_width().get()),
        )
        .get(offset)
        .map_or(self.buffer.line_len_chars(line_index), |cell| cell.column)
        .min(self.buffer.line_len_chars(line_index));
        SourcePosition { line, column }
    }

    fn reconcile_window(&mut self) {
        let geometry = text_surface_geometry(self.area, &self.buffer, &self.settings);
        let width = geometry.text_width();
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

fn gutter_width_for(width: u16, buffer: &TextBuffer, settings: &EditorSettings) -> usize {
    if settings.display.number || settings.display.relative_number {
        (buffer.line_count().to_string().len() + 1).min(usize::from(width.saturating_sub(1)))
    } else {
        0
    }
}

fn measure(value: char, cell: usize, tab_width: usize) -> (usize, Option<char>) {
    debug_assert!(tab_width > 0, "realized settings keep tab width non-zero");
    if value == '\t' {
        return (tab_width - cell % tab_width, None);
    }
    match value.width() {
        None => (1, None),
        Some(0) => (0, None),
        Some(width) => (width, Some(value)),
    }
}

fn terminal_column(text: &str, tab_width: usize, column: usize) -> usize {
    let mut cell = 0usize;
    for (index, value) in text.chars().take(ROW_SCAN_CHARS_MAX).enumerate() {
        if index >= column {
            break;
        }
        cell = cell.saturating_add(measure(value, cell, tab_width).0);
    }
    cell
}

fn layout_row(text: &str, tab_width: usize, first_cell: usize, width: usize) -> Vec<RowCell> {
    let end = first_cell.saturating_add(width);
    let mut cells = Vec::with_capacity(width);
    let mut cell = 0usize;
    let mut column = 0usize;
    for value in text.chars().take(ROW_SCAN_CHARS_MAX) {
        if cell >= end {
            break;
        }
        let (used, visible) = measure(value, cell, tab_width);
        if used == 0 {
            column = column.saturating_add(1);
            continue;
        }
        let complete = cell >= first_cell && cell.saturating_add(used) <= end;
        for step in 0..used {
            let at = cell.saturating_add(step);
            if at < first_cell || at >= end {
                continue;
            }
            let symbol = match (step, visible) {
                (0, Some(value)) if complete => RowSymbol::Char(value),
                (_, Some(_)) if complete => RowSymbol::WideTail,
                _ => RowSymbol::Blank,
            };
            cells.push(RowCell { symbol, column });
        }
        cell = cell.saturating_add(used);
        column = column.saturating_add(1);
    }
    while cells.len() < width {
        cells.push(RowCell {
            symbol: RowSymbol::Blank,
            column,
        });
        column = column.saturating_add(1);
    }
    debug_assert_eq!(cells.len(), width, "row layout fills the visible width");
    cells
}

fn char_cells(value: char) -> usize {
    value.width().unwrap_or(1)
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
    let geometry = text_surface_geometry(area, &editor.buffer, &editor.settings);
    let gutter_width = geometry.gutter;

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
        let text_x = geometry.text_x();
        let text_width = usize::from(geometry.text_width().get());
        if text_width == 0 {
            continue;
        }
        let line = editor
            .buffer
            .line_index(line_number)
            .expect("the render loop bounds the line index");
        let text = editor.buffer.line_text(line);
        let tab_width = usize::from(editor.settings.indent.tab_width.get());
        let first_cell = terminal_column(&text, tab_width, viewport.left_column());
        let row = layout_row(&text, tab_width, first_cell, text_width);
        let mut scratch = String::new();
        for (offset, cell) in row.into_iter().enumerate() {
            let symbol = match cell.symbol {
                RowSymbol::Char(value) => {
                    scratch.clear();
                    scratch.push(value);
                    scratch.as_str()
                }
                RowSymbol::WideTail => "",
                RowSymbol::Blank => " ",
            };
            cells[(text_x + u16::try_from(offset).unwrap_or(u16::MAX), y)].set_symbol(symbol);
        }
    }

    if let Some(x) = geometry.scrollbar_x {
        let (thumb_start, thumb_len) = scrollbar_thumb(
            area.height,
            editor.buffer.line_count(),
            viewport.first_line(),
        )
        .unwrap_or((u16::MAX, 0));
        for row in 0..area.height {
            let thumb = row >= thumb_start && row < thumb_start.saturating_add(thumb_len);
            let cell = &mut cells[(x, area.y.saturating_add(row))];
            cell.set_symbol(if thumb { "┃" } else { "│" });
            cell.set_fg(if thumb { Color::Gray } else { Color::DarkGray });
        }
    }

    let cursor_y = area.y
        + u16::try_from(cursor.line().get().saturating_sub(viewport.first_line()))
            .unwrap_or(u16::MAX);
    let cursor_text = editor.buffer.line_text(cursor.line());
    let tab_width = usize::from(editor.settings.indent.tab_width.get());
    let first_cell = terminal_column(&cursor_text, tab_width, viewport.left_column());
    let cursor_cell = terminal_column(&cursor_text, tab_width, cursor.column().get());
    let cursor_offset = cursor_cell.saturating_sub(first_cell);
    let text_x = geometry.text_x();
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
