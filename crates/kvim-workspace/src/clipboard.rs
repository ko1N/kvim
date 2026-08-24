//! The file-operation clipboard.
//!
//! The clipboard holds copied or cut workspace entries. It is distinct from the
//! text registers that `editor` owns and from the system clipboard. A file
//! operation never reads a text register, and a text paste never reads this
//! clipboard. See `docs/files.md` and `docs/clipboard.md`.
//!
//! A cut entry stays in place until a paste completes, because the clipboard
//! records the intent only. The paste builds the move.

use kvim_path::{WorktreeDirectoryPath, WorktreeRelativePath};

use super::mutation::{FileOperation, MUTATION_PATHS_MAX, TransferMode};

/// The largest number of entries that the file-operation clipboard holds.
///
/// The value matches [`MUTATION_PATHS_MAX`], so every held entry fits into one
/// paste.
pub const FILE_CLIPBOARD_PATHS_MAX: usize = MUTATION_PATHS_MAX;

/// The entries that one paste transfers, and what the paste does with them.
#[derive(Clone, Debug)]
struct HeldEntries {
    mode: TransferMode,
    paths: Vec<WorktreeRelativePath>,
}

/// The copied or cut workspace entries of one editor.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use kvim_path::{WorktreeDirectoryPath, WorktreeRelativePath};
/// use kvim_workspace::{FileClipboard, TransferMode};
///
/// let mut clipboard = FileClipboard::default();
/// assert!(clipboard.is_empty());
///
/// clipboard.hold(TransferMode::Move, vec![WorktreeRelativePath::new("main.rs")?]);
/// assert_eq!(clipboard.mode(), Some(TransferMode::Move));
///
/// let destination = WorktreeDirectoryPath::Relative(WorktreeRelativePath::new("src")?);
/// let operation = clipboard
///     .paste(&destination)
///     .expect("the clipboard holds one entry");
/// assert!(matches!(operation, kvim_workspace::FileOperation::Transfer { .. }));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Default)]
pub struct FileClipboard {
    held: Option<HeldEntries>,
}

impl FileClipboard {
    /// Holds the named entries for the next paste.
    ///
    /// The clipboard keeps the first [`FILE_CLIPBOARD_PATHS_MAX`] entries and
    /// replaces every entry that it held before.
    pub fn hold(&mut self, mode: TransferMode, mut paths: Vec<WorktreeRelativePath>) {
        paths.truncate(FILE_CLIPBOARD_PATHS_MAX);
        self.held = if paths.is_empty() {
            None
        } else {
            Some(HeldEntries { mode, paths })
        };
    }

    /// Drops every held entry.
    ///
    /// The caller clears the clipboard after a move paste completes, so one cut
    /// never moves the same entry twice.
    pub fn clear(&mut self) {
        self.held = None;
    }

    /// Returns what a paste does with the held entries.
    #[must_use]
    pub fn mode(&self) -> Option<TransferMode> {
        self.held.as_ref().map(|held| held.mode)
    }

    /// Returns the held entries.
    #[must_use]
    pub fn paths(&self) -> &[WorktreeRelativePath] {
        self.held.as_ref().map_or(&[], |held| &held.paths)
    }

    /// Reports whether the clipboard holds no entry.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_none()
    }

    /// Returns the operation that pastes the held entries into one directory.
    ///
    /// Returns `None` while the clipboard holds no entry.
    #[must_use]
    pub fn paste(&self, destination: &WorktreeDirectoryPath) -> Option<FileOperation> {
        let held = self.held.as_ref()?;
        Some(FileOperation::Transfer {
            mode: held.mode,
            sources: held.paths.clone(),
            destination: destination.clone(),
        })
    }
}
