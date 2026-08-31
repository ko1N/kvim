//! The language services of one workspace.
//!
//! The type holds one persistent session for each declared server, and it
//! delivers every result through one queue. The editor asks for the sessions of
//! a path, and the registry selects the adapter. Nothing here names a language
//! or a server product.
//!
//! One adapter declares a table of servers, so one path can reach several
//! sessions. The map keys a session by the pair of the adapter identifier and
//! the declaration identifier, and every result names that pair. One server
//! that proves missing or that stops therefore leaves every other server of the
//! same language running.
//!
//! A declaration names the workspace root markers that prove that the workspace
//! uses its server. The constructor reads the root once and records the markers
//! that it holds, so a later session start reads that record and never the
//! filesystem. A server without a marker in the root starts no process and
//! holds no session budget.
//!
//! A language without a server declaration, a language whose servers this
//! workspace does not use, and a language whose servers are not installed leave
//! the editor fully usable with no diagnostics. The state is reported once and
//! never becomes an error path.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use kvim_settings::EditorSettings;

use super::server::{
    LanguageServerDeclaration, LanguageServerId, RootMarkers, ServerGate, declarations_are_valid,
};
use super::session::{
    FormatIndent, LanguageEvent, LanguageOutcome, LanguageServerHandle, SessionConfig, start,
};
use super::{AnalysisError, LanguageRegistry};
use kvim_lsp::{
    LSP_EVENT_QUEUE_CAPACITY, LSP_SESSIONS_MAX, LspBound, LspError, ProjectId, ServerId,
    ServerLaunchRequest, TransportFactory, WorkspaceRoot,
};

/// The state of one declared server in this workspace.
enum LanguageService {
    /// The session runs and accepts requests.
    Running {
        /// The editor side of the session.
        handle: LanguageServerHandle,
        /// The task that owns the server process.
        task: JoinHandle<()>,
    },
    /// The server has no service, and kvim reported the state once.
    Unavailable,
}

/// One persistent language-server session for each declared server.
///
/// The editor owns one value of this type. Every method returns without
/// waiting, so the terminal event loop never blocks on a server.
pub struct LanguageServices {
    registry: LanguageRegistry,
    root: WorkspaceRoot,
    markers: RootMarkers,
    settings: EditorSettings,
    services: HashMap<LanguageServerId, LanguageService>,
    events: mpsc::Sender<LanguageEvent>,
    results: mpsc::Receiver<LanguageEvent>,
    cancellation: CancellationToken,
    /// The project that every session of this value belongs to.
    ///
    /// The editor edits one workspace, so it owns exactly one project. The
    /// neutral records of `kvim-lsp` still carry this identity, so a later host
    /// can run several of these values without mixing their records.
    project: ProjectId,
    /// The neutral identity of the next session that this value starts.
    next_server: u64,
}

impl LanguageServices {
    /// Creates the language services of one workspace root.
    ///
    /// The caller resolves the root before it calls this constructor, because
    /// the language module performs no filesystem lookup on the event loop.
    ///
    /// The constructor reads the workspace root once, and it records every
    /// declared root marker that the root holds. The editor calls it at start,
    /// before the terminal event loop runs, so the gate of a later session
    /// start reads the recorded markers alone. See `docs/language-services.md`.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::PathEscape`] when the root is relative or holds a
    /// `.` or `..` component.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    ///
    /// use kvim_language::{LanguageRegistry, LanguageServices};
    /// use kvim_settings::EditorSettings;
    ///
    /// let services = LanguageServices::new(
    ///     LanguageRegistry::first_release(),
    ///     PathBuf::from("/work/project"),
    ///     EditorSettings::default(),
    /// )
    /// .expect("the root is absolute");
    /// assert_eq!(services.root(), std::path::Path::new("/work/project"));
    /// ```
    pub fn new(
        registry: LanguageRegistry,
        root: std::path::PathBuf,
        settings: EditorSettings,
    ) -> Result<Self, LspError> {
        let (events, results) = mpsc::channel(LSP_EVENT_QUEUE_CAPACITY);
        let root = WorkspaceRoot::new(root)?;
        let markers = RootMarkers::probe(root.path(), registry);
        Ok(Self {
            registry,
            root,
            markers,
            settings,
            services: HashMap::new(),
            events,
            results,
            cancellation: CancellationToken::new(),
            project: ProjectId::FIRST,
            next_server: 0,
        })
    }

    /// Returns the containment boundary of every served document.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// Returns the number of sessions that this workspace holds.
    ///
    /// The count is the quantity that [`LSP_SESSIONS_MAX`] bounds. A gated
    /// server enters no session, so it never raises this count. The accessor
    /// exists for the tests that prove that rule.
    #[cfg(test)]
    #[must_use]
    pub(super) fn session_count(&self) -> usize {
        self.services.len()
    }

