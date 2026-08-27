//! Optional worktree-backed editor facade.
//!
//! `crates/kvim-embed/examples/worktree_editor.rs` demonstrates its complete
//! lifecycle.

use std::error::Error as StdError;
use std::fmt;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kvim_input::{
    BindingManifest, BindingProfile, BindingScope, Command, CommandOwner, ContextGeneration,
    Dispatch, InputContextSnapshot, Key, Mode, PasteText, TypedText, is_register_name,
};
use kvim_language::{LanguageRegistry, LanguageServices};
use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{FileWatcher, RuntimeLimits};
use kvim_settings::EditorSettings;
use kvim_tui::__private::{
    ClipboardAccess as TuiClipboardAccess, Completed, CursorShape as TuiCursorShape,
    EditorAccess as TuiEditorAccess, EditorCapacity as TuiEditorCapacity,
    EditorEvent as TuiEditorEvent, EditorShutdown as TuiEditorShutdown, EmbeddedEditor,
    GeometryError as TuiGeometryError, HostReportRequest as TuiHostReportRequest,
    HostWorkspace as TuiHostWorkspace, InputRequest as TuiInputRequest,
    PublishedEvent as TuiPublishedEvent, Redraw as TuiRedraw, Reduction as TuiReduction,
    ReductionOutcome as TuiReductionOutcome, Refusal as TuiRefusal, RunState as TuiRunState,
    TerminalEvent as TuiTerminalEvent,
};
use kvim_ui::Direction;
use kvim_workspace::{EntryKind, FileOperation, TransferMode};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use thiserror::Error;
use tokio::runtime::Runtime as TokioRuntime;

/// The maximum number of queued background completions.
pub const COMPLETION_CAPACITY_MAX: usize = kvim_runtime::EVENT_QUEUE_CAPACITY_MAX;
/// The maximum number of concurrent blocking jobs.
pub const WORKER_CAPACITY_MAX: usize = kvim_runtime::WORKER_CONCURRENCY_LIMIT_MAX;
/// The maximum number of concurrent child processes.
pub const PROCESS_CAPACITY_MAX: usize = kvim_runtime::PROCESS_CONCURRENCY_LIMIT;
/// The maximum number of queued editor events.
pub const EVENT_CAPACITY_MAX: usize = kvim_tui::__private::EDITOR_EVENTS_MAX;

/// Validated bounded execution capacity for one editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorktreeCapacity {
    completions: usize,
    workers: usize,
    processes: usize,
}

impl WorktreeCapacity {
    /// Creates explicit nonzero capacities within the published maxima.
    pub const fn new(
        completions: usize,
        workers: usize,
        processes: usize,
    ) -> Result<Self, CapacityError> {
        if completions == 0 || completions > COMPLETION_CAPACITY_MAX {
            return Err(CapacityError::Completions);
        }
        if workers == 0 || workers > WORKER_CAPACITY_MAX {
            return Err(CapacityError::Workers);
        }
        if processes == 0 || processes > PROCESS_CAPACITY_MAX {
            return Err(CapacityError::Processes);
        }
        Ok(Self {
            completions,
            workers,
            processes,
        })
    }
}

impl Default for WorktreeCapacity {
    fn default() -> Self {
        let limits = RuntimeLimits::default();
        Self {
            completions: limits.event_queue(),
            workers: limits.workers(),
            processes: limits.processes(),
        }
    }
}

/// One invalid execution-capacity dimension.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CapacityError {
    /// Completion capacity is zero or too large.
    #[error("completion capacity is outside its published bounds")]
    Completions,
    /// Worker capacity is zero or too large.
    #[error("worker capacity is outside its published bounds")]
    Workers,
    /// Process capacity is zero or too large.
    #[error("process capacity is outside its published bounds")]
    Processes,
}

/// What filesystem and text access the host grants.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorktreeAccess {
    /// Permit editing and filesystem mutations.
    #[default]
    ReadWrite,
    /// Permit viewing only.
    ViewOnly,
}

/// Policy for an optional built-in service.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ServicePolicy {
    /// Do not construct or start the service.
    #[default]
    Disabled,
    /// Use kvim's production built-in service and fail opening if it cannot start.
    BuiltIn,
    /// Use kvim's production built-in service, but continue without it if startup fails.
    BestEffortBuiltIn,
}

/// Explicit optional capabilities of one worktree editor.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorktreeCapabilities {
    /// Git status process capability.
    pub git: ServicePolicy,
    /// Filesystem watcher capability.
    pub watcher: ServicePolicy,
    /// Language analysis and Language Server Protocol capability.
    pub language: ServicePolicy,
    /// System clipboard process capability.
    pub clipboard: ServicePolicy,
}

/// The kind of a created workspace entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceEntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
}

/// Whether a transfer keeps its sources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceTransfer {
    /// Keep each source.
    Copy,
    /// Remove each source after transfer.
    Move,
}

/// The maximum number of paths in one published workspace operation.
pub const WORKSPACE_OPERATION_PATHS_MAX: usize = kvim_workspace::MUTATION_PATHS_MAX;

/// The discoverable kind of one workspace operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceOperationKind {
    /// Create one entry.
    Create,
    /// Delete entries.
    Delete,
    /// Rename one entry.
    Rename,
    /// Copy or move entries.
    Transfer,
}

/// Facade-owned description of one bounded workspace operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceOperation {
    data: WorkspaceOperationData,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkspaceOperationData {
    Create {
        path: WorktreeRelativePath,
        kind: WorkspaceEntryKind,
    },
    Delete {
        paths: Vec<WorktreeRelativePath>,
    },
    Rename {
        from: WorktreeRelativePath,
        to: WorktreeRelativePath,
    },
    Transfer {
        mode: WorkspaceTransfer,
        sources: Vec<WorktreeRelativePath>,
        destination: Option<WorktreeRelativePath>,
    },
}

