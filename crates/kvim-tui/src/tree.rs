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

use std::env;
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use kvim_editor::{SearchDirection, Viewport};
use kvim_settings::FileTreeIcons;
use kvim_workspace::{
    DirectoryListing, EntryKind, Expansion, FileClipboard, FileOperation, FileTree, LinkKind,
    MutateRequest, MutationOutcome, Notice, OpenBuffer, ReadError, RowContent, TransferMode,
    TreeRow, WorkspaceRequest,
};

use super::buffer_view::WindowFocus;
use super::icons::{ICON_CELLS, directory_icon, row_icon};
use super::theme::{Theme, ThemeRole};

/// The number of cells that one tree level indents.
pub(super) const TREE_INDENT_CELLS: usize = 2;

/// The number of rows that the sidebar title occupies.
pub(super) const TREE_TITLE_ROWS: u16 = 1;

/// The largest number of characters that one entry name accepts.
///
/// The bound protects the prompt line against a name that no filesystem
/// accepts. Every common filesystem stops at 255 bytes for one name.
pub(super) const TREE_NAME_CHARS_MAX: usize = 128;

/// The marker of one expanded directory row, while the tree hides its icons.
const EXPANDED_MARKER: &str = "▾ ";

/// The marker of one collapsed directory row, while the tree hides its icons.
const COLLAPSED_MARKER: &str = "▸ ";

/// The number of cells that the selection mark reserves at the left edge.
///
/// The column stays blank on every other row, so one mark never moves a name.
const MARK_CELLS: usize = 1;

/// The mark of the selected row, at the left edge of the sidebar.
const SELECTION_MARK: &str = "▌";

/// The indent guide of one level that holds a further entry below the row.
const GUIDE_TRUNK: &str = "│ ";

/// The indent guide that closes the last child of one level.
const GUIDE_ELBOW: &str = "└ ";

/// The indent guide of one level that holds no further entry.
const GUIDE_BLANK: &str = "  ";

/// The suffix of one symbolic link.
const LINK_SUFFIX: &str = "@";

/// The entry names whose content one tool generates.
///
/// The tree dims these entries, because they hold machine output instead of
/// work of the user. The list is presentation data beside the icon table, and
/// it names a small fixed set, so one lookup costs a bounded number of
/// comparisons. See `docs/files.md`.
const GENERATED_NAMES: [&str; 5] = [".direnv", ".git", "__pycache__", "node_modules", "target"];

/// What one sidebar row shows, beyond the name that it holds.
///
/// The value decides the style of the row and the suffix behind the name. A
/// row takes exactly one state, so no two of them can disagree, and a held
/// entry carries the mode of the file operation instead of two flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowState {
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
    /// operation is the report that the reader waits for.
    fn of(row: &TreeRow, held: Option<TransferMode>) -> Self {
        if let Some(mode) = held {
            return Self::Held(mode);
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
    const fn suffix(self) -> &'static str {
        match self {
            Self::Held(TransferMode::Move) => " (cut)",
            Self::Held(TransferMode::Copy) => " (copied)",
            Self::Directory | Self::File | Self::Generated | Self::Omitted | Self::Incomplete => "",
        }
    }

    /// Returns the role that colors the row.
    const fn role(self) -> ThemeRole {
        match self {
            Self::Directory => ThemeRole::TreeDirectory,
            Self::File => ThemeRole::Text,
            Self::Generated | Self::Held(_) => ThemeRole::TreeMuted,
            Self::Omitted => ThemeRole::TreeNotice,
            Self::Incomplete => ThemeRole::TreeIncomplete,
        }
    }
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
    Mutation,
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
    /// The name holds more characters than the bound allows.
    NameTooLong,
    /// The file-operation clipboard holds no entry.
    ClipboardEmpty,
    /// One workspace operation is already running.
    Busy,
}

