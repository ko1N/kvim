//! The bounded read-only capture of one worktree diff.
//!
//! The editor never runs `git` itself. [`WorktreeDiffRequest::command`] builds
//! one [`ProcessRequest`], the bounded process service runs it, and
//! [`WorktreeDiffRequest::publish`] turns the captured output into the next step
//! of the capture. Every command carries the [`GitExecutionPolicy`] of
//! `docs/git.md`, so no repository and no host setting can start another
//! program, take an optional lock, or redirect the read.
//!
//! One capture runs three passes over the same command sequence. The first pass
//! reads the authority fingerprint, the second pass collects the candidate, and
//! the third pass reads the authority fingerprint again. The three values must
//! match before the capture publishes. A mismatch retries inside
//! [`DIFF_CAPTURE_ATTEMPTS_MAX`], and exhaustion returns
//! [`WorktreeDiffFailure::ChangedDuringCapture`]. The middle pass therefore sits
//! between two authority reads, which also rejects a change that returns to its
//! first value during the capture.
//!
//! This module reads the repository and never writes it. No function here
//! stages, unstages, reverts, or discards anything. See `docs/git.md`.

use std::ffi::OsString;
use std::io::Read as _;
use std::str;
use std::sync::Arc;
use std::time::Duration;

use blake3::Hasher;
use cap_std::fs::MetadataExt as _;
use kvim_path::{ResolvedTargetState, WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{ProcessOutput, ProcessRequest, RuntimeError};

use crate::diff::{
    BaseRevision, CandidateAuthority, DIFF_FILE_HUNKS_MAX, DIFF_FILES_MAX, DIFF_HUNK_LINES_MAX,
    DIGEST_BYTES, DiffChange, DiffContent, DiffLimit, DiffLine, DiffLineText, DiffSide, DiffTarget,
    DiffTruncation, FileDiff, FileMode, FileSide, HeadAuthority, Hunk, HunkId, IndexAuthority,
    LineEnding, LineOrigin, NewLine, NewLineRange, OldLine, OldLineRange, TextDiff, WorktreeDiff,
    absorb, absorb_count,
};
use crate::git::GitExecutionPolicy;

/// The largest number of capture attempts of one request.
///
/// One attempt reads the authority, collects the candidate, and reads the
/// authority again. A repository that changes through every attempt returns
/// [`WorktreeDiffFailure::ChangedDuringCapture`] instead of a mixed candidate.
pub const DIFF_CAPTURE_ATTEMPTS_MAX: usize = 3;

/// The deadline of one command of the capture.
pub const DIFF_CAPTURE_DEADLINE: Duration = Duration::from_secs(15);

/// The largest output that one listing or patch command of the capture
/// captures, in bytes.
///
/// The bound covers the output of one process. It is separate from
/// [`DIFF_SOURCE_BYTES_MAX`], which bounds the exact source bytes of one file
/// that the capture reads from the worktree.
pub const DIFF_PROCESS_OUTPUT_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The largest output that one short answer of the capture captures, in bytes.
pub const DIFF_ANSWER_OUTPUT_BYTES_MAX: usize = 8 * 1024;

/// The largest number of exact source bytes that the capture reads from one
/// untracked worktree file.
///
/// A larger file publishes no line and reports truncation, so the reader always
/// sees that content is missing.
pub const DIFF_SOURCE_BYTES_MAX: usize = 1024 * 1024;

/// The number of leading bytes that decide whether an untracked file is binary.
///
/// Git inspects the same window, so an untracked file and a tracked file take
/// the same answer.
pub const DIFF_BINARY_SCAN_BYTES: usize = 8000;

/// The domain separator of every authority projection of this module.
const PROJECTION_DOMAIN: &[u8] = b"kvim.diff.projection.v1";

/// The domain separator of every status digest of this module.
const STATUS_DOMAIN: &[u8] = b"kvim.diff.status.v1";

/// The domain separator of every index digest of this module.
const INDEX_DOMAIN: &[u8] = b"kvim.diff.index.v1";

/// The type name that `git cat-file -t` writes for one commit object.
const COMMIT_TYPE: &[u8] = b"commit";

/// The exit code that `git rev-parse --verify --quiet HEAD` returns while the
/// checked-out branch holds no commit.
///
/// Git separates this refusal from every other one by its exit code, so no
/// branch of the capture reads message text.
const UNBORN_HEAD_EXIT: i32 = 1;

/// The variables that every command of the capture sets beside the policy.
///
/// A fixed locale keeps every marker of the patch format stable, so the parser
/// reads one format on every host.
const CAPTURE_VARIABLES: [(&str, &str); 2] = [("LANGUAGE", "C"), ("LC_ALL", "C")];

/// The arguments that name the object type of the review base.
const BASE_KIND_ARGUMENTS: [&str; 2] = ["cat-file", "-t"];

/// The arguments that name the commit of the current `HEAD`.
const HEAD_ARGUMENTS: [&str; 4] = ["rev-parse", "--verify", "--quiet", "HEAD"];

/// The arguments that read the index authority.
///
/// The staged listing names every stage of every entry, so an unresolved merge
/// changes the digest exactly as a staged change does.
const INDEX_ARGUMENTS: [&str; 3] = ["ls-files", "--stage", "-z"];

/// The arguments that read the status records of the fingerprint.
const STATUS_ARGUMENTS: [&str; 5] = [
    "status",
    "--porcelain=v2",
    "-z",
    "--ignored=no",
    "--untracked-files=all",
];

/// The arguments that list every untracked file that no ignore rule names.
const UNTRACKED_ARGUMENTS: [&str; 4] = ["ls-files", "--others", "--exclude-standard", "-z"];

/// The arguments that every diff read of the capture shares.
///
/// The exact-byte rule of `docs/git.md` demands `--no-ext-diff` and
/// `--no-textconv`. The remaining arguments keep the record order and the
/// rename pairing of the two diff reads identical.
const DIFF_ARGUMENTS: [&str; 5] = [
    "diff",
    "--no-ext-diff",
    "--no-textconv",
    "--find-renames",
    "--ignore-submodules=none",
];

/// The arguments that name the paths, modes, and kinds of every changed file.
const RAW_ARGUMENTS: [&str; 3] = ["--raw", "-z", "--relative"];

/// The arguments that publish the exact lines of every changed file.
///
/// Each hunk carries three unchanged lines around its change, which is the
/// number that a reader needs to place a hunk without reading the whole file.
const PATCH_ARGUMENTS: [&str; 4] = ["--patch", "--no-color", "--unified=3", "--relative"];

/// The pathspec that names every entry below the worktree root.
const ROOT_PATHSPEC: &str = ".";

/// The separator that follows every option and precedes every pathspec.
const PATHSPEC_SEPARATOR: &str = "--";

/// The marker that opens one hunk of the patch format.
const HUNK_MARKER: &[u8] = b"@@ ";

/// The marker that opens one file section of the patch format.
const SECTION_MARKER: &[u8] = b"diff --git ";

/// The markers that name binary content in the patch format.
const BINARY_MARKERS: [&[u8]; 2] = [b"Binary files ", b"GIT binary patch"];

// ---------------------------------------------------------------------------
// Published values
// ---------------------------------------------------------------------------

/// The identity that one candidate and one authority read both derive.
///
/// The value is a BLAKE3 digest of the base commit, the published truncation,
/// and, for every collected file in order, the path, the published mode, and
/// the exact side bytes of both sides. Two captures of one unchanged repository
/// therefore share one projection, and any change of a collected path, mode, or
/// side byte produces another one.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use kvim_workspace::{
///     AuthorityProjection, BaseRevision, CandidateAuthority, DiffTarget, DiffTruncation,
///     HeadAuthority, IndexAuthority, WorktreeDiff,
/// };
///
/// let base = BaseRevision::new("0123456789abcdef0123456789abcdef01234567")?;
/// let authority = CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest([0; 32]));
/// let empty = WorktreeDiff::new(
///     base,
///     DiffTarget::Worktree,
///     &authority,
///     Vec::new(),
///     DiffTruncation::Complete,
/// )?;
///
/// assert_eq!(AuthorityProjection::of(&empty), AuthorityProjection::of(&empty));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AuthorityProjection([u8; DIGEST_BYTES]);

impl AuthorityProjection {
    /// Derives the projection of one collected candidate.
    #[must_use]
    pub fn of(diff: &WorktreeDiff) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(PROJECTION_DOMAIN);
        absorb(&mut hasher, diff.base().as_bytes());
        hasher.update(&[u8::from(diff.truncation().is_truncated())]);
        absorb_count(&mut hasher, diff.files().len());
        for file in diff.files() {
            for side in [DiffSide::Old, DiffSide::New] {
                let Some(published) = file.change().side(side) else {
                    hasher.update(&[0]);
                    continue;
                };
                hasher.update(&[1]);
                absorb(
                    &mut hasher,
                    published.path().as_path().as_os_str().as_encoded_bytes(),
                );
                hasher.update(published.mode().as_octal().as_bytes());
                match file.content() {
                    DiffContent::Text(text) => absorb(&mut hasher, &text.side_bytes(side)),
                    DiffContent::Binary
                    | DiffContent::SymbolicLink
                    | DiffContent::Submodule
                    | DiffContent::Unsupported => absorb(&mut hasher, &[]),
                }
            }
        }
        Self(*hasher.finalize().as_bytes())
    }

    /// Returns the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }
}

