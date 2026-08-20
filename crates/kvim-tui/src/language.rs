//! The editor side of the language-server wiring.
//!
//! The session builds one bounded request and never sends it. The event loop
//! takes the request and hands it to [`LanguageServices`], which owns the
//! process, the deadlines, and the protocol bounds. Every published result
//! passes the buffer-version gate before it changes visible state, so an
//! obsolete answer never reaches the screen.
//!
//! Nothing in this file names a language or a server product. A missing server
//! and a stopped server are normal states: the editor reports each one once and
//! stays fully usable. See `docs/language-services.md` and
//! `docs/responsiveness.md`.
//!
//! [`LanguageServices`]: kvim_language::LanguageServices

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kvim_core::BufferVersion;
use kvim_language::{
    ContentChange, Diagnostic, DiagnosticSet, DiagnosticSeverity, DocumentPosition, FormatEdits,
    FormatterRequest, LANGUAGE_SERVERS_MAX, LanguageFormatter, LanguageRegistry, LanguageRequestId,
    LanguageServerHandle, LanguageServerId, LspError, MarkupDocument, MarkupKind, MarkupText,
    ServerFormatting, SourceLocation,
};
use kvim_workspace::BufferId;

/// The requests that the session holds for the event loop.
///
/// The event loop drains the queue after every step, so the queue holds one
/// burst of one transition. A full queue means the loop stopped draining, and
/// the session then opens every document again instead of sending a change that
/// describes text the server never received.
pub const LANGUAGE_OUTBOX_MAX: usize = 64;

/// The rows that one floating overlay shows below its title.
///
/// The float sits beside the cursor, so it covers buffer text that the reader
/// still needs. The bound keeps one long hover text or one long diagnostic from
/// filling the window. The overlay replaces the last row with a note when it
/// holds more rows than the bound allows.
pub const FLOAT_ROWS_MAX: usize = 16;

/// The terminal cells that one floating overlay row occupies.
///
/// A wrapped row stays readable at this width, and the float still leaves the
/// buffer text beside it visible in a wide terminal. A narrower window wraps at
/// its own width instead, because the float never reaches outside the window.
pub const FLOAT_COLUMNS_MAX: usize = 96;

/// The rows that one float keeps before the overlay wraps them.
///
/// The overlay shows at most [`FLOAT_ROWS_MAX`] rows and replaces the last one
/// with a note as soon as it holds more, so one extra row is enough to report
/// that the float hides content.
const FLOAT_SOURCE_ROWS_MAX: usize = FLOAT_ROWS_MAX + 1;

/// The characters that one source row of a float keeps.
///
/// The overlay shows at most [`FLOAT_ROWS_MAX`] rows of [`FLOAT_COLUMNS_MAX`]
/// cells, so no character beyond this bound can ever become visible text. The
/// bound keeps one pathological message small before any wrapping runs.
const FLOAT_SOURCE_CHARS_MAX: usize = FLOAT_ROWS_MAX * FLOAT_COLUMNS_MAX;

/// One document synchronization or one question for the language services.
///
/// The session builds the value and the event loop sends it, so the session
/// performs no protocol work.
#[derive(Debug)]
pub enum LanguageRequest {
    /// Open one document with the exact text of one buffer version.
    Open {
        /// The buffer that the document belongs to.
        buffer: BufferId,
        /// The path of the document.
        path: PathBuf,
        /// The buffer version of the text.
        version: BufferVersion,
        /// The exact text of that buffer version.
        text: Arc<str>,
    },
    /// Synchronize one applied edit transaction.
    Change {
        /// The buffer that the document belongs to.
        buffer: BufferId,
        /// The path of the document.
        path: PathBuf,
        /// The buffer version that the transaction produced.
        version: BufferVersion,
        /// The changes of that transaction, in descending order.
        changes: Vec<ContentChange>,
    },
    /// Close one document.
    Close {
        /// The path of the document.
        path: PathBuf,
    },
    /// Ask one question about one buffer version.
    Query {
        /// The buffer that the question asks about.
        buffer: BufferId,
        /// The path of the document.
        path: PathBuf,
        /// The buffer version that the question asks about.
        version: BufferVersion,
        /// The question.
        query: LanguageQuery,
    },
}

