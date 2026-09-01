//! Supported action-agnostic dialog lifecycle for editor facades.

use std::cell::RefCell;

use kvim_keymap::{
    Chord, ContextGeneration, Key, KeyCode, PointerAction, PointerButton, PointerEvent,
};
use kvim_ui::{
    Cell, Dialog, DialogChoice as UiChoice, DialogError as UiError, DialogKey as UiKey,
    DialogKeyOutcome as UiKeyOutcome, DialogOutcome as UiOutcome, DialogPlacement as UiPlacement,
    DialogPointerAction as UiPointerAction, DialogPointerButton as UiPointerButton,
    DialogPointerEvent as UiPointerEvent, DialogPointerOutcome as UiPointerOutcome,
    DialogStyles as UiStyles,
};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use thiserror::Error;

/// A bounded caller-owned choice identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DialogChoiceId(u64);

impl DialogChoiceId {
    /// Creates an identity from a caller-owned process-local value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the caller-owned value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One named choice in a facade dialog request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogChoice {
    inner: UiChoice<DialogChoiceId>,
}

impl DialogChoice {
    /// Creates a named choice without a direct key.
    #[must_use]
    pub fn new(identity: DialogChoiceId, label: impl Into<String>) -> Self {
        Self {
            inner: UiChoice::new(identity, label),
        }
    }

    /// Assigns one unique printable direct key.
    #[must_use]
    pub fn with_direct_key(mut self, key: char) -> Self {
        self.inner = self.inner.with_direct_key(key);
        self
    }
}

/// Semantic styles used to paint a facade dialog.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DialogStyles {
    /// Style applied to the supplied body before popup painting.
    pub dim: Style,
    /// Popup surface and padding style.
    pub surface: Style,
    /// Full-height rail style.
    pub rail: Style,
    /// Optional body-text style.
    pub body: Style,
    /// Question style.
    pub question: Style,
    /// Ordinary choice style.
    pub choice: Style,
    /// Unfocused safe-default style.
    pub default_choice: Style,
    /// Focused choice style.
    pub focused_choice: Style,
}

impl From<DialogStyles> for UiStyles {
    fn from(styles: DialogStyles) -> Self {
        Self {
            dim: styles.dim,
            surface: styles.surface,
            rail: styles.rail,
            body: styles.body,
            question: styles.question,
            choice: styles.choice,
            default_choice: styles.default_choice,
            focused_choice: styles.focused_choice,
        }
    }
}

/// A validated request to open one facade dialog.
///
/// The request owns bounded text, choices, identities, styles, and the body
/// rectangle that the dialog may dim and occupy.
///
/// ```
/// use kvim_embed::{DialogChoice, DialogChoiceId, DialogRequest, DialogStyles};
/// use ratatui::layout::Rect;
///
/// let keep = DialogChoiceId::new(1);
/// let discard = DialogChoiceId::new(2);
/// let request = DialogRequest::new(
///     "Discard changes?",
///     ["The buffer has unsaved text."],
///     [
///         DialogChoice::new(keep, "Keep editing").with_direct_key('n'),
///         DialogChoice::new(discard, "Discard").with_direct_key('y'),
///     ],
///     keep,
///     keep,
///     Rect::new(0, 0, 40, 10),
///     DialogStyles::default(),
/// )?;
/// assert_eq!(request.body_area(), Rect::new(0, 0, 40, 10));
/// # Ok::<(), kvim_embed::DialogOpenError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogRequest {
    dialog: Dialog<DialogChoiceId>,
    body_area: Rect,
    styles: DialogStyles,
}

impl DialogRequest {
    /// Validates all dialog content and geometry before opening live state.
    pub fn new<B, C>(
        question: impl Into<String>,
        body: B,
        choices: C,
        default: DialogChoiceId,
        cancel: DialogChoiceId,
        body_area: Rect,
        styles: DialogStyles,
    ) -> Result<Self, DialogOpenError>
    where
        B: IntoIterator,
        B::Item: Into<String>,
        C: IntoIterator<Item = DialogChoice>,
    {
        let dialog = Dialog::new(
            question,
            body,
            choices.into_iter().map(|choice| choice.inner),
            default,
            cancel,
        )?;
        dialog.placement_for(body_area)?;
        Ok(Self {
            dialog,
            body_area,
            styles,
        })
    }

    /// Returns the owner-supplied body rectangle.
    #[must_use]
    pub const fn body_area(&self) -> Rect {
        self.body_area
    }
}