/// The repository authority that one pass of the capture read.
///
/// The fingerprint covers the commit of `HEAD`, the index, the status records,
/// and the projection of the collected candidate. The projection names every
/// selected path, its published mode, and the digest of its exact side bytes,
/// so the fingerprint carries the identity and the content of every selected
/// worktree file. See `docs/git.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CaptureFingerprint {
    head: HeadAuthority,
    index: IndexAuthority,
    status: [u8; DIGEST_BYTES],
    projection: AuthorityProjection,
}

/// The reasons that one worktree diff capture published no candidate.
///
/// Every value is a typed outcome. No branch of this module reads the message
/// text of `git` or of any error. See `docs/git.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeDiffFailure {
    /// The host holds no `git` command.
    CommandMissing,
    /// Git produced no answer, or the answer was malformed.
    ///
    /// The directory is inside no repository, or Git refused the command.
    Unavailable,
    /// The base names no object of the repository, or no commit object.
    BaseUnavailable,
    /// The request was cancelled or superseded.
    Cancelled,
    /// One command passed its deadline.
    DeadlineExpired,
    /// One command wrote more than its output bound.
    ProcessOutputLimit,
    /// The repository changed through every capture attempt.
    ChangedDuringCapture,
}

impl WorktreeDiffFailure {
    /// Classifies one refused background request.
    ///
    /// The classification reads the typed variant and the error kind of the
    /// runtime, never message text.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_runtime::RuntimeError;
    /// use kvim_workspace::WorktreeDiffFailure;
    ///
    /// assert_eq!(
    ///     WorktreeDiffFailure::from_runtime(&RuntimeError::Timeout),
    ///     WorktreeDiffFailure::DeadlineExpired
    /// );
    /// ```
    #[must_use]
    pub fn from_runtime(error: &RuntimeError) -> Self {
        match error {
            RuntimeError::Cancelled => Self::Cancelled,
            RuntimeError::Timeout => Self::DeadlineExpired,
            RuntimeError::OutputLimit { .. } => Self::ProcessOutputLimit,
            RuntimeError::ProcessSpawn(source) if source.kind() == std::io::ErrorKind::NotFound => {
                Self::CommandMissing
            }
            RuntimeError::ProcessSpawn(_)
            | RuntimeError::ProcessRead(_)
            | RuntimeError::ProcessWrite(_)
            | RuntimeError::WorkerFailure(_) => Self::Unavailable,
        }
    }
}

/// The next step of one bounded worktree diff capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeDiffRead {
    /// The capture needs one further command before it can publish.
    Pending(Box<WorktreeDiffRequest>),
    /// The capture finished and produced one consistent candidate.
    Published(Box<WorktreeDiff>),
}

// ---------------------------------------------------------------------------
// The capture state machine
// ---------------------------------------------------------------------------

/// The command that one pass of the capture runs next.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureStep {
    /// Prove that the base names one commit object.
    BaseKind,
    /// Read the commit of the current `HEAD`.
    Head,
    /// Read the index authority.
    Index,
    /// Read the status records.
    Status,
    /// List every untracked file.
    Untracked,
    /// Read the paths, modes, and kinds of every changed file.
    Raw,
    /// Read the exact lines of every changed file.
    Patch,
}

/// The role of one pass of the capture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapturePass {
    /// The pass reads the authority before the collection.
    Initial,
    /// The pass collects the candidate that the capture may publish.
    Candidate,
    /// The pass reads the authority after the collection.
    Final,
}

/// The answers that one pass collected so far.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PassReads {
    head: Option<HeadAuthority>,
    index: Option<IndexAuthority>,
    status: Option<[u8; DIGEST_BYTES]>,
    untracked: Option<Vec<WorktreeRelativePath>>,
    raw: Option<Vec<RawRecord>>,
}

/// One bounded read-only capture of one worktree diff.
///
/// The caller submits the command of [`WorktreeDiffRequest::command`] to the
/// bounded process service and hands the captured output back to
/// [`WorktreeDiffRequest::publish`] until the capture publishes a candidate or
/// returns a typed failure. The terminal event loop therefore runs no `git`
/// command and performs no filesystem work of its own.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::sync::Arc;
///
/// use kvim_path::WorktreeRoot;
/// use kvim_workspace::{BaseRevision, DiffTarget, GIT_PROGRAM, WorktreeDiffRequest};
///
/// let root = Arc::new(WorktreeRoot::open(std::env::current_dir()?)?);
/// let base = BaseRevision::new("0123456789abcdef0123456789abcdef01234567")?;
/// let request = WorktreeDiffRequest::new(root, base, DiffTarget::Worktree);
/// let command = request.command();
///
/// assert_eq!(command.program, GIT_PROGRAM);
/// assert_eq!(command.current_dir.as_deref(), Some(request.root().as_path()));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeDiffRequest {
    policy: GitExecutionPolicy,
    base: BaseRevision,
    target: DiffTarget,
    attempt: usize,
    pass: CapturePass,
    step: CaptureStep,
    reads: PassReads,
    initial: Option<CaptureFingerprint>,
    candidate: Option<Box<WorktreeDiff>>,
    collected: Option<CaptureFingerprint>,
}

impl WorktreeDiffRequest {
    /// Creates one capture of one caller-supplied base commit.
    ///
    /// Kvim discovers no review base. The caller names the full commit object
    /// identifier that the review compares against.
    #[must_use]
    pub fn new(root: Arc<WorktreeRoot>, base: BaseRevision, target: DiffTarget) -> Self {
        Self {
            policy: GitExecutionPolicy::new(root),
            base,
            target,
            attempt: 0,
            pass: CapturePass::Initial,
            step: CaptureStep::BaseKind,
            reads: PassReads::default(),
            initial: None,
            candidate: None,
            collected: None,
        }
    }

    /// Returns the workspace root that this capture reads.
    #[must_use]
    pub fn root(&self) -> &WorktreeRoot {
        self.policy.root()
    }

    /// Returns the commit that the capture compares against.
    #[must_use]
    pub const fn base(&self) -> BaseRevision {
        self.base
    }

    /// Returns the selection that the capture publishes.
    #[must_use]
    pub const fn target(&self) -> &DiffTarget {
        &self.target
    }