impl WorkspaceOperation {
    /// Returns the operation kind.
    #[must_use]
    pub const fn kind(&self) -> WorkspaceOperationKind {
        match &self.data {
            WorkspaceOperationData::Create { .. } => WorkspaceOperationKind::Create,
            WorkspaceOperationData::Delete { .. } => WorkspaceOperationKind::Delete,
            WorkspaceOperationData::Rename { .. } => WorkspaceOperationKind::Rename,
            WorkspaceOperationData::Transfer { .. } => WorkspaceOperationKind::Transfer,
        }
    }

    /// Returns the created path and entry kind, when this is a create operation.
    #[must_use]
    pub const fn create(&self) -> Option<(&WorktreeRelativePath, WorkspaceEntryKind)> {
        match &self.data {
            WorkspaceOperationData::Create { path, kind } => Some((path, *kind)),
            _ => None,
        }
    }

    /// Returns the deleted paths, when this is a delete operation.
    #[must_use]
    pub fn deleted_paths(&self) -> Option<&[WorktreeRelativePath]> {
        match &self.data {
            WorkspaceOperationData::Delete { paths } => Some(paths),
            _ => None,
        }
    }

    /// Returns the old and new paths, when this is a rename operation.
    #[must_use]
    pub const fn rename(&self) -> Option<(&WorktreeRelativePath, &WorktreeRelativePath)> {
        match &self.data {
            WorkspaceOperationData::Rename { from, to } => Some((from, to)),
            _ => None,
        }
    }

    /// Returns transfer facts, when this is a transfer operation.
    #[must_use]
    pub fn transfer(
        &self,
    ) -> Option<(
        WorkspaceTransfer,
        &[WorktreeRelativePath],
        Option<&WorktreeRelativePath>,
    )> {
        match &self.data {
            WorkspaceOperationData::Transfer {
                mode,
                sources,
                destination,
            } => Some((*mode, sources, destination.as_ref())),
            _ => None,
        }
    }
}

/// The workspace root used by one host diagnostics report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeHostWorkspace {
    /// The caller resolved the root.
    Resolved {
        /// The resolved root.
        root: PathBuf,
    },
    /// The caller could not resolve the root.
    Unresolved {
        /// Why root resolution failed.
        reason: String,
    },
}

/// One bounded diagnostics probe for a worktree host.
#[derive(Clone)]
pub struct WorktreeHostReportRequest(TuiHostReportRequest);

impl WorktreeHostReportRequest {
    /// Creates a report request with kvim's built-in language registry.
    #[must_use]
    pub fn built_in(workspace: WorktreeHostWorkspace) -> Self {
        let workspace = match workspace {
            WorktreeHostWorkspace::Resolved { root } => TuiHostWorkspace::Resolved { root },
            WorktreeHostWorkspace::Unresolved { reason } => TuiHostWorkspace::Unresolved { reason },
        };
        Self(TuiHostReportRequest::new(
            LanguageRegistry::first_release(),
            workspace,
        ))
    }

    /// Probes the host and returns the plain-text report.
    #[must_use]
    pub fn run(self) -> String {
        self.0.run()
    }
}

/// How one worktree editor resolves physical bindings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeBindingMode {
    /// Kvim resolves the standalone binding profile inside the facade.
    FacadeResolved,
    /// The host resolves the embedded profile and reserves one escape key.
    HostResolved {
        /// One normalized physical key that the host always handles before kvim.
        ///
        /// [`Key`] represents exactly one non-empty key press. It cannot
        /// represent an empty or multi-key sequence.
        reserved_escape: Key,
    },
}

impl Default for WorktreeBindingMode {
    fn default() -> Self {
        Self::FacadeResolved
    }
}

impl WorktreeBindingMode {
    const fn profile(self) -> BindingProfile {
        match self {
            Self::FacadeResolved => BindingProfile::Standalone,
            Self::HostResolved { .. } => BindingProfile::Embedded,
        }
    }
}

/// Current bounded input metadata for a host-owned resolver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorktreeBindingContext {
    instance: WorktreeInstanceId,
    context: InputContextSnapshot<BindingScope>,
    overlay_scope: Option<BindingScope>,
    reserved_escape: Key,
}

impl WorktreeBindingContext {
    /// Returns the addressed editor instance.
    #[must_use]
    pub const fn instance(&self) -> WorktreeInstanceId {
        self.instance
    }
    /// Returns the context generation and semantic phases.
    #[must_use]
    pub const fn context(&self) -> InputContextSnapshot<BindingScope> {
        self.context
    }
    /// Returns the optional scope that precedes the focused prompt scope.
    #[must_use]
    pub const fn overlay_scope(&self) -> Option<BindingScope> {
        self.overlay_scope
    }
    /// Returns the physical key that the host always reserves.
    #[must_use]
    pub const fn reserved_escape(&self) -> Key {
        self.reserved_escape
    }
}

/// One addressed decision produced by a host-owned resolver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeSemanticDispatch {
    instance: WorktreeInstanceId,
    generation: ContextGeneration,
    decision: WorktreeDispatchDecision,
}

impl WorktreeSemanticDispatch {
    /// Addresses one decision to the context that produced it.
    #[must_use]
    pub const fn new(
        instance: WorktreeInstanceId,
        generation: ContextGeneration,
        decision: WorktreeDispatchDecision,
    ) -> Self {
        Self {
            instance,
            generation,
            decision,
        }
    }
}

/// The semantic result of one host-owned resolution request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeDispatchDecision {
    /// One editor command completed.
    Complete {
        /// The semantic editor command selected by the host resolver.
        command: Command,
    },
    /// A static key sequence waits in the host-owned resolver.
    ///
    /// This does not alter kvim's semantic reducer. The host owns and clears
    /// the static prefix.
    Pending,
    /// A validated `i` or `a` prefix waits for a text-object selection.
    ///
    /// This decision is accepted only in a scope that binds text objects.
    TextObjectPending,
    /// The current context takes literal text.
    TextFallback(TypedText),
    /// No binding or text owner accepted the input.
    Unbound,
    /// A preceding host scope interrupted kvim's pending input.
    Interrupted,
}

