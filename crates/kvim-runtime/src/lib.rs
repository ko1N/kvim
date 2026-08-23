//! Bounded background work with cancellation, deadlines, and publication gates.
//! Adapted from ReviewGraph (MIT), src/runtime.rs.
//!
//! The crate owns the worker service, the external-process service, request
//! identity, and result delivery. It stays generic. It knows nothing about
//! buffers, modes, pickers, or language servers. The caller defines one
//! [`RequestSlot`] for each of those operations.
//!
//! Submission never waits. A submission reserves the result slot first, then
//! reserves the worker or process permit. Both reservations use the non-blocking
//! form. When either resource is full, submission returns
//! [`SubmitError::Saturated`] and the caller keeps its previous visible state.
//! Every accepted request produces exactly one [`RuntimeEvent`].
//!
//! See `docs/responsiveness.md` for the binding bounds and the shutdown order.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::time;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

mod watch;

#[cfg(test)]
mod tests;

pub use watch::{
    FileWatcher, WATCH_BATCH_DIRECTORIES_MAX, WATCH_BATCH_QUEUE_MAX, WATCH_BURST_EVENTS_MAX,
    WATCH_COALESCE_WINDOW, WATCH_DEPTH_MAX, WATCH_DIRECTORIES_MAX, WATCH_DIRECTORY_SCAN_MAX,
    WATCH_EVENT_QUEUE_MAX, WatchBatch, WatchCoverage, WatchError, WatchEvent, WatchEventError,
    WatchFidelity, WatchKind, is_ignored, watch_limit_setting,
};

/// The number of results that the runtime holds for the event loop.
pub const EVENT_QUEUE_CAPACITY: usize = 256;

/// The number of external processes that run at the same time.
pub const PROCESS_CONCURRENCY_LIMIT: usize = 8;

/// The largest number of worker jobs that run at the same time.
pub const WORKER_CONCURRENCY_LIMIT_MAX: usize = 8;

/// The largest standard input that one process request sends.
pub const PROCESS_INPUT_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The output limit that [`ProcessRequest::new`] applies.
pub const PROCESS_OUTPUT_BYTES_DEFAULT: usize = 1024 * 1024;

/// The largest output limit that one process request may request.
pub const PROCESS_OUTPUT_BYTES_MAX: usize = 16 * 1024 * 1024;

/// The deadline that [`ProcessRequest::new`] applies.
pub const PROCESS_DEADLINE_DEFAULT: Duration = Duration::from_secs(10);

/// The suggested deadline for one worker job.
pub const WORKER_DEADLINE_DEFAULT: Duration = Duration::from_secs(5);

static PROCESS_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// Returns the process pool that every default runtime shares.
///
/// One editor must not multiply its external-process capacity by constructing a
/// second runtime.
fn process_permits() -> Arc<Semaphore> {
    Arc::clone(PROCESS_PERMITS.get_or_init(|| Arc::new(Semaphore::new(PROCESS_CONCURRENCY_LIMIT))))
}

/// Validated concurrency and queue capacities for one runtime.
///
/// # Examples
///
/// ```
/// use kvim_runtime::{RuntimeLimits, SubmitError};
///
/// let limits = RuntimeLimits::new(16, 2, 2).unwrap();
/// assert_eq!(limits.event_queue(), 16);
/// assert!(matches!(
///     RuntimeLimits::new(0, 2, 2),
///     Err(SubmitError::InvalidLimits)
/// ));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeLimits {
    event_queue: usize,
    workers: usize,
    processes: usize,
}

impl RuntimeLimits {
    /// Creates limits from explicit capacities.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError::InvalidLimits`] when any capacity is zero. A zero
    /// capacity would reject every request.
    pub const fn new(
        event_queue: usize,
        workers: usize,
        processes: usize,
    ) -> Result<Self, SubmitError> {
        if event_queue == 0 || workers == 0 || processes == 0 {
            return Err(SubmitError::InvalidLimits);
        }
        Ok(Self {
            event_queue,
            workers,
            processes,
        })
    }

    /// Returns the number of results that the runtime holds.
    #[must_use]
    pub const fn event_queue(self) -> usize {
        self.event_queue
    }

    /// Returns the number of worker jobs that run at the same time.
    #[must_use]
    pub const fn workers(self) -> usize {
        self.workers
    }

    /// Returns the number of processes that run at the same time.
    #[must_use]
    pub const fn processes(self) -> usize {
        self.processes
    }
}