/// A typed refusal to open a facade dialog.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DialogOpenError {
    /// Another dialog is open.
    #[error("one facade dialog is already open")]
    AlreadyOpen,
    /// An answer event must be drained before another dialog opens.
    #[error("the previous facade dialog answer has not been drained")]
    AnswerPending,
    /// Content or geometry failed the shared `kvim-ui` policy.
    #[error(transparent)]
    Invalid(#[from] UiError),
}

/// One facade dialog input. Every variant is consumed while a dialog is open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogInput {
    /// One terminal-neutral key.
    Key(Key),
    /// One paste event. Its contents are intentionally irrelevant to a dialog.
    Paste,
    /// One terminal-neutral pointer event.
    Pointer(PointerEvent),
    /// Any unsupported physical input.
    Unsupported,
}

/// The result of one dialog input transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogInputOutcome {
    /// No dialog was open, so the editor may process the input.
    NotOpen,
    /// The open dialog consumed the input without a visible change.
    Consumed,
    /// Focus changed and the host must redraw.
    Redraw,
    /// The dialog closed and queued exactly one answer event.
    Answered,
}

/// The answer returned by one closed dialog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialogAnswer {
    /// Caller-owned identity selected by key, cancellation, or pointer.
    pub choice: DialogChoiceId,
}

/// The exact painted rectangle of one choice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialogChoicePlacement {
    /// Caller-owned identity of the choice.
    pub identity: DialogChoiceId,
    /// Exact painted cells of the visible choice.
    pub area: Rect,
}

/// Exact geometry published by the latest current render.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogPlacement {
    /// Owner-supplied body rectangle.
    pub body_area: Rect,
    /// Complete popup rectangle.
    pub popup: Rect,
    /// Full-height rail rectangle.
    pub rail: Rect,
    /// Popup content rectangle.
    pub content: Rect,
    /// Optional body-text rectangle.
    pub body_text: Rect,
    /// Wrapped-question rectangle.
    pub question: Rect,
    /// Exact visible choice rectangles.
    pub choices: Vec<DialogChoicePlacement>,
}

impl From<UiPlacement<DialogChoiceId>> for DialogPlacement {
    fn from(value: UiPlacement<DialogChoiceId>) -> Self {
        Self {
            body_area: value.body_area,
            popup: value.popup,
            rail: value.rail,
            content: value.content,
            body_text: value.body_text,
            question: value.question,
            choices: value
                .choices
                .into_iter()
                .map(|choice| DialogChoicePlacement {
                    identity: choice.identity,
                    area: choice.area,
                })
                .collect(),
        }
    }
}

/// Current dialog state for accessibility and host hit-testing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogSnapshot {
    generation: ContextGeneration,
    focused: DialogChoiceId,
    placement: Option<DialogPlacement>,
}

impl DialogSnapshot {
    /// Returns the generation of this state and placement.
    #[must_use]
    pub const fn generation(&self) -> ContextGeneration {
        self.generation
    }

    /// Returns the currently focused caller-owned choice.
    #[must_use]
    pub const fn focused(&self) -> DialogChoiceId {
        self.focused
    }