impl LanguageRequest {
    /// Returns the document that the request names.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Open { path, .. }
            | Self::Change { path, .. }
            | Self::Close { path }
            | Self::Query { path, .. } => path,
        }
    }

    /// Returns the request class, which decides how a refusal is reported.
    #[must_use]
    pub const fn kind(&self) -> LanguageRequestKind {
        match self {
            Self::Open { .. } | Self::Change { .. } | Self::Close { .. } => {
                LanguageRequestKind::Synchronization
            }
            Self::Query { .. } => LanguageRequestKind::Query,
        }
    }

    /// Returns the buffer that the request describes.
    ///
    /// A close names one path alone, because the editor no longer holds that
    /// document at that path. No fresh open can therefore repair the copy of a
    /// refused close. See `docs/language-services.md`.
    #[must_use]
    pub(super) const fn buffer(&self) -> Option<BufferId> {
        match self {
            Self::Open { buffer, .. }
            | Self::Change { buffer, .. }
            | Self::Query { buffer, .. } => Some(*buffer),
            Self::Close { .. } => None,
        }
    }

    /// Reports whether this request carries a change of one buffer.
    ///
    /// A fresh open carries the complete text, so it supersedes every such
    /// request of the same buffer. The mode of the server decides what one
    /// change notification carries, and the editor reads no mode.
    fn changes_buffer(&self, buffer: BufferId) -> bool {
        matches!(self, Self::Change { buffer: owner, .. } if *owner == buffer)
    }
}

/// The question that one query asks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageQuery {
    /// Resolve the definition at one position.
    Definition(DocumentPosition),
    /// Describe the symbol at one position.
    Hover(DocumentPosition),
    /// Format the complete document.
    Format,
}

/// One question that one server accepted.
///
/// The dispatch produces one value for each server that took the question, in
/// declaration order. The answer of that server later carries the same request
/// identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptedQuery {
    /// The server that the question reached.
    pub server: LanguageServerId,
    /// The identity that the session assigned.
    pub request: LanguageRequestId,
}

/// Sends one request to every session that serves its document.
///
/// The call returns at once, so the terminal event loop never reads, writes, or
/// waits for a server. One language can run several servers, so a question
/// reaches each of them and answers with one identity for each. A formatting
/// request reaches the one declared server that formats.
///
/// One server that refuses the request leaves the remaining servers serving.
/// The call fails only when no server took the request at all.
///
/// # Errors
///
/// Returns [`LspError::NoServerDeclared`] when no running server serves this
/// request, [`LspError::Saturated`] when every request queue is full, and
/// [`LspError::Stopped`] after every session stopped.
pub(super) fn send_request(
    handles: &[&LanguageServerHandle],
    request: &LanguageRequest,
) -> Result<Vec<AcceptedQuery>, LspError> {
    let mut accepted = Vec::with_capacity(handles.len());
    let mut served = 0_usize;
    let mut refused = None;
    for handle in handles {
        if !serves(handle, request) {
            continue;
        }
        match send_one(handle, request) {
            Ok(Some(id)) => {
                served += 1;
                accepted.push(AcceptedQuery {
                    server: handle.id(),
                    request: id,
                });
            }
            Ok(None) => served += 1,
            // The first refusal names the state that the message line reports,
            // because a later refusal describes the same lost request.
            Err(error) => refused = refused.or(Some(error)),
        }
    }
    if served > 0 {
        return Ok(accepted);
    }
    // No server took the request. A refusal names its typed reason, and no
    // refusal at all means that no running server serves this request.
    Err(refused.unwrap_or(LspError::NoServerDeclared))
}

/// Reports whether one server receives one request.
///
/// Exactly one declared server of one adapter formats, so a formatting request
/// reaches that server alone. Every other request reaches every running server.
fn serves(handle: &LanguageServerHandle, request: &LanguageRequest) -> bool {
    match request {
        LanguageRequest::Query {
            query: LanguageQuery::Format,
            ..
        } => handle.formatting() == ServerFormatting::Enabled,
        LanguageRequest::Open { .. }
        | LanguageRequest::Change { .. }
        | LanguageRequest::Close { .. }
        | LanguageRequest::Query { .. } => true,
    }
}

/// Sends one request to one session.
fn send_one(
    handle: &LanguageServerHandle,
    request: &LanguageRequest,
) -> Result<Option<LanguageRequestId>, LspError> {
    match request {
        LanguageRequest::Open {
            path,
            version,
            text,
            ..
        } => handle.open(path, *version, Arc::clone(text)).map(|()| None),
        LanguageRequest::Change {
            path,
            version,
            changes,
            ..
        } => handle
            .change(path, *version, changes.clone())
            .map(|()| None),
        LanguageRequest::Close { path } => handle.close(path).map(|()| None),
        LanguageRequest::Query {
            path,
            version,
            query,
            ..
        } => match *query {
            LanguageQuery::Definition(position) => {
                handle.definition(path, *version, position).map(Some)
            }
            LanguageQuery::Hover(position) => handle.hover(path, *version, position).map(Some),
            LanguageQuery::Format => handle.format(path, *version).map(Some),
        },
    }
}

