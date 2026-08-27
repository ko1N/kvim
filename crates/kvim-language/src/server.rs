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

use std::collections::HashSet;
use std::path::{self, Path};

use serde_json::Value;

use kvim_settings::LanguageSettings;

use super::LanguageRegistry;

/// The largest number of servers that one language adapter declares.
///
/// One language splits its work over a type checker, a linter, and few other
/// tools. Four declarations cover that practice and still bound the merged
/// answer of one buffer.
pub const LANGUAGE_SERVERS_MAX: usize = 4;

/// The largest number of workspace root markers that one declaration names.
///
/// One linter names every file name that can hold its configuration. The
/// reference `eslint` configuration names twelve of them, so sixteen covers
/// that practice and still bounds the probe of one workspace.
pub const LANGUAGE_ROOT_MARKERS_MAX: usize = 16;

/// Whether one declared server receives the document-formatting requests.
///
/// Exactly one declaration of one adapter carries this role, so two servers of
/// one language never format the same buffer. An external formatter of the same
/// adapter takes precedence over the role, so the role names the fallback
/// formatter of its language. See `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerFormatting {
    /// The session sends the document-formatting requests to this server while
    /// its adapter declares no external formatter.
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
/// use kvim_language::{LanguageServerDeclaration, ServerFormatting};
/// use kvim_settings::LanguageSettings;
///
/// let declaration = LanguageServerDeclaration {
///     id: "example",
///     program: "example-language-server",
///     args: &["--stdio"],
///     language_id: "example",
///     formatting: ServerFormatting::Disabled,
///     root_markers: &[],
///     initialization_options: |_| serde_json::Value::Null,
///     workspace_settings: None,
/// };
/// assert_eq!(declaration.id, "example");
/// assert_eq!(declaration.language_id, "example");
/// assert_eq!(
///     declaration.options(LanguageSettings::default()),
///     serde_json::Value::Null,
/// );
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
    /// Whether this server formats the documents of its adapter while the
    /// adapter declares no external formatter.
    pub formatting: ServerFormatting,
    /// The workspace root markers that prove that the workspace uses this
    /// server.
    ///
    /// A marker names one file or one directory of the workspace root, and it
    /// carries no directory of its own. The session starts the server only
    /// when the root holds one of these names. An empty table names no marker,
    /// so the server always starts. See `docs/language-services.md`.
    pub root_markers: &'static [&'static str],
    /// Maps the language-neutral settings onto the options of this server.
    ///
    /// The function is pure. It is one of the two places that may name a
    /// setting of one concrete server.
    pub initialization_options: fn(LanguageSettings) -> Value,
    /// Maps the language-neutral settings onto the workspace settings of this
    /// server.
    ///
    /// A server that reads its behavior from the workspace configuration needs
    /// this data. The session then declares the `workspace.configuration`
    /// client capability, it sends one `workspace/didChangeConfiguration`
    /// notification after the handshake, and it answers the
    /// `workspace/configuration` request of the server.
    ///
    /// `None` names no settings, which keeps the session without a
    /// configuration channel. The function is pure, and it is the second place
    /// that may name a setting of one concrete server. See
    /// `docs/language-services.md`.
    pub workspace_settings: Option<fn(LanguageSettings) -> Value>,
}

impl LanguageServerDeclaration {
    /// Returns the initialization options for the current settings.
    #[must_use]
    pub fn options(&self, settings: LanguageSettings) -> Value {
        (self.initialization_options)(settings)
    }

    /// Returns the workspace settings for the current settings.
    ///
    /// The answer is `None` while the declaration names no settings, which
    /// leaves the session without a configuration channel.
    #[must_use]
    pub fn settings(&self, settings: LanguageSettings) -> Option<Value> {
        self.workspace_settings.map(|map| map(settings))
    }
}

/// Whether the workspace uses one declared language server.
///
/// The answer is a normal state of the workspace, never a failure. A server
/// that the workspace does not use starts no child process. See
/// `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ServerGate {
    /// The workspace uses this server, so its session may start.
    Used,
    /// The workspace holds no marker of this server, so it never starts.
    Unused,
}

/// The declared root markers that one workspace root holds.
///
/// The value is the complete filesystem knowledge of the server gate. One
/// probe fills it before the terminal event loop runs, and every later gate
/// decision reads it alone, so no gate reaches the filesystem on that loop.
pub(super) struct RootMarkers {
    /// The declared marker names that the workspace root holds.
    present: HashSet<&'static str>,
}

impl RootMarkers {
    /// Reads one workspace root and records every declared marker that it
    /// holds.
    ///
    /// The probe asks the filesystem for one path for each distinct marker of
    /// the registry, so its cost follows the adapter data and never the size of
    /// the workspace. A marker matches a file of the root, and it matches a
    /// directory of the root.
    ///
    /// The caller runs this probe once, when it creates the language services
    /// and before the terminal event loop runs. The workspace root does not
    /// change while the editor runs, so one probe answers for every buffer.
    ///
    /// A root that the process cannot read records no marker. Every gated
    /// server then stays off, and every server without a marker still starts.
    pub(super) fn probe(root: &Path, registry: LanguageRegistry) -> Self {
        let mut declared: HashSet<&'static str> = HashSet::new();
        for adapter in registry.adapters() {
            let declarations = adapter.language_servers();
            debug_assert!(
                declarations_are_valid(declarations),
                "an adapter declares at most LANGUAGE_ROOT_MARKERS_MAX root markers for one \
                 server, and each marker names one entry of the workspace root"
            );
            for declaration in declarations {
                declared.extend(declaration.root_markers);
            }
        }
        let present = declared
            .into_iter()
            .filter(|marker| root.join(marker).try_exists().unwrap_or(false))
            .collect();
        Self { present }
    }

    /// Reports whether the workspace uses the server of one declaration.
    ///
    /// The answer is pure: it reads the recorded markers and the declaration
    /// alone. A declaration that names no marker always answers
    /// [`ServerGate::Used`].
    pub(super) fn gate(&self, declaration: &LanguageServerDeclaration) -> ServerGate {
        if declaration.root_markers.is_empty() {
            return ServerGate::Used;
        }
        let used = declaration
            .root_markers
            .iter()
            .any(|marker| self.present.contains(marker));
        if used {
            ServerGate::Used
        } else {
            ServerGate::Unused
        }
    }
}

/// Reports whether one root marker names one entry of a workspace root.
///
/// A marker carries no directory component, so the probe joins it to the root
/// without leaving that root.
#[must_use]
pub(super) fn marker_is_valid(marker: &str) -> bool {
    !marker.is_empty() && marker != "." && marker != ".." && !marker.contains(path::is_separator)
}

/// Reports whether one adapter declares a valid server table.
///
/// The table holds at most [`LANGUAGE_SERVERS_MAX`] declarations, every
/// identifier is unique inside the adapter, and at most one declaration
/// formats. Each declaration names at most [`LANGUAGE_ROOT_MARKERS_MAX`] root
/// markers, and each marker names one entry of the workspace root. The rules
/// belong to `docs/language-services.md`, and a debug assertion of the services
/// checks them once for each adapter table.
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
        declaration.root_markers.len() <= LANGUAGE_ROOT_MARKERS_MAX
            && declaration
                .root_markers
                .iter()
                .all(|marker| marker_is_valid(marker))
            && declarations[..index]
                .iter()
                .all(|earlier| earlier.id != declaration.id)
    })
}
