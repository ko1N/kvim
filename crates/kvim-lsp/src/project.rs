//! The project manager, the project driver, and the bounded server supervisor.
//! Adapted from ReviewGraph (MIT), src/analysis/lsp.rs.
//!
//! One [`ProjectManager`] opens several projects. The caller names each project
//! with its own [`ProjectId`], so two projects on one root stay independent and
//! two roots never share a queue, a process, or a cancellation owner.
//!
//! [`ProjectManager::open`] starts nothing. It returns one [`ProjectHandle`] and
//! one [`ProjectDriver`]. The host runs the driver future, so this crate creates
//! no runtime and detaches no task. Dropping the handle cancels the project, and
//! [`ProjectHandle::close`] consumes the handle and waits a bounded time for the
//! driver to end.
//!
//! [`ServerSupervisor`] owns the bounded restart loop of one server: it starts
//! the process, runs the handshake, hands the live streams to one
//! [`ServerConversation`], ends the process, and starts at most
//! [`LSP_RESTARTS_MAX`] further attempts. It records every step as one
//! [`ProjectEvent`], and every event carries project identity and server
//! identity. See `docs/language-services.md`.

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::join_all;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::process::{
    Envelopes, Handshake, HandshakeOutcome, LSP_RESTARTS_MAX, ServerCapabilities, ServerInput,
    ServerProcess, ServerReport, ServerStreams, TransportFactory, initialize, shutdown,
};
use crate::protocol::{LspBound, LspError, ProtocolWriter, WorkspaceRoot, enforce};

/// The projects that one manager holds open at the same time.
///
/// A host edits few worktrees at once, and every project owns child processes
/// of its own. Eight exceeds normal practice and still bounds the processes,
/// the queues, and the documents that one manager can reach.
pub const LSP_PROJECTS_MAX: usize = 8;

/// The language-server sessions that one project runs at the same time.
///
/// One project mixes few languages, and a session starts only when a caller
/// opens a document of its language. Sixteen exceeds normal practice and still
/// bounds one project's child processes. A session owns a long-lived child that
/// no bounded process service starts, so this constant bounds those children on
/// its own. See `docs/language-services.md`.
pub const LSP_SESSIONS_MAX: usize = 16;

/// The documents that one project holds open at the same time.
///
/// One caller opens one document for each visible or recently used buffer.
/// Sixty-four exceeds normal practice and still bounds the server memory.
pub const LSP_OPEN_DOCUMENTS_MAX: usize = 64;

/// The results that one project queue holds for its host.
///
/// The value matches the result queue of `docs/responsiveness.md`, so one slow
/// frame of the host does not stall a session.
pub const LSP_EVENT_QUEUE_CAPACITY: usize = 256;

/// The server processes that every project of one manager runs together.
///
/// The value is smaller than [`LSP_PROJECTS_MAX`] times [`LSP_SESSIONS_MAX`],
/// because no host runs every language of every project at once. It bounds the
/// children of the complete manager, so one project cannot spend the budget of
/// every other project.
pub const LSP_MANAGER_PROCESSES_MAX: usize = 64;

/// The documents that every project of one manager holds open together.
///
/// The value is four times [`LSP_OPEN_DOCUMENTS_MAX`], so four projects may each
/// reserve their complete document budget and a fifth project must ask for less.
pub const LSP_MANAGER_DOCUMENTS_MAX: usize = 256;

/// The queue slots that every project of one manager reserves together.
///
/// The value is [`LSP_PROJECTS_MAX`] times [`LSP_EVENT_QUEUE_CAPACITY`], so every
/// project may reserve the complete result queue and no further project can.
pub const LSP_MANAGER_QUEUE_CAPACITY_MAX: usize = LSP_PROJECTS_MAX * LSP_EVENT_QUEUE_CAPACITY;

/// The deadline of one bounded project shutdown.
///
/// Every server of the project ends inside its own shutdown deadline, and the
/// servers end together, so two seconds covers the scheduling of a full project.
/// A driver that never ends cannot hold the caller past this value.
pub const LSP_PROJECT_CLOSE_DEADLINE: Duration = Duration::from_secs(2);