/// The class of one dispatched request.
///
/// A refused synchronization leaves no caller waiting. A refused query must
/// release the question that the editor waits for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageRequestKind {
    /// One document synchronization.
    Synchronization,
    /// One question about one buffer version.
    Query,
}

/// The step that follows one successful save.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AfterSave {
    /// Keep the window open.
    Stay,
    /// Close the focused window, like `:wq`.
    CloseWindow,
}

/// Why the editor asked one question.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QueryPurpose {
    /// Move the cursor to the definition of the symbol under it.
    Definition,
    /// Show the hover text of the symbol under the cursor.
    Hover,
    /// Format the buffer, then save it and run the step after the save.
    FormatBeforeSave(AfterSave),
}

/// The answer that one server gave to one question.
#[derive(Debug)]
pub(super) enum Answer {
    /// The definition targets inside the workspace root, in answer order.
    Definition(Vec<SourceLocation>),
    /// The hover text of the symbol under the cursor, and its markup.
    Hover(MarkupText),
    /// The accepted formatting edits of one buffer version.
    Formatting(FormatEdits),
    /// The server produced no value for this question.
    ///
    /// A failure, a timeout, a missing server, and a stopped session all reach
    /// this state, so no question waits for a server that answers nothing.
    Empty,
}

/// One server that a question reached, and the answer that it gave.
#[derive(Debug)]
struct AnswerSlot {
    /// The server that the question reached.
    server: LanguageServerId,
    /// The identity that the session assigned.
    request: LanguageRequestId,
    /// The answer, or `None` while the server still owes one.
    answer: Option<Answer>,
}

/// The servers that one question reached.
#[derive(Debug)]
enum QueryDispatch {
    /// The editor queued the question, and the event loop has not sent it yet.
    Queued,
    /// The dispatch named every server that accepted the question, in
    /// declaration order.
    Accepted(Vec<AnswerSlot>),
}

/// Whether one question still waits for an answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QueryState {
    /// At least one server still owes an answer.
    Waiting,
    /// Every server answered, so the merged answer is ready.
    Complete,
}

/// The question that the editor waits for.
///
/// One question runs at a time, so no answer can reach a buffer state that a
/// newer question already replaced. The question reaches every running server
/// of its language, and it holds one slot for each of them. The merge rules of
/// `docs/language-services.md` read the slots in declaration order, so the
/// merged answer never depends on which server answers first.
#[derive(Debug)]
pub(super) struct PendingQuery {
    /// The buffer that the question asks about.
    pub(super) buffer: BufferId,
    /// The buffer version that the question asks about.
    pub(super) version: BufferVersion,
    /// Why the editor asked.
    pub(super) purpose: QueryPurpose,
    /// The servers that the question reached.
    dispatch: QueryDispatch,
}

impl PendingQuery {
    /// Creates one question that waits for its dispatch.
    pub(super) const fn new(
        buffer: BufferId,
        version: BufferVersion,
        purpose: QueryPurpose,
    ) -> Self {
        Self {
            buffer,
            version,
            purpose,
            dispatch: QueryDispatch::Queued,
        }
    }

    /// Records the servers that accepted the question.
    pub(super) fn accept(&mut self, accepted: Vec<AcceptedQuery>) {
        debug_assert!(
            matches!(self.dispatch, QueryDispatch::Queued),
            "the event loop dispatches one question exactly once"
        );
        self.dispatch = QueryDispatch::Accepted(
            accepted
                .into_iter()
                .map(|query| AnswerSlot {
                    server: query.server,
                    request: query.request,
                    answer: None,
                })
                .collect(),
        );
    }

    /// Reports whether one answer belongs to this question.
    pub(super) fn owns(&self, request: LanguageRequestId) -> bool {
        self.slots()
            .is_some_and(|slots| slots.iter().any(|slot| slot.request == request))
    }

    /// Records one answer, and reports whether every server answered.
    pub(super) fn resolve(&mut self, request: LanguageRequestId, answer: Answer) -> QueryState {
        if let QueryDispatch::Accepted(slots) = &mut self.dispatch
            && let Some(slot) = slots.iter_mut().find(|slot| slot.request == request)
            && slot.answer.is_none()
        {
            slot.answer = Some(answer);
        }
        self.state()
    }

    /// Records that one server answers nothing further.
    ///
    /// A missing server, a stopped session, and a session failure carry no
    /// request identity, so they release every slot of that server at once.
    pub(super) fn abandon(&mut self, server: LanguageServerId) -> QueryState {
        if let QueryDispatch::Accepted(slots) = &mut self.dispatch {
            for slot in slots.iter_mut().filter(|slot| slot.server == server) {
                slot.answer.get_or_insert(Answer::Empty);
            }
        }
        self.state()
    }

