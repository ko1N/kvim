//! Files, buffers, atomic save, the file tree, workspace mutations, and pickers.
//!
//! The module owns buffer identity, the loaded buffer list, file loading, the
//! staged atomic save, external-change detection, and the persistent undo file.
//! The file tree, the workspace mutations, and the pickers arrive in Slices 10
//! and 11.
//!
//! Every blocking step lives in [`FileRequest::run`]. The terminal event loop
//! builds one request, hands it to the bounded worker service, and applies the
//! returned [`FileResult`] as one state transition. No function of this module
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
mod file;
mod request;
mod undo_file;

#[cfg(test)]
pub(crate) mod temp;
#[cfg(test)]
mod tests;

pub use buffer::{BUFFERS_MAX, BufferId, Buffers, FileBuffer, SCRATCH_BUFFER_NAME};
pub use file::{
    FileIdentity, LoadedFile, OpenError, SaveError, SavedFile, load, render_content, save,
};
pub use request::{FileRequest, FileResult, OpenRequest, OpenedFile, SaveRequest, SavedBuffer};
pub use undo_file::{
    UNDO_FILE_BYTES_MAX, UNDO_FILE_CHANGE_BYTES_MAX, UNDO_FILE_STEPS_MAX, UNDO_FILE_VERSION,
    UndoRecord, read_record, undo_file_path, write_record,
};
