//! The standalone kvim adapter over the one shared key resolver.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! The module owns no key table and no sequence matching. `kvim-keymap` owns
//! the shared resolver and the pending prefix, and [`SemanticReducer`] owns the
//! count, the operator, the register, the text object, and the prompt phases.
//! This adapter joins the two and reports the outcome in the shape that the
//! standalone editor consumes.
//!
//! The adapter never reads a clock. The terminal event loop measures the
//! elapsed time and supplies it with every request, so resolution stays
//! deterministic and testable.

use std::num::NonZeroU32;
use std::time::Duration;

use kvim_keymap::{
    Chord, Dispatch, Input, InputContextSnapshot, Key, KeyCode, Resolver as SharedResolver,
    TypedText,
};
use kvim_settings::InputSettings;

use super::command::Command;
use super::mode::{BindingScope, InputContext};
use super::reducer::{Reduced, Reduction, SemanticOperation, SemanticReducer};
use super::registry::{Registry, WhichKeyRow};

/// One edit of an open line prompt.
///
/// The shared registry holds the prompt keys, and this value names what each
/// one does, so the command line and the search prompt never compare a key.
///
/// The enumeration is exhaustive on purpose. Every variant demands a decision
/// from a host that draws its own prompt line, so a new variant must stop the
/// build of that host. A host that absorbed a motion into a wildcard arm would
/// drop it without a compile error, and its reader would lose the key. A new
/// variant is therefore a breaking facade change and takes the obligations that
/// `docs/architecture.md` states for one. The six cursor motions arrived
/// together in that form: a host adds one match arm for each and moves the
/// cursor of its own line by characters. See `docs/architecture.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptEdit {
    /// Write one character before the cursor of the prompt line.
    ///
    /// The cursor then steps over the written character. A line whose cursor
    /// stands at its end therefore appends, which is what every prompt did
    /// before the line held a cursor. See `docs/input-actions.md`.
    Insert(char),
    /// Remove the character before the cursor.
    DeleteBackward,
    /// Remove the word before the cursor.
    ///
    /// The edit removes the blanks before the word as well as the word, like
    /// Vim, readline, and every terminal shell. An empty line changes nothing
    /// and keeps the prompt open, so a host that binds `Ctrl-W` for its own
    /// purpose never cancels a prompt with it. See `docs/input-actions.md`.
    DeleteWordBackward,
    /// Move the cursor one character back, and stop at the start of the line.
    ///
    /// Every motion counts characters, because a character is the unit that a
    /// reader inserts and deletes. A motion at the end that it names changes
    /// nothing and never wraps to the other end. See `docs/input-actions.md`.
    CursorLeft,
    /// Move the cursor one character forward, and stop at the end of the line.
    CursorRight,
    /// Move the cursor to the start of the word before it.
    ///
    /// The motion passes the blanks before the word as well as the word, so it
    /// lands where [`PromptEdit::DeleteWordBackward`] would cut.
    CursorWordBackward,
    /// Move the cursor to the start of the word after it.
    ///
    /// The motion passes the rest of the word under the cursor and then the
    /// blanks after it. It is the return of [`PromptEdit::CursorWordBackward`],
    /// because a terminal reader presses the two arrow chords as one pair.
    CursorWordForward,
    /// Move the cursor before the first character of the line.
    CursorLineStart,
    /// Move the cursor after the last character of the line.
    CursorLineEnd,
    /// Write the next completion candidate into the prompt line.
    CompleteNext,
    /// Write the previous completion candidate into the prompt line.
    CompletePrevious,
    /// Run the prompt line.
    Accept,
    /// Cancel the prompt and restore the previous mode.
    ///
    /// An open candidate list takes this edit first and restores the text that
    /// the user typed, so a second cancel closes the prompt. See
    /// `docs/input-actions.md`.
    Cancel,
}

