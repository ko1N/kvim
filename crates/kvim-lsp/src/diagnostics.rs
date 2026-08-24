//! Changed-file diagnostics of one warm project session.
//!
//! The caller supplies one validated path, the exact text of one document
//! revision, one language, its result limits, one wait policy, and one
//! cancellation owner. The operation reads no Git state, starts no build, and
//! opens no file. It dispatches to every configured server that declares that
//! language, in declaration order.
//!
//! [`DiagnosticsHub`] owns the request side. [`DiagnosticsHub::server`] creates
//! one [`DiagnosticsConversation`] for each declared server, and the caller
//! hands that value to `ProjectDeclaration::server`. The hub therefore reuses
//! one warm project session for every later request.
//!
//! [`WaitPolicy::Until`] keeps one request alive through process startup and
//! diagnostic completion. The request needs no watcher, no polling, and no
//! resubmission, because a conversation reads the active request as soon as its
//! server answers the handshake. [`WaitPolicy::Immediate`] returns
//! [`DiagnosticsOutcome::Starting`] instead of waiting.
//!
//! Each server owns one result slot and reaches exactly one terminal outcome.
//! A missing program, an unsupported completion policy, a failure, and a
//! cancelled session are ordinary outcomes, so one refused server stays visible
//! beside the answers of every other server.
//!
//! See `docs/language-services.md` and
//! `crates/kvim-lsp/examples/lsp_diagnostics.rs`.

use std::cmp::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;

use kvim_path::WorktreeRelativePath;

use crate::document::{Diagnostic, RawDiagnostic};
use crate::encoding::{DocumentMapping, TextMirroring};
use crate::process::{DiagnosticsModel, LSP_RESULT_ID_BYTES_MAX};
use crate::project::{
    Attempt, AttemptEnd, LSP_SESSIONS_MAX, ServerConversation, ServerEvent, ServerId,
};
use crate::protocol::{
    ArrayBudget, LspBound, LspError, ProtocolSpan, RpcEnvelope, RpcId, SourceSpan,
    deserialize_bounded_array, enforce,
};

/// The bytes of the exact text of one changed document.
///
/// The value matches the file bound of `docs/text-model.md`, so every buffer
/// that the editor holds also reaches a language server.
pub const LSP_DOCUMENT_BYTES_MAX: usize = 4 * 1024 * 1024;

/// The diagnostics that one server contributes for one document.
///
/// One file with more than a thousand diagnostics is already unreadable, and a
/// reader sees the diagnostics of the visible lines only. See
/// `docs/language-services.md`.
pub const LSP_DIAGNOSTICS_MAX: usize = 1_024;

/// The diagnostics that one merged changed-file report holds.
///
/// One adapter declares at most four servers for one language, so the merge of
/// one document holds at most four full server results.
pub const LSP_MERGED_DIAGNOSTICS_MAX: usize = 4 * LSP_DIAGNOSTICS_MAX;

/// The related information entries that one diagnostic keeps.
///
/// One diagnostic names few other places of the same document. A longer list
/// means a wrong or hostile answer.
pub const LSP_RELATED_INFORMATION_MAX: usize = 64;

/// The bytes that one diagnostic message keeps.
///
/// One message names one problem. A longer text cannot fit on a terminal row,
/// and it costs memory for every diagnostic of the merge.
pub const LSP_DIAGNOSTIC_MESSAGE_BYTES_MAX: usize = 8 * 1024;

/// The protocol bytes that one server spends on one changed-file request.
///
/// The bound counts the parameters and the results of every message that one
/// server sends while it answers one request. A server that never completes
/// therefore cannot allocate without limit before its deadline passes.
pub const LSP_REQUEST_BYTES_MAX: usize = 16 * 1024 * 1024;

/// The deadline of one changed-file request that names no other deadline.
///
/// A server analyses the complete document, and a cold linter loads its
/// configuration first, so it needs the time of a formatter. See
/// `docs/language-services.md`.
pub const LSP_DIAGNOSTIC_DEADLINE: Duration = Duration::from_secs(10);

/// The languages that one server declares.
///
/// One server serves one language family. Sixteen covers a server that reads
/// several dialects and still bounds the selection of one request.
pub const LSP_SERVER_LANGUAGES_MAX: usize = 16;

/// The bytes of one language identifier.
///
/// The protocol carries short identifiers such as `rust` or `typescriptreact`.
pub const LSP_LANGUAGE_BYTES_MAX: usize = 64;

/// The method that carries one pulled diagnostic report.
const PULL_METHOD: &str = "textDocument/diagnostic";

/// The method that carries one published diagnostic set.
const PUBLISH_METHOD: &str = "textDocument/publishDiagnostics";

/// The `kind` value of a report that repeats the previous set.
const UNCHANGED_REPORT: &str = "unchanged";

/// The revision of one document that the caller supplies.
///
/// The caller owns the value. It names the exact text of one request, and a
/// result of another revision never reaches that request.
///
/// # Examples
///
/// ```
/// use kvim_lsp::DocumentRevision;
///
/// let first = DocumentRevision::new(7);
/// assert!(first < first.next());
/// assert_eq!(first.get(), 7);
/// ```
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DocumentRevision(i32);

impl DocumentRevision {
    /// The revision of a document that no caller changed yet.
    pub const FIRST: Self = Self(0);

    /// Creates the revision that the caller names.
    #[must_use]
    pub const fn new(value: i32) -> Self {
        Self(value)
    }

    /// Returns the revision that follows this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Returns the underlying value for the wire and for comparisons.
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

/// One bounded language identifier of the protocol.
///
/// The value is caller data. This crate names no language, so a caller supplies
/// the identifier that its servers declare.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{LSP_LANGUAGE_BYTES_MAX, LanguageId, LspError};
///
/// let rust = LanguageId::new("rust")?;
/// assert_eq!(rust.as_str(), "rust");
/// assert!(LanguageId::new(&"x".repeat(LSP_LANGUAGE_BYTES_MAX + 1)).is_err());
/// assert!(LanguageId::new("").is_err());
/// # Ok::<(), LspError>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LanguageId(String);

impl LanguageId {
    /// Validates and owns one language identifier.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::Bounds`] for an empty identifier and for one above
    /// [`LSP_LANGUAGE_BYTES_MAX`].
    pub fn new(value: &str) -> Result<Self, LspError> {
        if value.is_empty() {
            return Err(LspError::Bounds {
                measure: LspBound::Languages,
                limit: LSP_LANGUAGE_BYTES_MAX,
                actual: 0,
            });
        }
        enforce(value.len(), LSP_LANGUAGE_BYTES_MAX, LspBound::Languages)?;
        Ok(Self(value.to_owned()))
    }

    /// Returns the identifier that the protocol carries.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How one server completes the diagnostics of one revision.
///
/// The caller declares the policy, because only the caller knows the server
/// product. kvim never guesses that versionless push diagnostics completed
/// after a quiet period. See `docs/language-services.md`.
///
/// # Examples
///
/// ```
/// use kvim_lsp::CompletionPolicy;
///
/// // A server that publishes without a document version has no safe
/// // completion, so its declaration names the unsupported policy.
/// let outcome = CompletionPolicy::Unsupported;
/// assert_ne!(outcome, CompletionPolicy::Pull);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionPolicy {
    /// Ask with `textDocument/diagnostic` and complete on the answer.
    ///
    /// The handshake must advertise a diagnostic provider. A server without
    /// that capability answers no pull, so the request reports it as
    /// unsupported.
    Pull,
    /// Complete on a `textDocument/publishDiagnostics` notification that names
    /// the exact requested revision.
    VersionedPush,
    /// The server names no safe completion, so every request reports it as
    /// unsupported.
    Unsupported,
}

/// How long one changed-file request waits.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use kvim_lsp::WaitPolicy;
///
/// let wait = WaitPolicy::Until(Duration::from_secs(5));
/// assert_ne!(wait, WaitPolicy::Immediate);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitPolicy {
    /// Return the state of the requested revision without waiting.
    ///
    /// The call can return [`DiagnosticsOutcome::Starting`]. It publishes no
    /// later result for that request.
    Immediate,
    /// Keep this request alive until its deadline passes.
    ///
    /// The wait covers process startup and diagnostic completion, so the
    /// request needs no watcher, no polling, and no resubmission.
    Until(Duration),
}

/// What one request does with an older revision that is still running.
///
/// # Examples
///
/// ```
/// use kvim_lsp::RevisionPolicy;
///
/// // The default keeps the running revision and waits behind it.
/// assert_eq!(RevisionPolicy::default(), RevisionPolicy::Queue);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RevisionPolicy {
    /// Wait until the running revision reaches its own outcome.
    #[default]
    Queue,
    /// End the running revision, which then returns
    /// [`DiagnosticsOutcome::Superseded`].
    Supersede,
}

/// Whether one bound dropped diagnostics from one list.
///
/// # Examples
///
/// ```
/// use kvim_lsp::Truncation;
///
/// assert_eq!(Truncation::Complete, Truncation::Complete);
/// assert_ne!(Truncation::Complete, Truncation::Truncated { dropped: 2 });
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Truncation {
    /// Every diagnostic of the list survived its bound.
    Complete,
    /// The bound dropped the lowest severities of the list.
    Truncated {
        /// The diagnostics that the bound dropped.
        dropped: usize,
    },
}

/// The result limits of one changed-file request.
///
/// [`DiagnosticsLimits::default`] names the constants of this module. A caller
/// that renders few rows lowers them. Every field is validated against its
/// constant, so no request can raise a bound of this crate.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{LSP_DIAGNOSTICS_MAX, DiagnosticsLimits};
///
/// let limits = DiagnosticsLimits::default().per_server(16);
/// assert_eq!(limits.per_server, 16);
/// assert!(DiagnosticsLimits::default().per_server(LSP_DIAGNOSTICS_MAX + 1).validate().is_err());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticsLimits {
    /// The diagnostics that one server contributes to the merge.
    pub per_server: usize,
    /// The diagnostics that the merged report holds.
    pub merged: usize,
    /// The related information entries that one diagnostic keeps.
    pub related_information: usize,
    /// The bytes that one diagnostic message keeps.
    pub message_bytes: usize,
    /// The bytes of the exact document text of one request.
    pub text_bytes: usize,
    /// The protocol bytes that one server spends on one request.
    pub protocol_bytes: usize,
}