/// What addressed semantic dispatch did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeDispatchOutcome {
    /// The complete command was applied.
    Complete(WorktreeInputOutcome),
    /// Kvim retained semantic state for a later decision.
    Pending,
    /// Literal text was applied.
    TextFallback(WorktreeInputOutcome),
    /// Kvim cleared pending state after an unbound decision.
    Unbound,
    /// The host must complete a cancel-pending transition before acting.
    Interrupted(CancelPendingProposal),
}

/// One instance- and generation-bound cancellation proposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelPendingProposal {
    instance: WorktreeInstanceId,
    generation: ContextGeneration,
}

/// A validated idle context returned after atomic cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelPendingResume {
    instance: WorktreeInstanceId,
    context: InputContextSnapshot<BindingScope>,
}

impl CancelPendingResume {
    /// Returns the addressed editor instance.
    #[must_use]
    pub const fn instance(self) -> WorktreeInstanceId {
        self.instance
    }
    /// Returns the validated idle context.
    #[must_use]
    pub const fn context(self) -> InputContextSnapshot<BindingScope> {
        self.context
    }
}

/// Why addressed input or cancellation was refused.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeDispatchError {
    /// The value addresses another editor.
    #[error("the input transition addresses another editor")]
    WrongInstance,
    /// The editor has published another context generation.
    #[error("the input transition uses a stale context generation")]
    StaleGeneration,
    /// Host-owned dispatch is not enabled.
    #[error("the editor owns its binding resolver")]
    FacadeResolved,
    /// Kvim has no semantic input to cancel.
    #[error("the editor has no pending semantic input")]
    NoPending,
    /// The completed command is not bound in the addressed current context.
    #[error("the resolved command is not valid in the current binding context")]
    InvalidResolvedCommand,
}

/// One normalized, terminal-neutral input supplied by a host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeInput {
    /// A normalized key press or repeat.
    Key(Key),
    /// One validated bracketed-paste block.
    Paste(PasteText),
    /// A new terminal or host-surface size.
    Resize {
        /// Width in terminal cells.
        columns: u16,
        /// Height in terminal cells.
        rows: u16,
    },
    /// Input that no binding accepts.
    Unsupported,
}

/// Why normalized physical input was refused before mutation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeInputError {
    /// The host owns physical key and paste arbitration for this editor.
    #[error("the host owns physical key and paste resolution")]
    HostResolved,
}

/// Why an input was refused before a side effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeRefusal {
    /// The host granted view-only access.
    ViewOnly,
    /// A bounded internal queue has no capacity.
    Saturated,
}

/// A synchronous host request produced by input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeInputRequest {
    /// Focus reached an outer edge.
    FocusBoundary(Direction),
    /// The last window closed.
    CloseRequested,
}

/// Result of one synchronous input reduction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeInputOutcome {
    /// The editor accepted the input.
    Applied,
    /// The editor asks the host to act.
    Request(WorktreeInputRequest),
    /// The editor refused the input.
    Refused(WorktreeRefusal),
}

/// Whether a transition requests a new frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeUpdate {
    /// Visible state did not change.
    Unchanged,
    /// Visible state changed.
    Redraw,
}

/// Whether one editor still accepts input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeRunState {
    /// The editor accepts input.
    Running,
    /// The editor requested exit.
    ExitRequested,
}

/// The cursor requested by one rendered frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorktreeCursor {
    /// Cursor cell, or `None` when hidden.
    pub position: Option<Position>,
    /// Cursor shape.
    pub shape: WorktreeCursorShape,
}

/// A terminal-neutral cursor shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeCursorShape {
    /// A block cursor.
    Block,
    /// A vertical bar cursor.
    Bar,
}

/// One facade-owned editor fact or host request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeEvent {
    /// The active file changed.
    ActiveFileChanged {
        /// Active contained path.
        path: Option<WorktreeRelativePath>,
    },
    /// A file write committed.
    FileWritten {
        /// Written contained path.
        path: WorktreeRelativePath,
    },
    /// A workspace operation committed.
    WorkspaceChanged {
        /// Committed operation.
        operation: WorkspaceOperation,
    },
    /// A save has an uncertain durable result and requires reconciliation.
    SaveReconciliationRequired {
        /// Path requiring reconciliation.
        path: WorktreeRelativePath,
    },
    /// A workspace operation has an uncertain durable result.
    WorkspaceReconciliationRequired {
        /// Operation requiring reconciliation.
        operation: WorkspaceOperation,
    },
    /// The sidebar activated a file.
    FileActivated {
        /// Activated contained path.
        path: WorktreeRelativePath,
    },
    /// Visible state changed.
    RedrawRequested,
    /// Focus reached an outer edge.
    FocusBoundary(Direction),
    /// The last window closed.
    CloseRequested,
}

/// Geometry rejected by a worktree editor.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeGeometryError {
    /// The rectangle contains no cell.
    #[error("the editor rectangle {area:?} contains no cell")]
    Empty {
        /// Rejected rectangle.
        area: Rect,
    },
    /// The rectangle leaves the supplied buffer.
    #[error("the editor rectangle {area:?} leaves the buffer {buffer:?}")]
    OutsideBuffer {
        /// Rejected rectangle.
        area: Rect,
        /// Cell-buffer rectangle.
        buffer: Rect,
    },
    /// The draw rectangle differs from the accepted rectangle.
    #[error("the editor rectangle {area:?} differs from {accepted:?}")]
    Unreconciled {
        /// Supplied rectangle.
        area: Rect,
        /// Accepted rectangle.
        accepted: Rect,
    },
}