impl Default for RuntimeLimits {
    /// Clamps the detected parallelism into the supported worker range.
    fn default() -> Self {
        let workers = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .clamp(1, WORKER_CONCURRENCY_LIMIT_MAX);
        Self {
            event_queue: EVENT_QUEUE_CAPACITY,
            workers,
            processes: PROCESS_CONCURRENCY_LIMIT,
        }
    }
}

/// A monotonically increasing identity for one background request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId(u64);

impl RequestId {
    /// Returns the underlying value for logs and comparisons.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A stable operation slot whose newest request may publish.
///
/// One slot names one operation, such as the file picker or the active buffer
/// analysis. The caller assigns the concrete slot values.
///
/// # Examples
///
/// ```
/// use kvim_runtime::RequestSlot;
///
/// let picker = RequestSlot::new(1);
/// assert_eq!(picker, RequestSlot::new(1));
/// assert_ne!(picker, RequestSlot::new(2));
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestSlot(u16);

impl RequestSlot {
    /// Creates a slot from a stable operation number.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }
}

/// Cancellation ownership and identity for one accepted request.
#[derive(Clone, Debug)]
pub struct RequestHandle {
    id: RequestId,
    slot: RequestSlot,
    cancellation: CancellationToken,
}

impl RequestHandle {
    /// Returns the identity of this request.
    #[must_use]
    pub const fn id(&self) -> RequestId {
        self.id
    }

    /// Returns the publication slot of this request.
    #[must_use]
    pub const fn slot(&self) -> RequestSlot {
        self.slot
    }

    /// Returns the cancellation token that the background job observes.
    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Cancels this request.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}

/// Tracks the newest request in each publication slot.
///
/// The gate never mutates visible state. The event loop asks the gate before it
/// applies a result. [`PublicationGate::begin`] cancels the request that it
/// displaces, so obsolete work stops as early as its job allows.
///
/// # Examples
///
/// ```
/// use kvim_runtime::{PublicationGate, RequestSlot};
/// use tokio_util::sync::CancellationToken;
///
/// let root = CancellationToken::new();
/// let gate = PublicationGate::default();
/// let first = gate.begin(RequestSlot::new(1), &root);
/// let second = gate.begin(RequestSlot::new(1), &root);
///
/// assert!(first.cancellation().is_cancelled());
/// assert!(!gate.accepts(&first));
/// assert!(gate.accepts(&second));
/// ```
#[derive(Debug, Default)]
pub struct PublicationGate {
    next_id: AtomicU64,
    active: Mutex<HashMap<RequestSlot, RequestHandle>>,
}

impl PublicationGate {
    /// Starts one request and cancels the prior request in the same slot.
    pub fn begin(&self, slot: RequestSlot, parent: &CancellationToken) -> RequestHandle {
        let id = RequestId(self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let handle = RequestHandle {
            id,
            slot,
            cancellation: parent.child_token(),
        };
        let prior = self.lock_active().insert(slot, handle.clone());
        if let Some(prior) = prior {
            prior.cancel();
        }
        handle
    }

    /// Returns true only while this handle is the newest request in its slot.
    #[must_use]
    pub fn accepts(&self, handle: &RequestHandle) -> bool {
        self.lock_active()
            .get(&handle.slot)
            .is_some_and(|active| active.id == handle.id)
    }

    /// Cancels every tracked request.
    pub fn cancel_all(&self) {
        for handle in self.lock_active().values() {
            handle.cancel();
        }
    }

    fn lock_active(&self) -> MutexGuard<'_, HashMap<RequestSlot, RequestHandle>> {
        self.active
            .lock()
            .expect("the gate mutex guards only local map operations that cannot panic")
    }
}

/// The service that produced one result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkKind {
    /// A processor-bound job on the worker service.
    Worker,
    /// An external command on the process service.
    Process,
}

/// The resource that rejected one submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaturatedResource {
    /// The result queue holds no free slot.
    EventQueue,
    /// The process service holds no free permit.
    Processes,
    /// The worker service holds no free permit.
    Workers,
}

/// The reason that the runtime refused one submission.
///
/// A refused submission starts no work and produces no [`RuntimeEvent`].
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SubmitError {
    /// A runtime limit was zero.
    #[error("runtime limits must be nonzero")]
    InvalidLimits,
    /// The runtime rejects new work.
    #[error("runtime is shutting down")]
    ShuttingDown,
    /// One bounded resource is full.
    #[error("runtime resource is saturated: {0:?}")]
    Saturated(SaturatedResource),
    /// The process request exceeds the input or output bound.
    #[error("process request exceeds the runtime input or output limit")]
    ProcessBounds,
}

