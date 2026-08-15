//! The bounded, clock-independent key-sequence resolver.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! The resolver never reads a clock. The terminal event loop measures the
//! elapsed time and supplies it with every request, so resolution stays
//! deterministic and testable.

use std::num::NonZeroU32;
use std::time::Duration;

use crate::settings::InputSettings;
use crate::terminal::{Chord, Key, KeyCode};

use super::command::Command;
use super::mode::InputContext;
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
    /// Run the prompt line.
    Accept,
    /// Cancel the prompt and restore the previous mode.
    Cancel,
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
    /// The sequence is a valid prefix of at least one longer sequence.
    Pending,
    /// The key reaches no command. Pending input is reset.
    ///
    /// In Insert mode a printable key produces this outcome, and the editor
    /// inserts the character through an edit transaction.
    NoMatch,
}

/// The outcome of one deadline check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expiry {
    /// The pending sequence reached its deadline and was reset.
    Expired,
    /// Nothing changed.
    Unchanged,
}

/// The pending input of the resolver.
///
/// The active variant ties the pending keys, the pending count, and the two
/// deadlines together, so a pending sequence without a deadline cannot exist.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum PendingInput {
    /// No key and no count wait for completion.
    #[default]
    Idle,
    /// A key sequence, a count, or both wait for completion.
    Active {
        keys: Vec<Key>,
        count: Option<u32>,
        deadline: Duration,
        overlay_at: Duration,
    },
}

