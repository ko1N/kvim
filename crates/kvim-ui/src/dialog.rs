//! A bounded, action-agnostic dialog model.
//!
//! The module owns validation and deterministic choice focus. Layout, painting,
//! and input decoding remain separate concerns. The caller owns every choice
//! identity and maps an answer to its own action.

use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use thiserror::Error;
use unicode_width::UnicodeWidthChar;

use crate::cells::text_cells;

/// The largest number of characters in one dialog question.
pub const DIALOG_QUESTION_CHARS_MAX: usize = 512;
/// The largest number of optional body lines in one dialog.
pub const DIALOG_BODY_LINES_MAX: usize = 8;
/// The largest number of characters in one dialog body line.
pub const DIALOG_BODY_LINE_CHARS_MAX: usize = 160;
/// The largest number of choices in one dialog.
pub const DIALOG_CHOICES_MAX: usize = 8;
/// The largest number of characters in one choice label.
pub const DIALOG_CHOICE_LABEL_CHARS_MAX: usize = 48;
/// The largest number of direct choice keys in one dialog.
pub const DIALOG_DIRECT_KEYS_MAX: usize = DIALOG_CHOICES_MAX;
/// The largest number of rows that one popup may occupy.
pub const DIALOG_POPUP_ROWS_MAX: u16 = 24;
/// The largest number of columns that one popup may occupy.
pub const DIALOG_POPUP_COLUMNS_MAX: u16 = 80;

/// One caller-owned choice of a dialog.
///
/// The caller maps [`DialogChoice::identity`] to its own action after the
/// dialog returns it. A choice never carries an action from this crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogChoice<Id> {
    identity: Id,
    label: String,
    direct_key: Option<char>,
}

impl<Id> DialogChoice<Id> {
    /// Creates one choice with no direct key.
    #[must_use]
    pub fn new(identity: Id, label: impl Into<String>) -> Self {
        Self {
            identity,
            label: label.into(),
            direct_key: None,
        }
    }

    /// Returns the choice with one direct key.
    #[must_use]
    pub const fn with_direct_key(mut self, direct_key: char) -> Self {
        self.direct_key = Some(direct_key);
        self
    }

    /// Returns the caller-owned identity of this choice.
    #[must_use]
    pub const fn identity(&self) -> &Id {
        &self.identity
    }

    /// Returns the label that the dialog displays for this choice.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the direct key of this choice, if it has one.
    #[must_use]
    pub const fn direct_key(&self) -> Option<char> {
        self.direct_key
    }
}

/// The result of one dialog interaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogOutcome<Id> {
    /// Focus moved to this choice.
    Focused(Id),
    /// The dialog answered with this choice.
    Answered(Id),
}

/// One portable keyboard input for a dialog.
///
/// A host converts its own keyboard events into this small vocabulary. The
/// dialog does not depend on a terminal event library.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogKey {
    /// One unmodified character.
    Char(char),
    /// The left arrow key.
    Left,
    /// The right arrow key.
    Right,
    /// The up arrow key.
    Up,
    /// The down arrow key.
    Down,
    /// The Enter key.
    Enter,
    /// The Escape key.
    Esc,
    /// The Ctrl-C key chord.
    CtrlC,
    /// A key that has no dialog-specific representation.
    Unsupported,
}

/// The result of driving one dialog keyboard input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogKeyOutcome<Id> {
    /// The input moved focus or answered the dialog.
    Interaction(DialogOutcome<Id>),
    /// The dialog consumed an unsupported input.
    Consumed,
}

/// One pointer button that a dialog can receive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogPointerButton {
    /// The primary pointer button.
    Primary,
    /// The secondary pointer button.
    Secondary,
    /// The middle pointer button.
    Middle,
    /// Another pointer button.
    Other,
}

