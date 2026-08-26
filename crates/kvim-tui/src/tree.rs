//! The file-tree sidebar: the visible state, its transitions, and its rows.
//!
//! The sidebar owns one [`FileTree`], the file-operation clipboard, and the
//! scroll offset of the visible rows. It performs no filesystem work. Every
//! directory read and every mutation becomes one [`WorkspaceRequest`] that the
//! event loop hands to the bounded worker service, and the typed result returns
//! through one transition. See `docs/files.md` and `docs/responsiveness.md`.
//!
//! The sidebar runs one workspace operation at a time, as the file operations
//! do, so a result can never reach a tree state that a newer operation already
//! replaced.
//!
//! The rows, the selection moves, the scroll offset, and the visible placements
//! belong to the generic [`SidebarState`] of `kvim-ui`. This module keeps every
//! filesystem, Git, and clipboard meaning, and it hands that state one opaque
//! identity for each row.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use kvim_editor::{SearchDirection, Viewport};
use kvim_path::{WorktreeDirectoryPath, WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{WatchBatch, WatchFidelity};
use kvim_settings::FileTreeIcons;
use kvim_ui::{
    ListMotion, RowKind, SIDEBAR_GUIDE_BLANK, SidebarCanvas, SidebarEvent, SidebarInput,
    SidebarRow, SidebarState, sidebar_guides,
};
use kvim_workspace::{
    DirectoryListing, EntryKind, Expansion, FileClipboard, FileOperation, FileTree, GitStatus,
    GitStatusRequest, GitStatusSnapshot, LinkKind, MutateRequest, MutationOutcome, Notice,
    OpenBuffer, Overwrite, ReadError, RowContent, TakenDestination, TransferMode, TreeRow,
    WorkspaceRequest,
};

use super::buffer_view::RegionFocus;
use super::file_sidebar::{
    FILE_SIDEBAR_ROWS_MAX, FileRow, FileRowGit, FileRowKind, FileSidebarInput, FileSidebarOutcome,
    LabelMatch, draw_file_row, draw_git_mark, label_offset_cells,
};
use super::icons::{directory_icon, row_icon};
use super::theme::{Theme, ThemeRole};

/// The number of rows that the sidebar title occupies.
pub(super) const TREE_TITLE_ROWS: u16 = 1;

/// The largest number of bytes that one entry name accepts.
///
/// The bound protects the prompt line against a name that no filesystem
/// accepts. A Linux filesystem such as ext4 stops one name at 255 raw bytes.
/// macOS stops one name at 255 UTF-16 code units instead. One UTF-8 byte
/// never encodes less than one UTF-16 code unit of the same character. A name
/// within 255 UTF-8 bytes therefore stays within the macOS limit too. The
/// unit is bytes, because that unit matches what both filesystems enforce.
pub(super) const TREE_NAME_BYTES_MAX: usize = 255;

/// The marker of one expanded directory row, while the tree hides its icons.
pub(super) const EXPANDED_MARKER: &str = "▾ ";

/// The marker of one collapsed directory row, while the tree hides its icons.
pub(super) const COLLAPSED_MARKER: &str = "▸ ";

/// The number of cells that the selection mark reserves at the left edge.
///
/// The column stays blank on every other row, so one mark never moves a name.
///
/// `file_sidebar` republishes this width as
/// [`FILE_SIDEBAR_MARK_CELLS`](super::file_sidebar::FILE_SIDEBAR_MARK_CELLS),
/// so a host aligns a tree of its own beside the file tree of the editor.
pub(super) const MARK_CELLS: usize = 1;

/// The mark of the selected row, at the left edge of the sidebar.
///
/// `file_sidebar` republishes this glyph as
/// [`FILE_SIDEBAR_SELECTION_MARK`](super::file_sidebar::FILE_SIDEBAR_SELECTION_MARK),
/// so a host that draws its own rows can draw kvim's own mark.
pub(super) const SELECTION_MARK: &str = "▌";

/// The entry names whose content one tool generates.
///
/// The tree dims these entries, because they hold machine output instead of
/// work of the user. The list is presentation data beside the icon table, and
/// it names a small fixed set, so one lookup costs a bounded number of
/// comparisons. The Git ignore rules dim the same way, and the list stays the
/// answer for a workspace that is no repository.
///
/// The workspace watcher ignores exactly these names, so one build writes no
/// event at all and the two rules can never disagree about one entry. A host
/// that starts a [`FileWatcher`](kvim_runtime::FileWatcher) for one editor
/// hands this list to it, so the watch and the tree stay one rule. See
/// `docs/files.md` and `docs/git.md`.
///
/// ```
/// assert!(kvim_tui::GENERATED_NAMES.contains(&"target"));
/// ```
pub const GENERATED_NAMES: [&str; 5] = [".direnv", ".git", "__pycache__", "node_modules", "target"];

/// The number of cells that the Git mark reserves at the right edge.
///
/// The column stays blank on every row without a Git state, so one mark never
/// moves a name.
pub(super) const GIT_MARK_CELLS: u16 = 1;

/// Returns the mark of one recorded Git state.
///
/// The glyphs are presentation data beside the icon table, as the reference
/// configuration paints them: a filled shape for a change that the reader owns,
/// an outlined shape for an entry that the repository does not track yet, and a
/// checked box for an entry that the ignore rules name. [`FileRowGit::glyph`]
/// of `file_sidebar.rs` holds the one table, so the standalone tree and a host
/// that draws kvim's own marks can never disagree about one state.
pub(super) const fn git_mark(status: GitStatus) -> &'static str {
    host_git_state(status).glyph()
}

/// What one sidebar row shows, beyond the name that it holds.
///
/// The value decides the style of the row and the suffix behind the name. A
/// row takes exactly one state, so no two of them can disagree, and a held
/// entry carries the mode of the file operation instead of two flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RowState {
    /// One ordinary directory entry.
    Directory,
    /// One ordinary file entry.
    File,
    /// One entry whose content a tool generates.
    Generated,
    /// One entry that the file-operation clipboard holds for the next paste.
    Held(TransferMode),
    /// One row that counts the entries that the tree keeps out of its rows.
    Omitted,
    /// One row that reports a bounded or a failed directory read.
    Incomplete,
}

