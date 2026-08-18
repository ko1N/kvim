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

use kvim_settings::PENDING_KEYS_MAX;
use kvim_terminal::{Chord, Key, KeyCode};

use super::command::Command;
use super::mode::{BindingScope, Mode};

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
            write!(formatter, "{}", KeyLabel(*key))?;
        }
        Ok(())
    }
}

/// One key in the help form that the which-key overlay shows.
///
/// The form names a chord prefix and a key name, such as `C-d`, `Space`, or
/// `Enter`. It is help text, never a value that code compares.
///
/// ```
/// use kvim_input::{Mode, Registry};
/// use kvim_terminal::{Key, KeyCode};
///
/// let registry = Registry::first_release();
/// let rows = registry.rows_for_prefix(Mode::Normal, &[Key::plain(KeyCode::Char(' '))]);
/// let first = rows.first().expect("the leader reaches several commands");
/// assert_eq!(first.key_label().to_string(), "/");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyLabel(Key);

impl fmt::Display for KeyLabel {
    /// Writes one key in its help form.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.chord() {
            Chord::Plain => {}
            Chord::Ctrl => formatter.write_str("C-")?,
            Chord::CtrlAlt => formatter.write_str("C-A-")?,
        }
        let name = match self.0.code() {
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
    /// The scope that owns the mapping.
    pub scope: BindingScope,
    /// The key sequence that reaches the command.
    pub keys: Vec<Key>,
    /// The command that the sequence reaches.
    pub command: Command,
}

/// A rejected registry construction.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RegistryError {
    /// A binding held no key.
    #[error("the {scope} binding for `{command}` holds no key")]
    EmptySequence {
        /// The scope of the rejected binding.
        scope: BindingScope,
        /// The command of the rejected binding.
        command: Command,
    },
    /// A binding held more keys than one pending sequence can hold.
    #[error(
        "the {scope} binding for `{command}` holds {keys} keys, but the pending-key maximum is {keys_max}"
    )]
    SequenceTooLong {
        /// The scope of the rejected binding.
        scope: BindingScope,
        /// The command of the rejected binding.
        command: Command,
        /// The number of keys in the rejected binding.
        keys: usize,
        /// The pending-key maximum.
        keys_max: u8,
    },
    /// Two bindings of one scope held the same sequence.
    #[error("the {scope} sequence `{keys}` reaches both `{first}` and `{second}`")]
    DuplicateSequence {
        /// The scope that holds both bindings.
        scope: BindingScope,
        /// The repeated sequence.
        keys: KeySequence,
        /// The command of the first binding.
        first: Command,
        /// The command of the second binding.
        second: Command,
    },
    /// One sequence was a strict prefix of another sequence in the same scope.
    #[error(
        "the {scope} sequence `{prefix}` for `{prefix_command}` is a strict prefix of `{longer}` for `{longer_command}`"
    )]
    AmbiguousPrefix {
        /// The scope that holds both bindings.
        scope: BindingScope,
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

/// What one next key of the which-key overlay reaches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WhichKeyTarget {
    /// The key completes exactly one command, and the row shows its label.
    Command(Command),
    /// The key opens a group of commands.
    ///
    /// which-key.nvim marks such a key with a `+` prefix. The count names the
    /// commands that the group holds.
    Group {
        /// The number of commands behind the key. The value is at least two.
        commands: usize,
    },
}

impl WhichKeyTarget {
    /// Returns the target that one more command behind the same key produces.
    fn grown(self) -> Self {
        match self {
            Self::Command(_) => Self::Group { commands: 2 },
            Self::Group { commands } => Self::Group {
                commands: commands.saturating_add(1),
            },
        }
    }
}

impl fmt::Display for WhichKeyTarget {
    /// Writes the overlay text of one row.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(command) => formatter.write_str(command.label()),
            Self::Group { commands } => write!(formatter, "+{commands} commands"),
        }
    }
}

/// One which-key overlay row.
///
/// The overlay shows one level at a time, so a row names the single key that may
/// follow the pending sequence, never a complete sequence. See
/// `docs/input-actions.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WhichKeyRow {
    /// The key that follows the pending sequence.
    pub key: Key,
    /// What the key reaches.
    pub target: WhichKeyTarget,
}

