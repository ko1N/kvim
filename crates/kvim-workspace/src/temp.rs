//! One temporary directory for a filesystem test.
//!
//! Every test that touches the filesystem works inside its own directory and
//! removes it when it finishes, so no test reads or writes the editor state
//! directory of the user.
//!
//! The module is a test seam, never editor behavior. A test build of this
//! crate always holds it, and the `test-support` feature publishes it for the
//! editor tests of `kvim-tui`, which work over real files in the same way.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// The counter that keeps two temporary directories of one process apart.
static DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary directory that disappears with the test.
pub struct TempDir {
    /// The directory that holds every file of one test.
    pub path: PathBuf,
}

impl TempDir {
    /// Creates one empty directory under the temporary directory of the system.
    pub fn new(label: &str) -> Self {
        let counter = DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kvim-test-{label}-{}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("the temporary directory is writable");
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