    /// Returns the merged definition targets of the first server that found
    /// one.
    pub(super) fn definition(&self) -> &[SourceLocation] {
        self.answers()
            .find_map(|answer| match answer {
                Answer::Definition(locations) if !locations.is_empty() => Some(&locations[..]),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Returns the hover answers of every server, in declaration order.
    ///
    /// Each answer carries its own markup kind, because two servers of one
    /// language answer on their own. The caller joins the answers, and an empty
    /// list means that no server described the symbol.
    pub(super) fn hover(&self) -> Vec<&MarkupText> {
        self.answers()
            .filter_map(|answer| match answer {
                Answer::Hover(markup) => Some(markup),
                _ => None,
            })
            .collect()
    }

    /// Returns the formatting edits of the one server that formats.
    pub(super) fn formatting(&self) -> Option<&FormatEdits> {
        self.answers().find_map(|answer| match answer {
            Answer::Formatting(edits) => Some(edits),
            _ => None,
        })
    }

    /// Returns the slots, or `None` while the question waits for its dispatch.
    const fn slots(&self) -> Option<&Vec<AnswerSlot>> {
        match &self.dispatch {
            QueryDispatch::Queued => None,
            QueryDispatch::Accepted(slots) => Some(slots),
        }
    }

    /// Returns the answers that arrived, in declaration order.
    fn answers(&self) -> impl Iterator<Item = &Answer> {
        self.slots()
            .into_iter()
            .flatten()
            .filter_map(|slot| slot.answer.as_ref())
    }

    /// Reports whether every server that took the question answered.
    fn state(&self) -> QueryState {
        match self.slots() {
            Some(slots) if slots.iter().all(|slot| slot.answer.is_some()) => QueryState::Complete,
            Some(_) | None => QueryState::Waiting,
        }
    }
}

/// One external format that a save waits for.
///
/// The session builds the run and never starts the program. The event loop
/// takes the run and hands it to the bounded process service, so no formatter
/// ever runs on that loop. See `docs/responsiveness.md`.
#[derive(Debug)]
pub(super) struct PendingFormat {
    /// The buffer that the formatter formats.
    pub(super) buffer: BufferId,
    /// The step that follows the save.
    pub(super) then: AfterSave,
    /// How far the run progressed.
    stage: FormatStage,
}

/// How far one external format progressed.
///
/// The run and the stage are one value, so no state can name a queued run that
/// the event loop already took.
#[derive(Debug)]
enum FormatStage {
    /// The run waits for the event loop.
    Queued(FormatterRequest),
    /// The bounded process service runs the formatter.
    Running,
}

/// The definition target that waits for its document to load.
#[derive(Debug)]
pub(super) struct PendingJump {
    /// The document that holds the target.
    pub(super) path: PathBuf,
    /// The position of the target inside that document.
    pub(super) position: DocumentPosition,
}

/// One normal language-service state that the editor reports once.
///
/// None of these states is a failure. The editor stays fully usable, and it
/// never repeats the report. See `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum LanguageNotice {
    /// No adapter serves the path, or the adapter declares no server.
    NoServer,
    /// This workspace uses no declared server of the path.
    ///
    /// The state differs from [`LanguageNotice::NotInstalled`]. The server was
    /// never meant to run in this workspace, and it therefore never started.
    UnusedInWorkspace,
    /// The declared server is not installed on this system.
    NotInstalled,
    /// The session stopped and accepts no further request.
    Stopped,
}

impl LanguageNotice {
    /// Returns the normal state of one failure, or `None` for a real failure.
    #[must_use]
    pub(super) const fn of(error: &LspError) -> Option<Self> {
        match error {
            LspError::UnsupportedPath | LspError::NoServerDeclared => Some(Self::NoServer),
            LspError::UnusedInWorkspace => Some(Self::UnusedInWorkspace),
            LspError::NotInstalled => Some(Self::NotInstalled),
            LspError::Stopped => Some(Self::Stopped),
            _ => None,
        }
    }

    /// Returns the message that the message line shows once.
    #[must_use]
    pub(super) const fn message(self) -> &'static str {
        match self {
            Self::NoServer => "no language server serves this buffer",
            Self::UnusedInWorkspace => {
                "this workspace uses no language server for this buffer; editing continues"
            }
            Self::NotInstalled => "no language server is installed; editing continues without one",
            Self::Stopped => "the language server stopped; editing continues without it",
        }
    }
}

/// What one refused language request leaves behind.
///
/// A running session holds a copy of every document that it opened. A refusal
/// of such a session therefore leaves that copy behind the buffer, and the
/// editor must open the document again. See `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Refusal {
    /// A running session dropped the request, so its copy is behind the buffer.
    CopyDrifted,
    /// No running session took the request, so no session holds a copy.
    NoCopyHeld,
}

