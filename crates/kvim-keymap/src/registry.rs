//! The validated registry that binds a key sequence to a command.
//!
//! The registry is the only source of dispatch, conflict reports, and help. It
//! validates itself at construction, so a resolver never meets a duplicate
//! sequence or an ambiguous prefix pair.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ops::Bound;

use thiserror::Error;

use crate::binding::{
    Binding, BoundCommand, COMMAND_ID_BYTES_MAX, COMMAND_LABEL_BYTES_MAX, CommandMetadata,
    SCOPES_MAX, Scope,
};
use crate::hint::WhichKeyHint;
use crate::key::Key;
use crate::sequence::{KeySequence, SEQUENCE_KEYS_MAX, SequenceError};

/// The largest number of bindings that one registry accepts.
///
/// A host and every surface contribute to one registry. The bound keeps the
/// composed table finite, so validation and help generation stay finite too.
pub const BINDINGS_MAX: usize = 4096;

/// A rejected registry construction.
///
/// Every variant names the binding that broke the rule, so a composition root
/// can report the exact contribution.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RegistryError<C, S> {
    /// The caller asked for a sequence limit outside the accepted range.
    #[error(
        "the sequence limit is {keys_max}, but a registry accepts 1 to {SEQUENCE_KEYS_MAX} keys"
    )]
    SequenceLimitOutOfRange {
        /// The rejected limit.
        keys_max: u8,
    },
    /// The scope type declared more tables than a registry accepts.
    #[error("the scope type declares {scopes} tables, but the maximum is {SCOPES_MAX}")]
    TooManyScopes {
        /// The declared scope count.
        scopes: usize,
    },
    /// The bindings held more scopes than the scope type declares.
    #[error("the bindings hold {scopes} scopes, but the scope type declares {declared}")]
    UndeclaredScopes {
        /// The number of scopes that the bindings hold.
        scopes: usize,
        /// The scope count that the type declares.
        declared: usize,
    },
    /// The contribution list held more bindings than a registry accepts.
    #[error("the bindings hold {bindings} entries, but the maximum is {BINDINGS_MAX}")]
    TooManyBindings {
        /// The number of supplied bindings.
        bindings: usize,
    },
    /// A command identifier was longer than a registry accepts.
    #[error(
        "the identifier of `{command}` holds {bytes} bytes, but the maximum is {COMMAND_ID_BYTES_MAX}"
    )]
    CommandIdTooLong {
        /// The command that carries the identifier.
        command: C,
        /// The length of the identifier in bytes.
        bytes: usize,
    },
    /// A command label was longer than a registry accepts.
    #[error(
        "the label of `{command}` holds {bytes} bytes, but the maximum is {COMMAND_LABEL_BYTES_MAX}"
    )]
    CommandLabelTooLong {
        /// The command that carries the label.
        command: C,
        /// The length of the label in bytes.
        bytes: usize,
    },
    /// A binding held no key.
    #[error("the {scope} binding for `{command}` holds no key")]
    EmptySequence {
        /// The scope of the rejected binding.
        scope: S,
        /// The command of the rejected binding.
        command: C,
    },
    /// A binding held more keys than one pending sequence can hold.
    #[error(
        "the {scope} binding for `{command}` holds {keys} keys, but the pending-key maximum is {keys_max}"
    )]
    SequenceTooLong {
        /// The scope of the rejected binding.
        scope: S,
        /// The command of the rejected binding.
        command: C,
        /// The number of keys in the rejected binding.
        keys: usize,
        /// The pending-key maximum.
        keys_max: u8,
    },
    /// Two bindings of one scope held the same sequence.
    #[error("the {scope} sequence `{keys}` reaches both `{first}` and `{second}`")]
    DuplicateSequence {
        /// The scope that holds both bindings.
        scope: S,
        /// The repeated sequence.
        keys: KeySequence,
        /// The command of the first binding.
        first: C,
        /// The command of the second binding.
        second: C,
    },
    /// One sequence was a strict prefix of another sequence in the same scope.
    #[error(
        "the {scope} sequence `{prefix}` for `{prefix_command}` is a strict prefix of `{longer}` for `{longer_command}`"
    )]
    AmbiguousPrefix {
        /// The scope that holds both bindings.
        scope: S,
        /// The shorter sequence.
        prefix: KeySequence,
        /// The command of the shorter sequence.
        prefix_command: C,
        /// The longer sequence.
        longer: KeySequence,
        /// The command of the longer sequence.
        longer_command: C,
    },
}