/// The modal input resolver.
///
/// The resolver accepts an optional decimal count, then a bounded key sequence.
/// It classifies every request as a complete match, a valid prefix, or no match.
///
/// ```
/// use std::time::Duration;
///
/// use kvim::input::{Command, Registry, Resolution, Resolver};
/// use kvim::settings::InputSettings;
/// use kvim::terminal::{Key, KeyCode};
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

    /// Returns the elapsed time at which the pending sequence expires.
    ///
    /// The event loop uses the value to wake before the deadline.
    #[must_use]
    pub fn deadline(&self) -> Option<Duration> {
        match self.pending {
            PendingInput::Idle => None,
            PendingInput::Active { deadline, .. } => Some(deadline),
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
    }

    /// Resets pending input when it reached its deadline.
    ///
    /// The event loop calls the function on every tick with the elapsed time.
    pub fn expire(&mut self, now: Duration) -> Expiry {
        match self.pending {
            PendingInput::Active { deadline, .. } if now >= deadline => {
                self.pending = PendingInput::Idle;
                Expiry::Expired
            }
            _ => Expiry::Unchanged,
        }
    }

    /// Returns the which-key overlay rows, or `None` while the overlay stays
    /// hidden.
    ///
    /// The overlay appears after the which-key delay and lists the commands that
    /// the pending sequence can still reach. The rows come from the registry, so
    /// their order is deterministic.
    #[must_use]
    pub fn which_key(&self, now: Duration) -> Option<Vec<WhichKeyRow>> {
        match &self.pending {
            PendingInput::Active {
                keys, overlay_at, ..
            } if !keys.is_empty() && now >= *overlay_at => {
                Some(self.registry.rows_for_prefix(self.context.mode(), keys))
            }
            _ => None,
        }
    }

    /// Resolves one key at the elapsed time `now`.
    ///
    /// The function expires an overdue sequence first, then accumulates a
    /// decimal count, then extends the pending sequence.
    pub fn resolve(&mut self, key: Key, now: Duration) -> Resolution {
        if self.context.prompt().is_some() {
            debug_assert!(
                matches!(self.pending, PendingInput::Idle),
                "a context change resets pending input, so a prompt holds none"
            );
            return prompt_edit(key).map_or(Resolution::NoMatch, Resolution::Prompt);
        }
        self.expire(now);
        let mode = self.context.mode();
        // Taking the pending state first makes every later branch a reset by
        // default. Only a still-pending outcome puts it back.
        let (mut keys, count) = self.take_pending();
        // A digit builds the count only before the sequence starts. A count
        // inside an operator-pending sequence belongs to the editor.
        if keys.is_empty() {
            match self.accumulate_count(key, count) {
                CountStep::Grown(value) => {
                    self.pending = self.arm(Vec::new(), Some(value), now);
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
        let complete = self.registry.command(mode, &keys);
        let longer = self.registry.has_longer_sequence(mode, &keys);
        debug_assert!(
            !(complete.is_some() && longer),
            "the registry rejects a strict prefix pair, so a sequence never matches and extends at once"
        );
        if let Some(command) = complete {
            debug_assert!(
                count != Some(0),
                "a count starts with a digit between 1 and 9, so it is never zero"
            );
            return Resolution::Command {
                command,
                count: count.and_then(NonZeroU32::new),
            };
        }
        if longer {
            self.pending = self.arm(keys, count, now);
            return Resolution::Pending;
        }
        Resolution::NoMatch
    }

    /// Takes the pending keys and count and leaves the resolver idle.
    fn take_pending(&mut self) -> (Vec<Key>, Option<u32>) {
        match std::mem::take(&mut self.pending) {
            PendingInput::Idle => (Vec::new(), None),
            PendingInput::Active { keys, count, .. } => (keys, count),
        }
    }

    /// Builds the active pending state and arms both deadlines.
    fn arm(&self, keys: Vec<Key>, count: Option<u32>, now: Duration) -> PendingInput {
        PendingInput::Active {
            keys,
            count,
            deadline: saturating_deadline(now, self.settings.sequence_timeout),
            overlay_at: saturating_deadline(now, self.settings.which_key_delay),
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

/// Translates one key into a prompt line edit.
fn prompt_edit(key: Key) -> Option<PromptEdit> {
    match (key.chord(), key.code()) {
        (Chord::Plain, KeyCode::Char(value)) => Some(PromptEdit::Insert(value)),
        (Chord::Plain, KeyCode::Backspace) => Some(PromptEdit::DeleteBackward),
        (Chord::Plain, KeyCode::Enter) => Some(PromptEdit::Accept),
        (Chord::Plain, KeyCode::Esc) | (Chord::Ctrl, KeyCode::Char('c')) => {
            Some(PromptEdit::Cancel)
        }
        _ => None,
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

    use crate::input::{Command, InputContext, Mode, PromptKind, Registry};
    use crate::settings::InputSettings;
    use crate::terminal::{Key, KeyCode};

    use super::{Expiry, PromptEdit, Resolution, Resolver};

    const NOW: Duration = Duration::ZERO;

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
                assert_eq!(resolver.context().mode(), mode);
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
    fn a_count_above_the_maximum_resets_pending_input() {
        let mut resolver = resolver();
        for key in [ch('9'), ch('9'), ch('9'), ch('9')] {
            assert_eq!(resolver.resolve(key, NOW), Resolution::Pending);
        }
        assert_eq!(resolver.resolve(ch('9'), NOW), Resolution::NoMatch);
        assert_eq!(resolver.deadline(), None);
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
            assert!(resolver.deadline().is_some());
        }
    }

    #[test]
    fn an_unknown_sequence_resets_pending_input() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        assert_eq!(resolver.resolve(ch('q'), NOW), Resolution::NoMatch);
        assert!(resolver.pending_keys().is_empty());
        assert_eq!(resolver.deadline(), None);
    }

    #[test]
    fn the_sequence_deadline_resets_pending_input() {
        let settings = InputSettings::default();
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        let before = settings.sequence_timeout - Duration::from_millis(1);
        assert_eq!(resolver.expire(before), Expiry::Unchanged);
        assert_eq!(resolver.pending_keys(), [ch('g')]);
        assert_eq!(resolver.expire(settings.sequence_timeout), Expiry::Expired);
        assert!(resolver.pending_keys().is_empty());
        assert_eq!(
            resolver.expire(settings.sequence_timeout),
            Expiry::Unchanged
        );
    }

    #[test]
    fn a_key_after_the_deadline_starts_a_new_sequence() {
        let settings = InputSettings::default();
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        assert_eq!(
            resolver.resolve(ch('g'), settings.sequence_timeout),
            Resolution::Pending,
            "the expired `g` must not complete `gg`"
        );
        assert_eq!(resolver.pending_keys(), [ch('g')]);
    }

    #[test]
    fn a_mode_change_clears_pending_input() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch('g'), NOW), Resolution::Pending);
        resolver.set_context(InputContext::Mode(Mode::Visual));
        assert!(resolver.pending_keys().is_empty());
        assert_eq!(resolver.deadline(), None);
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
    fn the_leader_sequence_cancels_without_a_command() {
        let mut resolver = resolver();
        assert_eq!(resolver.resolve(ch(' '), NOW), Resolution::Pending);
        assert_eq!(
            resolver.resolve(Key::plain(KeyCode::Esc), NOW),
            Resolution::NoMatch,
            "Esc carries no Normal-mode binding, so the leader sequence cancels"
        );
        assert!(resolver.pending_keys().is_empty());
        assert!(resolver.which_key(Duration::from_secs(1)).is_none());
    }

    #[test]
    fn the_which_key_overlay_appears_after_the_delay() {
        let settings = InputSettings::default();
        let mut resolver = resolver();
        assert!(resolver.which_key(NOW).is_none(), "no sequence is pending");
        assert_eq!(resolver.resolve(ch(' '), NOW), Resolution::Pending);
        let before = settings.which_key_delay - Duration::from_millis(1);
        assert!(resolver.which_key(before).is_none());
        let rows = resolver
            .which_key(settings.which_key_delay)
            .expect("the overlay appears after the delay");
        let listed = rows
            .iter()
            .map(|row| (row.keys.to_string(), row.label()))
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            vec![
                ("/".to_owned(), "Toggle the comment"),
                (
                    "\\".to_owned(),
                    "Split the window with the inverse adaptive rule"
                ),
                (
                    "c f".to_owned(),
                    "Toggle format-on-save for the active buffer"
                ),
                ("e".to_owned(), "Show the diagnostic float"),
                ("f /".to_owned(), "Open the ripgrep search picker"),
                ("f b".to_owned(), "Open the buffer picker"),
                ("f f".to_owned(), "Open the file search picker"),
                ("k".to_owned(), "Show hover information"),
                ("o".to_owned(), "Open the buffer picker"),
                ("q".to_owned(), "Close the focused window"),
                ("x".to_owned(), "Unload the active buffer"),
                (
                    "Enter".to_owned(),
                    "Split the window with the adaptive rule"
                ),
            ]
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