impl TreeRefusal {
    /// Returns the message that the message line shows.
    pub(super) const fn message(self) -> &'static str {
        match self {
            Self::NoSelection => "the file tree shows no selected entry",
            Self::EmptyName => "the name is empty",
            Self::NameHasPath => "the name must hold one entry name, not a path",
            Self::NameTooLong => "the name is too long",
            Self::ClipboardEmpty => "the file clipboard holds no entry",
            Self::Busy => "one workspace operation is already running",
        }
    }
}

/// The file-tree sidebar of one editor.
#[derive(Debug)]
pub(super) struct TreeSidebar {
    tree: FileTree,
    clipboard: FileClipboard,
    /// The first visible row. The reconciliation keeps the selection visible.
    first_row: usize,
    /// The operation that waits for the bounded worker service.
    outbox: Option<WorkspaceRequest>,
    /// The operation that the sidebar waits for.
    pending: Option<PendingWorkspace>,
    /// The home directory of the user, while the environment names one.
    ///
    /// The header shortens the root path against this value. The sidebar reads
    /// the environment once, at construction, so the render path stays free of
    /// every ambient read.
    home: Option<PathBuf>,
}

impl TreeSidebar {
    /// Creates one sidebar over a workspace root and asks for its first read.
    pub(super) fn new(root: PathBuf) -> Self {
        let mut sidebar = Self {
            tree: FileTree::new(root),
            clipboard: FileClipboard::default(),
            first_row: 0,
            outbox: None,
            pending: None,
            home: env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
        };
        sidebar.pump();
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
        self.clipboard
            .paths()
            .iter()
            .any(|held| held == path)
            .then_some(mode)
    }

    /// Returns the first visible row.
    pub(super) const fn first_row(&self) -> usize {
        self.first_row
    }