/// The caller-supplied identity of one project.
///
/// The manager never derives this value. The caller names each project, so two
/// projects on one root remain separate identities and every event, request, and
/// handle names the project that produced it.
///
/// # Examples
///
/// ```
/// use kvim_lsp::ProjectId;
///
/// let review = ProjectId::new(1);
/// let editor = ProjectId::new(2);
/// assert_ne!(review, editor);
/// assert_eq!(review.get(), 1);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProjectId(u64);

impl ProjectId {
    /// The identity of a host that opens exactly one project.
    pub const FIRST: Self = Self(0);

    /// Creates the identity that the caller names.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying value for logs and comparisons.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The caller-supplied identity of one server inside one project.
///
/// The value is unique inside its project only. Two projects may name the same
/// server, so every correlation reads [`ServerAddress`] and never this value
/// alone.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{ProjectId, ServerId};
///
/// let checker = ServerId::new(0);
/// let linter = ServerId::new(1);
/// assert!(checker < linter);
/// assert_ne!(
///     ProjectId::new(1).server(checker),
///     ProjectId::new(2).server(checker)
/// );
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServerId(u64);

impl ServerId {
    /// Creates the identity that the caller names.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying value for logs and comparisons.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The identity of one server of one project.
///
/// Every handle, every event, and every request correlation of this crate
/// carries this pair, so one failing server disables its own session alone.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{ProjectId, ServerId};
///
/// let address = ProjectId::new(7).server(ServerId::new(1));
/// assert_eq!(address.project(), ProjectId::new(7));
/// assert_eq!(address.server(), ServerId::new(1));
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServerAddress {
    project: ProjectId,
    server: ServerId,
}

impl ProjectId {
    /// Returns the address of one server of this project.
    #[must_use]
    pub const fn server(self, server: ServerId) -> ServerAddress {
        ServerAddress {
            project: self,
            server,
        }
    }
}

impl ServerAddress {
    /// Returns the project that owns the server.
    #[must_use]
    pub const fn project(self) -> ProjectId {
        self.project
    }

    /// Returns the server inside that project.
    #[must_use]
    pub const fn server(self) -> ServerId {
        self.server
    }

    /// Returns the correlation key of one request number of this server.
    #[must_use]
    pub const fn request(self, number: u64) -> RequestKey {
        RequestKey {
            address: self,
            number,
        }
    }
}

/// The correlation key of one protocol request.
///
/// Every server numbers its own requests from one, so two projects and two
/// servers of one project produce the same number. The key therefore carries
/// project identity, server identity, and the number, and no answer can reach
/// the wrong waiting caller.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{ProjectId, ServerId};
///
/// let server = ServerId::new(0);
/// let first = ProjectId::new(1).server(server).request(1);
/// let second = ProjectId::new(2).server(server).request(1);
/// assert_ne!(first, second);
/// assert_eq!(first.number(), second.number());
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestKey {
    address: ServerAddress,
    number: u64,
}

impl RequestKey {
    /// Returns the server that the request went to.
    #[must_use]
    pub const fn address(self) -> ServerAddress {
        self.address
    }

    /// Returns the protocol request number.
    #[must_use]
    pub const fn number(self) -> u64 {
        self.number
    }
}

/// The attempt of one session that produced one record.
///
/// A session restarts after a server failure, and the new server assigns its own
/// tokens and its own request numbers. The generation therefore separates the
/// records of two attempts, so a record of the attempt that failed can never
/// change visible state.
///
/// # Examples
///
/// ```
/// use kvim_lsp::SessionGeneration;
///
/// let first = SessionGeneration::FIRST;
/// assert!(first < first.next());
/// ```
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SessionGeneration(u64);

impl SessionGeneration {
    /// The generation of the first attempt of one session.
    pub const FIRST: Self = Self(0);

