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

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tokio_util::sync::CancellationToken;

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
    /// The absolute paths of the files, in directory order.
    pub files: Vec<PathBuf>,
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
/// use std::path::Path;
///
/// use kvim::workspace::walk_files;
/// use tokio_util::sync::CancellationToken;
///
/// let outcome = walk_files(Path::new("."), &CancellationToken::new());
/// // A complete walk found every file that the ignore rules keep.
/// assert!(!outcome.truncated || outcome.files.len() <= kvim::workspace::WALK_FILES_MAX);
/// ```
#[must_use]
pub fn walk_files(root: &Path, cancellation: &CancellationToken) -> WalkOutcome {
    let mut outcome = WalkOutcome::default();
    let mut queue: VecDeque<Directory> = VecDeque::new();
    queue.push_back(Directory {
        path: root.to_path_buf(),
        relative: String::new(),
        depth: 0,
        rules: None,
    });
    let mut directories = 0_usize;
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
        let Ok(listing) = read_directory(&directory.path) else {
            // An unreadable directory holds no file that the picker can open.
            continue;
        };
        // The shared directory reader keeps one bounded listing, so a very
        // large directory also truncates the walk.
        if listing.truncation != Truncation::Complete {
            outcome.truncated = true;
        }
        let rules = read_rules(&directory);
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
            let path = directory.path.join(&entry.name);
            if is_directory {
                if directory.depth < WALK_DEPTH_MAX {
                    queue.push_back(Directory {
                        path,
                        relative,
                        depth: directory.depth.saturating_add(1),
                        rules: rules.clone(),
                    });
                }
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
    /// The absolute path of the directory.
    path: PathBuf,
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
fn read_rules(directory: &Directory) -> Option<Rc<Rules>> {
    let path = directory.path.join(IGNORE_FILE_NAME);
    let readable =
        fs::metadata(&path).is_ok_and(|it| it.is_file() && it.len() <= IGNORE_FILE_BYTES_MAX);
    if !readable {
        return directory.rules.clone();
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return directory.rules.clone();
    };
    let patterns: Vec<Pattern> = text
        .lines()
        .filter_map(Pattern::parse)
        .take(IGNORE_PATTERNS_MAX)
        .collect();
    if patterns.is_empty() {
        return directory.rules.clone();
    }
    Some(Rc::new(Rules {
        base: directory.relative.clone(),
        patterns,
        parent: directory.rules.clone(),
    }))
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
mod tests {
    use std::path::PathBuf;

    use tokio_util::sync::CancellationToken;

    use crate::workspace::TREE_DIRECTORY_ENTRIES_MAX;
    use crate::workspace::temp::TempDir;

    use super::{Pattern, glob_matches, walk_files};

    /// Returns the walked files, relative to the root and in ascending order.
    fn walked(dir: &TempDir) -> Vec<String> {
        let outcome = walk_files(&dir.path, &CancellationToken::new());
        let mut files: Vec<String> = outcome
            .files
            .iter()
            .filter_map(|path| path.strip_prefix(&dir.path).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect();
        files.sort();
        files
    }

    #[test]
    fn the_walk_collects_every_file_below_the_root() {
        let dir = TempDir::new("walk-plain");
        dir.file("src/main.rs", "fn main() {}\n");
        dir.file("src/tui/mod.rs", "\n");
        dir.file("README.md", "\n");
        assert_eq!(
            walked(&dir),
            vec!["README.md", "src/main.rs", "src/tui/mod.rs"]
        );
    }

    #[test]
    fn the_walk_drops_the_ignored_files_and_the_git_directory() {
        let dir = TempDir::new("walk-ignore");
        dir.file(".gitignore", "target/\n*.tmp\n!keep.tmp\n");
        dir.file("src/main.rs", "\n");
        dir.file("target/debug/kvim", "\n");
        dir.file("scratch.tmp", "\n");
        dir.file("keep.tmp", "\n");
        dir.file(".git/config", "\n");
        assert_eq!(
            walked(&dir),
            vec![".gitignore", "keep.tmp", "src/main.rs"],
            "the ignore file drops target/ and *.tmp, and keeps the negated name"
        );
    }

    #[test]
    fn one_ignore_file_applies_below_its_own_directory() {
        let dir = TempDir::new("walk-nested");
        dir.file("src/.gitignore", "generated.rs\n");
        dir.file("src/generated.rs", "\n");
        dir.file("src/main.rs", "\n");
        dir.file("generated.rs", "\n");
        assert_eq!(
            walked(&dir),
            vec!["generated.rs", "src/.gitignore", "src/main.rs"],
            "the ignore file of `src` names no file of the root"
        );
    }

    #[test]
    fn an_anchored_pattern_names_the_directory_of_its_ignore_file() {
        let dir = TempDir::new("walk-anchored");
        dir.file(".gitignore", "/build\n");
        dir.file("build/output", "\n");
        dir.file("src/build/output", "\n");
        assert_eq!(walked(&dir), vec![".gitignore", "src/build/output"]);
    }

    #[test]
    fn a_cancelled_walk_returns_a_truncated_outcome() {
        let dir = TempDir::new("walk-cancelled");
        dir.file("src/main.rs", "\n");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let outcome = walk_files(&dir.path, &cancellation);
        assert!(outcome.truncated);
        assert_eq!(outcome.files, Vec::<PathBuf>::new());
    }

    #[test]
    fn a_walk_above_one_bound_reports_the_truncation() {
        // The shared directory reader stops at its own entry bound, so a
        // directory above that bound truncates the walk as well.
        let dir = TempDir::new("walk-bound");
        for index in 0..TREE_DIRECTORY_ENTRIES_MAX + 4 {
            dir.file(&format!("f{index}"), "\n");
        }
        let outcome = walk_files(&dir.path, &CancellationToken::new());
        assert!(outcome.truncated);
        assert_eq!(outcome.files.len(), TREE_DIRECTORY_ENTRIES_MAX);
        assert!(outcome.files.len() <= super::WALK_FILES_MAX);
    }

    #[test]
    fn the_glob_matches_the_supported_pattern_subset() {
        let cases = [
            ("*.rs", "main.rs", true),
            ("*.rs", "main.txt", false),
            ("*.rs", "src/main.rs", false),
            ("**/main.rs", "src/tui/main.rs", true),
            ("**/main.rs", "main.rs", true),
            ("src/*.rs", "src/main.rs", true),
            ("src/*.rs", "src/tui/main.rs", false),
            ("ma?n.rs", "main.rs", true),
            ("build", "build", true),
        ];
        for (glob, text, expected) in cases {
            assert_eq!(
                glob_matches(glob, text),
                expected,
                "`{glob}` against `{text}`"
            );
        }
    }

    #[test]
    fn a_comment_and_an_empty_line_name_no_pattern() {
        assert_eq!(Pattern::parse("# comment"), None);
        assert_eq!(Pattern::parse("   "), None);
        assert_eq!(Pattern::parse(""), None);
        assert_eq!(Pattern::parse("/"), None);
    }
}