impl PromptEdit {
    /// Returns the edit that one resolved command names for an open prompt.
    ///
    /// The mapping belongs to this type, so a key that reaches the prompt and
    /// a host-supplied command reach the identical edit. An open picker owns
    /// its own chords above the query line, so every other command returns
    /// `None` and continues to the owners below the prompt.
    ///
    /// ```
    /// use kvim_input::{Command, PromptEdit};
    ///
    /// assert_eq!(
    ///     PromptEdit::of_command(Command::PromptAccept),
    ///     Some(PromptEdit::Accept)
    /// );
    /// assert_eq!(PromptEdit::of_command(Command::MoveDown), None);
    /// ```
    #[must_use]
    pub const fn of_command(command: Command) -> Option<Self> {
        match command {
            Command::PromptAccept => Some(Self::Accept),
            Command::PromptCancel => Some(Self::Cancel),
            Command::PromptDeleteBackward => Some(Self::DeleteBackward),
            Command::PromptDeleteWordBackward => Some(Self::DeleteWordBackward),
            Command::PromptCursorLeft => Some(Self::CursorLeft),
            Command::PromptCursorRight => Some(Self::CursorRight),
            Command::PromptCursorWordBackward => Some(Self::CursorWordBackward),
            Command::PromptCursorWordForward => Some(Self::CursorWordForward),
            Command::PromptCursorLineStart => Some(Self::CursorLineStart),
            Command::PromptCursorLineEnd => Some(Self::CursorLineEnd),
            Command::PromptCompleteNext => Some(Self::CompleteNext),
            Command::PromptCompletePrevious => Some(Self::CompletePrevious),
            _ => None,
        }
    }
}

/// One edit of the answer of an open confirmation.
///
/// The confirmation completes nothing, so this enumeration holds no completion
/// edit. The answer holds no cursor, so it holds no motion either, and the
/// prompt scope alone binds the motion keys. See `docs/input-actions.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmEdit {
    /// Append one character to the answer.
    Insert(char),
    /// Remove the character before the cursor.
    DeleteBackward,
    /// Remove the word before the cursor.
    ///
    /// The answer takes the same edit as the prompt line, because the
    /// confirmation scope binds the same keys.
    DeleteWordBackward,
    /// Read the answer and close the question.
    Accept,
    /// Cancel the action and close the question.
    Cancel,
    /// Change nothing and keep the question open.
    ///
    /// An open confirmation owns every key, so a key that its table does not
    /// hold reaches no other owner and inserts no buffer text.
    Ignore,
}

impl ConfirmEdit {
    /// Returns the edit that one resolved command names for an open question.
    ///
    /// The question completes nothing, so it names fewer edits than a prompt.
    /// A command that it does not name returns `None`, and the caller decides
    /// whether the question ignores that command or lets it pass.
    ///
    /// ```
    /// use kvim_input::{Command, ConfirmEdit};
    ///
    /// assert_eq!(
    ///     ConfirmEdit::of_command(Command::PromptCancel),
    ///     Some(ConfirmEdit::Cancel)
    /// );
    /// assert_eq!(ConfirmEdit::of_command(Command::PromptCompleteNext), None);
    /// ```
    #[must_use]
    pub const fn of_command(command: Command) -> Option<Self> {
        match command {
            Command::PromptAccept => Some(Self::Accept),
            Command::PromptCancel => Some(Self::Cancel),
            Command::PromptDeleteBackward => Some(Self::DeleteBackward),
            Command::PromptDeleteWordBackward => Some(Self::DeleteWordBackward),
            _ => None,
        }
    }
}

/// The answer to one open confirmation.
///
/// The resolver reads the keys, and this value reads the typed word, so one
/// keypress never performs the action. See `docs/input-actions.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmAnswer {
    /// Perform the action that waits for the answer.
    Yes,
    /// Cancel the action and change nothing.
    No,
}