impl RowState {
    /// Returns the state of one visible row.
    ///
    /// A held entry wins over every other state, because the pending file
    /// operation is the report that the reader waits for. An entry that the Git
    /// ignore rules name reads as generated content, so one rule decides the
    /// color of every quiet row. See `docs/git.md`.
    fn of(row: &TreeRow, held: Option<TransferMode>, git: Option<GitStatus>) -> Self {
        if let Some(mode) = held {
            return Self::Held(mode);
        }
        if git == Some(GitStatus::Ignored) && row.is_selectable() {
            return Self::Generated;
        }
        match &row.content {
            // A count of hidden entries reports a choice of the reader, and a
            // bounded or failed read reports a limit. The two must not read
            // alike, so each one takes its own state.
            RowContent::Notice(Notice::Hidden { .. }) => Self::Omitted,
            RowContent::Notice(Notice::Truncated { .. } | Notice::Unreadable) => Self::Incomplete,
            RowContent::Directory { name, .. } | RowContent::File { name, .. }
                if GENERATED_NAMES.contains(&name.as_str()) =>
            {
                Self::Generated
            }
            RowContent::Directory { .. } => Self::Directory,
            RowContent::File { .. } => Self::File,
        }
    }

    /// Returns the text that follows the name of the row.
    pub(super) const fn suffix(self) -> &'static str {
        match self {
            Self::Held(TransferMode::Move) => " (cut)",
            Self::Held(TransferMode::Copy) => " (copied)",
            Self::Directory | Self::File | Self::Generated | Self::Omitted | Self::Incomplete => "",
        }
    }

    /// Returns the role that colors the row.
    pub(super) const fn role(self) -> ThemeRole {
        match self {
            Self::Directory => ThemeRole::TreeDirectory,
            Self::File => ThemeRole::Text,
            Self::Generated | Self::Held(_) => ThemeRole::TreeMuted,
            Self::Omitted => ThemeRole::TreeNotice,
            Self::Incomplete => ThemeRole::TreeIncomplete,
        }
    }
}

/// The result of one completed Git status read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GitPublication {
    /// The snapshot describes the workspace that the sidebar shows.
    Applied,
    /// The snapshot describes another workspace root, so the sidebar drops it.
    Obsolete,
}

/// The result of one move between the matches of the file-tree search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TreeMatchOutcome {
    /// The selection moved to one match.
    Moved,
    /// The tree shows no match, so the selection stayed where it was.
    Missed,
}

/// One move of the file-tree selection, measured in rows.
///
/// The sidebar is a flat row list, so the buffer navigation keys mean the same
/// here as in a window: `Ctrl-D` and `Ctrl-U` move half a page, `Ctrl-F` and
/// `Ctrl-B` move a full page, and `gg` and `G` reach a named row. The caller
/// converts the command and its count into one of these values, and the sidebar
/// bounds the move by its own rows. See `docs/input-actions.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TreeMotion {
    /// Move down the given number of rows.
    Down(usize),
    /// Move up the given number of rows.
    Up(usize),
    /// Move to the row of the given index.
    ToRow(usize),
    /// Move to the last row.
    LastRow,
}

/// The workspace operation that the sidebar waits for.
///
/// The editor runs one operation at a time, so the sidebar holds one value.
#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingWorkspace {
    /// One directory read runs.
    Read {
        /// The directory that the read names.
        path: PathBuf,
    },
    /// One workspace mutation runs.
    Mutation {
        /// The operation that the mutation performs.
        ///
        /// A refused destination returns the operation to the editor, which
        /// asks the user before it stages the same operation again. See
        /// `docs/files.md`.
        operation: FileOperation,
    },
}

/// The reason that the sidebar refused one operation.
///
/// The refusal happens before the request reaches the worker, so it changes no
/// workspace state and no buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TreeRefusal {
    /// The tree shows no selected entry.
    NoSelection,
    /// The prompt line held no name.
    EmptyName,
    /// The name holds a path component instead of one entry name.
    NameHasPath,
    /// The name holds more bytes than [`TREE_NAME_BYTES_MAX`] allows.
    NameTooLong,
    /// The file-operation clipboard holds no entry.
    ClipboardEmpty,
    /// The tree lost the entry between the question and the answer.
    EntryGone,
    /// One workspace operation is already running.
    Busy,
}

impl TreeRefusal {
    /// Returns the message that the message line shows.
    pub(super) fn message(self) -> String {
        match self {
            Self::NoSelection => "the file tree shows no selected entry".to_owned(),
            Self::EmptyName => "the name is empty".to_owned(),
            Self::NameHasPath => "the name must hold one entry name, not a path".to_owned(),
            Self::NameTooLong => {
                format!("the name holds more than {TREE_NAME_BYTES_MAX} bytes")
            }
            Self::ClipboardEmpty => "the file clipboard holds no entry".to_owned(),
            Self::EntryGone => "the file tree no longer shows the entry".to_owned(),
            Self::Busy => "one workspace operation is already running".to_owned(),
        }
    }
}

/// The file-tree sidebar of one editor.
#[derive(Debug)]
pub(super) struct TreeSidebar {
    root: Arc<WorktreeRoot>,
    tree: FileTree,
    clipboard: FileClipboard,
    /// The generic rows: the selection, the scroll offset, and the placements.
    ///
    /// The identity of one row is its position in [`FileTree::rows`], so the
    /// tree stays the one owner of every path. [`TreeSidebar::sync_rows`]
    /// copies the current rows and the current selection into this state.
    view: SidebarState<usize>,
    /// The operation that waits for the bounded worker service.
    outbox: Option<WorkspaceRequest>,
    /// The operation that the sidebar waits for.
    pending: Option<PendingWorkspace>,
    /// The Git status read that the event loop must submit.
    ///
    /// One read runs at a time. A newer trigger replaces the queued request, so
    /// the sidebar never asks for two reads of one workspace. See
    /// `docs/git.md`.
    git_outbox: Option<GitStatusRequest>,
    /// The published Git state of the workspace, while one read produced it.
    git: Option<GitStatusSnapshot>,
    /// The home directory of the user, while the environment names one.
    ///
    /// The header shortens the root path against this value. The sidebar reads
    /// the environment once, at construction, so the render path stays free of
    /// every ambient read.
    home: Option<PathBuf>,
}

