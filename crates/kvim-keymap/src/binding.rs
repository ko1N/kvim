//! Generic bindings, command ownership, and the two registry contracts.

use std::fmt;

use crate::key::Key;

/// The longest stable command identifier that a registry accepts.
///
/// A configuration file names a command by this identifier, so the value stays
/// short enough for one readable line.
pub const COMMAND_ID_BYTES_MAX: usize = 64;

/// The longest command help label that a registry accepts.
///
/// A which-key overlay shows one label for each row, so the bound keeps every
/// row inside a narrow terminal column.
pub const COMMAND_LABEL_BYTES_MAX: usize = 96;

/// The largest number of scopes that a registry accepts.
///
/// One scope owns one binding table. A host declares its scopes, and this bound
/// keeps the declared set finite.
pub const SCOPES_MAX: usize = 32;

/// The identity and the help metadata of one command.
///
/// A registry stays generic over the command type. It reads only the stable
/// identifier and the short help label, and it checks both against
/// [`COMMAND_ID_BYTES_MAX`] and [`COMMAND_LABEL_BYTES_MAX`].
///
/// ```
/// use std::fmt;
///
/// use kvim_keymap::CommandMetadata;
///
/// #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// enum HostCommand {
///     Quit,
/// }
///
/// impl fmt::Display for HostCommand {
///     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
///         formatter.write_str(self.id())
///     }
/// }
///
/// impl CommandMetadata for HostCommand {
///     fn id(&self) -> &str {
///         "quit"
///     }
///
///     fn label(&self) -> &str {
///         "Quit"
///     }
/// }
///
/// assert_eq!(HostCommand::Quit.to_string(), "quit");
/// ```
pub trait CommandMetadata: Copy + Eq + Ord + fmt::Debug + fmt::Display {
    /// Returns the stable identifier of the command.
    fn id(&self) -> &str;

    /// Returns the short help label of the command.
    fn label(&self) -> &str;
}

/// One binding table of a registry.
///
/// Exactly one scope owns keyboard input at a time, so one key sequence may
/// appear in several scopes with different commands. The implementor declares
/// how many scopes it holds, and a registry checks that count against
/// [`SCOPES_MAX`].
///
/// ```
/// use std::fmt;
///
/// use kvim_keymap::Scope;
///
/// #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// enum HostScope {
///     Global,
///     Overlay,
/// }
///
/// impl fmt::Display for HostScope {
///     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
///         formatter.write_str(match self {
///             Self::Global => "Global",
///             Self::Overlay => "Overlay",
///         })
///     }
/// }
///
/// impl Scope for HostScope {
///     const COUNT: usize = 2;
/// }
///
/// assert_eq!(HostScope::COUNT, 2);
/// ```
pub trait Scope: Copy + Eq + Ord + fmt::Debug + fmt::Display {
    /// The number of scopes that the type holds.
    const COUNT: usize;
}

/// The dispatch owner of one command.
///
/// A composed interface routes a resolved command to the host or to the focused
/// surface. The binding names that owner, so dispatch never guesses it.
///
/// ```
/// use kvim_keymap::CommandOwner;
///
/// assert_ne!(CommandOwner::Host, CommandOwner::Surface);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CommandOwner {
    /// The host executes the command.
    Host,
    /// The focused surface executes the command.
    Surface,
}

impl fmt::Display for CommandOwner {
    /// Writes the owner name that a conflict message shows.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Host => "host",
            Self::Surface => "surface",
        })
    }
}

/// One command with its dispatch owner.
///
/// A registry lookup answers with this pair, so the caller knows both the
/// command and the side that runs it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BoundCommand<C> {
    /// The command that the sequence reaches.
    pub command: C,
    /// The side that executes the command.
    pub owner: CommandOwner,
}

/// One mapping from a key sequence to a command inside one scope.
///
/// A host and every surface contribute bindings to one registry. The registry
/// validates the complete contribution list, so a conflict fails at composition
/// time instead of at dispatch time.
///
/// ```
/// use kvim_keymap::{Binding, CommandOwner, Key, KeyCode};
///
/// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// # struct HostCommand;
/// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// # struct HostScope;
/// let binding = Binding::host(HostScope, &[Key::plain(KeyCode::Esc)], HostCommand);
/// assert_eq!(binding.owner, CommandOwner::Host);
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Binding<C, S> {
    /// The scope that owns the mapping.
    pub scope: S,
    /// The key sequence that reaches the command.
    pub keys: Vec<Key>,
    /// The command that the sequence reaches.
    pub command: C,
    /// The side that executes the command.
    pub owner: CommandOwner,
}

impl<C, S> Binding<C, S> {
    /// Builds a binding that the host executes.
    ///
    /// ```
    /// use kvim_keymap::{Binding, CommandOwner, Key, KeyCode};
    ///
    /// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// # struct HostCommand;
    /// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// # struct HostScope;
    /// let binding = Binding::host(HostScope, &[Key::plain(KeyCode::Esc)], HostCommand);
    /// assert_eq!(binding.keys.len(), 1);
    /// ```
    #[must_use]
    pub fn host(scope: S, keys: &[Key], command: C) -> Self {
        Self {
            scope,
            keys: keys.to_vec(),
            command,
            owner: CommandOwner::Host,
        }
    }

    /// Builds a binding that the focused surface executes.
    ///
    /// ```
    /// use kvim_keymap::{Binding, CommandOwner, Key, KeyCode};
    ///
    /// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// # struct SurfaceCommand;
    /// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// # struct SurfaceScope;
    /// let binding = Binding::surface(
    ///     SurfaceScope,
    ///     &[Key::plain(KeyCode::Char('j'))],
    ///     SurfaceCommand,
    /// );
    /// assert_eq!(binding.owner, CommandOwner::Surface);
    /// ```
    #[must_use]
    pub fn surface(scope: S, keys: &[Key], command: C) -> Self {
        Self {
            scope,
            keys: keys.to_vec(),
            command,
            owner: CommandOwner::Surface,
        }
    }
}