/// Stable classification of a worktree open failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeOpenErrorKind {
    /// The root is not a usable confined worktree.
    Root,
    /// Settings are invalid.
    Settings,
    /// Geometry is invalid.
    Geometry,
    /// The private asynchronous executor could not start.
    Executor,
    /// Built-in language services rejected the root.
    Language,
    /// The requested filesystem watcher could not start.
    Watcher,
}

/// Failure while opening a worktree editor.
#[derive(Debug)]
pub struct WorktreeOpenError {
    kind: WorktreeOpenErrorKind,
    path: Option<PathBuf>,
    source: Box<dyn StdError + Send + Sync>,
}

impl WorktreeOpenError {
    fn new(
        kind: WorktreeOpenErrorKind,
        path: Option<PathBuf>,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            path,
            source: Box::new(source),
        }
    }

    /// Returns the stable failure classification.
    #[must_use]
    pub const fn kind(&self) -> WorktreeOpenErrorKind {
        self.kind
    }

    /// Returns the supplied root path for a root failure.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl fmt::Display for WorktreeOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            WorktreeOpenErrorKind::Root => write!(
                formatter,
                "cannot open worktree root {:?}",
                self.path.as_deref().unwrap_or_else(|| Path::new(""))
            ),
            WorktreeOpenErrorKind::Settings => formatter.write_str("invalid editor settings"),
            WorktreeOpenErrorKind::Geometry => self.source.fmt(formatter),
            WorktreeOpenErrorKind::Executor => {
                formatter.write_str("cannot start the editor executor")
            }
            WorktreeOpenErrorKind::Language => {
                formatter.write_str("cannot configure built-in language services")
            }
            WorktreeOpenErrorKind::Watcher => {
                formatter.write_str("cannot start the built-in filesystem watcher")
            }
        }
    }
}

impl StdError for WorktreeOpenError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

/// A host supplied an invalid resolved command.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeCommandError {
    /// The character cannot name a register.
    #[error("{name:?} is not a valid register name")]
    InvalidRegisterName {
        /// The rejected character.
        name: char,
    },
}

/// Facade-owned identity of one worktree editor.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorktreeInstanceId(u64);

impl WorktreeInstanceId {
    /// Returns the process-local instance number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The kind of a failed completion application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeApplyErrorKind {
    /// The completion belongs to another editor.
    WrongInstance {
        /// The editor that received the completion.
        editor: WorktreeInstanceId,
        /// The editor that produced the completion.
        completion: WorktreeInstanceId,
    },
}

/// A completion that was routed to another worktree editor.
///
/// The error retains the opaque completion. Call [`Self::into_completion`] to
/// route it to its owning editor without losing a reserved result.
pub struct WorktreeApplyError {
    kind: WorktreeApplyErrorKind,
    completion: WorktreeCompletion,
}

impl WorktreeApplyError {
    /// Returns the typed rejection kind.
    #[must_use]
    pub const fn kind(&self) -> WorktreeApplyErrorKind {
        self.kind
    }

    /// Recovers the unapplied completion for correct routing.
    pub fn into_completion(self) -> WorktreeCompletion {
        self.completion
    }
}

impl fmt::Debug for WorktreeApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorktreeApplyError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for WorktreeApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let WorktreeApplyErrorKind::WrongInstance { editor, completion } = self.kind;
        write!(
            formatter,
            "completion belongs to instance {completion:?}, not editor {editor:?}"
        )
    }
}

impl StdError for WorktreeApplyError {}

/// One opaque completed unit returned by [`WorktreeEditor::ready`].
#[must_use = "apply the completion to the editor that produced it"]
pub struct WorktreeCompletion {
    instance: WorktreeInstanceId,
    inner: Completed,
}

impl fmt::Debug for WorktreeCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorktreeCompletion(..)")
    }
}

/// Remaining mandatory events after a bounded shutdown deadline.
#[must_use = "complete the drain to observe durable-operation events"]
pub struct WorktreeDrain {
    runtime: TokioRuntime,
    drain: kvim_tui::__private::EditorDrain,
}

impl fmt::Debug for WorktreeDrain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorktreeDrain(..)")
    }
}

impl WorktreeDrain {
    /// Waits for committing work and returns all remaining events.
    pub async fn complete(self) -> Vec<WorktreeEvent> {
        let Self { runtime, drain } = self;
        let _guard = runtime.enter();
        let events = drain
            .complete()
            .await
            .into_iter()
            .map(convert_published)
            .collect();
        drop(_guard);
        runtime.shutdown_background();
        events
    }
}

/// Result of consuming shutdown.
#[must_use = "a drain still owns mandatory durable-operation events"]
#[derive(Debug)]
pub enum WorktreeShutdown {
    /// Every task finished before the deadline.
    Finished {
        /// Remaining facade-owned events.
        events: Vec<WorktreeEvent>,
    },
    /// Committing work still needs to deliver events.
    Draining(Box<WorktreeDrain>),
}

/// Builder for one worktree editor with explicit capabilities.
#[derive(Debug)]
pub struct WorktreeEditorBuilder {
    root: PathBuf,
    area: Rect,
    settings: EditorSettings,
    access: WorktreeAccess,
    capacity: WorktreeCapacity,
    capabilities: WorktreeCapabilities,
    binding_mode: WorktreeBindingMode,
}

