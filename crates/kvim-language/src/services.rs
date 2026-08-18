//! The language services of one workspace.
//!
//! The type holds one persistent session for each language that declares a
//! server, and it delivers every result through one queue. The editor asks for
//! the session of a path, and the registry selects the adapter. Nothing here
//! names a language or a server product.
//!
//! A language without a server declaration, and a language whose server is not
//! installed, leave the editor fully usable with no diagnostics. The state is
//! reported once and never becomes an error path.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::OsString;
use std::path::Path;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use kvim_settings::EditorSettings;

use super::protocol::{LspError, WorkspaceRoot};
use super::session::{
    LSP_EVENT_QUEUE_CAPACITY, LanguageEvent, LanguageOutcome, LanguageServerHandle, SessionConfig,
    TransportFactory, start,
};
use super::{AnalysisError, LanguageRegistry};

/// The state of one language in this workspace.
enum LanguageService {
    /// The session runs and accepts requests.
    Running {
        /// The editor side of the session.
        handle: LanguageServerHandle,
        /// The task that owns the server process.
        task: JoinHandle<()>,
    },
    /// The language has no service, and Kvim reported the state once.
    Unavailable,
}

/// One persistent language-server session for each declared language.
///
/// The editor owns one value of this type. Every method returns without
/// waiting, so the terminal event loop never blocks on a server.
pub struct LanguageServices {
    registry: LanguageRegistry,
    root: WorkspaceRoot,
    settings: EditorSettings,
    services: HashMap<&'static str, LanguageService>,
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

    /// Returns the session that serves one path, and starts it on first use.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::UnsupportedPath`] when no adapter owns the path,
    /// [`LspError::NoServerDeclared`] when the adapter declares no server, and
    /// [`LspError::NotInstalled`] after the declared server proved missing.
    pub fn session(&mut self, path: &Path) -> Result<&LanguageServerHandle, LspError> {
        // An unsupported path and an ambiguous path both mean that no one
        // adapter owns the path, so neither starts a session.
        let adapter = self
            .registry
            .adapter(path)
            .map_err(|_: AnalysisError| LspError::UnsupportedPath)?;
        match self.services.entry(adapter.id()) {
            Entry::Occupied(entry) => match entry.into_mut() {
                LanguageService::Running { handle, .. } => Ok(handle),
                LanguageService::Unavailable => Err(LspError::NotInstalled),
            },
            Entry::Vacant(entry) => {
                let declaration = adapter
                    .language_server()
                    .ok_or(LspError::NoServerDeclared)?;
                let config = SessionConfig {
                    adapter: adapter.id(),
                    language_id: declaration.language_id,
                    server: declaration.program,
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
                match entry.insert(LanguageService::Running { handle, task }) {
                    LanguageService::Running { handle, .. } => Ok(handle),
                    LanguageService::Unavailable => Err(LspError::NotInstalled),
                }
            }
        }
    }

    /// Takes one ready result without waiting.
    ///
    /// The call also records a language whose server proved unavailable or
    /// stopped, so no later request starts that server again.
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
    fn record(&mut self, event: &LanguageEvent) {
        match event.outcome {
            LanguageOutcome::Unavailable | LanguageOutcome::Stopped => {
                self.services
                    .insert(event.adapter, LanguageService::Unavailable);
            }
            _ => {}
        }
    }
}
