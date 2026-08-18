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

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use kvim_editor::Viewport;
use kvim_settings::FileTreeIcons;
use kvim_workspace::{
    DirectoryListing, EntryKind, Expansion, FileClipboard, FileOperation, FileTree, LinkKind,
    MutateRequest, MutationOutcome, Notice, OpenBuffer, ReadError, RowContent, TransferMode,
    TreeRow, WorkspaceRequest,
};

use super::buffer_view::WindowFocus;
use super::icons::{ICON_CELLS, row_icon};
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

/// The marker of one expanded directory row.
const EXPANDED_MARKER: &str = "▾ ";

/// The marker of one collapsed directory row.
const COLLAPSED_MARKER: &str = "▸ ";

/// The marker of one row that holds no directory.
const FILE_MARKER: &str = "  ";

/// The number of cells that one expansion marker occupies.
const MARKER_CELLS: usize = 2;

/// The suffix of one symbolic link.
const LINK_SUFFIX: &str = "@";

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
        };
        sidebar.pump();
        sidebar
    }

    /// Returns the tree model.
    pub(super) const fn tree(&self) -> &FileTree {
        &self.tree
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

    /// Narrows the visible rows to the names that hold the query.
    pub(super) fn set_query(&mut self, query: &str) {
        self.tree.set_query(query);
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

    /// Reports whether one completed paste consumed the held entries.
    ///
    /// A move paste clears the clipboard, so one cut never moves the same entry
    /// twice. See `docs/files.md`.
    pub(super) fn clear_moved_clipboard(&mut self) {
        if self.clipboard.mode() == Some(TransferMode::Move) {
            self.clipboard.clear();
        }
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

/// Returns the text of one tree row, without the selection style.
///
/// The row holds the indent of its depth, the expansion marker of a directory,
/// the icon, and the name. A notice row reports a bounded or a failed directory
/// read instead of an entry, so it carries no icon and keeps the reserved cells
/// blank. Every row of one depth therefore starts its name at one column, with
/// icons and without them.
pub(super) fn row_text(row: &TreeRow, icons: FileTreeIcons) -> String {
    let indent = " ".repeat(row.depth * TREE_INDENT_CELLS);
    let icon = match icons {
        FileTreeIcons::Hidden => String::new(),
        FileTreeIcons::Shown => match row_icon(row, icons) {
            Some(icon) => format!("{} ", icon.glyph),
            None => " ".repeat(ICON_CELLS),
        },
    };
    match &row.content {
        RowContent::File { name, link } => {
            let suffix = if *link == LinkKind::Symlink {
                LINK_SUFFIX
            } else {
                ""
            };
            format!("{indent}{FILE_MARKER}{icon}{name}{suffix}")
        }
        RowContent::Directory {
            name,
            link,
            expansion,
        } => {
            let marker = match expansion {
                Expansion::Collapsed => COLLAPSED_MARKER,
                Expansion::Expanded | Expansion::Pending => EXPANDED_MARKER,
            };
            let suffix = if *link == LinkKind::Symlink {
                LINK_SUFFIX
            } else {
                ""
            };
            format!("{indent}{marker}{icon}{name}{suffix}")
        }
        RowContent::Notice(Notice::Truncated { shown, total }) => {
            format!("{indent}{FILE_MARKER}{icon}… {shown} of {total} entries")
        }
        RowContent::Notice(Notice::Unreadable) => {
            format!("{indent}{FILE_MARKER}{icon}… unreadable")
        }
    }
}

/// Renders the file-tree sidebar into its layout rectangle.
///
/// The title row names the workspace root, and it carries the focus color, so
/// the reader sees which region owns the keys. Every further row shows one
/// entry or one notice of the tree.
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
    render_title(target, area, theme, sidebar.tree().root(), focus);
    let body = area.height.checked_sub(TREE_TITLE_ROWS).map(|height| {
        Rect::new(
            area.x,
            area.y.saturating_add(TREE_TITLE_ROWS),
            area.width,
            height,
        )
    })?;

    let mut cursor = None;
    let selected = sidebar.tree().selected();
    let width = usize::from(body.width);
    for (offset, row) in sidebar
        .tree()
        .rows()
        .iter()
        .skip(sidebar.first_row())
        .take(usize::from(body.height))
        .enumerate()
    {
        let Ok(offset) = u16::try_from(offset) else {
            debug_assert!(false, "the visible rows never pass the terminal height");
            return cursor;
        };
        let y = body.y + offset;
        let style = match &row.content {
            // A notice reports a bounded or a failed read, so it reads as a
            // warning instead of an entry.
            RowContent::Notice(_) => theme
                .style(ThemeRole::Text)
                .patch(theme.style(ThemeRole::Warning)),
            RowContent::Directory { .. } => theme
                .style(ThemeRole::Text)
                .patch(theme.style(ThemeRole::Title)),
            RowContent::File { .. } => theme.style(ThemeRole::Text),
        };
        let style = if row.is_selectable() && selected == Some(row.path.as_path()) {
            cursor = Some(Position::new(body.x, y));
            style.patch(theme.style(ThemeRole::PopupSelection))
        } else {
            style
        };
        // The selection covers the complete row, so the reader finds it at any
        // indent depth.
        target.set_style(Rect::new(body.x, y, body.width, 1), style);
        target.set_stringn(body.x, y, row_text(row, icons), width, style);
        // The icon carries its own color over the row style, so a selected row
        // keeps its background behind the glyph.
        render_row_icon(target, body, y, row, icons, theme, style);
    }
    cursor
}

/// Paints the icon cell of one row with the color of its role.
///
/// The icon sits behind the indent and the expansion marker, so its column
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
    let Ok(offset) = u16::try_from(row.depth * TREE_INDENT_CELLS + MARKER_CELLS) else {
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

/// Renders the title row of the sidebar.
fn render_title(
    target: &mut CellBuffer,
    area: Rect,
    theme: Theme,
    root: &Path,
    focus: WindowFocus,
) {
    let role = match focus {
        WindowFocus::Focused => ThemeRole::Title,
        WindowFocus::Unfocused => ThemeRole::TitleMuted,
    };
    let name = root.file_name().map_or_else(
        || root.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let title = format!(" {name} ");
    let band = Rect::new(area.x, area.y, area.width, TREE_TITLE_ROWS);
    target.set_style(band, theme.style(ThemeRole::Winbar));
    target.set_stringn(
        area.x,
        area.y,
        &title,
        usize::from(area.width),
        theme.style(role),
    );
}
