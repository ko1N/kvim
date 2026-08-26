//! The file sidebar that one embedded host draws beside its editor.
//!
//! [`EmbeddedEditor`] already owns one lazy file tree over its worktree root.
//! This module publishes that tree as a host surface. The host reads one
//! bounded list of [`FileRow`] values, draws each row itself, and hands one
//! [`FileSidebarInput`] back for every key that reaches the sidebar. See
//! `docs/embedding.md`.
//!
//! The surface names no type of `kvim-workspace`, which
//! `docs/architecture.md` keeps out of the supported packages. It names its own
//! vocabulary, the paths of `kvim-path`, and the geometry of `kvim-ui`.
//!
//! The tree reads no directory on the host event loop. A row that needs a
//! listing leaves the editor as one unit of work through
//! [`EmbeddedEditor::dispatch`], and the listing reaches the tree through
//! [`EmbeddedEditor::apply`]. The host therefore drives the reads with the one
//! work channel that it already drives for the editor.
//!
//! `crates/kvim-tui/examples/embedded_file_sidebar.rs` is one complete host of
//! one such sidebar.
//!
//! [`EmbeddedEditor`]: super::embed::EmbeddedEditor
//! [`EmbeddedEditor::dispatch`]: super::embed::EmbeddedEditor::dispatch
//! [`EmbeddedEditor::apply`]: super::embed::EmbeddedEditor::apply

use kvim_path::WorktreeRelativePath;
use kvim_ui::{ListMotion, SIDEBAR_LABEL_CHARS_MAX, SIDEBAR_ROWS_MAX};

use super::embed::EditorEvent;

/// The largest number of rows that one file sidebar hands to a host.
///
/// The bound is the row bound of the generic sidebar of `kvim-ui`, because the
/// same rows reach that sidebar inside the editor. One owner keeps the two
/// lists from disagreeing about the same tree.
pub const FILE_SIDEBAR_ROWS_MAX: usize = SIDEBAR_ROWS_MAX;

/// The largest number of characters that one row label holds.
///
/// A filesystem accepts a name that is longer than any sidebar can show. The
/// facade clips the label at this bound, which is the bound that the drawing
/// canvas of `kvim-ui` accepts, so a host can hand any published label to that
/// canvas without a refusal.
pub const FILE_SIDEBAR_LABEL_CHARS_MAX: usize = SIDEBAR_LABEL_CHARS_MAX;

/// What one row of the file sidebar shows.
///
/// The value carries the complete state of the row, so a host draws one row
/// from one match and never combines two flags. See `docs/embedding.md`.
///
/// # Examples
///
/// ```
/// use kvim_tui::FileRowKind;
///
/// assert!(FileRowKind::ClosedDirectory.is_directory());
/// assert!(!FileRowKind::ClosedDirectory.shows_entries());
/// assert!(FileRowKind::OpenDirectory.shows_entries());
/// assert!(!FileRowKind::Note.is_selectable());
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FileRowKind {
    /// One file entry.
    File,
    /// One directory entry that hides the entries below it.
    ClosedDirectory,
    /// One directory entry that shows its loaded entries.
    OpenDirectory,
    /// One directory entry that is open and waits for its listing.
    ///
    /// The listing arrives as one finished unit of work, so this state reports
    /// a read that the host has not handed back yet.
    LoadingDirectory,
    /// One report about the directory of the row.
    ///
    /// The row names no entry. It reports a bounded read, a failed read, or the
    /// number of entries that the hidden-entry policy keeps out of the rows.
    Note,
}

impl FileRowKind {
    /// Reports whether the row names one directory.
    #[inline]
    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(
            self,
            Self::ClosedDirectory | Self::OpenDirectory | Self::LoadingDirectory
        )
    }

    /// Reports whether the rows below this directory are visible.
    #[inline]
    #[must_use]
    pub const fn shows_entries(self) -> bool {
        matches!(self, Self::OpenDirectory | Self::LoadingDirectory)
    }

    /// Reports whether the selection may rest on this row.
    #[inline]
    #[must_use]
    pub const fn is_selectable(self) -> bool {
        !matches!(self, Self::Note)
    }
}

/// One drawable row of the file sidebar of one embedded editor.
///
/// The row holds the text, the indent guides, the depth, the state, and the
/// selection of one visible line. It holds no color, no icon, and no cell, so
/// the host owns the complete look of its own sidebar.
///
/// [`FileRow::guides`] already carries the leading blank that the file tree of
/// kvim draws, because the workspace-root header of that tree is no sibling of
/// the first entries. A host that reproduces the look of kvim draws the guides
/// exactly as they are published. See `docs/windows.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileRow {
    label: String,
    guides: String,
    depth: usize,
    kind: FileRowKind,
    selected: bool,
}