    /// Returns the generation of the attempt that follows this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Returns the underlying value for logs and comparisons.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One recorded step of one supervised server.
///
/// The value names no language, no editor state, and no server product. A host
/// translates it into its own outcome vocabulary.
#[derive(Debug)]
pub enum ServerEvent {
    /// The handshake completed, so the server serves its documents.
    Started,
    /// The server process recorded one fact.
    ///
    /// [`ServerSupervisor`] never records this event. The process reporter of
    /// the caller records it, because a recorder of the standard error must
    /// never wait for queue space.
    Reported(ServerReport),
    /// The declared program is not installed, so this server has no service.
    ///
    /// The state is normal. The supervisor records it once and starts no further
    /// attempt.
    Unavailable,
    /// One attempt failed with a typed cause.
    Failed(LspError),
    /// The supervisor started a new attempt after a failure.
    ///
    /// The new server holds no document. The caller must open its documents
    /// again.
    Restarted {
        /// The generation of the new attempt.
        generation: SessionGeneration,
    },
    /// The supervisor accepts no further attempt.
    Stopped,
}

/// One recorded step and the server of the project that produced it.
#[derive(Debug)]
pub struct ProjectEvent {
    /// The project and the server that produced the step.
    pub address: ServerAddress,
    /// The recorded step.
    pub event: ServerEvent,
}

impl ProjectEvent {
    /// Records one step of one server.
    #[must_use]
    pub const fn new(address: ServerAddress, event: ServerEvent) -> Self {
        Self { address, event }
    }
}

/// Receives every recorded step of one supervised server.
///
/// The sink waits for queue space, so no recorded step disappears. One
/// `tokio::sync::mpsc::Sender<ProjectEvent>` already implements the trait, and a
/// host that owns another outcome vocabulary implements it over its own queue.
pub trait ProjectEvents: Send {
    /// Records one step and waits for queue space.
    fn record(&mut self, event: ProjectEvent) -> impl Future<Output = ()> + Send;
}

impl ProjectEvents for mpsc::Sender<ProjectEvent> {
    async fn record(&mut self, event: ProjectEvent) {
        // A closed queue means that the host stopped reading. The supervisor
        // ends through its cancellation token, so the step needs no failure.
        let _ = self.send(event).await;
    }
}

/// The live streams of one server attempt.
///
/// The value borrows both halves of the transport, so the conversation writes
/// while it waits for the next frame. [`ServerSupervisor`] keeps both halves, so
/// it can run the `shutdown` sequence after the conversation ends.
pub struct Attempt<'a> {
    /// The project and the server of this attempt.
    pub address: ServerAddress,
    /// The generation of this attempt.
    pub generation: SessionGeneration,
    /// What the server confirmed in its handshake.
    pub capabilities: ServerCapabilities,
    /// The containment boundary of every path and every `file` URI.
    pub root: &'a WorkspaceRoot,
    /// The writer of every request and every notification.
    pub writer: &'a mut ProtocolWriter<ServerInput>,
    /// The frames that the reader task delivers.
    pub envelopes: &'a mut Envelopes,
    /// The cancellation owner of the project.
    pub cancellation: &'a CancellationToken,
}

/// Why one server attempt ended.
#[derive(Debug)]
pub enum AttemptEnd {
    /// The caller closed or cancelled the session, so no restart follows.
    Stopped,
    /// The attempt failed, so a bounded restart may follow.
    Failed(LspError),
}

/// Serves one attempt of one server.
///
/// The implementation owns everything above the protocol: the open documents,
/// the pending requests, the deadlines of its own queries, and the results that
/// it publishes. [`ServerSupervisor`] owns everything below: the process, the
/// handshake, the shutdown, and the bounded restart.
pub trait ServerConversation: Send {
    /// Serves one attempt until the server or the caller ends it.
    fn serve(&mut self, attempt: Attempt<'_>) -> impl Future<Output = AttemptEnd> + Send;

    /// Observes one recorded step of this server.
    ///
    /// [`ServerSupervisor`] calls this before it records the step, so a
    /// conversation learns every step that [`ServerConversation::serve`] cannot
    /// see. A server that is not installed never serves one attempt, and a
    /// caller that waits for its result must still receive one terminal
    /// outcome.
    ///
    /// The call must not wait, because the supervisor records the step next.
    /// The default implementation ignores every step.
    fn observe(&mut self, event: &ServerEvent) {
        let _ = event;
    }
}

/// Why one attempt of the supervisor ended, including the start failures.
enum AttemptOutcome {
    /// The caller closed or cancelled the session.
    Stopped,
    /// The declared program is not installed.
    NotInstalled,
    /// The attempt failed, so a bounded restart may follow.
    Failed(LspError),
}

/// One server, its transport, and the bounded restart loop that owns it.
///
/// The value starts no task. The caller awaits [`ServerSupervisor::run`] inside
/// its own future, so the supervisor ends when that future is dropped.
pub struct ServerSupervisor<'a, C, E, R> {
    /// The project and the server that this supervisor owns.
    pub address: ServerAddress,
    /// The transport of the first attempt and of every restart.
    pub factory: TransportFactory,
    /// What the client declares in the handshake of every attempt.
    pub handshake: Handshake<'a>,
    /// The conversation that serves each attempt.
    pub conversation: C,
    /// The sink of every recorded step.
    pub events: E,
    /// The sink of every recorded process fact, which never waits.
    pub report: R,
}

impl<C, E, R> ServerSupervisor<'_, C, E, R>
where
    C: ServerConversation,
    E: ProjectEvents,
    R: Fn(ServerReport) + Clone + Send + 'static,
{
    /// Runs one server and restarts it a bounded number of times.
    ///
    /// The call records [`ServerEvent::Unavailable`] and returns when the
    /// program is missing, because a missing program is a normal state that no
    /// restart repairs. Every other end records [`ServerEvent::Stopped`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::ffi::OsString;
    /// use std::path::PathBuf;
    ///
    /// use kvim_lsp::{
    ///     Attempt, AttemptEnd, Handshake, LspError, ProjectEvent, ProjectId, ServerConversation,
    ///     ServerId, ServerSupervisor, TransportFactory, WorkspaceRoot,
    /// };
    /// use serde_json::json;
    /// use tokio::sync::mpsc;
    /// use tokio_util::sync::CancellationToken;
    ///
    /// struct Idle;
    ///
    /// impl ServerConversation for Idle {
    ///     async fn serve(&mut self, attempt: Attempt<'_>) -> AttemptEnd {
    ///         attempt.cancellation.cancelled().await;
    ///         AttemptEnd::Stopped
    ///     }
    /// }
    ///
    /// # async fn drive() -> Result<(), LspError> {
    /// let root = WorkspaceRoot::new(PathBuf::from("/work/project"))?;
    /// let options = json!({});
    /// let (events, _results) = mpsc::channel::<ProjectEvent>(8);
    /// let supervisor = ServerSupervisor {
    ///     address: ProjectId::FIRST.server(ServerId::new(0)),
    ///     factory: TransportFactory::Process {
    ///         program: OsString::from("rust-analyzer"),
    ///         args: Vec::new(),
    ///         root: root.path().to_path_buf(),
    ///     },
    ///     handshake: Handshake {
    ///         root: &root,
    ///         options: &options,
    ///         settings: None,
    ///     },
    ///     conversation: Idle,
    ///     events,
    ///     report: |_report| {},
    /// };
    /// supervisor.run(&CancellationToken::new()).await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn run(mut self, cancellation: &CancellationToken) {
        let mut restarts = 0_usize;
        let mut generation = SessionGeneration::FIRST;
        loop {
            match self.attempt(generation, cancellation).await {
                AttemptOutcome::Stopped => break,
                AttemptOutcome::NotInstalled => {
                    self.record(ServerEvent::Unavailable).await;
                    return;
                }
                AttemptOutcome::Failed(error) => {
                    self.record(ServerEvent::Failed(error)).await;
                    if restarts >= LSP_RESTARTS_MAX || cancellation.is_cancelled() {
                        break;
                    }
                    restarts = restarts.saturating_add(1);
                    debug_assert!(
                        restarts <= LSP_RESTARTS_MAX,
                        "the branch above refuses every restart past the bound"
                    );
                    // The new server assigns its own progress tokens and its own
                    // request numbers, so the next attempt reports a later
                    // generation and every record of the failed attempt is
                    // obsolete.
                    generation = generation.next();
                    self.record(ServerEvent::Restarted { generation }).await;
                }
            }
        }
        self.record(ServerEvent::Stopped).await;
    }

