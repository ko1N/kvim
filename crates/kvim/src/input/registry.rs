//! The mapping registry that binds a key sequence to a semantic command.
//!
//! The registry is the only place outside `terminal` that holds a raw key. It
//! validates itself at construction, so the resolver never meets a duplicate
//! sequence or an ambiguous prefix pair.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;
use std::ops::Bound;

use thiserror::Error;

use crate::settings::PENDING_KEYS_MAX;
use crate::terminal::{Chord, Key, KeyCode};

use super::command::Command;
use super::mode::Mode;

/// A non-empty key sequence that fits the pending-key maximum.
///
/// The type holds both bounds, so the registry cannot store a sequence that the
/// resolver could never complete.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeySequence(Vec<Key>);

impl KeySequence {
    /// Returns the keys of the sequence.
    #[inline]
    pub fn keys(&self) -> &[Key] {
        &self.0
    }

    /// Builds a sequence and checks both bounds.
    fn new(keys: &[Key], keys_max: u8) -> Result<Self, SequenceBound> {
        if keys.is_empty() {
            return Err(SequenceBound::Empty);
        }
        if keys.len() > usize::from(keys_max) {
            return Err(SequenceBound::TooLong {
                keys: keys.len(),
                keys_max,
            });
        }
        Ok(Self(keys.to_vec()))
    }
}

impl Borrow<[Key]> for KeySequence {
    /// Lets a lookup use a plain key slice.
    ///
    /// The ordering of `[Key]` equals the ordering of the wrapped `Vec<Key>`, so
    /// the borrowed form keeps the map ordering valid.
    #[inline]
    fn borrow(&self) -> &[Key] {
        &self.0
    }
}

impl fmt::Display for KeySequence {
    /// Writes the keys separated by one space.
    ///
    /// A named key such as `Space` is several characters wide, so a separator
    /// keeps `Space f` distinct from a single key.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, key) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(" ")?;
            }
            write_key(formatter, *key)?;
        }
        Ok(())
    }
}

/// Writes one key in its help form.
fn write_key(formatter: &mut fmt::Formatter<'_>, key: Key) -> fmt::Result {
    match key.chord() {
        Chord::Plain => {}
        Chord::Ctrl => formatter.write_str("C-")?,
        Chord::CtrlAlt => formatter.write_str("C-A-")?,
    }
    let name = match key.code() {
        KeyCode::Char(' ') => "Space",
        KeyCode::Char(value) => return write!(formatter, "{value}"),
        KeyCode::Up => "Up",
        KeyCode::Down => "Down",
        KeyCode::Left => "Left",
        KeyCode::Right => "Right",
        KeyCode::Enter => "Enter",
        KeyCode::Tab => "Tab",
        KeyCode::BackTab => "S-Tab",
        KeyCode::Backspace => "BS",
        KeyCode::Delete => "Del",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PgUp",
        KeyCode::PageDown => "PgDn",
        KeyCode::Esc => "Esc",
    };
    formatter.write_str(name)
}

/// The bound that one candidate sequence broke.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SequenceBound {
    Empty,
    TooLong { keys: usize, keys_max: u8 },
}

/// One mapping from a key sequence to a semantic command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    /// The mode that owns the mapping.
    pub mode: Mode,
    /// The key sequence that reaches the command.
    pub keys: Vec<Key>,
    /// The command that the sequence reaches.
    pub command: Command,
}