/// The validated binding registry, keyed by scope.
///
/// One key sequence may appear in several scopes with different commands,
/// because only one scope owns input at a time. A snapshot is immutable, so a
/// composed interface can share it.
///
/// Equality compares every binding of every scope, so a caller keeps the
/// comparison out of a hot path.
///
/// ```
/// use std::fmt;
///
/// use kvim_keymap::{Binding, CommandMetadata, Key, KeyCode, Registry, Scope};
///
/// #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// enum Action {
///     Quit,
/// }
///
/// impl fmt::Display for Action {
///     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
///         formatter.write_str(self.id())
///     }
/// }
///
/// impl CommandMetadata for Action {
///     fn id(&self) -> &str {
///         "quit"
///     }
///
///     fn label(&self) -> &str {
///         "Quit"
///     }
/// }
///
/// #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// struct Global;
///
/// impl fmt::Display for Global {
///     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
///         formatter.write_str("Global")
///     }
/// }
///
/// impl Scope for Global {
///     const COUNT: usize = 1;
/// }
///
/// let keys = [Key::ctrl(KeyCode::Char('q'))];
/// let registry = Registry::from_bindings(&[Binding::host(Global, &keys, Action::Quit)], 4)?;
/// assert_eq!(registry.command(Global, &keys), Some(Action::Quit));
/// # Ok::<(), kvim_keymap::RegistryError<Action, Global>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Registry<C, S> {
    by_scope: BTreeMap<S, BTreeMap<KeySequence, BoundCommand<C>>>,
}