    /// Runs one server process from its start to its end.
    async fn attempt(
        &mut self,
        generation: SessionGeneration,
        cancellation: &CancellationToken,
    ) -> AttemptOutcome {
        let opened = ServerProcess::open(&mut self.factory, self.report.clone());
        let (process, streams) = match opened {
            Ok(opened) => opened,
            Err(LspError::NotInstalled) => return AttemptOutcome::NotInstalled,
            Err(error) => return AttemptOutcome::Failed(error),
        };
        let ServerStreams {
            mut writer,
            mut envelopes,
        } = streams;
        // The handshake, the record of its success, and the conversation all
        // borrow one field of this value, so the borrows stay separate here.
        let Self {
            address,
            handshake,
            conversation,
            events,
            ..
        } = self;
        let outcome = converse(
            *address,
            handshake,
            conversation,
            events,
            generation,
            &mut writer,
            &mut envelopes,
            cancellation,
        )
        .await;
        if matches!(outcome, AttemptOutcome::Stopped) {
            // The sequence carries its own deadline, and the process ends next,
            // so a server that refuses the sequence still leaves no running
            // child.
            let _ = shutdown(&mut writer, &mut envelopes).await;
        }
        process.close().await;
        outcome
    }

    /// Records one step of this server and shows it to its conversation.
    async fn record(&mut self, event: ServerEvent) {
        self.conversation.observe(&event);
        self.events
            .record(ProjectEvent::new(self.address, event))
            .await;
    }
}

/// Runs the handshake of one attempt and then serves its conversation.
///
/// The conversation and the record sink are separate borrows of one supervisor,
/// because the record of a completed handshake runs between them.
#[expect(
    clippy::too_many_arguments,
    reason = "one attempt needs its address, its declaration, both sinks, its \
              generation, both stream halves, and its cancellation owner"
)]
async fn converse<C, E>(
    address: ServerAddress,
    handshake: &Handshake<'_>,
    conversation: &mut C,
    events: &mut E,
    generation: SessionGeneration,
    writer: &mut ProtocolWriter<ServerInput>,
    envelopes: &mut Envelopes,
    cancellation: &CancellationToken,
) -> AttemptOutcome
where
    C: ServerConversation,
    E: ProjectEvents,
{
    let capabilities = match initialize(writer, envelopes, handshake, cancellation).await {
        Ok(HandshakeOutcome::Ready(capabilities)) => capabilities,
        Ok(HandshakeOutcome::Cancelled) => return AttemptOutcome::Stopped,
        Err(error) => return AttemptOutcome::Failed(error),
    };
    // The server answered the handshake, so it serves its documents from here.
    conversation.observe(&ServerEvent::Started);
    events
        .record(ProjectEvent::new(address, ServerEvent::Started))
        .await;
    let end = conversation
        .serve(Attempt {
            address,
            generation,
            capabilities,
            root: handshake.root,
            writer,
            envelopes,
            cancellation,
        })
        .await;
    match end {
        AttemptEnd::Stopped => AttemptOutcome::Stopped,
        AttemptEnd::Failed(error) => AttemptOutcome::Failed(error),
    }
}