    /// Returns the bounded command of the current step.
    ///
    /// Every command carries the complete [`GitExecutionPolicy`], a fixed
    /// locale, an explicit output bound, and the capture deadline.
    #[must_use]
    pub fn command(&self) -> ProcessRequest {
        let base = self.base.to_hex();
        let mut arguments: Vec<&str> = Vec::new();
        let output_bytes_max = match self.step {
            CaptureStep::BaseKind => {
                arguments.extend(BASE_KIND_ARGUMENTS);
                arguments.push(&base);
                DIFF_ANSWER_OUTPUT_BYTES_MAX
            }
            CaptureStep::Head => {
                arguments.extend(HEAD_ARGUMENTS);
                DIFF_ANSWER_OUTPUT_BYTES_MAX
            }
            CaptureStep::Index => {
                arguments.extend(INDEX_ARGUMENTS);
                DIFF_PROCESS_OUTPUT_BYTES_MAX
            }
            CaptureStep::Status => {
                arguments.extend(STATUS_ARGUMENTS);
                DIFF_PROCESS_OUTPUT_BYTES_MAX
            }
            CaptureStep::Untracked => {
                arguments.extend(UNTRACKED_ARGUMENTS);
                DIFF_PROCESS_OUTPUT_BYTES_MAX
            }
            CaptureStep::Raw => {
                arguments.extend(DIFF_ARGUMENTS);
                arguments.extend(RAW_ARGUMENTS);
                arguments.push(&base);
                DIFF_PROCESS_OUTPUT_BYTES_MAX
            }
            CaptureStep::Patch => {
                arguments.extend(DIFF_ARGUMENTS);
                arguments.extend(PATCH_ARGUMENTS);
                arguments.push(&base);
                DIFF_PROCESS_OUTPUT_BYTES_MAX
            }
        };

        let mut request = self.policy.command(&arguments);
        // The pathspec follows the separator, so a path that starts with a
        // hyphen stays a path. A path is not always valid text, so the argument
        // keeps its exact bytes.
        if let Some(pathspec) = self.pathspec() {
            request.args.push(OsString::from(PATHSPEC_SEPARATOR));
            request.args.push(pathspec);
        }
        request.child_variables.extend(
            CAPTURE_VARIABLES
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        request.output_bytes_max = output_bytes_max;
        request.deadline = DIFF_CAPTURE_DEADLINE;
        request
    }

    /// Returns the pathspec of the current step, or `None` while it takes none.
    ///
    /// The two diff reads always cover the complete worktree root, because
    /// rename detection inside one pathspec would split the pair that a
    /// one-path target must publish whole. The publication filters the parsed
    /// records instead, so a one-path capture still names only its own file.
    fn pathspec(&self) -> Option<OsString> {
        match self.step {
            CaptureStep::BaseKind | CaptureStep::Head => None,
            CaptureStep::Raw | CaptureStep::Patch => Some(OsString::from(ROOT_PATHSPEC)),
            CaptureStep::Index | CaptureStep::Status | CaptureStep::Untracked => {
                Some(match &self.target {
                    DiffTarget::Worktree => OsString::from(ROOT_PATHSPEC),
                    DiffTarget::Path(path) => path.as_path().as_os_str().to_os_string(),
                })
            }
        }
    }

    /// Turns the captured output of the current step into the next step.
    ///
    /// The call runs on the bounded process service, never on the terminal
    /// event loop. The patch step also reads the exact bytes of every untracked
    /// file through the confined worktree root.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeDiffFailure::BaseUnavailable`] when the base names no
    /// commit object, [`WorktreeDiffFailure::ChangedDuringCapture`] when the
    /// repository changed through every attempt, and
    /// [`WorktreeDiffFailure::Unavailable`] when Git refused a command or wrote
    /// a malformed answer.
    pub fn publish(
        mut self,
        output: &ProcessOutput,
    ) -> Result<WorktreeDiffRead, WorktreeDiffFailure> {
        // Git reports every refusal through its exit code. The unborn branch is
        // the one refusal that the capture accepts, and it carries its own code.
        let succeeded = output.status_code == Some(0);
        match self.step {
            CaptureStep::BaseKind => {
                if !succeeded || trimmed(&output.stdout) != COMMIT_TYPE {
                    return Err(WorktreeDiffFailure::BaseUnavailable);
                }
                self.step = CaptureStep::Head;
            }
            CaptureStep::Head => {
                self.reads.head = Some(match output.status_code {
                    Some(0) => HeadAuthority::Commit(
                        str::from_utf8(trimmed(&output.stdout))
                            .ok()
                            .and_then(|hex| BaseRevision::new(hex).ok())
                            .ok_or(WorktreeDiffFailure::Unavailable)?,
                    ),
                    Some(UNBORN_HEAD_EXIT) => HeadAuthority::Unborn,
                    _ => return Err(WorktreeDiffFailure::Unavailable),
                });
                self.step = CaptureStep::Index;
            }
            CaptureStep::Index => {
                let stdout = require(succeeded, &output.stdout)?;
                self.reads.index = Some(IndexAuthority::from_digest(digest(INDEX_DOMAIN, stdout)));
                self.step = CaptureStep::Status;
            }
            CaptureStep::Status => {
                let stdout = require(succeeded, &output.stdout)?;
                self.reads.status = Some(digest(STATUS_DOMAIN, stdout));
                self.step = CaptureStep::Untracked;
            }
            CaptureStep::Untracked => {
                let stdout = require(succeeded, &output.stdout)?;
                self.reads.untracked = Some(parse_paths(stdout));
                self.step = CaptureStep::Raw;
            }
            CaptureStep::Raw => {
                let stdout = require(succeeded, &output.stdout)?;
                self.reads.raw =
                    Some(parse_raw_records(stdout).ok_or(WorktreeDiffFailure::Unavailable)?);
                self.step = CaptureStep::Patch;
            }
            CaptureStep::Patch => {
                let stdout = require(succeeded, &output.stdout)?;
                return self.finish_pass(stdout);
            }
        }
        Ok(WorktreeDiffRead::Pending(Box::new(self)))
    }

    /// Builds the candidate of the finished pass and advances the capture.
    fn finish_pass(mut self, patch: &[u8]) -> Result<WorktreeDiffRead, WorktreeDiffFailure> {
        let head = self.reads.head.ok_or(WorktreeDiffFailure::Unavailable)?;
        let index = self.reads.index.ok_or(WorktreeDiffFailure::Unavailable)?;
        let status = self.reads.status.ok_or(WorktreeDiffFailure::Unavailable)?;
        let untracked = self
            .reads
            .untracked
            .take()
            .ok_or(WorktreeDiffFailure::Unavailable)?;
        let raw = self
            .reads
            .raw
            .take()
            .ok_or(WorktreeDiffFailure::Unavailable)?;

        let sections = split_sections(patch).ok_or(WorktreeDiffFailure::Unavailable)?;
        let mut collected =
            collect_files(&raw, &sections).ok_or(WorktreeDiffFailure::Unavailable)?;
        collected.extend(untracked_files(self.policy.root(), &untracked));

        let (files, truncation) = select_files(collected, &self.target);
        let authority = CandidateAuthority::new(head, index);
        let candidate = WorktreeDiff::new(
            self.base,
            self.target.clone(),
            &authority,
            files,
            truncation,
        )
        .map_err(|_| WorktreeDiffFailure::Unavailable)?;
        let fingerprint = CaptureFingerprint {
            head,
            index,
            status,
            projection: AuthorityProjection::of(&candidate),
        };

        self.reads = PassReads::default();
        self.step = CaptureStep::Head;
        match self.pass {
            CapturePass::Initial => {
                self.initial = Some(fingerprint);
                self.pass = CapturePass::Candidate;
            }
            CapturePass::Candidate => {
                self.candidate = Some(Box::new(candidate));
                self.collected = Some(fingerprint);
                self.pass = CapturePass::Final;
            }
            CapturePass::Final => return self.decide(fingerprint),
        }
        Ok(WorktreeDiffRead::Pending(Box::new(self)))
    }

    /// Compares the three authority values and publishes or retries.
    ///
    /// The initial fingerprint, the candidate projection, and the final
    /// fingerprint must all match. The final fingerprint of a refused attempt
    /// becomes the initial fingerprint of the next one, because it names the
    /// state that the retry starts from.
    fn decide(mut self, last: CaptureFingerprint) -> Result<WorktreeDiffRead, WorktreeDiffFailure> {
        let initial = self.initial.ok_or(WorktreeDiffFailure::Unavailable)?;
        let collected = self.collected.ok_or(WorktreeDiffFailure::Unavailable)?;
        let candidate = self
            .candidate
            .take()
            .ok_or(WorktreeDiffFailure::Unavailable)?;
        if initial == collected && collected == last {
            return Ok(WorktreeDiffRead::Published(candidate));
        }

        self.attempt += 1;
        if self.attempt >= DIFF_CAPTURE_ATTEMPTS_MAX {
            return Err(WorktreeDiffFailure::ChangedDuringCapture);
        }
        self.initial = Some(last);
        self.collected = None;
        self.pass = CapturePass::Candidate;
        self.step = CaptureStep::Head;
        Ok(WorktreeDiffRead::Pending(Box::new(self)))
    }
}

/// Returns the captured output of one command that must have succeeded.
fn require(succeeded: bool, stdout: &[u8]) -> Result<&[u8], WorktreeDiffFailure> {
    succeeded
        .then_some(stdout)
        .ok_or(WorktreeDiffFailure::Unavailable)
}

/// Returns one domain-separated digest of captured bytes.
fn digest(domain: &[u8], bytes: &[u8]) -> [u8; DIGEST_BYTES] {
    let mut hasher = Hasher::new();
    hasher.update(domain);
    absorb(&mut hasher, bytes);
    *hasher.finalize().as_bytes()
}

/// Returns the answer of one short command without its line terminator.
fn trimmed(stdout: &[u8]) -> &[u8] {
    let mut end = stdout.len();
    while end > 0 && matches!(stdout[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &stdout[..end]
}

// ---------------------------------------------------------------------------
// The raw record listing
// ---------------------------------------------------------------------------

/// The kind of change that one `git diff --raw` record names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawStatus {
    /// The candidate holds a file that the base does not hold.
    Added,
    /// The base holds a file that the candidate does not hold.
    Deleted,
    /// The content or the permission of one path changed.
    Modified,
    /// The kind of one path changed, for example a file into a symbolic link.
    TypeChanged,
    /// The candidate holds the file under another path.
    Renamed,
    /// The candidate holds one further copy of the file.
    Copied,
    /// A state that this release does not read, for example an unresolved
    /// merge.
    Other,
}

impl RawStatus {
    /// Returns the status of one record, or `None` for an unknown letter.
    fn parse(field: &str) -> Option<Self> {
        Some(match field.as_bytes().first()? {
            b'A' => Self::Added,
            b'D' => Self::Deleted,
            b'M' => Self::Modified,
            b'T' => Self::TypeChanged,
            b'R' => Self::Renamed,
            b'C' => Self::Copied,
            b'U' | b'X' => Self::Other,
            _ => return None,
        })
    }

    /// Returns the number of path fields that the record holds.
    const fn paths(self) -> usize {
        match self {
            Self::Renamed | Self::Copied => 2,
            Self::Added | Self::Deleted | Self::Modified | Self::TypeChanged | Self::Other => 1,
        }
    }

    /// Returns the number of patch sections that the record owns.
    ///
    /// A Git blob holds no kind, so Git publishes a type change as one removal
    /// section and one creation section while the raw listing names one record.
    const fn sections(self) -> usize {
        match self {
            Self::TypeChanged => 2,
            Self::Added
            | Self::Deleted
            | Self::Modified
            | Self::Renamed
            | Self::Copied
            | Self::Other => 1,
        }
    }
}

/// One parsed record of the `git diff --raw -z` listing.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RawRecord {
    status: RawStatus,
    old_mode: FileMode,
    new_mode: FileMode,
    old_path: Option<WorktreeRelativePath>,
    new_path: Option<WorktreeRelativePath>,
}

impl RawRecord {
    /// Returns the published modes of the sides that the change holds.
    fn published_modes(&self) -> Vec<FileMode> {
        match self.status {
            RawStatus::Added | RawStatus::Copied => vec![self.new_mode],
            RawStatus::Deleted => vec![self.old_mode],
            RawStatus::Modified
            | RawStatus::TypeChanged
            | RawStatus::Renamed
            | RawStatus::Other => vec![self.old_mode, self.new_mode],
        }
    }

