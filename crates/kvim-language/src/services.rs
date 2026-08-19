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
//! A language without a server declaration, and a language whose servers are
//! not installed, leave the editor fully usable with no diagnostics. The state
//! is reported once and never becomes an error path.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use kvim_settings::EditorSettings;

use super::protocol::{LspBound, LspError, WorkspaceRoot};
use super::server::{LanguageServerDeclaration, LanguageServerId, declarations_are_valid};
use super::session::{
    LSP_EVENT_QUEUE_CAPACITY, LanguageEvent, LanguageOutcome, LanguageServerHandle, SessionConfig,
    TransportFactory, start,
};
use super::{AnalysisError, LanguageRegistry};

/// The language-server sessions that one workspace runs at the same time.
///
/// One workspace mixes few languages, and a session starts only when the user
/// opens a buffer of its language. Sixteen exceeds normal practice and still
/// bounds the child processes of one editor. A session owns a long-lived child
/// that no bounded process service starts, so this constant bounds those
/// children on its own. See `docs/language-services.md`.
pub const LSP_SESSIONS_MAX: usize = 16;

/// The state of one declared server in this workspace.
enum LanguageService {
    /// The session runs and accepts requests.
    Running {
        /// The editor side of the session.
        handle: LanguageServerHandle,
        /// The task that owns the server process.
        task: JoinHandle<()>,
    },
    /// The server has no service, and Kvim reported the state once.
    Unavailable,
}

/// One persistent language-server session for each declared server.
///
/// The editor owns one value of this type. Every method returns without
/// waiting, so the terminal event loop never blocks on a server.
pub struct LanguageServices {
    registry: LanguageRegistry,
    root: WorkspaceRoot,
    settings: EditorSettings,
    services: HashMap<LanguageServerId, LanguageService>,
    events: mpsc::Sender<LanguageEvent>,
    results: mpsc::Receiver<LanguageEvent>,
    cancellation: CancellationToken,
}

impl LanguageServices {
    /// Creates the language services of one workspace root.
    ///
    /// The caller resolves the root before it calls this constructor, because
    /// the language module performs no filesystem lookup on the event loop.
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
        Ok(Self {
            registry,
            root: WorkspaceRoot::new(root)?,
            settings,
            services: HashMap::new(),
            events,
            results,
            cancellation: CancellationToken::new(),
        })
    }

    /// Returns the containment boundary of every served document.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
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
    /// # Errors
    ///
    /// Returns [`LspError::UnsupportedPath`] when no adapter owns the path,
    /// [`LspError::NoServerDeclared`] when the adapter declares no server,
    /// [`LspError::NotInstalled`] after every declared server proved missing,
    /// and [`LspError::Bounds`] when the session limit refuses a new language.
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
        let ids: Vec<LanguageServerId> = declarations
            .iter()
            .enumerate()
            .map(|(order, declaration)| LanguageServerId::new(adapter.id(), order, declaration.id))
            .collect();
        self.start_missing(&ids, declarations)?;
        let running: Vec<&LanguageServerHandle> = ids
            .iter()
            .filter_map(|id| match self.services.get(id) {
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
    /// of its sessions behind.
    fn start_missing(
        &mut self,
        ids: &[LanguageServerId],
        declarations: &[LanguageServerDeclaration],
    ) -> Result<(), LspError> {
        debug_assert_eq!(
            ids.len(),
            declarations.len(),
            "one identity names one declaration of the adapter table"
        );
        let missing = ids
            .iter()
            .filter(|id| !self.services.contains_key(id))
            .count();
        let wanted = self.services.len() + missing;
        if wanted > LSP_SESSIONS_MAX {
            return Err(LspError::Bounds {
                measure: LspBound::Sessions,
                limit: LSP_SESSIONS_MAX,
                actual: wanted,
            });
        }
        for (id, declaration) in ids.iter().zip(declarations) {
            if self.services.contains_key(id) {
                continue;
            }
            let service = self.start_session(*id, declaration);
            self.services.insert(*id, service);
        }
        Ok(())
    }

    /// Starts one session over the declared child process.
    fn start_session(
        &self,
        id: LanguageServerId,
        declaration: &LanguageServerDeclaration,
    ) -> LanguageService {
        let config = SessionConfig {
            id,
            language_id: declaration.language_id,
            server: declaration.program,
            formatting: declaration.formatting,
            root: self.root.clone(),
            options: declaration.options(self.settings.language),
            indent: self.settings.indent,
            diagnostics_enabled: self.settings.language.diagnostics_enabled,
        };
        let factory = TransportFactory::Process {
            program: OsString::from(declaration.program),
            args: declaration.args.iter().map(OsString::from).collect(),
            root: self.root.path().to_path_buf(),
        };
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
