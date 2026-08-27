//! Files, buffers, atomic save, the file tree, workspace mutations, and pickers.
//!
//! The crate owns buffer identity, the loaded buffer list, file loading, the
//! staged atomic save, external-change detection, the persistent undo file, the
//! file tree, the file-operation clipboard, the workspace mutations, and the
//! picker framework.
//!
//! Every blocking step lives in [`FileRequest::run`] and
//! [`WorkspaceRequest::run`]. The terminal event loop builds one request, hands
//! it to the bounded worker service, and applies the returned [`FileResult`] or
//! [`WorkspaceResult`] as one state transition. No function of this crate
//! reads or writes visible editor state. See `docs/files.md` and
//! `docs/responsiveness.md`.
//!
//! # Examples
//!
//! ```
//! use std::sync::Arc;
//!
//! use kvim_path::{WorktreeRelativePath, WorktreeRoot};
//! use kvim_settings::FileSettings;
//! use kvim_workspace::{Buffers, FileBuffer, FileRequest, FileResult, OpenRequest};
//!
//! let files = FileSettings::default();
//! let (mut buffers, scratch) = Buffers::new(FileBuffer::scratch(&files));
//! let root = Arc::new(WorktreeRoot::open(std::env::current_dir()?)?);
//!
//! // The request holds every value that the worker needs.
//! let request = FileRequest::Open(OpenRequest {
//!     root,
//!     path: WorktreeRelativePath::new("Cargo.toml")?,
//!     files,
//! });
//!
//! // The worker runs the blocking step and returns one complete candidate.
//! if let FileResult::Opened { outcome: Ok(file), .. } = request.run() {
//!     let id = buffers
//!         .insert(FileBuffer::loaded(file.text, file.target, file.identity))
//!         .expect("the list holds fewer buffers than the limit");
//!     assert_ne!(id, scratch);
//!     assert_eq!(buffers.len(), 2);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod align;
mod buffer;
mod clipboard;
mod diff;
mod diff_capture;
mod durable;
mod file;
mod git;
mod mutation;
mod picker;
mod picker_request;
mod request;
mod review;
mod ripgrep;
mod tree;
mod tree_request;
mod undo_file;
mod walk;

#[cfg(any(test, feature = "test-support"))]
pub mod temp;
#[cfg(test)]
mod tests;