/// A rejected registry construction.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RegistryError {
    /// A binding held no key.
    #[error("the {mode} binding for `{command}` holds no key")]
    EmptySequence {
        /// The mode of the rejected binding.
        mode: Mode,
        /// The command of the rejected binding.
        command: Command,
    },
    /// A binding held more keys than one pending sequence can hold.
    #[error(
        "the {mode} binding for `{command}` holds {keys} keys, but the pending-key maximum is {keys_max}"
    )]
    SequenceTooLong {
        /// The mode of the rejected binding.
        mode: Mode,
        /// The command of the rejected binding.
        command: Command,
        /// The number of keys in the rejected binding.
        keys: usize,
        /// The pending-key maximum.
        keys_max: u8,
    },
    /// Two bindings of one mode held the same sequence.
    #[error("the {mode} sequence `{keys}` reaches both `{first}` and `{second}`")]
    DuplicateSequence {
        /// The mode that holds both bindings.
        mode: Mode,
        /// The repeated sequence.
        keys: KeySequence,
        /// The command of the first binding.
        first: Command,
        /// The command of the second binding.
        second: Command,
    },
    /// One sequence was a strict prefix of another sequence in the same mode.
    #[error(
        "the {mode} sequence `{prefix}` for `{prefix_command}` is a strict prefix of `{longer}` for `{longer_command}`"
    )]
    AmbiguousPrefix {
        /// The mode that holds both bindings.
        mode: Mode,
        /// The shorter sequence.
        prefix: KeySequence,
        /// The command of the shorter sequence.
        prefix_command: Command,
        /// The longer sequence.
        longer: KeySequence,
        /// The command of the longer sequence.
        longer_command: Command,
    },
}

/// One which-key overlay row.
///
/// The row names the keys that remain after the pending sequence and the command
/// that they reach. The overlay text comes from the command label only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhichKeyRow {
    /// The keys that remain after the pending sequence.
    pub keys: KeySequence,
    /// The command that the remaining keys reach.
    pub command: Command,
}

impl WhichKeyRow {
    /// Returns the overlay text of the row.
    #[inline]
    pub const fn label(&self) -> &'static str {
        self.command.label()
    }
}

/// The validated mapping registry, keyed by mode.
///
/// One key sequence may appear in several modes with different commands, because
/// only one mode is active.
///
/// ```
/// use kvim::input::{Command, Mode, Registry};
/// use kvim::terminal::{Key, KeyCode};
///
/// let registry = Registry::first_release();
/// let keys = [Key::plain(KeyCode::Char('d'))];
/// assert_eq!(
///     registry.command(Mode::Normal, &keys),
///     Some(Command::DeleteOverMotion)
/// );
/// assert_eq!(
///     registry.command(Mode::Visual, &keys),
///     Some(Command::DeleteSelection)
/// );
/// ```
#[derive(Clone, Debug)]
pub struct Registry {
    by_mode: [BTreeMap<KeySequence, Command>; Mode::COUNT],
}

impl Registry {
    /// Builds the registry from a binding list and validates it.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] for an empty sequence, a sequence longer than
    /// `keys_max`, a duplicate sequence inside one mode, or a strict prefix pair
    /// inside one mode.
    pub fn from_bindings(bindings: &[Binding], keys_max: u8) -> Result<Self, RegistryError> {
        let mut by_mode: [BTreeMap<KeySequence, Command>; Mode::COUNT] =
            std::array::from_fn(|_| BTreeMap::new());
        for binding in bindings {
            let keys = KeySequence::new(&binding.keys, keys_max).map_err(|bound| match bound {
                SequenceBound::Empty => RegistryError::EmptySequence {
                    mode: binding.mode,
                    command: binding.command,
                },
                SequenceBound::TooLong { keys, keys_max } => RegistryError::SequenceTooLong {
                    mode: binding.mode,
                    command: binding.command,
                    keys,
                    keys_max,
                },
            })?;
            match by_mode[binding.mode.index()].entry(keys) {
                Entry::Vacant(slot) => {
                    slot.insert(binding.command);
                }
                Entry::Occupied(slot) => {
                    return Err(RegistryError::DuplicateSequence {
                        mode: binding.mode,
                        keys: slot.key().clone(),
                        first: *slot.get(),
                        second: binding.command,
                    });
                }
            }
        }
        for mode in Mode::ALL {
            check_prefix_pairs(mode, &by_mode[mode.index()])?;
        }
        Ok(Self { by_mode })
    }