impl Default for DiagnosticsLimits {
    fn default() -> Self {
        Self {
            per_server: LSP_DIAGNOSTICS_MAX,
            merged: LSP_MERGED_DIAGNOSTICS_MAX,
            related_information: LSP_RELATED_INFORMATION_MAX,
            message_bytes: LSP_DIAGNOSTIC_MESSAGE_BYTES_MAX,
            text_bytes: LSP_DOCUMENT_BYTES_MAX,
            protocol_bytes: LSP_REQUEST_BYTES_MAX,
        }
    }
}

impl DiagnosticsLimits {
    /// Bounds the diagnostics that one server contributes.
    #[must_use]
    pub const fn per_server(mut self, limit: usize) -> Self {
        self.per_server = limit;
        self
    }

    /// Bounds the diagnostics of the merged report.
    #[must_use]
    pub const fn merged(mut self, limit: usize) -> Self {
        self.merged = limit;
        self
    }

    /// Bounds the related information entries of one diagnostic.
    #[must_use]
    pub const fn related_information(mut self, limit: usize) -> Self {
        self.related_information = limit;
        self
    }

    /// Bounds the bytes of one diagnostic message.
    #[must_use]
    pub const fn message_bytes(mut self, limit: usize) -> Self {
        self.message_bytes = limit;
        self
    }

    /// Bounds the bytes of the exact document text.
    #[must_use]
    pub const fn text_bytes(mut self, limit: usize) -> Self {
        self.text_bytes = limit;
        self
    }

    /// Bounds the protocol bytes of one server and one request.
    #[must_use]
    pub const fn protocol_bytes(mut self, limit: usize) -> Self {
        self.protocol_bytes = limit;
        self
    }

    /// Rejects a limit above the bound of this crate.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::Bounds`] for the first field above its constant.
    pub fn validate(&self) -> Result<(), LspError> {
        enforce(self.per_server, LSP_DIAGNOSTICS_MAX, LspBound::Diagnostics)?;
        enforce(
            self.merged,
            LSP_MERGED_DIAGNOSTICS_MAX,
            LspBound::MergedDiagnostics,
        )?;
        enforce(
            self.related_information,
            LSP_RELATED_INFORMATION_MAX,
            LspBound::RelatedInformation,
        )?;
        enforce(
            self.message_bytes,
            LSP_DIAGNOSTIC_MESSAGE_BYTES_MAX,
            LspBound::DiagnosticMessageBytes,
        )?;
        enforce(
            self.text_bytes,
            LSP_DOCUMENT_BYTES_MAX,
            LspBound::DocumentBytes,
        )?;
        enforce(
            self.protocol_bytes,
            LSP_REQUEST_BYTES_MAX,
            LspBound::RequestBytes,
        )
    }
}

/// One place of the changed document that a diagnostic also names.
///
/// The crate holds the exact text of the changed document only, so it keeps the
/// entries of that document and drops an entry that names another document. A
/// range of another document has no text to validate against, and an unchecked
/// range could address text that no buffer holds.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{DocumentPosition, RelatedInformation, SourceSpan};
///
/// let related = RelatedInformation {
///     span: SourceSpan::new(DocumentPosition::new(1, 0), DocumentPosition::new(1, 4)),
///     message: "first defined here".to_owned(),
/// };
/// assert!(related.span.contains(DocumentPosition::new(1, 2)));
/// ```
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RelatedInformation {
    /// The range of the changed document that the entry names.
    pub span: SourceSpan,
    /// The message of the entry.
    pub message: String,
}

/// One diagnostic of one changed-file report.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{
///     Diagnostic, DiagnosticSeverity, DocumentPosition, ReportedDiagnostic, SourceSpan,
/// };
///
/// let reported = ReportedDiagnostic {
///     diagnostic: Diagnostic {
///         span: SourceSpan::new(DocumentPosition::new(0, 0), DocumentPosition::new(0, 3)),
///         severity: DiagnosticSeverity::Error,
///         message: "unknown name".to_owned(),
///         source: "checker".to_owned(),
///     },
///     related: Vec::new(),
/// };
/// assert_eq!(reported.diagnostic.severity, DiagnosticSeverity::Error);
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportedDiagnostic {
    /// The diagnostic of the changed document.
    pub diagnostic: Diagnostic,
    /// The bounded related information of the changed document.
    pub related: Vec<RelatedInformation>,
}

/// The terminal outcome of one server of one changed-file request.
///
/// Every variant is a normal state. The request records one variant for each
/// selected server, so one refusal never disappears behind one acceptance.
#[derive(Debug)]
pub enum ServerOutcome {
    /// The server answered for the accepted revision.
    Ready {
        /// The diagnostics that this server contributed to the merge.
        diagnostics: usize,
        /// Whether the per-server bound dropped diagnostics of this server.
        truncation: Truncation,
    },
    /// The declared program is not installed, so this server has no service.
    Unavailable,
    /// The server names no safe completion policy for this revision.
    Unsupported,
    /// The attempt failed with a typed cause.
    ///
    /// The value keeps the protocol, process, and invalid-response causes
    /// distinct, and it keeps the source of each of them.
    Failed(LspError),
    /// The session of this server stopped before it answered.
    Cancelled,
}

/// One selected server and its terminal outcome.
#[derive(Debug)]
pub struct ServerDiagnostics {
    /// The server inside its project.
    pub server: ServerId,
    /// What that server reached for the accepted revision.
    pub outcome: ServerOutcome,
}

/// The merged diagnostics of one accepted document revision.
///
/// The report names the accepted revision, so no caller can read it as the
/// answer of another revision. It holds the outcome of every selected server in
/// declaration order, so a partial refusal, a failure, and a per-server
/// truncation all stay visible.
#[derive(Debug)]
pub struct ChangedFileReport {
    revision: DocumentRevision,
    diagnostics: Vec<ReportedDiagnostic>,
    servers: Vec<ServerDiagnostics>,
    truncation: Truncation,
}

impl ChangedFileReport {
    /// Returns the document revision that every diagnostic describes.
    #[must_use]
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    /// Returns the merged diagnostics.
    ///
    /// The list holds no exact duplicate. It is sorted by severity, range,
    /// source, and message, and two equal keys keep their server declaration
    /// order.
    #[must_use]
    pub fn diagnostics(&self) -> &[ReportedDiagnostic] {
        &self.diagnostics
    }

    /// Returns the outcome of every selected server, in declaration order.
    #[must_use]
    pub fn servers(&self) -> &[ServerDiagnostics] {
        &self.servers
    }

    /// Returns whether the aggregate bound dropped diagnostics.
    #[must_use]
    pub const fn truncation(&self) -> Truncation {
        self.truncation
    }
}

/// What one changed-file request returned.
///
/// Every variant is a normal state. A caller reads a typed failure only for a
/// request that this crate refuses before it dispatches.
#[derive(Debug)]
pub enum DiagnosticsOutcome {
    /// Every selected server reached a terminal outcome for the revision.
    ///
    /// The report is shared, because two requests of one revision read one
    /// report and neither copies it.
    Ready(Arc<ChangedFileReport>),
    /// A selected server has not answered yet, and the policy waits for
    /// nothing.
    ///
    /// The request ends here and publishes no later result. A later request of
    /// the same revision reads the report that the servers still produce.
    Starting,
    /// No configured server declares the language of the request.
    Unsupported,
    /// A newer request ended this revision.
    Superseded,
    /// The wait deadline passed before every selected server answered.
    TimedOut,
    /// The caller cancelled the request.
    Cancelled,
}

/// One changed-file diagnostics request.
///
/// Build the value with [`ChangedFile::new`] and the setters. The defaults name
/// the complete result limits, one queued revision policy, and the deadline of
/// this module.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use kvim_lsp::{ChangedFile, DocumentRevision, LanguageId, LspError, WaitPolicy};
/// use kvim_path::WorktreeRelativePath;
///
/// let request = ChangedFile::new(
///     WorktreeRelativePath::new("src/main.rs").expect("the path is relative"),
///     "fn main() {}\n".to_owned(),
///     DocumentRevision::new(3),
///     LanguageId::new("rust")?,
/// )
/// .wait(WaitPolicy::Until(Duration::from_secs(5)));
/// assert_eq!(request.revision(), DocumentRevision::new(3));
/// # Ok::<(), LspError>(())
/// ```
pub struct ChangedFile {
    path: WorktreeRelativePath,
    text: String,
    revision: DocumentRevision,
    language: LanguageId,
    limits: DiagnosticsLimits,
    wait: WaitPolicy,
    revisions: RevisionPolicy,
    cancellation: Option<CancellationToken>,
}

impl ChangedFile {
    /// Declares one request over the exact text of one document revision.
    #[must_use]
    pub fn new(
        path: WorktreeRelativePath,
        text: String,
        revision: DocumentRevision,
        language: LanguageId,
    ) -> Self {
        Self {
            path,
            text,
            revision,
            language,
            limits: DiagnosticsLimits::default(),
            wait: WaitPolicy::Until(LSP_DIAGNOSTIC_DEADLINE),
            revisions: RevisionPolicy::Queue,
            cancellation: None,
        }
    }

    /// Returns the revision that this request describes.
    #[must_use]
    pub const fn revision(&self) -> DocumentRevision {
        self.revision
    }

    /// Bounds the result of this request.
    #[must_use]
    pub const fn limits(mut self, limits: DiagnosticsLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Selects how long this request waits.
    #[must_use]
    pub const fn wait(mut self, wait: WaitPolicy) -> Self {
        self.wait = wait;
        self
    }

    /// Selects what this request does with a running older revision.
    #[must_use]
    pub const fn revisions(mut self, revisions: RevisionPolicy) -> Self {
        self.revisions = revisions;
        self
    }

    /// Attaches the cancellation owner of this request.
    ///
    /// A cancelled token ends the wait and returns
    /// [`DiagnosticsOutcome::Cancelled`]. It also ends the work of every server
    /// that still serves the request.
    #[must_use]
    pub fn cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = Some(cancellation);
        self
    }
}

/// What one server produced for one job, before the merge.
///
/// The value carries the diagnostics of one server, so the merge reads every
/// server list in declaration order and allocates the merged list once.
#[derive(Debug)]
enum SlotOutcome {
    /// The server answered for the accepted revision.
    Ready {
        /// The diagnostics that survived the per-server bound.
        items: Vec<ReportedDiagnostic>,
        /// Whether that bound dropped diagnostics.
        truncation: Truncation,
    },
    /// The declared program is not installed.
    Unavailable,
    /// The server names no safe completion policy.
    Unsupported,
    /// The attempt failed with a typed cause.
    Failed(LspError),
    /// The session of this server stopped before it answered.
    Cancelled,
}

