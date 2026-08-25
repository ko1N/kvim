//! The bounded workspace file walk that the file picker uses.
//!
//! The walk reads one directory at a time through
//! [`read_directory`](super::read_directory), so the picker and the file tree
//! share one bounded directory reader. Every step blocks, so the walk runs on
//! the bounded worker service only. See `docs/files.md` and
//! `docs/responsiveness.md`.
//!
//! The walk drops the files that the Git ignore rules of the workspace name. It
//! reads the `.gitignore` file of every visited directory and applies the
//! supported pattern subset that `docs/files.md` records. It reads no global
//! ignore file and no Git configuration, because it starts no Git process.

use std::collections::{BTreeSet, VecDeque};
use std::io::Read as _;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use kvim_path::{ResolvedTargetState, WorktreeDirectoryPath, WorktreeRelativePath, WorktreeRoot};

use super::picker::PICKER_CANDIDATES_MAX;
use super::tree::{EntryKind, Truncation, read_directory};

/// The largest number of files that one walk collects.
pub const WALK_FILES_MAX: usize = PICKER_CANDIDATES_MAX;

/// The largest number of directories that one walk reads.
pub const WALK_DIRECTORIES_MAX: usize = 4096;

/// The largest depth below the workspace root that the walk reaches.
pub const WALK_DEPTH_MAX: usize = 16;

/// The largest ignore file that the walk reads, in bytes.
pub const IGNORE_FILE_BYTES_MAX: u64 = 64 * 1024;

/// The largest number of patterns that the walk keeps for one ignore file.
pub const IGNORE_PATTERNS_MAX: usize = 512;

/// The name of the ignore file that the walk reads.
const IGNORE_FILE_NAME: &str = ".gitignore";

/// The directory that every walk skips, as Git itself does.
const GIT_DIRECTORY: &str = ".git";

/// The separator that the ignore patterns use.
const SEPARATOR: char = '/';

/// The files that one bounded walk found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WalkOutcome {
    /// The validated paths of the files, in directory order.
    pub files: Vec<WorktreeRelativePath>,
    /// Reports whether the walk stopped at one of its bounds.
    pub truncated: bool,
}

/// Collects the files of one workspace, without the ignored files.
///
/// The call blocks. Run it on the bounded worker service only. The walk checks
/// the cancellation token before every directory, so a superseded walk stops as
/// early as one directory read allows.
///
/// The walk stops at [`WALK_FILES_MAX`] files, [`WALK_DIRECTORIES_MAX`]
/// directories, and [`WALK_DEPTH_MAX`] levels, and reports the truncation.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
///
/// use kvim_path::WorktreeRoot;
/// use kvim_workspace::walk_files;
/// use tokio_util::sync::CancellationToken;
///
/// let root = Arc::new(WorktreeRoot::open(std::env::current_dir()?)?);
/// let outcome = walk_files(root, &CancellationToken::new());
/// // A complete walk found every file that the ignore rules keep.
/// assert!(!outcome.truncated || outcome.files.len() <= kvim_workspace::WALK_FILES_MAX);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn walk_files(root: Arc<WorktreeRoot>, cancellation: &CancellationToken) -> WalkOutcome {
    let mut outcome = WalkOutcome::default();
    let mut queue: VecDeque<Directory> = VecDeque::new();
    queue.push_back(Directory {
        path: WorktreeDirectoryPath::Root,
        relative: String::new(),
        depth: 0,
        rules: None,
    });
    let mut directories = 0_usize;
    let mut resolved = BTreeSet::new();
    while let Some(directory) = queue.pop_front() {
        if cancellation.is_cancelled() {
            outcome.truncated = true;
            return outcome;
        }
        directories = directories.saturating_add(1);
        if directories > WALK_DIRECTORIES_MAX {
            outcome.truncated = true;
            return outcome;
        }
        let Ok(listing) = read_directory(&root, &directory.path) else {
            // An unreadable directory holds no file that the picker can open.
            outcome.truncated = true;
            continue;
        };
        if !resolved.insert(listing.identity.clone()) {
            // A contained link aliases a directory that the walk already read.
            // Keep source order deterministic and never repeat its subtree.
            outcome.truncated = true;
            continue;
        }
        // The shared directory reader keeps one bounded listing, so a very
        // large directory also truncates the walk.
        if listing.truncation != Truncation::Complete {
            outcome.truncated = true;
        }
        let rules_read = read_rules(&root, &directory);
        outcome.truncated |= rules_read.truncated;
        let rules = rules_read.rules;
        for entry in listing.entries {
            if entry.name == GIT_DIRECTORY {
                continue;
            }
            let relative = join(&directory.relative, &entry.name);
            let is_directory = entry.kind == EntryKind::Directory;
            if rules
                .as_deref()
                .is_some_and(|rules| rules.ignores(&relative, is_directory))
            {
                continue;
            }
            let Ok(path) = WorktreeRelativePath::new(Path::new(&relative)) else {
                outcome.truncated = true;
                continue;
            };
            if is_directory {
                if directory.depth < WALK_DEPTH_MAX {
                    queue.push_back(Directory {
                        path: WorktreeDirectoryPath::Relative(path),
                        relative,
                        depth: directory.depth.saturating_add(1),
                        rules: rules.clone(),
                    });
                } else {
                    outcome.truncated = true;
                }
                continue;
            }
            let Ok(resolved) = root.resolve(&path) else {
                // An escaping, dangling, or looping link is not an openable
                // picker candidate. Report the omission through the existing
                // visible truncation state.
                outcome.truncated = true;
                continue;
            };
            if resolved.state() != ResolvedTargetState::Existing
                || root.revalidate(&path, &resolved).is_err()
            {
                outcome.truncated = true;
                continue;
            }
            if outcome.files.len() >= WALK_FILES_MAX {
                outcome.truncated = true;
                return outcome;
            }
            outcome.files.push(path);
        }
    }
    outcome
}

