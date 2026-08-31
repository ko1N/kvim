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

use std::collections::{HashMap, HashSet};
use std::path;

use serde_json::Value;

use kvim_lsp::CompletionPolicy;
use kvim_settings::LanguageSettings;

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
/// use kvim_language::{CompletionPolicy, LanguageServerDeclaration, ServerFormatting};
/// use kvim_settings::LanguageSettings;
///
/// let declaration = LanguageServerDeclaration {
///     id: "example",
///     program: "example-language-server",
///     args: &["--stdio"],
///     language_id: "example",
///     formatting: ServerFormatting::Disabled,
///     diagnostics_completion: CompletionPolicy::Unsupported,
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
#[derive(Clone, Copy, Debug)]
pub struct LanguageServerDeclaration {
    /// The stable identifier of this declaration inside its adapter.
    ///
    /// The identifier keys the session. It is also the fallback diagnostic
    /// source when the server omits `source`. Both uses share the
    /// `LANGUAGE_SERVICE_ID_BYTES_MAX` validation bound.
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
    /// The exact-revision diagnostics completion contract.
    ///
    /// `Pull` completes from a pull response for the requested revision.
    /// `VersionedPush` requires a matching published document version.
    /// `Unsupported` never completes from an unversioned quiet-period guess.
    pub diagnostics_completion: CompletionPolicy,
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

/// Whether one declaration is used by a workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServerGate {
    /// The declaration has no marker requirement.
    NoMarkersRequired,
    /// One declared marker exists at the root.
    Matched {
        marker: &'static str,
        kind: MarkerKind,
    },
    /// None of the declared markers exists at the root.
    Unused,
}

/// The filesystem shape of one present marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkerKind {
    File,
    Directory,
}

/// The declared root markers that one workspace root holds.
pub(crate) struct RootMarkers {
    present: HashMap<&'static str, MarkerKind>,
}

impl RootMarkers {
    /// Probes each distinct marker once with the existing unreadable-as-absent rule.
    pub(crate) fn probe<'a>(
        root: &std::path::Path,
        declarations: impl IntoIterator<Item = &'a LanguageServerDeclaration>,
    ) -> Self {
        let mut declared: HashSet<&'static str> = HashSet::new();
        for declaration in declarations {
            debug_assert!(
                declaration.root_markers.len() <= LANGUAGE_ROOT_MARKERS_MAX
                    && declaration
                        .root_markers
                        .iter()
                        .all(|marker| marker_is_valid(marker)),
                "registry validation bounds and validates every root marker"
            );
            declared.extend(declaration.root_markers);
        }
        let present = declared
            .into_iter()
            .filter_map(|marker| {
                let path = root.join(marker);
                if !path.try_exists().unwrap_or(false) {
                    return None;
                }
                let kind = if path.is_dir() {
                    MarkerKind::Directory
                } else {
                    MarkerKind::File
                };
                Some((marker, kind))
            })
            .collect();
        Self { present }
    }

    /// Returns the pure gate decision for one declaration.
    pub(crate) fn gate(&self, declaration: &LanguageServerDeclaration) -> ServerGate {
        if declaration.root_markers.is_empty() {
            return ServerGate::NoMarkersRequired;
        }
        declaration
            .root_markers
            .iter()
            .find_map(|marker| {
                self.present.get(marker).map(|kind| ServerGate::Matched {
                    marker,
                    kind: *kind,
                })
            })
            .unwrap_or(ServerGate::Unused)
    }
}

/// Reports whether one root marker names one entry of a workspace root.
///
/// A marker carries no directory component, so the probe joins it to the root
/// without leaving that root.
#[must_use]
pub(crate) fn marker_is_valid(marker: &str) -> bool {
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
#[cfg(feature = "editor-services")]
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