impl Refusal {
    /// Returns what one refusal of one dispatched request leaves behind.
    ///
    /// A full request queue is the one refusal that a running session produces.
    /// Every other state names a path that no session serves, a process that
    /// never started, or a session that stopped.
    #[must_use]
    pub(super) const fn of(error: &LspError) -> Self {
        match error {
            LspError::Saturated => Self::CopyDrifted,
            _ => Self::NoCopyHeld,
        }
    }
}

/// Returns the formatter that formats one document.
///
/// An external formatter takes precedence over a formatting server, so the
/// adapter of the path decides which path a format-on-save runs. A buffer
/// without a file name and a path that no adapter owns have no formatter. The
/// answer is adapter data alone, and every caller derives it again instead of
/// storing it, because a stored copy could disagree with the adapter table.
///
/// Whether a declared server or a declared program is installed, running, or
/// stopped is a separate runtime state that [`LanguageNotice`] reports.
pub(super) fn formatter(
    languages: LanguageRegistry,
    path: Option<&Path>,
) -> Option<LanguageFormatter> {
    languages.adapter(path?).ok()?.formatter()
}

/// Reports whether a formatter can format one document.
///
/// The answer covers both paths: an adapter that declares an external formatter
/// and an adapter that declares a formatting server can both format a buffer.
pub(super) fn has_formatter(languages: LanguageRegistry, path: Option<&Path>) -> bool {
    formatter(languages, path).is_some()
}

/// Whether one buffer formats through its language server before a save.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatOnSave {
    /// Format the buffer before every save.
    Enabled,
    /// Save the buffer as it is.
    Disabled,
}

impl FormatOnSave {
    /// Returns the state that a buffer holds before a toggle changes it.
    ///
    /// `EditorSettings` records the default as one flag, so this is the one
    /// boundary that turns that flag into the typed state.
    #[must_use]
    pub const fn from_setting(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }

    /// Returns the state that a toggle reaches.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Enabled => Self::Disabled,
            Self::Disabled => Self::Enabled,
        }
    }

    /// Returns the short label that the statusline shows.
    ///
    /// The label names the state of the focused buffer, so it stays short
    /// enough for one narrow statusline. See `docs/windows.md`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Enabled => "fmt:on",
            Self::Disabled => "fmt:off",
        }
    }

    /// Returns the message that reports the state of the active buffer.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Enabled => "format-on-save is on for this buffer",
            Self::Disabled => "format-on-save is off for this buffer",
        }
    }
}

/// The direction of one diagnostic jump.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticJump {
    /// Move to the next diagnostic, and wrap after the last one.
    Next,
    /// Move to the previous diagnostic, and wrap before the first one.
    Previous,
}

/// Returns the position that one diagnostic jump reaches.
///
/// The diagnostics ascend by position, so the search is deterministic. The jump
/// wraps at both ends, and an empty set reaches no position.
#[must_use]
pub(super) fn jump_target(
    diagnostics: &[Diagnostic],
    cursor: DocumentPosition,
    jump: DiagnosticJump,
) -> Option<DocumentPosition> {
    let found = match jump {
        DiagnosticJump::Next => diagnostics
            .iter()
            .find(|diagnostic| diagnostic.span.start > cursor)
            .or_else(|| diagnostics.first()),
        DiagnosticJump::Previous => diagnostics
            .iter()
            .rev()
            .find(|diagnostic| diagnostic.span.start < cursor)
            .or_else(|| diagnostics.last()),
    };
    found.map(|diagnostic| diagnostic.span.start)
}

/// One row of a floating overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FloatRow {
    /// The text of the row. A source row holds at most
    /// [`FLOAT_SOURCE_CHARS_MAX`] characters, and the overlay wraps it into
    /// rows of at most [`FLOAT_COLUMNS_MAX`] terminal cells.
    pub(super) text: String,
    /// The severity that colors the row, or `None` for neutral text.
    pub(super) severity: Option<DiagnosticSeverity>,
}

/// What one floating overlay shows.
///
/// A markup document holds roles and structure, and a plain text holds neither.
/// The two therefore stay apart until the overlay paints them, because a
/// markdown parse of a plain text would remove the characters that mark up a
/// document. See `docs/language-services.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum FloatContent {
    /// Rows of plain text, which the overlay wraps at its own width.
    Text(Vec<FloatRow>),
    /// One markup document, which the overlay renders at its own width.
    Markup(MarkupDocument),
}

/// One floating overlay of the language services.
///
/// The float is decoration. It changes no buffer text, no line mapping, and no
/// cursor position, and the next key closes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Float {
    /// The title band of the overlay.
    pub(super) title: &'static str,
    /// The content of the overlay, bounded by [`FLOAT_ROWS_MAX`] rows as soon
    /// as the overlay knows its width.
    pub(super) content: FloatContent,
}