/// The result slot of one selected server of one job.
#[derive(Debug)]
struct JobSlot {
    /// The declaration index of the server, which orders the merge.
    index: usize,
    /// The server inside its project.
    server: ServerId,
    /// What that server reached, or `None` while it still serves.
    outcome: Option<SlotOutcome>,
}

/// How one job ended.
#[derive(Debug)]
enum JobCompletion {
    /// Every slot reached a terminal outcome, so the merge produced a report.
    Ready(Arc<ChangedFileReport>),
    /// A newer request ended this revision.
    Superseded,
    /// The deadline of this job passed.
    TimedOut,
    /// The caller cancelled this job.
    Cancelled,
}

/// The mutable state of one job.
#[derive(Debug)]
struct JobState {
    /// One slot for each selected server, in declaration order.
    slots: Vec<JobSlot>,
    /// How the job ended, or `None` while it runs.
    completion: Option<JobCompletion>,
}

/// One changed-file request that every selected server serves together.
///
/// The value is shared: the waiting caller reads it, and every conversation of
/// the project fills its own slot in it. A job ends exactly once, so no obsolete
/// result reaches a caller and no server publishes a second outcome.
struct Job {
    path: WorktreeRelativePath,
    text: String,
    revision: DocumentRevision,
    language: LanguageId,
    limits: DiagnosticsLimits,
    /// The instant after which no server of this job may still answer.
    deadline: Instant,
    /// The owner that ends every wait of this job.
    ended: CancellationToken,
    state: Mutex<JobState>,
}

impl Job {
    /// Creates one job over its selected servers, in declaration order.
    ///
    /// A server that already reached a terminal availability state fills its
    /// slot here, so a job whose servers all refused completes at once.
    fn new(request: &ChangedFile, deadline: Instant, slots: Vec<JobSlot>) -> Arc<Self> {
        let job = Arc::new(Self {
            path: request.path.clone(),
            text: request.text.clone(),
            revision: request.revision,
            language: request.language.clone(),
            limits: request.limits,
            deadline,
            ended: CancellationToken::new(),
            state: Mutex::new(JobState {
                slots,
                completion: None,
            }),
        });
        job.complete_if_ready(&mut job.state());
        job
    }

    /// Returns the mutable state of this job.
    fn state(&self) -> MutexGuard<'_, JobState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reports whether this job accepts no further outcome.
    ///
    /// A job that passed its deadline also ended, so an abandoned job never
    /// holds the request slot of the hub.
    fn has_ended(&self) -> bool {
        self.ended.is_cancelled() || Instant::now() >= self.deadline
    }

    /// Reports whether two jobs describe the same work.
    ///
    /// Two requests share one job only when the path, the exact text, the
    /// revision, the language, and the result limits are equal, because a
    /// joined caller reads the report of the running job.
    fn same_work(&self, other: &Self) -> bool {
        self.path == other.path
            && self.revision == other.revision
            && self.language == other.language
            && self.limits == other.limits
            && self.text == other.text
    }

    /// Returns the report of this job, or `None` while it holds none.
    fn report(&self) -> Option<Arc<ChangedFileReport>> {
        match self.state().completion.as_ref()? {
            JobCompletion::Ready(report) => Some(Arc::clone(report)),
            JobCompletion::Superseded | JobCompletion::TimedOut | JobCompletion::Cancelled => None,
        }
    }

    /// Returns the declaration index of the slot that one server must fill.
    fn open_slot(&self, server: ServerId) -> Option<usize> {
        if self.has_ended() {
            return None;
        }
        let state = self.state();
        if state.completion.is_some() {
            return None;
        }
        state
            .slots
            .iter()
            .find(|slot| slot.server == server && slot.outcome.is_none())
            .map(|slot| slot.index)
    }

    /// Records the terminal outcome of one server.
    ///
    /// A job that already ended keeps its outcome, so a late answer of a
    /// superseded revision never publishes.
    fn fill(&self, index: usize, outcome: SlotOutcome) {
        let mut state = self.state();
        if state.completion.is_some() {
            return;
        }
        // A liveness record fills the slot of a server without reading the
        // selection of the job, so a job that never selected that server holds
        // no slot for it.
        let Some(slot) = state.slots.iter_mut().find(|slot| slot.index == index) else {
            return;
        };
        if slot.outcome.is_some() {
            // Each server publishes one terminal outcome, so the first one
            // stands and a restart cannot replace it.
            return;
        }
        slot.outcome = Some(outcome);
        self.complete_if_ready(&mut state);
    }

    /// Ends this job without a report.
    fn end(&self, completion: JobCompletion) {
        let mut state = self.state();
        if state.completion.is_some() {
            return;
        }
        state.completion = Some(completion);
        drop(state);
        self.ended.cancel();
    }

    /// Merges every filled slot as soon as the last slot is filled.
    fn complete_if_ready(&self, state: &mut JobState) {
        if state.completion.is_some() || state.slots.iter().any(|slot| slot.outcome.is_none()) {
            return;
        }
        let finished = state
            .slots
            .iter_mut()
            .map(|slot| {
                let outcome = slot
                    .outcome
                    .take()
                    .expect("the branch above refuses an unfilled slot");
                (slot.server, outcome)
            })
            .collect();
        let report = merge(finished, &self.limits, self.revision);
        state.completion = Some(JobCompletion::Ready(Arc::new(report)));
        self.ended.cancel();
    }

    /// Returns the outcome of this job, or `None` while it still runs.
    fn outcome(&self) -> Option<DiagnosticsOutcome> {
        match self.state().completion.as_ref()? {
            JobCompletion::Ready(report) => Some(DiagnosticsOutcome::Ready(Arc::clone(report))),
            JobCompletion::Superseded => Some(DiagnosticsOutcome::Superseded),
            JobCompletion::TimedOut => Some(DiagnosticsOutcome::TimedOut),
            JobCompletion::Cancelled => Some(DiagnosticsOutcome::Cancelled),
        }
    }
}

/// Merges the outcomes of every selected server into one report.
///
/// The function is pure. It reads the outcomes and the limits, and it performs
/// no input and no output.
///
/// The merge keeps the declaration order of the servers, removes every exact
/// duplicate once, and then sorts by severity, range, source, and message. The
/// sort is stable, so two equal keys keep their server declaration order. The
/// aggregate bound therefore drops the lowest severities first, and an error of
/// one server always survives a warning of another server.
fn merge(
    finished: Vec<(ServerId, SlotOutcome)>,
    limits: &DiagnosticsLimits,
    revision: DocumentRevision,
) -> ChangedFileReport {
    let mut diagnostics: Vec<ReportedDiagnostic> = Vec::new();
    let mut servers = Vec::with_capacity(finished.len());
    for (server, outcome) in finished {
        let outcome = match outcome {
            SlotOutcome::Ready {
                mut items,
                truncation,
            } => {
                let count = items.len();
                diagnostics.append(&mut items);
                ServerOutcome::Ready {
                    diagnostics: count,
                    truncation,
                }
            }
            SlotOutcome::Unavailable => ServerOutcome::Unavailable,
            SlotOutcome::Unsupported => ServerOutcome::Unsupported,
            SlotOutcome::Failed(error) => ServerOutcome::Failed(error),
            SlotOutcome::Cancelled => ServerOutcome::Cancelled,
        };
        servers.push(ServerDiagnostics { server, outcome });
    }
    diagnostics.sort_by(severity_order);
    diagnostics.dedup();
    let truncation = truncate(&mut diagnostics, limits.merged);
    ChangedFileReport {
        revision,
        diagnostics,
        servers,
        truncation,
    }
}

/// Orders two diagnostics by severity, range, source, and message.
///
/// The related information decides the last comparison, so two diagnostics that
/// differ only there still stand beside each other and one exact duplicate pair
/// stays adjacent for the merge.
fn severity_order(left: &ReportedDiagnostic, right: &ReportedDiagnostic) -> Ordering {
    left.diagnostic
        .severity
        .cmp(&right.diagnostic.severity)
        .then_with(|| left.diagnostic.span.cmp(&right.diagnostic.span))
        .then_with(|| left.diagnostic.source.cmp(&right.diagnostic.source))
        .then_with(|| left.diagnostic.message.cmp(&right.diagnostic.message))
        .then_with(|| left.related.cmp(&right.related))
}

/// Drops the tail of one sorted list above its bound.
fn truncate(diagnostics: &mut Vec<ReportedDiagnostic>, limit: usize) -> Truncation {
    if diagnostics.len() <= limit {
        return Truncation::Complete;
    }
    let dropped = diagnostics.len() - limit;
    diagnostics.truncate(limit);
    Truncation::Truncated { dropped }
}

/// The bytes of one declared diagnostic source name.
///
/// The name reaches every diagnostic that its server sends without a `source`
/// field, so a short name keeps the merged report readable.
pub const LSP_SERVER_SOURCE_BYTES_MAX: usize = 64;

/// What one supervised server reached, as its conversation observed it.
///
/// A server that is not installed never serves one attempt. The liveness of the
/// registry therefore answers for it, so every request still records one
/// terminal outcome for that server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServerLiveness {
    /// The server has not answered its handshake yet.
    Starting,
    /// The server answered its handshake, so its conversation serves.
    Serving,
    /// The declared program is not installed.
    Unavailable,
    /// The supervisor accepts no further attempt of this server.
    Stopped,
}

impl ServerLiveness {
    /// Returns the outcome that this liveness already decides.
    fn terminal(self) -> Option<SlotOutcome> {
        match self {
            Self::Starting | Self::Serving => None,
            Self::Unavailable => Some(SlotOutcome::Unavailable),
            Self::Stopped => Some(SlotOutcome::Cancelled),
        }
    }
}

/// One server that the hub dispatches to.
struct RegisteredServer {
    /// The declaration index, which orders the dispatch and the merge.
    index: usize,
    /// The server inside its project.
    id: ServerId,
    /// The name of every diagnostic of this server that carries no `source`.
    source: String,
    /// The languages that this server declares.
    languages: Vec<LanguageId>,
    /// What the supervisor of this server reached.
    liveness: ServerLiveness,
}

