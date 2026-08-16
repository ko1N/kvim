//! Files, buffers, atomic save, the file tree, workspace mutations, and pickers.
//!
//! The module owns buffer identity, the loaded buffer list, file loading, the
//! staged atomic save, external-change detection, the persistent undo file, the
//! file tree, the file-operation clipboard, and the workspace mutations. The
//! pickers arrive in Slice 11.
//!
//! Every blocking step lives in [`FileRequest::run`] and
//! [`WorkspaceRequest::run`]. The terminal event loop builds one request, hands
//! it to the bounded worker service, and applies the returned [`FileResult`] or
//! [`WorkspaceResult`] as one state transition. No function of this module
//! reads or writes visible editor state. See `docs/files.md` and
//! `docs/responsiveness.md`.
//!
//! # Examples
//!
//! ```
//! use kvim::settings::FileSettings;
//! use kvim::workspace::{Buffers, FileBuffer, FileRequest, FileResult, OpenRequest};
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
mod mutation;
mod request;
mod tree;
mod tree_request;
mod undo_file;

#[cfg(test)]
pub(crate) mod temp;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tree_tests;

pub use buffer::{BUFFERS_MAX, BufferId, Buffers, FileBuffer, SCRATCH_BUFFER_NAME};
pub use clipboard::{FILE_CLIPBOARD_PATHS_MAX, FileClipboard};
pub use file::{
    FileIdentity, LoadedFile, OpenError, SaveError, SavedFile, load, render_content, save,
};
pub use mutation::{
    BufferPathUpdate, COPY_DEPTH_MAX, COPY_ENTRIES_MAX, FileOperation, MUTATION_PATHS_MAX,
    MutationError, MutationOutcome, MutationPlan, OpenBuffer, TransferMode,
};
pub use request::{FileRequest, FileResult, OpenRequest, OpenedFile, SaveRequest, SavedBuffer};
pub use tree::{
    DirectoryListing, EntryKind, Expansion, FileTree, HIDDEN_NAMES, HiddenPolicy, LinkKind, Notice,
    ReadError, RowContent, TREE_DEPTH_MAX, TREE_DIRECTORY_ENTRIES_MAX, TREE_DIRECTORY_SCAN_MAX,
    TREE_ENTRIES_MAX, TREE_FILTER_CHARS_MAX, TREE_PENDING_READS_MAX, TreeEntry, TreeFilter,
    TreeRow, Truncation, read_directory,
};
pub use tree_request::{MutateRequest, WorkspaceRequest, WorkspaceResult};
pub use undo_file::{
    UNDO_FILE_BYTES_MAX, UNDO_FILE_CHANGE_BYTES_MAX, UNDO_FILE_STEPS_MAX, UNDO_FILE_VERSION,
    UndoRecord, read_record, undo_file_path, write_record,
};
