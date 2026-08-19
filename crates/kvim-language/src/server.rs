//! The language-server declarations of one adapter.
//!
//! A declaration is data. The session sends what the declaration names and
//! knows no server product. Adding a language server therefore means adding one
//! declaration to one adapter, and nothing above the adapter boundary changes.
//!
//! One adapter declares a table of servers, so a language whose tools split the
//! work runs every declared server together. The position of a declaration in
//! that table is its declaration order, and every merge of two answers reads
//! that order. See `docs/language-services.md`.

use serde_json::Value;

use kvim_settings::LanguageSettings;

/// The largest number of servers that one language adapter declares.
///
/// One language splits its work over a type checker, a linter, and few other
/// tools. Four declarations cover that practice and still bound the merged
/// answer of one buffer.
pub const LANGUAGE_SERVERS_MAX: usize = 4;

/// Whether one declared server receives the document-formatting requests.
///
/// Exactly one declaration of one adapter formats, so two servers of one
/// language never format the same buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerFormatting {
    /// The session sends the document-formatting requests to this server.
    Enabled,
    /// This server never receives a document-formatting request.
    Disabled,
}

/// The identity of one declared language server.
///
/// The identity is the pair of the adapter identifier and the declaration
/// identifier. It also carries the position of the declaration in the table of
/// its adapter, because the merge rules of `docs/language-services.md` prefer
/// the earlier declaration. The derived order therefore groups the servers by
/// adapter and sorts each group by declaration.
///
/// # Examples
///
/// ```
/// use kvim_language::LanguageServerId;
///
/// let linter = LanguageServerId::new("typescript", 0, "eslint");
/// let checker = LanguageServerId::new("typescript", 1, "ts_ls");
/// assert_eq!(linter.server(), "eslint");
/// assert!(linter < checker);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LanguageServerId {
    /// The identifier of the adapter that declared the server.
    adapter: &'static str,
    /// The position of the declaration in the table of that adapter.
    order: usize,
    /// The declaration identifier, unique inside that adapter.
    server: &'static str,
}

impl LanguageServerId {
    /// Creates the identity of one declaration of one adapter.
    ///
    /// `order` is the position of the declaration in the table of the adapter.
    #[must_use]
    pub const fn new(adapter: &'static str, order: usize, server: &'static str) -> Self {
        Self {
            adapter,
            order,
            server,
        }
    }

    /// Returns the identifier of the adapter that declared the server.
    #[must_use]
    pub const fn adapter(&self) -> &'static str {
        self.adapter
    }

    /// Returns the declaration identifier, which is unique inside its adapter.
    #[must_use]
    pub const fn server(&self) -> &'static str {
        self.server
    }

    /// Returns the position of the declaration in the table of its adapter.
    #[must_use]
    pub const fn order(&self) -> usize {
        self.order
    }
}

/// One language server of one language adapter.
///
/// # Examples
///
/// ```
/// use kvim_language::{LanguageAdapter, RustAdapter};
/// use kvim_settings::LanguageSettings;
///
/// let declaration = *RustAdapter::new()
///     .language_servers()
///     .first()
///     .expect("the Rust adapter declares a language server");
/// assert_eq!(declaration.id, "rust_analyzer");
/// assert_eq!(declaration.language_id, "rust");
/// // The adapter maps the language-neutral setting onto its own server option.
/// let options = declaration.options(LanguageSettings::default());
/// assert_eq!(options["check"]["command"], "clippy");
/// ```
#[derive(Clone, Copy)]
pub struct LanguageServerDeclaration {
    /// The stable identifier of this declaration inside its adapter.
    ///
    /// The identifier keys the session of this server. It also names the
    /// producer of a diagnostic whose server sends no `source` field.
    pub id: &'static str,
    /// The executable that runs the server.
    pub program: &'static str,
    /// The arguments of that executable.
    pub args: &'static [&'static str],
    /// The protocol language identifier of a document of this language.
    pub language_id: &'static str,
    /// Whether this server formats the documents of its adapter.
    pub formatting: ServerFormatting,
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

/// Reports whether one adapter declares a valid server table.
///
/// The table holds at most [`LANGUAGE_SERVERS_MAX`] declarations, every
/// identifier is unique inside the adapter, and at most one declaration
/// formats. The rules belong to `docs/language-services.md`, and a debug
/// assertion of the services checks them once for each adapter table.
#[must_use]
pub(super) fn declarations_are_valid(declarations: &[LanguageServerDeclaration]) -> bool {
    if declarations.len() > LANGUAGE_SERVERS_MAX {
        return false;
    }
    let formatters = declarations
        .iter()
        .filter(|declaration| declaration.formatting == ServerFormatting::Enabled)
        .count();
    if formatters > 1 {
        return false;
    }
    declarations.iter().enumerate().all(|(index, declaration)| {
        declarations[..index]
            .iter()
            .all(|earlier| earlier.id != declaration.id)
    })
}