impl TreeSidebar {
    /// Creates one sidebar over a workspace root and asks for its first read.
    pub(super) fn new(root: Arc<WorktreeRoot>) -> Self {
        let root_path = root.as_path().to_path_buf();
        let mut sidebar = Self {
            root,
            tree: FileTree::new(root_path),
            clipboard: FileClipboard::default(),
            view: SidebarState::default(),
            outbox: None,
            pending: None,
            git_outbox: None,
            git: None,
            home: env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
        };
        sidebar.pump();
        sidebar.request_git_status();
        sidebar
    }

    /// Returns the tree model.
    pub(super) const fn tree(&self) -> &FileTree {
        &self.tree
    }

    /// Returns the workspace root as the header row shows it.
    pub(super) fn root_label(&self) -> String {
        root_label(self.tree.root(), self.home.as_deref())
    }

    /// Returns how the file-operation clipboard holds one entry.
    ///
    /// The sidebar keeps no second copy of that state, so one hold, one paste,
    /// and one cancel each change exactly one place.
    pub(super) fn held_mode(&self, path: &Path) -> Option<TransferMode> {
        let mode = self.clipboard.mode()?;
        let contained = self.contained(path)?;
        self.clipboard.paths().contains(&contained).then_some(mode)
    }

    /// Returns the generic rows that the renderer walks.
    pub(super) const fn view(&self) -> &SidebarState<usize> {
        &self.view
    }

    /// Returns the workspace request that the event loop must submit.
    pub(super) fn take_request(&mut self) -> Option<WorkspaceRequest> {
        self.outbox.take()
    }

    /// Returns the Git status read that the event loop must submit.
    pub(super) fn take_git_request(&mut self) -> Option<GitStatusRequest> {
        self.git_outbox.take()
    }

    /// Asks for a new read of the repository state.
    ///
    /// The sidebar queues one read. A save, a workspace mutation, a
    /// workspace-watch burst, and the refresh command each reach this entry
    /// point, and no timer does, because the renderer runs no unconditional
    /// frame loop. See `docs/git.md`.
    pub(super) fn request_git_status(&mut self) {
        self.git_outbox = Some(GitStatusRequest::new(Arc::clone(&self.root)));
    }

    /// Queues the next command of one status read that needs a further step.
    ///
    /// A newer refresh replaces the queued request, exactly as a new trigger
    /// does, so the sidebar still runs one read at a time. See `docs/git.md`.
    pub(super) fn resume_git_status(&mut self, request: GitStatusRequest) {
        self.git_outbox = Some(request);
    }

    /// Publishes one completed Git status read.
    ///
    /// The publication gate already rejects the result of a request that a
    /// newer one replaced. This second check rejects a snapshot of another
    /// workspace root from the visible state itself.
    pub(super) fn apply_git_status(&mut self, snapshot: GitStatusSnapshot) -> GitPublication {
        if snapshot.root() != &*self.root {
            return GitPublication::Obsolete;
        }
        self.git = Some(snapshot);
        GitPublication::Applied
    }

    /// Returns the recorded Git state of one entry, or `None` while none is
    /// known.
    pub(super) fn git_state(&self, path: &Path) -> Option<GitStatus> {
        self.git.as_ref()?.state(&self.contained(path)?)
    }

    /// Applies one completed directory read and asks for the next one.
    pub(super) fn apply_directory(
        &mut self,
        path: &Path,
        outcome: Result<DirectoryListing, ReadError>,
    ) {
        self.pending = None;
        match outcome {
            Ok(listing) => self.tree.apply_listing(listing),
            Err(_) => self.tree.apply_read_failure(path),
        }
        self.pump();
    }

    /// Applies the tree half of one completed mutation.
    ///
    /// The caller applies the buffer paths of the same outcome, so the
    /// workspace and the buffers change together. The tree reads only the
    /// directories that the mutation changed, and it selects the new entry.
    pub(super) fn apply_mutation(&mut self, outcome: &MutationOutcome) {
        self.pending = None;
        for directory in &outcome.changed {
            self.tree.refresh(directory);
        }
        if let Some(selection) = &outcome.selection {
            self.tree.reveal(selection);
        }
        self.pump();
        // A mutation adds, removes, or renames an entry, so the recorded state
        // of the workspace changed with it.
        self.request_git_status();
    }

    /// Reports that one workspace operation produced no result.
    ///
    /// The tree keeps its loaded state, so the user can repeat the operation.
    pub(super) fn abandon_request(&mut self) {
        self.pending = None;
        self.outbox = None;
        self.pump();
    }

    /// Selects one path and expands every directory above it.
    pub(super) fn reveal(&mut self, path: &Path) {
        self.tree.reveal(path);
        self.pump();
    }

    /// Moves the selection by one bounded row move.
    ///
    /// The move stops at the first and the last row, so it never wraps. A row
    /// that reports a bounded or a failed directory read carries no selection,
    /// so the sidebar takes the nearest selectable row in the direction of
    /// travel, and the nearest one behind it when the direction holds none. An
    /// empty tree keeps its empty selection.
    pub(super) fn move_selection(&mut self, motion: TreeMotion) {
        self.sync_rows();
        let motion = match motion {
            TreeMotion::Down(step) => ListMotion::Down(step),
            TreeMotion::Up(step) => ListMotion::Up(step),
            TreeMotion::ToRow(row) => ListMotion::ToRow(row),
            TreeMotion::LastRow => ListMotion::LastRow,
        };
        // An empty tree and a tree whose rows all report a read both leave the
        // selection where it was, so the reduction reports no event.
        let Some(SidebarEvent::SelectionChanged { row }) =
            self.view.reduce(&SidebarInput::Move(motion))
        else {
            return;
        };
        let Some(path) = self.tree.rows().get(row).map(|row| row.path.clone()) else {
            debug_assert!(false, "the selected row comes from the current rows");
            return;
        };
        self.tree.select(&path);
    }

    /// Moves the selection to the directory that holds the selected entry.
    pub(super) fn select_parent(&mut self) {
        self.tree.select_parent();
    }

