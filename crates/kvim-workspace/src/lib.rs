//! Files, buffers, atomic save, the file tree, workspace mutations, and pickers.
//!
//! The crate owns buffer identity, the loaded buffer list, file loading, the
//! staged atomic save, external-change detection, the persistent undo file, the
//! file tree, the file-operation clipboard, the workspace mutations, and the
//! picker framework.
//!
//! With the `services` feature, every blocking step lives in `FileRequest::run`
//! and `WorkspaceRequest::run`. The terminal event loop builds one request,
//! hands it to the bounded worker service, and applies the returned `FileResult`
//! or `WorkspaceResult` as one state transition. No function of this crate
//! reads or writes visible editor state. See `docs/files.md` and
//! `docs/responsiveness.md`.
//!
//! # Service example
//!
//! The `services` feature provides the complete file-request workflow. Run
//! `cargo run -p kvim-tui --example worktree_diff_review` for a maintained
//! service-backed example.

mod align;
#[cfg(feature = "services")]
mod buffer;
#[cfg(feature = "services")]
mod clipboard;
mod diff;
#[cfg(feature = "services")]
mod diff_capture;
#[cfg(feature = "services")]
mod durable;
#[cfg(feature = "services")]
mod file;
#[cfg(feature = "services")]
mod git;
#[cfg(feature = "services")]
mod hash;
#[cfg(feature = "services")]
mod mutation;
#[cfg(feature = "services")]
mod picker;
#[cfg(feature = "services")]
mod picker_request;
#[cfg(feature = "services")]
mod recovery;
#[cfg(feature = "services")]
mod request;
mod review;
#[cfg(feature = "services")]
mod ripgrep;
#[cfg(feature = "services")]
mod tree;
#[cfg(feature = "services")]
mod tree_request;
#[cfg(feature = "services")]
mod undo_file;
#[cfg(feature = "services")]
mod walk;

#[cfg(all(feature = "services", any(test, feature = "test-support")))]
pub mod temp;
#[cfg(all(test, feature = "services"))]
mod tests;

