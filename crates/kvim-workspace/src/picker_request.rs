//! The blocking picker operations and their bounded previews.
//!
//! The file follows the pattern of [`FileRequest`](super::FileRequest): one
//! request holds every value that the operation needs, and one result holds the
//! complete candidate. The terminal event loop builds the request, the bounded
//! worker or process service runs it, and the event loop applies the result as
//! one state transition. See `docs/files.md` and `docs/responsiveness.md`.

use std::io::{self, Read};
use std::path::PathBuf;
use std::str;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use kvim_path::{WorktreeConfinementError, WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{ProcessOutput, ProcessRequest};

use super::picker::{Candidate, PreviewTarget};
use super::ripgrep::{RIPGREP_DEADLINE, parse_matches, ripgrep_command};
use super::walk::walk_files;

/// The largest number of bytes that one preview reads.
pub const PREVIEW_BYTES_MAX: usize = 128 * 1024;

/// The largest number of lines that one preview shows.
pub const PREVIEW_LINES_MAX: usize = 200;

/// The largest number of characters that one preview line keeps.
pub const PREVIEW_LINE_CHARS_MAX: usize = 400;

/// The number of lines that the preview shows above the matched line.
pub const PREVIEW_CONTEXT_LINES: usize = 8;

/// The deadline of one workspace walk.
pub const PICKER_WALK_DEADLINE: Duration = Duration::from_secs(5);

/// The deadline of one preview read.
pub const PICKER_PREVIEW_DEADLINE: Duration = Duration::from_secs(2);

/// The publication slot of one picker operation.
///
/// A newer request of one slot makes the older request of that slot obsolete.
/// See `docs/responsiveness.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PickerSlot {
    /// The candidates of the picker.
    Candidates,
    /// The preview of the selected row.
    Preview,
}

/// The file and the region that one preview shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewKey {
    /// The root capability used for the preview read.
    root: Arc<WorktreeRoot>,
    /// The validated path used for capability access.
    relative: WorktreeRelativePath,
    /// The root-derived absolute display path of the previewed file.
    path: PathBuf,
    /// The region of the file, and the line that the preview marks.
    target: PreviewTarget,
}

impl PreviewKey {
    /// Creates one preview identity below a root capability.
    #[must_use]
    pub fn new(
        root: Arc<WorktreeRoot>,
        relative: WorktreeRelativePath,
        target: PreviewTarget,
    ) -> Self {
        let path = root.as_path().join(relative.as_path());
        Self {
            root,
            relative,
            path,
            target,
        }
    }

    /// Returns the root-derived absolute display path.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Returns the region that the preview shows.
    #[must_use]
    pub const fn target(&self) -> PreviewTarget {
        self.target
    }
}

/// One bounded region of one file.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Preview {
    /// The zero-based line of the first shown row.
    pub first_line: usize,
    /// The shown rows, bounded by [`PREVIEW_LINES_MAX`].
    pub lines: Vec<String>,
    /// Reports whether a byte, line, or character bound clipped the preview.
    pub truncated: bool,
}