pub use align::{AlignedRow, align_hunk};
pub use buffer::{
    BUFFERS_MAX, BufferId, Buffers, ExternalChange, FileBuffer, SCRATCH_BUFFER_NAME,
    SaveApplyOutcome,
};
pub use clipboard::{FILE_CLIPBOARD_PATHS_MAX, FileClipboard};
pub use diff::{
    AmbiguityReason, AnchorContext, AnchorContextError, AnchorLocation, BaseRevision,
    BaseRevisionError, CandidateAuthority, CommentBody, CommentBodyError, DIFF_FILE_HUNKS_MAX,
    DIFF_FILES_MAX, DIFF_HUNK_LINES_MAX, DIFF_LINE_BYTES_MAX, DIFF_LINE_NUMBER_MAX, DIGEST_BYTES,
    DiffChange, DiffComparison, DiffContent, DiffLimit, DiffLine, DiffLineText, DiffLineTextError,
    DiffOldSide, DiffRevision, DiffSide, DiffTarget, DiffTruncation, FILE_MODE_DIGITS, FileDiff,
    FileDiffError, FileMode, FileModeError, FileSide, HeadAuthority, Hunk, HunkError, HunkId,
    IndexAuthority, LineEnding, LineNumberError, LineOrigin, LineRangeError, NewLine, NewLineRange,
    OldLine, OldLineRange, RELOCATION_WINDOWS_MAX, REVIEW_COMMENT_BYTES_MAX,
    REVIEW_CONTEXT_LINES_MAX, Relocation, ReviewAnchor, ReviewAnchorError, SHA1_HEX_CHARS,
    SHA256_HEX_CHARS, SelectionDigest, TextDiff, TextDiffError, UnsupportedMode, WorktreeDiff,
    WorktreeDiffError, relocate,
};
pub use diff_capture::{
    AuthorityProjection, DIFF_ANSWER_OUTPUT_BYTES_MAX, DIFF_BINARY_SCAN_BYTES,
    DIFF_CAPTURE_ATTEMPTS_MAX, DIFF_CAPTURE_DEADLINE, DIFF_PROCESS_OUTPUT_BYTES_MAX,
    DIFF_SOURCE_BYTES_MAX, WorktreeDiffFailure, WorktreeDiffRead, WorktreeDiffRequest,
};
pub use durable::{
    DurableOutcome, INDETERMINATE_PATHS_MAX, Indeterminate, IndeterminateLimitError,
    RECOVERY_FAILURES_MAX, RecoveryAction, RecoveryFailure,
};
pub use file::{
    FileChange, FileIdentity, FileTarget, LoadedFile, OpenError, SaveError, SavedFile, identity,
    load, render_content, save,
};
// The scoring rule names no path and no buffer, so it lives in its own charter
// and this crate consumes it. The re-export keeps the picker vocabulary of this
// crate in one place. See `docs/files.md`.
pub use git::{
    GIT_PATH_DEPTH_MAX, GIT_PREFIX_OUTPUT_BYTES_MAX, GIT_PROGRAM, GIT_STATUS_DEADLINE,
    GIT_STATUS_ENTRIES_MAX, GIT_STATUS_OUTPUT_BYTES_MAX, GitExecutionPolicy, GitStatus,
    GitStatusFailure, GitStatusRead, GitStatusRequest, GitStatusSnapshot,
};
pub use kvim_fuzzy::{FUZZY_NAME_WEIGHT, FUZZY_TEXT_CHARS_MAX, score_candidate};
pub use mutation::{
    BufferPathUpdate, COPY_DEPTH_MAX, COPY_ENTRIES_MAX, FileOperation, MUTATION_PATHS_MAX,
    MutationError, MutationOutcome, MutationPlan, OpenBuffer, Overwrite, TakenDestination,
    TransferMode,
};
pub use picker::{
    Acceptance, Candidate, CandidateTarget, PICKER_CANDIDATES_MAX, PICKER_MATCH_CHARS_MAX,
    PICKER_QUERY_CHARS_MAX, Picker, PickerKind, PreviewTarget, rank_candidates,
};
pub use picker_request::{
    PICKER_PREVIEW_DEADLINE, PICKER_WALK_DEADLINE, PREVIEW_BYTES_MAX, PREVIEW_CONTEXT_LINES,
    PREVIEW_LINE_CHARS_MAX, PREVIEW_LINES_MAX, PickerRequest, PickerResult, PickerSlot, Preview,
    PreviewError, PreviewKey, read_preview,
};
pub use request::{
    FileRequest, FileResult, OpenRequest, OpenedFile, RELOAD_TARGETS_MAX, ReloadOutcome,
    ReloadRequest, ReloadTarget, ReloadTrigger, ReloadedBuffer, SaveRequest, SavedBuffer,
};
pub use review::{
    HunkStep, REVIEW_EVENTS_MAX, ReviewCursor, ReviewEvent, ReviewRow, ReviewSelectError,
    ReviewState, StaleLocation, SubmitCommentError, TargetAuthority,
};
pub use ripgrep::{
    RIPGREP_COLUMNS_MAX, RIPGREP_DEADLINE, RIPGREP_FILE_MATCHES_MAX, RIPGREP_MATCHES_MAX,
    RIPGREP_OUTPUT_BYTES_MAX, RIPGREP_PROGRAM, parse_matches, ripgrep_command,
};
pub use tree::{
    DirectoryIdentity, DirectoryListing, EntryKind, Expansion, FileTree, HIDDEN_NAMES,
    HiddenPolicy, LinkKind, NameMatch, Notice, ReadError, RowContent, TREE_DEPTH_MAX,
    TREE_DIRECTORY_ENTRIES_MAX, TREE_DIRECTORY_SCAN_MAX, TREE_ENTRIES_MAX, TREE_PENDING_READS_MAX,
    TREE_SEARCH_CHARS_MAX, TREE_SEARCH_MATCHES_MAX, TREE_SEARCH_READS_MAX, TreeEntry, TreeRow,
    Truncation, read_directory,
};
pub use tree_request::{MutateRequest, WorkspaceRequest, WorkspaceResult};
pub use undo_file::{
    UNDO_FILE_BYTES_MAX, UNDO_FILE_CHANGE_BYTES_MAX, UNDO_FILE_STEPS_MAX, UNDO_FILE_VERSION,
    UndoRecord, read_record, undo_file_path, write_record,
};
pub use walk::{
    IGNORE_FILE_BYTES_MAX, IGNORE_PATTERNS_MAX, WALK_DEPTH_MAX, WALK_DIRECTORIES_MAX,
    WALK_FILES_MAX, WalkOutcome, walk_files,
};