/// One portable pointer action for a dialog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogPointerAction {
    /// The pointer moved without a button capture.
    Motion,
    /// A pointer button was pressed.
    Press(DialogPointerButton),
    /// A pointer button was released.
    Release(DialogPointerButton),
    /// A pointer button dragged across a cell.
    Drag(DialogPointerButton),
    /// A wheel moved over a cell.
    Wheel,
}

/// One portable pointer input for a dialog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialogPointerEvent {
    /// The terminal cell that received the pointer action.
    pub cell: crate::Cell,
    /// The pointer action at `cell`.
    pub action: DialogPointerAction,
}

/// The result of driving one dialog pointer input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogPointerOutcome<Id> {
    /// The input moved focus or answered the dialog.
    Interaction(DialogOutcome<Id>),
    /// The input was inside the popup but did not target a choice.
    Consumed,
    /// The input was outside the popup and was consumed by the open dialog.
    OutsidePopup,
    /// The placement does not describe this dialog's published choices.
    PlacementMismatch,
}

/// The reason that a dialog request was refused.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DialogError {
    /// The question passed its character bound.
    #[error("the dialog question holds at most {max} characters, and the caller supplied {chars}")]
    QuestionChars {
        /// The number of characters that the caller supplied.
        chars: usize,
        /// The bound that the question passed.
        max: usize,
    },
    /// The body passed its line-count bound.
    #[error("the dialog body holds at most {max} lines, and the caller supplied {lines}")]
    BodyLines {
        /// The number of lines that the caller supplied.
        lines: usize,
        /// The bound that the body passed.
        max: usize,
    },
    /// A body line passed its character bound.
    #[error(
        "dialog body line {line} holds at most {max} characters, and the caller supplied {chars}"
    )]
    BodyLineChars {
        /// The zero-based body-line position.
        line: usize,
        /// The number of characters that the caller supplied.
        chars: usize,
        /// The bound that the line passed.
        max: usize,
    },
    /// The choice list was empty.
    #[error("the dialog needs at least one choice")]
    EmptyChoices,
    /// The choice list passed its bound.
    #[error("the dialog holds at most {max} choices, and the caller supplied {choices}")]
    Choices {
        /// The number of choices that the caller supplied.
        choices: usize,
        /// The bound that the choice list passed.
        max: usize,
    },
    /// A choice label passed its character bound.
    #[error(
        "dialog choice {choice} holds at most {max} characters, and the caller supplied {chars}"
    )]
    ChoiceLabelChars {
        /// The zero-based choice position.
        choice: usize,
        /// The number of characters that the caller supplied.
        chars: usize,
        /// The bound that the label passed.
        max: usize,
    },
    /// A direct key is not a printable ASCII character.
    #[error("dialog direct key {key:?} must be a printable ASCII character")]
    InvalidDirectKey {
        /// The invalid key.
        key: char,
    },
    /// Two choices use the same direct key.
    #[error("dialog direct key {key:?} appears more than once")]
    DuplicateDirectKey {
        /// The duplicate key.
        key: char,
    },
    /// The safe default identity names no choice.
    #[error("the dialog safe default identity names no choice")]
    UnknownDefault,
    /// The safe default identity names more than one choice.
    #[error("the dialog safe default identity names more than one choice")]
    AmbiguousDefault,
    /// The cancel identity names no choice.
    #[error("the dialog cancel identity names no choice")]
    UnknownCancel,
    /// The cancel identity names more than one choice.
    #[error("the dialog cancel identity names more than one choice")]
    AmbiguousCancel,
    /// A focus request names no choice.
    #[error("the dialog choice identity names no choice")]
    UnknownChoice,
    /// A focus request names more than one choice.
    #[error("the dialog choice identity names more than one choice")]
    AmbiguousChoice,
    /// The supplied body rectangle has coordinates that cannot form a valid edge.
    #[error("the dialog body rectangle {body:?} has an impossible edge")]
    InvalidBodyArea {
        /// The supplied body rectangle.
        body: Rect,
    },
    /// A body cannot hold the smallest complete popup.
    #[error("the body rectangle {body:?} cannot hold the dialog popup")]
    BodyTooSmall {
        /// The supplied body rectangle.
        body: Rect,
    },
    /// The supplied body rectangle is outside the target buffer.
    #[error("the dialog body rectangle {body:?} is outside target buffer {buffer:?}")]
    TargetArea {
        /// The supplied body rectangle.
        body: Rect,
        /// The target buffer rectangle.
        buffer: Rect,
    },
    /// A popup has no drawable rows or columns.
    #[error("the popup rectangle {area:?} needs at least one row and one column")]
    EmptyPopup {
        /// The invalid popup rectangle.
        area: Rect,
    },
    /// A popup passed its row bound.
    #[error("the popup holds at most {max} rows, and the caller supplied {rows}")]
    PopupRows {
        /// The number of rows that the caller supplied.
        rows: u16,
        /// The bound that the popup passed.
        max: u16,
    },
    /// A popup passed its column bound.
    #[error("the popup holds at most {max} columns, and the caller supplied {columns}")]
    PopupColumns {
        /// The number of columns that the caller supplied.
        columns: u16,
        /// The bound that the popup passed.
        max: u16,
    },
}

