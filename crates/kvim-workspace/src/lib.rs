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
//! use kvim_settings::FileSettings;
//! use kvim_workspace::{Buffers, FileBuffer, FileRequest, FileResult, OpenRequest};
//!
//! let files = FileSettings::default();
//! let (mut buffers, scratch) = Buffers::new(FileBuffer::scratch(&files));
//!
//! // The request holds every value that the worker needs.
//! let request = FileRequest::Open(OpenRequest {
//!     path: "Cargo.toml".into(),
//!     files,
//! });
//!
//! // The worker runs the blocking step and returns one complete candidate.
//! if let FileResult::Opened { outcome: Ok(file), .. } = request.run() {
//!     let id = buffers
//!         .insert(FileBuffer::loaded(file.text, file.path, file.identity))
//!         .expect("the list holds fewer buffers than the limit");
//!     assert_ne!(id, scratch);
//!     assert_eq!(buffers.len(), 2);
//! }
//! ```

mod buffer;
mod clipboard;
mod file;
mod fuzzy;
mod git;
mod mutation;
mod picker;
mod picker_request;
mod request;
mod ripgrep;
mod tree;
mod tree_request;
mod undo_file;
mod walk;

#[cfg(any(test, feature = "test-support"))]
pub mod temp;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tree_tests;

pub use buffer::{BUFFERS_MAX, BufferId, Buffers, ExternalChange, FileBuffer, SCRATCH_BUFFER_NAME};
pub use clipboard::{FILE_CLIPBOARD_PATHS_MAX, FileClipboard};
pub use file::{
    FileChange, FileIdentity, LoadedFile, OpenError, SaveError, SavedFile, identity, load,
    render_content, save,
};
pub use fuzzy::{FUZZY_NAME_WEIGHT, FUZZY_TEXT_CHARS_MAX, score_candidate};
pub use git::{
    GIT_PATH_DEPTH_MAX, GIT_PROGRAM, GIT_STATUS_DEADLINE, GIT_STATUS_ENTRIES_MAX,
    GIT_STATUS_OUTPUT_BYTES_MAX, GitStatus, GitStatusFailure, GitStatusRequest, GitStatusSnapshot,
};
pub use mutation::{
    BufferPathUpdate, COPY_DEPTH_MAX, COPY_ENTRIES_MAX, FileOperation, MUTATION_PATHS_MAX,
    MutationError, MutationOutcome, MutationPlan, OpenBuffer, Overwrite, TakenDestination,
    TransferMode,
};
pub use picker::{
    Acceptance, Candidate, CandidateTarget, PICKER_CANDIDATES_MAX, PICKER_MATCH_CHARS_MAX,
    PICKER_QUERY_CHARS_MAX, Picker, PickerKind, PreviewTarget,
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
pub use ripgrep::{
    RIPGREP_COLUMNS_MAX, RIPGREP_DEADLINE, RIPGREP_FILE_MATCHES_MAX, RIPGREP_MATCHES_MAX,
    RIPGREP_OUTPUT_BYTES_MAX, RIPGREP_PROGRAM, parse_matches, ripgrep_command,
};
pub use tree::{
    DirectoryListing, EntryKind, Expansion, FileTree, HIDDEN_NAMES, HiddenPolicy, LinkKind,
    NameMatch, Notice, ReadError, RowContent, TREE_DEPTH_MAX, TREE_DIRECTORY_ENTRIES_MAX,
    TREE_DIRECTORY_SCAN_MAX, TREE_ENTRIES_MAX, TREE_PENDING_READS_MAX, TREE_SEARCH_CHARS_MAX,
    TREE_SEARCH_MATCHES_MAX, TREE_SEARCH_READS_MAX, TreeEntry, TreeRow, Truncation, read_directory,
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
