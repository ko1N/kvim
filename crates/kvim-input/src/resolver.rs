//! The bounded, clock-independent key-sequence resolver.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! The resolver never reads a clock. The terminal event loop measures the
//! elapsed time and supplies it with every request, so resolution stays
//! deterministic and testable. The elapsed time serves the which-key overlay
//! only. A pending sequence holds no deadline and waits for the next key.

use std::num::NonZeroU32;
use std::time::Duration;

use kvim_keymap::{Chord, Key, KeyCode};
use kvim_settings::InputSettings;

use super::command::Command;
use super::mode::{BindingScope, InputContext};
use super::registry::{Registry, WhichKeyRow};

/// One edit of an open line prompt.
///
/// The resolver translates the raw key, so the command line and the search
/// prompt never compare a key value.
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

/// One edit of the answer of an open confirmation.
///
/// The resolver translates the raw key, so the editor never compares a key
/// value. The confirmation completes nothing, so this enumeration holds no
/// completion edit. See `docs/input-actions.md`.
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
    /// An open confirmation owns every key, so a key that it does not read
    /// reaches no other owner and inserts no buffer text.
    Ignore,
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
    /// The sequence completed one command.
    Command {
        /// The command that the sequence reached.
        command: Command,
        /// The decimal count before the sequence.
        count: Option<NonZeroU32>,
    },
    /// An open prompt owns input and the key edits its line.
    Prompt(PromptEdit),
    /// An open confirmation owns input and the key edits its answer.
    Confirmation(ConfirmEdit),
    /// The sequence is a valid prefix of at least one longer sequence.
    Pending,
    /// `Esc` or `Ctrl-C` cancelled the pending sequence and the pending count.
    Cancelled,
    /// The key reaches no command. Pending input is reset.
    ///
    /// In Insert mode a printable key produces this outcome, and the editor
    /// inserts the character through an edit transaction.
    NoMatch,
}

/// The which-key overlay state of one pending sequence.
///
/// The delay governs the first appearance only. The overlay then stays visible
/// for the rest of the sequence, so a deeper level updates its rows without
/// hiding them again. See `docs/input-actions.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Overlay {
    /// The overlay appears at this elapsed time.
    Delayed { at: Duration },
    /// The overlay is visible and stays visible while the sequence continues.
    Visible,
}

/// The pending input of the resolver.
///
/// The active variant ties the pending keys, the pending count, and the overlay
/// state together, so a pending sequence without an overlay state cannot exist.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum PendingInput {
    /// No key and no count wait for completion.
    #[default]
    Idle,
    /// A key sequence, a count, or both wait for completion.
    Active {
        keys: Vec<Key>,
        count: Option<u32>,
        overlay: Overlay,
    },
}

/// Whether an operator waits for the target that the next keys name.
///
/// The resolver derives the state from the commands that it emitted itself, so
/// it needs no report from the editor. An operator command opens the state, and
/// the next completed command closes it, exactly as the operator-pending state
/// of the editor consumes the next command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum OperatorInput {
    /// No operator waits, so the active editor mode owns the keys.
    #[default]
    Idle,
    /// One operator waits, so [`BindingScope::OperatorPending`] owns the keys.
    Pending,
}

/// The modal input resolver.
///
/// The resolver accepts an optional decimal count, then a bounded key sequence.
/// It classifies every request as a complete match, a valid prefix, a cancel, or
/// no match. Only a scope that reports
/// [`crate::BindingScope::accepts_count`] opens a count, so a digit stays
/// buffer text in Insert mode.
///
/// An operator command moves the keys into
/// [`BindingScope::OperatorPending`] until the next command completes, so `i`
/// and `a` start a text object after `d`, `c`, and `y` instead of Insert mode.
///
/// A pending sequence holds no deadline. It waits for the next key, and only
/// `Esc`, `Ctrl-C`, a mismatch, a completed command, or a mode change ends it.
/// The registry rejects a sequence that both completes a command and starts a
/// longer sequence, so no ambiguity remains for a timer to resolve.
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
///     }
/// );
/// ```
#[derive(Clone, Debug)]
pub struct Resolver {
    registry: Registry,
    settings: InputSettings,
    context: InputContext,
    pending: PendingInput,
    operator: OperatorInput,
}