    /// Returns the workspace request that the event loop must submit.
    pub(super) fn take_request(&mut self) -> Option<WorkspaceRequest> {
        self.outbox.take()
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
        let rows = self.tree.rows();
        let Some(last) = rows.len().checked_sub(1) else {
            return;
        };
        let current = self.selected_index().unwrap_or(0);
        let (target, forward) = match motion {
            TreeMotion::Down(step) => (current.saturating_add(step).min(last), true),
            TreeMotion::Up(step) => (current.saturating_sub(step), false),
            TreeMotion::ToRow(row) => (row.min(last), true),
            TreeMotion::LastRow => (last, false),
        };
        let ahead = rows[target..].iter().find(|row| row.is_selectable());
        let behind = rows[..=target].iter().rev().find(|row| row.is_selectable());
        let found = if forward {
            ahead.or(behind)
        } else {
            behind.or(ahead)
        };
        let Some(path) = found.map(|row| row.path.clone()) else {
            // Every row reports a read instead of an entry, so nothing accepts
            // the selection.
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

    /// Asks for a new read of every expanded directory.
    pub(super) fn refresh_all(&mut self) {
        self.tree.refresh_all();
        self.pump();
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
        self.clipboard.hold(mode, vec![path.clone()]);
        Ok(path)
    }

    /// Returns the paste of the held entries into the destination directory.
    ///
    /// # Errors
    ///
    /// Returns [`TreeRefusal::ClipboardEmpty`] while the clipboard holds no
    /// entry.
    pub(super) fn stage_paste(&self) -> Result<FileOperation, TreeRefusal> {
        self.clipboard
            .paste(&self.tree.destination_directory())
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

    /// Returns the removal of the selected entry.
    ///
    /// # Errors
    ///
    /// Returns [`TreeRefusal::NoSelection`] while the tree shows no entry.
    pub(super) fn stage_delete(&self) -> Result<FileOperation, TreeRefusal> {
        Ok(FileOperation::Delete {
            paths: vec![self.selected_entry()?],
        })
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
        Ok(FileOperation::Create {
            path: self.tree.destination_directory().join(name),
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
        let parent = from
            .parent()
            .map_or_else(|| self.tree.root().to_path_buf(), Path::to_path_buf);
        Ok(FileOperation::Rename {
            from,
            to: parent.join(name),
        })
    }

    /// Queues one validated mutation for the bounded worker service.
    ///
    /// # Errors
    ///
    /// Returns [`TreeRefusal::Busy`] while another workspace operation runs, so
    /// no result can reach a tree state that a newer operation replaced.
    pub(super) fn start_mutation(
        &mut self,
        operation: FileOperation,
        buffers: Vec<OpenBuffer>,
    ) -> Result<(), TreeRefusal> {
        if self.pending.is_some() || self.outbox.is_some() {
            return Err(TreeRefusal::Busy);
        }
        self.outbox = Some(WorkspaceRequest::Mutate(MutateRequest {
            operation,
            root: self.tree.root().to_path_buf(),
            buffers,
        }));
        self.pending = Some(PendingWorkspace::Mutation);
        Ok(())
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
        let (Some(viewport), Some(last_row)) = (viewport, self.tree.rows().len().checked_sub(1))
        else {
            self.first_row = 0;
            return;
        };
        let rows_visible = usize::from(viewport.height_rows().get());
        let selected = self.selected_index().unwrap_or(0);
        // The sidebar marks no row after its last entry, so it also fills its
        // region instead of showing blank rows below the last one. A buffer
        // window keeps those rows and marks them.
        let last_start = (last_row + 1).saturating_sub(rows_visible);
        self.first_row = viewport
            .reconciled_first_row(self.first_row, selected, last_row, margin_rows)
            .min(last_start);
        debug_assert!(
            self.first_row <= selected && selected < self.first_row + rows_visible,
            "the reconciled offset always keeps the selected row visible"
        );
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

    /// Queues the next directory read while no other operation runs.
    fn pump(&mut self) {
        if self.pending.is_some() || self.outbox.is_some() {
            return;
        }
        let Some(path) = self.tree.take_pending_read() else {
            return;
        };
        self.outbox = Some(WorkspaceRequest::ReadDirectory { path: path.clone() });
        self.pending = Some(PendingWorkspace::Read { path });
    }
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
    if name.chars().count() > TREE_NAME_CHARS_MAX {
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

/// Reports whether one further row of `depth` follows the row at `index`.
///
/// The scan stops at the first shallower row, which closes the level, so the
/// answer covers the siblings of one directory alone.
fn level_continues(rows: &[TreeRow], index: usize, depth: usize) -> bool {
    rows.get(index.saturating_add(1)..)
        .unwrap_or_default()
        .iter()
        .take_while(|row| row.depth >= depth)
        .any(|row| row.depth == depth)
}

/// Returns the indent guides of one visible row.
///
/// One level that holds a further entry below the row draws a trunk, and the
/// last child of a level closes it with an elbow. Every guide is one
/// box-drawing character of one terminal cell, so a level always costs
/// [`TREE_INDENT_CELLS`] cells.
fn row_guides(rows: &[TreeRow], index: usize) -> String {
    let Some(depth) = rows.get(index).map(|row| row.depth) else {
        debug_assert!(
            false,
            "the renderer only reads the rows that the tree holds"
        );
        return String::new();
    };
    let mut guides = String::with_capacity(depth.saturating_add(1) * TREE_INDENT_CELLS);
    for level in 0..=depth {
        // The entries of the workspace root need no guide. The header row above
        // them is no sibling, so no guide would ever close.
        let segment = if level == 0 {
            GUIDE_BLANK
        } else if level_continues(rows, index, level) {
            GUIDE_TRUNK
        } else if level == depth {
            GUIDE_ELBOW
        } else {
            GUIDE_BLANK
        };
        guides.push_str(segment);
    }
    guides
}

/// Returns the two cells that sit between the guides and the name.
///
/// The cells hold the icon of the entry. Without a patched font the expansion
/// marker of a directory takes the same cells, so the state of a directory
/// stays visible and the names keep one column in both icon settings.
fn row_glyph(row: &TreeRow, icons: FileTreeIcons) -> String {
    if let Some(icon) = row_icon(row, icons) {
        return format!("{} ", icon.glyph);
    }
    match (&row.content, icons) {
        (RowContent::Directory { expansion, .. }, FileTreeIcons::Hidden) => match expansion {
            Expansion::Collapsed => COLLAPSED_MARKER,
            Expansion::Expanded | Expansion::Pending => EXPANDED_MARKER,
        }
        .to_owned(),
        _ => " ".repeat(ICON_CELLS),
    }
}

/// Returns the text of one tree row, without the selection style.
///
/// The row holds the blank mark cell, the indent guides of its levels, the
/// glyph cells, the name, and the suffix of the row state. A notice row reports
/// about the directory instead of naming an entry, so it carries no icon and
/// keeps the glyph cells blank.
fn row_text(row: &TreeRow, guides: &str, state: RowState, icons: FileTreeIcons) -> String {
    let mark = " ".repeat(MARK_CELLS);
    let glyph = row_glyph(row, icons);
    let held = state.suffix();
    match &row.content {
        RowContent::File { name, link } | RowContent::Directory { name, link, .. } => {
            let link = if *link == LinkKind::Symlink {
                LINK_SUFFIX
            } else {
                ""
            };
            format!("{mark}{guides}{glyph}{name}{link}{held}")
        }
        RowContent::Notice(Notice::Truncated { shown, total }) => {
            format!("{mark}{guides}{glyph}… {shown} of {total} entries")
        }
        RowContent::Notice(Notice::Unreadable) => {
            format!("{mark}{guides}{glyph}… unreadable")
        }
        RowContent::Notice(Notice::Hidden { count }) => {
            let items = if *count == 1 { "item" } else { "items" };
            format!("{mark}{guides}{glyph}({count} hidden {items})")
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
/// own cursor there while the sidebar holds the focus.
pub(super) fn render_tree(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    sidebar: &TreeSidebar,
    focus: WindowFocus,
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
    let rows = sidebar.tree().rows();
    let selected = sidebar.tree().selected();
    let width = usize::from(body.width);
    for (offset, index) in (sidebar.first_row()..rows.len())
        .take(usize::from(body.height))
        .enumerate()
    {
        let Ok(offset) = u16::try_from(offset) else {
            debug_assert!(false, "the visible rows never pass the terminal height");
            return cursor;
        };
        let row = &rows[index];
        let y = body.y + offset;
        let state = RowState::of(row, sidebar.held_mode(&row.path));
        let style = theme
            .style(ThemeRole::Text)
            .patch(theme.style(state.role()));
        let current = row.is_selectable() && selected == Some(row.path.as_path());
        let style = if current {
            cursor = Some(Position::new(body.x, y));
            style.patch(theme.style(ThemeRole::PopupSelection))
        } else {
            style
        };
        // The selection covers the complete row, so the reader finds it at any
        // indent depth.
        target.set_style(Rect::new(body.x, y, body.width, 1), style);
        let guides = row_guides(rows, index);
        target.set_stringn(
            body.x,
            y,
            row_text(row, &guides, state, icons),
            width,
            style,
        );
        // The guides carry their own color, so they separate from the names
        // without the state of the row changing their meaning.
        paint_span(
            target,
            body,
            y,
            MARK_CELLS,
            guides.chars().count(),
            style.patch(theme.style(ThemeRole::TreeIndentGuide)),
        );
        if current {
            target.set_stringn(
                body.x,
                y,
                SELECTION_MARK,
                MARK_CELLS,
                style.patch(theme.style(ThemeRole::TreeSelectionMark)),
            );
        }
        // The icon carries its own color over the row style, so a selected row
        // keeps its background behind the glyph.
        render_row_icon(target, body, y, row, icons, theme, style);
        // The search marks every match. The selected row carries the match that
        // `n` and `N` moved to, so it reads as the current one, exactly as the
        // match under the cursor does in a buffer window. The mark wins over
        // every dimmed style, so a match inside a held or generated entry stays
        // readable as one match.
        if let Some(matched) = row.matched {
            let role = if current {
                ThemeRole::CurrentSearchMatch
            } else {
                ThemeRole::SearchMatch
            };
            paint_span(
                target,
                body,
                y,
                name_offset_cells(row.depth).saturating_add(matched.start),
                matched.len,
                style.patch(theme.style(role)),
            );
        }
    }
    cursor
}

/// Returns the cell column of the glyph cells inside one sidebar row.
///
/// The glyph follows the mark cell and the indent guides of every level, which
/// each cost [`TREE_INDENT_CELLS`] cells. The workspace root is one level above
/// the first entry, so a row of depth zero already carries one guide.
const fn glyph_offset_cells(depth: usize) -> usize {
    MARK_CELLS + TREE_INDENT_CELLS * (depth + 1)
}

/// Returns the cell column of the entry name inside one sidebar row.
///
/// Both icon settings reserve the same glyph cells, so the name of one depth
/// always starts at one column.
const fn name_offset_cells(depth: usize) -> usize {
    glyph_offset_cells(depth) + ICON_CELLS
}

/// Paints one span of a sidebar row and clips it at the right edge.
///
/// A span that starts outside the sidebar paints nothing, and a span that
/// reaches the edge stops there, so a narrow sidebar writes no cell outside its
/// own rectangle.
fn paint_span(
    target: &mut CellBuffer,
    body: Rect,
    y: u16,
    start: usize,
    cells: usize,
    style: Style,
) {
    let (Ok(start), Ok(cells)) = (u16::try_from(start), u16::try_from(cells)) else {
        debug_assert!(
            false,
            "the tree depth and the query both stay inside their bounds"
        );
        return;
    };
    if start >= body.width {
        return;
    }
    let width = cells.min(body.width - start);
    target.set_style(Rect::new(body.x + start, y, width, 1), style);
}

/// Paints the icon cell of one row with the color of its role.
///
/// The icon sits behind the mark cell and the indent guides, so its column
/// follows the depth of the row. A row whose icon falls outside the sidebar
/// keeps the clipped text that the row already wrote.
fn render_row_icon(
    target: &mut CellBuffer,
    body: Rect,
    y: u16,
    row: &TreeRow,
    icons: FileTreeIcons,
    theme: Theme,
    style: Style,
) {
    let Some(icon) = row_icon(row, icons) else {
        return;
    };
    let Ok(offset) = u16::try_from(glyph_offset_cells(row.depth)) else {
        debug_assert!(false, "the tree depth stays inside TREE_DEPTH_MAX");
        return;
    };
    if offset >= body.width {
        return;
    }
    target.set_stringn(
        body.x.saturating_add(offset),
        y,
        icon.glyph,
        ICON_CELLS,
        style.patch(theme.style(ThemeRole::Icon(icon.role))),
    );
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
    focus: WindowFocus,
    icons: FileTreeIcons,
) {
    let role = match focus {
        WindowFocus::Focused => ThemeRole::TreeRoot,
        WindowFocus::Unfocused => ThemeRole::TitleMuted,
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
mod tests {
    use std::path::PathBuf;

    use kvim_workspace::{Notice, RowContent, TreeRow};

    use super::{RowState, ThemeRole};

    /// Returns one notice row of the workspace root.
    fn notice_row(notice: Notice) -> TreeRow {
        TreeRow {
            path: PathBuf::from("/workspace"),
            depth: 0,
            content: RowContent::Notice(notice),
            matched: None,
        }
    }

    #[test]
    fn a_truncated_listing_never_reads_like_a_count_of_hidden_entries() {
        // A truncated listing keeps entries out that the reader expects, so it
        // warns. A hidden count reports a choice of the reader instead. The
        // entry bound of the tree is far above a practical test workspace, so
        // the row builder answers this question directly.
        let truncated = RowState::of(
            &notice_row(Notice::Truncated {
                shown: 8192,
                total: 9000,
            }),
            None,
        );
        let counted = RowState::of(&notice_row(Notice::Hidden { count: 5 }), None);
        assert_eq!(truncated.role(), ThemeRole::TreeIncomplete);
        assert_eq!(counted.role(), ThemeRole::TreeNotice);
        assert_eq!(
            RowState::of(&notice_row(Notice::Unreadable), None).role(),
            ThemeRole::TreeIncomplete,
            "a failed read warns like a truncated one"
        );
    }
}