    /// Returns exact geometry only after the current generation was rendered.
    #[must_use]
    pub fn placement(&self) -> Option<&DialogPlacement> {
        self.placement.as_ref()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DialogHost {
    open: Option<OpenDialog>,
    answer: Option<DialogAnswer>,
    generation: ContextGeneration,
}

#[derive(Clone, Debug)]
struct OpenDialog {
    dialog: Dialog<DialogChoiceId>,
    body_area: Rect,
    styles: DialogStyles,
    placement: RefCell<Option<UiPlacement<DialogChoiceId>>>,
}

impl DialogHost {
    pub(crate) const fn new() -> Self {
        Self {
            open: None,
            answer: None,
            generation: ContextGeneration::FIRST,
        }
    }

    pub(crate) fn open(&mut self, request: DialogRequest) -> Result<(), DialogOpenError> {
        if self.open.is_some() {
            return Err(DialogOpenError::AlreadyOpen);
        }
        if self.answer.is_some() {
            return Err(DialogOpenError::AnswerPending);
        }
        self.advance();
        self.open = Some(OpenDialog {
            dialog: request.dialog,
            body_area: request.body_area,
            styles: request.styles,
            placement: RefCell::new(None),
        });
        Ok(())
    }

    pub(crate) fn close(&mut self) -> bool {
        if self.open.take().is_none() {
            return false;
        }
        self.advance();
        true
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    #[cfg(feature = "worktree")]
    pub(crate) const fn generation(&self) -> ContextGeneration {
        self.generation
    }

    pub(crate) fn invalidate(&mut self) {
        if let Some(open) = &mut self.open {
            *open.placement.get_mut() = None;
            self.advance();
        }
    }

    pub(crate) fn snapshot(&self) -> Option<DialogSnapshot> {
        self.open.as_ref().map(|open| DialogSnapshot {
            generation: self.generation,
            focused: *open.dialog.focused_identity(),
            placement: open.placement.borrow().clone().map(Into::into),
        })
    }

    pub(crate) fn body_fits(&self, accepted: Rect) -> bool {
        self.open
            .as_ref()
            .is_none_or(|open| rect_contains(accepted, open.body_area))
    }

    pub(crate) fn render(&self, target: &mut Buffer) -> Result<(), UiError> {
        let Some(open) = &self.open else {
            return Ok(());
        };
        *open.placement.borrow_mut() = None;
        let placement = open
            .dialog
            .render(target, open.body_area, open.styles.into())?;
        *open.placement.borrow_mut() = Some(placement);
        Ok(())
    }

    pub(crate) fn input(&mut self, input: DialogInput) -> DialogInputOutcome {
        let Some(open) = &mut self.open else {
            return DialogInputOutcome::NotOpen;
        };
        let outcome = match input {
            DialogInput::Key(key) => match open.dialog.drive_key(map_key(key)) {
                UiKeyOutcome::Interaction(outcome) => Some(outcome),
                UiKeyOutcome::Consumed => return DialogInputOutcome::Consumed,
            },
            DialogInput::Pointer(pointer) => {
                let placement = open.placement.borrow();
                let Some(placement) = placement.as_ref() else {
                    return DialogInputOutcome::Consumed;
                };
                match open.dialog.drive_pointer(map_pointer(pointer), placement) {
                    UiPointerOutcome::Interaction(outcome) => Some(outcome),
                    UiPointerOutcome::Consumed
                    | UiPointerOutcome::OutsidePopup
                    | UiPointerOutcome::PlacementMismatch => return DialogInputOutcome::Consumed,
                }
            }
            DialogInput::Paste | DialogInput::Unsupported => return DialogInputOutcome::Consumed,
        };
        match outcome.expect("interaction paths produce one outcome") {
            UiOutcome::Focused(_) => {
                *open.placement.get_mut() = None;
                self.advance();
                DialogInputOutcome::Redraw
            }
            UiOutcome::Answered(choice) => {
                self.answer = Some(DialogAnswer { choice });
                self.open = None;
                self.advance();
                DialogInputOutcome::Answered
            }
        }
    }

    pub(crate) fn take_answer(&mut self) -> Option<DialogAnswer> {
        self.answer.take()
    }

    fn advance(&mut self) {
        self.generation = self.generation.advanced();
    }
}

pub(crate) fn validate_dialog_body(
    request: &DialogRequest,
    accepted: Rect,
) -> Result<(), DialogOpenError> {
    let body = request.body_area;
    if !rect_contains(accepted, body) {
        return Err(DialogOpenError::Invalid(UiError::TargetArea {
            body,
            buffer: accepted,
        }));
    }
    Ok(())
}

fn rect_contains(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

fn map_key(key: Key) -> UiKey {
    match (key.chord(), key.code()) {
        (Chord::Plain, KeyCode::Char(character)) => UiKey::Char(character),
        (Chord::Plain, KeyCode::Left) => UiKey::Left,
        (Chord::Plain, KeyCode::Right) => UiKey::Right,
        (Chord::Plain, KeyCode::Up) => UiKey::Up,
        (Chord::Plain, KeyCode::Down) => UiKey::Down,
        (Chord::Plain, KeyCode::Enter) => UiKey::Enter,
        (Chord::Plain, KeyCode::Esc) => UiKey::Esc,
        (Chord::Ctrl, KeyCode::Char('c')) => UiKey::CtrlC,
        _ => UiKey::Unsupported,
    }
}

fn map_pointer(pointer: PointerEvent) -> UiPointerEvent {
    let position = pointer.position();
    let button = |button| match button {
        PointerButton::Left => UiPointerButton::Primary,
        PointerButton::Right => UiPointerButton::Secondary,
        PointerButton::Middle => UiPointerButton::Middle,
    };
    let action = match pointer.action() {
        PointerAction::Motion => UiPointerAction::Motion,
        PointerAction::Press(value) => UiPointerAction::Press(button(value)),
        PointerAction::Release(value) => UiPointerAction::Release(button(value)),
        PointerAction::Drag(value) => UiPointerAction::Drag(button(value)),
        PointerAction::Wheel(_) => UiPointerAction::Wheel,
    };
    UiPointerEvent {
        cell: Cell::new(position.column(), position.row()),
        action,
    }
}

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod tests;
