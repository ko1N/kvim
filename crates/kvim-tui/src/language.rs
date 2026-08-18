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
    ContentChange, Diagnostic, DiagnosticSet, DiagnosticSeverity, DocumentPosition,
    LanguageRequestId, LanguageServerHandle, LspError,
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

    /// Reports whether this request synchronizes one buffer incrementally.
    ///
    /// A fresh open carries the complete text, so it supersedes every such
    /// request of the same buffer.
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

/// Sends one request to the session that serves its document.
///
/// The call returns at once, so the terminal event loop never reads, writes, or
/// waits for a server. A query answers with the identity that its later result
/// carries.
///
/// # Errors
///
/// Returns [`LspError::Saturated`] when the request queue of the session is
/// full, and [`LspError::Stopped`] after the session stopped.
pub(super) fn send_request(
    handle: &LanguageServerHandle,
    request: LanguageRequest,
) -> Result<Option<LanguageRequestId>, LspError> {
    match request {
        LanguageRequest::Open {
            path,
            version,
            text,
            ..
        } => handle.open(&path, version, text).map(|()| None),
        LanguageRequest::Change {
            path,
            version,
            changes,
            ..
        } => handle.change(&path, version, changes).map(|()| None),
        LanguageRequest::Close { path } => handle.close(&path).map(|()| None),
        LanguageRequest::Query {
            path,
            version,
            query,
            ..
        } => match query {
            LanguageQuery::Definition(position) => {
                handle.definition(&path, version, position).map(Some)
            }
            LanguageQuery::Hover(position) => handle.hover(&path, version, position).map(Some),
            LanguageQuery::Format => handle.format(&path, version).map(Some),
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

/// The question that the editor waits for.
///
/// One question runs at a time, so no answer can reach a buffer state that a
/// newer question already replaced.
#[derive(Debug)]
pub(super) struct PendingQuery {
    /// The buffer that the question asks about.
    pub(super) buffer: BufferId,
    /// The buffer version that the question asks about.
    pub(super) version: BufferVersion,
    /// Why the editor asked.
    pub(super) purpose: QueryPurpose,
    /// The identity that the services assigned, once they accepted it.
    pub(super) id: Option<LanguageRequestId>,
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
            Self::NotInstalled => "no language server is installed; editing continues without one",
            Self::Stopped => "the language server stopped; editing continues without it",
        }
    }
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

/// One floating overlay of the language services.
///
/// The float is decoration. It changes no buffer text, no line mapping, and no
/// cursor position, and the next key closes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Float {
    /// The title band of the overlay.
    pub(super) title: &'static str,
    /// The rows of the overlay, bounded by [`FLOAT_ROWS_MAX`].
    pub(super) rows: Vec<FloatRow>,
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
        Self { title, rows }
    }

    /// Creates a bounded float from the diagnostics at one position.
    ///
    /// One position can carry several diagnostics, and the float shows every
    /// one of them. A blank row separates two diagnostics, so a reader sees
    /// where one message ends and the next one starts.
    #[must_use]
    pub(super) fn diagnostics(title: &'static str, diagnostics: &[&Diagnostic]) -> Self {
        let rows = diagnostics
            .iter()
            .enumerate()
            .flat_map(|(position, diagnostic)| {
                let severity = diagnostic.severity;
                let source = diagnostic.source.as_deref().unwrap_or_default();
                let prefix = if source.is_empty() {
                    String::new()
                } else {
                    format!("{source}: ")
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
        Self { title, rows }
    }
}

/// Clips one source row to [`FLOAT_SOURCE_CHARS_MAX`] characters.
fn clip(line: &str) -> String {
    line.chars().take(FLOAT_SOURCE_CHARS_MAX).collect()
}

/// The language-service state that the session owns.
///
/// The state holds only bounded collections: the outbox, the buffers that need
/// a fresh open, the newest diagnostics of every buffer, and the per-buffer
/// format-on-save overrides. The buffer list bounds every map.
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
    /// The newest accepted diagnostics of every buffer.
    diagnostics: BTreeMap<BufferId, DiagnosticSet>,
    /// The buffers whose format-on-save state differs from the settings.
    format_on_save: BTreeMap<BufferId, FormatOnSave>,
    /// The definition target that waits for its document to load.
    pub(super) jump: Option<PendingJump>,
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

    /// Opens one buffer again and drops its queued incremental changes.
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

    /// Publishes the diagnostics of one buffer.
    pub(super) fn publish(&mut self, buffer: BufferId, set: DiagnosticSet) {
        self.diagnostics.insert(buffer, set);
    }

    /// Drops every published diagnostic.
    pub(super) fn clear_diagnostics(&mut self) {
        self.diagnostics.clear();
    }

    /// Returns the diagnostics of one buffer, in ascending position order.
    #[must_use]
    pub(super) fn diagnostics(&self, buffer: BufferId) -> &[Diagnostic] {
        self.diagnostics
            .get(&buffer)
            .map_or(&[][..], DiagnosticSet::diagnostics)
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