/// The reason that one accepted request produced no value.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// The request was cancelled, superseded, or stopped by shutdown.
    #[error("request was cancelled")]
    Cancelled,
    /// The request exceeded its deadline.
    #[error("request exceeded its deadline")]
    Timeout,
    /// The worker thread panicked or was aborted.
    #[error("background worker failed")]
    WorkerFailure(#[source] tokio::task::JoinError),
    /// The command could not start.
    #[error("failed to spawn process")]
    ProcessSpawn(#[source] std::io::Error),
    /// The runtime could not read the process output.
    #[error("failed to read process output")]
    ProcessRead(#[source] std::io::Error),
    /// The runtime could not write the process input.
    #[error("failed to write process input")]
    ProcessWrite(#[source] std::io::Error),
    /// The process wrote more than its output limit.
    #[error("process output exceeded {limit} bytes")]
    OutputLimit {
        /// The shared limit over standard output and standard error.
        limit: usize,
    },
}

/// One guaranteed result for one accepted background request.
#[derive(Debug)]
pub struct RuntimeEvent<T> {
    /// The identity and cancellation owner of the request.
    pub request: RequestHandle,
    /// The service that produced the result.
    pub kind: WorkKind,
    /// The typed value or the typed failure.
    pub result: Result<T, RuntimeError>,
}

/// Receives accepted results on the terminal event loop.
///
/// The receiver stays open while the runtime lives. It returns `None` after the
/// runtime is dropped and every accepted result is delivered.
pub struct EventReceiver<T> {
    receiver: mpsc::Receiver<RuntimeEvent<T>>,
}

impl<T> EventReceiver<T> {
    /// Waits for the next result.
    pub async fn recv(&mut self) -> Option<RuntimeEvent<T>> {
        self.receiver.recv().await
    }

    /// Takes one ready result without waiting.
    ///
    /// # Errors
    ///
    /// Returns the queue state when no result is ready.
    pub fn try_recv(&mut self) -> Result<RuntimeEvent<T>, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

/// A bounded external command.
///
/// # Examples
///
/// ```
/// use kvim_runtime::{PROCESS_DEADLINE_DEFAULT, PROCESS_OUTPUT_BYTES_DEFAULT, ProcessRequest};
///
/// let mut request = ProcessRequest::new("rg");
/// request.args = vec!["--json".into(), "needle".into()];
/// assert_eq!(request.output_bytes_max, PROCESS_OUTPUT_BYTES_DEFAULT);
/// assert_eq!(request.deadline, PROCESS_DEADLINE_DEFAULT);
/// ```
#[derive(Clone, Debug)]
pub struct ProcessRequest {
    /// The command to run.
    pub program: OsString,
    /// The command arguments.
    pub args: Vec<OsString>,
    /// The working directory of the child.
    pub current_dir: Option<PathBuf>,
    /// The variables that the child must not inherit from this process.
    ///
    /// A caller drops every name that could redirect the command or make it
    /// start another program. The runtime drops these names before it applies
    /// [`ProcessRequest::child_variables`], so a name in both lists keeps the
    /// value that the caller chose.
    pub dropped_variables: Vec<OsString>,
    /// The variables that the child receives with an explicit value.
    pub child_variables: Vec<(OsString, OsString)>,
    /// The bytes that the runtime writes to standard input.
    pub stdin: Vec<u8>,
    /// The shared limit over standard output and standard error.
    pub output_bytes_max: usize,
    /// The deadline for the complete run.
    pub deadline: Duration,
}

impl ProcessRequest {
    /// Creates a request with the default output limit and deadline.
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            current_dir: None,
            dropped_variables: Vec::new(),
            child_variables: Vec::new(),
            stdin: Vec::new(),
            output_bytes_max: PROCESS_OUTPUT_BYTES_DEFAULT,
            deadline: PROCESS_DEADLINE_DEFAULT,
        }
    }
}

/// One captured process result with bounded output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    /// The exit code, or `None` after a signal.
    pub status_code: Option<i32>,
    /// The captured standard output.
    pub stdout: Vec<u8>,
    /// The captured standard error.
    pub stderr: Vec<u8>,
}

/// Owns bounded worker jobs, child processes, and result delivery.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use kvim_runtime::{PublicationGate, RequestSlot, Runtime};
///
/// # let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
/// #     .worker_threads(1)
/// #     .enable_all()
/// #     .build()
/// #     .unwrap();
/// tokio_runtime.block_on(async {
///     let (runtime, mut events) = Runtime::new();
///     let gate = PublicationGate::default();
///     let request = gate.begin(RequestSlot::new(1), &runtime.cancellation_root());
///
///     runtime
///         .submit_worker(request, Duration::from_secs(1), |_cancellation| 21 * 2)
///         .unwrap();
///
///     let event = events.recv().await.unwrap();
///     assert_eq!(event.result.unwrap(), 42);
///     runtime.shutdown().await;
/// });
/// ```
pub struct Runtime<T> {
    event_sender: mpsc::Sender<RuntimeEvent<T>>,
    worker_permits: Arc<Semaphore>,
    process_permits: Arc<Semaphore>,
    shutdown: CancellationToken,
    shutting_down: AtomicBool,
    tasks: TaskTracker,
}

impl<T> Drop for Runtime<T> {
    /// Rejects new work and cancels owned work, but waits for nothing.
    ///
    /// Normal editor shutdown must call [`Runtime::shutdown`]. This drop is only
    /// the safety net for a panic or an early return.
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        self.tasks.close();
        self.shutdown.cancel();
    }
}