/// One declared server of one changed-file diagnostics service.
///
/// Every member is caller data, so no code of this crate names one server
/// product.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{CompletionPolicy, DiagnosticsServer, LanguageId, LspError, ServerId};
///
/// let declaration = DiagnosticsServer {
///     id: ServerId::new(0),
///     source: "checker".to_owned(),
///     languages: vec![LanguageId::new("rust")?],
///     completion: CompletionPolicy::Pull,
/// };
/// assert_eq!(declaration.languages.len(), 1);
/// # Ok::<(), LspError>(())
/// ```
pub struct DiagnosticsServer {
    /// The identity of this server inside its project.
    pub id: ServerId,
    /// The name that every diagnostic without a `source` field carries.
    pub source: String,
    /// The languages that this server declares, which select it.
    pub languages: Vec<LanguageId>,
    /// How this server completes the diagnostics of one revision.
    pub completion: CompletionPolicy,
}

/// The state that the hub and every conversation of one project share.
struct Shared {
    servers: Mutex<Vec<RegisteredServer>>,
    /// The request that every conversation serves, or `None`.
    ///
    /// A conversation reads this value as soon as its server answers the
    /// handshake, so one request survives the startup of its servers without a
    /// watcher and without a resubmission.
    active: watch::Sender<Option<Arc<Job>>>,
}

/// Whether one candidate job became the active request.
enum Install {
    /// The job that now serves the request, which may be an equal running one.
    Active(Arc<Job>),
    /// Another revision is still running.
    Busy(Arc<Job>),
}

impl Shared {
    /// Returns the slots of every server that declares one language.
    ///
    /// A server that already reached a terminal availability state fills its
    /// slot here, because no conversation of that server will serve again.
    fn select(&self, language: &LanguageId) -> Vec<JobSlot> {
        self.servers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|server| server.languages.contains(language))
            .map(|server| JobSlot {
                index: server.index,
                server: server.id,
                outcome: server.liveness.terminal(),
            })
            .collect()
    }

    /// Returns the source name of one declared server.
    fn source(&self, index: usize) -> String {
        self.servers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(index)
            .map(|server| server.source.clone())
            .unwrap_or_default()
    }

    /// Records the liveness of one server and answers a waiting request.
    fn record(&self, index: usize, liveness: ServerLiveness) {
        {
            let mut servers = self.servers.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(server) = servers.get_mut(index) {
                server.liveness = liveness;
            }
        }
        let Some(outcome) = liveness.terminal() else {
            return;
        };
        let active = self.active.borrow().clone();
        if let Some(job) = active {
            job.fill(index, outcome);
        }
    }

    /// Installs one candidate job, or names the revision that still runs.
    ///
    /// The decision and the exchange happen together, so two callers cannot
    /// install two jobs for one project.
    fn install(&self, candidate: &Arc<Job>) -> Install {
        let mut install = None;
        self.active.send_if_modified(|slot| match slot.as_ref() {
            // A request of the same work joins the running job, and it also
            // joins a finished job that already holds its report. A caller
            // therefore never repeats the work of the revision that it asks
            // for.
            Some(current)
                if current.same_work(candidate)
                    && (current.report().is_some() || !current.has_ended()) =>
            {
                install = Some(Install::Active(Arc::clone(current)));
                false
            }
            Some(current) if !current.has_ended() => {
                install = Some(Install::Busy(Arc::clone(current)));
                false
            }
            _ => {
                *slot = Some(Arc::clone(candidate));
                install = Some(Install::Active(Arc::clone(candidate)));
                true
            }
        });
        install.expect("every branch of the exchange names one decision")
    }
}

/// The request side of the changed-file diagnostics of one project.
///
/// The hub owns no process and no task. It creates one
/// [`DiagnosticsConversation`] for each declared server, and the caller hands
/// those values to `ProjectDeclaration::server`. The project driver then keeps
/// every server warm, so a later request reuses one running session.
///
/// Declare every server before the first request, because a request dispatches
/// to the servers that the hub already holds.
///
/// # Examples
///
/// ```
/// use kvim_lsp::{
///     CompletionPolicy, DiagnosticsHub, DiagnosticsServer, LanguageId, LspError, ServerId,
/// };
///
/// let hub = DiagnosticsHub::new();
/// let conversation = hub.server(DiagnosticsServer {
///     id: ServerId::new(0),
///     source: "checker".to_owned(),
///     languages: vec![LanguageId::new("rust")?],
///     completion: CompletionPolicy::Pull,
/// })?;
/// // The caller hands the conversation to one project declaration.
/// drop(conversation);
/// # Ok::<(), LspError>(())
/// ```
pub struct DiagnosticsHub {
    shared: Arc<Shared>,
}

impl Default for DiagnosticsHub {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticsHub {
    /// Creates one hub that holds no server yet.
    #[must_use]
    pub fn new() -> Self {
        let (active, _) = watch::channel(None);
        Self {
            shared: Arc::new(Shared {
                servers: Mutex::new(Vec::new()),
                active,
            }),
        }
    }

    /// Declares one server and returns the conversation that serves it.
    ///
    /// The declaration order of the calls is the dispatch order and the merge
    /// order of every later request.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::DuplicateServer`] when two servers take one
    /// identity, and [`LspError::Bounds`] for a server without a language, for
    /// more than [`LSP_SERVER_LANGUAGES_MAX`] languages, for a source name
    /// above [`LSP_SERVER_SOURCE_BYTES_MAX`], and for more servers than one
    /// project may run.
    pub fn server(
        &self,
        declaration: DiagnosticsServer,
    ) -> Result<DiagnosticsConversation, LspError> {
        let DiagnosticsServer {
            id,
            source,
            languages,
            completion,
        } = declaration;
        if languages.is_empty() {
            // A server without a language is never selected, so the request
            // would wait for an outcome that no dispatch can produce.
            return Err(LspError::Bounds {
                measure: LspBound::Languages,
                limit: LSP_SERVER_LANGUAGES_MAX,
                actual: 0,
            });
        }
        enforce(
            languages.len(),
            LSP_SERVER_LANGUAGES_MAX,
            LspBound::Languages,
        )?;
        enforce(
            source.len(),
            LSP_SERVER_SOURCE_BYTES_MAX,
            LspBound::SourceBytes,
        )?;
        let mut servers = self
            .shared
            .servers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if servers.iter().any(|server| server.id == id) {
            return Err(LspError::DuplicateServer);
        }
        enforce(
            servers.len().saturating_add(1),
            LSP_SESSIONS_MAX,
            LspBound::Sessions,
        )?;
        let index = servers.len();
        servers.push(RegisteredServer {
            index,
            id,
            source,
            languages,
            liveness: ServerLiveness::Starting,
        });
        drop(servers);
        Ok(DiagnosticsConversation {
            shared: Arc::clone(&self.shared),
            jobs: self.shared.active.subscribe(),
            index,
            server: id,
            completion,
            open: None,
        })
    }