impl ConfirmAnswer {
    /// Returns the answer that one typed text gives.
    ///
    /// The text `y` and the text `yes` confirm, in any letter case. The capital
    /// `N` of the question names the default, so every other text cancels, and
    /// an empty text cancels as well.
    ///
    /// ```
    /// use kvim_input::ConfirmAnswer;
    ///
    /// assert_eq!(ConfirmAnswer::from_text("y"), ConfirmAnswer::Yes);
    /// assert_eq!(ConfirmAnswer::from_text("YES"), ConfirmAnswer::Yes);
    /// assert_eq!(ConfirmAnswer::from_text("no"), ConfirmAnswer::No);
    /// assert_eq!(ConfirmAnswer::from_text(""), ConfirmAnswer::No);
    /// ```
    #[inline]
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        if text.eq_ignore_ascii_case("y") || text.eq_ignore_ascii_case("yes") {
            return Self::Yes;
        }
        Self::No
    }
}

/// The outcome of one resolution request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resolution {
    /// The input completed one semantic operation.
    Command {
        /// The command that the operation performs.
        command: Command,
        /// The decimal count before the operation.
        count: Option<NonZeroU32>,
        /// The register that qualifies the operation.
        register: Option<char>,
    },
    /// An open prompt owns input and the key edits its line.
    Prompt(PromptEdit),
    /// An open confirmation owns input and the key edits its answer.
    Confirmation(ConfirmEdit),
    /// The focused scope takes the key as literal text.
    ///
    /// Insert mode reaches this outcome for every printable key, and the editor
    /// inserts the character through an edit transaction.
    Text(char),
    /// A key sequence, a count, a register selection, or a text object waits
    /// for more input.
    Pending,
    /// `Esc` or `Ctrl-C` cancelled the pending input.
    Cancelled,
    /// The key reaches no command and no text owner. Pending input is reset.
    NoMatch,
}

/// The standalone modal input adapter.
///
/// The adapter holds one shared resolver and one semantic reducer. The shared
/// resolver owns the pending key prefix and the which-key overlay. The reducer
/// owns the count, the operator, the register, the text object, and the prompt
/// phases, and it publishes one [`InputContextSnapshot`] after every input.
///
/// A pending sequence holds no deadline. It waits for the next key, and only
/// `Esc`, `Ctrl-C`, a mismatch, a completed command, or a context change ends
/// it. The registry rejects a sequence that both completes a command and starts
/// a longer sequence, so no ambiguity remains for a timer to resolve.
///
/// ```
/// use std::time::Duration;
///
/// use kvim_input::{Command, Registry, Resolution, Resolver};
/// use kvim_settings::InputSettings;
/// use kvim_keymap::{Key, KeyCode};
///
/// let mut resolver = Resolver::new(Registry::first_release(), InputSettings::default());
/// let now = Duration::ZERO;
/// assert_eq!(
///     resolver.resolve(Key::plain(KeyCode::Char('g')), now),
///     Resolution::Pending
/// );
/// assert_eq!(
///     resolver.resolve(Key::plain(KeyCode::Char('g')), now),
///     Resolution::Command {
///         command: Command::MoveFirstLine,
///         count: None,
///         register: None,
///     }
/// );
/// ```
#[derive(Clone, Debug)]
pub struct Resolver {
    registry: Registry,
    shared: SharedResolver<Command, BindingScope>,
    reducer: SemanticReducer,
}

impl Resolver {
    /// Creates a resolver over one registry and one bound set.
    #[must_use]
    pub fn new(registry: Registry, settings: InputSettings) -> Self {
        debug_assert!(
            settings.pending_keys_max > 0 && settings.count_max > 0,
            "EditorSettings holds positive input bounds"
        );
        let shared = SharedResolver::new(
            registry.shared(),
            settings.pending_keys_max,
            settings.which_key_delay,
        );
        Self {
            registry,
            shared,
            reducer: SemanticReducer::new(settings),
        }
    }

    /// Returns the mapping registry.
    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Returns the current input context.
    #[must_use]
    pub const fn context(&self) -> InputContext {
        self.reducer.context()
    }

    /// Returns the context that the next resolution request carries.
    #[must_use]
    pub fn snapshot(&self) -> InputContextSnapshot<BindingScope> {
        self.reducer.snapshot()
    }

