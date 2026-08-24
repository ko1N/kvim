//! The which-key hints of one pending key prefix.
//!
//! A hint names one distinct next key and every command behind it. The hints
//! come from the same registry that dispatch reads, so a hint can never
//! disagree with the command that the next key reaches. The overlay shows one
//! level at a time, so a hint holds one key, never a complete sequence.
//!
//! `crates/kvim-ui/examples/which_key.rs` derives these hints from a pending
//! prefix and renders them.

use std::fmt;

use crate::binding::CommandMetadata;
use crate::key::{Key, KeyLabel};

/// What one next key of a which-key overlay reaches.
///
/// ```
/// # use std::fmt;
/// # use kvim_keymap::{CommandMetadata, WhichKeyTarget};
/// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// # struct Save;
/// # impl fmt::Display for Save {
/// #     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
/// #         formatter.write_str(self.id())
/// #     }
/// # }
/// # impl CommandMetadata for Save {
/// #     fn id(&self) -> &str {
/// #         "save"
/// #     }
/// #     fn label(&self) -> &str {
/// #         "Save the buffer"
/// #     }
/// # }
/// assert_eq!(WhichKeyTarget::Command(Save).to_string(), "Save the buffer");
/// assert_eq!(
///     WhichKeyTarget::<Save>::Group { commands: 3 }.to_string(),
///     "+3 commands"
/// );
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WhichKeyTarget<C> {
    /// The key completes exactly one command, and the row shows its label.
    Command(C),
    /// The key opens a group of commands.
    ///
    /// which-key.nvim marks such a key with a `+` prefix. The count names the
    /// distinct commands that the group holds.
    Group {
        /// The number of distinct commands behind the key, which is at least
        /// two.
        commands: usize,
    },
}

impl<C: CommandMetadata> fmt::Display for WhichKeyTarget<C> {
    /// Writes the final label of one hint.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(command) => formatter.write_str(command.label()),
            Self::Group { commands } => write!(formatter, "+{commands} commands"),
        }
    }
}

/// One which-key hint: one next key and every distinct command behind it.
///
/// A caller reads the commands to choose an icon, a color, or another
/// presentation value of its own. It reads [`WhichKeyHint::target`] for the
/// final label. The command list keeps the deterministic order of the registry,
/// and it holds each command once.
///
/// ```
/// # use std::fmt;
/// # use kvim_keymap::{Binding, CommandMetadata, Key, KeyCode, Registry, Scope, WhichKeyTarget};
/// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// # enum Action {
/// #     Files,
/// #     Buffers,
/// # }
/// # impl fmt::Display for Action {
/// #     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
/// #         formatter.write_str(self.id())
/// #     }
/// # }
/// # impl CommandMetadata for Action {
/// #     fn id(&self) -> &str {
/// #         match self {
/// #             Self::Files => "files",
/// #             Self::Buffers => "buffers",
/// #         }
/// #     }
/// #     fn label(&self) -> &str {
/// #         match self {
/// #             Self::Files => "Open the file picker",
/// #             Self::Buffers => "Open the buffer picker",
/// #         }
/// #     }
/// # }
/// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// # struct Global;
/// # impl fmt::Display for Global {
/// #     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
/// #         formatter.write_str("Global")
/// #     }
/// # }
/// # impl Scope for Global {
/// #     const COUNT: usize = 1;
/// # }
/// let leader = Key::plain(KeyCode::Char(' '));
/// let key = |value| [leader, Key::plain(KeyCode::Char('f')), Key::plain(KeyCode::Char(value))];
/// let registry = Registry::from_bindings(
///     &[
///         Binding::host(Global, &key('f'), Action::Files),
///         Binding::host(Global, &key('b'), Action::Buffers),
///     ],
///     4,
/// )?;
///
/// let hints = registry.hints_for_prefix(Global, &[leader]);
/// assert_eq!(hints.len(), 1, "one next key follows the leader");
/// assert_eq!(hints[0].key_label().to_string(), "f");
/// assert_eq!(hints[0].target(), WhichKeyTarget::Group { commands: 2 });
/// # Ok::<(), kvim_keymap::RegistryError<Action, Global>>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WhichKeyHint<C> {
    key: Key,
    commands: Vec<C>,
}

impl<C: CommandMetadata> WhichKeyHint<C> {
    /// Returns the key that follows the pending sequence.
    #[inline]
    #[must_use]
    pub const fn key(&self) -> Key {
        self.key
    }

    /// Returns the key in its help form.
    #[inline]
    #[must_use]
    pub const fn key_label(&self) -> KeyLabel {
        self.key.label()
    }

    /// Returns every distinct command behind the key, in registry order.
    ///
    /// The list is never empty, because a key without a command produces no
    /// hint.
    #[inline]
    #[must_use]
    pub fn commands(&self) -> &[C] {
        &self.commands
    }

    /// Returns what the key reaches.
    ///
    /// One command answers with its own label. Two or more answer with a group
    /// marker that counts them.
    #[must_use]
    pub fn target(&self) -> WhichKeyTarget<C> {
        match self.commands.as_slice() {
            [command] => WhichKeyTarget::Command(*command),
            commands => {
                debug_assert!(!commands.is_empty(), "a hint holds at least one command");
                WhichKeyTarget::Group {
                    commands: commands.len(),
                }
            }
        }
    }

    /// Folds the commands of one next key into one hint.
    ///
    /// The registry returns every extension of a prefix in sequence order, so
    /// the sequences behind one next key are contiguous. The fold therefore
    /// reads the extensions once and keeps the deterministic key order of the
    /// registry.
    pub(crate) fn fold(hints: &mut Vec<Self>, key: Key, command: C) {
        match hints.last_mut() {
            Some(last) if last.key == key => {
                // A key that reaches the same command through two sequences
                // still reaches one command, so the group counts it once.
                if !last.commands.contains(&command) {
                    last.commands.push(command);
                }
            }
            _ => hints.push(Self {
                key,
                commands: vec![command],
            }),
        }
    }
}
