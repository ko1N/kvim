//! The bounded read-only Git status of the workspace.
//!
//! The editor never runs `git` itself. [`GitStatusRequest::command`] builds one
//! [`ProcessRequest`], the bounded process service runs it, and
//! [`GitStatusRequest::publish`] turns the captured output into one
//! [`GitStatusSnapshot`]. [`GitStatusSnapshot::parse`] is pure and defensive: a
//! malformed record is dropped, never a panic.
//!
//! This module reads the repository and never writes it. No function here
//! stages, unstages, reverts, or discards anything. See `docs/git.md`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::str;
use std::time::Duration;

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

/// The deadline of one status read.
pub const GIT_STATUS_DEADLINE: Duration = Duration::from_secs(5);

/// The largest number of directory levels that one bounded path walk inspects.
///
/// The search for the repository, the roll-up onto the directories above one
/// entry, and the lookup of an inherited state all stop here, so no malformed
/// path can cost unbounded time.
pub const GIT_PATH_DEPTH_MAX: usize = 64;

/// The entry that marks the top level of one repository.
///
/// The entry is a directory in an ordinary clone and a file inside a linked
/// worktree or a submodule, so the search only asks whether it exists.
const REPOSITORY_MARKER: &str = ".git";

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

/// One bounded read of the repository state of one workspace root.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_workspace::{GIT_PROGRAM, GitStatusRequest};
///
/// let request = GitStatusRequest::new(Path::new("/workspace").to_path_buf());
/// let command = request.command();
/// assert_eq!(command.program, GIT_PROGRAM);
/// assert_eq!(command.current_dir.as_deref(), Some(Path::new("/workspace")));
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusRequest {
    root: PathBuf,
}

impl GitStatusRequest {
    /// Creates one request over a workspace root.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Returns the workspace root that this request reads.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the bounded command of one status read.
    ///
    /// The command gives up every optional lock, so it never writes the
    /// repository. The pathspec keeps the report inside the workspace root,
    /// which may sit below the top level of the repository.
    #[must_use]
    pub fn command(&self) -> ProcessRequest {
        let mut request = ProcessRequest::new(GIT_PROGRAM);
        request.args = vec![
            // The read must change nothing, so it refreshes no index cache.
            "--no-optional-locks".into(),
            "status".into(),
            "--porcelain=v2".into(),
            // The records are NUL separated, so a name that holds a space, a
            // quote, or a line break still names one entry.
            "-z".into(),
            // The traditional mode names one ignored directory instead of every
            // file below it, so a large build directory costs one record.
            "--ignored=traditional".into(),
            // The mode is explicit, because the ignored mode above collapses a
            // directory only while the untracked mode collapses one too.
            "--untracked-files=normal".into(),
            // The pathspec follows the separator, so a workspace root that
            // starts with a hyphen stays a path.
            "--".into(),
            ".".into(),
        ];
        request.current_dir = Some(self.root.clone());
        request.output_bytes_max = GIT_STATUS_OUTPUT_BYTES_MAX;
        request.deadline = GIT_STATUS_DEADLINE;
        request
    }

    /// Turns the captured output of one status read into one snapshot.
    ///
    /// The call runs on the bounded process service, never on the terminal
    /// event loop, because it locates the repository that holds the workspace
    /// root with a bounded search for the [`REPOSITORY_MARKER`] entry. Git
    /// reports every path against the top level of the repository, so the
    /// snapshot cannot resolve one record without that directory.
    ///
    /// # Errors
    ///
    /// Returns [`GitStatusFailure::Unavailable`] when Git refused the request
    /// and when the workspace root sits inside no repository.
    pub fn publish(&self, output: &ProcessOutput) -> Result<GitStatusSnapshot, GitStatusFailure> {
        // Git reports every refusal, including a directory outside a
        // repository, through its exit code. No branch reads its message text.
        if output.status_code != Some(0) {
            return Err(GitStatusFailure::Unavailable);
        }
        let top_level = repository_root(&self.root).ok_or(GitStatusFailure::Unavailable)?;
        Ok(GitStatusSnapshot::parse(
            &self.root,
            &top_level,
            &output.stdout,
        ))
    }
}