    /// Expands the selected directory, or returns the selected file.
    ///
    /// `l` reaches this entry point. An expanded directory stays open, so the
    /// key only ever moves the reader deeper into the tree. The caller opens the
    /// returned path in the focused editor window, which is the nvim-tree and
    /// neo-tree rule for a file. See `docs/input-actions.md`.
    pub(super) fn expand_selected(&mut self) -> Option<PathBuf> {
        let row = self.tree.selected_row()?;
        match row.kind()? {
            EntryKind::File => Some(row.path.clone()),
            EntryKind::Directory => {
                let path = row.path.clone();
                self.tree.expand(&path);
                self.pump();
                None
            }
        }
    }

    /// Collapses the selected directory, or selects the parent directory.
    ///
    /// `h` reaches this entry point. An expanded directory closes, and every
    /// other row leaves for the directory that holds it. Two presses therefore
    /// take a file to its folder and then close that folder, which is the
    /// nvim-tree and neo-tree rule. See `docs/input-actions.md`.
    pub(super) fn collapse_selected(&mut self) {
        let expanded = self.tree.selected_row().is_some_and(|row| {
            matches!(
                row.content,
                RowContent::Directory {
                    expansion: Expansion::Expanded | Expansion::Pending,
                    ..
                }
            )
        });
        if !expanded {
            self.tree.select_parent();
            return;
        }
        let Some(path) = self.tree.selected().map(Path::to_path_buf) else {
            debug_assert!(false, "an expanded row is always the selected row");
            return;
        };
        self.tree.collapse(&path);
        self.pump();
    }

    /// Opens the selected directory, or closes it again.
    pub(super) fn toggle_selected(&mut self) {
        let Some(row) = self.tree.selected_row() else {
            return;
        };
        if row.kind() != Some(EntryKind::Directory) {
            return;
        }
        let path = row.path.clone();
        self.tree.toggle(&path);
        self.pump();
    }

    /// Returns the selected file, or expands the selected directory.
    ///
    /// The caller opens the returned path in the focused editor window.
    pub(super) fn open_selected(&mut self) -> Option<PathBuf> {
        let row = self.tree.selected_row()?;
        match row.kind()? {
            EntryKind::File => Some(row.path.clone()),
            EntryKind::Directory => {
                self.toggle_selected();
                None
            }
        }
    }

    /// Applies one coalesced burst of workspace filesystem changes.
    ///
    /// The sidebar reads only the directories that the burst named, so a change
    /// deep inside the workspace never rebuilds the tree. The expansion, the
    /// selection, and the first visible row all survive, because each named
    /// directory takes the ordinary refresh path.
    ///
    /// A burst that lost events names an incomplete set of directories, so the
    /// sidebar reads every expanded directory again instead of trusting it.
    ///
    /// The rows change when the reads return, so the burst itself paints
    /// nothing. See `docs/files.md`.
    pub(super) fn apply_watch(&mut self, batch: &WatchBatch) {
        match batch.fidelity() {
            WatchFidelity::Complete => {
                for directory in batch.directories() {
                    self.tree.refresh(&directory);
                }
            }
            WatchFidelity::Dropped => self.tree.refresh_all(),
        }
        self.pump();
        // Every change of the workspace can change the recorded state of the
        // repository, as a save and a mutation already do.
        self.request_git_status();
    }

    /// Asks for a new read of every expanded directory and of the repository.
    pub(super) fn refresh_all(&mut self) {
        self.tree.refresh_all();
        self.pump();
        self.request_git_status();
    }

    /// Shows the hidden entries, or hides them again.
    pub(super) fn toggle_hidden(&mut self) {
        self.tree.toggle_hidden();
    }

    /// Starts one search, or refines the query of the active one.
    ///
    /// The search keeps every row. It may open a directory that holds a match,
    /// and it may need the listing of a directory that the tree never read, so
    /// the sidebar queues the next read here as every other transition does.
    pub(super) fn start_search(&mut self, query: &str) {
        self.tree.start_search(query);
        self.pump();
    }

    /// Ends the active search and restores the expansion of the user.
    pub(super) fn end_search(&mut self) {
        self.tree.end_search();
    }

    /// Moves the selection to the next or the previous match.
    ///
    /// The move wraps at the first and the last match, as `n` and `N` wrap in
    /// a buffer window.
    pub(super) fn select_match(&mut self, direction: SearchDirection) -> TreeMatchOutcome {
        let rows = self.tree.rows();
        let current = self.selected_index().unwrap_or(0);
        let found = match direction {
            SearchDirection::Forward => rows
                .iter()
                .skip(current.saturating_add(1))
                .find(|row| row.matched.is_some())
                .or_else(|| rows.iter().find(|row| row.matched.is_some())),
            SearchDirection::Backward => rows[..current.min(rows.len())]
                .iter()
                .rev()
                .find(|row| row.matched.is_some())
                .or_else(|| rows.iter().rev().find(|row| row.matched.is_some())),
        };
        let Some(row) = found else {
            return TreeMatchOutcome::Missed;
        };
        debug_assert!(
            row.is_selectable(),
            "a notice row carries no name, so it never holds a match"
        );
        let path = row.path.clone();
        self.tree.select(&path);
        TreeMatchOutcome::Moved
    }

    /// Holds the selected entry for the next paste.
    ///
    /// # Errors
    ///
    /// Returns [`TreeRefusal::NoSelection`] while the tree shows no entry.
    pub(super) fn hold(&mut self, mode: TransferMode) -> Result<PathBuf, TreeRefusal> {
        let path = self.selected_entry()?;
        let contained = self.contained(&path).ok_or(TreeRefusal::EntryGone)?;
        self.clipboard.hold(mode, vec![contained]);
        Ok(path)
    }

    /// Returns the paste of the held entries into the destination directory.
    ///
    /// # Errors
    ///
    /// Returns [`TreeRefusal::ClipboardEmpty`] while the clipboard holds no
    /// entry.
    pub(super) fn stage_paste(&self) -> Result<FileOperation, TreeRefusal> {
        let destination = self.destination_target()?;
        self.clipboard
            .paste(&destination)
            .ok_or(TreeRefusal::ClipboardEmpty)
    }

