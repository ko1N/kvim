//! The terminal-independent editor driver.
//!
//! The driver owns the external services of one editor instance: request
//! identity, result routing, the publication gates, the optional language and
//! watch handles, and the shutdown state. It owns no terminal and no event
//! loop. The host owns both, and the host also owns the visible [`Session`]
//! state that every result reaches. See `docs/embedding.md`.
//!
//! The driver creates no runtime and starts no detached task. The caller
//! supplies one bounded worker and process spawner, and the driver submits
//! every filesystem read, external command, Git read, formatter run, and
//! Tree-sitter parse through it. No entry point of this module performs that
//! work itself, so a host event loop stays free of it. See
//! `docs/responsiveness.md`.
//!
//! [`EditorDriver::shutdown`] consumes the driver, rejects new work, cancels
//! pre-commit work, closes the optional services, and observes one deadline. A
//! deadline that expires while a committed task can still hold a mandatory
//! event returns a must-use [`ShutdownDrain`]. The host keeps its runtime alive
//! until that drain completes.

use std::fmt;
use std::time::Duration;

use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use kvim_language::{
    ANALYSIS_DEADLINE, FormattedDocument, FormatterFailure, LanguageEvent, LanguageServices,
    LspError,
};
use kvim_runtime::{
    EventReceiver, FileWatcher, ProcessOutput, PublicationGate, RequestSlot, Runtime, RuntimeDrain,
    RuntimeError, RuntimeEvent, SubmitError, WORKER_DEADLINE_DEFAULT, WatchBatch,
};
use kvim_workspace::{
    BUFFERS_MAX, FileResult, GitStatusFailure, GitStatusRead, PickerResult, PickerSlot,
    WorkspaceResult,
};

use super::clipboard::{command_failure, refused_submission};
use super::embed::EditorInstanceId;
use super::language::{LANGUAGE_OUTBOX_MAX, Refusal, send_request};
use super::picker::PickerFailure;
use super::session::{
    AnalysisResult, FileRequestFailure, HostProbeFailure, JOB_ANALYSIS, JOB_OBSOLETE, JOB_REFUSED,
    JOB_WALK, MessageLevel, Redraw, Session,
};

/// One completed background operation of one editor instance.
///
/// The value is opaque. A host names it only to build the bounded spawner that
/// [`EditorDriver`] submits its work through, because the spawner is generic
/// over the value that it delivers. See `docs/embedding.md`.
///
/// # Examples
///
/// ```no_run
/// use kvim_runtime::Runtime;
/// use kvim_tui::EditorWork;
///
/// let (spawner, results) = Runtime::<EditorWork>::new();
/// drop((spawner, results));
/// ```
pub struct EditorWork(WorkResult);

impl fmt::Debug for EditorWork {
    /// Names the service that produced the value, and no payload.
    ///
    /// A buffer text, a search result, and a clipboard payload can all hold
    /// user data, so the report names the kind alone.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("EditorWork")
            .field(&self.0.kind())
            .finish()
    }
}

/// One finished unit of background work that the driver observed.
///
/// The value carries the instance that produced it, so a host that drives
/// several editors routes it back to the editor that owns its state.
#[must_use = "a completed unit of work must reach the editor that owns its state"]
pub struct Completed {
    instance: EditorInstanceId,
    outcome: Outcome,
}

impl fmt::Debug for Completed {
    /// Names the instance and the service, and no payload.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Completed")
            .field("instance", &self.instance)
            .field("outcome", &self.outcome.kind())
            .finish()
    }
}

impl Completed {
    /// Returns the editor that owns this work.
    #[inline]
    #[must_use]
    pub const fn instance(&self) -> EditorInstanceId {
        self.instance
    }
}

/// What one wait of the driver produced.
enum Outcome {
    /// One request of the bounded spawner finished, or the spawner is gone.
    Work(Box<Option<RuntimeEvent<EditorWork>>>),
    /// The language services published one typed event.
    Language(LanguageEvent),
    /// The workspace watcher published one coalesced burst, or the watch ended.
    Watch(Box<Option<WatchBatch>>),
}

impl Outcome {
    /// Returns the service that produced this outcome.
    const fn kind(&self) -> &'static str {
        match self {
            Self::Work(_) => "work",
            Self::Language(_) => "language",
            Self::Watch(_) => "watch",
        }
    }
}

/// The external services of one embedded editor instance.
///
/// The driver holds the caller-supplied bounded spawner, the result stream of
/// that spawner, the publication gates, and the optional language and watch
/// handles. It performs no filesystem, process, Git, language-server,
/// formatting, or Tree-sitter work of its own: every such job leaves through
/// the spawner as a submitted task. See `docs/responsiveness.md`.
///
/// The host owns the event loop. It calls [`EditorDriver::dispatch`] after each
/// transition, waits on [`EditorDriver::recv`] beside its own events, and hands
/// each finished unit back through [`EditorDriver::apply`].
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use ratatui::layout::Rect;
///
/// use kvim_runtime::Runtime;
/// use kvim_settings::EditorSettings;
/// use kvim_tui::{EditorDriver, EditorWork, Session};
///
/// # let tokio_runtime = tokio::runtime::Builder::new_current_thread()
/// #     .enable_all()
/// #     .build()
/// #     .expect("the example builds one runtime");
/// # tokio_runtime.block_on(async {
/// let root = std::sync::Arc::new(
///     kvim_path::WorktreeRoot::open(
///         std::env::current_dir().expect("the process holds a working directory"),
///     )
///     .expect("the working directory is a worktree"),
/// );
/// let mut session = Session::new(Rect::new(0, 0, 80, 24), EditorSettings::default(), root);
///
/// // The host owns the spawner, so the driver creates no runtime of its own.
/// let (spawner, results) = Runtime::<EditorWork>::new();
/// let mut driver = EditorDriver::new(session.instance(), spawner, results);
/// assert_eq!(driver.instance(), session.instance());
///
/// let _redraw = driver.dispatch(&mut session);
/// let drain = driver.shutdown(&mut session, Duration::from_secs(5)).await;
/// assert!(drain.is_none(), "every task of this editor finished");
/// # });
/// ```
pub struct EditorDriver {
    instance: EditorInstanceId,
    spawner: Runtime<EditorWork>,
    results: EventReceiver<EditorWork>,
    gate: PublicationGate,
    language: Option<LanguageServices>,
    watcher: Option<FileWatcher>,
}

impl EditorDriver {
    /// Creates the driver of one editor instance.
    ///
    /// The caller owns the spawner, so the driver creates no runtime and starts
    /// no detached task. Capacity is isolated for one instance unless the
    /// caller supplies a spawner that shares an explicit pool.
    #[must_use]
    pub fn new(
        instance: EditorInstanceId,
        spawner: Runtime<EditorWork>,
        results: EventReceiver<EditorWork>,
    ) -> Self {
        Self {
            instance,
            spawner,
            results,
            gate: PublicationGate::default(),
            language: None,
            watcher: None,
        }
    }

    /// Adds the language services of this instance.
    ///
    /// The services are optional. An editor without them stays fully usable,
    /// with no diagnostics, no completion, and no external formatter.
    #[must_use]
    pub fn with_language(mut self, language: LanguageServices) -> Self {
        self.language = Some(language);
        self
    }

