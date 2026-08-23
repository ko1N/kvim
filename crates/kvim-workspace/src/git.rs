//! The bounded read-only Git status of the workspace.
//!
//! The editor never runs `git` itself. [`GitStatusRequest::command`] builds one
//! [`ProcessRequest`], the bounded process service runs it, and
//! [`GitStatusRequest::publish`] turns the captured output into the next step of
//! the read. One read takes two commands: the first names the place of the
//! worktree root inside its repository, and the second collects the status
//! records. The record parser is pure and defensive: a malformed record is
//! dropped, never a panic.
//!
//! This module reads the repository and never writes it. No function here
//! stages, unstages, reverts, or discards anything. See `docs/git.md`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::str;
use std::sync::Arc;
use std::time::Duration;

use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{ProcessOutput, ProcessRequest};

/// The external command that reads the repository state.
pub const GIT_PROGRAM: &str = "git";

/// The largest number of status records that one snapshot keeps.
///
/// A workspace above this bound leaves the remaining entries unmarked. The
/// marks are decoration, so the tree stays fully usable.
pub const GIT_STATUS_ENTRIES_MAX: usize = 4096;

/// The largest output that one status read captures, in bytes.
pub const GIT_STATUS_OUTPUT_BYTES_MAX: usize = 1024 * 1024;

/// The largest output that the prefix read of one status captures, in bytes.
///
/// The answer is one path below the top level of the repository, so the bound
/// follows the path bound of one worktree entry.
pub const GIT_PREFIX_OUTPUT_BYTES_MAX: usize = 8 * 1024;

/// The deadline of one status read.
pub const GIT_STATUS_DEADLINE: Duration = Duration::from_secs(5);

/// The largest number of directory levels that one bounded path walk inspects.
///
/// The roll-up onto the directories above one entry and the lookup of an
/// inherited state both stop here, so no malformed path can cost unbounded
/// time.
pub const GIT_PATH_DEPTH_MAX: usize = 64;

/// The configuration that every Git command of kvim overrides.
///
/// Command-line configuration outranks the repository and the host, so no
/// checkout can make one read start another program. `/dev/null` is a file
/// rather than a directory, so Git finds no hook below it, and a host without
/// that name finds no hook either.
const POLICY_CONFIGURATION: [&str; 14] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.askPass=",
    "-c",
    "core.pager=cat",
    "-c",
    "credential.helper=",
    "-c",
    "diff.external=",
    "-c",
    "gc.auto=0",
];

/// The variables that no Git command of kvim inherits.
///
/// Each name either redirects the read to another repository, another index, or
/// another configuration file, or names a program that Git would start.
const DROPPED_VARIABLES: [&str; 22] = [
    "EDITOR",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_ASKPASS",
    "GIT_CEILING_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_DIR",
    "GIT_EDITOR",
    "GIT_EXTERNAL_DIFF",
    "GIT_INDEX_FILE",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PAGER",
    "GIT_PROXY_COMMAND",
    "GIT_SEQUENCE_EDITOR",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_WORK_TREE",
    "PAGER",
];

/// The variables that every Git command of kvim sets explicitly.
const CHILD_VARIABLES: [(&str, &str); 3] = [
    // The host configuration cannot name an external program for the read.
    ("GIT_CONFIG_NOSYSTEM", "1"),
    // The read gives up every optional lock, so it writes nothing.
    ("GIT_OPTIONAL_LOCKS", "0"),
    // A read that would ask the user fails instead of blocking the service.
    ("GIT_TERMINAL_PROMPT", "0"),
];

/// The arguments of the bounded status read.
const STATUS_ARGUMENTS: [&str; 7] = [
    "status",
    "--porcelain=v2",
    // The records are NUL separated, so a name that holds a space, a quote, or
    // a line break still names one entry.
    "-z",
    // The traditional mode names one ignored directory instead of every file
    // below it, so a large build directory costs one record.
    "--ignored=traditional",
    // The mode is explicit, because the ignored mode above collapses a
    // directory only while the untracked mode collapses one too.
    "--untracked-files=normal",
    // The pathspec follows the separator, so a workspace root that starts with
    // a hyphen stays a path.
    "--",
    // The pathspec keeps the report inside the workspace root, which may sit
    // below the top level of the repository.
    ".",
];

/// The arguments that name the place of the worktree root in its repository.
const PREFIX_ARGUMENTS: [&str; 2] = ["rev-parse", "--show-prefix"];

/// The character that `git status --porcelain=v2` writes for an unchanged half
/// of one two-character state field.
const UNCHANGED: char = '.';

/// The suffix that marks one collapsed directory record.
///
/// `git status` names one untracked or ignored directory instead of every file
/// below it, and it closes that name with a separator.
const DIRECTORY_SUFFIX: char = '/';

/// The number of fields that one ordinary changed-entry record holds after its
/// two-character type prefix.
const ORDINARY_FIELDS: usize = 8;