/// The caller-supplied semantic styles of a dialog popup.
///
/// The dialog owns no palette. The host supplies semantic styles for its
/// surface, dimmed background, rail, and content roles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DialogStyles {
    /// The style applied to the supplied body before the popup paints.
    pub dim: Style,
    /// The style of the popup surface and blank padding.
    pub surface: Style,
    /// The style of the full-height left rail.
    pub rail: Style,
    /// The style of optional body text.
    pub body: Style,
    /// The style of the wrapped question.
    pub question: Style,
    /// The style of a non-default unfocused choice.
    pub choice: Style,
    /// The style of the safe default choice when it is unfocused.
    pub default_choice: Style,
    /// The style of the focused choice.
    pub focused_choice: Style,
}

/// The exact painted rectangle of one visible dialog choice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogChoicePlacement<Id> {
    /// The caller-owned identity of this choice.
    pub identity: Id,
    /// The exact cells painted for this choice, excluding the rail.
    pub area: Rect,
}

/// The geometry that one dialog render uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogPlacement<Id> {
    /// The owner-supplied body rectangle that bounds dimming and the popup.
    pub body_area: Rect,
    /// The rectangle occupied by the complete popup.
    pub popup: Rect,
    /// The full-height rectangle occupied by the left rail.
    pub rail: Rect,
    /// The content rectangle after the rail and blank separator column.
    pub content: Rect,
    /// The rectangle occupied by optional body text lines.
    pub body_text: Rect,
    /// The rectangle occupied by wrapped question lines.
    pub question: Rect,
    /// The exact rectangles occupied by visible choices.
    pub choices: Vec<DialogChoicePlacement<Id>>,
}

/// A bounded dialog and its focused choice.
///
/// The constructor validates all caller data. Movement and answer methods are
/// deterministic. They read no terminal, clock, filesystem, process, or
/// network state.
///
/// ```
/// use kvim_ui::{Dialog, DialogChoice, DialogOutcome};
///
/// let choices = [
///     DialogChoice::new("keep", "Keep editing"),
///     DialogChoice::new("discard", "Discard changes").with_direct_key('d'),
/// ];
/// let mut dialog = Dialog::new(
///     "Discard unsaved changes?",
///     std::iter::empty::<&str>(),
///     choices,
///     "keep",
///     "keep",
/// )?;
/// assert_eq!(dialog.next(), DialogOutcome::Focused("discard"));
/// assert_eq!(dialog.answer_for_direct_key('d'), Some(DialogOutcome::Answered("discard")));
/// # Ok::<(), kvim_ui::DialogError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dialog<Id> {
    question: String,
    body: Vec<String>,
    choices: Vec<DialogChoice<Id>>,
    default: usize,
    cancel: usize,
    focused: usize,
}