    /// Releases the entries that the file-operation clipboard holds.
    ///
    /// One completed workspace mutation and one cancel both reach this entry
    /// point, so the row of a held entry loses its marker as soon as the
    /// operation finishes or the user drops it. A completed paste therefore
    /// never moves the same entry twice. See `docs/files.md`.
    pub(super) fn release_hold(&mut self) {
        self.clipboard.clear();
    }

    /// Returns the entries that a removal of the selection destroys.
    ///
    /// The caller names these entries in its question, and it stages the
    /// removal of exactly these entries when the user confirms. See
    /// `docs/files.md`.
    ///
    /// # Errors
    ///
    /// Returns [`TreeRefusal::NoSelection`] while the tree shows no entry.
    pub(super) fn delete_selection(&self) -> Result<Vec<PathBuf>, TreeRefusal> {
        Ok(vec![self.selected_entry()?])
    }

    /// Returns the removal of the named entries.
    ///
    /// The tree reads its rows again here, because a watcher event can drop an
    /// entry while the question waits for its answer. The removal of an entry
    /// that the tree no longer shows therefore reaches no worker, and the
    /// answer never removes the entry that took its place.
    ///
    /// # Errors
    ///
    /// Returns [`TreeRefusal::EntryGone`] when the tree shows no row of one
    /// named entry.
    pub(super) fn stage_delete(&self, paths: Vec<PathBuf>) -> Result<FileOperation, TreeRefusal> {
        let rows = self.tree.rows();
        let mut contained = Vec::with_capacity(paths.len());
        for path in &paths {
            let shown = rows
                .iter()
                .any(|row| row.is_selectable() && row.path == *path);
            if !shown {
                return Err(TreeRefusal::EntryGone);
            }
            contained.push(self.contained(path).ok_or(TreeRefusal::EntryGone)?);
        }
        Ok(FileOperation::Delete { paths: contained })
    }

    /// Returns the creation of one entry inside the destination directory.
    ///
    /// # Errors
    ///
    /// Returns [`TreeRefusal`] for an empty name and for a name that holds a
    /// path component.
    pub(super) fn stage_create(
        &self,
        name: &str,
        kind: EntryKind,
    ) -> Result<FileOperation, TreeRefusal> {
        let name = check_name(name)?;
        let destination = self.destination_target()?;
        Ok(FileOperation::Create {
            path: contained_child(&destination, name)?,
            kind,
        })
    }

    /// Returns the rename of the selected entry.
    ///
    /// # Errors
    ///
    /// Returns [`TreeRefusal`] for an empty name, for a name that holds a path
    /// component, and while the tree shows no entry.
    pub(super) fn stage_rename(&self, name: &str) -> Result<FileOperation, TreeRefusal> {
        let name = check_name(name)?;
        let from = self.selected_entry()?;
        let from = self.contained(&from).ok_or(TreeRefusal::EntryGone)?;
        let parent = from
            .as_path()
            .parent()
            .map_or(WorktreeDirectoryPath::Root, |parent| {
                WorktreeRelativePath::new(parent)
                    .map_or(WorktreeDirectoryPath::Root, WorktreeDirectoryPath::Relative)
            });
        Ok(FileOperation::Rename {
            to: contained_child(&parent, name)?,
            from,
        })
    }

    /// Queues one validated mutation for the bounded worker service.
    ///
    /// The `overwrite` value names every destination that the user approved.
    /// [`Overwrite::Refuse`] destroys no entry that holds a destination.
    ///
    /// # Errors
    ///
    /// Returns [`TreeRefusal::Busy`] while another workspace operation runs, so
    /// no result can reach a tree state that a newer operation replaced.
    pub(super) fn start_mutation(
        &mut self,
        operation: FileOperation,
        overwrite: Overwrite,
        buffers: Vec<OpenBuffer>,
    ) -> Result<(), TreeRefusal> {
        if self.pending.is_some() || self.outbox.is_some() {
            return Err(TreeRefusal::Busy);
        }
        self.outbox = Some(WorkspaceRequest::Mutate(MutateRequest {
            operation: operation.clone(),
            root: Arc::clone(&self.root),
            buffers,
            overwrite,
        }));
        self.pending = Some(PendingWorkspace::Mutation { operation });
        Ok(())
    }

    /// Returns the operation of the mutation that the sidebar waits for.
    ///
    /// The editor reads it when the worker refuses a taken destination, so the
    /// question of the overwrite names the operation that the user asked for.
    pub(super) fn pending_mutation(&self) -> Option<FileOperation> {
        match self.pending.as_ref()? {
            PendingWorkspace::Read { .. } => None,
            PendingWorkspace::Mutation { operation } => Some(operation.clone()),
        }
    }

    /// Moves the visible rows so the selected row keeps the scroll margin.
    ///
    /// The caller owns the rectangle of the sidebar and the display settings,
    /// so it supplies the viewport of the entry rows and the margin. The
    /// margin rule itself belongs to [`Viewport::reconciled_first_row`], which
    /// the buffer windows read as well, so a window and the sidebar keep the
    /// same number of rows around the reader.
    ///
    /// A closed sidebar and an empty tree both show their rows from the first
    /// one.
    pub(super) fn reconcile(&mut self, viewport: Option<Viewport>, margin_rows: usize) {
        self.sync_rows();
        self.view
            .set_scroll_margin(u16::try_from(margin_rows).unwrap_or(u16::MAX));
        // The row state never scrolls past its last terminal row, so the
        // sidebar marks no row after its last entry and fills its region
        // instead of showing blank rows below that entry. A buffer window keeps
        // those rows and marks them.
        self.view
            .set_height_rows(viewport.map_or(0, |viewport| viewport.height_rows().get()));
    }

    /// Copies the current rows and the current selection into the row state.
    ///
    /// The renderer reads the tree row of one placement directly, because every
    /// row carries its own position as its identity.
    fn sync_rows(&mut self) {
        let rows = self.generic_rows();
        if let Err(error) = self.view.set_rows(rows) {
            debug_assert!(false, "the tree bounds hold every row: {error}");
            return;
        }
        // The tree owns the selection, so the row state follows it instead of
        // keeping a second answer.
        match self.selected_index() {
            Some(index) => {
                self.view.select(&index);
            }
            None => self.view.clear_selection(),
        }
    }