    /// Returns the two published sides of the change.
    fn change(&self) -> Option<DiffChange> {
        let old = self
            .old_path
            .clone()
            .map(|path| FileSide::new(path, self.old_mode));
        let new = self
            .new_path
            .clone()
            .map(|path| FileSide::new(path, self.new_mode));
        Some(match self.status {
            RawStatus::Added | RawStatus::Copied => DiffChange::Added { new: new? },
            RawStatus::Deleted => DiffChange::Deleted { old: old? },
            RawStatus::Renamed => DiffChange::Renamed {
                old: old?,
                new: new?,
            },
            RawStatus::Modified | RawStatus::TypeChanged | RawStatus::Other => {
                DiffChange::Modified {
                    old: old?,
                    new: new?,
                }
            }
        })
    }

    /// Returns the reviewable content of the change.
    ///
    /// The published modes decide the kind, so a symbolic link, a submodule,
    /// and a mode that this release does not read never carry text hunks. Only
    /// a change whose every side stores file content reaches the patch body.
    fn content(&self, section: &Section<'_>) -> DiffContent {
        let modes = self.published_modes();
        if modes
            .iter()
            .any(|mode| matches!(mode, FileMode::Unsupported(_)))
        {
            return DiffContent::Unsupported;
        }
        if modes.contains(&FileMode::Submodule) {
            return DiffContent::Submodule;
        }
        if modes.contains(&FileMode::SymbolicLink) {
            return DiffContent::SymbolicLink;
        }
        if section.binary {
            return DiffContent::Binary;
        }
        DiffContent::Text(text_of(section))
    }
}

/// Parses the `git diff --raw -z` listing of one pass.
///
/// The listing separates every field with a NUL byte, so no path needs quoting
/// and no name can hide a field boundary. A malformed listing returns `None`,
/// because a partial listing cannot name the sections of the patch.
fn parse_raw_records(stdout: &[u8]) -> Option<Vec<RawRecord>> {
    let mut records = Vec::new();
    let mut fields = stdout.split(|byte| *byte == 0);
    while let Some(meta) = fields.next() {
        // The listing ends with a separator, so the last field is empty.
        if meta.is_empty() {
            continue;
        }
        let meta = str::from_utf8(meta).ok()?.strip_prefix(':')?;
        let mut parts = meta.split(' ').filter(|part| !part.is_empty());
        let old_mode = FileMode::from_octal(parts.next()?).ok()?;
        let new_mode = FileMode::from_octal(parts.next()?).ok()?;
        let (_old_object, _new_object) = (parts.next()?, parts.next()?);
        let status = RawStatus::parse(parts.next()?)?;

        let first = relative_path(fields.next()?);
        let (old_path, new_path) = if status.paths() == 2 {
            (first, relative_path(fields.next()?))
        } else {
            (first.clone(), first)
        };
        records.push(RawRecord {
            status,
            old_mode,
            new_mode,
            old_path,
            new_path,
        });
    }
    Some(records)
}

/// Returns one contained relative path of a NUL separated listing.
///
/// A name that is not text names no reviewable file, and Git closes a listed
/// untracked directory with a separator. Neither publishes one file side.
fn relative_path(bytes: &[u8]) -> Option<WorktreeRelativePath> {
    let text = str::from_utf8(bytes).ok()?;
    if text.ends_with('/') {
        return None;
    }
    WorktreeRelativePath::new(text).ok()
}

/// Parses one NUL separated listing of contained relative paths.
fn parse_paths(stdout: &[u8]) -> Vec<WorktreeRelativePath> {
    stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .filter_map(relative_path)
        .collect()
}

// ---------------------------------------------------------------------------
// The patch body
// ---------------------------------------------------------------------------

/// The two ranges that one hunk header names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HunkHeader {
    old_first: u32,
    old_count: u32,
    new_first: u32,
    new_count: u32,
}

