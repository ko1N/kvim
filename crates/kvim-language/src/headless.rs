//! Realized grammar-free diagnostics projects.

use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::Value;
use thiserror::Error;

use kvim_lsp::{
    CompletionPolicy, DefaultServerLauncher, DiagnosticsConversation, DiagnosticsHub,
    DiagnosticsServer, LanguageId, LspError, ProjectDeclaration, ProjectDriver, ProjectHandle,
    ProjectId, ProjectManager, ServerDeclaration, ServerId, ServerLaunchError, ServerLaunchRequest,
    ServerLauncher, TransportFactory, WorkspaceRoot,
};
use kvim_path::WorktreeRelativePath;
use kvim_settings::LanguageSettings;

use crate::profile::{
    DiagnosticsRegistry, DiagnosticsSelectionError, LANGUAGE_INITIALIZATION_OPTIONS_BYTES_MAX,
    LANGUAGE_WORKSPACE_SETTINGS_BYTES_MAX, LanguageServiceProfile,
};
use crate::server::{MarkerKind, RootMarkers, ServerGate};
use crate::{LANGUAGE_SERVICE_ID_BYTES_MAX, LanguageServerId};

/// The marker decision for one realized server declaration.
///
/// Active declarations are [`Self::NoMarkersRequired`] or [`Self::Matched`]. A
/// [`Self::Gated`] declaration remains visible as metadata but receives no
/// project identity and starts no launcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticsMarkerGate {
    /// The declaration needs no workspace marker.
    NoMarkersRequired,
    /// The workspace contains one of the declared markers.
    Matched {
        /// The first matching marker in declaration order.
        marker: &'static str,
        /// Whether the marker is a file or directory.
        kind: DiagnosticsMarkerKind,
    },
    /// None of the required markers exists at the workspace root.
    Gated {
        /// The required markers in declaration order.
        required: &'static [&'static str],
    },
}

/// The filesystem shape of a matched workspace marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsMarkerKind {
    /// The marker is a file or another non-directory filesystem entry.
    File,
    /// The marker is a directory.
    Directory,
}

/// One validated and realized language-server declaration.
///
/// It publishes stable Kvim identity, fallback diagnostic source, launch data,
/// protocol language identity, declared markers and gate result, realized
/// initialization options and workspace settings, completion policy, and the
/// optional neutral identity assigned when the declaration opens.
pub struct RealizedDiagnosticsServer {
    id: LanguageServerId,
    source: &'static str,
    program: &'static str,
    arguments: &'static [&'static str],
    language_id: LanguageId,
    root_markers: &'static [&'static str],
    gate: DiagnosticsMarkerGate,
    initialization_options: Value,
    workspace_settings: Option<Value>,
    completion: CompletionPolicy,
    neutral_id: Option<ServerId>,
}

impl RealizedDiagnosticsServer {
    /// Returns the stable Kvim declaration identity.
    #[must_use]
    pub const fn id(&self) -> LanguageServerId {
        self.id
    }
    /// Returns the fallback source for diagnostics without a protocol source.
    #[must_use]
    pub const fn source(&self) -> &'static str {
        self.source
    }
    /// Returns the declared executable.
    #[must_use]
    pub const fn program(&self) -> &'static str {
        self.program
    }
    /// Returns the ordered executable arguments.
    #[must_use]
    pub const fn arguments(&self) -> &'static [&'static str] {
        self.arguments
    }
    /// Returns the protocol language identifier.
    #[must_use]
    pub const fn language_id(&self) -> &LanguageId {
        &self.language_id
    }
    /// Returns the declared root markers.
    #[must_use]
    pub const fn root_markers(&self) -> &'static [&'static str] {
        self.root_markers
    }
    /// Returns the visible workspace marker decision.
    #[must_use]
    pub const fn gate(&self) -> &DiagnosticsMarkerGate {
        &self.gate
    }
    /// Returns the realized initialization options.
    #[must_use]
    pub const fn initialization_options(&self) -> &Value {
        &self.initialization_options
    }
    /// Returns the realized workspace settings, when declared.
    #[must_use]
    pub const fn workspace_settings(&self) -> Option<&Value> {
        self.workspace_settings.as_ref()
    }
    /// Returns the exact-revision diagnostics completion policy.
    #[must_use]
    pub const fn completion(&self) -> CompletionPolicy {
        self.completion
    }
    /// Returns the project-scoped server identity after this declaration opens.
    ///
    /// An unopened declaration and a marker-gated declaration return `None`.
    #[must_use]
    pub const fn neutral_id(&self) -> Option<ServerId> {
        self.neutral_id
    }
}

/// The selected profile and its realized declarations.
///
/// Selection uses one [`WorktreeRelativePath`]. It preserves every declaration
/// in source order, including marker-gated declarations.
pub struct HeadlessDiagnosticsSelection<'a> {
    profile: &'static LanguageServiceProfile,
    declarations: Vec<&'a RealizedDiagnosticsServer>,
}