/// Returns the top level of the repository that holds one directory.
///
/// The search inspects at most [`GIT_PATH_DEPTH_MAX`] directories above the
/// start, so a very deep path costs bounded time.
fn repository_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .take(GIT_PATH_DEPTH_MAX)
        .find(|directory| directory.join(REPOSITORY_MARKER).exists())
        .map(Path::to_path_buf)
}

/// The published Git state of every entry below one workspace root.
///
/// The snapshot holds the state of each reported entry, the state that one
/// collapsed directory record covers, and the state that rolls up onto the
/// directories above a changed entry. It performs no filesystem work.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_workspace::{GitStatus, GitStatusSnapshot};
///
/// let root = Path::new("/workspace");
/// let output = b"1 .M N... 100644 100644 100644 aa bb src/main.rs\0! target/\0";
/// let snapshot = GitStatusSnapshot::parse(root, root, output);
///
/// assert_eq!(snapshot.state(&root.join("src/main.rs")), Some(GitStatus::Modified));
/// // The directory above one changed entry carries its state.
/// assert_eq!(snapshot.state(&root.join("src")), Some(GitStatus::Modified));
/// // One ignored directory record covers every entry below it.
/// assert_eq!(snapshot.state(&root.join("target/debug")), Some(GitStatus::Ignored));
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusSnapshot {
    /// The workspace root that the snapshot describes.
    root: PathBuf,
    /// The state of one exact path, including every rolled-up directory.
    entries: BTreeMap<PathBuf, GitStatus>,
    /// The state that one collapsed directory record covers below itself.
    subtrees: BTreeMap<PathBuf, GitStatus>,
}

impl GitStatusSnapshot {
    /// Builds one snapshot from the captured output of `git status`.
    ///
    /// `top_level` is the top level of the repository, because Git reports
    /// every path against that directory. `root` is the workspace root, and the
    /// snapshot drops every record outside it.
    ///
    /// The parser drops every record that names no known type, every record
    /// that holds too few fields, every record whose path leaves the workspace
    /// root, and the last record when the output bound stopped inside it. It
    /// keeps at most [`GIT_STATUS_ENTRIES_MAX`] records.
    #[must_use]
    pub fn parse(root: &Path, top_level: &Path, stdout: &[u8]) -> Self {
        let mut snapshot = Self {
            root: root.to_path_buf(),
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
            let Some(path) = absolute_path(top_level, record.path) else {
                continue;
            };
            if !path.starts_with(root) {
                continue;
            }
            kept += 1;
            snapshot.insert(&path, record.status, record.reach);
        }
        snapshot
    }

    /// Returns the workspace root that the snapshot describes.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the state of one entry, or `None` while Git reports none.
    ///
    /// The lookup answers from the exact path first, and then from the nearest
    /// collapsed directory record above it, so an entry inside an ignored or an
    /// untracked directory reports the state of that directory.
    #[must_use]
    pub fn state(&self, path: &Path) -> Option<GitStatus> {
        if let Some(state) = self.entries.get(path) {
            return Some(*state);
        }
        path.ancestors()
            .take(GIT_PATH_DEPTH_MAX)
            .take_while(|ancestor| ancestor.starts_with(&self.root))
            .find_map(|ancestor| self.subtrees.get(ancestor).copied())
    }

