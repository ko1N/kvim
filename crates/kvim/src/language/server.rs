//! The language-server declaration of one adapter.
//!
//! The declaration is data. The session sends what the declaration names and
//! knows no server product. Adding a language server therefore means adding one
//! declaration to one adapter, and nothing above the adapter boundary changes.
//! See `docs/language-services.md`.

use serde_json::Value;

use crate::settings::LanguageSettings;

/// The language server of one language adapter.
///
/// # Examples
///
/// ```
/// use kvim::language::{LanguageAdapter, RustAdapter};
/// use kvim::settings::LanguageSettings;
///
/// let declaration = RustAdapter::new()
///     .language_server()
///     .expect("the Rust adapter declares a language server");
/// assert_eq!(declaration.language_id, "rust");
/// // The adapter maps the language-neutral setting onto its own server option.
/// let options = declaration.options(LanguageSettings::default());
/// assert_eq!(options["check"]["command"], "clippy");
/// ```
#[derive(Clone, Copy)]
pub struct LanguageServerDeclaration {
    /// The executable that runs the server.
    pub program: &'static str,
    /// The arguments of that executable.
    pub args: &'static [&'static str],
    /// The protocol language identifier of a document of this language.
    pub language_id: &'static str,
    /// Maps the language-neutral settings onto the options of this server.
    ///
    /// The function is pure. It is the one place that may name a setting of one
    /// concrete server.
    pub initialization_options: fn(LanguageSettings) -> Value,
}

impl LanguageServerDeclaration {
    /// Returns the initialization options for the current settings.
    #[must_use]
    pub fn options(&self, settings: LanguageSettings) -> Value {
        (self.initialization_options)(settings)
    }
}