/// The number of fields that one renamed or copied entry record holds after its
/// two-character type prefix.
const RENAMED_FIELDS: usize = 9;

/// The number of fields that one unmerged entry record holds after its
/// two-character type prefix.
const UNMERGED_FIELDS: usize = 10;

/// The recorded Git state of one workspace entry.
///
/// The variants are ordered by rising severity, so a derived comparison ranks
/// two states the way a reader ranks them. Git records the staged half and the
/// worktree half of one change separately, and both halves can hold a change at
/// one time, so the combined state is one variant instead of two flags.
///
/// # Examples
///
/// ```
/// use kvim_workspace::GitStatus;
///
/// // A directory reports the strongest state below it, and a subtree that
/// // holds staged work and unstaged work reports both.
/// let rolled = GitStatus::Staged.merged(GitStatus::Modified);
/// assert_eq!(rolled, GitStatus::StagedAndModified);
/// assert!(GitStatus::Conflicted > GitStatus::Modified);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GitStatus {
    /// The Git ignore rules name the entry.
    Ignored,
    /// The repository tracks no entry of this path.
    Untracked,
    /// The index holds a change that the last commit does not hold.
    Staged,
    /// The working tree holds a change that the index does not hold.
    Modified,
    /// The index and the working tree each hold a change.
    StagedAndModified,
    /// The entry holds an unresolved merge conflict.
    Conflicted,
}

impl GitStatus {
    /// Returns the state that covers both of two states.
    ///
    /// A directory carries the state of the entries below it, so the roll-up
    /// combines the entries with this operation. A conflict wins over every
    /// other state, because it blocks the work of the reader. A staged half and
    /// a worktree half combine into the state that holds both.
    #[must_use]
    pub const fn merged(self, other: Self) -> Self {
        if matches!(self, Self::Conflicted) || matches!(other, Self::Conflicted) {
            return Self::Conflicted;
        }
        let staged = self.holds_staged_change() || other.holds_staged_change();
        let worktree = self.holds_worktree_change() || other.holds_worktree_change();
        match (staged, worktree) {
            (true, true) => Self::StagedAndModified,
            (true, false) => Self::Staged,
            (false, true) => Self::Modified,
            // Neither half tracks a change, so the stronger of the two remains.
            (false, false) if (self as u8) >= (other as u8) => self,
            (false, false) => other,
        }
    }

    /// Reports whether the index holds a change of this entry.
    const fn holds_staged_change(self) -> bool {
        matches!(self, Self::Staged | Self::StagedAndModified)
    }

    /// Reports whether the working tree holds a change of this entry.
    const fn holds_worktree_change(self) -> bool {
        matches!(self, Self::Modified | Self::StagedAndModified)
    }

    /// Reports whether the state may reach the directories above the entry.
    ///
    /// An ignored entry never reports upward. The workspace root of an ordinary
    /// repository holds one ignored build directory, and that directory must
    /// not make the whole workspace read as ignored.
    const fn rolls_up(self) -> bool {
        !matches!(self, Self::Ignored)
    }
}

/// The reason that one status read produced no snapshot.
///
/// Both values are normal states. The file tree keeps every row and every key,
/// and it shows no mark. See `docs/git.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitStatusFailure {
    /// The host holds no `git` command.
    ///
    /// The editor names this state once for each session, because it never
    /// changes while the editor runs.
    CommandMissing,
    /// Git produced no status.
    ///
    /// The directory is inside no repository, or the request was cancelled,
    /// passed its deadline, or passed its output bound.
    Unavailable,
}

/// The process policy of every Git read.
///
/// The policy builds one command without a shell. It gives the canonical
/// worktree root to the child as its explicit working directory, and it neither
/// reads nor changes the current directory of this process. It also disables
/// every repository and host setting that could start another program. See
/// `docs/git.md`.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::sync::Arc;
///
/// use kvim_path::WorktreeRoot;
/// use kvim_workspace::{GIT_PROGRAM, GitExecutionPolicy};
///
/// let root = Arc::new(WorktreeRoot::open(std::env::current_dir()?)?);
/// let policy = GitExecutionPolicy::new(Arc::clone(&root));
/// let command = policy.command(&["rev-parse", "--show-prefix"]);
///
/// assert_eq!(command.program, GIT_PROGRAM);
/// assert_eq!(command.current_dir.as_deref(), Some(root.as_path()));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitExecutionPolicy {
    root: Arc<WorktreeRoot>,
}

impl GitExecutionPolicy {
    /// Creates the policy of one canonical worktree root.
    #[must_use]
    pub fn new(root: Arc<WorktreeRoot>) -> Self {
        Self { root }
    }

    /// Returns the canonical worktree root that every command reads.
    #[must_use]
    pub fn root(&self) -> &WorktreeRoot {
        &self.root
    }

    /// Returns the shared owner of the canonical worktree root.
    #[must_use]
    pub fn root_handle(&self) -> Arc<WorktreeRoot> {
        Arc::clone(&self.root)
    }