/// One directory that waits for its read.
struct Directory {
    /// The validated directory at or below the root.
    path: WorktreeDirectoryPath,
    /// The path of the directory below the root, with `/` separators.
    relative: String,
    /// The number of levels between the root and this directory.
    depth: usize,
    /// The ignore rules that the parent directories established.
    rules: Option<Rc<Rules>>,
}

/// Returns the ignore rules that apply inside one directory.
///
/// The rules of the directory sit above the rules of its parents, so a deeper
/// ignore file decides first.
fn read_rules(root: &WorktreeRoot, directory: &Directory) -> RulesRead {
    let relative = join(&directory.relative, IGNORE_FILE_NAME);
    let Ok(requested) = WorktreeRelativePath::new(Path::new(&relative)) else {
        return RulesRead::truncated(directory);
    };
    let Ok(resolved) = root.resolve(&requested) else {
        return RulesRead::truncated(directory);
    };
    let metadata = match root.directory().metadata(resolved.path().as_path()) {
        Ok(metadata) => metadata,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && resolved.state() == ResolvedTargetState::Missing =>
        {
            return RulesRead::inherited(directory);
        }
        Err(_) => return RulesRead::truncated(directory),
    };
    if !metadata.is_file() {
        return RulesRead::truncated(directory);
    }
    if metadata.len() > IGNORE_FILE_BYTES_MAX {
        return RulesRead::truncated(directory);
    }
    let Ok(file) = root.directory().open(resolved.path().as_path()) else {
        return RulesRead::truncated(directory);
    };
    let Ok(opened) = file.metadata() else {
        return RulesRead::truncated(directory);
    };
    let opened = metadata_identity(&opened);
    let mut bytes = Vec::new();
    let Ok(_) = file
        .take(IGNORE_FILE_BYTES_MAX.saturating_add(1))
        .read_to_end(&mut bytes)
    else {
        return RulesRead::truncated(directory);
    };
    if bytes.len() > usize::try_from(IGNORE_FILE_BYTES_MAX).unwrap_or(usize::MAX) {
        return RulesRead::truncated(directory);
    }
    let Ok(current) = root.directory().metadata(resolved.path().as_path()) else {
        return RulesRead::truncated(directory);
    };
    if metadata_identity(&current) != opened || root.revalidate(&requested, &resolved).is_err() {
        return RulesRead::truncated(directory);
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return RulesRead::truncated(directory);
    };
    let mut patterns: Vec<Pattern> = text
        .lines()
        .filter_map(Pattern::parse)
        .take(IGNORE_PATTERNS_MAX.saturating_add(1))
        .collect();
    let truncated = patterns.len() > IGNORE_PATTERNS_MAX;
    patterns.truncate(IGNORE_PATTERNS_MAX);
    if patterns.is_empty() {
        return RulesRead {
            rules: directory.rules.clone(),
            truncated,
        };
    }
    RulesRead {
        rules: Some(Rc::new(Rules {
            base: directory.relative.clone(),
            patterns,
            parent: directory.rules.clone(),
        })),
        truncated,
    }
}

struct RulesRead {
    rules: Option<Rc<Rules>>,
    truncated: bool,
}

impl RulesRead {
    fn inherited(directory: &Directory) -> Self {
        Self {
            rules: directory.rules.clone(),
            truncated: false,
        }
    }

    fn truncated(directory: &Directory) -> Self {
        Self {
            rules: directory.rules.clone(),
            truncated: true,
        }
    }
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

/// Returns the path of one entry inside one directory.
fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        return name.to_owned();
    }
    format!("{directory}{SEPARATOR}{name}")
}

/// The ignore patterns of one directory and of every directory above it.
struct Rules {
    /// The directory that owns the patterns, relative to the workspace root.
    base: String,
    /// The patterns of that directory, in file order.
    patterns: Vec<Pattern>,
    /// The rules of the directories above.
    parent: Option<Rc<Rules>>,
}