    /// Returns the generic rows of the current tree rows.
    ///
    /// Every row occupies one terminal row and carries its own position as its
    /// identity. A row that reports a bounded or a failed directory read is
    /// inert, so it takes no selection.
    ///
    /// `FileTree` withholds the children of a collapsed directory from
    /// [`FileTree::rows`] itself, so this list already holds the visible rows
    /// alone. The collapsed flag of the sidebar stays unset here on purpose:
    /// [`SidebarRow::with_collapsed`] would hide a row that `FileTree` never
    /// emitted, so it changes nothing, and one owner of the truth stays enough.
    /// See `docs/windows.md`.
    fn generic_rows(&self) -> Vec<SidebarRow<usize>> {
        self.tree
            .rows()
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let kind = if row.is_selectable() {
                    RowKind::Selectable
                } else {
                    RowKind::Inert
                };
                SidebarRow::single(index, kind).with_depth(row.depth)
            })
            .collect()
    }

    /// Returns the rows that one embedded host draws for this tree.
    ///
    /// The rows carry the text, the indent guides, the depth, the state, and
    /// the selection of the tree as it stands now. The call reads no
    /// filesystem: it copies loaded state alone, and a directory that holds no
    /// listing yet reports [`FileRowKind::LoadingDirectory`] until the host
    /// hands its read back. See `docs/embedding.md`.
    pub(super) fn host_rows(&self) -> Vec<FileRow> {
        let generic = self.generic_rows();
        debug_assert!(
            generic.len() <= FILE_SIDEBAR_ROWS_MAX,
            "the bounds of the file tree hold every row inside the sidebar bound"
        );
        (0..self.tree.rows().len())
            .filter_map(|index| self.host_row(&generic, index))
            .collect()
    }

    /// Returns one published row of this tree, or `None` outside the rows.
    ///
    /// Every drawn row of the file tree comes from this one builder, so the
    /// standalone editor and an embedded host can never disagree about what
    /// one row holds. The caller supplies the generic rows, because the indent
    /// guides read the depth of the neighbors of the row.
    pub(super) fn host_row(&self, generic: &[SidebarRow<usize>], index: usize) -> Option<FileRow> {
        let row = self.tree.rows().get(index)?;
        let label = match &row.content {
            RowContent::File { name, .. } | RowContent::Directory { name, .. } => name.clone(),
            RowContent::Notice(notice) => notice_text(notice),
        };
        // The shared rule starts a guide at depth 1, and the header row of the
        // workspace root is no sibling of the first entries, so the file tree
        // prepends one blank guide of its own. The host takes the complete
        // indent, so it reproduces that look without holding a second copy of
        // the rule. See `docs/windows.md`.
        let guides = format!("{SIDEBAR_GUIDE_BLANK}{}", sidebar_guides(generic, index));
        let selected = row.is_selectable() && self.tree.selected() == Some(row.path.as_path());
        // A notice row carries the path of its own directory, so it must not
        // borrow the Git state of that directory.
        let git = row
            .is_selectable()
            .then(|| self.git_state(&row.path))
            .flatten();
        let is_symlink = match &row.content {
            RowContent::File { link, .. } | RowContent::Directory { link, .. } => {
                *link == LinkKind::Symlink
            }
            RowContent::Notice(_) => false,
        };
        // The icon reaches the host regardless of the icon-visibility setting
        // of kvim's own file tree, because a host may draw its own icons while
        // kvim would draw none. The row painter reads the setting instead.
        let icon = row_icon(row, FileTreeIcons::Shown);
        let matched = row.matched.map(|matched| LabelMatch {
            start: matched.start,
            len: matched.len,
        });
        let state = RowState::of(row, self.held_mode(&row.path), git);
        Some(
            FileRow::new(
                label,
                guides,
                row.depth,
                host_row_kind(row),
                state,
                selected,
            )
            .with_git(git.map(host_git_state))
            .with_symlink(is_symlink)
            .with_icon(icon)
            .with_matched(matched),
        )
    }

    /// Applies one input of an embedded host to this tree.
    ///
    /// The reduction changes the selection and the expansion alone. It opens no
    /// buffer and reads no directory: an expansion queues the listing that
    /// [`TreeSidebar::take_request`] hands to the worker service.
    pub(super) fn reduce_host(&mut self, input: FileSidebarInput) -> FileSidebarOutcome {
        let activated = match input {
            FileSidebarInput::Move(motion) => {
                // `Parent` reaches the directory that holds the selected
                // entry through the same method the standalone editor's
                // `TreeSelectParent` command uses, so the embedded host and
                // the standalone editor climb through one call, not two.
                match motion {
                    ListMotion::Parent => self.select_parent(),
                    ListMotion::Down(step) => self.move_selection(TreeMotion::Down(step)),
                    ListMotion::Up(step) => self.move_selection(TreeMotion::Up(step)),
                    ListMotion::ToRow(row) => self.move_selection(TreeMotion::ToRow(row)),
                    ListMotion::LastRow => self.move_selection(TreeMotion::LastRow),
                }
                None
            }
            FileSidebarInput::Open => self.expand_selected(),
            FileSidebarInput::Close => {
                self.collapse_selected();
                None
            }
            FileSidebarInput::Activate => self.open_selected(),
        };
        // The tree names an absolute path. A path that names no contained entry
        // of the root belongs to no row, so it reaches no host either.
        match activated.and_then(|path| self.contained(&path)) {
            Some(path) => FileSidebarOutcome::Activated { path },
            None => FileSidebarOutcome::Applied,
        }
    }

    /// Returns the row index of the selection, or `None` while no row holds it.
    ///
    /// A notice row carries the path of its own directory, so the search keeps
    /// the selectable rows only. Without that filter one unreadable directory
    /// would hold two rows of one path.
    fn selected_index(&self) -> Option<usize> {
        let selected = self.tree.selected()?;
        self.tree
            .rows()
            .iter()
            .position(|row| row.is_selectable() && row.path == selected)
    }

    /// Returns the selected entry.
    fn selected_entry(&self) -> Result<PathBuf, TreeRefusal> {
        self.tree
            .selected_row()
            .filter(|row| row.is_selectable())
            .map(|row| row.path.clone())
            .ok_or(TreeRefusal::NoSelection)
    }

    /// Returns the name of the selected entry, for the rename prompt to seed.
    ///
    /// The rename prompt reads a bare entry name, not a path, so the seed
    /// carries the whole name, including any extension. The caller places the
    /// cursor at the end of the stem, before the extension, so a reader who
    /// edits the stem never moves the cursor first. The caller opens an empty
    /// prompt while no row is selected, and the submitted rename then reports
    /// [`TreeRefusal::NoSelection`], exactly as it did before the prompt
    /// seeded a name. See `docs/files.md`.
    pub(super) fn selected_entry_name(&self) -> Option<String> {
        self.selected_entry().ok().map(|path| entry_name(&path))
    }

    /// Queues the next directory read while no other operation runs.
    fn pump(&mut self) {
        if self.pending.is_some() || self.outbox.is_some() {
            return;
        }
        let Some(path) = self.tree.take_pending_read() else {
            return;
        };
        let target = self
            .directory_target(&path)
            .expect("the file tree queues only validated paths below its root");
        self.outbox = Some(WorkspaceRequest::ReadDirectory {
            root: Arc::clone(&self.root),
            path: target,
        });
        self.pending = Some(PendingWorkspace::Read { path });
    }

    /// Returns the contained path of one absolute path below the root.
    ///
    /// The tree shows one root, so a path that names no contained entry of it
    /// belongs to no row and reaches no capability call.
    fn contained(&self, path: &Path) -> Option<WorktreeRelativePath> {
        let relative = path.strip_prefix(self.root.as_path()).ok()?;
        WorktreeRelativePath::new(relative).ok()
    }

    /// Returns the contained directory target of one absolute path.
    fn directory_target(&self, path: &Path) -> Option<WorktreeDirectoryPath> {
        if path == self.root.as_path() {
            return Some(WorktreeDirectoryPath::Root);
        }
        self.contained(path).map(WorktreeDirectoryPath::Relative)
    }

    /// Returns the contained directory that receives a create or a paste.
    fn destination_target(&self) -> Result<WorktreeDirectoryPath, TreeRefusal> {
        self.directory_target(&self.tree.destination_directory())
            .ok_or(TreeRefusal::EntryGone)
    }
}

