//! Editor modes, semantic commands, the mapping registry, the bounded sequence
//! resolver, which-key generation, and the command line parser.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! The crate knows no terminal type except the normalized [`Key`]. It holds no
//! buffer, file, or rendering concept. `docs/input-actions.md` owns the rules
//! that this crate implements.
//!
//! # Boundaries
//!
//! The registry maps keys to commands. It does not encode the Vim operator
//! grammar. `d`, `c`, and `y` each reach one operator command, and the
//! operator-pending state of the editor consumes the following motion. A
//! repeated operator key means linewise. `dd`, `cc`, and `yy` are therefore
//! absent from the registry: they would collide with the `d`, `c`, and `y`
//! prefixes, and the operator-times-motion product would grow the table without
//! bound. [`Command::DeleteLine`], [`Command::ChangeLine`], and
//! [`Command::YankLine`] exist for the editor to emit.
//!
//! Genuine multi-key sequences belong in the registry when no bound prefix
//! blocks them: `gg`, `gd`, `zz`, `zt`, `zb`, `]d`, `[d`, and every `Space`-led
//! sequence.
//!
//! The resolver is clock-independent. The terminal event loop measures the
//! elapsed time and passes it in.
//!
//! # Example
//!
//! ```
//! use std::time::Duration;
//!
//! use kvim_input::{Command, Registry, Resolution, Resolver};
//! use kvim_settings::InputSettings;
//! use kvim_terminal::{Key, KeyCode};
//!
//! let mut resolver = Resolver::new(Registry::first_release(), InputSettings::default());
//! let now = Duration::ZERO;
//! resolver.resolve(Key::plain(KeyCode::Char('3')), now);
//! assert!(matches!(
//!     resolver.resolve(Key::plain(KeyCode::Char('j')), now),
//!     Resolution::Command {
//!         command: Command::MoveDown,
//!         count: Some(_),
//!     }
//! ));
//! ```
//!
//! [`Key`]: kvim_terminal::Key

mod command;
mod command_line;
mod mode;
mod registry;
mod resolver;

pub use command::{Command, CommandGroup};
pub use command_line::{COMMAND_LINE_CHARS_MAX, CommandLineCommand, CommandLineError};
pub use mode::{BindingScope, InputContext, Mode, PromptKind, TreePrompt};
pub use registry::{
    Binding, KeyLabel, KeySequence, Registry, RegistryError, WhichKeyRow, WhichKeyTarget,
};
pub use resolver::{ConfirmAnswer, PromptEdit, Resolution, Resolver};
