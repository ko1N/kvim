//! Editor modes, semantic commands, the kvim binding preset, the semantic
//! reducer, which-key generation, and the command line parser.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! The crate knows no terminal type except the normalized [`Key`] of
//! `kvim-keymap`. It holds no buffer, file, or rendering concept.
//! `docs/input-actions.md` owns the rules that this crate implements.
//!
//! # Boundaries
//!
//! `kvim-keymap` owns the shared resolver, the only pending key sequence, and
//! the registry that answers dispatch, conflicts, help, and which-key. This
//! crate contributes the kvim preset to that registry and composes the Vim
//! grammar afterwards with [`SemanticReducer`]. It holds no second key table,
//! and `tests/single_key_table.rs` proves that structurally.
//!
//! The registry maps keys to commands. It does not encode the Vim operator
//! grammar. `d`, `c`, and `y` each reach one operator command, and the
//! operator-pending state consumes the following motion. A repeated operator
//! key means linewise. `dd`, `cc`, and `yy` are therefore absent from the
//! registry: they would collide with the `d`, `c`, and `y` prefixes, and the
//! operator-times-motion product would grow the table without bound.
//! [`Command::DeleteLine`], [`Command::ChangeLine`], and
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
//! use kvim_input::{Command, Key, KeyCode, Registry, Resolution, Resolver};
//! use kvim_settings::InputSettings;
//!
//! let mut resolver = Resolver::new(Registry::first_release(), InputSettings::default());
//! let now = Duration::ZERO;
//! resolver.resolve(Key::plain(KeyCode::Char('3')), now);
//! assert!(matches!(
//!     resolver.resolve(Key::plain(KeyCode::Char('j')), now),
//!     Resolution::Command {
//!         command: Command::MoveDown,
//!         count: Some(_),
//!         register: None,
//!     }
//! ));
//! ```
//!
//! [`Key`]: kvim_keymap::Key

mod command;
mod command_line;
mod edited_line;
mod mode;
mod profile;
mod reducer;
mod registry;
mod resolver;

pub use command::{Command, CommandAuthority, CommandGroup};
pub use command_line::{
    COMMAND_LINE_CHARS_MAX, CommandLineCommand, CommandLineError, CommandPathArgument,
};
pub use edited_line::{EditedLine, EditedLineError, LineChange};
pub use kvim_keymap::{Chord, Key, KeyCode, KeyLabel, KeySequence};
pub use kvim_keymap::{
    CommandOwner, ContextGeneration, Dispatch, DispatchContext, Input, InputContextSnapshot,
    PasteText, Phase, SemanticPhases, TextFallback, TypedText,
};
pub use mode::{BindingScope, InputContext, Mode, PromptKind, TreePrompt};
pub use profile::{
    BINDING_MANIFEST_ENTRIES_MAX, BINDING_OVERRIDES_MAX, BINDING_REPLACEMENTS_MAX,
    BindingInterruptionPolicy, BindingManifest, BindingManifestEntry, BindingOverride,
    BindingProfile, BindingProfileError, BindingReplacement, BindingReplacementError,
    ReviewBindingProfile,
};
pub use reducer::{Reduced, Reduction, SemanticOperation, SemanticReducer, is_register_name};
pub use registry::{Binding, Registry, RegistryError, WhichKeyRow, WhichKeyTarget};
pub use resolver::{ConfirmAnswer, ConfirmEdit, PromptEdit, Resolution, Resolver};