impl FileRow {
    /// Creates one published row.
    pub(super) fn new(
        label: String,
        guides: String,
        depth: usize,
        kind: FileRowKind,
        selected: bool,
    ) -> Self {
        debug_assert!(
            !selected || kind.is_selectable(),
            "the tree rests its selection on an entry row alone"
        );
        let label = if label.chars().count() > FILE_SIDEBAR_LABEL_CHARS_MAX {
            label
                .chars()
                .take(FILE_SIDEBAR_LABEL_CHARS_MAX)
                .collect::<String>()
        } else {
            label
        };
        Self {
            label,
            guides,
            depth,
            kind,
            selected,
        }
    }

    /// Returns the text of the row.
    ///
    /// An entry row carries the entry name. A [`FileRowKind::Note`] row carries
    /// the report that the tree wrote about its directory. The text holds at
    /// most [`FILE_SIDEBAR_LABEL_CHARS_MAX`] characters.
    #[inline]
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the indent guides that belong before the label.
    ///
    /// The string is the complete indent of the row, including the leading
    /// blank of the workspace-root header.
    #[inline]
    #[must_use]
    pub fn guides(&self) -> &str {
        &self.guides
    }

    /// Returns the number of directories between the worktree root and the row.
    #[inline]
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns what the row shows.
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> FileRowKind {
        self.kind
    }

    /// Reports whether the selection rests on this row.
    #[inline]
    #[must_use]
    pub const fn is_selected(&self) -> bool {
        self.selected
    }
}

/// One input that a host applies to the file sidebar of one embedded editor.
///
/// The sidebar runs no host command and opens no file. It reports what the
/// input means through [`FileSidebarOutcome`], and the host decides the effect.
///
/// # Examples
///
/// ```
/// use kvim_tui::{FileSidebarInput, ListMotion};
///
/// let down = FileSidebarInput::Move(ListMotion::Down(1));
/// assert_eq!(down, FileSidebarInput::Move(ListMotion::Down(1)));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSidebarInput {
    /// Move the selection by one bounded move.
    ///
    /// The move stops at the first and the last row, so it never wraps. A
    /// [`FileRowKind::Note`] row takes no selection, so the move takes the
    /// nearest entry row in the direction of travel.
    Move(ListMotion),
    /// Open the selected directory, or activate the selected file.
    ///
    /// An open directory stays open, so this input only ever takes the reader
    /// deeper into the tree. `l` reaches this rule in kvim.
    Open,
    /// Close the selected directory, or select the directory that holds the
    /// selected row.
    ///
    /// Two of these inputs therefore take a file to its directory and then
    /// close that directory. `h` reaches this rule in kvim.
    Close,
    /// Activate the selected file, or open and close the selected directory.
    ///
    /// `Enter` reaches this rule in kvim.
    Activate,
}

/// What one file-sidebar input produced.
///
/// The value returns from the input that produced it, exactly as the focus and
/// close requests of the editor do. Nothing is queued, so no activation waits
/// behind another event. See `docs/embedding.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileSidebarOutcome {
    /// The sidebar applied the input and asks the host for nothing.
    Applied,
    /// The reader activated one file of the worktree.
    ///
    /// The sidebar opened no buffer. A host that shows the file calls
    /// [`EmbeddedEditor::open_file`] with this path.
    ///
    /// [`EmbeddedEditor::open_file`]: super::embed::EmbeddedEditor::open_file
    Activated {
        /// The contained path of the activated file.
        path: WorktreeRelativePath,
    },
}

impl FileSidebarOutcome {
    /// Returns the activation as one editor event.
    ///
    /// A host that keeps one uniform event stream converts the synchronous
    /// outcome with this method, exactly as it converts one
    /// [`InputRequest`](super::embed::InputRequest).
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_path::WorktreeRelativePath;
    /// use kvim_tui::{EditorEvent, FileSidebarOutcome};
    ///
    /// let path = WorktreeRelativePath::new("src/main.rs").expect("the path is contained");
    /// let outcome = FileSidebarOutcome::Activated { path: path.clone() };
    /// assert_eq!(
    ///     outcome.event(),
    ///     Some(EditorEvent::FileActivated { path }),
    /// );
    /// assert_eq!(FileSidebarOutcome::Applied.event(), None);
    /// ```
    #[must_use]
    pub fn event(&self) -> Option<EditorEvent> {
        match self {
            Self::Applied => None,
            Self::Activated { path } => Some(EditorEvent::FileActivated { path: path.clone() }),
        }
    }

    /// Returns the file that the reader activated, if the input activated one.
    #[inline]
    #[must_use]
    pub const fn activated(&self) -> Option<&WorktreeRelativePath> {
        match self {
            Self::Applied => None,
            Self::Activated { path } => Some(path),
        }
    }
}

#[cfg(test)]
#[path = "file_sidebar_tests.rs"]
mod tests;