    /// Returns the diagnostics of one changed document revision.
    ///
    /// The call keeps one request alive through server startup and diagnostic
    /// completion when [`WaitPolicy::Until`] names its deadline. It starts no
    /// watcher, polls nothing, and never resubmits.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::Bounds`] for a result limit above the bound of this
    /// crate and for text above the declared text bound. Every other state is
    /// one ordinary [`DiagnosticsOutcome`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::time::Duration;
    ///
    /// use kvim_lsp::{
    ///     ChangedFile, DiagnosticsHub, DiagnosticsOutcome, DocumentRevision, LanguageId,
    ///     LspError, WaitPolicy,
    /// };
    /// use kvim_path::WorktreeRelativePath;
    ///
    /// # async fn ask(hub: &DiagnosticsHub) -> Result<(), LspError> {
    /// let request = ChangedFile::new(
    ///     WorktreeRelativePath::new("src/main.rs").expect("the path is relative"),
    ///     "fn main() {}\n".to_owned(),
    ///     DocumentRevision::new(1),
    ///     LanguageId::new("rust")?,
    /// )
    /// .wait(WaitPolicy::Until(Duration::from_secs(5)));
    /// if let DiagnosticsOutcome::Ready(report) = hub.changed_file(request).await? {
    ///     assert_eq!(report.revision(), DocumentRevision::new(1));
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn changed_file(&self, request: ChangedFile) -> Result<DiagnosticsOutcome, LspError> {
        request.limits.validate()?;
        enforce(
            request.text.len(),
            request.limits.text_bytes,
            LspBound::DocumentBytes,
        )?;
        let slots = self.shared.select(&request.language);
        if slots.is_empty() {
            return Ok(DiagnosticsOutcome::Unsupported);
        }
        let immediate = matches!(request.wait, WaitPolicy::Immediate);
        let deadline = Instant::now()
            + match request.wait {
                WaitPolicy::Immediate => LSP_DIAGNOSTIC_DEADLINE,
                WaitPolicy::Until(wait) => wait,
            };
        let candidate = Job::new(&request, deadline, slots);
        let job = loop {
            match self.shared.install(&candidate) {
                Install::Active(job) => break job,
                Install::Busy(current) => match request.revisions {
                    // The newer revision ends the older one, which returns
                    // `Superseded` to its own caller. No obsolete result
                    // publishes after that.
                    RevisionPolicy::Supersede => current.end(JobCompletion::Superseded),
                    RevisionPolicy::Queue => {
                        if immediate {
                            return Ok(DiagnosticsOutcome::Starting);
                        }
                        if time::timeout_at(deadline, current.ended.cancelled())
                            .await
                            .is_err()
                        {
                            return Ok(DiagnosticsOutcome::TimedOut);
                        }
                    }
                },
            }
        };
        if immediate {
            // The request ends here. The servers keep the job, so a later
            // request of this revision reads the report that they produce.
            return Ok(job.outcome().unwrap_or(DiagnosticsOutcome::Starting));
        }
        // A caller that joined the job of another caller owns no deadline of
        // that job, so it ends its own wait and leaves the job running.
        let owned = Arc::ptr_eq(&job, &candidate);
        tokio::select! {
            biased;
            () = job.ended.cancelled() => {}
            () = cancelled(request.cancellation.as_ref()) => {
                if owned {
                    job.end(JobCompletion::Cancelled);
                }
                return Ok(DiagnosticsOutcome::Cancelled);
            }
            () = time::sleep_until(deadline) => {
                if owned {
                    job.end(JobCompletion::TimedOut);
                }
                return Ok(DiagnosticsOutcome::TimedOut);
            }
        }
        Ok(job.outcome().unwrap_or(DiagnosticsOutcome::TimedOut))
    }
}

/// Waits for one cancellation owner, or forever when the request names none.
async fn cancelled(token: Option<&CancellationToken>) {
    match token {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

/// The conversation that serves the changed-file requests of one server.
///
/// [`DiagnosticsHub::server`] creates the value, and the caller hands it to
/// `ProjectDeclaration::server`. Every attempt of that server reads the active
/// request as soon as its handshake completes, so a request that a caller sent
/// before the process started still reaches this server.
pub struct DiagnosticsConversation {
    shared: Arc<Shared>,
    jobs: watch::Receiver<Option<Arc<Job>>>,
    /// The declaration index of this server.
    index: usize,
    /// The identity of this server inside its project.
    server: ServerId,
    /// How this server completes the diagnostics of one revision.
    completion: CompletionPolicy,
    /// The URI that this attempt holds open, or `None`.
    open: Option<String>,
}

/// What one attempt waits for after it synchronized the document.
#[derive(Clone, Copy, Debug)]
enum Target {
    /// The answer of one `textDocument/diagnostic` request.
    Pull {
        /// The request number that the answer must carry.
        number: u64,
    },
    /// A `textDocument/publishDiagnostics` notification of the exact revision.
    Push,
}

impl ServerConversation for DiagnosticsConversation {
    async fn serve(&mut self, mut attempt: Attempt<'_>) -> AttemptEnd {
        // A restarted server holds no document of the previous attempt, so this
        // attempt opens every document that it serves again.
        self.open = None;
        loop {
            let active = self.jobs.borrow_and_update().clone();
            if let Some(job) = active
                && let Some(index) = job.open_slot(self.server)
            {
                match self.produce(&mut attempt, &job).await {
                    Ok(Some(outcome)) => job.fill(index, outcome),
                    // The job ended, or the caller cancelled the project.
                    // Neither publishes an outcome for this revision.
                    Ok(None) => {}
                    // A fatal failure leaves the message stream unusable.
                    // The supervisor restarts the server, and the next
                    // attempt serves this job again. The restart bound ends
                    // the session, and the stopped record then fills the
                    // slot, so the request still reaches one outcome.
                    Err(error) if error.is_fatal() => return AttemptEnd::Failed(error),
                    Err(error) => job.fill(index, SlotOutcome::Failed(error)),
                }
            }
            tokio::select! {
                biased;
                () = attempt.cancellation.cancelled() => return AttemptEnd::Stopped,
                changed = self.jobs.changed() => if changed.is_err() {
                    return AttemptEnd::Stopped;
                },
            }
        }
    }

    fn observe(&mut self, event: &ServerEvent) {
        let liveness = match event {
            ServerEvent::Started | ServerEvent::Restarted { .. } => ServerLiveness::Serving,
            ServerEvent::Unavailable => ServerLiveness::Unavailable,
            ServerEvent::Stopped => ServerLiveness::Stopped,
            // A failed attempt may restart, so it decides no terminal outcome.
            // A process report changes no request at all.
            ServerEvent::Failed(_) | ServerEvent::Reported(_) => return,
        };
        self.shared.record(self.index, liveness);
    }
}

impl DiagnosticsConversation {
    /// Produces the outcome of this server for one job.
    ///
    /// `Ok(None)` means that the job ended before this server answered, so the
    /// slot stays empty and no obsolete result publishes.
    async fn produce(
        &mut self,
        attempt: &mut Attempt<'_>,
        job: &Job,
    ) -> Result<Option<SlotOutcome>, LspError> {
        let completion = match self.completion {
            CompletionPolicy::Unsupported => return Ok(Some(SlotOutcome::Unsupported)),
            CompletionPolicy::VersionedPush => Completion::Push,
            CompletionPolicy::Pull => match attempt.capabilities.diagnostics() {
                DiagnosticsModel::Pull { identifier } => Completion::Pull {
                    identifier: identifier.clone(),
                },
                // The server advertises no diagnostic provider, so it answers
                // no pull and this policy names no safe completion.
                DiagnosticsModel::Push => return Ok(Some(SlotOutcome::Unsupported)),
            },
        };
        let uri = attempt.root.relative_uri(&job.path)?;
        // The mapping converts every received column against the exact text
        // that this request supplied, so no answer can address another
        // revision of the document.
        let mapping = DocumentMapping::new(
            attempt.capabilities.encoding(),
            TextMirroring::Absent,
            &job.text,
        );
        self.reopen(attempt, &uri, job).await?;
        let target = match completion {
            Completion::Pull { identifier } => {
                let mut params = json!({ "textDocument": { "uri": uri } });
                if let Some(identifier) = identifier {
                    params["identifier"] = Value::String(identifier);
                }
                Target::Pull {
                    number: attempt.writer.request(PULL_METHOD, params).await?,
                }
            }
            Completion::Push => Target::Push,
        };
        self.collect(attempt, job, &uri, &mapping, target).await
    }

    /// Sends the exact text of one revision to the server.
    ///
    /// The call closes the document that the attempt still holds and opens the
    /// requested revision again. One open notification carries the complete
    /// text, so the sequence serves every synchronization mode and it triggers
    /// a fresh analysis of a push server.
    async fn reopen(
        &mut self,
        attempt: &mut Attempt<'_>,
        uri: &str,
        job: &Job,
    ) -> Result<(), LspError> {
        if let Some(open) = self.open.take() {
            attempt
                .writer
                .notify(
                    "textDocument/didClose",
                    json!({ "textDocument": { "uri": open } }),
                )
                .await?;
        }
        attempt
            .writer
            .notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": job.language.as_str(),
                        "version": job.revision.get(),
                        "text": job.text,
                    }
                }),
            )
            .await?;
        self.open = Some(uri.to_owned());
        Ok(())
    }

    /// Reads messages until the target of this policy arrives.
    ///
    /// Every wait carries the deadline of the job, the end of the job, and the
    /// cancellation owner of the project, so no server can hold this request
    /// open.
    async fn collect(
        &self,
        attempt: &mut Attempt<'_>,
        job: &Job,
        uri: &str,
        mapping: &DocumentMapping,
        target: Target,
    ) -> Result<Option<SlotOutcome>, LspError> {
        let mut traffic = 0_usize;
        loop {
            let envelope = tokio::select! {
                biased;
                () = attempt.cancellation.cancelled() => return Ok(None),
                () = job.ended.cancelled() => return Ok(None),
                () = time::sleep_until(job.deadline) => {
                    job.end(JobCompletion::TimedOut);
                    return Ok(None);
                }
                envelope = attempt.envelopes.recv() => envelope,
            };
            let envelope = envelope.ok_or(LspError::Stopped)??;
            traffic = traffic.saturating_add(payload_bytes(&envelope));
            enforce(traffic, job.limits.protocol_bytes, LspBound::RequestBytes)?;
            if let Some(method) = envelope.method.as_deref() {
                if method == PUBLISH_METHOD {
                    if matches!(target, Target::Push)
                        && let Some(outcome) = self.accept_publish(job, uri, mapping, &envelope)?
                    {
                        return Ok(Some(outcome));
                    }
                    continue;
                }
                // kvim implements no server-to-client request, so it answers
                // every one of them and no server stalls behind this request.
                if let Some(id) = envelope.id {
                    attempt.writer.reject_server_request(id).await?;
                }
                continue;
            }
            let Target::Pull { number } = target else {
                continue;
            };
            let Some(RpcId::Unsigned(id)) = envelope.id else {
                continue;
            };
            if id != number {
                continue;
            }
            if let Some(error) = envelope.error {
                return Err(LspError::Response { code: error.code });
            }
            let result = envelope.result.ok_or(LspError::MalformedResponse)?;
            return self.read_report(job, mapping, uri, &result).map(Some);
        }
    }

    /// Reads one published set, or `None` for a set of another revision.
    ///
    /// The notification must name the requested document and the exact
    /// requested revision. A set without a version names no revision, so it
    /// completes no request and never publishes.
    fn accept_publish(
        &self,
        job: &Job,
        uri: &str,
        mapping: &DocumentMapping,
        envelope: &RpcEnvelope,
    ) -> Result<Option<SlotOutcome>, LspError> {
        let params = envelope
            .params
            .as_deref()
            .ok_or(LspError::MalformedResponse)?;
        let published: PublishedDiagnostics =
            serde_json::from_str(params.get()).map_err(|_| LspError::MalformedResponse)?;
        if published.uri != uri || published.version != Some(job.revision.get()) {
            return Ok(None);
        }
        self.read_items(job, mapping, uri, &published.diagnostics)
            .map(Some)
    }

    /// Reads one pulled report of the requested revision.
    ///
    /// The request carries no previous result identifier, so a conformant
    /// server answers a full report. An unchanged report would repeat a set
    /// that this request never received, so it is an invalid answer.
    fn read_report(
        &self,
        job: &Job,
        mapping: &DocumentMapping,
        uri: &str,
        result: &RawValue,
    ) -> Result<SlotOutcome, LspError> {
        let report: PulledReport =
            serde_json::from_str(result.get()).map_err(|_| LspError::MalformedResponse)?;
        if let Some(result_id) = report.result_id.as_deref() {
            enforce(
                result_id.len(),
                LSP_RESULT_ID_BYTES_MAX,
                LspBound::ResultIdBytes,
            )?;
        }
        if report.kind == UNCHANGED_REPORT {
            return Err(LspError::MalformedResponse);
        }
        let items = report.items.as_deref().ok_or(LspError::MalformedResponse)?;
        self.read_items(job, mapping, uri, items)
    }

    /// Converts one received list into the bounded result of this server.
    ///
    /// The per-server bound applies here, before the merge applies the
    /// aggregate bound. The list is sorted by severity first, so the bound
    /// keeps the errors of this server and drops its lowest severities.
    fn read_items(
        &self,
        job: &Job,
        mapping: &DocumentMapping,
        uri: &str,
        items: &RawValue,
    ) -> Result<SlotOutcome, LspError> {
        let mut budget = ArrayBudget::new(LSP_DIAGNOSTICS_MAX, LSP_DIAGNOSTICS_MAX);
        let raw: Vec<RawDiagnostic> = deserialize_bounded_array(
            items,
            LSP_DIAGNOSTICS_MAX,
            LspBound::Diagnostics,
            &mut budget,
        )?;
        let source = self.shared.source(self.index);
        let mut converted = Vec::with_capacity(raw.len());
        for diagnostic in raw {
            converted.push(convert(diagnostic, &source, job, mapping, uri)?);
        }
        converted.sort_by(severity_order);
        converted.dedup();
        let truncation = truncate(&mut converted, job.limits.per_server);
        Ok(SlotOutcome::Ready {
            items: converted,
            truncation,
        })
    }
}