    /// Returns the keys of the pending sequence.
    #[must_use]
    pub fn pending_keys(&self) -> &[Key] {
        self.shared.pending_keys()
    }

    /// Returns the elapsed time at which the which-key overlay appears.
    ///
    /// The event loop uses the value to wake exactly when the overlay becomes
    /// visible. A visible overlay and an empty sequence both report no time,
    /// because no transition could consume it.
    #[must_use]
    pub fn overlay_deadline(&self) -> Option<Duration> {
        self.shared.overlay_deadline()
    }

    /// Moves input to another context and resets pending input.
    ///
    /// A mode change and a prompt change both reset the pending keys, every
    /// grammar phase, and the which-key overlay. An unchanged context keeps the
    /// pending sequence.
    pub fn set_context(&mut self, context: InputContext) {
        if context == self.reducer.context() {
            return;
        }
        self.reducer.set_context(context);
        self.shared.clear_pending();
    }

    /// Clears the pending keys, every grammar phase, and the which-key overlay.
    ///
    /// A reset never changes buffer text and never cancels background work.
    pub fn reset(&mut self) {
        self.shared.clear_pending();
        self.reducer.reset();
    }

    /// Returns the which-key overlay rows, or `None` while the overlay stays
    /// hidden.
    ///
    /// The rows come from the same registry and the same pending prefix that
    /// dispatch reads, so a row can never disagree with the command that its
    /// key reaches.
    ///
    /// The shared resolver hints from every scope that extends the prefix, in
    /// scope order. The standalone editor sets no host-global scope. Only its
    /// own scope and an open overlay scope can contribute, so these rows keep
    /// their present order.
    pub fn which_key(&mut self, now: Duration) -> Option<Vec<WhichKeyRow>> {
        let view = self.shared.which_key(now)?;
        Some(
            view.hints()
                .iter()
                .map(|hint| WhichKeyRow::of(hint.hint()))
                .collect(),
        )
    }

    /// Cancels every pending key and every pending grammar phase.
    ///
    /// A cancel key reaches this path, and so does the addressed cancellation
    /// effect that a workspace composer proposes before it moves focus or
    /// overlay ownership. A waiting operator reports
    /// [`Command::ReturnToNormal`], which names no motion and no text object,
    /// so it aborts the operator and changes nothing else.
    pub fn cancel(&mut self) -> Resolution {
        let scope = self.reducer.active_scope();
        self.shared.clear_pending();
        resolution(self.reducer.cancel(), scope)
    }

    /// Reports that the terminal sent input which no binding accepts.
    ///
    /// A key with an unsupported modifier and a paste block above the accepted
    /// bound both reach this path. The reducer resets the count, the operator,
    /// the register, the text object, and the prompt phase, and the shared
    /// resolver drops its pending prefix, so the rejected input never degrades
    /// into the binding of a shorter sequence.
    ///
    /// A waiting operator lives in the editor as well, so this path reports
    /// [`Command::ReturnToNormal`] for it, exactly as [`Resolver::cancel`]
    /// does. The call changes no buffer text.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use kvim_input::{Resolution, Resolver};
    /// use kvim_keymap::{Key, KeyCode};
    /// use kvim_settings::InputSettings;
    ///
    /// let mut resolver = Resolver::new(
    ///     kvim_input::Registry::first_release(),
    ///     InputSettings::default(),
    /// );
    /// // A decimal count opens one grammar prefix.
    /// assert_eq!(
    ///     resolver.resolve(Key::plain(KeyCode::Char('3')), Duration::ZERO),
    ///     Resolution::Pending
    /// );
    ///
    /// assert_eq!(resolver.unsupported(), Resolution::NoMatch);
    /// assert!(resolver.snapshot().phases.is_idle());
    /// ```
    pub fn unsupported(&mut self) -> Resolution {
        let scope = self.reducer.active_scope();
        self.shared.clear_pending();
        let reduced = self.reducer.reduce(Dispatch::Unsupported);
        if scope == BindingScope::OperatorPending {
            // A waiting operator lives in the editor too, so the abort must
            // reach it. [`Command::ReturnToNormal`] names no motion and no text
            // object, which aborts the operator and changes nothing else. The
            // cancel path above uses the same command for the same reason.
            return Resolution::Command {
                command: Command::ReturnToNormal,
                count: None,
                register: None,
            };
        }
        resolution(reduced, scope)
    }