impl WorktreeEditorBuilder {
    /// Sets validated editor settings.
    #[must_use]
    pub fn settings(mut self, settings: EditorSettings) -> Self {
        self.settings = settings;
        self
    }
    /// Sets filesystem and text access.
    #[must_use]
    pub fn access(mut self, access: WorktreeAccess) -> Self {
        self.access = access;
        self
    }
    /// Sets bounded execution capacity.
    #[must_use]
    pub fn capacity(mut self, capacity: WorktreeCapacity) -> Self {
        self.capacity = capacity;
        self
    }
    /// Sets all optional service policies.
    #[must_use]
    pub fn capabilities(mut self, capabilities: WorktreeCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
    /// Selects facade-owned or host-owned physical binding resolution.
    #[must_use]
    pub fn binding_mode(mut self, binding_mode: WorktreeBindingMode) -> Self {
        self.binding_mode = binding_mode;
        self
    }
    /// Opens the editor and its private executor.
    pub fn open(self) -> Result<WorktreeEditor, WorktreeOpenError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|source| {
                WorktreeOpenError::new(WorktreeOpenErrorKind::Executor, None, source)
            })?;
        let root_path = self.root;
        let root = Arc::new(WorktreeRoot::open(&root_path).map_err(|source| {
            WorktreeOpenError::new(WorktreeOpenErrorKind::Root, Some(root_path.clone()), source)
        })?);
        let limits = RuntimeLimits::new(
            self.capacity.completions,
            self.capacity.workers,
            self.capacity.processes,
        )
        .expect("WorktreeCapacity validates runtime limits");
        let _guard = runtime.enter();
        let registry = self
            .binding_mode
            .profile()
            .registry()
            .expect("built-in binding profiles are validated by focused tests");
        let manifest = self
            .binding_mode
            .profile()
            .manifest()
            .expect("built-in binding profiles are validated by focused tests");
        let mut builder = EmbeddedEditor::builder(Arc::clone(&root), self.area)
            .registry(registry)
            .settings(self.settings)
            .access(match self.access {
                WorktreeAccess::ReadWrite => TuiEditorAccess::ReadWrite,
                WorktreeAccess::ViewOnly => TuiEditorAccess::ViewOnly,
            })
            .capacity(TuiEditorCapacity::Isolated(limits))
            .git_status(self.capabilities.git != ServicePolicy::Disabled)
            .clipboard(match self.capabilities.clipboard {
                ServicePolicy::Disabled => TuiClipboardAccess::None,
                ServicePolicy::BuiltIn | ServicePolicy::BestEffortBuiltIn => {
                    TuiClipboardAccess::System
                }
            });
        let language = construct_service(self.capabilities.language, || {
            LanguageServices::new(
                LanguageRegistry::first_release(),
                root.as_path().to_path_buf(),
                self.settings,
            )
        })
        .map_err(|source| WorktreeOpenError::new(WorktreeOpenErrorKind::Language, None, source))?;
        if let Some(language) = language {
            builder = builder.language(language);
        }
        let watcher = match self.capabilities.watcher {
            ServicePolicy::Disabled => None,
            ServicePolicy::BuiltIn => Some(
                FileWatcher::start(Arc::clone(&root), &kvim_tui::__private::GENERATED_NAMES)
                    .map_err(|source| {
                        WorktreeOpenError::new(WorktreeOpenErrorKind::Watcher, None, source)
                    })?,
            ),
            ServicePolicy::BestEffortBuiltIn => {
                match FileWatcher::start(Arc::clone(&root), &kvim_tui::__private::GENERATED_NAMES) {
                    Ok(watcher) => Some(watcher),
                    Err(_) => {
                        builder = builder.watcher_unavailable();
                        None
                    }
                }
            }
        };
        if let Some(watcher) = watcher {
            builder = builder.watcher(watcher);
        }
        let inner = builder.open().map_err(|error| match error {
            kvim_tui::__private::EditorOpenError::Settings(source) => {
                WorktreeOpenError::new(WorktreeOpenErrorKind::Settings, None, source)
            }
            kvim_tui::__private::EditorOpenError::Geometry(source) => WorktreeOpenError::new(
                WorktreeOpenErrorKind::Geometry,
                None,
                WorktreeGeometryError::from(source),
            ),
            kvim_tui::__private::EditorOpenError::LanguageRootMismatch { .. } => {
                unreachable!("facade constructs services from the same validated root")
            }
        })?;
        drop(_guard);
        Ok(WorktreeEditor {
            instance: WorktreeInstanceId(inner.instance().get()),
            inner: Some(inner),
            runtime: Some(runtime),
            binding_mode: self.binding_mode,
            manifest: Arc::new(manifest),
            #[cfg(test)]
            capabilities: self.capabilities,
        })
    }
}

fn construct_service<T, E>(
    policy: ServicePolicy,
    construct: impl FnOnce() -> Result<T, E>,
) -> Result<Option<T>, E> {
    match policy {
        ServicePolicy::Disabled => Ok(None),
        ServicePolicy::BuiltIn => construct().map(Some),
        ServicePolicy::BestEffortBuiltIn => Ok(construct().ok()),
    }
}

/// A rendered worktree editor with owned bounded orchestration.
///
/// The default constructor starts no Git, watcher, language, or clipboard
/// service. Filesystem open, edit, render, and save remain available.
/// Public asynchronous methods require the host to poll their futures. All
/// internal work executes on this editor's private runtime.
///
/// Call [`WorktreeEditor::shutdown`] to observe every mandatory event from
/// durable work. Dropping the editor cancels its private runtime as a
/// best-effort fallback and does not guarantee durable event delivery.
pub struct WorktreeEditor {
    instance: WorktreeInstanceId,
    inner: Option<EmbeddedEditor>,
    runtime: Option<TokioRuntime>,
    binding_mode: WorktreeBindingMode,
    manifest: Arc<BindingManifest>,
    #[cfg(test)]
    capabilities: WorktreeCapabilities,
}

impl fmt::Debug for WorktreeEditor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorktreeEditor(..)")
    }
}

impl WorktreeEditor {
    /// Starts construction over one filesystem root and nonempty rectangle.
    #[must_use]
    pub fn builder(root: impl AsRef<Path>, area: Rect) -> WorktreeEditorBuilder {
        WorktreeEditorBuilder {
            root: root.as_ref().to_path_buf(),
            area,
            settings: EditorSettings::default(),
            access: WorktreeAccess::default(),
            capacity: WorktreeCapacity::default(),
            capabilities: WorktreeCapabilities::default(),
            binding_mode: WorktreeBindingMode::default(),
        }
    }
    #[cfg(test)]
    fn capabilities(&self) -> WorktreeCapabilities {
        self.capabilities
    }