impl<T> Runtime<T>
where
    T: Send + 'static,
{
    /// Creates a runtime that shares the one process pool of this program.
    ///
    /// A second default runtime reuses the same process permits, so it adds no
    /// external-process capacity.
    #[must_use]
    pub fn new() -> (Self, EventReceiver<T>) {
        Self::with_resources(RuntimeLimits::default(), process_permits())
    }

    /// Creates an isolated runtime with explicit limits.
    ///
    /// The process permits belong to this runtime alone. Use this constructor
    /// for tests and for embedded runtimes with their own budget.
    #[must_use]
    pub fn with_limits(limits: RuntimeLimits) -> (Self, EventReceiver<T>) {
        let process_permits = Arc::new(Semaphore::new(limits.processes));
        Self::with_resources(limits, process_permits)
    }

    fn with_resources(
        limits: RuntimeLimits,
        process_permits: Arc<Semaphore>,
    ) -> (Self, EventReceiver<T>) {
        let (event_sender, receiver) = mpsc::channel(limits.event_queue);
        (
            Self {
                event_sender,
                worker_permits: Arc::new(Semaphore::new(limits.workers)),
                process_permits,
                shutdown: CancellationToken::new(),
                shutting_down: AtomicBool::new(false),
                tasks: TaskTracker::new(),
            },
            EventReceiver { receiver },
        )
    }

    /// Returns the token that cancels every request of this runtime.
    #[must_use]
    pub fn cancellation_root(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Accepts one worker job without waiting for worker or result capacity.
    ///
    /// The job receives the cancellation token of the request. A long job must
    /// check that token between its steps.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError::ShuttingDown`] after shutdown started, or
    /// [`SubmitError::Saturated`] when the result queue or the worker service is
    /// full.
    pub fn submit_worker<F>(
        &self,
        request: RequestHandle,
        deadline: Duration,
        job: F,
    ) -> Result<(), SubmitError>
    where
        F: FnOnce(CancellationToken) -> T + Send + 'static,
    {
        self.ensure_running()?;
        // Reserve delivery before capacity, so every accepted request keeps one
        // guaranteed result slot for its whole run.
        let result_slot = self.reserve_result_slot()?;
        let worker_permit = self
            .worker_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| SubmitError::Saturated(SaturatedResource::Workers))?;
        self.tasks.spawn(run_worker_job(
            request,
            deadline,
            job,
            worker_permit,
            result_slot,
            self.shutdown.clone(),
        ));
        Ok(())
    }

    /// Accepts one external command without waiting for process or result capacity.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError::ProcessBounds`] when the request exceeds
    /// [`PROCESS_INPUT_BYTES_MAX`] or [`PROCESS_OUTPUT_BYTES_MAX`], and returns
    /// the saturation or shutdown reasons of [`Runtime::submit_worker`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use kvim_runtime::{ProcessRequest, PublicationGate, RequestSlot, Runtime};
    ///
    /// # let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
    /// #     .worker_threads(1)
    /// #     .enable_all()
    /// #     .build()
    /// #     .unwrap();
    /// tokio_runtime.block_on(async {
    ///     let (runtime, mut events) = Runtime::new();
    ///     let gate = PublicationGate::default();
    ///     let request = gate.begin(RequestSlot::new(1), &runtime.cancellation_root());
    ///     let mut process = ProcessRequest::new("rg");
    ///     process.args = vec!["--files".into()];
    ///
    ///     runtime
    ///         .submit_process(request, process, |output| output.stdout)
    ///         .unwrap();
    ///
    ///     let event = events.recv().await.unwrap();
    ///     assert!(event.result.is_ok());
    ///     runtime.shutdown().await;
    /// });
    /// ```
    pub fn submit_process<F>(
        &self,
        request: RequestHandle,
        process: ProcessRequest,
        map: F,
    ) -> Result<(), SubmitError>
    where
        F: FnOnce(ProcessOutput) -> T + Send + 'static,
    {
        self.ensure_running()?;
        if process.output_bytes_max == 0
            || process.output_bytes_max > PROCESS_OUTPUT_BYTES_MAX
            || process.stdin.len() > PROCESS_INPUT_BYTES_MAX
        {
            return Err(SubmitError::ProcessBounds);
        }
        let result_slot = self.reserve_result_slot()?;
        let process_permit = self
            .process_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| SubmitError::Saturated(SaturatedResource::Processes))?;
        self.tasks.spawn(run_process(
            request,
            process,
            map,
            process_permit,
            result_slot,
            self.shutdown.clone(),
        ));
        Ok(())
    }

    /// Rejects new work, cancels owned work, and waits for cleanup.
    ///
    /// The operation consumes the runtime, so no caller can submit after it.
    pub async fn shutdown(self) {
        self.shutting_down.store(true, Ordering::Release);
        self.tasks.close();
        self.shutdown.cancel();
        self.tasks.wait().await;
    }

    fn ensure_running(&self) -> Result<(), SubmitError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(SubmitError::ShuttingDown);
        }
        Ok(())
    }

    fn reserve_result_slot(&self) -> Result<mpsc::OwnedPermit<RuntimeEvent<T>>, SubmitError> {
        self.event_sender
            .clone()
            .try_reserve_owned()
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    SubmitError::Saturated(SaturatedResource::EventQueue)
                }
                mpsc::error::TrySendError::Closed(_) => SubmitError::ShuttingDown,
            })
    }
}