    /// Resolves one key at the elapsed time `now`.
    ///
    /// A cancel key ends pending input first, at every depth and in every mode.
    /// Every other key reaches the shared resolver, and the semantic reducer
    /// composes the outcome.
    pub fn resolve(&mut self, key: Key, now: Duration) -> Resolution {
        if self.holds_pending_input() && is_cancel_key(key) {
            return self.cancel();
        }
        let context = self.reducer.dispatch_context();
        let scope = context.focus.scope;
        let dispatch = self.shared.dispatch(&context, Input::Key(key), Some(now));
        let reduced = self.reducer.reduce(dispatch);
        if self.reducer.holds_grammar_prefix() {
            // The reducer opened its own prefix, such as a count, so the
            // which-key delay counts from this input.
            self.shared.arm_overlay(now);
        }
        resolution(reduced, scope)
    }

    /// Reports whether a pending sequence or a pending grammar prefix waits.
    ///
    /// A waiting operator is absent here, because its own scope binds the
    /// cancel keys to [`Command::ReturnToNormal`].
    fn holds_pending_input(&self) -> bool {
        !self.shared.pending_keys().is_empty() || self.reducer.holds_grammar_prefix()
    }
}

/// Reports the reduction in the shape that the standalone editor consumes.
fn resolution(reduced: Reduced, scope: BindingScope) -> Resolution {
    match reduced.reduction {
        Reduction::Prefix => Resolution::Pending,
        Reduction::Cancelled => Resolution::Cancelled,
        // The reducer already reset every grammar phase, and no owner takes
        // input that no binding accepts, so nothing else changes.
        Reduction::Unsupported => Resolution::NoMatch,
        Reduction::Unbound => match scope {
            // An open confirmation owns every key, so an unbound key changes
            // nothing and reaches no owner below the question.
            BindingScope::Confirmation => Resolution::Confirmation(ConfirmEdit::Ignore),
            _ => Resolution::NoMatch,
        },
        Reduction::Text(TypedText::Typed(value)) => match scope {
            BindingScope::Prompt => Resolution::Prompt(PromptEdit::Insert(value)),
            BindingScope::Confirmation => Resolution::Confirmation(ConfirmEdit::Insert(value)),
            _ => Resolution::Text(value),
        },
        Reduction::Text(TypedText::Pasted(_)) => {
            debug_assert!(
                false,
                "the standalone adapter submits key input only; the editor applies one \
                 bracketed paste block itself"
            );
            Resolution::NoMatch
        }
        Reduction::Operation(operation) => operation_resolution(scope, operation),
    }
}

/// Reports one completed operation.
///
/// A prompt command and a confirmation command name an edit of the open line.
/// Every other command reaches the editor with its count and its register.
fn operation_resolution(scope: BindingScope, operation: SemanticOperation) -> Resolution {
    let edit = match scope {
        BindingScope::Prompt => PromptEdit::of_command(operation.command).map(Resolution::Prompt),
        BindingScope::Confirmation => {
            ConfirmEdit::of_command(operation.command).map(Resolution::Confirmation)
        }
        _ => None,
    };
    edit.unwrap_or(Resolution::Command {
        command: operation.command,
        count: operation.count,
        register: operation.register,
    })
}

/// Reports whether one key cancels pending input.
///
/// The reference configuration maps `<C-c>` to `<Esc>` in every mode, so both
/// keys cancel a pending sequence and both close an open prompt.
fn is_cancel_key(key: Key) -> bool {
    matches!(
        (key.chord(), key.code()),
        (Chord::Plain, KeyCode::Esc) | (Chord::Ctrl, KeyCode::Char('c'))
    )
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod tests;
