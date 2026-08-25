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
#[path = "git_tests.rs"]
mod tests;
