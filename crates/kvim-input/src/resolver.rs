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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptEdit {
    /// Append one character to the prompt line.
    Insert(char),
    /// Remove the character before the cursor.
    DeleteBackward,
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
            Command::PromptCompleteNext => Some(Self::CompleteNext),
            Command::PromptCompletePrevious => Some(Self::CompletePrevious),
            _ => None,
        }
    }
}

/// One edit of the answer of an open confirmation.
///
/// The confirmation completes nothing, so this enumeration holds no completion
/// edit. See `docs/input-actions.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmEdit {
    /// Append one character to the answer.
    Insert(char),
    /// Remove the character before the cursor.
    DeleteBackward,
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
    pub fn which_key(&mut self, now: Duration) -> Option<Vec<WhichKeyRow>> {
        let view = self.shared.which_key(now)?;
        Some(view.hints().iter().map(WhichKeyRow::of).collect())
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
mod tests {
    use std::num::NonZeroU32;
    use std::time::Duration;

    use crate::{BindingScope, Command, InputContext, Mode, PromptKind, Registry};
    use kvim_keymap::{Key, KeyCode};
    use kvim_settings::{InputSettings, WHICH_KEY_DELAY_DEFAULT};

    use super::{ConfirmAnswer, ConfirmEdit, PromptEdit, Resolution, Resolver};

    const NOW: Duration = Duration::ZERO;

    /// The which-key delay of the settings that every test resolver holds.
    const WHICH_KEY_DELAY: Duration = WHICH_KEY_DELAY_DEFAULT;

    fn resolver() -> Resolver {
        Resolver::new(Registry::first_release(), InputSettings::default())
    }

    fn ch(value: char) -> Key {
        Key::plain(KeyCode::Char(value))
    }

    fn feed(resolver: &mut Resolver, keys: &[Key]) -> Resolution {
        let mut last = Resolution::NoMatch;
        for &key in keys {
            last = resolver.resolve(key, NOW);
        }
        last
    }

    fn command(command: Command) -> Resolution {
        Resolution::Command {
            command,
            count: None,
            register: None,
        }
    }

    fn counted(command: Command, count: u32) -> Resolution {
        Resolution::Command {
            command,
            count: NonZeroU32::new(count),
            register: None,
        }
    }

    #[test]
    fn every_first_release_mapping_resolves_to_its_command() {
        let registry = Registry::first_release();
        for mode in Mode::ALL {
            for (keys, expected) in registry.bindings(mode) {
                let mut resolver = resolver();
                resolver.set_context(InputContext::Mode(mode));
                assert_eq!(resolver.context().scope(), BindingScope::Mode(mode));
                // A count digit and a register selection are grammar prefixes,
                // so they complete no operation of their own.
                let outcome =
                    if expected.count_digit().is_some() || expected == Command::SelectRegister {
                        Resolution::Pending
                    } else {
                        command(expected)
                    };
                assert_eq!(
                    feed(&mut resolver, keys.keys()),
                    outcome,
                    "{mode} `{keys}` must reach `{expected}`"
                );
                assert!(
                    resolver.pending_keys().is_empty(),
                    "a completed command resets pending input"
                );
            }
        }
    }

    #[test]
    fn a_decimal_count_precedes_the_command() {
        let cases = [
            (vec![ch('5'), ch('j')], counted(Command::MoveDown, 5)),
            (
                vec![ch('1'), ch('0'), ch('j')],
                counted(Command::MoveDown, 10),
            ),
            (
                vec![ch('9'), ch('9'), ch('9'), ch('9'), ch('G')],
                counted(Command::MoveLastLine, 9_999),
            ),
            // One digit above the maximum is a mismatch that resets pending input.
            (
                vec![ch('9'), ch('9'), ch('9'), ch('9'), ch('9')],
                Resolution::NoMatch,
            ),
            // `0` is the first-column motion until a count is already open.
            (vec![ch('0')], command(Command::MoveFirstColumn)),
        ];
        for (keys, expected) in cases {
            let mut resolver = resolver();
            assert_eq!(feed(&mut resolver, &keys), expected);
        }
    }

    #[test]
    fn an_arrow_motion_takes_a_count_only_where_the_mode_holds_one() {
        let right = Key::plain(KeyCode::Right);
        let word_right = Key::ctrl(KeyCode::Right);
        for mode in Mode::ALL {
            let mut resolver = resolver();
            resolver.set_context(InputContext::Mode(mode));
            let expected = if mode.accepts_count() {
                counted(Command::MoveRight, 3)
            } else {
                // Insert mode holds no count, so the digit becomes buffer text
                // and the arrow moves one column.
                command(Command::MoveRight)
            };
            assert_eq!(
                feed(&mut resolver, &[ch('3'), right]),
                expected,
                "a counted arrow in {mode}"
            );
            assert_eq!(
                resolver.resolve(word_right, NOW),
                command(Command::MoveNextWordStart),
                "the word chord in {mode}"
            );
        }
    }

    #[test]
    fn a_count_belongs_to_normal_mode_and_the_visual_modes() {
        for mode in Mode::ALL {
            let mut resolver = resolver();
            resolver.set_context(InputContext::Mode(mode));
            let expected = if mode.accepts_count() {
                Resolution::Pending
            } else {
                // Insert mode holds no count, so the digit reaches no command
                // and the text fallback of the scope types it.
                Resolution::Text('5')
            };
            assert_eq!(
                resolver.resolve(ch('5'), NOW),
                expected,
                "a digit in {mode}"
            );
            assert!(
                resolver.pending_keys().is_empty(),
                "a count key is no sequence key in {mode}"
            );
        }
    }

    #[test]
    fn a_count_above_the_maximum_resets_pending_input() {
        let mut resolver = resolver();
        for key in [ch('9'), ch('9'), ch('9'), ch('9')] {
            assert_eq!(resolver.resolve(key, NOW), Resolution::Pending);
        }
        assert_eq!(resolver.resolve(ch('9'), NOW), Resolution::NoMatch);
        assert_eq!(resolver.overlay_deadline(), None);
        assert_eq!(resolver.resolve(ch('j'), NOW), command(Command::MoveDown));
    }

    #[test]
    fn a_valid_prefix_stays_pending() {
        let cases = [
            vec![ch('g')],
            vec![ch('z')],
            vec![ch(' ')],
            vec![ch(' '), ch('f')],
            vec![ch(' '), ch('c')],
            vec![ch(']')],
        ];
        for keys in cases {
            let mut resolver = resolver();
            assert_eq!(
                feed(&mut resolver, &keys),
                Resolution::Pending,
                "{keys:?} is a valid prefix"
            );
            assert_eq!(resolver.pending_keys(), keys.as_slice());
            assert!(resolver.overlay_deadline().is_some());
        }
    }

    #[test]
    fn an_unknown_sequence_resets_pending_input() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        assert_eq!(resolver.resolve(ch('q'), NOW), Resolution::NoMatch);
        assert!(resolver.pending_keys().is_empty());
        assert_eq!(resolver.overlay_deadline(), None);
    }

    #[test]
    fn a_pending_sequence_waits_for_the_next_key_without_a_deadline() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        // The user reads the overlay for an hour and the sequence survives.
        let late = Duration::from_secs(3_600);
        assert_eq!(resolver.pending_keys(), [ch('g')]);
        assert!(resolver.which_key(late).is_some());
        assert_eq!(
            resolver.resolve(ch('g'), late),
            command(Command::MoveFirstLine),
            "no deadline abandons the sequence, so the late key completes it"
        );
    }

    /// Returns the two keys that cancel pending input.
    fn cancel_keys() -> [(&'static str, Key); 2] {
        [
            ("Esc", Key::plain(KeyCode::Esc)),
            ("Ctrl-C", Key::ctrl(KeyCode::Char('c'))),
        ]
    }

    #[test]
    fn a_cancel_key_ends_pending_input_in_every_mode_and_at_every_depth() {
        // Insert mode reaches no sequence and holds no count, so it never holds
        // pending input. Every other mode reaches one of these states.
        let cases: [(Mode, &[Key]); 5] = [
            (Mode::Normal, &[ch('3')]),
            (Mode::Normal, &[ch(' ')]),
            (Mode::Normal, &[ch(' '), ch('f')]),
            (Mode::Visual, &[ch(' ')]),
            (Mode::VisualBlock, &[ch('3')]),
        ];
        for (name, cancel) in cancel_keys() {
            for (mode, keys) in cases {
                let mut resolver = resolver();
                resolver.set_context(InputContext::Mode(mode));
                assert_eq!(
                    feed(&mut resolver, keys),
                    Resolution::Pending,
                    "{mode} `{keys:?}` must stay pending"
                );
                assert_eq!(
                    resolver.resolve(cancel, NOW),
                    Resolution::Cancelled,
                    "{name} must cancel `{keys:?}` in {mode}"
                );
                assert!(resolver.pending_keys().is_empty());
                assert!(resolver.which_key(Duration::from_secs(1)).is_none());
                assert_eq!(resolver.overlay_deadline(), None);
            }
        }
    }

    #[test]
    fn a_cancel_key_without_pending_input_reaches_the_registry() {
        for (name, cancel) in cancel_keys() {
            for mode in [
                Mode::Insert,
                Mode::Visual,
                Mode::VisualLine,
                Mode::VisualBlock,
            ] {
                let mut resolver = resolver();
                resolver.set_context(InputContext::Mode(mode));
                assert_eq!(
                    resolver.resolve(cancel, NOW),
                    command(Command::ReturnToNormal),
                    "{name} returns {mode} to Normal mode"
                );
            }
        }
    }

    #[test]
    fn an_operator_moves_the_keys_into_the_operator_pending_scope() {
        let mut resolver = resolver();
        assert_eq!(
            resolver.resolve(ch('d'), NOW),
            Resolution::Command {
                command: Command::DeleteOverMotion,
                count: None,
                register: None,
            }
        );
        // `i` reaches Insert mode in Normal mode, and a text object here.
        assert_eq!(resolver.resolve(ch('i'), NOW), Resolution::Pending);
        assert_eq!(
            resolver.resolve(ch(')'), NOW),
            Resolution::Command {
                command: Command::SelectInnerParen,
                count: None,
                register: None,
            }
        );
        // The completed command closes the scope, so `i` inserts again.
        assert_eq!(
            resolver.resolve(ch('i'), NOW),
            Resolution::Command {
                command: Command::InsertBeforeCursor,
                count: None,
                register: None,
            }
        );
    }

    #[test]
    fn a_repeated_operator_key_closes_the_operator_pending_scope() {
        let mut resolver = resolver();
        resolver.resolve(ch('d'), NOW);
        assert_eq!(
            resolver.resolve(ch('d'), NOW),
            Resolution::Command {
                command: Command::DeleteOverMotion,
                count: None,
                register: None,
            }
        );
        assert_eq!(
            resolver.resolve(ch('i'), NOW),
            Resolution::Command {
                command: Command::InsertBeforeCursor,
                count: None,
                register: None,
            }
        );
    }

    #[test]
    fn a_count_still_reaches_a_waiting_operator() {
        let mut resolver = resolver();
        resolver.resolve(ch('d'), NOW);
        assert_eq!(resolver.resolve(ch('2'), NOW), Resolution::Pending);
        assert_eq!(
            resolver.resolve(ch('w'), NOW),
            Resolution::Command {
                command: Command::MoveNextWordStart,
                count: NonZeroU32::new(2),
                register: None,
            }
        );
    }

    /// The key sequences that reach one end of the line, or the matching
    /// bracket, with the command that each one names.
    ///
    /// `_` and `^` share a command, and so do `Home` with `0` and `End` with
    /// `$`, because the two keys of each pair name the same target. Only `g_`
    /// and `%` name a target that no other key reaches.
    fn line_and_bracket_keys() -> Vec<(Vec<Key>, Command)> {
        vec![
            (vec![ch('0')], Command::MoveFirstColumn),
            (vec![Key::plain(KeyCode::Home)], Command::MoveFirstColumn),
            (vec![ch('^')], Command::MoveFirstNonBlank),
            (vec![ch('_')], Command::MoveFirstNonBlank),
            (vec![ch('$')], Command::MoveLineEnd),
            (vec![Key::plain(KeyCode::End)], Command::MoveLineEnd),
            (vec![ch('g'), ch('_')], Command::MoveLastNonBlank),
            (vec![ch('%')], Command::MoveMatchingBracket),
        ]
    }

    #[test]
    fn every_line_and_bracket_key_reaches_its_motion() {
        for (keys, expected) in line_and_bracket_keys() {
            let mut resolver = resolver();
            assert_eq!(
                feed(&mut resolver, &keys),
                command(expected),
                "Normal mode must reach `{expected}`"
            );
        }
    }

    #[test]
    fn every_line_and_bracket_key_reaches_a_waiting_operator() {
        for (keys, expected) in line_and_bracket_keys() {
            let mut resolver = resolver();
            // The operator moves the keys into the operator-pending scope, so a
            // key that scope does not hold would reach no command at all.
            resolver.resolve(ch('d'), NOW);
            assert_eq!(
                feed(&mut resolver, &keys),
                command(expected),
                "a waiting operator must reach `{expected}`"
            );
        }
    }

    #[test]
    fn a_cancel_key_ends_a_waiting_operator() {
        let escape = Key::plain(KeyCode::Esc);
        let mut alone = resolver();
        alone.resolve(ch('d'), NOW);
        assert_eq!(
            alone.resolve(escape, NOW),
            Resolution::Command {
                command: Command::ReturnToNormal,
                count: None,
                register: None,
            },
            "the editor aborts its operator over a command that names no target"
        );

        // A half-typed object cancels the pending keys and the operator at once.
        let mut half = resolver();
        half.resolve(ch('d'), NOW);
        half.resolve(ch('i'), NOW);
        assert_eq!(
            half.resolve(escape, NOW),
            Resolution::Command {
                command: Command::ReturnToNormal,
                count: None,
                register: None,
            }
        );
        assert_eq!(
            half.resolve(ch('i'), NOW),
            Resolution::Command {
                command: Command::InsertBeforeCursor,
                count: None,
                register: None,
            }
        );
    }

    #[test]
    fn a_mode_change_clears_pending_input() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        resolver.set_context(InputContext::Mode(Mode::Visual));
        assert!(resolver.pending_keys().is_empty());
        assert_eq!(resolver.overlay_deadline(), None);
        assert_eq!(
            resolver.resolve(ch('d'), NOW),
            command(Command::DeleteSelection)
        );
    }

    #[test]
    fn an_unchanged_context_keeps_pending_input() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        resolver.set_context(InputContext::NORMAL);
        assert_eq!(resolver.pending_keys(), [ch('g')]);
    }

    #[test]
    fn the_which_key_overlay_appears_after_the_delay_and_lists_next_keys() {
        let mut resolver = resolver();
        assert!(resolver.which_key(NOW).is_none(), "no sequence is pending");
        assert_eq!(resolver.resolve(ch(' '), NOW), Resolution::Pending);
        let before = WHICH_KEY_DELAY - Duration::from_millis(1);
        assert!(resolver.which_key(before).is_none());
        let rows = resolver
            .which_key(WHICH_KEY_DELAY)
            .expect("the overlay appears after the delay");
        let listed = rows
            .iter()
            .map(|row| (row.key_label().to_string(), row.target.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            vec![
                ("/".to_owned(), "Toggle the comment".to_owned()),
                (
                    "\\".to_owned(),
                    "Split the window with the inverse adaptive rule".to_owned()
                ),
                (
                    "c".to_owned(),
                    "Toggle format-on-save for the active buffer".to_owned()
                ),
                ("e".to_owned(), "Show the diagnostic float".to_owned()),
                ("f".to_owned(), "+3 commands".to_owned()),
                ("k".to_owned(), "Show hover information".to_owned()),
                ("o".to_owned(), "Open the buffer picker".to_owned()),
                ("q".to_owned(), "Close the focused window".to_owned()),
                ("x".to_owned(), "Unload the active buffer".to_owned()),
                (
                    "Enter".to_owned(),
                    "Split the window with the adaptive rule".to_owned()
                ),
            ]
        );

        // One more key moves the overlay one level down.
        assert_eq!(
            resolver.resolve(ch('f'), WHICH_KEY_DELAY),
            Resolution::Pending
        );
        let rows = resolver
            .which_key(WHICH_KEY_DELAY * 2)
            .expect("the overlay stays open while the sequence is pending");
        let listed = rows
            .iter()
            .map(|row| (row.key_label().to_string(), row.target.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            vec![
                ("/".to_owned(), "Open the ripgrep search picker".to_owned()),
                ("b".to_owned(), "Open the buffer picker".to_owned()),
                ("f".to_owned(), "Open the file search picker".to_owned()),
            ]
        );
    }

    #[test]
    fn the_overlay_stays_visible_for_the_rest_of_the_sequence() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch(' '), NOW), Resolution::Pending);
        assert!(
            resolver.which_key(NOW).is_none(),
            "the first appearance waits the complete delay"
        );
        assert!(resolver.which_key(WHICH_KEY_DELAY).is_some());

        // The next key of the same sequence updates the rows at the same
        // instant, with no second wait.
        assert_eq!(
            resolver.resolve(ch('f'), WHICH_KEY_DELAY),
            Resolution::Pending
        );
        let rows = resolver
            .which_key(WHICH_KEY_DELAY)
            .expect("a visible overlay stays visible while the sequence continues");
        assert_eq!(
            rows.iter()
                .map(|row| row.key_label().to_string())
                .collect::<Vec<_>>(),
            vec!["/".to_owned(), "b".to_owned(), "f".to_owned()],
            "the deeper level replaces the rows"
        );
        assert_eq!(
            resolver.overlay_deadline(),
            None,
            "a visible overlay needs no further wake"
        );
    }

    #[test]
    fn the_overlay_hides_after_a_command_a_cancel_and_a_reset() {
        // Every case opens the overlay first, so each assertion measures the
        // hide alone.
        let mut completed = resolver();
        assert_eq!(completed.resolve(ch(' '), NOW), Resolution::Pending);
        assert!(completed.which_key(WHICH_KEY_DELAY).is_some());
        assert_eq!(
            completed.resolve(ch('q'), WHICH_KEY_DELAY),
            command(Command::CloseWindow)
        );
        assert!(
            completed.which_key(WHICH_KEY_DELAY).is_none(),
            "a completed command hides the overlay"
        );

        let mut cancelled = resolver();
        assert_eq!(cancelled.resolve(ch(' '), NOW), Resolution::Pending);
        assert!(cancelled.which_key(WHICH_KEY_DELAY).is_some());
        assert_eq!(
            cancelled.resolve(Key::plain(KeyCode::Esc), WHICH_KEY_DELAY),
            Resolution::Cancelled
        );
        assert!(
            cancelled.which_key(WHICH_KEY_DELAY).is_none(),
            "a cancel hides the overlay"
        );

        let mut changed = resolver();
        assert_eq!(changed.resolve(ch(' '), NOW), Resolution::Pending);
        assert!(changed.which_key(WHICH_KEY_DELAY).is_some());
        changed.set_context(InputContext::Mode(Mode::Visual));
        assert!(
            changed.which_key(WHICH_KEY_DELAY).is_none(),
            "a mode change resets the pending sequence and hides the overlay"
        );
    }

    #[test]
    fn a_pending_count_alone_shows_no_overlay() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('3'), NOW), Resolution::Pending);

        assert!(
            resolver.which_key(WHICH_KEY_DELAY).is_none(),
            "the rows list the keys that follow a sequence, and none is pending"
        );
        // The count already armed the overlay time, so the first sequence key
        // after the delay shows the rows at once.
        assert_eq!(
            resolver.resolve(ch(' '), WHICH_KEY_DELAY),
            Resolution::Pending
        );
        assert!(resolver.which_key(WHICH_KEY_DELAY).is_some());
    }

    #[test]
    fn the_sidebar_scope_resolves_its_own_keys() {
        let mut resolver = resolver();
        resolver.set_context(InputContext::Sidebar);

        assert_eq!(
            resolver.resolve(ch(' '), NOW),
            command(Command::TreeToggleEntry),
            "the sidebar holds no leader sequence, so Space acts at once"
        );
        assert_eq!(
            feed(&mut resolver, &[ch('3'), ch('j')]),
            counted(Command::MoveDown, 3),
            "the sidebar moves with the buffer navigation keys, so it reads a count"
        );
        assert_eq!(
            feed(&mut resolver, &[ch('g'), ch('g')]),
            command(Command::MoveFirstLine),
            "the sidebar owns the `gg` sequence as the buffer does"
        );
    }

    #[test]
    fn a_confirmation_context_takes_every_key_as_an_answer_edit() {
        let cases = [
            (ch('y'), ConfirmEdit::Insert('y')),
            // The capital letters reach the answer as well, because the answer
            // compares them without case.
            (ch('Y'), ConfirmEdit::Insert('Y')),
            (ch('n'), ConfirmEdit::Insert('n')),
            (Key::plain(KeyCode::Backspace), ConfirmEdit::DeleteBackward),
            (Key::plain(KeyCode::Enter), ConfirmEdit::Accept),
            (Key::plain(KeyCode::Esc), ConfirmEdit::Cancel),
            (Key::ctrl(KeyCode::Char('c')), ConfirmEdit::Cancel),
            // The confirmation completes nothing, so both completion keys
            // change nothing.
            (Key::plain(KeyCode::Tab), ConfirmEdit::Ignore),
            (Key::plain(KeyCode::BackTab), ConfirmEdit::Ignore),
            (Key::ctrl(KeyCode::Char('y')), ConfirmEdit::Ignore),
        ];
        for (key, edit) in cases {
            let mut resolver = resolver();
            resolver.set_context(InputContext::NORMAL.open_confirmation());
            assert_eq!(
                resolver.resolve(key, NOW),
                Resolution::Confirmation(edit),
                "{key:?} in a confirmation"
            );
        }
    }

    #[test]
    fn the_accepted_answers_are_y_and_yes_in_every_letter_case() {
        for text in ["y", "Y", "yes", "Yes", "YES", "yEs"] {
            assert_eq!(
                ConfirmAnswer::from_text(text),
                ConfirmAnswer::Yes,
                "{text} performs the action"
            );
        }
    }

    #[test]
    fn every_other_answer_cancels_the_action() {
        // The empty text is the default that the capital `N` of the question
        // names. The blank cases prove that the answer takes the text exactly
        // as the user typed it.
        for text in ["", "n", "N", "no", "ya", "yess", " y", "y ", "yes!"] {
            assert_eq!(
                ConfirmAnswer::from_text(text),
                ConfirmAnswer::No,
                "{text:?} cancels the action"
            );
        }
    }

    #[test]
    fn a_closed_confirmation_takes_no_key_of_a_mode_or_of_an_operator() {
        let mut resolver = resolver();
        assert_eq!(
            resolver.resolve(ch('y'), NOW),
            command(Command::YankOverMotion),
            "`y` reaches the yank operator while no confirmation is open"
        );
        // The operator waits for its target, so the next key belongs to it.
        assert_eq!(
            resolver.resolve(ch('y'), NOW),
            command(Command::YankOverMotion),
            "the waiting operator keeps the repeated key"
        );
        assert_eq!(
            feed(&mut resolver, &[ch('2'), ch('n')]),
            counted(Command::SearchNext, 2),
            "a pending count keeps its key too"
        );
    }

    #[test]
    fn a_prompt_context_edits_the_line_instead_of_the_registry() {
        let cases = [
            (ch('w'), Some(PromptEdit::Insert('w'))),
            (
                Key::plain(KeyCode::Backspace),
                Some(PromptEdit::DeleteBackward),
            ),
            (Key::plain(KeyCode::Tab), Some(PromptEdit::CompleteNext)),
            (
                Key::plain(KeyCode::BackTab),
                Some(PromptEdit::CompletePrevious),
            ),
            (Key::plain(KeyCode::Enter), Some(PromptEdit::Accept)),
            (Key::plain(KeyCode::Esc), Some(PromptEdit::Cancel)),
            (Key::ctrl(KeyCode::Char('c')), Some(PromptEdit::Cancel)),
            (Key::ctrl(KeyCode::Char('d')), None),
        ];
        for (key, expected) in cases {
            let mut resolver = resolver();
            resolver.set_context(InputContext::NORMAL.open_prompt(PromptKind::CommandLine));
            let expected = expected.map_or(Resolution::NoMatch, Resolution::Prompt);
            assert_eq!(resolver.resolve(key, NOW), expected, "{key:?} in a prompt");
        }
    }

    #[test]
    fn a_pending_count_reports_no_overlay_deadline() {
        // A time that no transition can consume would wake the event loop
        // forever, so the resolver reports none while the sequence holds no key.
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('5'), NOW), Resolution::Pending);
        assert_eq!(
            resolver.overlay_deadline(),
            None,
            "a pending count alone shows no overlay"
        );
        assert_eq!(
            resolver.which_key(WHICH_KEY_DELAY),
            None,
            "the overlay stays hidden, so the deadline must stay absent"
        );
        assert_eq!(
            resolver.overlay_deadline(),
            None,
            "the passed delay changes nothing while the sequence holds no key"
        );

        // The first key of the sequence arms the overlay, and the overlay then
        // consumes the deadline.
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        assert_eq!(resolver.overlay_deadline(), Some(WHICH_KEY_DELAY));
        assert!(resolver.which_key(WHICH_KEY_DELAY).is_some());
        assert_eq!(resolver.overlay_deadline(), None);
    }

    #[test]
    fn every_reported_overlay_deadline_reveals_the_overlay() {
        // The two functions must agree: a reported deadline always produces the
        // rows that clear it.
        for keys in [
            vec![ch('5')],
            vec![ch('1'), ch('2')],
            vec![ch(' ')],
            vec![ch('5'), ch(' ')],
            vec![ch('g')],
            vec![ch('5'), ch('g')],
        ] {
            let mut resolver = resolver();
            feed(&mut resolver, &keys);
            let deadline = resolver.overlay_deadline();
            let rows = resolver.which_key(WHICH_KEY_DELAY);
            assert_eq!(
                deadline.is_some(),
                rows.is_some(),
                "a reported deadline for {keys:?} must reveal the overlay"
            );
        }
    }

    #[test]
    fn opening_a_prompt_clears_the_pending_sequence() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        resolver.set_context(InputContext::NORMAL.open_prompt(PromptKind::Search));
        assert!(resolver.pending_keys().is_empty());
        assert_eq!(
            resolver.resolve(ch('g'), NOW),
            Resolution::Prompt(PromptEdit::Insert('g'))
        );
    }

    /// Returns the resolution of one register-qualified command.
    fn registered(command: Command, count: Option<u32>, register: char) -> Resolution {
        Resolution::Command {
            command,
            count: count.and_then(NonZeroU32::new),
            register: Some(register),
        }
    }

    #[test]
    fn a_count_reaches_the_operator_and_the_motion_separately() {
        // `2d3w` deletes six words. The editor multiplies the two counts, so
        // each one reaches it with its own command.
        let mut resolver = resolver();
        assert_eq!(feed(&mut resolver, &[ch('2')]), Resolution::Pending);
        assert_eq!(
            resolver.resolve(ch('d'), NOW),
            counted(Command::DeleteOverMotion, 2)
        );
        assert_eq!(feed(&mut resolver, &[ch('3')]), Resolution::Pending);
        assert_eq!(
            resolver.resolve(ch('w'), NOW),
            counted(Command::MoveNextWordStart, 3)
        );
        assert!(resolver.snapshot().phases.is_idle());
    }

    #[test]
    fn a_register_qualifies_the_next_operation_only() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('"'), NOW), Resolution::Pending);
        assert_eq!(
            resolver.snapshot().scope,
            BindingScope::RegisterSelection,
            "the selection waits for the name of the register"
        );
        assert_eq!(
            resolver.resolve(ch('a'), NOW),
            Resolution::Pending,
            "the printable key names the register through the text fallback"
        );
        assert_eq!(
            resolver.resolve(ch('Y'), NOW),
            registered(Command::YankLine, None, 'a')
        );
        assert_eq!(
            resolver.resolve(ch('Y'), NOW),
            command(Command::YankLine),
            "the register applies to one operation only"
        );
    }

    #[test]
    fn a_cancel_key_ends_a_register_selection() {
        let mut resolver = resolver();
        resolver.resolve(ch('"'), NOW);
        assert_eq!(
            resolver.resolve(Key::plain(KeyCode::Esc), NOW),
            Resolution::Cancelled
        );
        assert!(resolver.snapshot().phases.is_idle());
        assert_eq!(resolver.snapshot().scope, BindingScope::Mode(Mode::Normal));
    }

    #[test]
    fn the_picker_table_answers_above_its_query_line() {
        let mut resolver = resolver();
        resolver.set_context(InputContext::Picker.open_prompt(PromptKind::Picker));
        assert_eq!(
            resolver.resolve(Key::plain(KeyCode::Down), NOW),
            command(Command::PickerSelectNext),
            "the open picker owns its own chords"
        );
        assert_eq!(
            resolver.resolve(ch('w'), NOW),
            Resolution::Prompt(PromptEdit::Insert('w')),
            "every other printable key belongs to the query"
        );
        assert_eq!(
            resolver.resolve(Key::plain(KeyCode::Esc), NOW),
            Resolution::Prompt(PromptEdit::Cancel)
        );
    }

    #[test]
    fn insert_mode_types_a_character_and_binds_the_three_entry_keys() {
        let mut resolver = resolver();
        resolver.set_context(InputContext::Mode(Mode::Insert));
        assert_eq!(resolver.resolve(ch('x'), NOW), Resolution::Text('x'));
        assert_eq!(resolver.resolve(ch(' '), NOW), Resolution::Text(' '));
        for (key, expected) in [
            (Key::plain(KeyCode::Enter), Command::InsertLineBreak),
            (
                Key::plain(KeyCode::Backspace),
                Command::DeleteCharacterBefore,
            ),
            (Key::plain(KeyCode::Tab), Command::InsertIndent),
        ] {
            assert_eq!(
                resolver.resolve(key, NOW),
                command(expected),
                "Insert mode binds `{}`, because it types no character",
                key.label()
            );
        }
    }

    #[test]
    fn every_context_state_change_publishes_a_new_generation() {
        let mut resolver = resolver();
        let start = resolver.snapshot().generation;
        // A pending key sequence is the state of the resolver, not of the
        // surface, so it publishes no new generation.
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        assert_eq!(resolver.snapshot().generation, start);

        assert_eq!(
            resolver.resolve(ch('g'), NOW),
            command(Command::MoveFirstLine)
        );
        let completed = resolver.snapshot().generation;
        assert_ne!(completed, start);

        assert_eq!(resolver.resolve(ch('3'), NOW), Resolution::Pending);
        let counted = resolver.snapshot().generation;
        assert_ne!(counted, completed);

        resolver.set_context(InputContext::Mode(Mode::Visual));
        assert_ne!(resolver.snapshot().generation, counted);
    }

    #[test]
    fn every_default_binding_is_a_surface_contribution_of_the_shared_registry() {
        let registry = Registry::first_release();
        let mut bindings = 0_usize;
        for scope in BindingScope::ALL {
            for (keys, command) in registry.bindings(scope) {
                bindings += 1;
                assert_eq!(
                    registry.command(scope, keys.keys()),
                    Some(command),
                    "{scope} `{keys}` must reach `{command}` through the shared registry"
                );
            }
        }
        assert!(
            bindings > 300,
            "the shared registry holds the complete kvim preset, but it holds {bindings} bindings"
        );
    }
}