/// One declared server of one project.
///
/// Every member is data of the caller, so no code of this crate names one server
/// product.
pub struct ServerDeclaration {
    /// The identity of this server inside its project.
    pub id: ServerId,
    /// The transport of the first attempt and of every restart.
    pub transport: TransportFactory,
    /// The initialization options that the caller declared.
    pub options: Value,
    /// The workspace settings that the caller declared, or `None`.
    pub workspace_settings: Option<Value>,
}

/// One declared server and the conversation that serves it.
pub struct ProjectServer<C> {
    /// What the handshake of this server sends.
    pub declaration: ServerDeclaration,
    /// The conversation that serves each attempt of this server.
    pub conversation: C,
}

/// What one project reserves from its manager.
///
/// Build the value with [`ProjectDeclaration::new`] and the setters. The
/// defaults reserve the complete budget of one project, so a host that opens one
/// project names no setter.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
///
/// use kvim_lsp::{LspError, ProjectDeclaration, ProjectId, WorkspaceRoot};
///
/// let root = WorkspaceRoot::new(PathBuf::from("/work/project"))?;
/// let declaration = ProjectDeclaration::<()>::new(ProjectId::FIRST, root)
///     .open_documents(8)
///     .queue_capacity(32);
/// assert_eq!(declaration.id, ProjectId::FIRST);
/// # Ok::<(), LspError>(())
/// ```
pub struct ProjectDeclaration<C> {
    /// The identity that the caller names.
    pub id: ProjectId,
    /// The containment boundary of every path and every `file` URI.
    pub root: WorkspaceRoot,
    /// The servers of this project, in declaration order.
    pub servers: Vec<ProjectServer<C>>,
    /// The documents that this project may hold open.
    pub open_documents: usize,
    /// The result queue slots that this project reserves.
    pub queue_capacity: usize,
}