    /// Adds the workspace watcher of this instance.
    ///
    /// The watcher is optional. An editor without it stays fully usable, and
    /// the refresh command reads the workspace by hand.
    #[must_use]
    pub fn with_watcher(mut self, watcher: FileWatcher) -> Self {
        self.watcher = Some(watcher);
        self
    }

    /// Returns the editor that this driver serves.
    #[inline]
    #[must_use]
    pub const fn instance(&self) -> EditorInstanceId {
        self.instance
    }

    /// Hands every queued request of one host transition to its service.
    ///
    /// The call returns at once. A refused submission reaches the editor as a
    /// typed failure, which names its state on the message line, so the
    /// returned value reports that visible change and the host frame follows
    /// the dispatch as well as the transition before it.
    ///
    /// The call publishes no editor fact. The host reads those facts through
    /// [`Session::take_event`], because the host decides their effect.
    pub fn dispatch(&mut self, editor: &mut Session) -> Redraw {
        debug_assert_eq!(
            self.instance,
            editor.instance(),
            "one driver serves the one editor that it was created for"
        );
        dispatch(editor, &self.spawner, &self.gate, &mut self.language)
    }

    /// Waits for the next finished unit of background work.
    ///
    /// The future performs no work of its own. It installs no terminal, no
    /// signal handler, no panic hook, no tracing subscriber, and no other
    /// process-global owner, so a host can hold one of these futures for every
    /// editor that it runs.
    ///
    /// Every branch of the wait is cancellation safe, so a host may drop the
    /// future inside its own selection and lose no result.
    pub async fn recv(&mut self) -> Completed {
        let outcome = tokio::select! {
            event = self.results.recv() => Outcome::Work(Box::new(event)),
            event = next_language_event(&mut self.language) => Outcome::Language(event),
            batch = next_watch_batch(&mut self.watcher) => Outcome::Watch(Box::new(batch)),
        };
        Completed {
            instance: self.instance,
            outcome,
        }
    }

    /// Applies one finished unit of work as one editor transition.
    ///
    /// The publication gate rejects an obsolete result before it reaches the
    /// visible state, so a superseded picker, preview, completion, analysis,
    /// formatter, or language-server answer changes nothing.
    ///
    /// `now` is the elapsed time of the host, which the editor stamps on its
    /// log entries and its overlays. The editor reads no clock of its own.
    pub fn apply(&mut self, editor: &mut Session, completed: Completed, now: Duration) -> Redraw {
        debug_assert_eq!(
            self.instance, completed.instance,
            "one driver applies the work of the editor that it serves"
        );
        debug_assert_eq!(
            self.instance,
            editor.instance(),
            "one driver serves the one editor that it was created for"
        );
        editor.advance_clock(now);
        match completed.outcome {
            Outcome::Work(event) => complete(editor, &self.gate, *event),
            Outcome::Language(event) => publish(editor, event),
            Outcome::Watch(batch) => publish_watch(editor, batch.as_ref().as_ref()),
        }
    }

    /// Rejects new work, cancels pre-commit work, and closes every service.
    ///
    /// The operation consumes the driver, so no caller can submit after it. The
    /// watcher stops first, because a stopped watch queues no further directory
    /// read for the services below it.
    ///
    /// The driver cancels every request that has not committed yet. A task that
    /// entered its commit masks that cancellation and stays tracked, so the
    /// wait below covers its mandatory event. The driver aborts no such task.
    ///
    /// Returns `None` after every tracked task finished and every remaining
    /// result reached `editor`, which leaves the mandatory events in the
    /// bounded outbox of the editor for the host to read.
    ///
    /// Returns [`ShutdownDrain`] when `deadline` expired first. The drain owns
    /// the remaining tasks and their delivery, and the host must keep its
    /// runtime alive until the drain completes.
    pub async fn shutdown(self, editor: &mut Session, deadline: Duration) -> Option<ShutdownDrain> {
        debug_assert_eq!(
            self.instance,
            editor.instance(),
            "one driver serves the one editor that it was created for"
        );
        let Self {
            instance,
            spawner,
            mut results,
            gate,
            language,
            watcher,
        } = self;
        let expiry = Instant::now() + deadline;
        if let Some(watcher) = watcher {
            let _expired = timeout_at(expiry, watcher.shutdown()).await;
        }
        if let Some(language) = language {
            let _expired = timeout_at(expiry, language.shutdown()).await;
        }
        // The drop of the spawner inside this call rejects new work and cancels
        // every request that has not committed yet. The drain keeps the tracked
        // tasks, so a committed side effect still reaches its reserved slot.
        let tasks = spawner.begin_shutdown();
        if timeout_at(expiry, tasks.wait()).await.is_err() {
            return Some(ShutdownDrain {
                instance,
                tasks,
                results,
                gate,
            });
        }
        drain_results(editor, &gate, &mut results);
        None
    }
}

/// The tracked work of one driver whose shutdown deadline expired.
///
/// The drain owns every task that can still commit a side effect, the result
/// stream of those tasks, and the publication gates that admit their answers.
/// The host must keep its asynchronous runtime alive until
/// [`ShutdownDrain::complete`] returns. See `docs/embedding.md`.
#[must_use = "the drain owns the mandatory events of every committed side effect"]
pub struct ShutdownDrain {
    instance: EditorInstanceId,
    tasks: RuntimeDrain,
    results: EventReceiver<EditorWork>,
    gate: PublicationGate,
}

impl ShutdownDrain {
    /// Returns the editor that owns the remaining events.
    #[inline]
    #[must_use]
    pub const fn instance(&self) -> EditorInstanceId {
        self.instance
    }

    /// Waits for every tracked task and publishes every mandatory event.
    ///
    /// The wait is bounded by the deadlines of the submitted work alone, so it
    /// returns without a further deadline of its own. Every remaining result
    /// then reaches `editor`, which commits the reserved slot of each completed
    /// write and each completed workspace mutation.
    pub async fn complete(mut self, editor: &mut Session) -> Redraw {
        debug_assert_eq!(
            self.instance,
            editor.instance(),
            "one drain serves the editor of the driver that produced it"
        );
        self.tasks.wait().await;
        drain_results(editor, &self.gate, &mut self.results)
    }
}

/// Applies every ready result of one ended spawner.
///
/// Every tracked task already finished, so the stream holds every result that a
/// committed side effect produced and no further result can arrive.
fn drain_results(
    editor: &mut Session,
    gate: &PublicationGate,
    results: &mut EventReceiver<EditorWork>,
) -> Redraw {
    let mut redraw = Redraw::Skipped;
    while let Ok(event) = results.try_recv() {
        redraw = redraw.or(complete(editor, gate, Some(event)));
    }
    debug_assert!(
        results.try_recv().is_err(),
        "every tracked task finished, so no further result can arrive"
    );
    redraw
}

/// The publication slot of every file operation.
///
/// The editor runs one file operation at a time, so one slot holds every open
/// and every save. A newer request cancels the older request in this slot.
const FILE_SLOT: RequestSlot = RequestSlot::new(1);

/// The publication slot of every buffer analysis.
///
/// One slot holds the analysis of the active buffer, so a newer buffer version
/// cancels the parse of the version that it replaced.
const ANALYSIS_SLOT: RequestSlot = RequestSlot::new(2);

/// The publication slot of every workspace operation.
///
/// The file tree runs one directory read or one mutation at a time, so one slot
/// holds every workspace operation.
const WORKSPACE_SLOT: RequestSlot = RequestSlot::new(3);