/// One hunk header and the exact body lines that follow it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedHunk<'a> {
    header: HunkHeader,
    body: Vec<&'a [u8]>,
}

/// One file section of the patch output.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Section<'a> {
    binary: bool,
    hunks: Vec<ParsedHunk<'a>>,
}

/// Splits the patch output into one section for each published file.
///
/// A body line always carries one prefix byte, so the section marker can start
/// no body line. The parser also consumes exactly the number of lines that each
/// hunk header names, so no content can open a section.
fn split_sections(patch: &[u8]) -> Option<Vec<Section<'_>>> {
    let lines = split_lines(patch);
    let mut sections = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !lines[index].starts_with(SECTION_MARKER) {
            index += 1;
            continue;
        }
        index += 1;
        let mut section = Section::default();
        while index < lines.len() && !lines[index].starts_with(SECTION_MARKER) {
            let line = lines[index];
            if BINARY_MARKERS.iter().any(|marker| line.starts_with(marker)) {
                section.binary = true;
                index += 1;
                continue;
            }
            if !line.starts_with(HUNK_MARKER) {
                index += 1;
                continue;
            }
            let header = parse_hunk_header(line)?;
            index += 1;
            let mut body = Vec::new();
            let (mut old_left, mut new_left) = (header.old_count, header.new_count);
            while index < lines.len() && (old_left > 0 || new_left > 0) {
                let body_line = lines[index];
                match body_line.first() {
                    // The marker names the terminator of the line above it and
                    // belongs to no side of its own.
                    Some(b'\\') => {}
                    Some(b'-') => old_left = old_left.saturating_sub(1),
                    Some(b'+') => new_left = new_left.saturating_sub(1),
                    _ => {
                        old_left = old_left.saturating_sub(1);
                        new_left = new_left.saturating_sub(1);
                    }
                }
                body.push(body_line);
                index += 1;
            }
            while index < lines.len() && lines[index].first() == Some(&b'\\') {
                body.push(lines[index]);
                index += 1;
            }
            section.hunks.push(ParsedHunk { header, body });
        }
        sections.push(section);
    }
    Some(sections)
}

/// Splits captured output into lines without their terminators.
fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    lines
}

/// Parses one `@@ -<first>,<count> +<first>,<count> @@` header.
fn parse_hunk_header(line: &[u8]) -> Option<HunkHeader> {
    let rest = line.strip_prefix(HUNK_MARKER)?;
    let text = str::from_utf8(rest).ok()?;
    let end = text.find("@@")?;
    let mut ranges = text[..end].split_whitespace();
    let (old_first, old_count) = parse_range(ranges.next()?.strip_prefix('-')?)?;
    let (new_first, new_count) = parse_range(ranges.next()?.strip_prefix('+')?)?;
    Some(HunkHeader {
        old_first,
        old_count,
        new_first,
        new_count,
    })
}

/// Parses one `<first>[,<count>]` range of a hunk header.
fn parse_range(field: &str) -> Option<(u32, u32)> {
    match field.split_once(',') {
        Some((first, count)) => Some((first.parse().ok()?, count.parse().ok()?)),
        None => Some((field.parse().ok()?, 1)),
    }
}

/// Builds the published hunks of one text section.
///
/// A hunk that passes a published bound stops the collection of this file. The
/// hunks above it stay exact, and the file reports the bound that stopped it.
fn text_of(section: &Section<'_>) -> TextDiff {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut truncation = DiffTruncation::Complete;
    for parsed in &section.hunks {
        if hunks.len() >= DIFF_FILE_HUNKS_MAX {
            truncation = DiffTruncation::Truncated(DiffLimit::Hunks);
            break;
        }
        let id = HunkId::new(u32::try_from(hunks.len()).expect("the hunk bound fits one u32"));
        let Some(hunk) = build_hunk(id, parsed) else {
            truncation = DiffTruncation::Truncated(DiffLimit::Lines);
            break;
        };
        hunks.push(hunk);
    }
    TextDiff::new(hunks, truncation).unwrap_or_else(|_| empty_text(DiffLimit::Hunks))
}

/// Returns one text diff that publishes no line and names its bound.
fn empty_text(limit: DiffLimit) -> TextDiff {
    TextDiff::new(Vec::new(), DiffTruncation::Truncated(limit))
        .expect("an empty set of hunks needs no order")
}

/// Builds one published hunk from its header and its exact body lines.
fn build_hunk(id: HunkId, parsed: &ParsedHunk<'_>) -> Option<Hunk> {
    let old_first = parsed.header.old_first.max(1);
    let new_first = parsed.header.new_first.max(1);
    let old_range =
        OldLineRange::new(OldLine::new(old_first).ok()?, parsed.header.old_count).ok()?;
    let new_range =
        NewLineRange::new(NewLine::new(new_first).ok()?, parsed.header.new_count).ok()?;

    let (mut old_number, mut new_number) = (old_first, new_first);
    let mut lines: Vec<DiffLine> = Vec::new();
    for line in &parsed.body {
        if line.first() == Some(&b'\\') {
            let last = lines.pop()?;
            lines.push(DiffLine::new(
                last.origin(),
                last.text().clone(),
                LineEnding::EndOfFile,
            ));
            continue;
        }
        // A body line always carries one prefix byte. Git writes one space for
        // an unchanged line, and some releases write an empty line instead.
        let text = DiffLineText::new(line.get(1..).unwrap_or_default()).ok()?;
        let origin = match line.first() {
            Some(b'-') => {
                let origin = LineOrigin::Removed {
                    old: OldLine::new(old_number).ok()?,
                };
                old_number = old_number.saturating_add(1);
                origin
            }
            Some(b'+') => {
                let origin = LineOrigin::Added {
                    new: NewLine::new(new_number).ok()?,
                };
                new_number = new_number.saturating_add(1);
                origin
            }
            _ => {
                let origin = LineOrigin::Context {
                    old: OldLine::new(old_number).ok()?,
                    new: NewLine::new(new_number).ok()?,
                };
                old_number = old_number.saturating_add(1);
                new_number = new_number.saturating_add(1);
                origin
            }
        };
        lines.push(DiffLine::new(origin, text, LineEnding::Newline));
    }
    Hunk::new(id, old_range, new_range, lines).ok()
}

// ---------------------------------------------------------------------------
// The collected candidate
// ---------------------------------------------------------------------------