async fn run_worker_job<T, F>(
    request: RequestHandle,
    deadline: Duration,
    job: F,
    _worker_permit: OwnedSemaphorePermit,
    result_slot: mpsc::OwnedPermit<RuntimeEvent<T>>,
    shutdown: CancellationToken,
) where
    T: Send + 'static,
    F: FnOnce(CancellationToken) -> T + Send + 'static,
{
    let cancellation = request.cancellation();
    // Cancellation can arrive between submission and the first poll. Do not
    // start blocking work that no caller will accept.
    if cancellation.is_cancelled() || shutdown.is_cancelled() {
        result_slot.send(RuntimeEvent {
            request,
            kind: WorkKind::Worker,
            result: Err(RuntimeError::Cancelled),
        });
        return;
    }
    let worker_cancellation = cancellation.clone();
    let mut worker = tokio::task::spawn_blocking(move || job(worker_cancellation));
    let result = tokio::select! {
        biased;
        () = shutdown.cancelled() => {
            cancellation.cancel();
            Err(RuntimeError::Cancelled)
        },
        () = cancellation.cancelled() => Err(RuntimeError::Cancelled),
        result = time::timeout(deadline, &mut worker) => match result {
            // A value that finished after cancellation is obsolete by decision,
            // even though the worker produced it.
            Ok(Ok(_)) if cancellation.is_cancelled() => Err(RuntimeError::Cancelled),
            Ok(Ok(value)) => Ok(value),
            Ok(Err(source)) => Err(RuntimeError::WorkerFailure(source)),
            Err(_) => {
                cancellation.cancel();
                Err(RuntimeError::Timeout)
            },
        },
    };
    result_slot.send(RuntimeEvent {
        request,
        kind: WorkKind::Worker,
        result,
    });
    // Shutdown must wait for the blocking thread, not only for this task.
    if !worker.is_finished() {
        let _ = worker.await;
    }
}