/// The publication slot of the candidates of the open picker.
///
/// A newer query cancels the search that it replaces, so the obsolete `rg`
/// process stops and its result never reaches the screen.
const PICKER_SLOT: RequestSlot = RequestSlot::new(4);

/// The publication slot of the preview of the open picker.
///
/// A newer selection cancels the preview that it replaces.
const PREVIEW_SLOT: RequestSlot = RequestSlot::new(5);

/// The publication slot of every system clipboard command.
///
/// The session runs one clipboard operation at a time, so one slot holds every
/// write and every read. A newer operation cancels the command that it
/// replaces. See `docs/clipboard.md`.
const CLIPBOARD_SLOT: RequestSlot = RequestSlot::new(6);

/// The publication slot of the Git status of the workspace.
///
/// The file tree runs one status read at a time, so a newer trigger cancels the
/// read that it replaces and the gate rejects the obsolete result. See
/// `docs/git.md`.
const GIT_SLOT: RequestSlot = RequestSlot::new(7);

/// The publication slot of the external formatter of one buffer.
///
/// A save waits for its formatter answer, and the session starts no second
/// format while one runs, so one slot holds every formatter run. See
/// `docs/language-services.md`.
const FORMAT_SLOT: RequestSlot = RequestSlot::new(8);

/// The publication slot of the workspace walk of the command-line completion.
///
/// One open command line asks for one walk, so a newer command line cancels the
/// walk of the line that it replaces and the gate rejects the obsolete result.
/// See `docs/files.md`.
const COMPLETION_SLOT: RequestSlot = RequestSlot::new(9);

/// The publication slot of the host probe of the `:diagnostics` command.
///
/// One command starts one probe, and the session starts no second probe while
/// one runs, so one slot holds every host report. See `docs/architecture.md`.
const DIAGNOSTICS_SLOT: RequestSlot = RequestSlot::new(10);

/// The picker requests that one loop iteration submits.
///
/// One transition produces at most one candidate request and one preview
/// request, so the bound covers every request that it can produce.
const PICKER_DISPATCH_MAX: usize = 2;

/// The workspace requests that one loop iteration submits.
///
/// The file tree runs one workspace operation at a time, so one transition
/// produces at most one directory read or one mutation. The bound keeps the
/// submission loop finite even if a later change let the tree offer the same
/// read again, so a defect of that shape can never hang the event loop. See
/// `docs/responsiveness.md`.
const WORKSPACE_DISPATCH_MAX: usize = 1;

/// The language requests that one loop iteration sends.
///
/// The session holds a bounded outbox and one fresh open for each loaded
/// buffer, so this bound covers every request that one transition can produce.
const LANGUAGE_DISPATCH_MAX: usize = LANGUAGE_OUTBOX_MAX + BUFFERS_MAX;

/// The submission passes that one loop iteration runs.
///
/// One pass can queue the work of another owner: a formatting request that no
/// language server accepts completes the save that waited for it, and that save
/// must reach the worker service inside the same iteration. A single pass would
/// leave the save in its outbox until the next terminal event, so the write and
/// its report would follow the next key instead of the command. Two passes
/// cover that chain, because the second pass only reports a refusal on the
/// message line and queues no further work. The bound keeps the pass finite, so
/// a request that its service offers again can never hold the loop. See
/// `docs/responsiveness.md`.
const DISPATCH_PASSES_MAX: usize = 2;

/// One completed background operation of the editor.
///
/// The runtime is generic over its result, and the editor submits both file work
/// and language work, so one value names both.
enum WorkResult {
    /// One file operation finished.
    File(FileResult),
    /// One buffer analysis finished.
    Analysis(AnalysisResult),
    /// One workspace operation of the file tree finished.
    Workspace(WorkspaceResult),
    /// One picker operation finished.
    Picker(PickerResult),
    /// The workspace walk of the command-line completion finished.
    Completion(PickerResult),
    /// One system clipboard command finished.
    Clipboard(ProcessOutput),
    /// One Git status read of the workspace finished.
    Git(Result<GitStatusRead, GitStatusFailure>),
    /// One run of the external formatter of one buffer finished.
    Format(Result<Option<FormattedDocument>, FormatterFailure>),
    /// One host probe finished and produced the report as plain text.
    HostReport(String),
}

impl WorkResult {
    /// Returns the service that produced this result.
    const fn kind(&self) -> &'static str {
        match self {
            Self::File(_) => "file",
            Self::Analysis(_) => "analysis",
            Self::Workspace(_) => "workspace",
            Self::Picker(_) => "picker",
            Self::Completion(_) => "completion",
            Self::Clipboard(_) => "clipboard",
            Self::Git(_) => "git",
            Self::Format(_) => "format",
            Self::HostReport(_) => "host report",
        }
    }
}