impl<Id: Eq> Dialog<Id> {
    /// Validates and opens one dialog with focus on its safe default choice.
    ///
    /// The default and cancel identities may name the same choice. Each must
    /// name exactly one choice.
    ///
    /// # Errors
    ///
    /// Returns a typed refusal when content, direct keys, or choice roles pass
    /// a published bound or do not name exactly one choice.
    pub fn new<B, C>(
        question: impl Into<String>,
        body: B,
        choices: C,
        default: Id,
        cancel: Id,
    ) -> Result<Self, DialogError>
    where
        B: IntoIterator,
        B::Item: Into<String>,
        C: IntoIterator<Item = DialogChoice<Id>>,
    {
        let question = question.into();
        check_chars(&question, DIALOG_QUESTION_CHARS_MAX).map_err(|chars| {
            DialogError::QuestionChars {
                chars,
                max: DIALOG_QUESTION_CHARS_MAX,
            }
        })?;
        let body: Vec<String> = body.into_iter().map(Into::into).collect();
        if body.len() > DIALOG_BODY_LINES_MAX {
            return Err(DialogError::BodyLines {
                lines: body.len(),
                max: DIALOG_BODY_LINES_MAX,
            });
        }
        for (line, text) in body.iter().enumerate() {
            if let Err(chars) = check_chars(text, DIALOG_BODY_LINE_CHARS_MAX) {
                return Err(DialogError::BodyLineChars {
                    line,
                    chars,
                    max: DIALOG_BODY_LINE_CHARS_MAX,
                });
            }
        }
        let choices: Vec<DialogChoice<Id>> = choices.into_iter().collect();
        if choices.is_empty() {
            return Err(DialogError::EmptyChoices);
        }
        if choices.len() > DIALOG_CHOICES_MAX {
            return Err(DialogError::Choices {
                choices: choices.len(),
                max: DIALOG_CHOICES_MAX,
            });
        }
        let mut direct_keys = Vec::with_capacity(choices.len());
        for (choice, item) in choices.iter().enumerate() {
            if let Err(chars) = check_chars(&item.label, DIALOG_CHOICE_LABEL_CHARS_MAX) {
                return Err(DialogError::ChoiceLabelChars {
                    choice,
                    chars,
                    max: DIALOG_CHOICE_LABEL_CHARS_MAX,
                });
            }
            if let Some(key) = item.direct_key {
                if !key.is_ascii_graphic() {
                    return Err(DialogError::InvalidDirectKey { key });
                }
                if direct_keys.contains(&key.to_ascii_lowercase()) {
                    return Err(DialogError::DuplicateDirectKey { key });
                }
                direct_keys.push(key.to_ascii_lowercase());
            }
        }
        debug_assert!(
            direct_keys.len() <= DIALOG_DIRECT_KEYS_MAX,
            "each validated choice carries at most one direct key"
        );
        let default = choice_index(&choices, &default).map_err(role_error_default)?;
        let cancel = choice_index(&choices, &cancel).map_err(role_error_cancel)?;
        Ok(Self {
            question,
            body,
            choices,
            default,
            cancel,
            focused: default,
        })
    }

    /// Validates one rectangle before it becomes a popup rectangle.
    ///
    /// Layout uses this bound before it publishes or paints a popup.
    ///
    /// # Errors
    ///
    /// Returns a typed refusal when the rectangle has no cells or passes a
    /// published row or column bound.
    pub fn validate_popup_area(area: Rect) -> Result<(), DialogError> {
        if area.width == 0 || area.height == 0 {
            return Err(DialogError::EmptyPopup { area });
        }
        if area.height > DIALOG_POPUP_ROWS_MAX {
            return Err(DialogError::PopupRows {
                rows: area.height,
                max: DIALOG_POPUP_ROWS_MAX,
            });
        }
        if area.width > DIALOG_POPUP_COLUMNS_MAX {
            return Err(DialogError::PopupColumns {
                columns: area.width,
                max: DIALOG_POPUP_COLUMNS_MAX,
            });
        }
        Ok(())
    }