    /// Returns the running sessions of one path, and starts them on first use.
    ///
    /// The answer holds the handles in declaration order, so every caller reads
    /// the servers in the order that the merge rules of
    /// `docs/language-services.md` require. A server that proved missing or
    /// that stopped leaves the answer, and the remaining servers keep serving
    /// the path.
    ///
    /// The sessions of one adapter start together, so a workspace that reaches
    /// [`LSP_SESSIONS_MAX`] starts no session at all for a further language.
    ///
    /// A server whose root markers the workspace does not hold never starts, so
    /// it never appears in the answer and never holds a session budget slot.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::UnsupportedPath`] when no adapter owns the path,
    /// [`LspError::NoServerDeclared`] when the adapter declares no server,
    /// [`LspError::UnusedInWorkspace`] when the workspace holds no root marker
    /// of any declared server, [`LspError::NotInstalled`] after every declared
    /// server proved missing, and [`LspError::Bounds`] when the session limit
    /// refuses a new language.
    pub fn sessions(&mut self, path: &Path) -> Result<Vec<&LanguageServerHandle>, LspError> {
        // An unsupported path and an ambiguous path both mean that no one
        // adapter owns the path, so neither starts a session.
        let adapter = self
            .registry
            .adapter(path)
            .map_err(|_: AnalysisError| LspError::UnsupportedPath)?;
        let declarations = adapter.language_servers();
        debug_assert!(
            declarations_are_valid(declarations),
            "an adapter declares at most LANGUAGE_SERVERS_MAX servers, with unique \
             identifiers and at most one formatter"
        );
        if declarations.is_empty() {
            return Err(LspError::NoServerDeclared);
        }
        // The position of a declaration in its own table is its declaration
        // order, so the gate below removes a server without renumbering the
        // servers that stay.
        let used: Vec<(LanguageServerId, &LanguageServerDeclaration)> = declarations
            .iter()
            .enumerate()
            .filter(|(_, declaration)| self.markers.gate(declaration) == ServerGate::Used)
            .map(|(order, declaration)| {
                (
                    LanguageServerId::new(adapter.id(), order, declaration.id),
                    declaration,
                )
            })
            .collect();
        if used.is_empty() {
            return Err(LspError::UnusedInWorkspace);
        }
        // Only the adapter knows the indent width of its language, and every
        // session of this adapter serves that one language, so the width is
        // resolved once here.
        let indent =
            FormatIndent::for_language(&self.settings.indent, Some(adapter.indent_rule().width));
        self.start_missing(&used, indent)?;
        let running: Vec<&LanguageServerHandle> = used
            .iter()
            .filter_map(|(id, _)| match self.services.get(id) {
                Some(LanguageService::Running { handle, .. }) => Some(handle),
                Some(LanguageService::Unavailable) | None => None,
            })
            .collect();
        if running.is_empty() {
            return Err(LspError::NotInstalled);
        }
        Ok(running)
    }

    /// Starts every session of one adapter that does not run yet.
    ///
    /// The step is atomic: it checks the session budget for the complete table
    /// before it starts the first server, so a refused language leaves no half
    /// of its sessions behind. The caller passes the servers that this
    /// workspace uses, so a gated server never counts against that budget.
    fn start_missing(
        &mut self,
        used: &[(LanguageServerId, &LanguageServerDeclaration)],
        indent: FormatIndent,
    ) -> Result<(), LspError> {
        let missing = used
            .iter()
            .filter(|(id, _)| !self.services.contains_key(id))
            .count();
        let wanted = self.services.len() + missing;
        if wanted > LSP_SESSIONS_MAX {
            return Err(LspError::Bounds {
                measure: LspBound::Sessions,
                limit: LSP_SESSIONS_MAX,
                actual: wanted,
            });
        }
        for (id, declaration) in used {
            if self.services.contains_key(id) {
                continue;
            }
            let server_id = ServerId::new(self.next_server);
            self.next_server = self.next_server.saturating_add(1);
            let service = self.start_session(*id, server_id, declaration, indent);
            self.services.insert(*id, service);
        }
        Ok(())
    }

    /// Starts one session over the declared child process.
    ///
    /// `indent` is the resolved indent of the language that the session serves,
    /// because the caller reads the adapter and the session holds no adapter.
    fn start_session(
        &self,
        id: LanguageServerId,
        server_id: ServerId,
        declaration: &LanguageServerDeclaration,
        indent: FormatIndent,
    ) -> LanguageService {
        let config = SessionConfig {
            id,
            project: self.project,
            server_id,
            language_id: declaration.language_id,
            server: declaration.program,
            formatting: declaration.formatting,
            root: self.root.clone(),
            options: declaration.options(self.settings.language),
            workspace_settings: declaration.settings(self.settings.language),
            indent,
            diagnostics_enabled: self.settings.language.diagnostics_enabled,
            registry: self.registry,
        };
        let factory = TransportFactory::process(
            ServerLaunchRequest::new(
                OsString::from(declaration.program),
                declaration.args.iter().map(OsString::from).collect(),
                WorkspaceRoot::new(self.root.path().to_path_buf())
                    .expect("the process root is valid"),
            )
            .expect("the process request is valid"),
        );
        let (handle, task) = start(
            factory,
            config,
            self.events.clone(),
            self.cancellation.child_token(),
        );
        LanguageService::Running { handle, task }
    }

    /// Takes one ready result without waiting.
    ///
    /// The call also records a server that proved unavailable or that stopped,
    /// so no later request starts that server again.
    pub fn try_recv(&mut self) -> Option<LanguageEvent> {
        let event = self.results.try_recv().ok()?;
        self.record(&event);
        Some(event)
    }

    /// Waits for the next result.
    ///
    /// The editor event loop uses this call inside its own `select`, beside the
    /// terminal event stream.
    pub async fn recv(&mut self) -> Option<LanguageEvent> {
        let event = self.results.recv().await?;
        self.record(&event);
        Some(event)
    }

    /// Cancels every session and waits for the server processes to end.
    ///
    /// The operation consumes the value, so no caller can submit after it.
    pub async fn shutdown(mut self) {
        self.cancellation.cancel();
        for (_, service) in self.services.drain() {
            if let LanguageService::Running { task, .. } = service {
                let _ = task.await;
            }
        }
    }

    /// Records a session that stopped, so the editor starts no new server.
    ///
    /// The record names one server, never one language. A language that runs
    /// several servers therefore keeps every server that still answers.
    fn record(&mut self, event: &LanguageEvent) {
        match event.outcome {
            LanguageOutcome::Unavailable | LanguageOutcome::Stopped => {
                self.services
                    .insert(event.server, LanguageService::Unavailable);
            }
            _ => {}
        }
    }
}
