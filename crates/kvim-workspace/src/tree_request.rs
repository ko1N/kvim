//! The tree and mutation operations that the editor hands to the worker.
//!
//! The file follows the pattern of [`FileRequest`](super::FileRequest): one
//! request holds every value that the operation needs, and one result holds the
//! complete candidate. The event loop applies the result as one state
//! transition and never touches the filesystem itself. See
//! `docs/responsiveness.md`.

use std::path::PathBuf;
use std::sync::Arc;

use kvim_path::{WorktreeDirectoryPath, WorktreeRoot};

use super::mutation::{
    FileOperation, MutationError, MutationOutcome, MutationPlan, OpenBuffer, Overwrite,
};
use super::tree::{self, DirectoryListing, ReadError};

/// One workspace mutation with the state that its validation needs.
#[derive(Clone, Debug)]
pub struct MutateRequest {
    /// The operation that the user asked for.
    pub operation: FileOperation,
    /// The capability root that owns every affected path.
    pub root: Arc<WorktreeRoot>,
    /// The loaded buffers that the mutation can affect.
    pub buffers: Vec<OpenBuffer>,
    /// The destinations that one confirmed answer approved.
    ///
    /// Every request of a user command refuses a taken destination. Only the
    /// answer of the overwrite question names one. See `docs/files.md`.
    pub overwrite: Overwrite,
}

/// One blocking workspace operation.
#[derive(Clone, Debug)]
pub enum WorkspaceRequest {
    /// Read one directory for the file tree.
    ReadDirectory {
        /// The root capability used for this read.
        root: Arc<WorktreeRoot>,
        /// The validated directory at or below the root.
        path: WorktreeDirectoryPath,
    },
    /// Validate and apply one workspace mutation.
    Mutate(MutateRequest),
}

/// The completed result of one workspace operation.
#[derive(Debug)]
pub enum WorkspaceResult {
    /// One directory read finished.
    Directory {
        /// The directory that the read named.
        path: PathBuf,
        /// The listing, or the reason that the read failed.
        outcome: Result<DirectoryListing, ReadError>,
    },
    /// One mutation finished.
    Mutated {
        /// The affected buffers and directories, or the rejection.
        outcome: Result<MutationOutcome, MutationError>,
    },
}

impl WorkspaceRequest {
    /// Runs the operation and returns its complete typed result.
    ///
    /// The call blocks. Run it on the bounded worker service only.
    #[must_use]
    pub fn run(self) -> WorkspaceResult {
        match self {
            Self::ReadDirectory { root, path } => WorkspaceResult::Directory {
                outcome: tree::read_directory(&root, &path),
                path: path.display_path(&root),
            },
            Self::Mutate(request) => WorkspaceResult::Mutated {
                outcome: MutationPlan::stage_with(
                    &request.operation,
                    &request.root,
                    &request.buffers,
                    &request.overwrite,
                )
                .and_then(MutationPlan::apply),
            },
        }
    }
}
