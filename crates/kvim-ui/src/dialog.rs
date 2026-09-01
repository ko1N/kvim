//! A bounded, action-agnostic dialog model.
//!
//! The module owns validation and deterministic choice focus. Layout, painting,
//! and input decoding remain separate concerns. The caller owns every choice
//! identity and maps an answer to its own action.

use ratatui::layout::Rect;
use thiserror::Error;

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