impl<C> ProjectDeclaration<C> {
    /// Declares one project that reserves the complete budget of one project.
    #[must_use]
    pub fn new(id: ProjectId, root: WorkspaceRoot) -> Self {
        Self {
            id,
            root,
            servers: Vec::new(),
            open_documents: LSP_OPEN_DOCUMENTS_MAX,
            queue_capacity: LSP_EVENT_QUEUE_CAPACITY,
        }
    }

    /// Appends one server to the declaration order of this project.
    #[must_use]
    pub fn server(mut self, declaration: ServerDeclaration, conversation: C) -> Self {
        self.servers.push(ProjectServer {
            declaration,
            conversation,
        });
        self
    }

    /// Reserves the documents that this project may hold open.
    #[must_use]
    pub const fn open_documents(mut self, documents: usize) -> Self {
        self.open_documents = documents;
        self
    }

    /// Reserves the result queue slots of this project.
    #[must_use]
    pub const fn queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = capacity;
        self
    }
}

/// The limits that one manager applies to every project together.
///
/// [`ManagerLimits::default`] names the constants of this module. A host that
/// runs beside other work lowers them.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{LSP_PROJECTS_MAX, ManagerLimits};
///
/// let limits = ManagerLimits::default();
/// assert_eq!(limits.projects, LSP_PROJECTS_MAX);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagerLimits {
    /// The projects that the manager holds open at the same time.
    pub projects: usize,
    /// The server processes of every open project together.
    pub processes: usize,
    /// The open documents of every open project together.
    pub open_documents: usize,
    /// The result queue slots of every open project together.
    pub queue_capacity: usize,
}

impl Default for ManagerLimits {
    fn default() -> Self {
        Self {
            projects: LSP_PROJECTS_MAX,
            processes: LSP_MANAGER_PROCESSES_MAX,
            open_documents: LSP_MANAGER_DOCUMENTS_MAX,
            queue_capacity: LSP_MANAGER_QUEUE_CAPACITY_MAX,
        }
    }
}

/// What every open project of one manager spends together.
struct ManagerState {
    limits: ManagerLimits,
    /// The identities that are open, so no second project takes one of them.
    open: HashSet<ProjectId>,
    processes: usize,
    open_documents: usize,
    queue_capacity: usize,
}

impl ManagerState {
    /// Reserves the budget of one project, or refuses the complete project.
    ///
    /// The reservation is atomic: it validates every quantity before it changes
    /// one of them, so a refused project leaves no partial reservation behind.
    fn reserve(
        &mut self,
        id: ProjectId,
        processes: usize,
        open_documents: usize,
        queue_capacity: usize,
    ) -> Result<(), LspError> {
        if self.open.contains(&id) {
            return Err(LspError::ProjectOpen);
        }
        enforce(processes, LSP_SESSIONS_MAX, LspBound::Sessions)?;
        enforce(
            open_documents,
            LSP_OPEN_DOCUMENTS_MAX,
            LspBound::OpenDocuments,
        )?;
        enforce(
            queue_capacity,
            LSP_EVENT_QUEUE_CAPACITY,
            LspBound::QueueCapacity,
        )?;
        enforce(
            self.open.len().saturating_add(1),
            self.limits.projects,
            LspBound::Projects,
        )?;
        enforce(
            self.processes.saturating_add(processes),
            self.limits.processes,
            LspBound::Processes,
        )?;
        enforce(
            self.open_documents.saturating_add(open_documents),
            self.limits.open_documents,
            LspBound::OpenDocuments,
        )?;
        enforce(
            self.queue_capacity.saturating_add(queue_capacity),
            self.limits.queue_capacity,
            LspBound::QueueCapacity,
        )?;
        self.open.insert(id);
        self.processes += processes;
        self.open_documents += open_documents;
        self.queue_capacity += queue_capacity;
        Ok(())
    }
}

/// The reservation that one open project holds in its manager.
///
/// The value releases the reservation when it is dropped, so a closed project, a
/// cancelled project, and a project whose host forgot it all return their budget.
struct ProjectLease {
    state: Arc<Mutex<ManagerState>>,
    id: ProjectId,
    processes: usize,
    open_documents: usize,
    queue_capacity: usize,
}