/// Returns the contained path of one entry name inside a directory.
fn contained_child(
    directory: &WorktreeDirectoryPath,
    name: &str,
) -> Result<WorktreeRelativePath, TreeRefusal> {
    let base = directory
        .relative_path()
        .map_or_else(PathBuf::new, |path| path.as_path().to_path_buf());
    WorktreeRelativePath::new(base.join(name)).map_err(|_| TreeRefusal::EntryGone)
}

/// Returns the entry name that a prompt line holds.
///
/// The name must be one entry name, so the mutation stays inside the
/// destination directory that the tree selected.
fn check_name(name: &str) -> Result<&str, TreeRefusal> {
    let name = name.trim();
    if name.is_empty() {
        return Err(TreeRefusal::EmptyName);
    }
    if name.len() > TREE_NAME_BYTES_MAX {
        return Err(TreeRefusal::NameTooLong);
    }
    let one_component = Path::new(name)
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == name);
    if !one_component {
        return Err(TreeRefusal::NameHasPath);
    }
    Ok(name)
}

/// Returns the question that a removal of the named entries asks.
///
/// One entry appears by its name, which is the name that its row shows. Several
/// entries appear as a count, so the user knows the size of the removal before
/// the answer. The question holds no answer hint, because the message line adds
/// one. See `docs/files.md`.
pub(super) fn delete_question(paths: &[PathBuf]) -> String {
    let [path] = paths else {
        return format!("Delete {} entries", paths.len());
    };
    format!("Delete {}", entry_name(path))
}

/// Returns the question that an overwrite of the named destinations asks.
///
/// The text follows [`delete_question`] above, because both questions name the
/// entries that the action destroys. One transfer can take several
/// destinations, so the count keeps the size of the action visible. See
/// `docs/files.md`.
pub(super) fn overwrite_question(destinations: &[TakenDestination]) -> String {
    let [destination] = destinations else {
        return format!("Overwrite {} entries", destinations.len());
    };
    format!("Overwrite {}", entry_name(destination.path.as_path()))
}

/// Returns the display name of one entry: the file name, without a directory.
///
/// Every selectable row carries an entry name, so the complete path only
/// answers for a root that holds no row. A question over the complete path
/// would push its answer hint off the message line, and a seeded rename would
/// resolve against the wrong directory, so both callers need the bare name.
fn entry_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Returns the workspace root as the header row shows it.
///
/// The header shortens the home directory to `~`, as the reference shell and
/// the reference editor configuration do. A root outside the home directory,
/// and a session without one, keep the complete path.
pub(super) fn root_label(root: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return root.display().to_string();
    };
    match root.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => root.display().to_string(),
    }
}

/// Returns what one tree row shows to an embedded host.
///
/// The state of a directory reaches the host as one value, so a host draws one
/// row from one match and never combines two flags. See `docs/embedding.md`.
fn host_row_kind(row: &TreeRow) -> FileRowKind {
    match &row.content {
        RowContent::File { .. } => FileRowKind::File,
        RowContent::Directory { expansion, .. } => match expansion {
            Expansion::Collapsed => FileRowKind::ClosedDirectory,
            Expansion::Expanded => FileRowKind::OpenDirectory,
            Expansion::Pending => FileRowKind::LoadingDirectory,
        },
        RowContent::Notice(_) => FileRowKind::Note,
    }
}

/// Returns what one recorded Git state reaches an embedded host as.
///
/// The variant order mirrors [`GitStatus`], so the two never disagree about
/// the severity of one state. This is the one conversion between the two
/// vocabularies: the theme, the marks, and the published rows all name
/// [`FileRowGit`] from here on.
pub(super) const fn host_git_state(status: GitStatus) -> FileRowGit {
    match status {
        GitStatus::Ignored => FileRowGit::Ignored,
        GitStatus::Untracked => FileRowGit::Untracked,
        GitStatus::Staged => FileRowGit::Staged,
        GitStatus::Modified => FileRowGit::Modified,
        GitStatus::StagedAndModified => FileRowGit::StagedAndModified,
        GitStatus::Conflicted => FileRowGit::Conflicted,
    }
}