impl Resolver {
    /// Creates a resolver over one registry and one bound set.
    #[must_use]
    pub fn new(registry: Registry, settings: InputSettings) -> Self {
        debug_assert!(
            settings.pending_keys_max > 0 && settings.count_max > 0,
            "EditorSettings holds positive input bounds"
        );
        Self {
            registry,
            settings,
            context: InputContext::NORMAL,
            pending: PendingInput::Idle,
            operator: OperatorInput::Idle,
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
        self.context
    }

    /// Returns the keys of the pending sequence.
    #[must_use]
    pub fn pending_keys(&self) -> &[Key] {
        match &self.pending {
            PendingInput::Idle => &[],
            PendingInput::Active { keys, .. } => keys,
        }
    }

    /// Returns the elapsed time at which the which-key overlay appears.
    ///
    /// The event loop uses the value to wake exactly when the overlay becomes
    /// visible. It is the only time-driven state change of the resolver. A
    /// visible overlay needs no further wake, so it reports no time.
    ///
    /// A pending count alone reports no time either. The rows list the keys that
    /// follow a sequence, so [`Resolver::which_key`] shows no overlay while the
    /// pending sequence holds no key. A time that no transition can consume
    /// would wake the event loop forever. Both functions therefore apply the
    /// same condition.
    #[must_use]
    pub fn overlay_deadline(&self) -> Option<Duration> {
        let PendingInput::Active { keys, overlay, .. } = &self.pending else {
            return None;
        };
        if keys.is_empty() {
            return None;
        }
        match overlay {
            Overlay::Visible => None,
            Overlay::Delayed { at } => Some(*at),
        }
    }

    /// Moves input to another context and resets pending input.
    ///
    /// A mode change and a prompt change both reset the pending keys, the
    /// pending count, and the which-key overlay. An unchanged context keeps the
    /// pending sequence.
    pub fn set_context(&mut self, context: InputContext) {
        if context == self.context {
            return;
        }
        self.context = context;
        self.reset();
    }

    /// Clears the pending keys, the pending count, and the which-key overlay.
    ///
    /// A reset never changes buffer text and never cancels background work.
    pub fn reset(&mut self) {
        self.pending = PendingInput::Idle;
        self.operator = OperatorInput::Idle;
    }

    /// Returns the scope that owns the keys of the next request.
    ///
    /// A waiting operator owns them before the active mode does, because `i`
    /// and `a` start a text object there instead of Insert mode.
    fn active_scope(&self) -> BindingScope {
        match self.operator {
            OperatorInput::Idle => self.context.scope(),
            OperatorInput::Pending => BindingScope::OperatorPending,
        }
    }

    /// Returns the which-key overlay rows, or `None` while the overlay stays
    /// hidden.
    ///
    /// The overlay appears after the which-key delay of `EditorSettings` and
    /// lists the keys that may follow the pending sequence. The rows come from
    /// the registry, so their order is deterministic.
    ///
    /// The delay governs the first appearance only. The call records that
    /// appearance, so every further key of the same sequence updates the rows
    /// at once, without a second wait.
    pub fn which_key(&mut self, now: Duration) -> Option<Vec<WhichKeyRow>> {
        if !self.reveal_overlay(now) {
            return None;
        }
        let scope = self.active_scope();
        let PendingInput::Active { keys, .. } = &self.pending else {
            debug_assert!(false, "a hidden overlay leaves the resolver above");
            return None;
        };
        Some(self.registry.rows_for_prefix(scope, keys))
    }

    /// Reports whether the overlay is visible and records its first appearance.
    ///
    /// A pending count alone shows no overlay, because the rows list the keys
    /// that follow a sequence.
    fn reveal_overlay(&mut self, now: Duration) -> bool {
        let PendingInput::Active { keys, overlay, .. } = &mut self.pending else {
            return false;
        };
        if keys.is_empty() {
            return false;
        }
        match *overlay {
            Overlay::Visible => true,
            Overlay::Delayed { at } if now >= at => {
                *overlay = Overlay::Visible;
                true
            }
            Overlay::Delayed { .. } => false,
        }
    }

    /// Resolves one key at the elapsed time `now`.
    ///
    /// An open confirmation answers first, because it owns every key. The
    /// function then cancels pending input on `Esc` or `Ctrl-C`, accumulates a
    /// decimal count in a mode that holds one, and extends the pending
    /// sequence. The elapsed time only arms the which-key overlay.
    pub fn resolve(&mut self, key: Key, now: Duration) -> Resolution {
        // A confirmation reads its own answer and reaches no table, so it takes
        // the key before every other branch. `Enter` therefore reaches the
        // confirmation alone, never the prompt below it. Only this context
        // produces a confirmation edit, so no key reaches a closed
        // confirmation.
        if matches!(self.context, InputContext::Confirmation { .. }) {
            debug_assert!(
                matches!(self.pending, PendingInput::Idle),
                "a context change resets pending input, so a confirmation holds none"
            );
            return Resolution::Confirmation(confirm_edit(key));
        }
        if self.context.prompt().is_some() {
            debug_assert!(
                matches!(self.pending, PendingInput::Idle),
                "a context change resets pending input, so a prompt holds none"
            );
            // The picker reads a query and owns its own chords, so its table
            // answers before the query takes the key. Every other prompt reads
            // text alone.
            if self.context.scope() == BindingScope::Picker
                && let Some(command) = self.registry.command(BindingScope::Picker, &[key])
            {
                return Resolution::Command {
                    command,
                    count: None,
                };
            }
            return prompt_edit(key).map_or(Resolution::NoMatch, Resolution::Prompt);
        }
        // A cancel key ends pending input at every depth and in every mode.
        // Without pending input the same key reaches the registry, so `Esc` and
        // `Ctrl-C` still return to Normal mode.
        if matches!(self.pending, PendingInput::Active { .. }) && is_cancel_key(key) {
            self.pending = PendingInput::Idle;
            // A waiting operator lives in the editor too, so the cancel must
            // reach it. `ReturnToNormal` is no motion and no text object, which
            // aborts the operator and changes nothing.
            if self.operator == OperatorInput::Pending {
                self.operator = OperatorInput::Idle;
                return Resolution::Command {
                    command: Command::ReturnToNormal,
                    count: None,
                };
            }
            return Resolution::Cancelled;
        }
        let scope = self.active_scope();
        // Taking the pending state first makes every later branch a reset by
        // default. Only a still-pending outcome puts it back.
        let (mut keys, count, overlay) = self.take_pending();
        debug_assert!(
            count.is_none() || scope.accepts_count(),
            "only a scope that accepts a count opens one, and a context change resets pending input"
        );
        // A digit builds the count only before the sequence starts, and only in
        // a scope that holds a count. A count inside an operator-pending
        // sequence belongs to the editor.
        if keys.is_empty() && scope.accepts_count() {
            match self.accumulate_count(key, count) {
                CountStep::Grown(value) => {
                    self.pending = self.arm(Vec::new(), Some(value), overlay, now);
                    return Resolution::Pending;
                }
                CountStep::AboveMaximum => return Resolution::NoMatch,
                CountStep::NotADigit => {}
            }
        }

        debug_assert!(
            keys.len() < usize::from(self.settings.pending_keys_max),
            "the registry rejects a sequence above the pending-key maximum, so a pending sequence keeps room for one key"
        );
        keys.push(key);
        let complete = self.registry.command(scope, &keys);
        let longer = self.registry.has_longer_sequence(scope, &keys);
        debug_assert!(
            !(complete.is_some() && longer),
            "the registry rejects a strict prefix pair, so a sequence never matches and extends at once"
        );
        if let Some(command) = complete {
            debug_assert!(
                count != Some(0),
                "a count starts with a digit between 1 and 9, so it is never zero"
            );
            // The editor consumes exactly one command after an operator, so one
            // completed command always closes the operator-pending scope, even
            // when it names another operator: `dd` is one linewise delete.
            self.operator = match self.operator {
                OperatorInput::Idle if command.starts_operator_pending() => OperatorInput::Pending,
                OperatorInput::Idle | OperatorInput::Pending => OperatorInput::Idle,
            };
            return Resolution::Command {
                command,
                count: count.and_then(NonZeroU32::new),
            };
        }
        if longer {
            self.pending = self.arm(keys, count, overlay, now);
            return Resolution::Pending;
        }
        Resolution::NoMatch
    }

    /// Takes the pending keys, the pending count, and the overlay state, and
    /// leaves the resolver idle.
    fn take_pending(&mut self) -> (Vec<Key>, Option<u32>, Option<Overlay>) {
        match std::mem::take(&mut self.pending) {
            PendingInput::Idle => (Vec::new(), None, None),
            PendingInput::Active {
                keys,
                count,
                overlay,
            } => (keys, count, Some(overlay)),
        }
    }

    /// Builds the active pending state and keeps the overlay state.
    ///
    /// A visible overlay stays visible, and a delayed overlay keeps its
    /// original time, so the delay counts from the first key of the sequence
    /// only.
    fn arm(
        &self,
        keys: Vec<Key>,
        count: Option<u32>,
        overlay: Option<Overlay>,
        now: Duration,
    ) -> PendingInput {
        PendingInput::Active {
            keys,
            count,
            overlay: overlay.unwrap_or(Overlay::Delayed {
                at: saturating_deadline(now, self.settings.which_key_delay),
            }),
        }
    }

    /// Extends the decimal count with one digit key.
    ///
    /// `0` starts no count, because it is the first-column motion until a count
    /// is already open.
    fn accumulate_count(&self, key: Key, count: Option<u32>) -> CountStep {
        let Some(digit) = count_digit(key) else {
            return CountStep::NotADigit;
        };
        if digit == 0 && count.is_none() {
            return CountStep::NotADigit;
        }
        let next = count
            .unwrap_or(0)
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(digit)))
            .filter(|value| *value <= self.settings.count_max);
        next.map_or(CountStep::AboveMaximum, CountStep::Grown)
    }
}