impl Drop for ProjectLease {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.open.remove(&self.id);
        state.processes = state.processes.saturating_sub(self.processes);
        state.open_documents = state.open_documents.saturating_sub(self.open_documents);
        state.queue_capacity = state.queue_capacity.saturating_sub(self.queue_capacity);
    }
}

/// The manager of every open project of one host.
///
/// The manager starts nothing and owns no task. It owns the shared budget alone,
/// so [`ProjectManager::open`] refuses a project that the budget cannot hold and
/// changes no other project.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
///
/// use kvim_lsp::{
///     Attempt, AttemptEnd, LspError, ManagerLimits, ProjectDeclaration, ProjectId, ProjectManager,
///     ServerConversation, WorkspaceRoot,
/// };
///
/// struct Idle;
///
/// impl ServerConversation for Idle {
///     async fn serve(&mut self, attempt: Attempt<'_>) -> AttemptEnd {
///         attempt.cancellation.cancelled().await;
///         AttemptEnd::Stopped
///     }
/// }
///
/// let manager = ProjectManager::new(ManagerLimits::default());
/// let root = WorkspaceRoot::new(PathBuf::from("/work/project"))?;
/// let (handle, driver) =
///     manager.open(ProjectDeclaration::<Idle>::new(ProjectId::FIRST, root))?;
/// // The host runs the driver. Dropping the handle cancels the project.
/// drop(driver);
/// drop(handle);
/// # Ok::<(), LspError>(())
/// ```
pub struct ProjectManager {
    state: Arc<Mutex<ManagerState>>,
}

impl ProjectManager {
    /// Creates one manager over the supplied limits.
    #[must_use]
    pub fn new(limits: ManagerLimits) -> Self {
        Self {
            state: Arc::new(Mutex::new(ManagerState {
                limits,
                open: HashSet::new(),
                processes: 0,
                open_documents: 0,
                queue_capacity: 0,
            })),
        }
    }

    /// Returns the projects that this manager holds open.
    #[must_use]
    pub fn projects(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .open
            .len()
    }

    /// Opens one project and returns its handle and its driver future.
    ///
    /// The call starts no process and spawns no task. The host runs the returned
    /// driver, and the handle reads the results of that project alone.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::ProjectOpen`] when one project of that identity is
    /// already open, [`LspError::DuplicateServer`] when two servers of the
    /// project take one identity, and [`LspError::Bounds`] when the declaration
    /// passes one project limit or the shared budget of the manager.
    pub fn open<C>(
        &self,
        declaration: ProjectDeclaration<C>,
    ) -> Result<(ProjectHandle, ProjectDriver<C>), LspError>
    where
        C: ServerConversation,
    {
        let ProjectDeclaration {
            id,
            root,
            servers,
            open_documents,
            queue_capacity,
        } = declaration;
        // Every request correlation reads the server identity, so two servers of
        // one identity would route the answer of one server to the other.
        let mut declared = HashSet::with_capacity(servers.len());
        for server in &servers {
            if !declared.insert(server.declaration.id) {
                return Err(LspError::DuplicateServer);
            }
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reserve(id, servers.len(), open_documents, queue_capacity)?;
        let lease = ProjectLease {
            state: Arc::clone(&self.state),
            id,
            processes: servers.len(),
            open_documents,
            queue_capacity,
        };
        let (events, results) = mpsc::channel(queue_capacity.max(1));
        let cancellation = CancellationToken::new();
        let finished = CancellationToken::new();
        let handle = ProjectHandle {
            id,
            open_documents,
            results,
            cancellation: cancellation.clone(),
            finished: finished.clone(),
            lease,
        };
        let driver = ProjectDriver {
            id,
            root,
            servers,
            events,
            cancellation,
            finished,
        };
        Ok((handle, driver))
    }
}

/// The host side of one open project.
///
/// The value reads the results of its project and owns its cancellation. Every
/// method returns without waiting, except [`ProjectHandle::recv`] and
/// [`ProjectHandle::close`].
///
/// Dropping the handle cancels the project, so a host that forgets one project
/// still leaves no running child. The cancellation is best effort: it starts the
/// shutdown and waits for nothing.
pub struct ProjectHandle {
    id: ProjectId,
    open_documents: usize,
    results: mpsc::Receiver<ProjectEvent>,
    cancellation: CancellationToken,
    finished: CancellationToken,
    /// The reservation of this project, which the drop of this value releases.
    #[expect(
        dead_code,
        reason = "the field exists for its drop, which returns the budget of \
                  this project to its manager"
    )]
    lease: ProjectLease,
}

