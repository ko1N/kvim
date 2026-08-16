//! The tree and mutation operations that the editor hands to the worker.
//!
//! The file follows the pattern of [`FileRequest`](super::FileRequest): one
//! request holds every value that the operation needs, and one result holds the
//! complete candidate. The event loop applies the result as one state
//! transition and never touches the filesystem itself. See
//! `docs/responsiveness.md`.

use std::path::PathBuf;

use super::mutation::{FileOperation, MutationError, MutationOutcome, MutationPlan, OpenBuffer};
use super::tree::{self, DirectoryListing, ReadError};

/// One workspace mutation with the state that its validation needs.
#[derive(Clone, Debug)]
pub struct MutateRequest {
    /// The operation that the user asked for.
    pub operation: FileOperation,
    /// The workspace root. Every affected path must stay inside it.
    pub root: PathBuf,
    /// The loaded buffers that the mutation can affect.
    pub buffers: Vec<OpenBuffer>,
}

/// One blocking workspace operation.
#[derive(Clone, Debug)]
pub enum WorkspaceRequest {
    /// Read one directory for the file tree.
    ReadDirectory {
        /// The directory to read.
        path: PathBuf,
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
            Self::ReadDirectory { path } => WorkspaceResult::Directory {
                outcome: tree::read_directory(&path),
                path,
            },
            Self::Mutate(request) => WorkspaceResult::Mutated {
                outcome: MutationPlan::stage(&request.operation, &request.root, &request.buffers)
                    .and_then(MutationPlan::apply),
            },
        }
    }
}