/// Waits for the next result of the language services.
///
/// The future never completes while the editor runs without language services,
/// so the loop then waits for a terminal event alone.
async fn next_language_event(language: &mut Option<LanguageServices>) -> LanguageEvent {
    match language {
        Some(language) => match language.recv().await {
            Some(event) => event,
            // The services hold their own sender, so the queue never closes.
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

/// Waits for the next coalesced burst of the workspace watcher.
///
/// The future never completes while the editor runs without a watcher, so the
/// loop then waits for its other events alone.
///
/// Returns `None` once when the watch ended, which happens when the platform
/// refused the deferred registration. The call drops the ended watcher, so the
/// loop reports that state once and then waits for its other events alone.
async fn next_watch_batch(watcher: &mut Option<FileWatcher>) -> Option<WatchBatch> {
    let Some(active) = watcher else {
        return std::future::pending().await;
    };
    match active.recv().await {
        Some(batch) => Some(batch),
        None => {
            // The coalescing task ended, so no further burst can arrive. It
            // dropped the platform watcher before it closed this stream, so no
            // callback thread outlives this value.
            *watcher = None;
            None
        }
    }
}

/// Applies one coalesced burst of workspace filesystem changes.
///
/// A burst that never arrived reports that no watcher observes the workspace,
/// because the deferred registration failed. The editor stays fully usable and
/// the refresh command reads the workspace by hand. See `docs/files.md`.
fn publish_watch(editor: &mut Session, batch: Option<&WatchBatch>) -> Redraw {
    match batch {
        Some(batch) => editor.apply_watch_batch(batch),
        None => editor.report_watch_unavailable(),
    }
}

/// Applies one typed result of the language services.
///
/// The loop reports the elapsed time first, because a progress report and a
/// message both need it and neither carries a time of its own.
fn publish(editor: &mut Session, event: LanguageEvent) -> Redraw {
    editor.apply_language_event(event)
}

/// Hands the queued language requests to the language services.
///
/// Every call returns at once. The services own the process, the deadlines, and
/// the protocol bounds, so the loop never reads, writes, or waits for a server.
///
/// A refused request reaches the session as a typed failure, which reports the
/// state on the message line and completes a save that waited for a formatter.
/// The returned value carries that visible change to the caller, so the frame
/// follows the dispatch and not the next key.
///
/// A running session that refuses a request holds a copy of every document that
/// it opened, so the session opens the refused document again. The pass then
/// ends, because that session refuses every further request of the same pass.
/// See `docs/language-services.md`.
fn submit_language_work(
    editor: &mut Session,
    mut language: Option<&mut LanguageServices>,
) -> Redraw {
    let mut redraw = Redraw::Skipped;
    for _ in 0..LANGUAGE_DISPATCH_MAX {
        let Some(request) = editor.take_language_request() else {
            return redraw;
        };
        let result = match language.as_deref_mut() {
            // One language can run several servers, so the request reaches
            // every running session of its path.
            Some(language) => language
                .sessions(request.path())
                .and_then(|handles| send_request(&handles, &request)),
            // A workspace root that the services refused leaves the editor
            // usable with no language service at all.
            None => Err(LspError::NoServerDeclared),
        };
        let refusal = result.as_ref().err().map(Refusal::of);
        redraw = redraw.or(editor.apply_language_dispatch(&request, result));
        if refusal == Some(Refusal::CopyDrifted) {
            // The queue of that session stays full until the session drains it,
            // so every further request of this pass meets the same refusal. The
            // editor marked the refused document for a fresh open, and the next
            // pass sends that open.
            return redraw;
        }
    }
    debug_assert!(
        editor.take_language_request().is_none(),
        "one transition produces fewer requests than the dispatch bound"
    );
    redraw
}

/// Hands every queued request of one iteration to the service that runs it.
///
/// A pass can queue the work of another owner, so the dispatch repeats inside
/// [`DISPATCH_PASSES_MAX`]. The returned value reports every visible change that
/// a refused submission produced, because a refusal names its state on the
/// message line and the frame must follow that report.
fn dispatch(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
    language: &mut Option<LanguageServices>,
) -> Redraw {
    let mut redraw = Redraw::Skipped;
    for _ in 0..DISPATCH_PASSES_MAX {
        redraw = redraw.or(submit_background_work(editor, spawner, gate));
        redraw = redraw.or(submit_language_work(editor, language.as_mut()));
    }
    redraw
}

/// Hands the queued file and analysis jobs to the bounded worker service.
///
/// A refused submission reaches the session as a typed failure, so the editor
/// keeps its previous visible state and reports the refusal.
fn submit_background_work(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
) -> Redraw {
    submit_file_work(editor, spawner, gate)
        .or(submit_analysis_work(editor, spawner, gate))
        .or(submit_workspace_work(editor, spawner, gate))
        .or(submit_picker_work(editor, spawner, gate))
        .or(submit_completion_work(editor, spawner, gate))
        .or(submit_clipboard_work(editor, spawner, gate))
        .or(submit_git_work(editor, spawner, gate))
        .or(submit_format_work(editor, spawner, gate))
        .or(submit_host_work(editor, spawner, gate))
}

/// Hands the queued formatter run to the bounded process service.
///
/// The program reads the buffer and writes the formatted document, so it never
/// runs on this loop. A refused submission returns to the session as a typed
/// failure, which saves the unformatted content. See
/// `docs/language-services.md`.
fn submit_format_work(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_format_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(FORMAT_SLOT, &spawner.cancellation_root());
    let command = request.command();
    let submitted = spawner.submit_process(handle, command, move |output| {
        EditorWork(WorkResult::Format(request.publish(&output)))
    });
    if submitted.is_err() {
        return editor.apply_format_result(Err(FormatterFailure::Unavailable));
    }
    Redraw::Skipped
}

/// Hands the queued Git status read to the bounded process service.
///
/// The command reads the repository, so it never runs on this loop. A refused
/// submission returns to the session as a typed failure, which keeps the marks
/// of the last successful read. See `docs/git.md`.
fn submit_git_work(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_git_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(GIT_SLOT, &spawner.cancellation_root());
    let command = request.command();
    let submitted = spawner.submit_process(handle, command, move |output| {
        EditorWork(WorkResult::Git(request.publish(&output)))
    });
    if submitted.is_err() {
        return editor.apply_git_result(Err(GitStatusFailure::Unavailable));
    }
    Redraw::Skipped
}

/// Returns the typed Git failure of one runtime failure.
///
/// A command that cannot start is a normal state: the editor names it once and
/// stays usable without the repository state.
const fn git_failure(error: &RuntimeError) -> GitStatusFailure {
    match error {
        RuntimeError::ProcessSpawn(_) => GitStatusFailure::CommandMissing,
        RuntimeError::Cancelled
        | RuntimeError::Timeout
        | RuntimeError::WorkerFailure(_)
        | RuntimeError::ProcessRead(_)
        | RuntimeError::ProcessWrite(_)
        | RuntimeError::OutputLimit { .. } => GitStatusFailure::Unavailable,
    }
}

/// Hands the queued clipboard command to the bounded process service.
///
/// The command reaches the system clipboard, so it never runs on this loop. A
/// refused submission returns to the session as a typed failure, which keeps
/// the unnamed register and lets a deferred paste fall back to it. See
/// `docs/clipboard.md`.
fn submit_clipboard_work(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(command) = editor.take_clipboard_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(CLIPBOARD_SLOT, &spawner.cancellation_root());
    let submitted = spawner.submit_process(handle, command, |output| {
        EditorWork(WorkResult::Clipboard(output))
    });
    if let Err(error) = submitted {
        return editor.apply_clipboard_result(Err(refused_submission(error)));
    }
    Redraw::Skipped
}

/// Hands the queued picker work to the bounded worker and process services.
///
/// A workspace walk and a preview read are worker jobs. A ripgrep search is an
/// external command, so it reaches the process service instead. Both slots
/// cancel the request that a newer one replaces.
fn submit_picker_work(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
) -> Redraw {
    let mut redraw = Redraw::Skipped;
    for _ in 0..PICKER_DISPATCH_MAX {
        let Some(request) = editor.take_picker_request() else {
            return redraw;
        };
        let slot = request.slot();
        let deadline = request.deadline();
        let handle = gate.begin(publication_slot(slot), &spawner.cancellation_root());
        let submitted = match request.command() {
            Some(command) => spawner.submit_process(handle, command, move |output| {
                EditorWork(WorkResult::Picker(request.publish(&output)))
            }),
            None => spawner.submit_worker(handle, deadline, move |cancellation| {
                EditorWork(WorkResult::Picker(request.run(&cancellation)))
            }),
        };
        if let Err(error) = submitted {
            redraw = redraw.or(editor.abandon_picker_request(
                slot,
                match error {
                    SubmitError::Saturated(_) => PickerFailure::Saturated,
                    SubmitError::InvalidLimits
                    | SubmitError::ProcessBounds
                    | SubmitError::ShuttingDown => PickerFailure::Cancelled,
                },
            ));
        }
    }
    debug_assert!(
        editor.take_picker_request().is_none(),
        "one transition produces fewer picker requests than the dispatch bound"
    );
    redraw
}

/// Hands the workspace walk of the command-line completion to the worker.
///
/// The walk is the same job that the file picker submits, so one walk serves
/// both. The command line reads no directory and waits for no result, so a
/// refused submission leaves it without a path list and reports nothing. See
/// `docs/files.md`.
fn submit_completion_work(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_completion_request() else {
        return Redraw::Skipped;
    };
    let deadline = request.deadline();
    let handle = gate.begin(COMPLETION_SLOT, &spawner.cancellation_root());
    // A refusal leaves the completion in the state that it already holds, so
    // the editor has nothing to clear and nothing to report.
    let _refused = spawner.submit_worker(handle, deadline, move |cancellation| {
        EditorWork(WorkResult::Completion(request.run(&cancellation)))
    });
    Redraw::Skipped
}

/// Hands the queued host probe to the bounded worker service.
///
/// The probe reads the executable search path once for each declared program,
/// so it never runs on this loop. A refused submission reaches the session as a
/// typed failure, because the user asked for the report and must learn that it
/// failed. See `docs/architecture.md`.
fn submit_host_work(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_host_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(DIAGNOSTICS_SLOT, &spawner.cancellation_root());
    let submitted = spawner.submit_worker(handle, WORKER_DEADLINE_DEFAULT, move |_cancellation| {
        EditorWork(WorkResult::HostReport(request.run()))
    });
    if let Err(error) = submitted {
        return editor.abandon_host_request(match error {
            SubmitError::Saturated(_) => HostProbeFailure::Saturated,
            SubmitError::InvalidLimits | SubmitError::ProcessBounds | SubmitError::ShuttingDown => {
                HostProbeFailure::Cancelled
            }
        });
    }
    Redraw::Skipped
}

/// Returns the typed host-probe failure of one runtime failure.
const fn host_failure(error: &RuntimeError) -> HostProbeFailure {
    match error {
        RuntimeError::Timeout => HostProbeFailure::Timeout,
        RuntimeError::Cancelled
        | RuntimeError::WorkerFailure(_)
        | RuntimeError::ProcessSpawn(_)
        | RuntimeError::ProcessRead(_)
        | RuntimeError::ProcessWrite(_)
        | RuntimeError::OutputLimit { .. } => HostProbeFailure::Cancelled,
    }
}

/// Returns the publication slot of one picker operation.
const fn publication_slot(slot: PickerSlot) -> RequestSlot {
    match slot {
        PickerSlot::Candidates => PICKER_SLOT,
        PickerSlot::Preview => PREVIEW_SLOT,
    }
}

/// Returns the typed picker failure of one runtime failure.
///
/// A command that cannot start is a normal state: the editor reports it and
/// stays usable without the search picker.
const fn picker_failure(error: &RuntimeError) -> PickerFailure {
    match error {
        RuntimeError::Timeout => PickerFailure::Timeout,
        RuntimeError::ProcessSpawn(_) => PickerFailure::CommandMissing,
        RuntimeError::Cancelled
        | RuntimeError::WorkerFailure(_)
        | RuntimeError::ProcessRead(_)
        | RuntimeError::ProcessWrite(_)
        | RuntimeError::OutputLimit { .. } => PickerFailure::Cancelled,
    }
}

/// Hands the queued directory read or mutation to the bounded worker service.
///
/// A refused submission reaches the session as a typed failure, so the file
/// tree keeps the state that it held before the request.
fn submit_workspace_work(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
) -> Redraw {
    let mut redraw = Redraw::Skipped;
    for _ in 0..WORKSPACE_DISPATCH_MAX {
        let Some(request) = editor.take_workspace_request() else {
            return redraw;
        };
        let handle = gate.begin(WORKSPACE_SLOT, &spawner.cancellation_root());
        // A mutation owns a reserved outbox slot, so its job masks cancellation
        // and always reports whether the workspace changed. A directory read
        // changes nothing durable, so a newer read may cancel it. See
        // `docs/embedding.md`.
        let commits = request.commits();
        let job =
            |_cancellation: CancellationToken| EditorWork(WorkResult::Workspace(request.run()));
        let submitted = if commits {
            spawner.submit_committing_worker(handle, WORKER_DEADLINE_DEFAULT, job)
        } else {
            spawner.submit_worker(handle, WORKER_DEADLINE_DEFAULT, job)
        };
        // A refused submission clears the pending state of the tree, so the
        // next transition offers the read again instead of waiting for a result
        // that never arrives.
        if let Err(error) = submitted {
            redraw = redraw.or(editor.abandon_workspace_request(match error {
                SubmitError::Saturated(_) => FileRequestFailure::Saturated,
                SubmitError::InvalidLimits
                | SubmitError::ProcessBounds
                | SubmitError::ShuttingDown => FileRequestFailure::Cancelled,
            }));
        }
    }
    redraw
}

/// Hands the queued file request to the bounded worker service.
fn submit_file_work(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_file_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(FILE_SLOT, &spawner.cancellation_root());
    // A save owns a reserved outbox slot, so its job masks cancellation and
    // always reports whether the write reached the filesystem. An open and a
    // reload change nothing durable, so a newer request may cancel them. See
    // `docs/embedding.md`.
    let commits = request.commits();
    let job = |_cancellation: CancellationToken| EditorWork(WorkResult::File(request.run()));
    let submitted = if commits {
        spawner.submit_committing_worker(handle, WORKER_DEADLINE_DEFAULT, job)
    } else {
        spawner.submit_worker(handle, WORKER_DEADLINE_DEFAULT, job)
    };
    if let Err(error) = submitted {
        return editor.abandon_file_request(match error {
            SubmitError::Saturated(_) => FileRequestFailure::Saturated,
            SubmitError::InvalidLimits | SubmitError::ProcessBounds | SubmitError::ShuttingDown => {
                FileRequestFailure::Cancelled
            }
        });
    }
    Redraw::Skipped
}

/// Hands the analysis of the active buffer to the bounded worker service.
///
/// Highlighting is decoration, so a refused submission only frees the request
/// again and paints nothing. The next transition asks for it once more.
fn submit_analysis_work(
    editor: &mut Session,
    spawner: &Runtime<EditorWork>,
    gate: &PublicationGate,
) -> Redraw {
    let Some(request) = editor.take_analysis_request() else {
        return Redraw::Skipped;
    };
    let handle = gate.begin(ANALYSIS_SLOT, &spawner.cancellation_root());
    let submitted = spawner.submit_worker(handle, ANALYSIS_DEADLINE, move |cancellation| {
        EditorWork(WorkResult::Analysis(request.run(&cancellation)))
    });
    if submitted.is_err() {
        // The refusal paints nothing, so the log is the one place that holds
        // it. See `docs/responsiveness.md`.
        editor.record_job(JOB_ANALYSIS, MessageLevel::Warning, JOB_REFUSED);
        editor.abandon_analysis_request();
    }
    Redraw::Skipped
}

/// Returns the log outcome of one runtime failure.
///
/// Every outcome is one fixed text, so a job that fails the same way twice
/// collapses into one log entry. See `docs/windows.md`.
const fn job_outcome(error: &RuntimeError) -> &'static str {
    match error {
        RuntimeError::Timeout => "passed its deadline",
        RuntimeError::Cancelled => "was cancelled",
        RuntimeError::WorkerFailure(_) => "failed inside its worker",
        RuntimeError::ProcessSpawn(_) => "did not start",
        RuntimeError::ProcessRead(_) | RuntimeError::ProcessWrite(_) => "lost its pipe",
        RuntimeError::OutputLimit { .. } => "wrote more than its output limit",
    }
}

/// Returns the log severity of one runtime failure.
///
/// A newer request in the same slot cancels the older one, so a cancelled job
/// is a normal state. Every other failure needs attention.
const fn job_level(error: &RuntimeError) -> MessageLevel {
    match error {
        RuntimeError::Cancelled => MessageLevel::Info,
        RuntimeError::Timeout
        | RuntimeError::WorkerFailure(_)
        | RuntimeError::ProcessSpawn(_)
        | RuntimeError::ProcessRead(_)
        | RuntimeError::ProcessWrite(_)
        | RuntimeError::OutputLimit { .. } => MessageLevel::Warning,
    }
}

/// Applies one result of the bounded worker service.

fn complete(
    editor: &mut Session,
    gate: &PublicationGate,
    event: Option<RuntimeEvent<EditorWork>>,
) -> Redraw {
    let Some(event) = event else {
        // The spawner is gone, so no further result can arrive.
        return Redraw::Skipped;
    };
    if !gate.accepts(&event.request) {
        // A newer request owns the slot, so this result is obsolete. The log
        // records the analysis slot alone, because the log collapses one
        // repeated report and two obsolete kinds that alternate would collapse
        // into nothing. See `docs/responsiveness.md`.
        if event.request.slot() == ANALYSIS_SLOT {
            editor.record_job(JOB_ANALYSIS, MessageLevel::Info, JOB_OBSOLETE);
        }
        return Redraw::Skipped;
    }
    let analysis = event.request.slot() == ANALYSIS_SLOT;
    let workspace = event.request.slot() == WORKSPACE_SLOT;
    let clipboard = event.request.slot() == CLIPBOARD_SLOT;
    let git = event.request.slot() == GIT_SLOT;
    let format = event.request.slot() == FORMAT_SLOT;
    let completion = event.request.slot() == COMPLETION_SLOT;
    let host = event.request.slot() == DIAGNOSTICS_SLOT;
    let picker = if event.request.slot() == PICKER_SLOT {
        Some(PickerSlot::Candidates)
    } else if event.request.slot() == PREVIEW_SLOT {
        Some(PickerSlot::Preview)
    } else {
        None
    };
    let failure = |error: &RuntimeError| match error {
        RuntimeError::Timeout => FileRequestFailure::Timeout,
        // A cancelled request and a failed worker both leave the buffer
        // unchanged, so the editor stays usable and the user can try again.
        RuntimeError::Cancelled
        | RuntimeError::WorkerFailure(_)
        | RuntimeError::ProcessSpawn(_)
        | RuntimeError::ProcessRead(_)
        | RuntimeError::ProcessWrite(_)
        | RuntimeError::OutputLimit { .. } => FileRequestFailure::Cancelled,
    };
    match (picker, event.result) {
        (_, Ok(EditorWork(WorkResult::File(result)))) => editor.apply_file_result(result),
        (_, Ok(EditorWork(WorkResult::Analysis(result)))) => editor.apply_analysis_result(result),
        (_, Ok(EditorWork(WorkResult::Workspace(result)))) => editor.apply_workspace_result(result),
        (_, Ok(EditorWork(WorkResult::Picker(result)))) => editor.apply_picker_result(result),
        (_, Ok(EditorWork(WorkResult::Completion(result)))) => {
            editor.apply_completion_result(result)
        }
        (_, Ok(EditorWork(WorkResult::Clipboard(output)))) => {
            editor.apply_clipboard_result(Ok(output))
        }
        (_, Ok(EditorWork(WorkResult::Git(result)))) => editor.apply_git_result(result),
        (_, Ok(EditorWork(WorkResult::Format(result)))) => editor.apply_format_result(result),
        (_, Ok(EditorWork(WorkResult::HostReport(report)))) => editor.apply_host_report(&report),
        (Some(slot), Err(error)) => editor.abandon_picker_request(slot, picker_failure(&error)),
        // A clipboard command that fails, times out, or is cancelled keeps the
        // unnamed register, so the yank or the paste still holds its value.
        (None, Err(error)) if clipboard => {
            editor.apply_clipboard_result(Err(command_failure(&error)))
        }
        // An analysis that fails, times out, or is cancelled renders plain text
        // and reports nothing, because highlighting is decoration. The log
        // names the outcome instead.
        (None, Err(error)) if analysis => {
            editor.record_job(JOB_ANALYSIS, job_level(&error), job_outcome(&error));
            editor.abandon_analysis_request();
            Redraw::Skipped
        }
        // A status read that fails, times out, or is cancelled keeps the marks
        // of the last successful read, because they are decoration.
        (None, Err(error)) if git => editor.apply_git_result(Err(git_failure(&error))),
        // A formatter that fails, times out, or is cancelled leaves the buffer
        // as the user typed it, and the save that waited for it still runs.
        (None, Err(error)) if format => {
            editor.apply_format_result(Err(FormatterFailure::of(&error)))
        }
        // The user asked for the host report, so a probe that fails, times out,
        // or is cancelled opens no buffer and reports the outcome.
        (None, Err(error)) if host => editor.abandon_host_request(host_failure(&error)),
        // A walk that fails, times out, or is cancelled leaves the command line
        // without a path list. The user still types the path in full, so the
        // editor keeps nothing to clear and reports nothing. The log names the
        // outcome instead.
        (None, Err(error)) if completion => {
            editor.record_job(JOB_WALK, job_level(&error), job_outcome(&error));
            Redraw::Skipped
        }
        (None, Err(error)) if workspace => editor.abandon_workspace_request(failure(&error)),
        (None, Err(error)) => editor.abandon_file_request(failure(&error)),
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Not;
    use std::path::{Path, PathBuf};

    use ratatui::layout::Rect;
    use tokio::time::timeout;

    use kvim_runtime::{EVENT_QUEUE_CAPACITY, RuntimeLimits};
    use kvim_settings::EditorSettings;
    use kvim_terminal::{Key, KeyCode, TerminalEvent};
    use kvim_workspace::temp::TempDir;

    use super::*;
    use crate::embed::EditorEvent;
    use crate::session::test_root;
    use crate::tree::GENERATED_NAMES;

    /// The publication slot of the parked job of one shutdown test.
    const PARKED_SLOT: RequestSlot = RequestSlot::new(99);

    /// The deadline of the parked job of one shutdown test.
    ///
    /// The test releases the job, so the deadline only bounds a failed run.
    const PARKED_DEADLINE: Duration = Duration::from_secs(30);

    /// The text that the fixture file holds before one save.
    const ORIGINAL: &str = "one\n";

    /// The elapsed time that every transition of these tests reports.
    const NOW: Duration = Duration::ZERO;

    /// The time that one test waits for the refused registration.
    const REGISTRATION_WAIT: Duration = Duration::from_secs(5);

    /// The time that one test waits for a future that must never complete.
    const PARKED_WAIT: Duration = Duration::from_millis(50);

    /// The report of a workspace that no watcher observes.
    const WATCH_MISSING_NOTE: &str =
        "the workspace watcher could not start; the file tree updates on a refresh";

    #[tokio::test]
    async fn one_dispatch_hands_a_refused_format_and_the_save_behind_it_to_their_services() {
        let directory = TempDir::new("driver-dispatch-save");
        let path = directory.write("main.rs", "one\n");
        let mut settings = EditorSettings::default();
        settings.files.undo_file = false;
        let root = path
            .parent()
            .expect("the temporary file holds a parent directory")
            .to_path_buf();
        let mut editor = Session::new(Rect::new(0, 0, 80, 24), settings, test_root(root));
        let _ = editor.open_path(path);
        let request = editor
            .take_file_request()
            .expect("the open queued one file request");
        let _ = editor.apply_file_result(request.run());
        // One typed character leaves the buffer with an unsaved change.
        let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('i'))), NOW);
        let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('a'))), NOW);
        let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Esc)), NOW);
        assert!(editor.buffer().is_modified());

        let (runtime, _results) = Runtime::<EditorWork>::new();
        let gate = PublicationGate::default();
        // The editor runs without language services, so the formatter request of
        // the save reaches no server.
        let mut language = None;
        let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
        let redraw = dispatch(&mut editor, &runtime, &gate, &mut language);

        assert_eq!(
            redraw,
            Redraw::Needed,
            "the refused formatter request names its state on the message line"
        );
        assert!(
            editor.take_file_request().is_none(),
            "the dispatch hands the save to the worker service inside one iteration, \
             so the write never waits for the next terminal event"
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn one_dispatch_runs_the_external_formatter_and_the_save_behind_it() {
        let directory = TempDir::new("driver-dispatch-format");
        // The Nix adapter declares an external formatter, so the save reaches
        // the bounded process service instead of a language server.
        let path = directory.write("flake.nix", "{  }\n");
        let mut settings = EditorSettings::default();
        settings.files.undo_file = false;
        let root = path
            .parent()
            .expect("the temporary file holds a parent directory")
            .to_path_buf();
        let mut editor = Session::new(Rect::new(0, 0, 80, 24), settings, test_root(root));
        let _ = editor.open_path(path);
        let request = editor
            .take_file_request()
            .expect("the open queued one file request");
        let _ = editor.apply_file_result(request.run());
        // One typed character leaves the buffer with an unsaved change, so the
        // save behind the formatter writes the file.
        let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('i'))), NOW);
        let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char(' '))), NOW);
        let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Esc)), NOW);

        let (runtime, mut results) = Runtime::<EditorWork>::new();
        let gate = PublicationGate::default();
        let mut language = None;
        let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
        let _ = dispatch(&mut editor, &runtime, &gate, &mut language);

        assert!(
            editor.take_file_request().is_none(),
            "the save waits for the formatter answer"
        );
        // The same dispatch also started the directory read of the file tree,
        // so the loop applies every result until the formatter answers.
        let mut answered = false;
        for _ in 0..DISPATCH_PASSES_MAX + PICKER_DISPATCH_MAX {
            let event = results
                .recv()
                .await
                .expect("every accepted request produces one result");
            answered |= event.request.slot() == FORMAT_SLOT;
            let _ = complete(&mut editor, &gate, Some(event));
            if answered {
                break;
            }
        }
        assert!(
            answered,
            "the dispatch handed the run to the process service"
        );

        // A host without the program answers a typed failure, and a host with
        // it answers a document. The save follows either answer.
        assert!(
            editor.take_file_request().is_some(),
            "the formatter answer completes the save that waited for it"
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn a_registration_that_fails_reports_that_no_watcher_runs() {
        // The start places no watch, so it accepts the root that the deferred
        // registration then refuses.
        let directory = TempDir::new("driver-missing-watch-root");
        let root = test_root(directory.path.clone());
        std::fs::remove_dir_all(&directory.path)
            .expect("the fixture root exists before watcher registration");
        let mut watcher = FileWatcher::start(root, &GENERATED_NAMES).ok();
        assert!(
            watcher.is_some(),
            "the start defers every platform call, so it refuses no root"
        );

        let batch = timeout(REGISTRATION_WAIT, next_watch_batch(&mut watcher))
            .await
            .expect("the refused registration ends the published stream");

        assert!(batch.is_none(), "the ended stream publishes no burst");
        assert!(
            watcher.is_none(),
            "the loop drops the ended watch instead of reading it again"
        );
        assert!(
            timeout(PARKED_WAIT, next_watch_batch(&mut watcher))
                .await
                .is_err(),
            "the loop then waits for its other events alone"
        );

        let mut editor = Session::new(
            Rect::new(0, 0, 80, 24),
            EditorSettings::default(),
            test_root(std::env::current_dir().expect("the test process holds a working directory")),
        );
        let redraw = publish_watch(&mut editor, batch.as_ref());

        assert_eq!(
            redraw,
            Redraw::Needed,
            "the report changes the message line, so one frame follows it"
        );
        assert_eq!(
            editor
                .message()
                .map_or_else(String::new, |message| message.text().to_owned()),
            WATCH_MISSING_NOTE,
        );
    }

    /// The steps that one test runs before it gives up on a result.
    ///
    /// One open queues the read, the directory listing, the Git status, and the
    /// analysis, so the bound covers every result of that chain.
    const DRIVER_STEPS_MAX: usize = 16;

    /// The time that one test waits for one result of the spawner.
    const STEP_WAIT: Duration = Duration::from_secs(10);

    /// The shutdown deadline that every ordinary test gives the driver.
    const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);

    /// The reads that one test performs before it gives up on a write.
    const COMMIT_POLLS_MAX: usize = 1000;

    /// The time between two reads of one running save.
    const COMMIT_POLL: Duration = Duration::from_millis(10);

    /// The modules that the structural check reads.
    fn source_directory() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// Creates one editor over one temporary workspace.
    fn editor_at(root: &Path) -> Session {
        let mut settings = EditorSettings::default();
        settings.files.undo_file = false;
        Session::new(
            Rect::new(0, 0, 80, 24),
            settings,
            test_root(root.to_path_buf()),
        )
    }

    /// Runs one host step: one dispatch, one wait, and one transition.
    async fn step(driver: &mut EditorDriver, editor: &mut Session) {
        let _ = driver.dispatch(editor);
        let completed = timeout(STEP_WAIT, driver.recv())
            .await
            .expect("every accepted request produces one result");
        let _ = driver.apply(editor, completed, NOW);
    }

    /// Loads one file through the driver, like the host loop does.
    async fn open_through(driver: &mut EditorDriver, editor: &mut Session, path: PathBuf) {
        editor.open_path(path);
        for _ in 0..DRIVER_STEPS_MAX {
            step(driver, editor).await;
            if editor.active_buffer().path().is_some() {
                return;
            }
        }
        panic!("one open queues fewer results than the step bound");
    }

    /// Leaves the active buffer with one unsaved change.
    fn type_one_character(editor: &mut Session) {
        let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('i'))), NOW);
        let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Char('a'))), NOW);
        let _ = editor.handle_event(TerminalEvent::Key(Key::plain(KeyCode::Esc)), NOW);
        assert!(editor.buffer().is_modified());
    }

    /// Waits until the running save reached the file.
    ///
    /// A masked job starts its blocking work before it writes, so a changed file
    /// proves that the job entered its commit. No cancellation can drop its
    /// result after that point, which is the seam that the two shutdown tests
    /// below need. See `docs/embedding.md`.
    async fn wait_for_commit(path: &Path, before: &str) {
        for _ in 0..COMMIT_POLLS_MAX {
            if std::fs::read_to_string(path).is_ok_and(|text| text != before) {
                return;
            }
            tokio::time::sleep(COMMIT_POLL).await;
        }
        panic!("one accepted save reaches its file inside the poll bound");
    }

    /// Reports whether the editor published the fact of one completed write.
    fn published_a_write(editor: &mut Session) -> bool {
        let mut written = false;
        while let Some(published) = editor.take_event() {
            written |= matches!(published.event, EditorEvent::FileWritten { .. });
        }
        written
    }

    #[tokio::test]
    async fn the_driver_submits_every_syntax_request_through_the_caller_spawner() {
        let directory = TempDir::new("driver-analysis");
        let path = directory.write("main.rs", "fn main() {}\n");
        let mut editor = editor_at(&directory.path);
        let (spawner, results) = Runtime::<EditorWork>::new();
        let mut driver = EditorDriver::new(editor.instance(), spawner, results);

        editor.open_path(path);
        let mut analyses = 0;
        for _ in 0..DRIVER_STEPS_MAX {
            let _ = driver.dispatch(&mut editor);
            assert!(
                editor.take_analysis_request().is_none(),
                "the dispatch hands every syntax request to the spawner, so no \
                 request stays behind for this loop to run"
            );
            let completed = timeout(STEP_WAIT, driver.recv())
                .await
                .expect("every accepted request produces one result");
            if let Outcome::Work(event) = &completed.outcome
                && let Some(event) = event.as_ref()
                && event.request.slot() == ANALYSIS_SLOT
            {
                analyses += 1;
            }
            let _ = driver.apply(&mut editor, completed, NOW);
            if analyses > 0 {
                break;
            }
        }

        assert_eq!(
            analyses, 1,
            "the parse and the highlight query ran on the worker service"
        );
        let drain = driver.shutdown(&mut editor, SHUTDOWN_WAIT).await;
        assert!(drain.is_none(), "every task of this editor finished");
    }

    #[tokio::test]
    async fn a_shutdown_waits_for_the_committed_write_and_publishes_its_fact() {
        let directory = TempDir::new("driver-shutdown-write");
        let path = directory.write("main.rs", ORIGINAL);
        let saved = path.clone();
        let mut editor = editor_at(&directory.path);
        let (spawner, results) = Runtime::<EditorWork>::new();
        let mut driver = EditorDriver::new(editor.instance(), spawner, results);
        open_through(&mut driver, &mut editor, path).await;
        type_one_character(&mut editor);

        let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
        // The one dispatch refuses the formatter and hands the save behind it to
        // the worker service inside the same iteration.
        let _ = driver.dispatch(&mut editor);
        wait_for_commit(&saved, ORIGINAL).await;
        assert!(published_a_write(&mut editor).not(), "the write still runs");

        // The shutdown cancels every pre-commit request, and the write masks
        // that cancellation, so its reserved slot still publishes its fact.
        let drain = driver.shutdown(&mut editor, SHUTDOWN_WAIT).await;

        assert!(drain.is_none(), "the write finished inside the deadline");
        assert!(
            published_a_write(&mut editor),
            "a completed write always publishes its mandatory fact"
        );
    }

    #[tokio::test]
    async fn an_expired_deadline_returns_the_drain_that_publishes_the_write() {
        let directory = TempDir::new("driver-shutdown-drain");
        let path = directory.write("main.rs", ORIGINAL);
        let saved = path.clone();
        let mut editor = editor_at(&directory.path);
        let limits = RuntimeLimits::new(EVENT_QUEUE_CAPACITY, 4, 4)
            .expect("every capacity of this fixture is above zero");
        let (spawner, results) = Runtime::<EditorWork>::with_limits(limits);
        // One parked job holds the tracked work past the deadline, so the
        // shutdown below reaches its expiry with certainty.
        let (release, parked) = std::sync::mpsc::channel::<()>();
        let gate = PublicationGate::default();
        let handle = gate.begin(PARKED_SLOT, &spawner.cancellation_root());
        spawner
            .submit_worker(handle, PARKED_DEADLINE, move |_cancellation| {
                let _released = parked.recv();
                EditorWork(WorkResult::HostReport(String::new()))
            })
            .expect("the fresh spawner holds worker capacity");
        let mut driver = EditorDriver::new(editor.instance(), spawner, results);
        open_through(&mut driver, &mut editor, path).await;
        type_one_character(&mut editor);

        let _ = editor.handle_event(TerminalEvent::Key(Key::ctrl(KeyCode::Char('s'))), NOW);
        let _ = driver.dispatch(&mut editor);
        wait_for_commit(&saved, ORIGINAL).await;
        let drain = driver
            .shutdown(&mut editor, Duration::ZERO)
            .await
            .expect("the parked job holds the tracked work past the deadline");

        assert!(
            published_a_write(&mut editor).not(),
            "the drain still owns the fact of the write"
        );
        release
            .send(())
            .expect("the parked job still waits for its release");
        let _redraw = drain.complete(&mut editor).await;

        assert!(
            published_a_write(&mut editor),
            "the resumed drain publishes the mandatory fact before the host \
             stops its runtime"
        );
    }

    #[test]
    fn no_module_beside_the_driver_hands_work_to_a_spawner() {
        // The driver owns every submission, so no other module can start
        // filesystem, process, Git, formatter, or Tree-sitter work. See
        // `docs/responsiveness.md`.
        const SUBMISSIONS: [&str; 3] = [
            "submit_worker(",
            "submit_committing_worker(",
            "submit_process(",
        ];
        let mut checked = 0;
        for entry in
            std::fs::read_dir(source_directory()).expect("the crate holds its own source directory")
        {
            let path = entry
                .expect("the source directory lists its entries")
                .path();
            if path.extension().is_none_or(|extension| extension != "rs")
                || path.file_name().is_some_and(|name| name == "driver.rs")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("every module is readable text");
            for call in SUBMISSIONS {
                assert!(
                    !source.contains(call),
                    "{} calls {call}, but the driver owns every submission",
                    path.display()
                );
            }
            checked += 1;
        }
        assert!(checked > 1, "the check read the modules of this crate");
    }

    #[test]
    fn no_module_of_this_crate_owns_the_terminal() {
        // The `kvim` binary is the only terminal owner. This crate holds the
        // visible state and the presentation of one editor, so it never enters
        // raw mode, never enters the alternate screen, never reads the terminal
        // event stream, never installs a signal handler or a panic hook, and
        // never writes to standard output. See `docs/architecture.md` and
        // `docs/embedding.md`.
        const TERMINAL_OWNERS: [&str; 8] = [
            "TerminalSession",
            "CrosstermControl",
            "TerminationSource",
            "EventSource",
            "CrosstermBackend",
            "enable_raw_mode",
            "EnterAlternateScreen",
            "set_hook",
        ];
        let mut checked = 0;
        for entry in
            std::fs::read_dir(source_directory()).expect("the crate holds its own source directory")
        {
            let path = entry
                .expect("the source directory lists its entries")
                .path();
            if path.extension().is_none_or(|extension| extension != "rs")
                || path.file_name().is_some_and(|name| name == "driver.rs")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("every module is readable text");
            for owner in TERMINAL_OWNERS {
                assert!(
                    !source.contains(owner),
                    "{} names {owner}, but the binary owns the terminal",
                    path.display()
                );
            }
            checked += 1;
        }
        assert!(checked > 1, "the check read the modules of this crate");
    }

    #[test]
    fn the_session_builds_the_analysis_request_and_never_runs_it() {
        // `session.rs` owns the inert `AnalysisRequest` value and calls the
        // adapter once, inside `AnalysisRequest::run`. The worker service is the
        // only caller of that method, and the check above proves that the driver
        // owns every submission. The terminal event loop of the `kvim` binary
        // therefore names no syntax value at all, and its own guard proves that.
        let owner = std::fs::read_to_string(source_directory().join("session.rs"))
            .expect("the session module is readable text");
        assert_eq!(
            owner.matches(".analyze(").count(),
            1,
            "one call of the adapter exists, and `AnalysisRequest::run` owns it"
        );
        assert_eq!(
            owner.matches("AnalysisRequest::run").count(),
            0,
            "the session builds the request and never runs it"
        );
    }
}