impl ProjectHandle {
    /// Returns the identity that the caller named.
    #[must_use]
    pub const fn id(&self) -> ProjectId {
        self.id
    }

    /// Returns the documents that this project may hold open.
    #[must_use]
    pub const fn open_documents(&self) -> usize {
        self.open_documents
    }

    /// Returns the cancellation owner of this project.
    ///
    /// A conversation of this project ends when this token is cancelled, so a
    /// caller that owns a longer operation attaches a child token to it.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Takes one ready result without waiting.
    pub fn try_recv(&mut self) -> Option<ProjectEvent> {
        self.results.try_recv().ok()
    }

    /// Waits for the next result of this project.
    pub async fn recv(&mut self) -> Option<ProjectEvent> {
        self.results.recv().await
    }

    /// Cancels this project and waits a bounded time for its driver to end.
    ///
    /// The call consumes the handle, so no caller reads the project after it. It
    /// carries [`LSP_PROJECT_CLOSE_DEADLINE`], so one server that refuses to end
    /// cannot hold the caller. It cancels this project alone, so every other
    /// project of the manager keeps running.
    pub async fn close(self) {
        self.cancellation.cancel();
        let _ = time::timeout(LSP_PROJECT_CLOSE_DEADLINE, self.finished.cancelled()).await;
    }
}

impl Drop for ProjectHandle {
    fn drop(&mut self) {
        // A dropped handle can read no further result, so the project has no
        // reader left. The cancellation is best effort: every server ends
        // through its own process drop.
        self.cancellation.cancel();
    }
}

/// The future that runs every server of one project.
///
/// The host awaits [`ProjectDriver::run`]. The driver starts no task of its own,
/// so dropping the future ends every server of the project and kills every
/// child.
pub struct ProjectDriver<C> {
    id: ProjectId,
    root: WorkspaceRoot,
    servers: Vec<ProjectServer<C>>,
    events: mpsc::Sender<ProjectEvent>,
    cancellation: CancellationToken,
    finished: CancellationToken,
}

impl<C> ProjectDriver<C>
where
    C: ServerConversation,
{
    /// Returns the project that this driver serves.
    #[must_use]
    pub const fn id(&self) -> ProjectId {
        self.id
    }

    /// Runs every declared server of this project until the project ends.
    ///
    /// The servers run together inside this one future. A server that fails
    /// restarts inside its own bounds and leaves every other server running.
    pub async fn run(self) {
        let Self {
            id,
            root,
            servers,
            events,
            cancellation,
            finished,
        } = self;
        // The guard reports the end to a waiting close, and it also reports the
        // end of a driver future that the host dropped.
        let _guard = FinishGuard(finished);
        let running = servers
            .into_iter()
            .map(|server| drive_server(id, &root, server, events.clone(), cancellation.clone()));
        join_all(running).await;
    }
}

/// Reports the end of one project driver to a waiting close.
struct FinishGuard(CancellationToken);

impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Supervises one declared server of one project.
async fn drive_server<C>(
    project: ProjectId,
    root: &WorkspaceRoot,
    server: ProjectServer<C>,
    events: mpsc::Sender<ProjectEvent>,
    cancellation: CancellationToken,
) where
    C: ServerConversation,
{
    let ProjectServer {
        declaration,
        conversation,
    } = server;
    let ServerDeclaration {
        id,
        transport,
        options,
        workspace_settings,
    } = declaration;
    let address = project.server(id);
    let reports = events.clone();
    let supervisor = ServerSupervisor {
        address,
        factory: transport,
        handshake: Handshake {
            root,
            options: &options,
            settings: workspace_settings.as_ref(),
        },
        conversation,
        events,
        report: move |report| {
            // A recorder of the standard error must never wait, because a wait
            // here would stop the drain and fill the pipe of the child.
            let _ = reports.try_send(ProjectEvent::new(address, ServerEvent::Reported(report)));
        },
    };
    supervisor.run(&cancellation).await;
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