impl<C, S> Registry<C, S>
where
    C: CommandMetadata,
    S: Scope,
{
    /// Builds the registry from a contribution list and validates it.
    ///
    /// `keys_max` is the pending-sequence limit of the host. It must lie
    /// between one key and [`SEQUENCE_KEYS_MAX`].
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] for a limit outside the accepted range, a
    /// scope or binding count above its bound, command metadata above its
    /// bound, an empty sequence, a sequence longer than `keys_max`, a duplicate
    /// sequence inside one scope, or a strict prefix pair inside one scope.
    pub fn from_bindings(
        bindings: &[Binding<C, S>],
        keys_max: u8,
    ) -> Result<Self, RegistryError<C, S>> {
        if keys_max == 0 || keys_max > SEQUENCE_KEYS_MAX {
            return Err(RegistryError::SequenceLimitOutOfRange { keys_max });
        }
        if S::COUNT > SCOPES_MAX {
            return Err(RegistryError::TooManyScopes { scopes: S::COUNT });
        }
        if bindings.len() > BINDINGS_MAX {
            return Err(RegistryError::TooManyBindings {
                bindings: bindings.len(),
            });
        }
        let mut by_scope: BTreeMap<S, BTreeMap<KeySequence, BoundCommand<C>>> = BTreeMap::new();
        for binding in bindings {
            check_command_metadata(binding.command)?;
            let keys = KeySequence::new(&binding.keys, keys_max).map_err(|error| match error {
                SequenceError::Empty => RegistryError::EmptySequence {
                    scope: binding.scope,
                    command: binding.command,
                },
                SequenceError::TooLong { keys, keys_max } => RegistryError::SequenceTooLong {
                    scope: binding.scope,
                    command: binding.command,
                    keys,
                    keys_max,
                },
            })?;
            let bound = BoundCommand {
                command: binding.command,
                owner: binding.owner,
            };
            match by_scope.entry(binding.scope).or_default().entry(keys) {
                Entry::Vacant(slot) => {
                    slot.insert(bound);
                }
                Entry::Occupied(slot) => {
                    return Err(RegistryError::DuplicateSequence {
                        scope: binding.scope,
                        keys: slot.key().clone(),
                        first: slot.get().command,
                        second: binding.command,
                    });
                }
            }
        }
        if by_scope.len() > S::COUNT {
            return Err(RegistryError::UndeclaredScopes {
                scopes: by_scope.len(),
                declared: S::COUNT,
            });
        }
        for (scope, table) in &by_scope {
            check_prefix_pairs(*scope, table)?;
        }
        Ok(Self { by_scope })
    }

    /// Returns the command that the exact sequence reaches in the scope.
    ///
    /// The function answers `None` for an unbound sequence and for a scope
    /// without a table.
    #[must_use]
    pub fn command(&self, scope: S, keys: &[Key]) -> Option<C> {
        self.bound_command(scope, keys).map(|bound| bound.command)
    }

    /// Returns the command and its dispatch owner for the exact sequence.
    #[must_use]
    pub fn bound_command(&self, scope: S, keys: &[Key]) -> Option<BoundCommand<C>> {
        self.by_scope.get(&scope)?.get(keys).copied()
    }

    /// Reports whether the scope holds a sequence that extends the prefix.
    #[must_use]
    pub fn has_longer_sequence(&self, scope: S, prefix: &[Key]) -> bool {
        self.extensions_of_prefix(scope, prefix).next().is_some()
    }

    /// Returns every binding of one scope in sequence order.
    pub fn bindings(&self, scope: S) -> impl Iterator<Item = (&KeySequence, BoundCommand<C>)> {
        self.by_scope
            .get(&scope)
            .into_iter()
            .flat_map(|table| table.iter().map(|(keys, bound)| (keys, *bound)))
    }

    /// Returns every binding of one scope whose sequence is strictly longer
    /// than the prefix and starts with it.
    ///
    /// The map orders sequences lexicographically, so every extension of the
    /// prefix is contiguous and keeps the deterministic key order of the
    /// registry. A which-key overlay reads the same order.
    pub fn extensions_of_prefix(
        &self,
        scope: S,
        prefix: &[Key],
    ) -> impl Iterator<Item = (&KeySequence, BoundCommand<C>)> {
        self.by_scope
            .get(&scope)
            .into_iter()
            .flat_map(move |table| {
                table
                    .range::<[Key], _>((Bound::Excluded(prefix), Bound::Unbounded))
                    .take_while(move |(keys, _)| keys.keys().starts_with(prefix))
                    .map(|(keys, bound)| (keys, *bound))
            })
    }

    /// Returns one which-key hint for each distinct next key of the prefix.
    ///
    /// The overlay shows one level at a time, so every hint holds one key. A
    /// key that reaches exactly one command carries that command, and a key
    /// that reaches several carries all of them. The hints keep the
    /// deterministic key order of the registry.
    #[must_use]
    pub fn hints_for_prefix(&self, scope: S, prefix: &[Key]) -> Vec<WhichKeyHint<C>> {
        let mut hints = Vec::new();
        for (sequence, bound) in self.extensions_of_prefix(scope, prefix) {
            let Some(key) = sequence.keys().get(prefix.len()).copied() else {
                debug_assert!(
                    false,
                    "an extension of a prefix holds at least one more key"
                );
                continue;
            };
            WhichKeyHint::fold(&mut hints, key, bound.command);
        }
        hints
    }

    /// Returns the number of bindings that the registry holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_scope.values().map(BTreeMap::len).sum()
    }

    /// Reports whether the registry holds no binding.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_scope.values().all(BTreeMap::is_empty)
    }
}

/// Rejects command metadata above its byte bound.
fn check_command_metadata<C, S>(command: C) -> Result<(), RegistryError<C, S>>
where
    C: CommandMetadata,
{
    let id_bytes = command.id().len();
    if id_bytes > COMMAND_ID_BYTES_MAX {
        return Err(RegistryError::CommandIdTooLong {
            command,
            bytes: id_bytes,
        });
    }
    let label_bytes = command.label().len();
    if label_bytes > COMMAND_LABEL_BYTES_MAX {
        return Err(RegistryError::CommandLabelTooLong {
            command,
            bytes: label_bytes,
        });
    }
    Ok(())
}

/// Rejects a strict prefix pair inside one scope table.
///
/// Adjacent entries are enough: every extension of one sequence sorts directly
/// after it, so a prefix pair always appears as neighbours.
fn check_prefix_pairs<C, S>(
    scope: S,
    table: &BTreeMap<KeySequence, BoundCommand<C>>,
) -> Result<(), RegistryError<C, S>>
where
    C: CommandMetadata,
    S: Scope,
{
    let mut previous: Option<(&KeySequence, C)> = None;
    for (keys, bound) in table {
        if let Some((earlier, earlier_command)) = previous
            && keys.keys().starts_with(earlier.keys())
        {
            return Err(RegistryError::AmbiguousPrefix {
                scope,
                prefix: earlier.clone(),
                prefix_command: earlier_command,
                longer: keys.clone(),
                longer_command: bound.command,
            });
        }
        previous = Some((keys, bound.command));
    }
    Ok(())
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