    #[cfg(test)]
    fn git_status_enabled(&self) -> bool {
        self.inner().git_status_enabled()
    }

    #[cfg(test)]
    fn git_request_queued(&self) -> bool {
        self.inner().git_request_queued()
    }

    fn inner(&self) -> &EmbeddedEditor {
        self.inner.as_ref().expect("shutdown consumes the editor")
    }
    fn inner_mut(&mut self) -> &mut EmbeddedEditor {
        self.inner.as_mut().expect("shutdown consumes the editor")
    }
    /// Returns this editor's facade-owned routing identity.
    #[must_use]
    pub const fn instance(&self) -> WorktreeInstanceId {
        self.instance
    }

    /// Returns the accepted rectangle.
    #[must_use]
    pub fn area(&self) -> Rect {
        self.inner().area()
    }
    /// Changes the accepted rectangle.
    pub fn resize(&mut self, area: Rect) -> Result<WorktreeUpdate, WorktreeGeometryError> {
        self.inner_mut()
            .set_area(area)
            .map(convert_redraw)
            .map_err(Into::into)
    }
    /// Queues one contained file for asynchronous opening.
    pub fn open_file(&mut self, path: WorktreeRelativePath) -> WorktreeUpdate {
        convert_redraw(self.inner_mut().open_file(path))
    }
    /// Applies one normalized host input.
    ///
    /// Host-resolved editors reject keys, paste, and unsupported raw input
    /// before mutation. Resize does not use a binding resolver and remains
    /// accepted in both modes.
    pub fn input(
        &mut self,
        input: WorktreeInput,
        now: Duration,
    ) -> Result<WorktreeUpdate, WorktreeInputError> {
        if matches!(self.binding_mode, WorktreeBindingMode::HostResolved { .. })
            && !matches!(input, WorktreeInput::Resize { .. })
        {
            return Err(WorktreeInputError::HostResolved);
        }
        let input = match input {
            WorktreeInput::Key(key) => TuiTerminalEvent::Key(key),
            WorktreeInput::Paste(text) => TuiTerminalEvent::Paste(text),
            WorktreeInput::Resize { columns, rows } => TuiTerminalEvent::Resize { columns, rows },
            WorktreeInput::Unsupported => TuiTerminalEvent::Unsupported,
        };
        Ok(convert_redraw(self.inner_mut().input(input, now)))
    }
    /// Applies one direct semantic command.
    ///
    /// This method does not claim that a physical binding resolved in the
    /// current scope. It remains available in both binding modes for host
    /// actions such as menus and command palettes. Use [`Self::semantic_dispatch`]
    /// for a command produced by the host-owned physical resolver.
    ///
    /// Returns [`WorktreeCommandError::InvalidRegisterName`] before changing
    /// editor state when `register` is not a canonical register name.
    pub fn command(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
        register: Option<char>,
        now: Duration,
    ) -> Result<WorktreeInputOutcome, WorktreeCommandError> {
        if let Some(name) = register
            && !is_register_name(name)
        {
            return Err(WorktreeCommandError::InvalidRegisterName { name });
        }
        Ok(convert_reduction(
            self.inner_mut().command(command, count, register, now),
        ))
    }
    /// Inserts direct semantic text in the active input context.
    ///
    /// This method bypasses physical binding resolution. It remains available
    /// in both binding modes for host-owned text entry.
    pub fn literal(&mut self, text: &str, now: Duration) -> WorktreeInputOutcome {
        convert_reduction(self.inner_mut().insert_literal(text, now))
    }
    /// Applies direct semantic pasted text.
    ///
    /// This method bypasses physical binding resolution. In host-resolved mode,
    /// the host must arbitrate the physical paste event before it calls this
    /// method. [`Self::input`] rejects the raw paste path in that mode.
    pub fn paste(&mut self, text: &PasteText, now: Duration) -> WorktreeInputOutcome {
        convert_reduction(self.inner_mut().paste(text, now))
    }
    /// Returns the current modal mode.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.inner().mode()
    }
    /// Returns the current resolved-input context.
    #[must_use]
    pub fn input_context(&self) -> InputContextSnapshot<BindingScope> {
        self.inner().input_context()
    }
    /// Returns bounded changing metadata for host-owned resolution.
    #[must_use]
    pub fn binding_context(&self) -> Option<WorktreeBindingContext> {
        let WorktreeBindingMode::HostResolved { reserved_escape } = self.binding_mode else {
            return None;
        };
        let (context, overlay_scope) = self.inner().binding_context();
        Some(WorktreeBindingContext {
            instance: self.instance,
            context,
            overlay_scope,
            reserved_escape,
        })
    }
    /// Returns the immutable embedded binding manifest for host-owned resolution.
    ///
    /// Polling this accessor does not clone or allocate binding entries.
    #[must_use]
    pub fn binding_manifest(&self) -> Option<&BindingManifest> {
        matches!(self.binding_mode, WorktreeBindingMode::HostResolved { .. })
            .then_some(self.manifest.as_ref())
    }
    /// Applies one addressed physical-resolution decision.
    ///
    /// Unlike [`Self::command`], this method validates a completed command
    /// against the active focus and overlay scopes. Every decision is bound to
    /// the instance and context generation that produced it.
    pub fn semantic_dispatch(
        &mut self,
        dispatch: WorktreeSemanticDispatch,
        now: Duration,
    ) -> Result<WorktreeDispatchOutcome, WorktreeDispatchError> {
        if !matches!(self.binding_mode, WorktreeBindingMode::HostResolved { .. }) {
            return Err(WorktreeDispatchError::FacadeResolved);
        }
        let current = self.input_context();
        if dispatch.instance != self.instance {
            return Err(WorktreeDispatchError::WrongInstance);
        }
        if dispatch.generation != current.generation {
            return Err(WorktreeDispatchError::StaleGeneration);
        }
        let outcome = match dispatch.decision {
            WorktreeDispatchDecision::Interrupted => {
                if current.phases.is_idle() {
                    return Err(WorktreeDispatchError::NoPending);
                }
                return Ok(WorktreeDispatchOutcome::Interrupted(
                    CancelPendingProposal {
                        instance: self.instance,
                        generation: current.generation,
                    },
                ));
            }
            WorktreeDispatchDecision::Complete { command } => {
                let (_, overlay_scope) = self.inner().binding_context();
                if !resolved_command_is_valid(
                    self.manifest.as_ref(),
                    current.scope,
                    overlay_scope,
                    command,
                ) {
                    return Err(WorktreeDispatchError::InvalidResolvedCommand);
                }
                WorktreeDispatchOutcome::Complete(convert_reduction(
                    self.inner_mut()
                        .semantic_dispatch(Dispatch::Surface { command }, now),
                ))
            }
            WorktreeDispatchDecision::Pending => WorktreeDispatchOutcome::Pending,
            WorktreeDispatchDecision::TextObjectPending => {
                if !current.scope.binds_text_objects() {
                    return Err(WorktreeDispatchError::InvalidResolvedCommand);
                }
                let _ = self.inner_mut().semantic_dispatch(Dispatch::Pending, now);
                WorktreeDispatchOutcome::Pending
            }
            WorktreeDispatchDecision::TextFallback(text) => {
                if current.text_fallback.owner() != Some(CommandOwner::Surface) {
                    return Err(WorktreeDispatchError::InvalidResolvedCommand);
                }
                WorktreeDispatchOutcome::TextFallback(convert_reduction(
                    self.inner_mut().semantic_dispatch(
                        Dispatch::Text {
                            owner: CommandOwner::Surface,
                            text,
                        },
                        now,
                    ),
                ))
            }
            WorktreeDispatchDecision::Unbound => {
                let _ = self.inner_mut().semantic_dispatch(Dispatch::Unbound, now);
                WorktreeDispatchOutcome::Unbound
            }
        };
        Ok(outcome)
    }
    /// Atomically clears pending editor input for one addressed proposal.
    ///
    /// The transition applies all cancellation effects, including operator
    /// cancellation, before it returns the validated idle context.
    pub fn cancel_pending(
        &mut self,
        proposal: CancelPendingProposal,
        now: Duration,
    ) -> Result<CancelPendingResume, WorktreeDispatchError> {
        if proposal.instance != self.instance {
            return Err(WorktreeDispatchError::WrongInstance);
        }
        let before = self.input_context();
        if proposal.generation != before.generation {
            return Err(WorktreeDispatchError::StaleGeneration);
        }
        if before.phases.is_idle() {
            return Err(WorktreeDispatchError::NoPending);
        }
        let _ = self.inner_mut().cancel_pending(now);
        let context = self.input_context();
        assert!(
            context.phases.is_idle(),
            "the semantic reducer cancel transition clears every pending phase"
        );
        assert_ne!(
            context.generation, before.generation,
            "the semantic reducer cancel transition always advances its generation"
        );
        Ok(CancelPendingResume {
            instance: self.instance,
            context,
        })
    }
    /// Returns the next host elapsed-time deadline.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        self.inner().next_deadline()
    }
    /// Applies a host elapsed-time transition.
    pub fn tick(&mut self, now: Duration) -> WorktreeUpdate {
        convert_redraw(self.inner_mut().tick(now))
    }
    /// Reports whether input remains active.
    #[must_use]
    pub fn run_state(&self) -> WorktreeRunState {
        match self.inner().run_state() {
            TuiRunState::Running => WorktreeRunState::Running,
            TuiRunState::Finished => WorktreeRunState::ExitRequested,
        }
    }
    /// Renders into host-owned cells.
    pub fn render(&self, cells: &mut Buffer) -> Result<WorktreeCursor, WorktreeGeometryError> {
        let cursor = self.inner().draw(cells, self.inner().area())?;
        Ok(WorktreeCursor {
            position: cursor.position,
            shape: match cursor.shape {
                TuiCursorShape::Block => WorktreeCursorShape::Block,
                TuiCursorShape::Bar => WorktreeCursorShape::Bar,
            },
        })
    }
    /// Takes one facade-owned event.
    #[must_use]
    pub fn take_event(&mut self) -> Option<WorktreeEvent> {
        self.inner_mut().take_event().map(convert_published)
    }
    /// Submits all queued work to owned bounded execution capacity.
    pub fn dispatch(&mut self) -> WorktreeUpdate {
        let runtime = self
            .runtime
            .as_ref()
            .expect("shutdown consumes the executor");
        let _guard = runtime.enter();
        convert_redraw(self.inner_mut().dispatch())
    }
    /// Waits until one internal result is ready.
    pub async fn ready(&mut self) -> WorktreeCompletion {
        let runtime = self
            .runtime
            .as_ref()
            .expect("shutdown consumes the executor");
        let _guard = runtime.enter();
        let completion = self.inner_mut().recv().await;
        WorktreeCompletion {
            instance: WorktreeInstanceId(completion.instance().get()),
            inner: completion,
        }
    }
    /// Applies one completion returned by this editor.
    ///
    /// Returns [`WorktreeApplyErrorKind::WrongInstance`] before any mutation when
    /// another editor produced the completion.
    #[allow(clippy::result_large_err)]
    pub fn apply(
        &mut self,
        completion: WorktreeCompletion,
        now: Duration,
    ) -> Result<WorktreeUpdate, WorktreeApplyError> {
        if self.instance != completion.instance {
            return Err(WorktreeApplyError {
                kind: WorktreeApplyErrorKind::WrongInstance {
                    editor: self.instance,
                    completion: completion.instance,
                },
                completion,
            });
        }
        let redraw = self
            .inner_mut()
            .apply(completion.inner, now)
            .expect("facade identity validation matches the internal owner");
        Ok(convert_redraw(redraw))
    }
    /// Consumes the editor and performs bounded shutdown.
    pub async fn shutdown(mut self, deadline: Duration) -> WorktreeShutdown {
        let inner = self
            .inner
            .take()
            .expect("shutdown consumes one live editor");
        let runtime = self
            .runtime
            .as_ref()
            .expect("shutdown consumes the executor");
        let _guard = runtime.enter();
        match inner.shutdown(deadline).await {
            TuiEditorShutdown::Finished { events } => {
                drop(_guard);
                self.runtime
                    .take()
                    .expect("shutdown owns the executor")
                    .shutdown_background();
                WorktreeShutdown::Finished {
                    events: events.into_iter().map(convert_published).collect(),
                }
            }
            TuiEditorShutdown::Draining(drain) => {
                drop(_guard);
                let runtime = self.runtime.take().expect("shutdown owns the executor");
                WorktreeShutdown::Draining(Box::new(WorktreeDrain {
                    runtime,
                    drain: *drain,
                }))
            }
        }
    }
}