async fn run_process<T, F>(
    request: RequestHandle,
    process: ProcessRequest,
    map: F,
    _process_permit: OwnedSemaphorePermit,
    result_slot: mpsc::OwnedPermit<RuntimeEvent<T>>,
    shutdown: CancellationToken,
) where
    T: Send + 'static,
    F: FnOnce(ProcessOutput) -> T + Send + 'static,
{
    let cancellation = request.cancellation();
    if cancellation.is_cancelled() || shutdown.is_cancelled() {
        result_slot.send(RuntimeEvent {
            request,
            kind: WorkKind::Process,
            result: Err(RuntimeError::Cancelled),
        });
        return;
    }
    let deadline = process.deadline;
    let result = tokio::select! {
        biased;
        () = shutdown.cancelled() => {
            cancellation.cancel();
            Err(RuntimeError::Cancelled)
        },
        () = cancellation.cancelled() => Err(RuntimeError::Cancelled),
        result = time::timeout(deadline, execute_process(process)) => match result {
            Ok(result) => result.map(map),
            Err(_) => Err(RuntimeError::Timeout),
        },
    };
    // Dropping the process future kills the child, because the command sets
    // `kill_on_drop`.
    result_slot.send(RuntimeEvent {
        request,
        kind: WorkKind::Process,
        result,
    });
}

async fn execute_process(process: ProcessRequest) -> Result<ProcessOutput, RuntimeError> {
    debug_assert!(
        process.output_bytes_max > 0 && process.output_bytes_max <= PROCESS_OUTPUT_BYTES_MAX,
        "submit_process validates the output limit before it spawns the task"
    );
    let mut command = Command::new(&process.program);
    command
        .args(&process.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(current_dir) = &process.current_dir {
        command.current_dir(current_dir);
    }
    for name in &process.dropped_variables {
        command.env_remove(name);
    }
    for (name, value) in &process.child_variables {
        command.env(name, value);
    }
    let mut child = command.spawn().map_err(RuntimeError::ProcessSpawn)?;
    let mut stdin = child
        .stdin
        .take()
        .expect("the command configures a piped standard input");
    let stdout = child
        .stdout
        .take()
        .expect("the command configures a piped standard output");
    let stderr = child
        .stderr
        .take()
        .expect("the command configures a piped standard error");
    let input = process.stdin;
    let output_limit = process.output_bytes_max;
    let write_input = async move {
        stdin
            .write_all(&input)
            .await
            .map_err(RuntimeError::ProcessWrite)?;
        stdin.shutdown().await.map_err(RuntimeError::ProcessWrite)
    };
    let read_output = async move {
        // One shared counter bounds both streams together, so a noisy standard
        // error cannot double the captured bytes.
        let captured_bytes = Arc::new(AtomicUsize::new(0));
        let (stdout, stderr) = tokio::try_join!(
            read_bounded(stdout, Arc::clone(&captured_bytes), output_limit),
            read_bounded(stderr, captured_bytes, output_limit)
        )?;
        debug_assert!(
            stdout.len().saturating_add(stderr.len()) <= output_limit,
            "the shared counter bounds both captured streams"
        );
        Ok((stdout, stderr))
    };
    let (status, (), (stdout, stderr)) = tokio::try_join!(
        async { child.wait().await.map_err(RuntimeError::ProcessRead) },
        write_input,
        read_output,
    )?;
    Ok(ProcessOutput {
        status_code: status.code(),
        stdout,
        stderr,
    })
}

/// The read buffer for one captured stream.
const PROCESS_READ_CHUNK_BYTES: usize = 8 * 1024;

/// The largest buffer that one stream preallocates.
const PROCESS_READ_RESERVE_BYTES: usize = 64 * 1024;

async fn read_bounded<R>(
    mut reader: R,
    captured_bytes: Arc<AtomicUsize>,
    limit: usize,
) -> Result<Vec<u8>, RuntimeError>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::with_capacity(limit.min(PROCESS_READ_RESERVE_BYTES));
    let mut buffer = [0_u8; PROCESS_READ_CHUNK_BYTES];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .map_err(RuntimeError::ProcessRead)?;
        if count == 0 {
            return Ok(output);
        }
        let reserved =
            captured_bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |captured| {
                captured.checked_add(count).filter(|total| *total <= limit)
            });
        if reserved.is_err() {
            return Err(RuntimeError::OutputLimit { limit });
        }
        output.extend_from_slice(&buffer[..count]);
    }
}