impl Rules {
    /// Reports whether the rules drop one entry.
    ///
    /// The innermost ignore file decides first, and the last matching pattern
    /// of one file wins. Both rules follow Git.
    fn ignores(&self, relative: &str, is_directory: bool) -> bool {
        let mut node = Some(self);
        while let Some(rules) = node {
            if let Some(local) = strip_base(&rules.base, relative) {
                for pattern in rules.patterns.iter().rev() {
                    if pattern.matches(local, is_directory) {
                        return !pattern.negated;
                    }
                }
            }
            node = rules.parent.as_deref();
        }
        false
    }
}

/// Returns the path of one entry below the directory that owns one rule.
fn strip_base<'a>(base: &str, relative: &'a str) -> Option<&'a str> {
    if base.is_empty() {
        return Some(relative);
    }
    relative
        .strip_prefix(base)
        .and_then(|rest| rest.strip_prefix(SEPARATOR))
}

/// One supported Git ignore pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Pattern {
    /// The glob of the pattern, without its markers.
    glob: String,
    /// Reports whether the pattern keeps a file that an earlier pattern drops.
    negated: bool,
    /// Reports whether the pattern names directories alone.
    directory_only: bool,
    /// Reports whether the pattern starts at the directory of its ignore file.
    anchored: bool,
}

impl Pattern {
    /// Parses one line of one ignore file.
    ///
    /// A comment, an empty line, and a line that holds only spaces name no
    /// pattern.
    fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (negated, rest) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let (directory_only, rest) = match rest.strip_suffix(SEPARATOR) {
            Some(rest) => (true, rest),
            None => (false, rest),
        };
        let (rooted, rest) = match rest.strip_prefix(SEPARATOR) {
            Some(rest) => (true, rest),
            None => (false, rest),
        };
        if rest.is_empty() {
            return None;
        }
        Some(Self {
            glob: rest.to_owned(),
            negated,
            directory_only,
            // A pattern that holds a separator starts at the directory of its
            // ignore file. Every other pattern names one entry at any depth.
            anchored: rooted || rest.contains(SEPARATOR),
        })
    }

    /// Reports whether the pattern names one entry.
    fn matches(&self, relative: &str, is_directory: bool) -> bool {
        if self.directory_only && !is_directory {
            return false;
        }
        if self.anchored {
            return glob_matches(&self.glob, relative);
        }
        let name = relative.rsplit(SEPARATOR).next().unwrap_or(relative);
        glob_matches(&self.glob, name)
    }
}

/// Reports whether one glob matches one text.
///
/// The glob supports `?` for one character, `*` for any characters inside one
/// path component, and `**` for any characters across components. Every other
/// character matches itself.
fn glob_matches(glob: &str, text: &str) -> bool {
    let pattern: Vec<char> = glob.chars().collect();
    let value: Vec<char> = text.chars().collect();
    let mut pattern_index = 0_usize;
    let mut text_index = 0_usize;
    // The star records where the scan restarts after a longer match fails.
    let mut star: Option<Star> = None;
    while text_index < value.len() {
        match pattern.get(pattern_index) {
            Some('*') => {
                let (next, crosses) = read_star(&pattern, pattern_index);
                star = Some(Star {
                    pattern_index: next,
                    text_index,
                    crosses,
                });
                pattern_index = next;
                continue;
            }
            Some('?') if value[text_index] != SEPARATOR => {
                pattern_index = pattern_index.saturating_add(1);
                text_index = text_index.saturating_add(1);
                continue;
            }
            Some(expected) if *expected == value[text_index] => {
                pattern_index = pattern_index.saturating_add(1);
                text_index = text_index.saturating_add(1);
                continue;
            }
            Some(_) | None => {}
        }
        let Some(mark) = star else {
            return false;
        };
        if !mark.crosses && value[mark.text_index] == SEPARATOR {
            return false;
        }
        text_index = mark.text_index.saturating_add(1);
        pattern_index = mark.pattern_index;
        star = Some(Star { text_index, ..mark });
    }
    pattern
        .iter()
        .skip(pattern_index)
        .all(|value| *value == '*')
}

/// The restart position of one star inside one glob.
#[derive(Clone, Copy, Debug)]
struct Star {
    /// The glob position that follows the star.
    pattern_index: usize,
    /// The text position that the next restart consumes.
    text_index: usize,
    /// Reports whether the star matches a path separator.
    crosses: bool,
}

/// Returns the glob position after one star and whether it crosses a separator.
fn read_star(pattern: &[char], index: usize) -> (usize, bool) {
    let mut next = index.saturating_add(1);
    if pattern.get(next) != Some(&'*') {
        return (next, false);
    }
    next = next.saturating_add(1);
    // `**/` also matches the empty directory prefix, so it consumes its own
    // separator.
    if pattern.get(next) == Some(&SEPARATOR) {
        next = next.saturating_add(1);
    }
    (next, true)
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