impl Drop for WorktreeEditor {
    fn drop(&mut self) {
        let inner = self.inner.take();
        if inner.is_none() {
            return;
        }
        drop(inner);
        self.runtime
            .take()
            .expect("a live editor owns its executor")
            .shutdown_background();
    }
}

fn convert_redraw(redraw: TuiRedraw) -> WorktreeUpdate {
    match redraw {
        TuiRedraw::Skipped => WorktreeUpdate::Unchanged,
        TuiRedraw::Needed => WorktreeUpdate::Redraw,
    }
}
fn resolved_command_is_valid(
    manifest: &BindingManifest,
    focus_scope: BindingScope,
    overlay_scope: Option<BindingScope>,
    command: Command,
) -> bool {
    manifest.entries().iter().any(|entry| {
        entry.command() == command
            && (entry.scope() == focus_scope || overlay_scope == Some(entry.scope()))
    })
}

fn convert_reduction(reduction: TuiReduction) -> WorktreeInputOutcome {
    match reduction.outcome {
        TuiReductionOutcome::Applied => WorktreeInputOutcome::Applied,
        TuiReductionOutcome::Request(request) => WorktreeInputOutcome::Request(match request {
            TuiInputRequest::FocusBoundary(direction) => {
                WorktreeInputRequest::FocusBoundary(direction)
            }
            TuiInputRequest::CloseRequested => WorktreeInputRequest::CloseRequested,
        }),
        TuiReductionOutcome::Refused(refusal) => WorktreeInputOutcome::Refused(match refusal {
            TuiRefusal::ViewOnly => WorktreeRefusal::ViewOnly,
            TuiRefusal::Saturated => WorktreeRefusal::Saturated,
        }),
    }
}
fn convert_published(published: TuiPublishedEvent) -> WorktreeEvent {
    match published.event {
        TuiEditorEvent::ActiveFileChanged { path } => WorktreeEvent::ActiveFileChanged { path },
        TuiEditorEvent::FileWritten { path } => WorktreeEvent::FileWritten { path },
        TuiEditorEvent::WorkspaceChanged { operation } => WorktreeEvent::WorkspaceChanged {
            operation: convert_workspace_operation(operation),
        },
        TuiEditorEvent::SaveReconciliationRequired { path } => {
            WorktreeEvent::SaveReconciliationRequired { path }
        }
        TuiEditorEvent::WorkspaceReconciliationRequired { operation } => {
            WorktreeEvent::WorkspaceReconciliationRequired {
                operation: convert_workspace_operation(operation),
            }
        }
        TuiEditorEvent::FileActivated { path } => WorktreeEvent::FileActivated { path },
        TuiEditorEvent::RedrawRequested => WorktreeEvent::RedrawRequested,
        TuiEditorEvent::FocusBoundary(direction) => WorktreeEvent::FocusBoundary(direction),
        TuiEditorEvent::CloseRequested => WorktreeEvent::CloseRequested,
    }
}