/// Joins the raw listing with the patch sections of one pass.
///
/// The two diff reads carry the same rename detection, so their file order is
/// one order. A listing that does not account for every section returns `None`,
/// because the capture must never attach the lines of one file to another.
fn collect_files(records: &[RawRecord], sections: &[Section<'_>]) -> Option<Vec<FileDiff>> {
    let mut files = Vec::new();
    let mut index = 0;
    for record in records {
        let owned = record.status.sections();
        let section = sections.get(index)?;
        index = index
            .checked_add(owned)
            .filter(|next| *next <= sections.len())?;
        let Some(change) = record.change() else {
            continue;
        };
        if let Ok(file) = FileDiff::new(change, record.content(section)) {
            files.push(file);
        }
    }
    (index == sections.len()).then_some(files)
}

/// Builds one added file for every untracked worktree entry.
///
/// Git publishes no patch for an untracked file, so the capture reads its exact
/// source bytes through the confined worktree root. An entry that leaves the
/// root, that disappeared, or that names no file publishes nothing.
fn untracked_files(root: &WorktreeRoot, paths: &[WorktreeRelativePath]) -> Vec<FileDiff> {
    paths
        .iter()
        .filter_map(|path| untracked_file(root, path))
        .collect()
}

/// Builds one added file for one untracked worktree entry.
fn untracked_file(root: &WorktreeRoot, path: &WorktreeRelativePath) -> Option<FileDiff> {
    let resolved = root.resolve(path).ok()?;
    if resolved.state() != ResolvedTargetState::Existing {
        return None;
    }
    // The kind comes from the entry itself, never from its link target, so a
    // link and the file that it names stay distinct.
    let metadata = root.directory().symlink_metadata(path.as_path()).ok()?;
    if metadata.is_symlink() {
        let side = FileSide::new(path.clone(), FileMode::SymbolicLink);
        return FileDiff::new(DiffChange::Added { new: side }, DiffContent::SymbolicLink).ok();
    }
    if !metadata.is_file() {
        return None;
    }

    // Git stores one permission bit, so every other permission of the host maps
    // onto the two published file modes.
    let mode = if metadata.mode() & 0o111 == 0 {
        FileMode::Regular
    } else {
        FileMode::Executable
    };
    let content = match read_source(root, resolved.path()) {
        None => DiffContent::Text(empty_text(DiffLimit::SourceBytes)),
        Some(bytes) if is_binary(&bytes) => DiffContent::Binary,
        Some(bytes) => DiffContent::Text(added_text(&bytes)),
    };
    let side = FileSide::new(path.clone(), mode);
    FileDiff::new(DiffChange::Added { new: side }, content).ok()
}

/// Reads the exact source bytes of one contained file inside its bound.
///
/// A file above [`DIFF_SOURCE_BYTES_MAX`] returns `None`, so no part of an
/// oversized file reaches a review that cannot show the rest of it.
fn read_source(root: &WorktreeRoot, path: &WorktreeRelativePath) -> Option<Vec<u8>> {
    let file = root.directory().open(path.as_path()).ok()?;
    // One byte above the bound proves that the file passed it.
    let limit = u64::try_from(DIFF_SOURCE_BYTES_MAX).ok()?.checked_add(1)?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= DIFF_SOURCE_BYTES_MAX).then_some(bytes)
}

/// Reports whether one file holds content that no reader can review as text.
///
/// Git inspects the same leading window for one NUL byte, so a tracked file and
/// an untracked file take the same answer.
fn is_binary(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .take(DIFF_BINARY_SCAN_BYTES)
        .any(|byte| *byte == 0)
}

/// Publishes the complete content of one untracked file as added lines.
fn added_text(bytes: &[u8]) -> TextDiff {
    let (lines, final_newline) = source_lines(bytes);
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut truncation = DiffTruncation::Complete;
    let mut first: u32 = 1;
    let chunks: Vec<&[&[u8]]> = lines.chunks(DIFF_HUNK_LINES_MAX).collect();
    for (position, chunk) in chunks.iter().enumerate() {
        if hunks.len() >= DIFF_FILE_HUNKS_MAX {
            truncation = DiffTruncation::Truncated(DiffLimit::Hunks);
            break;
        }
        let last_chunk = position + 1 == chunks.len();
        let Some(hunk) = added_hunk(&mut first, chunk, last_chunk && !final_newline, &hunks) else {
            truncation = DiffTruncation::Truncated(DiffLimit::Lines);
            break;
        };
        hunks.push(hunk);
    }
    TextDiff::new(hunks, truncation).unwrap_or_else(|_| empty_text(DiffLimit::Hunks))
}

/// Builds one hunk of added lines and advances the new-side line number.
fn added_hunk(
    first: &mut u32,
    chunk: &[&[u8]],
    ends_without_newline: bool,
    hunks: &[Hunk],
) -> Option<Hunk> {
    let start = *first;
    let count = u32::try_from(chunk.len()).ok()?;
    let mut lines = Vec::with_capacity(chunk.len());
    for (offset, line) in chunk.iter().enumerate() {
        let number = start.checked_add(u32::try_from(offset).ok()?)?;
        let last = offset + 1 == chunk.len();
        let ending = if last && ends_without_newline {
            LineEnding::EndOfFile
        } else {
            LineEnding::Newline
        };
        lines.push(DiffLine::new(
            LineOrigin::Added {
                new: NewLine::new(number).ok()?,
            },
            DiffLineText::new(*line).ok()?,
            ending,
        ));
    }
    // An added file holds no old line, so every hunk names the same empty run
    // at the first old line.
    let old_range = OldLineRange::new(OldLine::new(1).ok()?, 0).ok()?;
    let new_range = NewLineRange::new(NewLine::new(start).ok()?, count).ok()?;
    let id = HunkId::new(u32::try_from(hunks.len()).ok()?);
    *first = start.checked_add(count)?;
    Hunk::new(id, old_range, new_range, lines).ok()
}

/// Splits exact source bytes into lines and reports the final terminator.
fn source_lines(bytes: &[u8]) -> (Vec<&[u8]>, bool) {
    if bytes.is_empty() {
        return (Vec::new(), true);
    }
    let final_newline = bytes.last() == Some(&b'\n');
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if final_newline {
        lines.pop();
    }
    (lines, final_newline)
}