    /// Records one parsed entry and rolls its state up onto its directories.
    fn insert(&mut self, path: &Path, status: GitStatus, reach: Reach) {
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
            .take_while(|ancestor| ancestor.starts_with(&self.root))
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

/// Returns the absolute path of one reported record, or `None` when the record
/// names a path that leaves the repository.
///
/// Git writes every path as a relative path of ordinary components. A record
/// that holds a root component or a parent step is malformed, and resolving it
/// would name an entry outside the repository.
fn absolute_path(top_level: &Path, reported: &str) -> Option<PathBuf> {
    let relative = Path::new(reported.trim_end_matches(DIRECTORY_SUFFIX));
    if relative.as_os_str().is_empty() {
        return None;
    }
    let ordinary = relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    ordinary.then(|| top_level.join(relative))
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
    use std::path::{Path, PathBuf};

    use kvim_runtime::ProcessOutput;

    use crate::temp::TempDir;

    use super::{
        GIT_STATUS_DEADLINE, GIT_STATUS_ENTRIES_MAX, GIT_STATUS_OUTPUT_BYTES_MAX, GitStatus,
        GitStatusFailure, GitStatusRequest, GitStatusSnapshot,
    };

    /// The workspace root of every parser test.
    fn root() -> PathBuf {
        PathBuf::from("/workspace")
    }

    /// Builds one snapshot whose workspace root is the repository top level.
    fn snapshot(output: &str) -> GitStatusSnapshot {
        GitStatusSnapshot::parse(&root(), &root(), output.as_bytes())
    }

    /// Returns the state of one path below the workspace root.
    fn state(snapshot: &GitStatusSnapshot, relative: &str) -> Option<GitStatus> {
        snapshot.state(&root().join(relative))
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
        // must not make the whole workspace read as ignored.
        let snapshot = snapshot("! target/\0");
        assert_eq!(snapshot.state(&root()), None);
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
        // Git reports every path against that top level.
        let top_level = PathBuf::from("/repository");
        let workspace = top_level.join("crates/kvim");
        let output = concat!(
            "1 .M N... 100644 100644 100644 aa bb crates/kvim/src/main.rs\0",
            "1 .M N... 100644 100644 100644 aa bb docs/other.md\0",
        );
        let snapshot = GitStatusSnapshot::parse(&workspace, &top_level, output.as_bytes());
        assert_eq!(
            snapshot.state(&workspace.join("src/main.rs")),
            Some(GitStatus::Modified)
        );
        assert_eq!(snapshot.state(&top_level.join("docs/other.md")), None);
    }

    #[test]
    fn the_command_reads_the_repository_and_never_writes_it() {
        let command = GitStatusRequest::new(root()).command();
        let args: Vec<String> = command
            .args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.contains(&"--no-optional-locks".to_owned()),
            "the read gives up every optional lock of the repository"
        );
        assert!(args.contains(&"--porcelain=v2".to_owned()));
        assert!(args.contains(&"-z".to_owned()));
        assert!(args.contains(&"--ignored=traditional".to_owned()));
        assert_eq!(
            args.last().map(String::as_str),
            Some("."),
            "the pathspec follows the separator, so a root cannot become a flag"
        );
        assert!(command.stdin.is_empty());
        assert_eq!(command.output_bytes_max, GIT_STATUS_OUTPUT_BYTES_MAX);
        assert_eq!(command.deadline, GIT_STATUS_DEADLINE);
    }

    #[test]
    fn a_refused_read_reports_that_no_status_is_available() {
        let output = ProcessOutput {
            status_code: Some(128),
            stdout: Vec::new(),
            stderr: b"fatal: not a git repository".to_vec(),
        };
        assert_eq!(
            GitStatusRequest::new(root()).publish(&output),
            Err(GitStatusFailure::Unavailable),
            "the exit code decides the state, never the message text"
        );
    }

    #[test]
    fn a_directory_outside_a_repository_reports_no_status() {
        let dir = TempDir::new("git-plain");
        let output = ProcessOutput {
            status_code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert_eq!(
            GitStatusRequest::new(dir.path.clone()).publish(&output),
            Err(GitStatusFailure::Unavailable)
        );
    }

    #[test]
    fn a_workspace_below_the_repository_resolves_against_the_top_level() {
        let dir = TempDir::new("git-nested");
        dir.dir(".git");
        let workspace = dir.dir("crates/kvim");
        let output = ProcessOutput {
            status_code: Some(0),
            stdout: b"1 .M N... 100644 100644 100644 aa bb crates/kvim/src/main.rs\0".to_vec(),
            stderr: Vec::new(),
        };
        let snapshot = GitStatusRequest::new(workspace.clone())
            .publish(&output)
            .expect("the temporary directory holds one repository marker");
        assert_eq!(snapshot.root(), workspace.as_path());
        assert_eq!(
            snapshot.state(&workspace.join("src/main.rs")),
            Some(GitStatus::Modified)
        );
    }

    #[test]
    fn the_snapshot_reads_no_path_outside_its_own_root() {
        let snapshot = snapshot(&ordinary(".M", "src/main.rs"));
        assert_eq!(snapshot.state(Path::new("/elsewhere/src/main.rs")), None);
    }
}