    /// Calculates the one placement that a render consumes.
    ///
    /// The popup is centered in `body`. It has a one-cell rail, one blank
    /// separator column, and one blank row above and below content.
    ///
    /// # Errors
    ///
    /// Returns a typed refusal when the supplied body cannot hold the bounded
    /// popup. This method does not change dialog state.
    pub fn placement_for(&self, body: Rect) -> Result<DialogPlacement<Id>, DialogError>
    where
        Id: Clone,
    {
        self.geometry(body)
    }

    /// Paints the dialog into `target` and returns the placement it consumed.
    ///
    /// The renderer dims only `body`. It rejects a stale body outside the
    /// target before changing any target cell.
    ///
    /// # Errors
    ///
    /// Returns a typed refusal for a body outside `target` or a body too small
    /// for the popup. Neither refusal changes dialog state.
    pub fn render(
        &self,
        target: &mut Buffer,
        body: Rect,
        styles: DialogStyles,
    ) -> Result<DialogPlacement<Id>, DialogError>
    where
        Id: Clone,
    {
        let buffer = *target.area();
        if !rect_fits(body, buffer) {
            return Err(DialogError::TargetArea { body, buffer });
        }
        let placement = self.geometry(body)?;
        target.set_style(body, styles.dim);
        fill(target, placement.popup, " ");
        target.set_style(placement.popup, styles.surface);
        for y in placement.rail.y..placement.rail.bottom() {
            target.set_stringn(
                placement.rail.x,
                y,
                "│",
                1,
                styles.surface.patch(styles.rail),
            );
        }
        for (line, y) in self
            .body
            .iter()
            .zip(placement.body_text.y..placement.body_text.bottom())
        {
            target.set_stringn(
                placement.content.x,
                y,
                line,
                usize::from(placement.content.width),
                styles.surface.patch(styles.body),
            );
        }
        for (line, y) in wrap(&self.question, usize::from(placement.content.width))
            .expect("placement validates every question character against the content width")
            .iter()
            .zip(placement.question.y..placement.question.bottom())
        {
            target.set_stringn(
                placement.content.x,
                y,
                line,
                usize::from(placement.content.width),
                styles.surface.patch(styles.question),
            );
        }
        for (index, choice) in placement.choices.iter().enumerate() {
            let style = if index == self.focused {
                styles.focused_choice
            } else if index == self.default {
                styles.default_choice
            } else {
                styles.choice
            };
            let label = format!("> {}", self.choices[index].label());
            target.set_stringn(
                choice.area.x,
                choice.area.y,
                label,
                usize::from(choice.area.width),
                styles.surface.patch(style),
            );
        }
        Ok(placement)
    }