/// What one attempt asks for after it synchronized the document.
enum Completion {
    /// Ask with `textDocument/diagnostic` and repeat the provider identifier.
    Pull {
        /// The provider identifier of the capability, when it names one.
        identifier: Option<String>,
    },
    /// Wait for a published set of the exact revision.
    Push,
}

/// Converts one received diagnostic into the value of one report.
///
/// The conversion validates the range against the exact text that the caller
/// supplied, so no answer can mark text that the buffer does not hold.
fn convert(
    raw: RawDiagnostic,
    source: &str,
    job: &Job,
    mapping: &DocumentMapping,
    uri: &str,
) -> Result<ReportedDiagnostic, LspError> {
    let related = match raw.related() {
        Some(related) => read_related(related, job, mapping, uri)?,
        None => Vec::new(),
    };
    let diagnostic = raw.into_diagnostic(source, mapping)?;
    enforce(
        diagnostic.message.len(),
        job.limits.message_bytes,
        LspBound::DiagnosticMessageBytes,
    )?;
    diagnostic.span.validate(&job.text)?;
    Ok(ReportedDiagnostic {
        diagnostic,
        related,
    })
}

/// Reads the bounded related information of one diagnostic.
///
/// The crate holds the exact text of the changed document only, so an entry of
/// another document has no text to validate its range against. Such an entry
/// leaves the report.
fn read_related(
    raw: &RawValue,
    job: &Job,
    mapping: &DocumentMapping,
    uri: &str,
) -> Result<Vec<RelatedInformation>, LspError> {
    let limit = job.limits.related_information;
    let mut budget = ArrayBudget::new(limit, limit);
    let entries: Vec<RawRelatedInformation> =
        deserialize_bounded_array(raw, limit, LspBound::RelatedInformation, &mut budget)?;
    let mut related = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.location.uri != uri {
            continue;
        }
        enforce(
            entry.message.len(),
            job.limits.message_bytes,
            LspBound::DiagnosticMessageBytes,
        )?;
        let span = mapping.span_to_document(entry.location.range)?;
        span.validate(&job.text)?;
        related.push(RelatedInformation {
            span,
            message: entry.message,
        });
    }
    Ok(related)
}

/// Returns the bytes that one message spends of the request traffic budget.
fn payload_bytes(envelope: &RpcEnvelope) -> usize {
    let params = envelope.params.as_deref().map_or(0, |raw| raw.get().len());
    let result = envelope.result.as_deref().map_or(0, |raw| raw.get().len());
    params.saturating_add(result)
}

/// The wire shape of one pulled diagnostic report.
///
/// The shape names no `relatedDocuments` member, so this crate never parses one
/// and allocates nothing for it. See `docs/language-services.md`.
#[derive(Debug, Deserialize)]
struct PulledReport {
    /// `full` for a complete report, and `unchanged` for the previous set.
    #[serde(default)]
    kind: String,
    /// The identifier that a later pull of this document would repeat.
    #[serde(default, rename = "resultId")]
    result_id: Option<String>,
    /// The unparsed items of a full report.
    #[serde(default)]
    items: Option<Box<RawValue>>,
}

/// The wire shape of one published diagnostic set.
#[derive(Debug, Deserialize)]
struct PublishedDiagnostics {
    /// The document that the set describes.
    uri: String,
    /// The revision that the set describes, when the server names one.
    #[serde(default)]
    version: Option<i32>,
    /// The unparsed items of the set.
    diagnostics: Box<RawValue>,
}

/// The wire shape of one related information entry.
#[derive(Debug, Deserialize)]
struct RawRelatedInformation {
    /// The place that the entry names.
    location: RawLocation,
    /// The message of the entry.
    #[serde(default)]
    message: String,
}

/// The wire shape of one protocol location.
#[derive(Debug, Deserialize)]
struct RawLocation {
    /// The document of the location.
    uri: String,
    /// The range inside that document.
    range: ProtocolSpan,
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::io::{AsyncWriteExt, DuplexStream, duplex};
    use tokio::process::Command;
    use tokio::task::JoinHandle;
    use tokio::time;
    use tokio_util::sync::CancellationToken;

    use kvim_path::WorktreeRelativePath;

    use crate::process::{Transport, TransportFactory};
    use crate::project::{
        ManagerLimits, ProjectDeclaration, ProjectHandle, ProjectId, ProjectManager,
        ServerDeclaration, ServerId,
    };
    use crate::protocol::{LSP_OUTPUT_BYTES_MAX, LspBound, LspError, WorkspaceRoot, read_frame};

    use super::{
        ChangedFile, CompletionPolicy, DiagnosticsHub, DiagnosticsLimits, DiagnosticsOutcome,
        DiagnosticsServer, DocumentRevision, LSP_DIAGNOSTICS_MAX, LSP_SERVER_LANGUAGES_MAX,
        LanguageId, RevisionPolicy, ServerOutcome, Truncation, WaitPolicy,
    };

    /// The capacity of one test pipe, in bytes.
    const PIPE_BYTES: usize = 256 * 1024;

    /// The guard that stops a broken test instead of hanging the suite.
    const TEST_DEADLINE: Duration = Duration::from_secs(30);

    /// The deadline of a request that must reach its own end.
    const SHORT_DEADLINE: Duration = Duration::from_millis(300);

    /// The deadline of a request that must not end before its servers answer.
    const LONG_DEADLINE: Duration = Duration::from_secs(20);

    /// The root of every test project.
    const ROOT: &str = "/workspace";

    /// The changed document of every test request.
    const DOCUMENT: &str = "src/main.rs";

    /// The `file` URI of [`DOCUMENT`] below [`ROOT`].
    const DOCUMENT_URI: &str = "file:///workspace/src/main.rs";

    /// The exact text of the changed revision.
    const TEXT: &str = "fn main() {}\n";

    /// The shell that runs the child of the process test.
    const SHELL: &str = "/bin/sh";

    /// The mock server side of one prepared session.
    struct MockServer {
        input: DuplexStream,
        output: DuplexStream,
        read_bytes: usize,
    }

    impl MockServer {
        /// Reads the next message, or `None` after the session ended.
        async fn read_message(&mut self) -> Option<Value> {
            let body = time::timeout(
                TEST_DEADLINE,
                read_frame(&mut self.output, &mut self.read_bytes, LSP_OUTPUT_BYTES_MAX),
            )
            .await
            .expect("the session writes before the test deadline")
            .ok()?;
            serde_json::from_slice(&body).ok()
        }

        /// Writes one raw frame to the session.
        async fn send(&mut self, value: &Value) {
            let body = serde_json::to_vec(value).expect("the test value serializes");
            let header = format!("Content-Length: {}\r\n\r\n", body.len());
            let _ = self.input.write_all(header.as_bytes()).await;
            let _ = self.input.write_all(&body).await;
            let _ = self.input.flush().await;
        }

        /// Answers `initialize` with the supplied capabilities.
        async fn handshake(&mut self, capabilities: Value) {
            let initialize = self
                .read_message()
                .await
                .expect("the session sends initialize");
            assert_eq!(initialize["method"], "initialize");
            self.send(&json!({
                "jsonrpc": "2.0",
                "id": initialize["id"],
                "result": { "capabilities": capabilities },
            }))
            .await;
            let initialized = self
                .read_message()
                .await
                .expect("the session sends initialized");
            assert_eq!(initialized["method"], "initialized");
        }

        /// Reads messages until the named method arrives.
        async fn expect(&mut self, method: &str) -> Value {
            loop {
                let message = self
                    .read_message()
                    .await
                    .expect("the session sends the expected method");
                if message["method"] == method {
                    return message;
                }
            }
        }
    }

    /// Creates one connected stream pair.
    fn pipe() -> (TransportFactory, MockServer) {
        let (session_input, server_output) = duplex(PIPE_BYTES);
        let (server_input, session_output) = duplex(PIPE_BYTES);
        (
            TransportFactory::Prepared(vec![Transport::prepared(session_input, session_output)]),
            MockServer {
                input: server_input,
                output: server_output,
                read_bytes: 0,
            },
        )
    }

    /// The capabilities of one server that answers a pull.
    fn pull_capabilities() -> Value {
        json!({
            "positionEncoding": "utf-8",
            "textDocumentSync": 1,
            "diagnosticProvider": { "identifier": "test" },
        })
    }

    /// The capabilities of one server that publishes its sets.
    fn push_capabilities() -> Value {
        json!({ "positionEncoding": "utf-8", "textDocumentSync": 1 })
    }

    /// Answers every pull of one server with the same items.
    fn pull_server(mock: MockServer, items: Value) -> JoinHandle<()> {
        pull_server_after(mock, Duration::ZERO, items)
    }

    /// Answers every pull after one startup delay.
    fn pull_server_after(mut mock: MockServer, delay: Duration, items: Value) -> JoinHandle<()> {
        tokio::spawn(async move {
            if !delay.is_zero() {
                time::sleep(delay).await;
            }
            mock.handshake(pull_capabilities()).await;
            while let Some(message) = mock.read_message().await {
                if message["method"] == "textDocument/diagnostic" {
                    mock.send(&json!({
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": { "kind": "full", "items": items },
                    }))
                    .await;
                }
            }
        })
    }

    /// Publishes one set for the revision of every opened document.
    fn push_server(mock: MockServer, items: Value) -> JoinHandle<()> {
        publishing_server(mock, Vec::new(), items)
    }