impl WhichKeyRow {
    /// Returns the key in its help form.
    #[inline]
    pub const fn key_label(&self) -> KeyLabel {
        KeyLabel(self.key)
    }
}

/// The validated mapping registry, keyed by binding scope.
///
/// One key sequence may appear in several scopes with different commands,
/// because only one scope is active.
///
/// ```
/// use kvim_input::{Command, Mode, Registry};
/// use kvim_terminal::{Key, KeyCode};
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
    by_scope: [BTreeMap<KeySequence, Command>; BindingScope::COUNT],
}

impl Registry {
    /// Builds the registry from a binding list and validates it.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] for an empty sequence, a sequence longer than
    /// `keys_max`, a duplicate sequence inside one scope, or a strict prefix
    /// pair inside one scope.
    pub fn from_bindings(bindings: &[Binding], keys_max: u8) -> Result<Self, RegistryError> {
        let mut by_scope: [BTreeMap<KeySequence, Command>; BindingScope::COUNT] =
            std::array::from_fn(|_| BTreeMap::new());
        for binding in bindings {
            let keys = KeySequence::new(&binding.keys, keys_max).map_err(|bound| match bound {
                SequenceBound::Empty => RegistryError::EmptySequence {
                    scope: binding.scope,
                    command: binding.command,
                },
                SequenceBound::TooLong { keys, keys_max } => RegistryError::SequenceTooLong {
                    scope: binding.scope,
                    command: binding.command,
                    keys,
                    keys_max,
                },
            })?;
            match by_scope[binding.scope.index()].entry(keys) {
                Entry::Vacant(slot) => {
                    slot.insert(binding.command);
                }
                Entry::Occupied(slot) => {
                    return Err(RegistryError::DuplicateSequence {
                        scope: binding.scope,
                        keys: slot.key().clone(),
                        first: *slot.get(),
                        second: binding.command,
                    });
                }
            }
        }
        for scope in BindingScope::ALL {
            check_prefix_pairs(scope, &by_scope[scope.index()])?;
        }
        Ok(Self { by_scope })
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

    /// Returns the command that the exact sequence reaches in the scope.
    #[must_use]
    pub fn command(&self, scope: impl Into<BindingScope>, keys: &[Key]) -> Option<Command> {
        self.by_scope[scope.into().index()].get(keys).copied()
    }

    /// Reports whether the scope holds a sequence that extends the prefix.
    ///
    /// The map orders sequences lexicographically, so every extension of the
    /// prefix sorts directly after it. The smallest sequence above the prefix is
    /// therefore an extension whenever one exists.
    #[must_use]
    pub fn has_longer_sequence(&self, scope: impl Into<BindingScope>, prefix: &[Key]) -> bool {
        self.by_scope[scope.into().index()]
            .range::<[Key], _>((Bound::Excluded(prefix), Bound::Unbounded))
            .next()
            .is_some_and(|(sequence, _)| sequence.keys().starts_with(prefix))
    }

    /// Returns the distinct next keys that the prefix can still reach.
    ///
    /// The overlay lists one level at a time, so the rows hold one key each. A
    /// key that reaches exactly one command carries that command. A key that
    /// reaches several carries a group marker with the command count.
    ///
    /// The map orders sequences lexicographically, so every sequence behind one
    /// next key is contiguous, and the rows keep the deterministic key order of
    /// the registry.
    #[must_use]
    pub fn rows_for_prefix(
        &self,
        scope: impl Into<BindingScope>,
        prefix: &[Key],
    ) -> Vec<WhichKeyRow> {
        let mut rows: Vec<WhichKeyRow> = Vec::new();
        for (sequence, command) in self.by_scope[scope.into().index()]
            .range::<[Key], _>((Bound::Included(prefix), Bound::Unbounded))
            .take_while(|(sequence, _)| sequence.keys().starts_with(prefix))
            .filter(|(sequence, _)| sequence.keys().len() > prefix.len())
        {
            let key = sequence.keys()[prefix.len()];
            match rows.last_mut() {
                Some(last) if last.key == key => last.target = last.target.grown(),
                _ => rows.push(WhichKeyRow {
                    key,
                    target: WhichKeyTarget::Command(*command),
                }),
            }
        }
        rows
    }

    /// Returns every binding of one scope in sequence order.
    pub fn bindings(
        &self,
        scope: impl Into<BindingScope>,
    ) -> impl Iterator<Item = (&KeySequence, Command)> {
        self.by_scope[scope.into().index()]
            .iter()
            .map(|(keys, command)| (keys, *command))
    }
}

/// Rejects a strict prefix pair inside one scope table.
///
/// Adjacent entries are enough: every extension of one sequence sorts directly
/// after it, so a prefix pair always appears as neighbours.
fn check_prefix_pairs(
    scope: BindingScope,
    table: &BTreeMap<KeySequence, Command>,
) -> Result<(), RegistryError> {
    let mut previous: Option<(&KeySequence, Command)> = None;
    for (keys, command) in table {
        if let Some((earlier, earlier_command)) = previous
            && keys.keys().starts_with(earlier.keys())
        {
            return Err(RegistryError::AmbiguousPrefix {
                scope,
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

/// Every scope in which a text object names a range.
///
/// A waiting operator takes the object as its target, and a Visual mode takes it
/// as its selection.
const TEXT_OBJECT_SCOPES: &[BindingScope] = &[
    BindingScope::OperatorPending,
    BindingScope::Mode(Mode::Visual),
    BindingScope::Mode(Mode::VisualLine),
    BindingScope::Mode(Mode::VisualBlock),
];

/// The key of each text object, with its inner command and its around command.
///
/// The open and the close delimiter name the same object, so `i(` and `i)` both
/// reach [`Command::SelectInnerParen`].
const TEXT_OBJECTS: &[(char, Command, Command)] = &[
    ('w', Command::SelectInnerWord, Command::SelectAroundWord),
    (
        'W',
        Command::SelectInnerLongWord,
        Command::SelectAroundLongWord,
    ),
    ('(', Command::SelectInnerParen, Command::SelectAroundParen),
    (')', Command::SelectInnerParen, Command::SelectAroundParen),
    (
        '[',
        Command::SelectInnerBracket,
        Command::SelectAroundBracket,
    ),
    (
        ']',
        Command::SelectInnerBracket,
        Command::SelectAroundBracket,
    ),
    ('{', Command::SelectInnerBrace, Command::SelectAroundBrace),
    ('}', Command::SelectInnerBrace, Command::SelectAroundBrace),
    ('<', Command::SelectInnerAngle, Command::SelectAroundAngle),
    ('>', Command::SelectInnerAngle, Command::SelectAroundAngle),
    (
        '"',
        Command::SelectInnerDoubleQuote,
        Command::SelectAroundDoubleQuote,
    ),
    (
        '\'',
        Command::SelectInnerSingleQuote,
        Command::SelectAroundSingleQuote,
    ),
    (
        '`',
        Command::SelectInnerBacktick,
        Command::SelectAroundBacktick,
    ),
];

/// Every mode that `Esc` and `Ctrl-C` leave for Normal mode.
const NON_NORMAL_MODES: &[Mode] = &[
    Mode::Insert,
    Mode::Visual,
    Mode::VisualLine,
    Mode::VisualBlock,
];

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
            scope: BindingScope::Mode(mode),
            keys: keys.to_vec(),
            command,
        });
    }
}

/// Adds one binding to every named scope.
///
/// A scope list is wider than a mode list: the operator-pending scope is no
/// editor mode, but it owns a table like one.
fn add_scoped(
    bindings: &mut Vec<Binding>,
    scopes: &[BindingScope],
    keys: &[Key],
    command: Command,
) {
    for &scope in scopes {
        bindings.push(Binding {
            scope,
            keys: keys.to_vec(),
            command,
        });
    }
}

/// Adds one single-key binding of the file-tree sidebar.
fn add_tree(bindings: &mut Vec<Binding>, key: Key, command: Command) {
    add_tree_keys(bindings, &[key], command);
}

/// Adds one binding of the file-tree sidebar over a key sequence.
fn add_tree_keys(bindings: &mut Vec<Binding>, keys: &[Key], command: Command) {
    bindings.push(Binding {
        scope: BindingScope::Sidebar,
        keys: keys.to_vec(),
        command,
    });
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
        NON_NORMAL_MODES,
        &[Key::plain(KeyCode::Esc)],
        Command::ReturnToNormal,
    );
    // The reference configuration maps `<C-c>` to `<Esc>` in every one of these
    // modes, so both keys leave the mode.
    add(
        table,
        NON_NORMAL_MODES,
        &[ctrl('c')],
        Command::ReturnToNormal,
    );

    // Mode switching between the Visual modes. Vim switches with the same key
    // that enters the target mode, and the key of the active mode returns to
    // Normal mode. The editing state keeps the selection anchor, so only the
    // shape of the selection changes.
    add(
        table,
        &[Mode::VisualLine, Mode::VisualBlock],
        &[ch('v')],
        Command::EnterVisual,
    );
    add(
        table,
        &[Mode::Visual, Mode::VisualBlock],
        &[ch('V')],
        Command::EnterVisualLine,
    );
    add(
        table,
        &[Mode::Visual, Mode::VisualLine],
        &[ctrl('v')],
        Command::EnterVisualBlock,
    );
    add(table, &[Mode::Visual], &[ch('v')], Command::ReturnToNormal);
    add(
        table,
        &[Mode::VisualLine],
        &[ch('V')],
        Command::ReturnToNormal,
    );
    add(
        table,
        &[Mode::VisualBlock],
        &[ctrl('v')],
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

    add_text_object_bindings(table);
    add_operator_pending_bindings(table);
    add_tree_bindings(table);
    add_picker_bindings(table);
    bindings
}

/// Adds the `i` and `a` text objects to every scope that takes one.
fn add_text_object_bindings(table: &mut Vec<Binding>) {
    for &(key, inner, around) in TEXT_OBJECTS {
        add_scoped(table, TEXT_OBJECT_SCOPES, &[ch('i'), ch(key)], inner);
        add_scoped(table, TEXT_OBJECT_SCOPES, &[ch('a'), ch(key)], around);
    }
}

/// Adds the binding table that answers while an operator waits for its target.
///
/// The table repeats the motions of Normal mode, because an operator takes a
/// motion. It adds `Esc` and `Ctrl-C`, which reach no Normal-mode command and
/// would otherwise leave the operator waiting. It keeps `d`, `c`, and `y`,
/// because a repeated operator key means linewise. Every other Normal-mode key
/// stays out, so `d` followed by an unrelated key changes nothing. See
/// `docs/input-actions.md`.
fn add_operator_pending_bindings(table: &mut Vec<Binding>) {
    const SCOPE: &[BindingScope] = &[BindingScope::OperatorPending];
    let single = [
        (ch('h'), Command::MoveLeft),
        (ch('j'), Command::MoveDown),
        (ch('k'), Command::MoveUp),
        (ch('l'), Command::MoveRight),
        (ch('w'), Command::MoveNextWordStart),
        (ch('b'), Command::MovePreviousWordStart),
        (ch('e'), Command::MoveNextWordEnd),
        (ch('0'), Command::MoveFirstColumn),
        (ch('^'), Command::MoveFirstNonBlank),
        (ch('$'), Command::MoveLineEnd),
        (ch('G'), Command::MoveLastLine),
        (ctrl('d'), Command::MoveHalfPageDown),
        (ctrl('u'), Command::MoveHalfPageUp),
        (ctrl('f'), Command::MoveFullPageDown),
        (ctrl('b'), Command::MoveFullPageUp),
        (ch('n'), Command::SearchNext),
        (ch('N'), Command::SearchPrevious),
        (ch('d'), Command::DeleteOverMotion),
        (ch('c'), Command::ChangeOverMotion),
        (ch('y'), Command::YankOverMotion),
        (Key::plain(KeyCode::Esc), Command::ReturnToNormal),
        (ctrl('c'), Command::ReturnToNormal),
    ];
    for (key, command) in single {
        add_scoped(table, SCOPE, &[key], command);
    }
    add_scoped(table, SCOPE, &[ch('g'), ch('g')], Command::MoveFirstLine);
}

/// Adds the binding table of the picker.
///
/// The picker reads a query, so every printable key belongs to that query. Only
/// these chords reach a command. `Esc` and `Ctrl-C` close the picker through
/// the prompt, and `Enter` accepts the selected row. See
/// `docs/input-actions.md`.
fn add_picker_bindings(table: &mut Vec<Binding>) {
    for (key, command) in [
        (ctrl('j'), Command::PickerSelectNext),
        (ctrl('k'), Command::PickerSelectPrevious),
    ] {
        table.push(Binding {
            scope: BindingScope::Picker,
            keys: vec![key],
            command,
        });
    }
}

/// Adds the binding table of the file-tree sidebar.
///
/// The keys follow the reference Neo-tree subset, and the navigation keys
/// follow the buffer instead, so one row list moves like another. The sidebar
/// holds no leader sequence. `Ctrl-E` and `q` both close the sidebar, and the
/// directional focus keys leave it. See `docs/input-actions.md`.
fn add_tree_bindings(table: &mut Vec<Binding>) {
    add_tree(table, ch('j'), Command::MoveDown);
    add_tree(table, ch('k'), Command::MoveUp);
    add_tree(table, ctrl('d'), Command::MoveHalfPageDown);
    add_tree(table, ctrl('u'), Command::MoveHalfPageUp);
    add_tree(table, ctrl('f'), Command::MoveFullPageDown);
    add_tree(table, ctrl('b'), Command::MoveFullPageUp);
    add_tree_keys(table, &[ch('g'), ch('g')], Command::MoveFirstLine);
    add_tree(table, ch('G'), Command::MoveLastLine);
    add_tree(table, Key::plain(KeyCode::Enter), Command::TreeOpenEntry);
    add_tree(table, ch(' '), Command::TreeToggleEntry);
    add_tree(table, ch('l'), Command::TreeExpandEntry);
    add_tree(table, ch('h'), Command::TreeCollapseEntry);
    add_tree(table, ch('R'), Command::TreeRefresh);
    add_tree(table, ch('a'), Command::TreeAddFile);
    add_tree(table, ch('A'), Command::TreeAddDirectory);
    add_tree(table, ch('d'), Command::TreeDelete);
    add_tree(table, ch('r'), Command::TreeRename);
    add_tree(table, ch('y'), Command::TreeCopyEntry);
    add_tree(table, ch('x'), Command::TreeCutEntry);
    add_tree(table, ch('p'), Command::TreePasteEntries);
    add_tree(table, ch('H'), Command::TreeToggleHidden);
    add_tree(table, ch('/'), Command::TreeFilter);
    add_tree(
        table,
        Key::plain(KeyCode::Backspace),
        Command::TreeSelectParent,
    );
    add_tree(table, ch('q'), Command::CloseWindow);
    add_tree(table, ctrl('e'), Command::CloseWindow);
    add_tree(table, ctrl('h'), Command::FocusWindowLeft);
    add_tree(table, ctrl('j'), Command::FocusWindowDown);
    add_tree(table, ctrl('k'), Command::FocusWindowUp);
    add_tree(table, ctrl('l'), Command::FocusWindowRight);
    add_tree(table, ctrl('s'), Command::SaveBuffer);
    add_tree(table, ctrl('q'), Command::CloseWindow);
}

#[cfg(test)]
mod tests {
    use super::{
        Binding, BindingScope, Command, Key, KeyCode, Mode, Registry, RegistryError,
        WhichKeyTarget, ch, ctrl, leader,
    };

    /// The scope of Normal mode, which most rejection cases use.
    const NORMAL_SCOPE: BindingScope = BindingScope::Mode(Mode::Normal);

    fn binding(mode: Mode, keys: &[Key], command: Command) -> Binding {
        Binding {
            scope: BindingScope::Mode(mode),
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
                    scope: NORMAL_SCOPE,
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
                    scope: NORMAL_SCOPE,
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
                    scope: NORMAL_SCOPE,
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
                    scope: NORMAL_SCOPE,
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
    fn the_open_and_the_close_delimiter_name_one_text_object() {
        let registry = Registry::first_release();
        let pairs = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];
        for scope in [
            BindingScope::OperatorPending,
            BindingScope::Mode(Mode::Visual),
        ] {
            for (open, close) in pairs {
                for prefix in ['i', 'a'] {
                    let opened = registry.command(scope, &[ch(prefix), ch(open)]);
                    assert!(
                        opened.is_some(),
                        "{scope} `{prefix}{open}` must reach a text object"
                    );
                    assert_eq!(
                        opened,
                        registry.command(scope, &[ch(prefix), ch(close)]),
                        "{scope} `{prefix}{open}` and `{prefix}{close}` name one object"
                    );
                }
            }
        }
    }

    #[test]
    fn an_insert_key_starts_a_text_object_while_an_operator_waits() {
        let registry = Registry::first_release();
        assert_eq!(
            registry.command(Mode::Normal, &[ch('i')]),
            Some(Command::InsertBeforeCursor)
        );
        assert_eq!(
            registry.command(BindingScope::OperatorPending, &[ch('i')]),
            None,
            "`i` alone names no command while an operator waits"
        );
        assert!(registry.has_longer_sequence(BindingScope::OperatorPending, &[ch('i')]));
        assert_eq!(
            registry.command(BindingScope::OperatorPending, &[ch('i'), ch('w')]),
            Some(Command::SelectInnerWord)
        );
        // A repeated operator key still means linewise.
        assert_eq!(
            registry.command(BindingScope::OperatorPending, &[ch('d')]),
            Some(Command::DeleteOverMotion)
        );
    }

    #[test]
    fn which_key_rows_list_one_level_of_next_keys() {
        let registry = Registry::first_release();
        let listed = |prefix: &[Key]| {
            registry
                .rows_for_prefix(Mode::Normal, prefix)
                .iter()
                .map(|row| (row.key_label().to_string(), row.target))
                .collect::<Vec<_>>()
        };

        // The leader prefix reaches `Space f b`, `Space f f`, and `Space f /`
        // through one next key, so that key carries a group marker.
        assert!(
            listed(&[leader()]).contains(&("f".to_owned(), WhichKeyTarget::Group { commands: 3 }))
        );
        // `Space c` reaches exactly one command, so the row names it.
        assert!(listed(&[leader()]).contains(&(
            "c".to_owned(),
            WhichKeyTarget::Command(Command::ToggleFormatOnSave)
        )));
        assert!(
            listed(&[leader()])
                .iter()
                .all(|(key, _)| !key.contains(' ')),
            "a row names one key, never a sequence"
        );

        // One key further, the group opens into its own single keys.
        assert_eq!(
            listed(&[leader(), ch('f')]),
            vec![
                (
                    "/".to_owned(),
                    WhichKeyTarget::Command(Command::OpenRipgrepPicker)
                ),
                (
                    "b".to_owned(),
                    WhichKeyTarget::Command(Command::OpenBufferPicker)
                ),
                (
                    "f".to_owned(),
                    WhichKeyTarget::Command(Command::OpenFilePicker)
                ),
            ]
        );
    }

    #[test]
    fn a_group_row_names_the_number_of_commands_behind_it() {
        assert_eq!(
            WhichKeyTarget::Group { commands: 3 }.to_string(),
            "+3 commands"
        );
    }

    #[test]
    fn the_visual_modes_switch_between_each_other_and_back_to_normal() {
        let registry = Registry::first_release();
        let control_v = ctrl('v');
        let cases = [
            (Mode::Visual, ch('V'), Command::EnterVisualLine),
            (Mode::Visual, control_v, Command::EnterVisualBlock),
            (Mode::Visual, ch('v'), Command::ReturnToNormal),
            (Mode::VisualLine, ch('v'), Command::EnterVisual),
            (Mode::VisualLine, control_v, Command::EnterVisualBlock),
            (Mode::VisualLine, ch('V'), Command::ReturnToNormal),
            (Mode::VisualBlock, ch('v'), Command::EnterVisual),
            (Mode::VisualBlock, ch('V'), Command::EnterVisualLine),
            (Mode::VisualBlock, control_v, Command::ReturnToNormal),
        ];
        for (mode, key, expected) in cases {
            assert_eq!(
                registry.command(mode, &[key]),
                Some(expected),
                "{mode} `{key:?}` must reach `{expected}`"
            );
        }
    }

    #[test]
    fn ctrl_c_returns_every_non_normal_mode_to_normal_mode() {
        let registry = Registry::first_release();
        for mode in [
            Mode::Insert,
            Mode::Visual,
            Mode::VisualLine,
            Mode::VisualBlock,
        ] {
            assert_eq!(
                registry.command(mode, &[ctrl('c')]),
                Some(Command::ReturnToNormal),
                "the reference configuration maps `<C-c>` to `<Esc>` in {mode}"
            );
        }
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