    fn geometry(&self, body: Rect) -> Result<DialogPlacement<Id>, DialogError>
    where
        Id: Clone,
    {
        const FRAME_COLUMNS: u16 = 2;
        const FRAME_ROWS: u16 = 2;
        let Some(body_right) = body.x.checked_add(body.width) else {
            return Err(DialogError::InvalidBodyArea { body });
        };
        let Some(body_bottom) = body.y.checked_add(body.height) else {
            return Err(DialogError::InvalidBodyArea { body });
        };
        let widest_choice = self
            .choices
            .iter()
            .map(|choice| text_cells(choice.label()).saturating_add(2))
            .max()
            .unwrap_or(0);
        let widest_body = self
            .body
            .iter()
            .map(|line| text_cells(line))
            .max()
            .unwrap_or(0);
        let required_content_width = widest_choice.max(widest_body).max(1);
        let required_width = u16::try_from(required_content_width)
            .ok()
            .and_then(|width| width.checked_add(FRAME_COLUMNS))
            .ok_or(DialogError::BodyTooSmall { body })?;
        let max_width = body.width.min(DIALOG_POPUP_COLUMNS_MAX);
        if required_width > max_width {
            return Err(DialogError::BodyTooSmall { body });
        }
        let content_width = max_width - FRAME_COLUMNS;
        let question = wrap(&self.question, usize::from(content_width))
            .ok_or(DialogError::BodyTooSmall { body })?;
        let content_rows = self
            .body
            .len()
            .checked_add(question.len())
            .and_then(|rows| rows.checked_add(self.choices.len()))
            .ok_or(DialogError::BodyTooSmall { body })?;
        let height = u16::try_from(content_rows)
            .ok()
            .and_then(|rows| rows.checked_add(FRAME_ROWS))
            .ok_or(DialogError::BodyTooSmall { body })?;
        if height > body.height || height > DIALOG_POPUP_ROWS_MAX {
            return Err(DialogError::BodyTooSmall { body });
        }
        let width = max_width;
        let popup_x = body
            .x
            .checked_add((body.width - width) / 2)
            .ok_or(DialogError::InvalidBodyArea { body })?;
        let popup_y = body
            .y
            .checked_add((body.height - height) / 2)
            .ok_or(DialogError::InvalidBodyArea { body })?;
        let popup = Rect::new(popup_x, popup_y, width, height);
        debug_assert!(
            popup.right() <= body_right,
            "validated popup stays inside the supplied body"
        );
        debug_assert!(
            popup.bottom() <= body_bottom,
            "validated popup stays inside the supplied body"
        );
        let rail = Rect::new(popup.x, popup.y, 1, popup.height);
        let content_x = popup
            .x
            .checked_add(FRAME_COLUMNS)
            .ok_or(DialogError::InvalidBodyArea { body })?;
        let content_y = popup
            .y
            .checked_add(1)
            .ok_or(DialogError::InvalidBodyArea { body })?;
        let content = Rect::new(content_x, content_y, content_width, height - FRAME_ROWS);
        let body_text = Rect::new(
            content.x,
            content.y,
            content.width,
            u16::try_from(self.body.len()).map_err(|_| DialogError::BodyTooSmall { body })?,
        );
        let question_y = body_text.bottom();
        let question = Rect::new(
            content.x,
            question_y,
            content.width,
            u16::try_from(question.len()).map_err(|_| DialogError::BodyTooSmall { body })?,
        );
        let choices_y = question.bottom();
        let choices = self
            .choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let cells = u16::try_from(text_cells(choice.label()).saturating_add(2))
                    .map_err(|_| DialogError::BodyTooSmall { body })?;
                let y = choices_y
                    .checked_add(
                        u16::try_from(index).map_err(|_| DialogError::BodyTooSmall { body })?,
                    )
                    .ok_or(DialogError::InvalidBodyArea { body })?;
                Ok(DialogChoicePlacement {
                    identity: choice.identity.clone(),
                    area: Rect::new(content.x, y, cells, 1),
                })
            })
            .collect::<Result<Vec<_>, DialogError>>()?;
        Ok(DialogPlacement {
            body_area: body,
            popup,
            rail,
            content,
            body_text,
            question,
            choices,
        })
    }

    /// Returns the question text.
    #[must_use]
    pub fn question(&self) -> &str {
        &self.question
    }

    /// Returns the optional body lines.
    #[must_use]
    pub fn body(&self) -> &[String] {
        &self.body
    }

    /// Returns the named choices.
    #[must_use]
    pub fn choices(&self) -> &[DialogChoice<Id>] {
        &self.choices
    }

    /// Returns the identity of the focused choice.
    #[must_use]
    pub fn focused_identity(&self) -> &Id {
        &self.choices[self.focused].identity
    }

    /// Returns the identity of the safe default choice.
    #[must_use]
    pub fn default_identity(&self) -> &Id {
        &self.choices[self.default].identity
    }

    /// Returns the identity of the safe cancel choice.
    #[must_use]
    pub fn cancel_identity(&self) -> &Id {
        &self.choices[self.cancel].identity
    }

    /// Moves focus to the previous choice, wrapping from the first choice.
    pub fn previous(&mut self) -> DialogOutcome<Id>
    where
        Id: Clone,
    {
        self.focused = if self.focused == 0 {
            self.choices.len() - 1
        } else {
            self.focused - 1
        };
        DialogOutcome::Focused(self.focused_identity().clone())
    }

    /// Moves focus to the next choice, wrapping from the last choice.
    pub fn next(&mut self) -> DialogOutcome<Id>
    where
        Id: Clone,
    {
        self.focused = (self.focused + 1) % self.choices.len();
        DialogOutcome::Focused(self.focused_identity().clone())
    }

    /// Focuses the choice with the supplied identity.
    ///
    /// # Errors
    ///
    /// Returns a typed refusal when the identity names no choice or more than
    /// one choice.
    pub fn focus(&mut self, identity: &Id) -> Result<DialogOutcome<Id>, DialogError>
    where
        Id: Clone,
    {
        self.focused = choice_index(&self.choices, identity).map_err(role_error_choice)?;
        Ok(DialogOutcome::Focused(self.focused_identity().clone()))
    }

    /// Returns the identity reached by one direct key.
    #[must_use]
    pub fn direct_key_identity(&self, key: char) -> Option<&Id> {
        let key = key.to_ascii_lowercase();
        self.choices.iter().find_map(|choice| {
            (choice
                .direct_key
                .map(|direct_key| direct_key.to_ascii_lowercase())
                == Some(key))
            .then_some(&choice.identity)
        })
    }

    /// Answers with the focused choice.
    #[must_use]
    pub fn answer_focused(&self) -> DialogOutcome<Id>
    where
        Id: Clone,
    {
        DialogOutcome::Answered(self.focused_identity().clone())
    }

    /// Answers with the safe default choice.
    #[must_use]
    pub fn answer_default(&self) -> DialogOutcome<Id>
    where
        Id: Clone,
    {
        DialogOutcome::Answered(self.default_identity().clone())
    }

    /// Answers with the safe cancel choice.
    #[must_use]
    pub fn answer_cancel(&self) -> DialogOutcome<Id>
    where
        Id: Clone,
    {
        DialogOutcome::Answered(self.cancel_identity().clone())
    }

    /// Answers with the choice that owns a direct key.
    #[must_use]
    pub fn answer_for_direct_key(&self, key: char) -> Option<DialogOutcome<Id>>
    where
        Id: Clone,
    {
        self.direct_key_identity(key)
            .cloned()
            .map(DialogOutcome::Answered)
    }

    /// Drives one keyboard input and consumes every key while the dialog is open.
    ///
    /// Declared direct keys take precedence over movement aliases. This keeps a
    /// caller-owned `h`, `j`, `k`, `l`, `y`, or `n` direct choice reachable.
    #[must_use]
    pub fn drive_key(&mut self, key: DialogKey) -> DialogKeyOutcome<Id>
    where
        Id: Clone,
    {
        if let DialogKey::Char(character) = key {
            if let Some(outcome) = self.answer_for_direct_key(character) {
                return DialogKeyOutcome::Interaction(outcome);
            }
        }
        let outcome = match key {
            DialogKey::Char('h' | 'k') | DialogKey::Left | DialogKey::Up => Some(self.previous()),
            DialogKey::Char('j' | 'l') | DialogKey::Right | DialogKey::Down => Some(self.next()),
            DialogKey::Enter => Some(self.answer_focused()),
            DialogKey::Esc | DialogKey::CtrlC => Some(self.answer_cancel()),
            DialogKey::Char(_) | DialogKey::Unsupported => None,
        };
        match outcome {
            Some(outcome) => DialogKeyOutcome::Interaction(outcome),
            None => DialogKeyOutcome::Consumed,
        }
    }

    /// Drives one pointer input through a published placement.
    ///
    /// The placement is the sole geometry source. Every event is consumed while
    /// the dialog is open, including events outside the popup.
    #[must_use]
    pub fn drive_pointer(
        &mut self,
        event: DialogPointerEvent,
        placement: &DialogPlacement<Id>,
    ) -> DialogPointerOutcome<Id>
    where
        Id: Clone,
    {
        if !self.placement_matches(placement) {
            return DialogPointerOutcome::PlacementMismatch;
        }
        if !crate::contains_cell(placement.popup, event.cell) {
            return DialogPointerOutcome::OutsidePopup;
        }
        let Some(choice) = placement
            .choices
            .iter()
            .find(|choice| crate::contains_cell(choice.area, event.cell))
        else {
            return DialogPointerOutcome::Consumed;
        };
        match event.action {
            DialogPointerAction::Motion => DialogPointerOutcome::Interaction(
                self.focus(&choice.identity)
                    .expect("the validated placement names one dialog choice"),
            ),
            DialogPointerAction::Press(DialogPointerButton::Primary) => {
                DialogPointerOutcome::Interaction(DialogOutcome::Answered(choice.identity.clone()))
            }
            DialogPointerAction::Press(_)
            | DialogPointerAction::Release(_)
            | DialogPointerAction::Drag(_)
            | DialogPointerAction::Wheel => DialogPointerOutcome::Consumed,
        }
    }

    fn placement_matches(&self, placement: &DialogPlacement<Id>) -> bool
    where
        Id: Clone,
    {
        match self.geometry(placement.body_area) {
            Ok(expected) => expected == *placement,
            Err(_) => false,
        }
    }
}