    /// Publishes the obsolete sets first and then the set of the revision.
    ///
    /// Each obsolete entry names its own version, or JSON `null` for a set that
    /// names no revision at all.
    fn publishing_server(
        mut mock: MockServer,
        obsolete: Vec<(Value, Value)>,
        items: Value,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            mock.handshake(push_capabilities()).await;
            loop {
                let opened = mock.expect("textDocument/didOpen").await;
                let uri = opened["params"]["textDocument"]["uri"].clone();
                let version = opened["params"]["textDocument"]["version"].clone();
                for (stale_version, stale_items) in &obsolete {
                    mock.send(&publication(&uri, stale_version, stale_items))
                        .await;
                }
                mock.send(&publication(&uri, &version, &items)).await;
            }
        })
    }

    /// Builds one `textDocument/publishDiagnostics` notification.
    fn publication(uri: &Value, version: &Value, items: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "version": version, "diagnostics": items },
        })
    }

    /// Answers the handshake and then answers nothing.
    fn silent_server(mut mock: MockServer, capabilities: Value) -> JoinHandle<()> {
        tokio::spawn(async move {
            mock.handshake(capabilities).await;
            while mock.read_message().await.is_some() {}
        })
    }

    /// Builds one received diagnostic of the changed document.
    fn item(line: u32, start: u32, end: u32, severity: u8, message: &str) -> Value {
        json!({
            "range": {
                "start": { "line": line, "character": start },
                "end": { "line": line, "character": end },
            },
            "severity": severity,
            "source": "test",
            "message": message,
        })
    }

    /// Returns the language that every test request names.
    fn language() -> LanguageId {
        LanguageId::new("rust").expect("the identifier is short")
    }

    /// Declares one server of the hub.
    fn declared(id: u64, completion: CompletionPolicy) -> DiagnosticsServer {
        DiagnosticsServer {
            id: ServerId::new(id),
            source: "test".to_owned(),
            languages: vec![language()],
            completion,
        }
    }

    /// Builds one request over the exact text of one revision.
    fn request(revision: i32) -> ChangedFile {
        ChangedFile::new(
            WorktreeRelativePath::new(DOCUMENT).expect("the path is relative"),
            TEXT.to_owned(),
            DocumentRevision::new(revision),
            language(),
        )
        .wait(WaitPolicy::Until(LONG_DEADLINE))
    }

    /// One open project, its hub, and its running driver.
    struct Session {
        hub: DiagnosticsHub,
        handle: ProjectHandle,
        driver: JoinHandle<()>,
    }

    impl Session {
        /// Opens one project over the declared servers and their transports.
        fn open(servers: Vec<DiagnosticsServer>, transports: Vec<TransportFactory>) -> Self {
            assert_eq!(servers.len(), transports.len(), "one transport per server");
            let hub = DiagnosticsHub::new();
            let manager = ProjectManager::new(ManagerLimits::default());
            let root = WorkspaceRoot::new(PathBuf::from(ROOT)).expect("the root is absolute");
            let mut declaration = ProjectDeclaration::new(ProjectId::FIRST, root);
            for (server, transport) in servers.into_iter().zip(transports) {
                let id = server.id;
                let conversation = hub.server(server).expect("the hub accepts the declaration");
                declaration = declaration.server(
                    ServerDeclaration {
                        id,
                        transport,
                        options: json!({}),
                        workspace_settings: None,
                    },
                    conversation,
                );
            }
            let (handle, driver) = manager.open(declaration).expect("the budget holds it");
            Self {
                hub,
                handle,
                driver: tokio::spawn(driver.run()),
            }
        }

        /// Asks for the diagnostics of one revision.
        async fn ask(&self, request: ChangedFile) -> DiagnosticsOutcome {
            time::timeout(TEST_DEADLINE, self.hub.changed_file(request))
                .await
                .expect("the request ends before the test deadline")
                .expect("the request holds every bound")
        }

        /// Ends the project and waits for its driver.
        async fn close(self) {
            self.handle.close().await;
            let _ = time::timeout(TEST_DEADLINE, self.driver).await;
        }
    }

    /// Returns the report of one ready outcome.
    fn ready(outcome: DiagnosticsOutcome) -> std::sync::Arc<super::ChangedFileReport> {
        match outcome {
            DiagnosticsOutcome::Ready(report) => report,
            other => panic!("the request returned {other:?} instead of one report"),
        }
    }

    /// Returns the failure of the first server of one report.
    fn failure(report: &super::ChangedFileReport) -> &LspError {
        match &report.servers()[0].outcome {
            ServerOutcome::Failed(error) => error,
            other => panic!("the server returned {other:?} instead of one failure"),
        }
    }

    #[tokio::test]
    async fn one_until_request_survives_the_startup_of_its_server() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        // The server answers its handshake long after the request starts, and
        // the request still returns the diagnostics of its own revision.
        let server = pull_server_after(
            mock,
            Duration::from_millis(400),
            json!([item(0, 3, 7, 2, "one marker")]),
        );

        let report = ready(session.ask(request(7)).await);

        assert_eq!(report.revision(), DocumentRevision::new(7));
        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(report.diagnostics()[0].diagnostic.message, "one marker");
        assert!(matches!(
            report.servers()[0].outcome,
            ServerOutcome::Ready {
                diagnostics: 1,
                truncation: Truncation::Complete,
            }
        ));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn an_immediate_request_reports_a_server_that_did_not_answer() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let server = silent_server(mock, pull_capabilities());

        let outcome = session.ask(request(1).wait(WaitPolicy::Immediate)).await;

        assert!(matches!(outcome, DiagnosticsOutcome::Starting));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn an_immediate_request_reads_the_report_of_the_finished_revision() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let server = pull_server(mock, json!([item(0, 0, 2, 1, "one error")]));

        let first = ready(session.ask(request(3)).await);
        // The finished revision keeps its report, so the second request reads
        // it without a second dispatch.
        let second = ready(session.ask(request(3).wait(WaitPolicy::Immediate)).await);

        assert_eq!(first.revision(), second.revision());
        assert_eq!(second.diagnostics().len(), 1);
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn a_versioned_push_server_completes_the_exact_revision() {
        let (transport, mock) = pipe();
        let session = Session::open(
            vec![declared(0, CompletionPolicy::VersionedPush)],
            vec![transport],
        );
        let server = push_server(mock, json!([item(0, 0, 2, 1, "one error")]));

        let report = ready(session.ask(request(5)).await);

        assert_eq!(report.revision(), DocumentRevision::new(5));
        assert_eq!(report.diagnostics().len(), 1);
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn a_published_set_of_another_revision_never_publishes() {
        let (transport, mock) = pipe();
        let session = Session::open(
            vec![declared(0, CompletionPolicy::VersionedPush)],
            vec![transport],
        );
        // The server publishes the set of an earlier revision, and then one set
        // that names no revision at all. Neither completes the request.
        let server = publishing_server(
            mock,
            vec![
                (json!(4), json!([item(0, 0, 2, 1, "stale error")])),
                (Value::Null, json!([item(0, 0, 2, 1, "versionless error")])),
            ],
            json!([item(0, 3, 7, 2, "current marker")]),
        );

        let report = ready(session.ask(request(5)).await);

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].diagnostic.message,
            "current marker",
            "an obsolete set reached the report"
        );
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn a_range_outside_the_exact_text_refuses_the_answer() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        // The document holds two lines, so line nine addresses no text of the
        // revision that the caller supplied.
        let server = pull_server(mock, json!([item(9, 0, 4, 1, "outside")]));

        let report = ready(session.ask(request(1)).await);

        assert!(matches!(failure(&report), LspError::MalformedResponse));
        assert!(report.diagnostics().is_empty());
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn the_per_server_bound_keeps_the_errors_of_that_server() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let server = pull_server(
            mock,
            json!([
                item(0, 3, 7, 2, "one warning"),
                item(0, 0, 2, 1, "one error")
            ]),
        );

        let report = ready(
            session
                .ask(request(1).limits(DiagnosticsLimits::default().per_server(1)))
                .await,
        );

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(report.diagnostics()[0].diagnostic.message, "one error");
        assert!(matches!(
            report.servers()[0].outcome,
            ServerOutcome::Ready {
                diagnostics: 1,
                truncation: Truncation::Truncated { dropped: 1 },
            }
        ));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn the_aggregate_bound_keeps_the_errors_of_every_server() {
        let (first_transport, first_mock) = pipe();
        let (second_transport, second_mock) = pipe();
        let session = Session::open(
            vec![
                declared(0, CompletionPolicy::Pull),
                declared(1, CompletionPolicy::VersionedPush),
            ],
            vec![first_transport, second_transport],
        );
        let first = pull_server(first_mock, json!([item(0, 3, 7, 2, "one warning")]));
        let second = push_server(second_mock, json!([item(0, 0, 2, 1, "one error")]));

        let report = ready(
            session
                .ask(request(1).limits(DiagnosticsLimits::default().merged(1)))
                .await,
        );

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(report.diagnostics()[0].diagnostic.message, "one error");
        assert_eq!(report.truncation(), Truncation::Truncated { dropped: 1 });
        session.close().await;
        first.abort();
        second.abort();
    }

    #[tokio::test]
    async fn mixed_servers_merge_in_declaration_order_and_drop_one_duplicate() {
        let (first_transport, first_mock) = pipe();
        let (second_transport, second_mock) = pipe();
        let session = Session::open(
            vec![
                declared(0, CompletionPolicy::Pull),
                declared(1, CompletionPolicy::VersionedPush),
            ],
            vec![first_transport, second_transport],
        );
        let shared = item(0, 0, 2, 1, "both servers report this");
        let first = pull_server(
            first_mock,
            json!([shared, item(0, 3, 7, 2, "only the pull server")]),
        );
        let second = push_server(
            second_mock,
            json!([shared, item(1, 0, 0, 3, "only the push server")]),
        );

        let report = ready(session.ask(request(1)).await);

        assert_eq!(report.servers().len(), 2);
        assert_eq!(report.servers()[0].server, ServerId::new(0));
        assert_eq!(report.servers()[1].server, ServerId::new(1));
        let messages: Vec<&str> = report
            .diagnostics()
            .iter()
            .map(|reported| reported.diagnostic.message.as_str())
            .collect();
        assert_eq!(
            messages,
            [
                "both servers report this",
                "only the pull server",
                "only the push server",
            ],
            "the merge sorts by severity and removes the duplicate once"
        );
        session.close().await;
        first.abort();
        second.abort();
    }

    #[tokio::test]
    async fn the_related_information_bound_refuses_a_longer_list() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let related = json!({
            "location": {
                "uri": DOCUMENT_URI,
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 2 },
                },
            },
            "message": "also here",
        });
        let mut diagnostic = item(0, 0, 2, 1, "one error");
        diagnostic["relatedInformation"] = json!([related, related]);
        let server = pull_server(mock, json!([diagnostic]));

        let report = ready(
            session
                .ask(request(1).limits(DiagnosticsLimits::default().related_information(1)))
                .await,
        );

        assert!(matches!(
            failure(&report),
            LspError::Bounds {
                measure: LspBound::RelatedInformation,
                ..
            }
        ));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn the_message_bound_refuses_a_longer_message() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let server = pull_server(mock, json!([item(0, 0, 2, 1, "a message above the bound")]));

        let report = ready(
            session
                .ask(request(1).limits(DiagnosticsLimits::default().message_bytes(4)))
                .await,
        );

        assert!(matches!(
            failure(&report),
            LspError::Bounds {
                measure: LspBound::DiagnosticMessageBytes,
                limit: 4,
                ..
            }
        ));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn the_traffic_bound_refuses_a_server_that_answers_too_much() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let items: Vec<Value> = (0..64)
            .map(|index| item(0, 0, 2, 1, &format!("diagnostic number {index}")))
            .collect();
        let server = pull_server(mock, json!(items));

        let report = ready(
            session
                .ask(request(1).limits(DiagnosticsLimits::default().protocol_bytes(128)))
                .await,
        );

        assert!(matches!(
            failure(&report),
            LspError::Bounds {
                measure: LspBound::RequestBytes,
                limit: 128,
                ..
            }
        ));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn the_text_bound_refuses_a_longer_document() {
        let hub = DiagnosticsHub::new();
        let long = "x".repeat(64);
        let request = ChangedFile::new(
            WorktreeRelativePath::new(DOCUMENT).expect("the path is relative"),
            long,
            DocumentRevision::FIRST,
            language(),
        )
        .limits(DiagnosticsLimits::default().text_bytes(16));

        let error = hub
            .changed_file(request)
            .await
            .expect_err("the text passes its bound");

        assert!(matches!(
            error,
            LspError::Bounds {
                measure: LspBound::DocumentBytes,
                limit: 16,
                actual: 64,
            }
        ));
    }

    #[tokio::test]
    async fn a_limit_above_the_bound_of_this_crate_refuses_the_request() {
        let hub = DiagnosticsHub::new();
        let request =
            request(1).limits(DiagnosticsLimits::default().per_server(LSP_DIAGNOSTICS_MAX + 1));

        let error = hub
            .changed_file(request)
            .await
            .expect_err("the limit passes the bound of this crate");

        assert!(matches!(
            error,
            LspError::Bounds {
                measure: LspBound::Diagnostics,
                limit: LSP_DIAGNOSTICS_MAX,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_language_that_no_server_declares_is_unsupported() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let server = pull_server(mock, json!([]));
        let other = LanguageId::new("zig").expect("the identifier is short");

        let outcome = session
            .ask(
                ChangedFile::new(
                    WorktreeRelativePath::new(DOCUMENT).expect("the path is relative"),
                    TEXT.to_owned(),
                    DocumentRevision::FIRST,
                    other,
                )
                .wait(WaitPolicy::Until(SHORT_DEADLINE)),
            )
            .await;

        assert!(matches!(outcome, DiagnosticsOutcome::Unsupported));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn a_server_without_a_safe_completion_reports_one_unsupported_outcome() {
        let (first_transport, first_mock) = pipe();
        let (second_transport, second_mock) = pipe();
        let session = Session::open(
            vec![
                declared(0, CompletionPolicy::Unsupported),
                // The declaration asks for a pull, and the server advertises no
                // diagnostic provider, so it answers no pull.
                declared(1, CompletionPolicy::Pull),
            ],
            vec![first_transport, second_transport],
        );
        let first = silent_server(first_mock, pull_capabilities());
        let second = silent_server(second_mock, push_capabilities());

        let report = ready(session.ask(request(1)).await);

        assert!(matches!(
            report.servers()[0].outcome,
            ServerOutcome::Unsupported
        ));
        assert!(matches!(
            report.servers()[1].outcome,
            ServerOutcome::Unsupported
        ));
        session.close().await;
        first.abort();
        second.abort();
    }

    #[tokio::test]
    async fn a_missing_program_reports_one_unavailable_server() {
        let (transport, mock) = pipe();
        let session = Session::open(
            vec![
                declared(0, CompletionPolicy::Pull),
                declared(1, CompletionPolicy::Pull),
            ],
            // An empty list of prepared transports reports the state of a
            // program that the system does not hold.
            vec![transport, TransportFactory::Prepared(Vec::new())],
        );
        let server = pull_server(mock, json!([item(0, 0, 2, 1, "one error")]));

        let report = ready(session.ask(request(1)).await);

        assert!(matches!(
            report.servers()[0].outcome,
            ServerOutcome::Ready { .. }
        ));
        assert!(matches!(
            report.servers()[1].outcome,
            ServerOutcome::Unavailable
        ));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn the_deadline_ends_a_request_that_no_server_answers() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let server = silent_server(mock, pull_capabilities());

        let outcome = session
            .ask(request(1).wait(WaitPolicy::Until(SHORT_DEADLINE)))
            .await;

        assert!(matches!(outcome, DiagnosticsOutcome::TimedOut));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn a_cancelled_request_returns_the_cancelled_outcome() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let server = silent_server(mock, pull_capabilities());
        let cancellation = CancellationToken::new();
        let owner = cancellation.clone();
        tokio::spawn(async move {
            time::sleep(Duration::from_millis(50)).await;
            owner.cancel();
        });

        let outcome = session.ask(request(1).cancellation(cancellation)).await;

        assert!(matches!(outcome, DiagnosticsOutcome::Cancelled));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn a_newer_revision_supersedes_the_running_one() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let server = silent_server(mock, pull_capabilities());
        let hub = &session.hub;

        let (older, newer) = tokio::join!(hub.changed_file(request(1)), async {
            time::sleep(Duration::from_millis(100)).await;
            hub.changed_file(
                request(2)
                    .revisions(RevisionPolicy::Supersede)
                    .wait(WaitPolicy::Until(SHORT_DEADLINE)),
            )
            .await
        });

        assert!(matches!(
            older.expect("the request holds every bound"),
            DiagnosticsOutcome::Superseded
        ));
        assert!(matches!(
            newer.expect("the request holds every bound"),
            DiagnosticsOutcome::TimedOut
        ));
        session.close().await;
        server.abort();
    }

    #[tokio::test]
    async fn a_queued_revision_waits_behind_the_running_one() {
        let (transport, mock) = pipe();
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);
        let server = silent_server(mock, pull_capabilities());
        let hub = &session.hub;

        let (older, newer) = tokio::join!(
            hub.changed_file(request(1).wait(WaitPolicy::Until(LONG_DEADLINE))),
            async {
                time::sleep(Duration::from_millis(50)).await;
                // The newer revision keeps the running one, so it never
                // installs and it ends on its own deadline instead.
                hub.changed_file(request(2).wait(WaitPolicy::Until(SHORT_DEADLINE)))
                    .await
            }
        );

        assert!(matches!(
            newer.expect("the request holds every bound"),
            DiagnosticsOutcome::TimedOut
        ));
        // The running revision never received the result of the newer one.
        assert!(matches!(
            older.expect("the request holds every bound"),
            DiagnosticsOutcome::TimedOut
        ));
        session.close().await;
        server.abort();
    }

    #[test]
    fn one_hub_refuses_a_server_declaration_that_passes_its_bounds() {
        let hub = DiagnosticsHub::new();
        assert!(hub.server(declared(0, CompletionPolicy::Pull)).is_ok());
        assert!(matches!(
            hub.server(declared(0, CompletionPolicy::Pull)),
            Err(LspError::DuplicateServer)
        ));

        let mut without = declared(1, CompletionPolicy::Pull);
        without.languages = Vec::new();
        assert!(matches!(
            hub.server(without),
            Err(LspError::Bounds {
                measure: LspBound::Languages,
                actual: 0,
                ..
            })
        ));

        let mut many = declared(2, CompletionPolicy::Pull);
        many.languages = (0..=LSP_SERVER_LANGUAGES_MAX)
            .map(|index| LanguageId::new(&format!("language{index}")).expect("it is short"))
            .collect();
        assert!(matches!(
            hub.server(many),
            Err(LspError::Bounds {
                measure: LspBound::Languages,
                ..
            })
        ));

        let mut named = declared(3, CompletionPolicy::Pull);
        named.source = "x".repeat(super::LSP_SERVER_SOURCE_BYTES_MAX + 1);
        assert!(matches!(
            hub.server(named),
            Err(LspError::Bounds {
                measure: LspBound::SourceBytes,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_request_over_a_real_child_leaves_no_untracked_process() {
        let marker = std::env::temp_dir().join(format!("kvim-lsp-child-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        // The child records its own identifier and then replaces itself, so the
        // recorded identifier names the process that the session must end.
        let script = format!("printf '%s' $$ > '{}'; exec sleep 600", marker.display());
        let transport = TransportFactory::Process {
            program: OsString::from(SHELL),
            args: vec![OsString::from("-c"), OsString::from(script)],
            root: PathBuf::from("/"),
        };
        let session = Session::open(vec![declared(0, CompletionPolicy::Pull)], vec![transport]);

        // The child answers no handshake, so the request reaches its deadline.
        let outcome = session
            .ask(request(1).wait(WaitPolicy::Until(SHORT_DEADLINE)))
            .await;
        assert!(matches!(outcome, DiagnosticsOutcome::TimedOut));
        let pid = recorded_pid(&marker).await;

        session.close().await;

        let deadline = time::Instant::now() + TEST_DEADLINE;
        while is_running(pid).await {
            assert!(
                time::Instant::now() < deadline,
                "the closed project left child {pid} running"
            );
            time::sleep(Duration::from_millis(10)).await;
        }
        let _ = std::fs::remove_file(&marker);
    }

    /// Waits until the child recorded its process identifier.
    async fn recorded_pid(marker: &Path) -> u32 {
        let deadline = time::Instant::now() + TEST_DEADLINE;
        loop {
            if let Ok(text) = std::fs::read_to_string(marker)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                return pid;
            }
            assert!(
                time::Instant::now() < deadline,
                "the child recorded no process identifier"
            );
            time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Reports whether the system still holds one process identifier.
    async fn is_running(pid: u32) -> bool {
        Command::new(SHELL)
            .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
            .status()
            .await
            .expect("the probe runs")
            .success()
    }
}