/// A rejected preview read.
#[derive(Debug, Error)]
pub enum PreviewError {
    /// The target did not remain confined to its worktree root.
    #[error("the preview target is not confined to the worktree")]
    Confinement(#[from] WorktreeConfinementError),
    /// The file could not be read.
    #[error("the file could not be read")]
    Read(#[source] io::Error),
    /// The file holds no UTF-8 text.
    #[error("the file holds no text")]
    Unsupported,
}

/// One blocking picker operation.
#[derive(Clone, Debug)]
pub enum PickerRequest {
    /// Collect the files of one workspace on the bounded worker service.
    Files {
        /// The workspace root capability that the walk starts at.
        root: Arc<WorktreeRoot>,
    },
    /// Search one workspace through the bounded process service.
    Search {
        /// The validated workspace root that the search covers.
        root: Arc<WorktreeRoot>,
        /// The query that the search sends to `rg`.
        query: String,
    },
    /// Read the region around one selected line on the worker service.
    Preview(PreviewKey),
}

/// The completed result of one picker operation.
#[derive(Debug)]
pub enum PickerResult {
    /// One candidate source finished.
    Candidates {
        /// The query that the search used, or an empty query for a walk.
        query: String,
        /// The collected candidates.
        candidates: Vec<Candidate>,
        /// Reports whether the source stopped at one of its bounds.
        truncated: bool,
    },
    /// One preview read finished.
    Preview {
        /// The selection that the preview describes.
        key: PreviewKey,
        /// The region, or the reason that the read failed.
        outcome: Result<Preview, PreviewError>,
    },
}

impl PickerRequest {
    /// Returns the publication slot of the request.
    #[must_use]
    pub const fn slot(&self) -> PickerSlot {
        match self {
            Self::Files { .. } | Self::Search { .. } => PickerSlot::Candidates,
            Self::Preview(_) => PickerSlot::Preview,
        }
    }

    /// Returns the deadline of the request.
    #[must_use]
    pub const fn deadline(&self) -> Duration {
        match self {
            Self::Files { .. } => PICKER_WALK_DEADLINE,
            Self::Search { .. } => RIPGREP_DEADLINE,
            Self::Preview(_) => PICKER_PREVIEW_DEADLINE,
        }
    }

    /// Returns the external command of a search, or `None` for a worker job.
    #[must_use]
    pub fn command(&self) -> Option<ProcessRequest> {
        match self {
            Self::Search { root, query } => Some(ripgrep_command(root, query)),
            Self::Files { .. } | Self::Preview(_) => None,
        }
    }

    /// Runs one worker operation and returns its complete typed result.
    ///
    /// The call blocks. Run it on the bounded worker service only. The walk
    /// checks the cancellation token, so a superseded walk stops early.
    #[must_use]
    pub fn run(self, cancellation: &CancellationToken) -> PickerResult {
        match self {
            Self::Files { root } => {
                let outcome = walk_files(Arc::clone(&root), cancellation);
                PickerResult::Candidates {
                    query: String::new(),
                    candidates: outcome
                        .files
                        .into_iter()
                        .map(|path| Candidate::file(&root, path))
                        .collect(),
                    truncated: outcome.truncated,
                }
            }
            Self::Preview(key) => {
                let outcome = read_preview(&key.root, &key.relative, key.target.line());
                PickerResult::Preview { key, outcome }
            }
            Self::Search { query, .. } => {
                debug_assert!(
                    false,
                    "the event loop runs a search on the bounded process service"
                );
                PickerResult::Candidates {
                    query,
                    candidates: Vec::new(),
                    truncated: false,
                }
            }
        }
    }

    /// Turns the captured output of one search into its typed result.
    ///
    /// A search that wrote nothing to standard output produces an empty list,
    /// which is the normal result of a query without a match.
    #[must_use]
    pub fn publish(self, output: &ProcessOutput) -> PickerResult {
        let Self::Search { root, query } = self else {
            debug_assert!(false, "only a search reaches the process service");
            return PickerResult::Candidates {
                query: String::new(),
                candidates: Vec::new(),
                truncated: false,
            };
        };
        let (candidates, truncated) = parse_matches(&root, &output.stdout);
        PickerResult::Candidates {
            query,
            candidates,
            truncated,
        }
    }
}

/// Reads the bounded region around one line of one file.
///
/// The read stops at [`PREVIEW_BYTES_MAX`] bytes, keeps at most
/// [`PREVIEW_LINES_MAX`] lines, and clips every line at
/// [`PREVIEW_LINE_CHARS_MAX`] characters.
///
/// The call blocks. Run it on the bounded worker service only.
///
/// # Errors
///
/// Returns [`PreviewError::Read`] for an unreadable file and
/// [`PreviewError::Unsupported`] for a file that holds no UTF-8 text.
pub fn read_preview(
    root: &WorktreeRoot,
    path: &WorktreeRelativePath,
    line: usize,
) -> Result<Preview, PreviewError> {
    let resolved = root.resolve(path)?;
    let file = root
        .directory()
        .open(resolved.path().as_path())
        .map_err(PreviewError::Read)?;
    let opened = file.metadata().map_err(PreviewError::Read)?;
    if !opened.is_file() {
        return Err(PreviewError::Unsupported);
    }
    let opened = metadata_identity(&opened);
    let mut bytes = Vec::new();
    file.take(bytes_limit())
        .read_to_end(&mut bytes)
        .map_err(PreviewError::Read)?;
    let mut truncated = bytes.len() > PREVIEW_BYTES_MAX;
    bytes.truncate(PREVIEW_BYTES_MAX);
    // A zero byte marks a file that no text editor can show, so the preview
    // reports the kind instead of writing control bytes to the terminal.
    if bytes.contains(&0) {
        return Err(PreviewError::Unsupported);
    }
    let text = match str::from_utf8(&bytes) {
        Ok(text) => text,
        // The bounded read can stop inside one character, which leaves a valid
        // prefix. Every other failure names a file that holds no text.
        Err(error) if truncated && error.error_len().is_none() => {
            str::from_utf8(&bytes[..error.valid_up_to()]).map_err(|_| PreviewError::Unsupported)?
        }
        Err(_) => return Err(PreviewError::Unsupported),
    };
    let current = root
        .directory()
        .metadata(resolved.path().as_path())
        .map_err(PreviewError::Read)?;
    if metadata_identity(&current) != opened {
        return Err(PreviewError::Confinement(
            WorktreeConfinementError::Replaced,
        ));
    }
    root.revalidate(path, &resolved)?;
    let first_line = line.saturating_sub(PREVIEW_CONTEXT_LINES);
    let mut source_lines = text.lines().skip(first_line);
    let mut lines = Vec::with_capacity(PREVIEW_LINES_MAX);
    for line in source_lines.by_ref().take(PREVIEW_LINES_MAX) {
        let mut chars = line.chars();
        let clipped: String = chars.by_ref().take(PREVIEW_LINE_CHARS_MAX).collect();
        truncated |= chars.next().is_some();
        lines.push(clipped);
    }
    truncated |= source_lines.next().is_some();
    Ok(Preview {
        first_line,
        lines,
        truncated,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MetadataIdentity {
    device: u64,
    inode: u64,
}

fn metadata_identity(metadata: &cap_std::fs::Metadata) -> MetadataIdentity {
    use cap_std::fs::MetadataExt as _;

    MetadataIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

/// Returns the read limit of one preview as a file offset.
fn bytes_limit() -> u64 {
    u64::try_from(PREVIEW_BYTES_MAX.saturating_add(1)).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "picker_request_tests.rs"]
mod tests;