fn rect_fits(area: Rect, buffer: Rect) -> bool {
    let Some(area_right) = area.x.checked_add(area.width) else {
        return false;
    };
    let Some(area_bottom) = area.y.checked_add(area.height) else {
        return false;
    };
    let Some(buffer_right) = buffer.x.checked_add(buffer.width) else {
        return false;
    };
    let Some(buffer_bottom) = buffer.y.checked_add(buffer.height) else {
        return false;
    };
    area.x >= buffer.x
        && area.y >= buffer.y
        && area_right <= buffer_right
        && area_bottom <= buffer_bottom
}

fn wrap(text: &str, width: usize) -> Option<Vec<String>> {
    debug_assert!(width > 0, "validated popup content has one column");
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut cells = 0;
    for character in text.chars() {
        let character_cells = UnicodeWidthChar::width(character).unwrap_or(0);
        if character_cells > width {
            return None;
        }
        if cells > 0 && cells + character_cells > width {
            lines.push(std::mem::take(&mut line));
            cells = 0;
        }
        line.push(character);
        cells += character_cells;
    }
    if line.is_empty() {
        lines.push(String::new());
    } else {
        lines.push(line);
    }
    Some(lines)
}

fn fill(target: &mut Buffer, area: Rect, symbol: &str) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = target.cell_mut((x, y)) {
                cell.set_symbol(symbol);
            }
        }
    }
}
fn check_chars(text: &str, max: usize) -> Result<(), usize> {
    let chars = text.chars().count();
    if chars > max { Err(chars) } else { Ok(()) }
}

fn choice_index<Id: Eq>(choices: &[DialogChoice<Id>], identity: &Id) -> Result<usize, bool> {
    let mut matches = choices
        .iter()
        .enumerate()
        .filter_map(|(index, choice)| (choice.identity == *identity).then_some(index));
    let Some(index) = matches.next() else {
        return Err(false);
    };
    if matches.next().is_some() {
        Err(true)
    } else {
        Ok(index)
    }
}

const fn role_error_default(ambiguous: bool) -> DialogError {
    if ambiguous {
        DialogError::AmbiguousDefault
    } else {
        DialogError::UnknownDefault
    }
}

const fn role_error_cancel(ambiguous: bool) -> DialogError {
    if ambiguous {
        DialogError::AmbiguousCancel
    } else {
        DialogError::UnknownCancel
    }
}

const fn role_error_choice(ambiguous: bool) -> DialogError {
    if ambiguous {
        DialogError::AmbiguousChoice
    } else {
        DialogError::UnknownChoice
    }
}

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod tests;