    /// Returns one bounded Git command that carries the complete policy.
    ///
    /// The caller supplies the subcommand and its arguments. The policy writes
    /// every option that keeps the read free of locks, pagers, prompts,
    /// external programs, and inherited redirection.
    ///
    /// The default output bound and deadline of the process service apply. A
    /// caller that needs another bound sets it on the returned request.
    #[must_use]
    pub fn command(&self, arguments: &[&str]) -> ProcessRequest {
        let mut request = ProcessRequest::new(GIT_PROGRAM);
        let mut args = Vec::with_capacity(3 + POLICY_CONFIGURATION.len() + arguments.len());
        // The read must change nothing, so it refreshes no index cache, and it
        // writes to no terminal that a pager would own.
        args.push(OsString::from("--no-pager"));
        args.push(OsString::from("--no-optional-locks"));
        // A pathspec is a literal path, never a magic expression.
        args.push(OsString::from("--literal-pathspecs"));
        args.extend(POLICY_CONFIGURATION.iter().map(OsString::from));
        args.extend(arguments.iter().map(OsString::from));
        request.args = args;
        request.current_dir = Some(self.root.as_path().to_path_buf());
        request.dropped_variables = DROPPED_VARIABLES.iter().map(OsString::from).collect();
        request.child_variables = CHILD_VARIABLES
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect();
        request
    }
}

/// The place of one worktree root inside its repository.
///
/// `git status --porcelain=v2 -z` names every path against the top level of the
/// repository, and that top level can sit above the worktree root. The prefix
/// is the path from the top level down to the root, so the publication can
/// subtract it and keep only contained relative paths. Git reports it, so kvim
/// inspects no directory above its own root.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RepositoryPrefix(PathBuf);

impl RepositoryPrefix {
    /// Returns the prefix that `git rev-parse --show-prefix` reported.
    ///
    /// A root that is the top level of its repository reports an empty prefix.
    /// A reported path of any other shape than ordinary components is
    /// malformed, so the read produces no snapshot.
    fn parse(stdout: &[u8]) -> Option<Self> {
        let reported = str::from_utf8(stdout)
            .ok()?
            .trim_end_matches(['\n', '\r'])
            .trim_end_matches(DIRECTORY_SUFFIX);
        if reported.is_empty() {
            return Some(Self::default());
        }
        let path = Path::new(reported);
        path.components()
            .all(|component| matches!(component, Component::Normal(_)))
            .then(|| Self(path.to_path_buf()))
    }

    /// Returns the prefix as a path of ordinary components.
    fn as_path(&self) -> &Path {
        &self.0
    }
}

/// The stage that one bounded status read runs next.
#[derive(Clone, Debug, Eq, PartialEq)]
enum GitStatusStage {
    /// Ask Git where the worktree root sits inside its repository.
    Prefix,
    /// Read the status records against the known prefix.
    Records(RepositoryPrefix),
}

/// The next step of one bounded status read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitStatusRead {
    /// The read needs one further command before it can publish.
    Pending(GitStatusRequest),
    /// The read finished and produced one snapshot.
    Published(GitStatusSnapshot),
}

/// One bounded read of the repository state of one workspace root.
///
/// The read takes two commands. The first learns the place of the root inside
/// its repository. The second collects the status records. The caller submits
/// the command of [`GitStatusRequest::command`] and hands the captured output
/// back to [`GitStatusRequest::publish`] until the read publishes a snapshot.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::sync::Arc;
///
/// use kvim_path::WorktreeRoot;
/// use kvim_workspace::{GIT_PROGRAM, GitStatusRequest};
///
/// let root = Arc::new(WorktreeRoot::open(std::env::current_dir()?)?);
/// let request = GitStatusRequest::new(Arc::clone(&root));
/// let command = request.command();
///
/// assert_eq!(command.program, GIT_PROGRAM);
/// assert_eq!(command.current_dir.as_deref(), Some(root.as_path()));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusRequest {
    policy: GitExecutionPolicy,
    stage: GitStatusStage,
}

impl GitStatusRequest {
    /// Creates one request over a canonical workspace root.
    #[must_use]
    pub fn new(root: Arc<WorktreeRoot>) -> Self {
        Self {
            policy: GitExecutionPolicy::new(root),
            stage: GitStatusStage::Prefix,
        }
    }

    /// Returns the workspace root that this request reads.
    #[must_use]
    pub fn root(&self) -> &WorktreeRoot {
        self.policy.root()
    }

    /// Returns the bounded command of the current stage.
    #[must_use]
    pub fn command(&self) -> ProcessRequest {
        let (arguments, output_bytes_max): (&[&str], usize) = match self.stage {
            GitStatusStage::Prefix => (&PREFIX_ARGUMENTS, GIT_PREFIX_OUTPUT_BYTES_MAX),
            GitStatusStage::Records(_) => (&STATUS_ARGUMENTS, GIT_STATUS_OUTPUT_BYTES_MAX),
        };
        let mut request = self.policy.command(arguments);
        request.output_bytes_max = output_bytes_max;
        request.deadline = GIT_STATUS_DEADLINE;
        request
    }

