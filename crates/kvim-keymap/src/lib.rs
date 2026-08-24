//! Terminal-neutral key values, validated key sequences, generic bindings, and
//! the validated binding registry.
//!
//! The crate holds no terminal, no editor, and no rendering concept. A terminal
//! adapter converts its own events into [`Key`] values, and a host composes one
//! [`Registry`] from its own commands and scopes. Every bound is explicit:
//! [`SEQUENCE_KEYS_MAX`], [`BINDINGS_MAX`], [`SCOPES_MAX`],
//! [`KEY_LABEL_BYTES_MAX`], [`COMMAND_ID_BYTES_MAX`], and
//! [`COMMAND_LABEL_BYTES_MAX`].
//!
//! A registry validates its complete contribution list at construction. A
//! duplicate sequence and an ambiguous prefix pair are typed errors, so a
//! conflict fails at composition time instead of at dispatch time.
//!
//! # Example
//!
//! ```
//! use std::fmt;
//!
//! use kvim_keymap::{Binding, CommandMetadata, Key, KeyCode, Registry, Scope};
//!
//! #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
//! enum Action {
//!     Save,
//! }
//!
//! impl fmt::Display for Action {
//!     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
//!         formatter.write_str(self.id())
//!     }
//! }
//!
//! impl CommandMetadata for Action {
//!     fn id(&self) -> &str {
//!         "save"
//!     }
//!
//!     fn label(&self) -> &str {
//!         "Save the buffer"
//!     }
//! }
//!
//! #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
//! struct Global;
//!
//! impl fmt::Display for Global {
//!     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
//!         formatter.write_str("Global")
//!     }
//! }
//!
//! impl Scope for Global {
//!     const COUNT: usize = 1;
//! }
//!
//! let keys = [Key::plain(KeyCode::Char(' ')), Key::plain(KeyCode::Char('w'))];
//! let registry = Registry::from_bindings(&[Binding::host(Global, &keys, Action::Save)], 4)?;
//!
//! assert!(registry.has_longer_sequence(Global, &keys[..1]));
//! assert_eq!(registry.command(Global, &keys), Some(Action::Save));
//! # Ok::<(), kvim_keymap::RegistryError<Action, Global>>(())
//! ```

mod binding;
mod context;
mod key;
mod registry;
mod resolver;
mod sequence;

pub use binding::{
    Binding, BoundCommand, COMMAND_ID_BYTES_MAX, COMMAND_LABEL_BYTES_MAX, CommandMetadata,
    CommandOwner, SCOPES_MAX, Scope,
};
pub use context::{ContextGeneration, InputContextSnapshot, Phase, SemanticPhases, TextFallback};
pub use key::{Chord, KEY_LABEL_BYTES_MAX, Key, KeyCode, KeyLabel};
pub use registry::{BINDINGS_MAX, Registry, RegistryError};
pub use resolver::{
    Dispatch, DispatchContext, Input, PASTE_BYTES_MAX, PasteError, PasteText, Resolver, TypedText,
    WhichKeyView,
};
pub use sequence::{KeySequence, SEQUENCE_KEYS_MAX, SequenceError};