    /// Builds the hardcoded first-release registry.
    ///
    /// The table is `docs/input-actions.md`. The first release parses no
    /// configuration file, so this is a cold-path bootstrap: an invalid table is
    /// a programming error and must fail loudly at startup.
    ///
    /// # Panics
    ///
    /// Panics when the hardcoded table breaks a registry rule.
    #[must_use]
    pub fn first_release() -> Self {
        let bindings = first_release_bindings();
        match Self::from_bindings(&bindings, PENDING_KEYS_MAX) {
            Ok(registry) => registry,
            Err(error) => panic!("the hardcoded first-release binding table is invalid: {error}"),
        }
    }

    /// Returns the command that the exact sequence reaches in the mode.
    #[must_use]
    pub fn command(&self, mode: Mode, keys: &[Key]) -> Option<Command> {
        self.by_mode[mode.index()].get(keys).copied()
    }

    /// Reports whether the mode holds a sequence that extends the prefix.
    ///
    /// The map orders sequences lexicographically, so every extension of the
    /// prefix sorts directly after it. The smallest sequence above the prefix is
    /// therefore an extension whenever one exists.
    #[must_use]
    pub fn has_longer_sequence(&self, mode: Mode, prefix: &[Key]) -> bool {
        self.by_mode[mode.index()]
            .range::<[Key], _>((Bound::Excluded(prefix), Bound::Unbounded))
            .next()
            .is_some_and(|(sequence, _)| sequence.keys().starts_with(prefix))
    }

    /// Returns the which-key rows that the prefix can still reach.
    ///
    /// The rows follow the sequence order of the registry, so the overlay is
    /// deterministic. An empty prefix returns every binding of the mode, which
    /// suits a complete help list.
    #[must_use]
    pub fn rows_for_prefix(&self, mode: Mode, prefix: &[Key]) -> Vec<WhichKeyRow> {
        self.by_mode[mode.index()]
            .range::<[Key], _>((Bound::Included(prefix), Bound::Unbounded))
            .take_while(|(sequence, _)| sequence.keys().starts_with(prefix))
            .filter(|(sequence, _)| sequence.keys().len() > prefix.len())
            .map(|(sequence, command)| WhichKeyRow {
                keys: KeySequence(sequence.keys()[prefix.len()..].to_vec()),
                command: *command,
            })
            .collect()
    }

    /// Returns every binding of one mode in sequence order.
    pub fn bindings(&self, mode: Mode) -> impl Iterator<Item = (&KeySequence, Command)> {
        self.by_mode[mode.index()]
            .iter()
            .map(|(keys, command)| (keys, *command))
    }
}

/// Rejects a strict prefix pair inside one mode table.
///
/// Adjacent entries are enough: every extension of one sequence sorts directly
/// after it, so a prefix pair always appears as neighbours.
fn check_prefix_pairs(
    mode: Mode,
    table: &BTreeMap<KeySequence, Command>,
) -> Result<(), RegistryError> {
    let mut previous: Option<(&KeySequence, Command)> = None;
    for (keys, command) in table {
        if let Some((earlier, earlier_command)) = previous
            && keys.keys().starts_with(earlier.keys())
        {
            return Err(RegistryError::AmbiguousPrefix {
                mode,
                prefix: earlier.clone(),
                prefix_command: earlier_command,
                longer: keys.clone(),
                longer_command: *command,
            });
        }
        previous = Some((keys, *command));
    }
    Ok(())
}