impl std::fmt::Debug for HeadlessDiagnosticsSelection<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeadlessDiagnosticsSelection")
            .field("profile", &self.profile.id())
            .field("declarations", &self.declarations.len())
            .finish()
    }
}

impl<'a> HeadlessDiagnosticsSelection<'a> {
    /// Returns the stable selected profile.
    #[must_use]
    pub const fn profile(&self) -> &'static LanguageServiceProfile {
        self.profile
    }
    /// Returns the protocol language identifier used by changed-file requests.
    pub fn language_id(&self) -> Result<LanguageId, LspError> {
        let Some(first) = self.declarations.first() else {
            return Err(LspError::NoServerDeclared);
        };
        Ok(first.language_id.clone())
    }
    /// Returns every relevant declaration in source order, including gated declarations.
    #[must_use]
    pub fn declarations(&self) -> &[&'a RealizedDiagnosticsServer] {
        &self.declarations
    }
}

/// A typed failure while realizing or opening a headless diagnostics project.
#[derive(Debug, Error)]
pub enum HeadlessDiagnosticsError {
    /// The path did not select exactly one service profile.
    #[error("could not select a diagnostics profile")]
    Selection(#[source] DiagnosticsSelectionError),
    /// The workspace root is not a valid absolute containment boundary.
    #[error("invalid diagnostics workspace root")]
    Root(#[source] LspError),
    /// One realized declaration violates an LSP bound.
    #[error("invalid realized declaration {id:?}")]
    Declaration {
        /// The stable declaration identity.
        id: LanguageServerId,
        /// The typed Language Server Protocol (LSP) failure.
        #[source]
        source: LspError,
    },
    /// The matching diagnostics hub rejected the staged declaration set.
    #[error("could not compose the diagnostics hub")]
    Hub(#[source] LspError),
    /// The project manager refused the complete staged project.
    #[error("could not open the diagnostics project")]
    Open(#[source] LspError),
}

struct BoxedLauncher(Box<dyn ServerLauncher>);
impl ServerLauncher for BoxedLauncher {
    fn launch(
        &mut self,
        request: &ServerLaunchRequest,
    ) -> Result<kvim_lsp::LaunchedServer, ServerLaunchError> {
        self.0.launch(request)
    }
}

/// A realized, unopened grammar-free diagnostics project.
///
/// `new(registry, root, settings, project_id)` selects the default launcher.
/// `with_launchers(...)` stores a host launcher factory. Both validate the
/// absolute [`WorkspaceRoot`], probe each declared root marker once, and realize
/// initialization options and workspace settings before publishing state.
/// Failure before return publishes no project. Construction starts no runtime,
/// process, or task.
///
/// [`HeadlessDiagnosticsProject::select`] accepts one validated
/// [`WorktreeRelativePath`] and returns stable profile identity plus every
/// declaration in source order. Gated declarations stay visible but receive no
/// neutral project identity and reserve no process capacity.
///
/// [`HeadlessDiagnosticsProject::open`] invokes the launcher factory only for
/// ungated declarations of the selected profile. It invokes each factory once,
/// in declaration order. The returned launcher serves the first attempt and all
/// bounded restarts. The host runs the returned driver and keeps the opened
/// value for warm changed-file requests. See
/// `examples/headless_diagnostics.rs`.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use kvim_language::{DiagnosticsRegistry, HeadlessDiagnosticsProject};
/// use kvim_lsp::ProjectId;
/// use kvim_settings::LanguageSettings;
///
/// let project = HeadlessDiagnosticsProject::new(
///     DiagnosticsRegistry::first_release(),
///     PathBuf::from("/work/project"),
///     LanguageSettings::default(),
///     ProjectId::FIRST,
/// )?;
/// assert_eq!(project.root().path(), std::path::Path::new("/work/project"));
/// # Ok::<(), kvim_language::HeadlessDiagnosticsError>(())
/// ```
type LauncherFactory = dyn FnMut(&RealizedDiagnosticsServer) -> Box<dyn ServerLauncher>;

/// A grammar-free diagnostics project realized before one path is selected.
pub struct HeadlessDiagnosticsProject {
    registry: DiagnosticsRegistry,
    root: WorkspaceRoot,
    declarations: Vec<RealizedDiagnosticsServer>,
    project_id: ProjectId,
    launcher_factory: Box<LauncherFactory>,
}

impl HeadlessDiagnosticsProject {
    /// Realizes a project with the default Tokio process launcher.
    pub fn new(
        registry: DiagnosticsRegistry,
        root: PathBuf,
        settings: LanguageSettings,
        project: ProjectId,
    ) -> Result<Self, HeadlessDiagnosticsError> {
        Self::with_launchers(registry, root, settings, project, |_| {
            Box::new(DefaultServerLauncher)
        })
    }

    /// Stores a launcher factory for the profile selected later by [`Self::open`].
    ///
    /// Realization does not call the factory. `open` calls it once for each
    /// ungated declaration of only the selected profile, in declaration order.
    /// Each returned launcher serves the first attempt and all bounded restarts.
    pub fn with_launchers(
        registry: DiagnosticsRegistry,
        root: PathBuf,
        settings: LanguageSettings,
        project_id: ProjectId,
        launcher: impl FnMut(&RealizedDiagnosticsServer) -> Box<dyn ServerLauncher> + 'static,
    ) -> Result<Self, HeadlessDiagnosticsError> {
        let root = WorkspaceRoot::new(root).map_err(HeadlessDiagnosticsError::Root)?;
        let markers = RootMarkers::probe(
            root.path(),
            registry
                .profiles()
                .iter()
                .flat_map(|profile| profile.language_servers()),
        );
        let mut declarations = Vec::new();
        for profile in registry.profiles() {
            for (order, declaration) in profile.language_servers().iter().enumerate() {
                let id = LanguageServerId::new(profile.id(), order, declaration.id);
                let language_id = LanguageId::new(declaration.language_id)
                    .map_err(|source| HeadlessDiagnosticsError::Declaration { id, source })?;
                let gate = match markers.gate(declaration) {
                    ServerGate::NoMarkersRequired => DiagnosticsMarkerGate::NoMarkersRequired,
                    ServerGate::Matched { marker, kind } => DiagnosticsMarkerGate::Matched {
                        marker,
                        kind: match kind {
                            MarkerKind::File => DiagnosticsMarkerKind::File,
                            MarkerKind::Directory => DiagnosticsMarkerKind::Directory,
                        },
                    },
                    ServerGate::Unused => DiagnosticsMarkerGate::Gated {
                        required: declaration.root_markers,
                    },
                };
                let initialization_options = declaration.options(settings);
                let workspace_settings = declaration.settings(settings);
                validate_json(
                    id,
                    &initialization_options,
                    LANGUAGE_INITIALIZATION_OPTIONS_BYTES_MAX,
                )?;
                if let Some(value) = &workspace_settings {
                    validate_json(id, value, LANGUAGE_WORKSPACE_SETTINGS_BYTES_MAX)?;
                }
                declarations.push(RealizedDiagnosticsServer {
                    id,
                    source: declaration.id,
                    program: declaration.program,
                    arguments: declaration.args,
                    language_id,
                    root_markers: declaration.root_markers,
                    gate,
                    initialization_options,
                    workspace_settings,
                    completion: declaration.diagnostics_completion,
                    neutral_id: None,
                });
            }
        }

        Ok(Self {
            registry,
            root,
            declarations,
            project_id,
            launcher_factory: Box::new(launcher),
        })
    }

    /// Returns the validated workspace root.
    #[must_use]
    pub const fn root(&self) -> &WorkspaceRoot {
        &self.root
    }
    /// Returns every realized declaration before path selection.
    ///
    /// These declarations have no project-scoped [`ServerId`].
    #[must_use]
    pub fn declarations(&self) -> &[RealizedDiagnosticsServer] {
        &self.declarations
    }
    /// Selects one profile for a validated workspace-relative path.
    pub fn select(
        &self,
        path: &WorktreeRelativePath,
    ) -> Result<HeadlessDiagnosticsSelection<'_>, DiagnosticsSelectionError> {
        let profile = self.registry.profile(path.as_path())?;
        let declarations = self
            .declarations
            .iter()
            .filter(|declaration| declaration.id.adapter() == profile.id())
            .collect();
        Ok(HeadlessDiagnosticsSelection {
            profile,
            declarations,
        })
    }
    /// The factory runs only after path selection and declaration validation.
    /// A later manager refusal can occur after factory calls, but running no
    /// driver means that no returned launcher starts a process.
    pub fn open(
        mut self,
        manager: &ProjectManager,
        path: &WorktreeRelativePath,
    ) -> Result<
        (
            OpenedHeadlessDiagnosticsProject,
            ProjectDriver<DiagnosticsConversation>,
        ),
        HeadlessDiagnosticsError,
    > {
        let profile = self
            .registry
            .profile(path.as_path())
            .map_err(HeadlessDiagnosticsError::Selection)?;
        let mut declarations: Vec<_> = self
            .declarations
            .into_iter()
            .filter(|declaration| declaration.id.adapter() == profile.id())
            .collect();
        let mut next_server = 0_u64;
        for declaration in declarations
            .iter_mut()
            .filter(|declaration| !matches!(declaration.gate, DiagnosticsMarkerGate::Gated { .. }))
        {
            declaration.neutral_id = Some(ServerId::new(next_server));
            next_server = next_server
                .checked_add(1)
                .expect("bounded declarations cannot exhaust server identities");
        }

        // Complete all validation and hub registration before invoking host
        // factories. A later manager refusal drops the unopened launchers; no
        // process starts until the host runs the returned driver.
        let hub = DiagnosticsHub::new();
        let mut staged = Vec::new();
        for (index, declaration) in declarations.iter().enumerate() {
            let Some(neutral_id) = declaration.neutral_id else {
                continue;
            };
            let request = ServerLaunchRequest::new(
                OsString::from(declaration.program),
                declaration.arguments.iter().map(OsString::from).collect(),
                self.root.clone(),
            )
            .map_err(|source| HeadlessDiagnosticsError::Declaration {
                id: declaration.id,
                source,
            })?;
            let conversation = hub
                .server(DiagnosticsServer {
                    id: neutral_id,
                    source: declaration.source.to_owned(),
                    languages: vec![declaration.language_id.clone()],
                    completion: declaration.completion,
                })
                .map_err(HeadlessDiagnosticsError::Hub)?;
            staged.push((index, request, conversation));
        }

        let mut project = ProjectDeclaration::new(self.project_id, self.root.clone());
        for (index, request, conversation) in staged {
            let declaration = &declarations[index];
            let neutral_id = declaration
                .neutral_id
                .expect("only active declarations are staged");
            let launcher = (self.launcher_factory)(declaration);
            project = project.server(
                ServerDeclaration {
                    id: neutral_id,
                    transport: TransportFactory::process_with(request, BoxedLauncher(launcher)),
                    options: declaration.initialization_options.clone(),
                    workspace_settings: declaration.workspace_settings.clone(),
                },
                conversation,
            );
        }
        let (handle, driver) = manager
            .open(project)
            .map_err(HeadlessDiagnosticsError::Open)?;
        Ok((
            OpenedHeadlessDiagnosticsProject {
                root: self.root,
                declarations,
                hub,
                handle,
            },
            driver,
        ))
    }
}

fn validate_json(
    id: LanguageServerId,
    value: &Value,
    limit: usize,
) -> Result<(), HeadlessDiagnosticsError> {
    let actual = serde_json::to_vec(value)
        .expect("serde_json::Value always serializes to JSON")
        .len();
    if actual > limit {
        return Err(HeadlessDiagnosticsError::Declaration {
            id,
            source: LspError::Bounds {
                measure: kvim_lsp::LspBound::MessageBytes,
                limit,
                actual,
            },
        });
    }
    debug_assert!(
        id.server().len() <= LANGUAGE_SERVICE_ID_BYTES_MAX,
        "registry validation bounds stable source identifiers"
    );
    Ok(())
}

/// The host side of one opened warm diagnostics project.
///
/// Keep this value while the separately returned [`ProjectDriver`] runs. Its
/// [`DiagnosticsHub`] serves all changed-file requests through the same warm
/// server sessions. Stable [`LanguageServerId`] values map to project-scoped
/// neutral [`ServerId`] values only for active declarations. Consume the
/// [`ProjectHandle`] with `close().await` for graceful protocol shutdown and
/// bounded process cleanup.
pub struct OpenedHeadlessDiagnosticsProject {
    root: WorkspaceRoot,
    declarations: Vec<RealizedDiagnosticsServer>,
    hub: DiagnosticsHub,
    handle: ProjectHandle,
}

impl OpenedHeadlessDiagnosticsProject {
    /// Returns the warm diagnostics request hub.
    #[must_use]
    pub const fn hub(&self) -> &DiagnosticsHub {
        &self.hub
    }
    /// Returns the project event and cancellation handle.
    #[must_use]
    pub const fn handle(&self) -> &ProjectHandle {
        &self.handle
    }
    /// Returns the validated workspace root.
    #[must_use]
    pub const fn root(&self) -> &WorkspaceRoot {
        &self.root
    }
    /// Returns the selected realized declarations in source order.
    ///
    /// Marker-gated declarations remain visible with no project identity.
    #[must_use]
    pub fn declarations(&self) -> &[RealizedDiagnosticsServer] {
        &self.declarations
    }
    /// Maps one active neutral server identity back to stable Kvim metadata.
    #[must_use]
    pub fn declaration_for(&self, id: ServerId) -> Option<&RealizedDiagnosticsServer> {
        self.declarations
            .iter()
            .find(|declaration| declaration.neutral_id == Some(id))
    }
    /// Consumes the wrapper and returns the lower-level warm project parts.
    #[must_use]
    pub fn into_parts(self) -> (DiagnosticsHub, ProjectHandle) {
        (self.hub, self.handle)
    }
}

#[cfg(test)]
#[path = "headless_tests.rs"]
mod tests;