/// Keeps the files that the target selects and orders them by path.
///
/// The publication bound stops at [`DIFF_FILES_MAX`] files. The remaining
/// files are omitted, and the candidate reports the bound that stopped it.
fn select_files(mut files: Vec<FileDiff>, target: &DiffTarget) -> (Vec<FileDiff>, DiffTruncation) {
    files.retain(|file| target.selects(file.change()));
    files.sort_by(|left, right| left.path().cmp(right.path()));
    files.dedup_by(|later, earlier| later.path() == earlier.path());
    if files.len() > DIFF_FILES_MAX {
        files.truncate(DIFF_FILES_MAX);
        return (files, DiffTruncation::Truncated(DiffLimit::Files));
    }
    (files, DiffTruncation::Complete)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use kvim_path::{WorktreeRelativePath, WorktreeRoot};
    use kvim_runtime::{
        ProcessOutput, ProcessRequest, PublicationGate, RequestSlot, Runtime, RuntimeLimits,
        SubmitError,
    };

    use crate::diff::{
        DiffChange, DiffContent, DiffSide, DiffTarget, FileMode, LineOrigin, WorktreeDiff,
    };
    use crate::temp::TempRepository;

    use super::{
        BaseRevision, DIFF_CAPTURE_ATTEMPTS_MAX, RawStatus, Section, WorktreeDiffFailure,
        WorktreeDiffRead, WorktreeDiffRequest, collect_files, parse_raw_records, split_sections,
    };

    /// The number of commands that one complete pass of the capture runs.
    const PASS_COMMANDS: usize = 6;

    /// The largest number of commands that one exhausted capture runs.
    ///
    /// The base check runs once. The first attempt adds one authority pass, and
    /// every attempt adds one collection pass and one authority pass.
    const CAPTURE_COMMANDS_MAX: usize =
        1 + PASS_COMMANDS + DIFF_CAPTURE_ATTEMPTS_MAX * 2 * PASS_COMMANDS;

    /// The identifier that names no object of any fixture repository.
    const ABSENT_BASE: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Runs one bounded command through the process service of the editor.
    async fn run(command: ProcessRequest) -> ProcessOutput {
        let limits = RuntimeLimits::new(1, 1, 1).expect("every capacity is nonzero");
        let (runtime, mut events) = Runtime::<ProcessOutput>::with_limits(limits);
        let handle =
            PublicationGate::default().begin(RequestSlot::new(1), &runtime.cancellation_root());
        let submitted: Result<(), SubmitError> =
            runtime.submit_process(handle, command, |output| output);
        submitted.expect("the isolated runtime holds one free permit");
        let event = events
            .recv()
            .await
            .expect("every accepted request produces one result");
        let output = event
            .result
            .expect("the development shell and the build sandbox both provide git");
        runtime.shutdown().await;
        output
    }

    /// Captures one worktree diff through the bounded process service.
    fn capture(
        root: &Path,
        base: &str,
        target: DiffTarget,
    ) -> Result<WorktreeDiff, WorktreeDiffFailure> {
        capture_with(root, base, target, |_| {})
    }

    /// Captures one worktree diff and lets the caller change the repository.
    ///
    /// The callback runs before every command, so a test can place one change
    /// exactly between two passes of the capture.
    fn capture_with<F>(
        root: &Path,
        base: &str,
        target: DiffTarget,
        mut before: F,
    ) -> Result<WorktreeDiff, WorktreeDiffFailure>
    where
        F: FnMut(usize),
    {
        let root = Arc::new(WorktreeRoot::open(root).expect("the fixture root is one directory"));
        let base = BaseRevision::new(base).expect("the fixture names one full identifier");
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("the test host starts one Tokio runtime");
        tokio.block_on(async move {
            let mut request = WorktreeDiffRequest::new(root, base, target);
            for step in 0..CAPTURE_COMMANDS_MAX {
                before(step);
                let output = run(request.command()).await;
                match request.publish(&output)? {
                    WorktreeDiffRead::Pending(next) => request = *next,
                    WorktreeDiffRead::Published(diff) => return Ok(*diff),
                }
            }
            unreachable!("one capture publishes or fails inside {CAPTURE_COMMANDS_MAX} commands");
        })
    }

    /// The first command of one pass, counted from the base check.
    const fn pass_start(pass: usize) -> usize {
        1 + pass * PASS_COMMANDS
    }

    fn path(value: &str) -> WorktreeRelativePath {
        WorktreeRelativePath::new(value).expect("the fixture names one contained path")
    }

    /// Returns the exact new-side bytes that one published file holds.
    fn new_bytes(diff: &WorktreeDiff, name: &str) -> Vec<u8> {
        let file = diff
            .file(&path(name))
            .unwrap_or_else(|| panic!("the candidate holds {name}"));
        match file.content() {
            DiffContent::Text(text) => text.side_bytes(DiffSide::New),
            other => panic!("{name} publishes {other:?} instead of text"),
        }
    }

    #[test]
    fn publishes_commits_after_the_base_and_dirty_changes() {
        let repository = TempRepository::new("diff-committed-and-dirty");
        repository.file("committed.txt", "one\ntwo\n");
        repository.file("dirty.txt", "three\nfour\n");
        repository.commit("base");
        let base = repository.head();

        repository.file("committed.txt", "one\nTWO\n");
        repository.commit("after the base");
        repository.file("dirty.txt", "three\nFOUR\n");

        let diff = capture(repository.path(), &base, DiffTarget::Worktree)
            .expect("the fixture holds one reachable base");

        assert_eq!(diff.files().len(), 2);
        assert_eq!(new_bytes(&diff, "committed.txt"), b"one\nTWO\n");
        assert_eq!(new_bytes(&diff, "dirty.txt"), b"three\nFOUR\n");
    }

    #[test]
    fn publishes_a_clean_commit_after_the_base() {
        let repository = TempRepository::new("diff-clean-commit");
        repository.file("only.txt", "first\n");
        repository.commit("base");
        let base = repository.head();

        repository.file("only.txt", "second\n");
        repository.commit("after the base");

        let diff = capture(repository.path(), &base, DiffTarget::Worktree)
            .expect("a clean worktree above the base stays reviewable");

        assert_eq!(diff.files().len(), 1);
        assert_eq!(new_bytes(&diff, "only.txt"), b"second\n");
    }

    #[test]
    fn publishes_a_rename_pair_under_either_name() {
        let repository = TempRepository::new("diff-rename");
        repository.file("old.txt", "stable content of the renamed file\n");
        repository.commit("base");
        let base = repository.head();

        repository.git(&["mv", "old.txt", "new.txt"]);
        repository.commit("rename");

        let diff = capture(repository.path(), &base, DiffTarget::Path(path("old.txt")))
            .expect("the one-path target names the old side of the rename");

        assert_eq!(diff.files().len(), 1);
        let file = diff.files().first().expect("the candidate holds the pair");
        let DiffChange::Renamed { old, new } = file.change() else {
            panic!(
                "the capture published {:?} instead of a rename",
                file.change()
            );
        };
        assert_eq!(old.path(), &path("old.txt"));
        assert_eq!(new.path(), &path("new.txt"));
        assert!(diff.file(&path("new.txt")).is_some());
    }

    #[test]
    fn publishes_the_exact_content_of_an_untracked_file() {
        let repository = TempRepository::new("diff-untracked");
        repository.file("tracked.txt", "tracked\n");
        repository.commit("base");
        let base = repository.head();

        repository.file("fresh.txt", "alpha\nbeta");

        let diff = capture(repository.path(), &base, DiffTarget::Worktree)
            .expect("an untracked file needs no commit");

        assert_eq!(diff.files().len(), 1);
        // The file holds no final line feed, and the candidate keeps that.
        assert_eq!(new_bytes(&diff, "fresh.txt"), b"alpha\nbeta");
        let file = diff
            .file(&path("fresh.txt"))
            .expect("the path is published");
        assert!(matches!(file.change(), DiffChange::Added { .. }));
    }

    #[test]
    fn rejects_a_base_that_names_no_commit() {
        let repository = TempRepository::new("diff-absent-base");
        repository.file("only.txt", "one\n");
        repository.commit("base");
        let tree = repository.head();

        assert_eq!(
            capture(repository.path(), ABSENT_BASE, DiffTarget::Worktree),
            Err(WorktreeDiffFailure::BaseUnavailable)
        );
        // The identifier of one commit stays available, so the fixture proves
        // that only the absent object is refused.
        assert!(capture(repository.path(), &tree, DiffTarget::Worktree).is_ok());
    }

    #[test]
    fn records_every_file_side_without_guessing() {
        let repository = TempRepository::new("diff-sides");
        repository.file("modified.txt", "one\n");
        repository.file("deleted.txt", "gone\n");
        repository.file("link.bin", "\u{0}\u{1}\u{2}before\n");
        repository.commit("base");
        let base = repository.head();

        repository.file("modified.txt", "ONE\n");
        std::fs::remove_file(repository.join("deleted.txt")).expect("the fixture holds the file");
        repository.file("link.bin", "\u{0}\u{1}\u{2}after\n");
        repository.file("added.txt", "added\n");
        repository.git(&["add", "added.txt"]);
        std::os::unix::fs::symlink("modified.txt", repository.join("link.txt"))
            .expect("the fixture host supports one symbolic link");

        let diff = capture(repository.path(), &base, DiffTarget::Worktree)
            .expect("the fixture holds one reachable base");

        let kinds: Vec<(&str, &DiffContent)> = diff
            .files()
            .iter()
            .map(|file| {
                (
                    file.path().as_path().to_str().expect("the fixture is text"),
                    file.content(),
                )
            })
            .collect();
        assert_eq!(kinds.len(), 5, "the candidate holds {kinds:?}");
        assert!(matches!(
            diff.file(&path("added.txt")).map(FileDiffKind::of),
            Some(FileDiffKind::Added)
        ));
        assert!(matches!(
            diff.file(&path("deleted.txt")).map(FileDiffKind::of),
            Some(FileDiffKind::Deleted)
        ));
        assert!(matches!(
            diff.file(&path("link.bin")).map(|file| file.content()),
            Some(DiffContent::Binary)
        ));
        assert!(matches!(
            diff.file(&path("link.txt")).map(|file| file.content()),
            Some(DiffContent::SymbolicLink)
        ));
        assert_eq!(new_bytes(&diff, "modified.txt"), b"ONE\n");
    }

    /// The kind of one published change, for a test that names it directly.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FileDiffKind {
        Added,
        Deleted,
        Modified,
        Renamed,
    }

    impl FileDiffKind {
        fn of(file: &crate::diff::FileDiff) -> Self {
            match file.change() {
                DiffChange::Added { .. } => Self::Added,
                DiffChange::Deleted { .. } => Self::Deleted,
                DiffChange::Modified { .. } => Self::Modified,
                DiffChange::Renamed { .. } => Self::Renamed,
            }
        }
    }

    #[test]
    fn refuses_a_repository_that_changes_through_every_attempt() {
        let repository = TempRepository::new("diff-changing");
        repository.file("busy.txt", "0\n");
        repository.commit("base");
        let base = repository.head();

        let outcome = capture_with(repository.path(), &base, DiffTarget::Worktree, |step| {
            repository.file("busy.txt", &format!("{step}\n"));
        });

        assert_eq!(outcome, Err(WorktreeDiffFailure::ChangedDuringCapture));
    }

    #[test]
    fn rejects_a_candidate_of_a_file_that_returns_to_its_first_content() {
        let repository = TempRepository::new("diff-file-race");
        repository.file("race.txt", "A\n");
        repository.commit("base commit");
        // The base holds the original content, so every state below is a change.
        repository.file("race.txt", "first\n");
        let base = repository.head();

        let diff = capture_with(repository.path(), &base, DiffTarget::Worktree, |step| {
            // The collection pass reads B, and both authority passes read A.
            if step == pass_start(1) {
                repository.file("race.txt", "second\n");
            }
            if step == pass_start(2) {
                repository.file("race.txt", "first\n");
            }
        })
        .expect("the retry collects one consistent candidate");

        assert_eq!(new_bytes(&diff, "race.txt"), b"first\n");
    }

    #[test]
    fn rejects_a_candidate_of_an_index_that_returns_to_its_first_state() {
        let repository = TempRepository::new("diff-index-race");
        repository.file("tracked.txt", "one\n");
        repository.commit("base");
        let base = repository.head();
        repository.file("staged.txt", "staged content\n");

        let diff = capture_with(repository.path(), &base, DiffTarget::Worktree, |step| {
            // The worktree never changes. Only the index leaves and returns to
            // its first state, so only the index digest rejects the candidate.
            if step == pass_start(1) {
                repository.git(&["add", "staged.txt"]);
            }
            if step == pass_start(2) {
                repository.git(&["rm", "--cached", "staged.txt"]);
            }
        })
        .expect("the retry collects one consistent candidate");

        // The worktree content proves nothing here, because it never changed.
        // The revision covers the index authority, so only a candidate that the
        // capture collected against the settled index carries this identity.
        let settled = capture(repository.path(), &base, DiffTarget::Worktree)
            .expect("the settled repository captures once more");
        assert_eq!(new_bytes(&diff, "staged.txt"), b"staged content\n");
        assert_eq!(diff.revision(), settled.revision());
    }

    #[test]
    fn starts_no_program_that_the_repository_names() {
        let repository = TempRepository::new("diff-hostile");
        let marker = repository.join("marker");
        let program = repository.join("hostile.sh");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ntouch {}\nexit 0\n",
                marker.to_str().expect("the fixture path is text")
            ),
        )
        .expect("the fixture writes one script");
        set_executable(&program);
        let named = program.to_str().expect("the fixture path is text");

        repository.file("source.txt", "one\n");
        repository.file(".gitattributes", "* diff=hostile\n");
        repository.commit("base");
        let base = repository.head();
        repository.file("source.txt", "two\n");

        for (name, value) in [
            ("core.askPass", named),
            ("core.editor", named),
            ("core.fsmonitor", named),
            ("core.pager", named),
            ("credential.helper", named),
            ("diff.external", named),
            ("diff.hostile.textconv", named),
        ] {
            repository.git(&["config", name, value]);
        }
        for hook in ["post-index-change", "pre-auto-gc", "reference-transaction"] {
            let installed = repository.join(&format!(".git/hooks/{hook}"));
            std::fs::copy(&program, &installed).expect("the fixture installs one hook");
            set_executable(&installed);
        }

        let diff = capture(repository.path(), &base, DiffTarget::Worktree)
            .expect("the policy keeps the read usable");

        assert_eq!(new_bytes(&diff, "source.txt"), b"two\n");
        assert!(
            !marker.exists(),
            "the policy must start no program that the repository names"
        );
    }

    /// Gives one fixture script the permission that Git needs to start it.
    fn set_executable(program: &Path) {
        use std::os::unix::fs::PermissionsExt as _;

        let mut permissions = std::fs::metadata(program)
            .expect("the fixture wrote the script")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(program, permissions).expect("the fixture owns the script");
    }

    // -----------------------------------------------------------------------
    // The pure listing and patch parsers
    // -----------------------------------------------------------------------

    fn sections(patch: &[u8]) -> Vec<Section<'_>> {
        split_sections(patch).expect("the fixture holds one well-formed patch")
    }

    #[test]
    fn publishes_a_submodule_side_without_lines() {
        let listing = b":160000 160000 1111111 2222222 M\0nested\0".as_slice();
        let patch = b"diff --git a/nested b/nested\nindex 1111111..2222222 160000\n--- a/nested\n+++ b/nested\n@@ -1 +1 @@\n-Subproject commit 1111111\n+Subproject commit 2222222\n";
        let records = parse_raw_records(listing).expect("the listing is well formed");

        let files = collect_files(&records, &sections(patch)).expect("every section is owned");

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content(), &DiffContent::Submodule);
        assert_eq!(
            files[0].change().new_side().map(|side| side.mode()),
            Some(FileMode::Submodule)
        );
    }

    #[test]
    fn publishes_an_unmerged_side_as_unsupported() {
        // Git names no mode for an unresolved entry, so neither side stores
        // reviewable content.
        let listing = b":000000 000000 0000000 0000000 U\0conflict.txt\0".as_slice();
        let patch = b"diff --git a/conflict.txt b/conflict.txt\n";
        let records = parse_raw_records(listing).expect("the listing is well formed");
        assert_eq!(records[0].status, RawStatus::Other);

        let files = collect_files(&records, &sections(patch)).expect("every section is owned");

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content(), &DiffContent::Unsupported);
    }

    #[test]
    fn owns_two_sections_for_one_type_change() {
        let listing = b":100644 120000 1111111 2222222 T\0target.txt\0".as_slice();
        let patch = b"diff --git a/target.txt b/target.txt\ndeleted file mode 100644\n--- a/target.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-plain\ndiff --git a/target.txt b/target.txt\nnew file mode 120000\n--- /dev/null\n+++ b/target.txt\n@@ -0,0 +1 @@\n+other.txt\n\\ No newline at end of file\n";
        let records = parse_raw_records(listing).expect("the listing is well formed");
        assert_eq!(sections(patch).len(), 2);

        let files = collect_files(&records, &sections(patch)).expect("every section is owned");

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content(), &DiffContent::SymbolicLink);
    }

    #[test]
    fn refuses_a_listing_that_leaves_one_section_unowned() {
        let listing = b":100644 100644 1111111 2222222 M\0one.txt\0".as_slice();
        let patch = b"diff --git a/one.txt b/one.txt\n--- a/one.txt\n+++ b/one.txt\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/two.txt b/two.txt\n--- a/two.txt\n+++ b/two.txt\n@@ -1 +1 @@\n-c\n+d\n";
        let records = parse_raw_records(listing).expect("the listing is well formed");

        assert!(collect_files(&records, &sections(patch)).is_none());
    }

    #[test]
    fn keeps_a_final_line_that_holds_no_line_feed() {
        let listing = b":100644 100644 1111111 2222222 M\0tail.txt\0".as_slice();
        let patch = b"diff --git a/tail.txt b/tail.txt\n--- a/tail.txt\n+++ b/tail.txt\n@@ -1,2 +1,2 @@\n keep\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n";
        let records = parse_raw_records(listing).expect("the listing is well formed");

        let files = collect_files(&records, &sections(patch)).expect("every section is owned");

        let DiffContent::Text(text) = files[0].content() else {
            panic!("a regular mode publishes text");
        };
        assert_eq!(text.side_bytes(DiffSide::Old), b"keep\nold");
        assert_eq!(text.side_bytes(DiffSide::New), b"keep\nnew");
        let lines = text.hunks()[0].lines();
        assert!(matches!(lines[0].origin(), LineOrigin::Context { .. }));
    }
}
