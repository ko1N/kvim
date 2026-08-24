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
mod tests {
    use kvim_path::{WorktreeConfinementError, WorktreeRelativePath, WorktreeRoot};

    use super::{PREVIEW_CONTEXT_LINES, PREVIEW_LINES_MAX, PreviewError, read_preview};
    use crate::temp::TempDir;

    fn preview(dir: &TempDir, path: &str, line: usize) -> Result<super::Preview, PreviewError> {
        let root = WorktreeRoot::open(&dir.path).expect("the fixture root exists");
        let path = WorktreeRelativePath::new(path).expect("the fixture path is valid");
        read_preview(&root, &path, line)
    }

    #[test]
    fn the_preview_shows_the_region_around_the_line() {
        let dir = TempDir::new("preview-region");
        let text: String = (0..64).map(|index| format!("line {index}\n")).collect();
        dir.file("src/main.rs", &text);
        let preview = preview(&dir, "src/main.rs", 40).expect("the file holds text");
        assert_eq!(preview.first_line, 40 - PREVIEW_CONTEXT_LINES);
        assert_eq!(preview.lines.first().map(String::as_str), Some("line 32"));
        assert!(preview.lines.len() <= PREVIEW_LINES_MAX);
        assert!(!preview.truncated);
    }

    #[test]
    fn a_line_above_the_file_end_shows_the_last_region() {
        let dir = TempDir::new("preview-end");
        dir.file("a.rs", "one\ntwo\n");
        let preview = preview(&dir, "a.rs", 900).expect("the file holds text");
        assert!(preview.lines.is_empty(), "the region starts after the text");
    }

    #[test]
    fn a_binary_file_reports_an_unsupported_preview() {
        let dir = TempDir::new("preview-binary");
        let path = dir.join("binary");
        std::fs::write(&path, [0_u8, 1, 2, 3]).expect("the temporary directory is writable");
        assert!(matches!(
            preview(&dir, "binary", 0),
            Err(PreviewError::Unsupported)
        ));
    }

    #[test]
    fn a_missing_file_reports_a_read_failure() {
        let dir = TempDir::new("preview-missing");
        assert!(matches!(
            preview(&dir, "absent", 0),
            Err(PreviewError::Read(_))
        ));
    }

    #[test]
    fn the_preview_stops_at_the_line_and_character_bounds() {
        let dir = TempDir::new("preview-bounds");
        let long: String = "x".repeat(super::PREVIEW_LINE_CHARS_MAX + 64);
        let text: String = (0..PREVIEW_LINES_MAX + 32)
            .map(|_| format!("{long}\n"))
            .collect();
        dir.file("long.rs", &text);
        let preview = preview(&dir, "long.rs", 0).expect("the file holds text");
        assert_eq!(preview.lines.len(), PREVIEW_LINES_MAX);
        assert!(
            preview
                .lines
                .iter()
                .all(|line| line.chars().count() <= super::PREVIEW_LINE_CHARS_MAX)
        );
        assert!(preview.truncated);
    }

    #[test]
    fn the_preview_reports_byte_clipping() {
        let dir = TempDir::new("preview-byte-bound");
        dir.file("large.rs", &"x".repeat(super::PREVIEW_BYTES_MAX + 8));

        let preview = preview(&dir, "large.rs", 0).expect("the file holds text");

        assert!(preview.truncated);
    }

    #[test]
    fn the_preview_reports_line_clipping() {
        let dir = TempDir::new("preview-line-bound");
        let text = "line\n".repeat(PREVIEW_LINES_MAX + 1);
        dir.file("lines.rs", &text);

        let preview = preview(&dir, "lines.rs", 0).expect("the file holds text");

        assert_eq!(preview.lines.len(), PREVIEW_LINES_MAX);
        assert!(preview.truncated);
    }

    #[test]
    fn the_preview_reports_character_clipping() {
        let dir = TempDir::new("preview-character-bound");
        dir.file("line.rs", &"x".repeat(super::PREVIEW_LINE_CHARS_MAX + 1));

        let preview = preview(&dir, "line.rs", 0).expect("the file holds text");

        assert_eq!(
            preview.lines[0].chars().count(),
            super::PREVIEW_LINE_CHARS_MAX
        );
        assert!(preview.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn escaping_dangling_and_looping_preview_links_are_rejected() {
        let dir = TempDir::new("preview-link-failures");
        let outside = TempDir::new("preview-link-failures-outside");
        outside.file("outside.rs", "outside\n");
        std::os::unix::fs::symlink(outside.join("outside.rs"), dir.join("escape.rs"))
            .expect("the temporary directory supports links");
        std::os::unix::fs::symlink("missing.rs", dir.join("dangling.rs"))
            .expect("the temporary directory supports links");
        std::os::unix::fs::symlink("loop-b.rs", dir.join("loop-a.rs"))
            .expect("the temporary directory supports links");
        std::os::unix::fs::symlink("loop-a.rs", dir.join("loop-b.rs"))
            .expect("the temporary directory supports links");

        assert!(matches!(
            preview(&dir, "escape.rs", 0),
            Err(PreviewError::Confinement(WorktreeConfinementError::Escape))
        ));
        assert!(matches!(
            preview(&dir, "dangling.rs", 0),
            Err(PreviewError::Confinement(
                WorktreeConfinementError::DanglingLink
            ))
        ));
        assert!(matches!(
            preview(&dir, "loop-a.rs", 0),
            Err(PreviewError::Confinement(
                WorktreeConfinementError::LinkLoop
            ))
        ));
    }
}