fn ch(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

fn ctrl(value: char) -> Key {
    Key::ctrl(KeyCode::Char(value))
}

fn ctrl_alt(value: char) -> Key {
    Key::ctrl_alt(KeyCode::Char(value))
}

/// The leader key. `docs/input-actions.md` selects Space.
fn leader() -> Key {
    ch(' ')
}

/// Every mode that accepts a motion.
///
/// `docs/input-actions.md` applies every motion row to Visual Block mode too.
const MOTION_MODES: &[Mode] = &[
    Mode::Normal,
    Mode::Visual,
    Mode::VisualLine,
    Mode::VisualBlock,
];

/// The three Visual modes.
const VISUAL_MODES: &[Mode] = &[Mode::Visual, Mode::VisualLine, Mode::VisualBlock];

/// Normal mode alone.
const NORMAL: &[Mode] = &[Mode::Normal];

/// Every mode, for a command that must stay reachable while the user types.
const ALL_MODES: &[Mode] = &[
    Mode::Normal,
    Mode::Insert,
    Mode::Visual,
    Mode::VisualLine,
    Mode::VisualBlock,
];

fn add(bindings: &mut Vec<Binding>, modes: &[Mode], keys: &[Key], command: Command) {
    for &mode in modes {
        bindings.push(Binding {
            mode,
            keys: keys.to_vec(),
            command,
        });
    }
}

/// Builds the complete first-release binding table.
///
/// The table mirrors `docs/input-actions.md`. It holds no operator grammar: `d`,
/// `c`, and `y` each reach one operator command, and the operator-pending state
/// of the editor consumes the following motion. `dd`, `cc`, and `yy` are
/// therefore absent, because they would collide with the `d`, `c`, and `y`
/// prefixes.
fn first_release_bindings() -> Vec<Binding> {
    let mut bindings = Vec::new();
    let table = &mut bindings;

    // Modes.
    add(table, NORMAL, &[ch('i')], Command::InsertBeforeCursor);
    add(table, NORMAL, &[ch('I')], Command::InsertAtFirstNonBlank);
    add(table, NORMAL, &[ch('a')], Command::InsertAfterCursor);
    add(table, NORMAL, &[ch('A')], Command::InsertAtLineEnd);
    add(table, NORMAL, &[ch('o')], Command::OpenLineBelow);
    add(table, NORMAL, &[ch('O')], Command::OpenLineAbove);
    add(table, NORMAL, &[ch('v')], Command::EnterVisual);
    add(table, NORMAL, &[ch('V')], Command::EnterVisualLine);
    add(table, NORMAL, &[ctrl('v')], Command::EnterVisualBlock);
    add(table, NORMAL, &[ch(':')], Command::OpenCommandLine);
    add(
        table,
        &[
            Mode::Insert,
            Mode::Visual,
            Mode::VisualLine,
            Mode::VisualBlock,
        ],
        &[Key::plain(KeyCode::Esc)],
        Command::ReturnToNormal,
    );

    // Motions.
    add(table, MOTION_MODES, &[ch('h')], Command::MoveLeft);
    add(table, MOTION_MODES, &[ch('j')], Command::MoveDown);
    add(table, MOTION_MODES, &[ch('k')], Command::MoveUp);
    add(table, MOTION_MODES, &[ch('l')], Command::MoveRight);
    add(table, MOTION_MODES, &[ch('w')], Command::MoveNextWordStart);
    add(
        table,
        MOTION_MODES,
        &[ch('b')],
        Command::MovePreviousWordStart,
    );
    add(table, MOTION_MODES, &[ch('e')], Command::MoveNextWordEnd);
    add(table, MOTION_MODES, &[ch('0')], Command::MoveFirstColumn);
    add(table, MOTION_MODES, &[ch('^')], Command::MoveFirstNonBlank);
    add(table, MOTION_MODES, &[ch('$')], Command::MoveLineEnd);
    add(
        table,
        MOTION_MODES,
        &[ch('g'), ch('g')],
        Command::MoveFirstLine,
    );
    add(table, MOTION_MODES, &[ch('G')], Command::MoveLastLine);
    add(table, MOTION_MODES, &[ctrl('d')], Command::MoveHalfPageDown);
    add(table, MOTION_MODES, &[ctrl('u')], Command::MoveHalfPageUp);
    add(table, MOTION_MODES, &[ctrl('f')], Command::MoveFullPageDown);
    add(table, MOTION_MODES, &[ctrl('b')], Command::MoveFullPageUp);
    add(
        table,
        MOTION_MODES,
        &[ch('z'), ch('z')],
        Command::CenterCursorLine,
    );
    add(
        table,
        MOTION_MODES,
        &[ch('z'), ch('t')],
        Command::AlignCursorLineTop,
    );
    add(
        table,
        MOTION_MODES,
        &[ch('z'), ch('b')],
        Command::AlignCursorLineBottom,
    );

    // Operators, registers, and repeat.
    add(table, NORMAL, &[ch('d')], Command::DeleteOverMotion);
    add(table, NORMAL, &[ch('c')], Command::ChangeOverMotion);
    add(table, NORMAL, &[ch('y')], Command::YankOverMotion);
    add(table, VISUAL_MODES, &[ch('d')], Command::DeleteSelection);
    add(table, VISUAL_MODES, &[ch('c')], Command::ChangeSelection);
    add(table, VISUAL_MODES, &[ch('y')], Command::YankSelection);
    add(
        table,
        &[Mode::VisualBlock],
        &[ch('I')],
        Command::BlockInsertBefore,
    );
    add(
        table,
        &[Mode::VisualBlock],
        &[ch('A')],
        Command::BlockInsertAfter,
    );
    add(table, NORMAL, &[ch('D')], Command::DeleteToLineEnd);
    add(table, NORMAL, &[ch('C')], Command::ChangeToLineEnd);
    add(table, NORMAL, &[ch('Y')], Command::YankLine);
    add(
        table,
        &[Mode::Normal, Mode::Visual, Mode::VisualLine],
        &[ch('p')],
        Command::PasteAfter,
    );
    add(
        table,
        &[Mode::Normal, Mode::Visual, Mode::VisualLine],
        &[ch('P')],
        Command::PasteBefore,
    );
    add(table, NORMAL, &[ch('u')], Command::Undo);
    add(table, NORMAL, &[ctrl('r')], Command::Redo);
    add(table, NORMAL, &[ch('.')], Command::RepeatChange);

    // Search.
    add(table, NORMAL, &[ch('/')], Command::OpenSearchPrompt);
    add(table, NORMAL, &[ch('n')], Command::SearchNext);
    add(table, NORMAL, &[ch('N')], Command::SearchPrevious);

    // Visual selection.
    add(table, VISUAL_MODES, &[ch('J')], Command::MoveSelectionDown);
    add(table, VISUAL_MODES, &[ch('K')], Command::MoveSelectionUp);
    add(table, VISUAL_MODES, &[ch('<')], Command::ShiftSelectionLeft);
    add(
        table,
        VISUAL_MODES,
        &[ch('>')],
        Command::ShiftSelectionRight,
    );

    // Files and buffers.
    add(table, ALL_MODES, &[ctrl('s')], Command::SaveBuffer);
    add(table, NORMAL, &[ctrl('e')], Command::RevealInFileTree);
    add(
        table,
        NORMAL,
        &[leader(), ch('o')],
        Command::OpenBufferPicker,
    );
    add(
        table,
        NORMAL,
        &[leader(), ch('f'), ch('b')],
        Command::OpenBufferPicker,
    );
    add(table, NORMAL, &[leader(), ch('x')], Command::UnloadBuffer);
    add(
        table,
        NORMAL,
        &[leader(), ch('f'), ch('f')],
        Command::OpenFilePicker,
    );
    add(
        table,
        NORMAL,
        &[leader(), ch('f'), ch('/')],
        Command::OpenRipgrepPicker,
    );

    // Windows.
    add(table, NORMAL, &[ctrl('h')], Command::FocusWindowLeft);
    add(table, NORMAL, &[ctrl('j')], Command::FocusWindowDown);
    add(table, NORMAL, &[ctrl('k')], Command::FocusWindowUp);
    add(table, NORMAL, &[ctrl('l')], Command::FocusWindowRight);
    add(table, NORMAL, &[ctrl_alt('h')], Command::ResizeWindowLeft);
    add(table, NORMAL, &[ctrl_alt('j')], Command::ResizeWindowDown);
    add(table, NORMAL, &[ctrl_alt('k')], Command::ResizeWindowUp);
    add(table, NORMAL, &[ctrl_alt('l')], Command::ResizeWindowRight);
    add(
        table,
        NORMAL,
        &[leader(), Key::plain(KeyCode::Enter)],
        Command::SplitAdaptive,
    );
    add(
        table,
        NORMAL,
        &[Key::ctrl(KeyCode::Enter)],
        Command::SplitAdaptive,
    );
    add(
        table,
        NORMAL,
        &[leader(), ch('\\')],
        Command::SplitInverseAdaptive,
    );
    add(table, NORMAL, &[ctrl('\\')], Command::SplitInverseAdaptive);
    add(table, NORMAL, &[leader(), ch('q')], Command::CloseWindow);
    add(table, ALL_MODES, &[ctrl('q')], Command::CloseWindow);

    // Language services.
    add(
        table,
        &[Mode::Normal, Mode::Visual, Mode::VisualLine],
        &[leader(), ch('/')],
        Command::ToggleComment,
    );
    add(table, NORMAL, &[ch('g'), ch('d')], Command::GoToDefinition);
    add(table, NORMAL, &[leader(), ch('k')], Command::ShowHover);
    add(
        table,
        NORMAL,
        &[leader(), ch('e')],
        Command::ShowDiagnosticFloat,
    );
    add(table, NORMAL, &[ch(']'), ch('d')], Command::NextDiagnostic);
    add(
        table,
        NORMAL,
        &[ch('['), ch('d')],
        Command::PreviousDiagnostic,
    );
    add(
        table,
        NORMAL,
        &[leader(), ch('c'), ch('f')],
        Command::ToggleFormatOnSave,
    );

    bindings
}

#[cfg(test)]
mod tests {
    use super::{Binding, Command, Key, KeyCode, Mode, Registry, RegistryError, ch, ctrl, leader};

    fn binding(mode: Mode, keys: &[Key], command: Command) -> Binding {
        Binding {
            mode,
            keys: keys.to_vec(),
            command,
        }
    }

    #[test]
    fn the_first_release_table_validates() {
        let registry = Registry::first_release();
        assert!(
            registry.bindings(Mode::Normal).count() > 40,
            "the Normal table holds the first-release binding set"
        );
    }

    #[test]
    fn the_registry_rejects_every_invalid_construction() {
        let cases: [(&str, Vec<Binding>, RegistryError); 4] = [
            (
                "an empty sequence",
                vec![binding(Mode::Normal, &[], Command::Undo)],
                RegistryError::EmptySequence {
                    mode: Mode::Normal,
                    command: Command::Undo,
                },
            ),
            (
                "a sequence above the pending-key maximum",
                vec![binding(
                    Mode::Normal,
                    &[ch('a'), ch('b'), ch('c'), ch('d'), ch('e')],
                    Command::Undo,
                )],
                RegistryError::SequenceTooLong {
                    mode: Mode::Normal,
                    command: Command::Undo,
                    keys: 5,
                    keys_max: 4,
                },
            ),
            (
                "a duplicate sequence inside one mode",
                vec![
                    binding(Mode::Normal, &[ch('u')], Command::Undo),
                    binding(Mode::Normal, &[ch('u')], Command::Redo),
                ],
                RegistryError::DuplicateSequence {
                    mode: Mode::Normal,
                    keys: super::KeySequence(vec![ch('u')]),
                    first: Command::Undo,
                    second: Command::Redo,
                },
            ),
            (
                "an ambiguous prefix pair",
                vec![
                    binding(Mode::Normal, &[ch('g')], Command::Undo),
                    binding(Mode::Normal, &[ch('g'), ch('g')], Command::MoveFirstLine),
                ],
                RegistryError::AmbiguousPrefix {
                    mode: Mode::Normal,
                    prefix: super::KeySequence(vec![ch('g')]),
                    prefix_command: Command::Undo,
                    longer: super::KeySequence(vec![ch('g'), ch('g')]),
                    longer_command: Command::MoveFirstLine,
                },
            ),
        ];
        for (name, bindings, expected) in cases {
            let rejection = Registry::from_bindings(&bindings, 4)
                .err()
                .unwrap_or_else(|| panic!("{name} must be rejected"));
            assert_eq!(rejection, expected, "{name} must be rejected");
        }
    }

    #[test]
    fn an_ambiguous_prefix_is_rejected_in_either_declaration_order() {
        let bindings = vec![
            binding(Mode::Normal, &[ch('g'), ch('g')], Command::MoveFirstLine),
            binding(Mode::Normal, &[ch('g')], Command::Undo),
        ];
        assert!(matches!(
            Registry::from_bindings(&bindings, 4),
            Err(RegistryError::AmbiguousPrefix { .. })
        ));
    }

    #[test]
    fn one_sequence_reaches_different_commands_in_different_modes() {
        let registry = Registry::first_release();
        let keys = [ch('d')];
        assert_eq!(
            registry.command(Mode::Normal, &keys),
            Some(Command::DeleteOverMotion)
        );
        for mode in [Mode::Visual, Mode::VisualLine, Mode::VisualBlock] {
            assert_eq!(
                registry.command(mode, &keys),
                Some(Command::DeleteSelection),
                "{mode} deletes the selection"
            );
        }
    }

    #[test]
    fn the_operator_grammar_stays_out_of_the_registry() {
        let registry = Registry::first_release();
        for (operator, command) in [
            ('d', Command::DeleteOverMotion),
            ('c', Command::ChangeOverMotion),
            ('y', Command::YankOverMotion),
        ] {
            assert_eq!(
                registry.command(Mode::Normal, &[ch(operator)]),
                Some(command)
            );
            assert!(
                !registry.has_longer_sequence(Mode::Normal, &[ch(operator)]),
                "the editor operator-pending state consumes the motion after `{operator}`"
            );
        }
    }

    #[test]
    fn which_key_rows_follow_the_prefix_in_deterministic_order() {
        let registry = Registry::first_release();
        let rows = registry.rows_for_prefix(Mode::Normal, &[leader(), ch('f')]);
        let listed = rows
            .iter()
            .map(|row| (row.keys.to_string(), row.label()))
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            vec![
                ("/".to_owned(), "Open the ripgrep search picker"),
                ("b".to_owned(), "Open the buffer picker"),
                ("f".to_owned(), "Open the file search picker"),
            ]
        );
    }

    #[test]
    fn a_prefix_without_a_binding_produces_no_row() {
        let registry = Registry::first_release();
        assert!(
            registry
                .rows_for_prefix(Mode::VisualBlock, &[leader()])
                .is_empty(),
            "docs/input-actions.md keeps the leader out of Visual Block mode"
        );
    }

    #[test]
    fn key_sequences_render_with_their_chord_and_name() {
        let cases = [
            (vec![ch('g'), ch('g')], "g g"),
            (vec![leader(), ch('f'), ch('/')], "Space f /"),
            (vec![ctrl('d')], "C-d"),
            (vec![super::ctrl_alt('h')], "C-A-h"),
            (vec![Key::ctrl(KeyCode::Enter)], "C-Enter"),
            (vec![Key::plain(KeyCode::Esc)], "Esc"),
        ];
        for (keys, expected) in cases {
            let sequence = super::KeySequence::new(&keys, 4).expect("the test sequence is bounded");
            assert_eq!(sequence.to_string(), expected);
        }
    }
}