impl Float {
    /// Creates a bounded float from one plain text.
    #[must_use]
    pub(super) fn text(title: &'static str, text: &str) -> Self {
        let rows = text
            .lines()
            .take(FLOAT_SOURCE_ROWS_MAX)
            .map(|line| FloatRow {
                text: clip(line),
                severity: None,
            })
            .collect();
        Self {
            title,
            content: FloatContent::Text(rows),
        }
    }

    /// Creates a bounded float from the hover answers of every server.
    ///
    /// The answers join in declaration order, and one blank row separates two
    /// of them. Each answer of markdown carries its own document, because the
    /// code of a fence takes the highlight of its language where the answer
    /// arrives, so the float joins documents and never a markdown text.
    ///
    /// One answer of plain text makes the whole float plain text, because a
    /// markdown parse of a plain text loses the characters that mark up a
    /// document. The float then joins the texts of every answer.
    ///
    /// The float parses nothing. `kvim-language` names every block, every role,
    /// and every highlight span before the answer reaches this layer, so the
    /// terminal event loop paints a finished value.
    #[must_use]
    pub(super) fn hover(title: &'static str, answers: &[&MarkupText]) -> Self {
        if answers
            .iter()
            .all(|answer| answer.kind == MarkupKind::Markdown)
        {
            let document = answers
                .iter()
                .fold(MarkupDocument::default(), |joined, answer| {
                    joined.joined(&answer.document)
                });
            return Self {
                title,
                content: FloatContent::Markup(document),
            };
        }

        let joined = answers
            .iter()
            .map(|answer| answer.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        Self::text(title, &joined)
    }

    /// Reports whether the float holds no content at all.
    #[must_use]
    pub(super) fn is_empty(&self) -> bool {
        match &self.content {
            FloatContent::Text(rows) => rows.is_empty(),
            FloatContent::Markup(document) => document.is_empty(),
        }
    }

    /// Reports whether one bound already dropped content of the float.
    ///
    /// The overlay ends such a float with its clip note, exactly as it ends one
    /// that holds more rows than it shows.
    #[must_use]
    pub(super) fn is_clipped(&self) -> bool {
        match &self.content {
            FloatContent::Text(_) => false,
            FloatContent::Markup(document) => document.is_clipped(),
        }
    }

    /// Creates a bounded float from the diagnostics at one position.
    ///
    /// One position can carry several diagnostics, and the float shows every
    /// one of them. A blank row separates two diagnostics, so a reader sees
    /// where one message ends and the next one starts.
    ///
    /// `source` decides whether each message names the server that reported it.
    /// The caller reads that state from the buffer, so one diagnostic never
    /// gains or loses its name while the cursor moves.
    #[must_use]
    pub(super) fn diagnostics(
        title: &'static str,
        diagnostics: &[&Diagnostic],
        source: DiagnosticSource,
    ) -> Self {
        let rows = diagnostics
            .iter()
            .enumerate()
            .flat_map(|(position, diagnostic)| {
                let severity = diagnostic.severity;
                let name = match source {
                    DiagnosticSource::Shown => diagnostic.source.as_str(),
                    DiagnosticSource::Hidden => "",
                };
                let prefix = if name.is_empty() {
                    String::new()
                } else {
                    format!("{name}: ")
                };
                let separator = (position > 0).then(|| FloatRow {
                    text: String::new(),
                    severity: None,
                });
                separator.into_iter().chain(
                    diagnostic
                        .message
                        .lines()
                        .enumerate()
                        .map(move |(index, line)| FloatRow {
                            text: if index == 0 {
                                clip(&format!("{prefix}{line}"))
                            } else {
                                clip(line)
                            },
                            severity: Some(severity),
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .take(FLOAT_SOURCE_ROWS_MAX)
            .collect();
        Self {
            title,
            content: FloatContent::Text(rows),
        }
    }
}

/// Clips one source row to [`FLOAT_SOURCE_CHARS_MAX`] characters.
fn clip(line: &str) -> String {
    line.chars().take(FLOAT_SOURCE_CHARS_MAX).collect()
}

/// Whether the diagnostic float names the producer of each diagnostic.
///
/// The state belongs to one buffer, never to one cursor position. Every
/// diagnostic of one buffer therefore names its producer, or none of them does.
/// See `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum DiagnosticSource {
    /// The buffer carries more than one producer name, so each message names
    /// the producer that reported it.
    Shown,
    /// The buffer carries one producer name, so a name would repeat on every
    /// row without telling the reader anything.
    #[default]
    Hidden,
}

/// The diagnostics of one buffer, from every server that describes it.
///
/// The map holds the newest accepted set of each server, and the merged list is
/// the view that the float, the markers, and the navigation read. Only the
/// servers of one adapter describe one buffer, so the map holds at most
/// [`LANGUAGE_SERVERS_MAX`] sets.
#[derive(Debug, Default)]
struct BufferDiagnostics {
    /// The newest accepted set of each server, in declaration order.
    servers: BTreeMap<LanguageServerId, DiagnosticSet>,
    /// The merged list, in ascending position order.
    merged: Vec<Diagnostic>,
}

impl BufferDiagnostics {
    /// Replaces the set of one server and merges the sets again.
    fn publish(&mut self, server: LanguageServerId, set: DiagnosticSet) {
        self.servers.insert(server, set);
        debug_assert!(
            self.servers.len() <= LANGUAGE_SERVERS_MAX,
            "only the servers of one adapter describe one buffer"
        );
        self.merged = merge(&self.servers);
    }

    /// Reports whether the float names the producer of each diagnostic.
    ///
    /// The count reads the producer names of the merged list, not the servers
    /// that reported them. One server reports under more than one name when it
    /// separates its own tools, and the reader then needs each name.
    ///
    /// A name that is empty names nothing, so it never turns the other names
    /// on. The protocol parser substitutes the declaration identifier for an
    /// absent or empty `source` field, so only a diagnostic that another
    /// producer builds can carry one.
    fn naming(&self) -> DiagnosticSource {
        let mut first: Option<&str> = None;
        for name in self
            .merged
            .iter()
            .map(|diagnostic| diagnostic.source.as_str())
            .filter(|name| !name.is_empty())
        {
            match first {
                None => first = Some(name),
                Some(seen) if seen == name => {}
                Some(_) => return DiagnosticSource::Shown,
            }
        }
        DiagnosticSource::Hidden
    }
}

/// Merges the diagnostics of every server of one buffer.
///
/// Two diagnostics describe the same problem when their range and their message
/// text are both identical. The map orders the servers by declaration, so the
/// merge keeps the diagnostic of the earlier declaration and drops the later
/// duplicate. The result ascends by position, which keeps diagnostic navigation
/// deterministic. See `docs/language-services.md`.
fn merge(servers: &BTreeMap<LanguageServerId, DiagnosticSet>) -> Vec<Diagnostic> {
    let mut seen = BTreeSet::new();
    let mut merged: Vec<Diagnostic> = servers
        .values()
        .flat_map(DiagnosticSet::diagnostics)
        .filter(|diagnostic| {
            seen.insert((
                diagnostic.span.start,
                diagnostic.span.end,
                diagnostic.message.as_str(),
            ))
        })
        .cloned()
        .collect();
    // The sort is stable, so two diagnostics that share the complete key keep
    // the declaration order of their servers.
    merged.sort_by(|left, right| {
        (left.span.start, left.span.end, left.severity)
            .cmp(&(right.span.start, right.span.end, right.severity))
            .then_with(|| left.message.cmp(&right.message))
    });
    merged
}

/// The language-service state that the session owns.
///
/// The state holds only bounded collections: the outbox, the buffers that need
/// a fresh open, the newest diagnostics of every buffer and server, and the
/// per-buffer format-on-save overrides. The buffer list bounds every map.
#[derive(Debug, Default)]
pub(super) struct LanguageState {
    /// The requests that wait for the event loop.
    outbox: VecDeque<LanguageRequest>,
    /// The buffers that need a fresh open, after a restart or a lost change.
    resync: BTreeSet<BufferId>,
    /// The question that the editor waits for.
    pub(super) pending: Option<PendingQuery>,
    /// The normal states that the editor already reported.
    reported: BTreeSet<LanguageNotice>,
    /// The newest accepted diagnostics of every buffer and server.
    diagnostics: BTreeMap<BufferId, BufferDiagnostics>,
    /// The buffers whose format-on-save state differs from the settings.
    format_on_save: BTreeMap<BufferId, FormatOnSave>,
    /// The definition target that waits for its document to load.
    pub(super) jump: Option<PendingJump>,
    /// The external format that a save waits for.
    format: Option<PendingFormat>,
}

impl LanguageState {
    /// Queues one request, and reports whether the outbox accepted it.
    pub(super) fn queue(&mut self, request: LanguageRequest) -> bool {
        if self.outbox.len() >= LANGUAGE_OUTBOX_MAX {
            return false;
        }
        self.outbox.push_back(request);
        true
    }

    /// Takes the next request that the event loop must send.
    ///
    /// A buffer that needs a fresh open comes first, and its request carries the
    /// exact text of the current buffer version.
    pub(super) fn take(&mut self) -> Option<BufferId> {
        let buffer = *self.resync.iter().next()?;
        self.resync.remove(&buffer);
        Some(buffer)
    }

    /// Takes the next queued request.
    pub(super) fn take_queued(&mut self) -> Option<LanguageRequest> {
        self.outbox.pop_front()
    }

    /// Opens one buffer again and drops its queued changes.
    ///
    /// The open carries the complete buffer text, so every queued change of
    /// that buffer describes text that the server never receives.
    pub(super) fn mark_resync(&mut self, buffer: BufferId) {
        self.outbox
            .retain(|request| !request.changes_buffer(buffer));
        self.resync.insert(buffer);
    }

    /// Reports whether one buffer waits for a fresh open.
    pub(super) fn awaits_open(&self, buffer: BufferId) -> bool {
        self.resync.contains(&buffer)
    }

    /// Drops every queued request and opens the named buffers again.
    pub(super) fn resync_all(&mut self, buffers: impl IntoIterator<Item = BufferId>) {
        self.outbox.clear();
        self.resync.extend(buffers);
    }

    /// Forgets every record of one unloaded buffer.
    pub(super) fn forget(&mut self, buffer: BufferId) {
        self.outbox
            .retain(|request| !request.changes_buffer(buffer));
        self.resync.remove(&buffer);
        self.diagnostics.remove(&buffer);
        self.format_on_save.remove(&buffer);
    }

    /// Records one normal state, and reports whether it is new.
    pub(super) fn report(&mut self, notice: LanguageNotice) -> bool {
        self.reported.insert(notice)
    }

    /// Records one external format that a save waits for.
    pub(super) fn start_format(
        &mut self,
        buffer: BufferId,
        then: AfterSave,
        request: FormatterRequest,
    ) {
        debug_assert!(
            self.format.is_none(),
            "the session runs one format at a time, and a save checks that state first"
        );
        self.format = Some(PendingFormat {
            buffer,
            then,
            stage: FormatStage::Queued(request),
        });
    }

    /// Reports whether one external format waits for its answer.
    pub(super) const fn formats(&self) -> bool {
        self.format.is_some()
    }

    /// Takes the formatter run that the event loop must submit.
    pub(super) fn take_format_request(&mut self) -> Option<FormatterRequest> {
        let pending = self.format.as_mut()?;
        match std::mem::replace(&mut pending.stage, FormatStage::Running) {
            FormatStage::Queued(request) => Some(request),
            // The event loop already took the run, so it owns the answer.
            FormatStage::Running => None,
        }
    }

    /// Takes the external format that one answer completes.
    pub(super) fn take_format(&mut self) -> Option<PendingFormat> {
        self.format.take()
    }

    /// Publishes the diagnostics that one server reported for one buffer.
    ///
    /// The set replaces the previous set of that server alone, so a second
    /// server of the same language keeps the diagnostics that it reported.
    pub(super) fn publish(
        &mut self,
        buffer: BufferId,
        server: LanguageServerId,
        set: DiagnosticSet,
    ) {
        self.diagnostics
            .entry(buffer)
            .or_default()
            .publish(server, set);
    }

    /// Drops every published diagnostic.
    pub(super) fn clear_diagnostics(&mut self) {
        self.diagnostics.clear();
    }

    /// Drops the published diagnostics of every server of one buffer.
    ///
    /// A reload replaces the buffer text, so every published diagnostic of that
    /// buffer describes text that no longer exists.
    pub(super) fn forget_diagnostics(&mut self, buffer: BufferId) {
        self.diagnostics.remove(&buffer);
    }

    /// Returns the merged diagnostics of one buffer, in ascending position
    /// order.
    #[must_use]
    pub(super) fn diagnostics(&self, buffer: BufferId) -> &[Diagnostic] {
        self.diagnostics
            .get(&buffer)
            .map_or(&[][..], |published| &published.merged)
    }

    /// Reports whether the float names the producer of each diagnostic of one
    /// buffer.
    ///
    /// The answer reads the complete buffer, not the cursor position, so one
    /// diagnostic keeps its name while the cursor moves over the buffer.
    #[must_use]
    pub(super) fn diagnostic_naming(&self, buffer: BufferId) -> DiagnosticSource {
        self.diagnostics
            .get(&buffer)
            .map_or(DiagnosticSource::Hidden, BufferDiagnostics::naming)
    }

    /// Returns the format-on-save state of one buffer.
    ///
    /// A buffer without an override follows the settings default, so every new
    /// buffer starts with the configured behavior.
    #[must_use]
    pub(super) fn format_on_save(&self, buffer: BufferId, default: FormatOnSave) -> FormatOnSave {
        self.format_on_save.get(&buffer).copied().unwrap_or(default)
    }

    /// Records the format-on-save state of one buffer.
    pub(super) fn set_format_on_save(&mut self, buffer: BufferId, state: FormatOnSave) {
        self.format_on_save.insert(buffer, state);
    }
}