    /// Turns the captured output of the current stage into the next step.
    ///
    /// The call runs on the bounded process service, never on the terminal
    /// event loop.
    ///
    /// # Errors
    ///
    /// Returns [`GitStatusFailure::Unavailable`] when Git refused the command,
    /// which includes a root that sits inside no repository, and when the
    /// reported prefix is malformed.
    pub fn publish(self, output: &ProcessOutput) -> Result<GitStatusRead, GitStatusFailure> {
        // Git reports every refusal, including a directory outside a
        // repository, through its exit code. No branch reads its message text.
        if output.status_code != Some(0) {
            return Err(GitStatusFailure::Unavailable);
        }
        match self.stage {
            GitStatusStage::Prefix => {
                let prefix =
                    RepositoryPrefix::parse(&output.stdout).ok_or(GitStatusFailure::Unavailable)?;
                Ok(GitStatusRead::Pending(Self {
                    policy: self.policy,
                    stage: GitStatusStage::Records(prefix),
                }))
            }
            GitStatusStage::Records(prefix) => Ok(GitStatusRead::Published(
                GitStatusSnapshot::parse(self.policy.root_handle(), &prefix, &output.stdout),
            )),
        }
    }
}

/// The published Git state of every entry below one workspace root.
///
/// The snapshot holds the state of each reported entry, the state that one
/// collapsed directory record covers, and the state that rolls up onto the
/// directories above a changed entry. It performs no filesystem work.
///
/// Every published path is a validated [`WorktreeRelativePath`] of the root, so
/// no state of an entry above or beside the workspace root can reach the
/// editor. See `docs/git.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusSnapshot {
    /// The workspace root that the snapshot describes.
    root: Arc<WorktreeRoot>,
    /// The state of one exact path, including every rolled-up directory.
    ///
    /// Every key is a contained relative path. The workspace root itself takes
    /// no key, because the sidebar marks entries and not its own header.
    entries: BTreeMap<PathBuf, GitStatus>,
    /// The state that one collapsed directory record covers below itself.
    subtrees: BTreeMap<PathBuf, GitStatus>,
}

impl GitStatusSnapshot {
    /// Builds one snapshot from the captured output of `git status`.
    ///
    /// `prefix` is the place of the workspace root inside its repository,
    /// because Git reports every path against the top level of that
    /// repository. The snapshot drops every record outside the root.
    ///
    /// The parser drops every record that names no known type, every record
    /// that holds too few fields, every record whose path leaves the workspace
    /// root, and the last record when the output bound stopped inside it. It
    /// keeps at most [`GIT_STATUS_ENTRIES_MAX`] records.
    #[must_use]
    fn parse(root: Arc<WorktreeRoot>, prefix: &RepositoryPrefix, stdout: &[u8]) -> Self {
        let mut snapshot = Self {
            root,
            entries: BTreeMap::new(),
            subtrees: BTreeMap::new(),
        };
        let records: Vec<&[u8]> = stdout.split(|byte| *byte == 0).collect();
        let mut index = 0;
        let mut kept = 0;
        while index < records.len() && kept < GIT_STATUS_ENTRIES_MAX {
            let record = records[index];
            index += 1;
            // A renamed entry writes its original path as one further record,
            // which names no state of its own. The type prefix decides that
            // before the decoding, so a name that is not UTF-8 cannot make the
            // parser read the original path as one entry.
            if record.starts_with(b"2 ") {
                index += 1;
            }
            let Ok(record) = str::from_utf8(record) else {
                continue;
            };
            let Some(record) = parse_record(record) else {
                continue;
            };
            let Some(path) = contained_path(prefix, record.path) else {
                continue;
            };
            kept += 1;
            snapshot.insert(path.as_path(), record.status, record.reach);
        }
        snapshot
    }

    /// Returns the workspace root that the snapshot describes.
    #[must_use]
    pub fn root(&self) -> &WorktreeRoot {
        &self.root
    }

    /// Returns the state of one entry, or `None` while Git reports none.
    ///
    /// The lookup answers from the exact path first, and then from the nearest
    /// collapsed directory record above it, so an entry inside an ignored or an
    /// untracked directory reports the state of that directory.
    #[must_use]
    pub fn state(&self, path: &WorktreeRelativePath) -> Option<GitStatus> {
        let path = path.as_path();
        if let Some(state) = self.entries.get(path) {
            return Some(*state);
        }
        path.ancestors()
            .take(GIT_PATH_DEPTH_MAX)
            .take_while(|ancestor| !ancestor.as_os_str().is_empty())
            .find_map(|ancestor| self.subtrees.get(ancestor).copied())
    }