pub use align::{AlignedRow, align_hunk};
#[cfg(feature = "services")]
pub use buffer::{
    BUFFERS_MAX, BufferId, Buffers, ExternalChange, FileBuffer, SCRATCH_BUFFER_NAME,
    SaveApplyOutcome,
};
#[cfg(feature = "services")]
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
#[cfg(feature = "services")]
pub use diff_capture::{
    AuthorityProjection, DIFF_ANSWER_OUTPUT_BYTES_MAX, DIFF_BINARY_SCAN_BYTES,
    DIFF_CAPTURE_ATTEMPTS_MAX, DIFF_CAPTURE_DEADLINE, DIFF_PROCESS_OUTPUT_BYTES_MAX,
    DIFF_SOURCE_BYTES_MAX, WorktreeDiffFailure, WorktreeDiffRead, WorktreeDiffRequest,
};
#[cfg(feature = "services")]
pub use durable::{
    DurableOutcome, INDETERMINATE_PATHS_MAX, Indeterminate, IndeterminateLimitError,
    RECOVERY_FAILURES_MAX, RecoveryAction, RecoveryFailure,
};
#[cfg(feature = "services")]
pub use file::{
    FileChange, FileIdentity, FileTarget, LoadedFile, OpenError, SaveError, SavedFile, identity,
    load, render_content, save,
};
// The workspace crate uses the domain-neutral selector for picker ranking. It
// does not re-export the lower-level fuzzy API. See `docs/files.md`.
#[cfg(feature = "services")]
pub use git::{
    GIT_PATH_DEPTH_MAX, GIT_PREFIX_OUTPUT_BYTES_MAX, GIT_PROGRAM, GIT_STATUS_DEADLINE,
    GIT_STATUS_ENTRIES_MAX, GIT_STATUS_OUTPUT_BYTES_MAX, GitExecutionPolicy, GitStatus,
    GitStatusFailure, GitStatusRead, GitStatusRequest, GitStatusSnapshot,
};
#[cfg(feature = "services")]
pub use mutation::{
    BufferPathUpdate, COPY_DEPTH_MAX, COPY_ENTRIES_MAX, FileOperation, MUTATION_PATHS_MAX,
    MutationError, MutationOutcome, MutationPlan, OpenBuffer, Overwrite, TakenDestination,
    TransferMode,
};
#[cfg(feature = "services")]
pub use picker::{
    Acceptance, Candidate, CandidateTarget, PICKER_CANDIDATES_MAX, PICKER_MATCH_CHARS_MAX,
    PICKER_QUERY_CHARS_MAX, Picker, PickerKind, PreviewTarget,
};
#[cfg(feature = "services")]
pub use picker_request::{
    PICKER_PREVIEW_DEADLINE, PICKER_WALK_DEADLINE, PREVIEW_BYTES_MAX, PREVIEW_CONTEXT_LINES,
    PREVIEW_LINE_CHARS_MAX, PREVIEW_LINES_MAX, PickerRequest, PickerResult, PickerSlot, Preview,
    PreviewError, PreviewKey, read_preview,
};
#[cfg(feature = "services")]
pub use recovery::{
    RECOVERY_RECORD_BYTES_MAX, RECOVERY_RECORD_VERSION, RecoveryBaseline, RecoveryError,
    RecoveryRecord, read_recovery_record, recovery_record_path, write_recovery_record,
};
#[cfg(feature = "services")]
pub use request::{
    FileRequest, FileResult, OpenRequest, OpenedFile, RELOAD_TARGETS_MAX, ReloadOutcome,
    ReloadRequest, ReloadTarget, ReloadTrigger, ReloadedBuffer, SaveRequest, SavedBuffer,
};
pub use review::{
    HunkStep, REVIEW_EVENTS_MAX, ReviewCursor, ReviewEvent, ReviewRow, ReviewSelectError,
    ReviewState, StaleLocation, SubmitCommentError, TargetAuthority,
};
#[cfg(feature = "services")]
pub use ripgrep::{
    RIPGREP_COLUMNS_MAX, RIPGREP_DEADLINE, RIPGREP_FILE_MATCHES_MAX, RIPGREP_MATCHES_MAX,
    RIPGREP_OUTPUT_BYTES_MAX, RIPGREP_PROGRAM, parse_matches, ripgrep_command,
};
#[cfg(feature = "services")]
pub use tree::{
    DirectoryIdentity, DirectoryListing, EntryKind, Expansion, FileTree, HIDDEN_NAMES,
    HiddenPolicy, LinkKind, NameMatch, Notice, ReadError, RowContent, TREE_DEPTH_MAX,
    TREE_DIRECTORY_ENTRIES_MAX, TREE_DIRECTORY_SCAN_MAX, TREE_ENTRIES_MAX, TREE_PENDING_READS_MAX,
    TREE_SEARCH_CHARS_MAX, TREE_SEARCH_MATCHES_MAX, TREE_SEARCH_READS_MAX, TreeEntry, TreeRow,
    Truncation, read_directory,
};
#[cfg(feature = "services")]
pub use tree_request::{MutateRequest, WorkspaceRequest, WorkspaceResult};
#[cfg(feature = "services")]
pub use undo_file::{
    UNDO_FILE_BYTES_MAX, UNDO_FILE_CHANGE_BYTES_MAX, UNDO_FILE_STEPS_MAX, UNDO_FILE_VERSION,
    UndoRecord, read_record, undo_file_path, write_record,
};
#[cfg(feature = "services")]
pub use walk::{
    IGNORE_FILE_BYTES_MAX, IGNORE_PATTERNS_MAX, WALK_DEPTH_MAX, WALK_DIRECTORIES_MAX,
    WALK_FILES_MAX, WalkOutcome, walk_files,
};