/// Returns the report that one notice row shows about its directory.
///
/// The renderer of the sidebar and the host surface of `file_sidebar.rs` both
/// read this rule, so a host that draws the rows itself reports a bounded read,
/// a failed read, and a hidden entry with the words that kvim shows.
fn notice_text(notice: &Notice) -> String {
    match notice {
        Notice::Truncated { shown, total } => format!("… {shown} of {total} entries"),
        Notice::Unreadable => "… unreadable".to_owned(),
        Notice::Hidden { count } => {
            let items = if *count == 1 { "item" } else { "items" };
            format!("({count} hidden {items})")
        }
    }
}

/// Renders the file-tree sidebar into its layout rectangle.
///
/// The header row names the workspace root, and it carries the focus color, so
/// the reader sees which region owns the keys. Every further row shows one
/// entry or one notice of the tree. The sidebar leaves every row below the last
/// entry blank, because the end-of-buffer marker belongs to a buffer window.
///
/// The function returns the cell of the selected row, so the terminal draws its
/// own cursor there while the sidebar holds the focus. The cell is the first
/// cell of the label of that row, not the mark cell, because a block cursor
/// inverts the cell that it stands on and would light the wrong half of the
/// selection mark. See `docs/windows.md`.
pub(super) fn render_tree(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    sidebar: &TreeSidebar,
    focus: RegionFocus,
    icons: FileTreeIcons,
) -> Option<Position> {
    if area.is_empty() {
        return None;
    }
    target.set_style(area, theme.style(ThemeRole::Text));
    render_header(target, area, theme, sidebar, focus, icons);
    let body = area.height.checked_sub(TREE_TITLE_ROWS).map(|height| {
        Rect::new(
            area.x,
            area.y.saturating_add(TREE_TITLE_ROWS),
            area.width,
            height,
        )
    })?;

    let mut cursor = None;
    // The standalone tree draws through the published painter, so the look of
    // this sidebar and the look that an embedded host reaches are one thing.
    // Two copies of one appearance are free to drift, which is the failure
    // that the two indent-guide copies already cost once.
    let outcome = sidebar.view().render(target, body, |canvas, placement| {
        let Some(row) = sidebar.host_row(sidebar.view().rows(), placement.index()) else {
            debug_assert!(false, "the row state follows the rows of the tree");
            return;
        };
        if row.is_selected() {
            cursor = Some(selected_cell(canvas.area(), row.depth()));
        }
        draw_file_row(canvas, &row, theme, icons, focus);
    });
    debug_assert!(
        outcome.is_ok(),
        "every sidebar row stays inside the bounds of the canvas"
    );
    cursor
}

/// Returns the cell that the terminal cursor takes on one selected row.
///
/// The cursor stands on the first cell of the label, where the content of the
/// row begins. The mark cell keeps its own glyph that way, because a block
/// cursor inverts the cell below it. A deep row of a narrow sidebar keeps the
/// last cell of its own rectangle, so the cursor never leaves the sidebar.
fn selected_cell(row: Rect, depth: usize) -> Position {
    let offset = u16::try_from(label_offset_cells(depth)).unwrap_or(u16::MAX);
    let last = row.x.saturating_add(row.width.saturating_sub(1));
    let column = row.x.saturating_add(offset).min(last);
    Position::new(column, row.y)
}

/// Returns the width of the selection mark, in cells of the canvas.
pub(super) fn mark_cells() -> u16 {
    u16::try_from(MARK_CELLS).unwrap_or(1)
}

/// Paints one span of a sidebar row and clips it at the right edge.
///
/// A span that starts outside the sidebar paints nothing, and a span that
/// reaches the edge stops there, so a narrow sidebar writes no cell outside its
/// own rectangle.
pub(super) fn paint_span(canvas: &mut SidebarCanvas<'_>, start: usize, cells: usize, style: Style) {
    let (Ok(start), Ok(cells)) = (u16::try_from(start), u16::try_from(cells)) else {
        debug_assert!(
            false,
            "the tree depth and the query both stay inside their bounds"
        );
        return;
    };
    if start >= canvas.width_cells() {
        return;
    }
    canvas.style_span(0, start, cells, style);
}

/// Paints the Git mark of one row at the right edge of the sidebar.
///
/// The changes panel of the review records a [`GitStatus`], so this call
/// converts once and hands the drawing to the one published painter of a Git
/// mark. See `docs/git.md`.
pub(super) fn render_git_mark(
    canvas: &mut SidebarCanvas<'_>,
    status: GitStatus,
    theme: Theme,
    style: Style,
) {
    draw_git_mark(canvas, host_git_state(status), theme, style);
}

/// Renders the header row of the sidebar.
///
/// The header holds the workspace root path with the home directory shortened
/// to `~`, behind an open-directory glyph. It takes the same mark cell and the
/// same glyph cells as an entry row, so the root reads as the level above the
/// first entry. An unfocused sidebar mutes the header, so the reader sees which
/// region owns the keys.
fn render_header(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    sidebar: &TreeSidebar,
    focus: RegionFocus,
    icons: FileTreeIcons,
) {
    let role = match focus {
        RegionFocus::Focused => ThemeRole::TreeRoot,
        RegionFocus::Unfocused => ThemeRole::TitleMuted,
    };
    let glyph = match icons {
        FileTreeIcons::Hidden => EXPANDED_MARKER.to_owned(),
        FileTreeIcons::Shown => format!("{} ", directory_icon(Expansion::Expanded).glyph),
    };
    let mark = " ".repeat(MARK_CELLS);
    let header = format!("{mark}{glyph}{}", sidebar.root_label());
    let band = Rect::new(area.x, area.y, area.width, TREE_TITLE_ROWS);
    target.set_style(band, theme.style(ThemeRole::Winbar));
    target.set_stringn(
        area.x,
        area.y,
        &header,
        usize::from(area.width),
        theme.style(role),
    );
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