    /// Records one parsed entry and rolls its state up onto its directories.
    fn insert(&mut self, path: &Path, status: GitStatus, reach: Reach) {
        debug_assert!(
            !path.as_os_str().is_empty(),
            "contained_path rejects a record that names the workspace root"
        );
        match reach {
            Reach::Entry => merge(&mut self.entries, path, status),
            Reach::Subtree => merge(&mut self.subtrees, path, status),
        }
        if !status.rolls_up() {
            return;
        }
        for ancestor in path
            .ancestors()
            .skip(1)
            .take(GIT_PATH_DEPTH_MAX)
            .take_while(|ancestor| !ancestor.as_os_str().is_empty())
        {
            merge(&mut self.entries, ancestor, status);
        }
    }
}

/// Combines one state into the state that a map already holds.
fn merge(states: &mut BTreeMap<PathBuf, GitStatus>, path: &Path, status: GitStatus) {
    states
        .entry(path.to_path_buf())
        .and_modify(|held| *held = held.merged(status))
        .or_insert(status);
}

/// Returns the contained path of one reported record.
///
/// Git writes every path against the top level of the repository, as a relative
/// path of ordinary components. A record that holds a root component or a
/// parent step is malformed, and a record that does not start with the prefix
/// names an entry outside the workspace root. Both return `None`, and so does
/// a record that names the workspace root itself.
fn contained_path(prefix: &RepositoryPrefix, reported: &str) -> Option<WorktreeRelativePath> {
    let reported = Path::new(reported.trim_end_matches(DIRECTORY_SUFFIX));
    let ordinary = reported
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if !ordinary {
        return None;
    }
    let contained = reported.strip_prefix(prefix.as_path()).ok()?;
    WorktreeRelativePath::new(contained).ok()
}

/// How far the state of one record reaches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reach {
    /// The state covers exactly the named path.
    Entry,
    /// The state covers the named directory and every entry below it.
    Subtree,
}

/// One parsed record of `git status --porcelain=v2`.
struct StatusRecord<'a> {
    status: GitStatus,
    reach: Reach,
    path: &'a str,
}

/// Returns the record of one status line, or `None` for a malformed line.
fn parse_record(record: &str) -> Option<StatusRecord<'_>> {
    let (kind, rest) = record.split_at_checked(2)?;
    let (status, path) = match kind {
        "1 " => {
            let (state, path) = split_entry(rest, ORDINARY_FIELDS)?;
            (tracked_status(state)?, path)
        }
        "2 " => {
            let (state, path) = split_entry(rest, RENAMED_FIELDS)?;
            (tracked_status(state)?, path)
        }
        // Both halves of an unmerged entry name a conflict, so the state field
        // adds nothing that the record type does not already say.
        "u " => (GitStatus::Conflicted, split_entry(rest, UNMERGED_FIELDS)?.1),
        "? " => (GitStatus::Untracked, rest),
        "! " => (GitStatus::Ignored, rest),
        _ => return None,
    };
    if path.is_empty() {
        return None;
    }
    let reach = if path.ends_with(DIRECTORY_SUFFIX) {
        Reach::Subtree
    } else {
        Reach::Entry
    };
    Some(StatusRecord {
        status,
        reach,
        path,
    })
}

/// Returns the state field and the path field of one changed-entry record.
///
/// The path is the last field, and it may hold spaces, so the split stops
/// before it. A record with fewer fields is malformed and returns `None`.
fn split_entry(rest: &str, fields: usize) -> Option<(&str, &str)> {
    debug_assert!(
        fields >= 2,
        "every changed-entry record holds a state field and a path field"
    );
    let mut parts = rest.splitn(fields, ' ');
    let state = parts.next()?;
    let mut path = parts.next()?;
    for _ in 2..fields {
        path = parts.next()?;
    }
    Some((state, path))
}