/// The outcome of one count digit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CountStep {
    /// The key is not a decimal count digit. The key starts a sequence instead.
    NotADigit,
    /// The count grew and stays inside the count maximum.
    Grown(u32),
    /// The count would pass the count maximum, so the input is a mismatch.
    AboveMaximum,
}

/// Returns the decimal value of a plain digit key.
fn count_digit(key: Key) -> Option<u8> {
    if key.chord() != Chord::Plain {
        return None;
    }
    let KeyCode::Char(value) = key.code() else {
        return None;
    };
    // Radix ten accepts `0` through `9` only, so the value fits one byte.
    u8::try_from(value.to_digit(10)?).ok()
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

/// Translates one key into a prompt line edit.
///
/// Every prompt reads the same keys. A prompt that offers no candidate ignores
/// the two completion edits, so only the command line answers them today. See
/// `docs/input-actions.md`.
fn prompt_edit(key: Key) -> Option<PromptEdit> {
    if is_cancel_key(key) {
        return Some(PromptEdit::Cancel);
    }
    match (key.chord(), key.code()) {
        (Chord::Plain, KeyCode::Char(value)) => Some(PromptEdit::Insert(value)),
        (Chord::Plain, KeyCode::Backspace) => Some(PromptEdit::DeleteBackward),
        (Chord::Plain, KeyCode::Tab) => Some(PromptEdit::CompleteNext),
        (Chord::Plain, KeyCode::BackTab) => Some(PromptEdit::CompletePrevious),
        (Chord::Plain, KeyCode::Enter) => Some(PromptEdit::Accept),
        _ => None,
    }
}

/// Translates one key into an edit of the confirmation answer.
///
/// The confirmation reads its own small table, so it completes nothing: `Tab`
/// and `Shift-Tab` change nothing. The function answers for every key, so an
/// open confirmation owns every key and none of them reaches the buffer below
/// it. See `docs/input-actions.md`.
fn confirm_edit(key: Key) -> ConfirmEdit {
    if is_cancel_key(key) {
        return ConfirmEdit::Cancel;
    }
    match (key.chord(), key.code()) {
        (Chord::Plain, KeyCode::Char(value)) => ConfirmEdit::Insert(value),
        (Chord::Plain, KeyCode::Backspace) => ConfirmEdit::DeleteBackward,
        (Chord::Plain, KeyCode::Enter) => ConfirmEdit::Accept,
        _ => ConfirmEdit::Ignore,
    }
}

/// Adds a bound to the elapsed time without overflow.
fn saturating_deadline(now: Duration, bound: Duration) -> Duration {
    now.checked_add(bound).unwrap_or(Duration::MAX)
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
        }
    }

    fn counted(command: Command, count: u32) -> Resolution {
        Resolution::Command {
            command,
            count: NonZeroU32::new(count),
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
                assert_eq!(
                    feed(&mut resolver, keys.keys()),
                    command(expected),
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
                // and the editor inserts it as buffer text.
                Resolution::NoMatch
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
            }
        );
        // `i` reaches Insert mode in Normal mode, and a text object here.
        assert_eq!(resolver.resolve(ch('i'), NOW), Resolution::Pending);
        assert_eq!(
            resolver.resolve(ch(')'), NOW),
            Resolution::Command {
                command: Command::SelectInnerParen,
                count: None,
            }
        );
        // The completed command closes the scope, so `i` inserts again.
        assert_eq!(
            resolver.resolve(ch('i'), NOW),
            Resolution::Command {
                command: Command::InsertBeforeCursor,
                count: None,
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
            }
        );
        assert_eq!(
            resolver.resolve(ch('i'), NOW),
            Resolution::Command {
                command: Command::InsertBeforeCursor,
                count: None,
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
            }
        );
        assert_eq!(
            half.resolve(ch('i'), NOW),
            Resolution::Command {
                command: Command::InsertBeforeCursor,
                count: None,
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
}
