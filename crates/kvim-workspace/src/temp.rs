//! One temporary directory for a filesystem test.
//!
//! Every test that touches the filesystem works inside its own directory and
//! removes it when it finishes, so no test reads or writes the editor state
//! directory of the user.
//!
//! [`TempRepository`] adds one temporary Git repository for a test that runs
//! the real `git` command end to end. Such a test proves that the flags of the
//! status read are right, which a recorded output can never prove.
//!
//! The module is a test seam, never editor behavior. A test build of this
//! crate always holds it, and the `test-support` feature publishes it for the
//! editor tests of `kvim-tui`, which work over real files in the same way.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use super::GIT_PROGRAM;

/// The counter that keeps two temporary directories of one process apart.
static DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary directory that disappears with the test.
pub struct TempDir {
    /// The canonical directory that holds every file of one test.
    pub path: PathBuf,
}

impl TempDir {
    /// Creates one empty directory under the temporary directory of the system.
    ///
    /// [`TempDir::path`] is always canonical, because the call resolves every
    /// symbolic link of the ambient temporary directory once. A host that
    /// reaches its temporary directory through a link, as macOS does with
    /// `/tmp`, would otherwise hand each test a path that no loaded buffer and
    /// no file-tree row ever holds, and a test would pass or fail by the shape
    /// of the host. See `docs/architecture.md`.
    pub fn new(label: &str) -> Self {
        let counter = DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kvim-test-{label}-{}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("the temporary directory is writable");
        let path = fs::canonicalize(&path).expect("the new directory exists");
        Self { path }
    }

    /// Returns one path inside the directory.
    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Creates one directory, and every directory above it, inside the
    /// temporary directory.
    pub fn dir(&self, name: &str) -> PathBuf {
        let path = self.join(name);
        fs::create_dir_all(&path).expect("the temporary directory is writable");
        path
    }

    /// Writes one file and every directory above it inside the temporary
    /// directory.
    pub fn file(&self, name: &str, content: &str) -> PathBuf {
        let path = self.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("the temporary directory is writable");
        }
        fs::write(&path, content).expect("the temporary directory is writable");
        path
    }

    /// Writes one file inside the directory and returns its path.
    pub fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.join(name);
        fs::write(&path, content).expect("the temporary directory is writable");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The branch that every temporary repository starts on.
///
/// The name is explicit, so a Git release that changes its own default cannot
/// change the result of a test or add a hint to its output.
const INITIAL_BRANCH: &str = "main";

/// The author that every `git` invocation of a test states for itself.
///
/// The build sandbox names no author of its own, so a commit without this pair
/// would fail there and pass on a developer machine.
const TEST_AUTHOR: [&str; 4] = [
    "-c",
    "user.email=tests@kvim.invalid",
    "-c",
    "user.name=kvim Tests",
];

/// One temporary Git repository for a test that runs the real command.
///
/// The repository lives inside its own [`TempDir`] and disappears with the
/// test, so no test reads or writes the repository that holds kvim itself.
///
/// Every invocation states its own author and reads no configuration of the
/// host. The system file and the global file are both neutralized for the child
/// commands below, and the repository additionally sets its own empty ignore
/// file, because the status read of the editor runs through the process service
/// and inherits the settings of the editor instead of these. A test would
/// otherwise pass or fail by the configuration of the developer, which
/// `docs/architecture.md` forbids.
pub struct TempRepository {
    directory: TempDir,
}

impl TempRepository {
    /// Creates one empty repository on the `INITIAL_BRANCH` branch.
    #[must_use]
    pub fn new(label: &str) -> Self {
        let repository = Self {
            directory: TempDir::new(label),
        };
        repository.git(&["init", "-b", INITIAL_BRANCH]);
        // The local value wins over the global one and over the system one, so
        // an ignore file of the host names no entry of this repository.
        repository.git(&["config", "core.excludesFile", "/dev/null"]);
        repository
    }

    /// Returns the working tree root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.directory.path
    }

    /// Returns one path inside the working tree.
    #[must_use]
    pub fn join(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }

    /// Writes one file and every directory above it inside the working tree.
    pub fn file(&self, name: &str, content: &str) -> PathBuf {
        self.directory.file(name, content)
    }

    /// Runs one `git` command inside the working tree.
    ///
    /// # Panics
    ///
    /// Panics when the command cannot start or reports a failure. The
    /// development shell and the build sandbox both provide `git`, so either
    /// outcome is a defect of the test or of the environment that runs it.
    pub fn git(&self, args: &[&str]) {
        let _ = self.answer(args);
    }

    /// Returns the full object identifier of the current `HEAD` commit.
    ///
    /// A review base is one full commit object identifier, so a test that
    /// captures one worktree diff needs the exact value that Git wrote.
    ///
    /// # Panics
    ///
    /// Panics when the repository holds no commit, because every caller records
    /// one first.
    #[must_use]
    pub fn head(&self) -> String {
        String::from_utf8(self.answer(&["rev-parse", "HEAD"]))
            .expect("git writes one hexadecimal identifier")
            .trim()
            .to_owned()
    }

    /// Runs one `git` command and returns its standard output.
    fn answer(&self, args: &[&str]) -> Vec<u8> {
        let output = Command::new(GIT_PROGRAM)
            .args(TEST_AUTHOR)
            .args(args)
            .current_dir(&self.directory.path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(Stdio::null())
            .output()
            .expect("the development shell and the build sandbox both provide git");
        assert!(
            output.status.success(),
            "the git command {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    /// Records every current file of the working tree in one commit.
    ///
    /// A test needs one commit, because an entry is only staged or modified
    /// against a commit that already holds it.
    pub fn commit(&self, message: &str) {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--message", message]);
    }
}