/// Returns the state of one two-character field of a tracked entry.
///
/// The first character reports the index against the last commit, and the
/// second reports the working tree against the index. A field that reports no
/// change on either side names no state at all.
fn tracked_status(field: &str) -> Option<GitStatus> {
    let mut halves = field.chars();
    let staged = halves.next()?;
    let worktree = halves.next()?;
    if halves.next().is_some() {
        return None;
    }
    match (staged != UNCHANGED, worktree != UNCHANGED) {
        (true, true) => Some(GitStatus::StagedAndModified),
        (true, false) => Some(GitStatus::Staged),
        (false, true) => Some(GitStatus::Modified),
        (false, false) => None,
    }
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

    use crate::temp::{TempDir, TempRepository};

    use super::{
        GIT_STATUS_DEADLINE, GIT_STATUS_ENTRIES_MAX, GIT_STATUS_OUTPUT_BYTES_MAX, GitStatus,
        GitStatusFailure, GitStatusRead, GitStatusRequest, GitStatusSnapshot, RepositoryPrefix,
    };

    /// The number of commands that one complete status read runs.
    const READ_COMMANDS: usize = 2;

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

    /// Reads the status of one workspace root through the bounded process
    /// service, exactly as the terminal event loop does.
    ///
    /// The call runs the real `git` command, so it proves the flags of
    /// [`GitStatusRequest::command`]. A recorded output can never prove them.
    fn read_status(root: &Path) -> Result<GitStatusSnapshot, GitStatusFailure> {
        let root = Arc::new(WorktreeRoot::open(root).expect("the fixture root is one directory"));
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("the test host starts one Tokio runtime");
        tokio.block_on(async move {
            let mut request = GitStatusRequest::new(root);
            for _ in 0..READ_COMMANDS {
                let output = run(request.command()).await;
                match request.publish(&output)? {
                    GitStatusRead::Pending(next) => request = next,
                    GitStatusRead::Published(snapshot) => return Ok(snapshot),
                }
            }
            unreachable!("one status read publishes after {READ_COMMANDS} commands");
        })
    }

    /// The workspace root of every parser test.
    ///
    /// The parser performs no filesystem work, so one capability over the
    /// working directory of the test process names the root of every snapshot.
    fn root() -> Arc<WorktreeRoot> {
        Arc::new(
            WorktreeRoot::open(
                std::env::current_dir().expect("the test process holds a working directory"),
            )
            .expect("the working directory is one canonical root"),
        )
    }

    /// Returns one validated contained path of the test root.
    fn relative(path: &str) -> WorktreeRelativePath {
        WorktreeRelativePath::new(path).expect("the fixture path is contained")
    }

    /// Builds one snapshot whose workspace root is the repository top level.
    fn snapshot(output: &str) -> GitStatusSnapshot {
        GitStatusSnapshot::parse(root(), &RepositoryPrefix::default(), output.as_bytes())
    }

    /// Returns the state of one path below the workspace root.
    fn state(snapshot: &GitStatusSnapshot, path: &str) -> Option<GitStatus> {
        snapshot.state(&relative(path))
    }

    /// One ordinary record, from the recorded output of `git status`.
    fn ordinary(field: &str, path: &str) -> String {
        format!(
            "1 {field} N... 100644 100644 100644 \
             78981922613b2afb6025042ff6bd878ac1994e85 \
             78981922613b2afb6025042ff6bd878ac1994e85 {path}\0"
        )
    }

    #[test]
    fn a_clean_tree_reports_no_state_at_all() {
        let snapshot = snapshot("");
        assert_eq!(state(&snapshot, "src/main.rs"), None);
        assert_eq!(state(&snapshot, "src"), None);
    }

    #[test]
    fn each_half_of_the_state_field_names_its_own_state() {
        let output = format!(
            "{}{}{}",
            ordinary(".M", "src/modified.rs"),
            ordinary("M.", "src/staged.rs"),
            ordinary("MM", "src/both.rs"),
        );
        let snapshot = snapshot(&output);
        assert_eq!(
            state(&snapshot, "src/modified.rs"),
            Some(GitStatus::Modified)
        );
        assert_eq!(state(&snapshot, "src/staged.rs"), Some(GitStatus::Staged));
        assert_eq!(
            state(&snapshot, "src/both.rs"),
            Some(GitStatus::StagedAndModified)
        );
    }

    #[test]
    fn an_untracked_and_an_ignored_record_name_their_own_state() {
        let snapshot = snapshot("? src/new.rs\0! target/\0");
        assert_eq!(state(&snapshot, "src/new.rs"), Some(GitStatus::Untracked));
        assert_eq!(state(&snapshot, "target"), Some(GitStatus::Ignored));
    }

    #[test]
    fn a_collapsed_directory_record_covers_every_entry_below_it() {
        // `git status` names one ignored directory instead of every file below
        // it, so the entries of that directory inherit its state.
        let snapshot = snapshot("! target/\0? crates/\0");
        assert_eq!(
            state(&snapshot, "target/debug/build/out.o"),
            Some(GitStatus::Ignored)
        );
        assert_eq!(
            state(&snapshot, "crates/kvim/src/main.rs"),
            Some(GitStatus::Untracked)
        );
    }

    #[test]
    fn the_state_of_one_entry_rolls_up_onto_the_directories_above_it() {
        let snapshot = snapshot(&ordinary(".M", "crates/kvim/src/main.rs"));
        assert_eq!(
            state(&snapshot, "crates/kvim/src"),
            Some(GitStatus::Modified)
        );
        assert_eq!(state(&snapshot, "crates/kvim"), Some(GitStatus::Modified));
        assert_eq!(state(&snapshot, "crates"), Some(GitStatus::Modified));
    }

    #[test]
    fn one_directory_reports_both_halves_of_the_changes_below_it() {
        let output = format!(
            "{}{}",
            ordinary("M.", "src/staged.rs"),
            ordinary(".M", "src/modified.rs"),
        );
        assert_eq!(
            state(&snapshot(&output), "src"),
            Some(GitStatus::StagedAndModified),
            "the roll-up keeps both halves that the entries below it hold"
        );
    }

    #[test]
    fn an_ignored_entry_never_reaches_the_directories_above_it() {
        // An ordinary repository ignores its build directory. That directory
        // must not make the whole workspace read as ignored, so no entry beside
        // it inherits the state.
        let snapshot = snapshot("! target/\0");
        assert_eq!(state(&snapshot, "target"), Some(GitStatus::Ignored));
        assert_eq!(state(&snapshot, "src/main.rs"), None);
    }

    #[test]
    fn a_renamed_record_names_one_entry_and_drops_its_original_path() {
        let output = concat!(
            "2 R. N... 100644 100644 100644 aaaa bbbb R100 docs/two.md\0",
            "docs/one.md\0",
            "? docs/three.md\0",
        );
        let snapshot = snapshot(output);
        assert_eq!(state(&snapshot, "docs/two.md"), Some(GitStatus::Staged));
        assert_eq!(
            state(&snapshot, "docs/three.md"),
            Some(GitStatus::Untracked),
            "the record behind the original path still parses"
        );
    }

    #[test]
    fn an_unmerged_record_names_one_conflict() {
        let output = "u UU N... 100644 100644 100644 100644 aa bb cc src/merge.rs\0";
        assert_eq!(
            state(&snapshot(output), "src/merge.rs"),
            Some(GitStatus::Conflicted)
        );
    }

    #[test]
    fn a_malformed_record_is_dropped_without_a_panic() {
        let output = concat!(
            "\0",
            "x unknown record type\0",
            "1 .M too few fields\0",
            "1 ... N... 100644 100644 100644 aa bb src/wide.rs\0",
            "1 .. N... 100644 100644 100644 aa bb src/quiet.rs\0",
            "? \0",
            "? ../outside.rs\0",
            "? /absolute.rs\0",
            "? src/only-valid.rs\0",
        );
        let snapshot = snapshot(output);
        assert_eq!(
            state(&snapshot, "src/only-valid.rs"),
            Some(GitStatus::Untracked)
        );
        assert_eq!(state(&snapshot, "src/wide.rs"), None);
        assert_eq!(state(&snapshot, "src/quiet.rs"), None);
        assert_eq!(state(&snapshot, "outside.rs"), None);
    }

    #[test]
    fn a_path_that_holds_spaces_stays_one_field() {
        let snapshot = snapshot(&ordinary(".M", "docs/two words.md"));
        assert_eq!(
            state(&snapshot, "docs/two words.md"),
            Some(GitStatus::Modified)
        );
    }

    #[test]
    fn the_record_list_stops_at_the_entry_bound() {
        let mut output = String::new();
        for index in 0..GIT_STATUS_ENTRIES_MAX + 8 {
            output.push_str(&format!("? src/file-{index}.rs\0"));
        }
        let snapshot = snapshot(&output);
        assert_eq!(
            state(&snapshot, "src/file-0.rs"),
            Some(GitStatus::Untracked)
        );
        assert_eq!(
            state(&snapshot, &format!("src/file-{GIT_STATUS_ENTRIES_MAX}.rs")),
            None,
            "the entry bound keeps the snapshot finite"
        );
    }

    #[test]
    fn a_record_outside_the_workspace_root_is_dropped() {
        // The workspace root may sit below the top level of the repository.
        // Git reports every path against that top level, and the prefix names
        // the place of the root inside it.
        let prefix = RepositoryPrefix::parse(b"crates/kvim/\n").expect("the prefix is one path");
        let output = concat!(
            "1 .M N... 100644 100644 100644 aa bb crates/kvim/src/main.rs\0",
            "1 .M N... 100644 100644 100644 aa bb docs/other.md\0",
        );
        let snapshot = GitStatusSnapshot::parse(root(), &prefix, output.as_bytes());
        assert_eq!(
            snapshot.state(&relative("src/main.rs")),
            Some(GitStatus::Modified)
        );
        assert_eq!(snapshot.state(&relative("docs/other.md")), None);
    }

    #[test]
    fn a_root_that_is_its_own_top_level_reports_an_empty_prefix() {
        assert_eq!(
            RepositoryPrefix::parse(b"\n"),
            Some(RepositoryPrefix::default())
        );
        assert_eq!(
            RepositoryPrefix::parse(b""),
            Some(RepositoryPrefix::default())
        );
    }

    #[test]
    fn a_prefix_that_leaves_the_repository_is_refused() {
        assert_eq!(RepositoryPrefix::parse(b"../escape/\n"), None);
        assert_eq!(RepositoryPrefix::parse(b"/absolute/\n"), None);
    }

    #[test]
    fn the_command_reads_the_repository_and_never_writes_it() {
        let command = GitStatusRequest::new(root()).command();
        let args: Vec<String> = command
            .args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        // The format flags need no assertion here. One real read proves them
        // by its result, and a recorded output could never prove them at all.
        assert!(
            args.contains(&"--no-optional-locks".to_owned()),
            "the read gives up every optional lock of the repository"
        );
        assert!(
            args.contains(&"core.hooksPath=/dev/null".to_owned()),
            "the repository cannot start a hook during a read"
        );
        assert!(
            args.contains(&"diff.external=".to_owned()),
            "the repository cannot start an external diff program"
        );
        assert!(command.stdin.is_empty());
        assert_eq!(command.current_dir.as_deref(), Some(root().as_path()));
    }

    #[test]
    fn every_command_drops_the_inherited_helper_variables() {
        let command = GitStatusRequest::new(root()).command();
        let dropped: Vec<String> = command
            .dropped_variables
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        // Each of these either redirects the read to another repository or
        // names a program that Git would start.
        for name in [
            "GIT_DIR",
            "GIT_EXTERNAL_DIFF",
            "GIT_PAGER",
            "GIT_SSH_COMMAND",
        ] {
            assert!(dropped.contains(&name.to_owned()), "{name} stays inherited");
        }
    }

    #[test]
    fn the_status_stage_carries_the_bounds_of_one_status_read() {
        let request = GitStatusRequest::new(root());
        let prefix = ProcessOutput {
            status_code: Some(0),
            stdout: b"\n".to_vec(),
            stderr: Vec::new(),
        };
        let GitStatusRead::Pending(request) = request
            .publish(&prefix)
            .expect("the prefix command answered")
        else {
            panic!("the first stage never publishes one snapshot");
        };
        let command = request.command();
        let args: Vec<String> = command
            .args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args.last().map(String::as_str),
            Some("."),
            "the pathspec follows the separator, so a root cannot become a flag"
        );
        assert_eq!(command.output_bytes_max, GIT_STATUS_OUTPUT_BYTES_MAX);
        assert_eq!(command.deadline, GIT_STATUS_DEADLINE);
    }

    #[test]
    fn a_refused_command_reports_no_status() {
        // Git reports a directory outside a repository through its exit code.
        let output = ProcessOutput {
            status_code: Some(128),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert_eq!(
            GitStatusRequest::new(root()).publish(&output),
            Err(GitStatusFailure::Unavailable)
        );
    }

    #[test]
    fn the_status_flags_report_every_state_of_one_real_repository() {
        // The recorded records above prove the parser. This test proves the
        // flags: only a real invocation shows that `--ignored=traditional`
        // still names one directory instead of every file below it, and that
        // `--porcelain=v2` still writes the format that the parser reads.
        let repository = TempRepository::new("git-states");
        repository.file(".gitignore", "build/\n");
        repository.file("src/modified.rs", "one\n");
        repository.file("src/staged.rs", "one\n");
        repository.file("docs/clean.md", "one\n");
        repository.commit("record the first state");

        repository.file("src/modified.rs", "one\ntwo\n");
        repository.file("src/staged.rs", "one\ntwo\n");
        repository.git(&["add", "src/staged.rs"]);
        repository.file("src/untracked.rs", "one\n");
        repository.file("build/output.o", "one\n");

        let snapshot = read_status(repository.path()).expect("the directory is one repository");
        let state = |name: &str| snapshot.state(&relative(name));

        assert_eq!(state("src/modified.rs"), Some(GitStatus::Modified));
        assert_eq!(state("src/staged.rs"), Some(GitStatus::Staged));
        assert_eq!(state("src/untracked.rs"), Some(GitStatus::Untracked));
        assert_eq!(state("build"), Some(GitStatus::Ignored));
        assert_eq!(
            state("build/output.o"),
            Some(GitStatus::Ignored),
            "one collapsed directory record covers every file below it"
        );
        assert_eq!(
            state("src"),
            Some(GitStatus::StagedAndModified),
            "the directory reports both halves of the changes below it"
        );
        assert_eq!(state("docs/clean.md"), None);
        assert_eq!(state("docs"), None, "a clean subtree reports nothing");
    }

    #[test]
    fn a_workspace_below_the_repository_reads_its_own_subtree() {
        let repository = TempRepository::new("git-nested");
        repository.file("crates/kvim/src/main.rs", "one\n");
        repository.file("docs/outside.md", "one\n");
        repository.commit("record the first state");

        repository.file("crates/kvim/src/main.rs", "one\ntwo\n");
        repository.file("docs/outside.md", "one\ntwo\n");

        let workspace = repository.join("crates/kvim");
        let snapshot = read_status(&workspace).expect("the directory sits inside one repository");

        assert_eq!(snapshot.root().as_path(), workspace.as_path());
        assert_eq!(
            snapshot.state(&relative("src/main.rs")),
            Some(GitStatus::Modified),
            "Git reports the path against the top level, and the prefix subtracts it"
        );
        assert_eq!(
            snapshot.state(&relative("docs/outside.md")),
            None,
            "the pathspec keeps the report inside the workspace root"
        );
    }

    #[test]
    fn a_directory_outside_a_repository_reports_no_status() {
        let dir = TempDir::new("git-plain");
        assert_eq!(
            read_status(&dir.path),
            Err(GitStatusFailure::Unavailable),
            "the refusal is a normal state, never an error of the editor"
        );
    }
}