fn convert_workspace_operation(operation: FileOperation) -> WorkspaceOperation {
    match operation {
        FileOperation::Create { path, kind } => WorkspaceOperation {
            data: WorkspaceOperationData::Create {
                path,
                kind: match kind {
                    EntryKind::File => WorkspaceEntryKind::File,
                    EntryKind::Directory => WorkspaceEntryKind::Directory,
                },
            },
        },
        FileOperation::Delete { paths } => {
            debug_assert!(
                paths.len() <= WORKSPACE_OPERATION_PATHS_MAX,
                "workspace validates mutation path bounds before publication"
            );
            WorkspaceOperation {
                data: WorkspaceOperationData::Delete { paths },
            }
        }
        FileOperation::Rename { from, to } => WorkspaceOperation {
            data: WorkspaceOperationData::Rename { from, to },
        },
        FileOperation::Transfer {
            mode,
            sources,
            destination,
        } => {
            debug_assert!(
                sources.len() <= WORKSPACE_OPERATION_PATHS_MAX,
                "workspace validates mutation path bounds before publication"
            );
            WorkspaceOperation {
                data: WorkspaceOperationData::Transfer {
                    mode: match mode {
                        TransferMode::Copy => WorkspaceTransfer::Copy,
                        TransferMode::Move => WorkspaceTransfer::Move,
                    },
                    sources,
                    destination: destination.relative_path().cloned(),
                },
            }
        }
    }
}

impl From<TuiGeometryError> for WorktreeGeometryError {
    fn from(error: TuiGeometryError) -> Self {
        match error {
            TuiGeometryError::Empty { area } => Self::Empty { area },
            TuiGeometryError::OutsideBuffer { area, buffer } => {
                Self::OutsideBuffer { area, buffer }
            }
            TuiGeometryError::Unreconciled { area, accepted } => {
                Self::Unreconciled { area, accepted }
            }
        }
    }
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;
