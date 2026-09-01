//! The file sidebar that one embedded host draws beside its editor.
//!
//! [`EmbeddedEditor`] already owns one lazy file tree over its worktree root.
//! This module publishes that tree as a host surface. The host reads one
//! bounded list of [`FileRow`] values and hands one [`FileSidebarInput`] back
//! for every key that reaches the sidebar. See `docs/embedding.md`.
//!
//! A host draws each row itself from the published facts, or hands the row to
//! [`draw_file_row`] and takes the look of kvim's own file tree. That tree
//! draws through the same painter, so one appearance exists and no second one
//! can drift away from it.
//!
//! The surface names no type of `kvim-workspace`, which
//! `docs/architecture.md` keeps out of the supported packages. It names its own
//! vocabulary, the paths of `kvim-path`, the palette of `kvim-settings`, and
//! the geometry of `kvim-ui`.
//!
//! The tree reads no directory on the host event loop. A row that needs a
//! listing leaves the editor as one unit of work through
//! [`EmbeddedEditor::dispatch`], and the listing reaches the tree through
//! [`EmbeddedEditor::apply`]. The host therefore drives the reads with the one
//! work channel that it already drives for the editor.
//!
//! `crates/kvim-embed/examples/worktree_editor.rs` is one complete host of
//! one such sidebar.
//!
//! [`EmbeddedEditor`]: super::embed::EmbeddedEditor
//! [`EmbeddedEditor::dispatch`]: super::embed::EmbeddedEditor::dispatch
//! [`EmbeddedEditor::apply`]: super::embed::EmbeddedEditor::apply

use kvim_path::WorktreeRelativePath;
use kvim_settings::FileTreeIcons;
use kvim_ui::{
    ListMotion, SIDEBAR_GUIDE_INDENT_CELLS, SIDEBAR_LABEL_CHARS_MAX, SIDEBAR_ROWS_MAX,
    SidebarCanvas,
};
use kvim_workspace::TransferMode;
use ratatui::style::Style;
use unicode_width::UnicodeWidthChar;

use crate::cells::{clip_cells, text_cells};

use super::buffer_view::RegionFocus;
use super::embed::EditorEvent;
use super::icons::{ICON_CELLS, Icon};
use super::theme::{IconRole, Theme, ThemeRole};
use super::tree::{
    COLLAPSED_MARKER, EXPANDED_MARKER, GIT_MARK_CELLS, MARK_CELLS, RowState, SELECTION_MARK,
    mark_cells, paint_span,
};

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

/// The suffix that kvim's own file tree draws behind a symbolic link.
///
/// [`FileRow::is_symlink`] names the fact alone. A host that reproduces the
/// look of kvim appends this suffix to the label; a host that draws its own
/// mark reads the fact instead of guessing the suffix.
pub const FILE_SIDEBAR_LINK_SUFFIX: &str = "@";

/// The number of cells that the icon column of one row occupies.
///
/// Every row reserves this width while the sidebar shows icons, so a row that
/// carries no icon keeps the labels of its neighbours aligned. A host that
/// draws a tree of its own beside the file tree of the editor reserves the
/// same width, so the two icon columns line up. A host that reads it needs no
/// icon table of its own: it chooses its own glyphs and keeps kvim's gutter.
/// [`kvim_settings::FileTreeIcons`] hides every icon of the editor, so one
/// setting answers for both trees.
pub const FILE_SIDEBAR_ICON_CELLS: usize = ICON_CELLS;

/// The number of cells that the selection mark of one row occupies.
///
/// Every row reserves this width at the left edge, so a row that carries no
/// mark keeps the guides and the label of its neighbours aligned. A host that
/// draws a tree of its own beside the file tree of the editor reserves the
/// same width, so the left columns of the two trees line up.
pub const FILE_SIDEBAR_MARK_CELLS: usize = MARK_CELLS;

/// The mark that kvim's own file tree draws on the selected row.
///
/// [`FILE_SIDEBAR_MARK_CELLS`] reserves the column that this glyph fills. A
/// host that reproduces the look of kvim draws this glyph in that column; a
/// host that draws its own mark reads the width alone and chooses its own
/// glyph, exactly as it chooses its own glyph for [`FileRowGit::glyph`].
///
/// kvim draws this mark only while its sidebar reports
/// [`RegionFocus::Focused`](super::buffer_view::RegionFocus::Focused). A host
/// that draws its own mark under a different rule disagrees with kvim about
/// when the mark appears, so a host that reproduces kvim's look shows this
/// glyph under the same rule. See `docs/windows.md`.
pub const FILE_SIDEBAR_SELECTION_MARK: &str = SELECTION_MARK;

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

/// The recorded Git state of one file-sidebar row.
///
/// The variants rise in the same severity order as the source that scans the
/// repository, so a derived comparison ranks two states the way a reader
/// ranks them. A row that carries no state, and a row of a workspace that no
/// read has covered yet, publish `None` from [`FileRow::git`] instead of a
/// variant of this enum.
///
/// # Examples
///
/// ```
/// use kvim_tui::FileRowGit;
///
/// assert!(FileRowGit::Conflicted > FileRowGit::Modified);
/// assert_eq!(FileRowGit::Staged.glyph(), "■");
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FileRowGit {
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

impl FileRowGit {
    /// Returns the mark that kvim's own file tree draws for this state.
    ///
    /// A host that reproduces the look of kvim draws this glyph behind the
    /// row. A host that draws its own marks matches on the state instead and
    /// never reads this method.
    #[inline]
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Staged => "■",
            Self::Modified => "●",
            Self::StagedAndModified => "◆",
            Self::Untracked => "□",
            Self::Ignored => "☑",
            Self::Conflicted => "▲",
        }
    }
}

/// Why one file-sidebar entry uses kvim's dimmed row style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileRowDimming {
    /// A fixed generated name or Git ignore state marks machine output.
    Generated,
    /// The file-operation clipboard holds the entry for copying.
    HeldCopy,
    /// The file-operation clipboard holds the entry for moving.
    HeldMove,
}

/// The characters of one row label that the file-tree search matched.
///
/// The span counts characters of the label, not cells and not bytes, so a
/// name outside the ASCII range marks the same characters that the search
/// found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LabelMatch {
    /// The first matched character of the label.
    pub(super) start: usize,
    /// The number of matched characters.
    pub(super) len: usize,
}

/// Stable semantic identity of one file-sidebar notice row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FileRowNoticeKind {
    /// A directory listing exceeded its entry bound.
    Truncated,
    /// A directory listing failed.
    Unreadable,
    /// Hidden entries were omitted.
    Hidden,
}

/// Stable semantic identity of one file-sidebar row.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FileRowIdentity {
    /// A contained file or directory.
    Entry(WorktreeRelativePath),
    /// A notice attached to the root or one contained directory.
    Notice {
        /// The contained parent, or `None` for the worktree root.
        parent: Option<WorktreeRelativePath>,
        /// The semantic notice kind.
        kind: FileRowNoticeKind,
    },
}

/// One drawable row of the file sidebar of one embedded editor.
///
/// The row holds the text, the indent guides, the depth, the state, the
/// selection, the recorded Git state, the symbolic-link fact, and the icon
/// role of one visible line. Every published accessor returns a fact, so a
/// host that draws its own cells owns the complete look of its sidebar.
///
/// A host that wants kvim's own look hands the row to [`draw_file_row`]
/// instead. The row carries the remaining presentation state that painter
/// needs, and kvim's own file tree draws through the same call, so the two
/// appearances cannot drift apart.
///
/// [`FileRow::guides`] already carries the leading blank that the file tree of
/// kvim draws, because the workspace-root header of that tree is no sibling of
/// the first entries. A host that reproduces the look of kvim draws the guides
/// exactly as they are published. See `docs/windows.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileRow {
    identity: FileRowIdentity,
    path: Option<WorktreeRelativePath>,
    label: String,
    guides: String,
    depth: usize,
    kind: FileRowKind,
    state: RowState,
    selected: bool,
    git: Option<FileRowGit>,
    is_symlink: bool,
    icon: Option<Icon>,
    matched: Option<LabelMatch>,
}

impl FileRow {
    /// Creates one published row.
    ///
    /// The Git state, the symbolic-link fact, the icon, and the matched
    /// characters each start at their absent value. [`FileRow::with_git`],
    /// [`FileRow::with_symlink`], [`FileRow::with_icon`], and
    /// [`FileRow::with_matched`] set them, because they are independent facts
    /// that a caller sets or leaves absent one at a time.
    pub(super) fn new(
        identity: FileRowIdentity,
        path: Option<WorktreeRelativePath>,
        label: String,
        guides: String,
        depth: usize,
        kind: FileRowKind,
        state: RowState,
    ) -> Self {
        let label = if label.chars().count() > FILE_SIDEBAR_LABEL_CHARS_MAX {
            label
                .chars()
                .take(FILE_SIDEBAR_LABEL_CHARS_MAX)
                .collect::<String>()
        } else {
            label
        };
        Self {
            identity,
            path,
            label,
            guides,
            depth,
            kind,
            state,
            selected: false,
            git: None,
            is_symlink: false,
            icon: None,
            matched: None,
        }
    }

    /// Sets whether this selectable row owns the current selection.
    #[must_use]
    pub(super) const fn with_selected(mut self, selected: bool) -> Self {
        debug_assert!(
            !selected || self.kind.is_selectable(),
            "the tree rests its selection on an entry row alone"
        );
        self.selected = selected;
        self
    }

    /// Sets the recorded Git state of the row.
    #[must_use]
    pub(super) const fn with_git(mut self, git: Option<FileRowGit>) -> Self {
        self.git = git;
        self
    }

    /// Sets whether the row names a symbolic link.
    #[must_use]
    pub(super) const fn with_symlink(mut self, is_symlink: bool) -> Self {
        self.is_symlink = is_symlink;
        self
    }

    /// Sets the icon of the row.
    ///
    /// The row publishes both the exact glyph and its semantic role. Kvim's
    /// painter reads this same icon value, so a host can reproduce its output.
    #[must_use]
    pub(super) const fn with_icon(mut self, icon: Option<Icon>) -> Self {
        self.icon = icon;
        self
    }

    /// Sets the characters of the label that the file-tree search matched.
    #[must_use]
    pub(super) fn with_matched(mut self, matched: Option<LabelMatch>) -> Self {
        let label_chars = self.label.chars().count();
        self.matched = matched.filter(|matched| {
            matched.len > 0
                && matched
                    .start
                    .checked_add(matched.len)
                    .is_some_and(|end| end <= label_chars)
        });
        self
    }

    /// Returns the stable identity of this row within one editor lifetime.
    #[inline]
    #[must_use]
    pub const fn identity(&self) -> &FileRowIdentity {
        &self.identity
    }

    /// Returns the contained entry path, or `None` for a notice row.
    #[inline]
    #[must_use]
    pub const fn path(&self) -> Option<&WorktreeRelativePath> {
        self.path.as_ref()
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
    pub const fn depth(&self) -> u16 {
        debug_assert!(
            self.depth <= kvim_path::WORKTREE_PATH_COMPONENTS_MAX,
            "validated worktree paths bound every published row depth"
        );
        self.depth as u16
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

    /// Returns the recorded Git state of the row, or `None` while the row
    /// carries no state.
    ///
    /// A [`FileRowKind::Note`] row and a row of a workspace that no read has
    /// covered yet both publish `None`. [`FileRowGit::glyph`] returns kvim's
    /// own mark for a state that this method reports.
    #[inline]
    #[must_use]
    pub const fn git(&self) -> Option<FileRowGit> {
        self.git
    }

    /// Reports whether the row names a symbolic link.
    ///
    /// [`FILE_SIDEBAR_LINK_SUFFIX`] is the suffix that kvim's own file tree
    /// draws behind a row that reports `true` here.
    #[inline]
    #[must_use]
    pub const fn is_symlink(&self) -> bool {
        self.is_symlink
    }

    /// Returns the icon role of the row, or `None` while the row carries no
    /// icon.
    ///
    /// A [`FileRowKind::Note`] row publishes `None`. Every other row publishes
    /// its role regardless of the icon-visibility setting of kvim's own file
    /// tree, so a host draws its own icons even while kvim would draw none.
    #[inline]
    #[must_use]
    pub const fn icon_role(&self) -> Option<IconRole> {
        match self.icon {
            Some(icon) => Some(icon.role),
            None => None,
        }
    }

    /// Returns why kvim dims this entry, or `None` for a non-dimmed row.
    ///
    /// Git ignored state remains available separately through [`Self::git`].
    #[inline]
    #[must_use]
    pub const fn dimming(&self) -> Option<FileRowDimming> {
        match self.state {
            RowState::Generated => Some(FileRowDimming::Generated),
            RowState::Held(TransferMode::Copy) => Some(FileRowDimming::HeldCopy),
            RowState::Held(TransferMode::Move) => Some(FileRowDimming::HeldMove),
            RowState::Directory | RowState::File | RowState::Omitted | RowState::Incomplete => None,
        }
    }

    /// Returns the semantic notice kind, or `None` for an entry row.
    #[inline]
    #[must_use]
    pub const fn notice_kind(&self) -> Option<FileRowNoticeKind> {
        match &self.identity {
            FileRowIdentity::Notice { kind, .. } => Some(*kind),
            FileRowIdentity::Entry(_) => None,
        }
    }

    /// Returns the exact one-cell icon glyph that kvim draws.
    ///
    /// The glyph requires the patched font described in `docs/files.md`.
    #[inline]
    #[must_use]
    pub const fn icon_glyph(&self) -> Option<&'static str> {
        match self.icon {
            Some(icon) => Some(icon.glyph),
            None => None,
        }
    }

    /// Returns the matched label span as `(start, length)` in characters.
    ///
    /// These values are not byte offsets or terminal-cell columns. Both values
    /// are bounded by [`FILE_SIDEBAR_LABEL_CHARS_MAX`].
    #[inline]
    #[must_use]
    pub const fn matched_characters(&self) -> Option<(usize, usize)> {
        match self.matched {
            Some(matched) => Some((matched.start, matched.len)),
            None => None,
        }
    }

    /// Returns the complete text of the row, as kvim's own file tree writes it.
    ///
    /// The text holds the blank mark cell, the indent guides, the glyph cells,
    /// the label, the symbolic-link suffix, and the suffix of the row state. A
    /// [`FileRowKind::Note`] row reports about its directory instead of naming
    /// an entry, so it carries neither suffix.
    fn text(&self, icons: FileTreeIcons) -> String {
        let mark = " ".repeat(MARK_CELLS);
        let glyph = self.glyph(icons);
        let guides = &self.guides;
        let label = &self.label;
        if !self.kind.is_selectable() {
            return format!("{mark}{guides}{glyph}{label}");
        }
        let link = if self.is_symlink {
            FILE_SIDEBAR_LINK_SUFFIX
        } else {
            ""
        };
        let held = self.state.suffix();
        format!("{mark}{guides}{glyph}{label}{link}{held}")
    }

    /// Returns the glyph cells that sit between the guides and the label.
    ///
    /// Without a patched font the expansion marker of a directory takes the
    /// same cells, so the state of a directory stays visible and the labels
    /// keep one column in both icon settings.
    fn glyph(&self, icons: FileTreeIcons) -> String {
        match (icons, self.icon, self.kind) {
            (FileTreeIcons::Shown, Some(icon), _) => format!("{} ", icon.glyph),
            (FileTreeIcons::Hidden, _, FileRowKind::ClosedDirectory) => COLLAPSED_MARKER.to_owned(),
            (
                FileTreeIcons::Hidden,
                _,
                FileRowKind::OpenDirectory | FileRowKind::LoadingDirectory,
            ) => EXPANDED_MARKER.to_owned(),
            _ => " ".repeat(ICON_CELLS),
        }
    }
}

/// Returns the cell column of the glyph cells inside one file sidebar row.
///
/// The glyph follows the mark cell and the indent guides of every level, which
/// each cost [`SIDEBAR_GUIDE_INDENT_CELLS`] cells. The workspace root is one
/// level above the first entry, so a row of depth zero already carries one
/// guide, the leading blank that stands for the header row.
const fn glyph_offset_cells(depth: usize) -> usize {
    MARK_CELLS + SIDEBAR_GUIDE_INDENT_CELLS * (depth + 1)
}

/// Returns the cell column of the label inside one file sidebar row.
///
/// Both icon settings reserve the same glyph cells, so the label of one depth
/// always starts at one column.
pub(super) const fn label_offset_cells(depth: usize) -> usize {
    glyph_offset_cells(depth) + ICON_CELLS
}

/// Draws one file sidebar row exactly as kvim's own file tree draws it.
///
/// The painter owns the complete layout of the row. The first cell holds the
/// selection mark, the indent guides and the glyph cells follow it, and the
/// last cell holds the Git mark. A canvas that is narrower than that layout
/// clips from the right edge, and the Git mark keeps the last cell it has, so
/// a very narrow sidebar still shows the start of every label.
///
/// The selection mark belongs to the focused sidebar alone. A sidebar that
/// reports [`RegionFocus::Unfocused`] leaves the mark cell blank, and the
/// selected row still reads as the selected one through the fill of the whole
/// row. The mark cell keeps its width in both states, so no glyph and no label
/// moves when the focus moves. See `docs/windows.md`.
///
/// The caller supplies the palette, the icon-visibility setting, and the focus
/// of its sidebar, because a host holds all three already. The painter reads
/// no other state: every further fact that it draws comes from the row. kvim's
/// own file tree draws through this call, so a host that uses it can never see
/// a second appearance.
///
/// # Examples
///
/// ```
/// use kvim_settings::FileTreeIcons;
/// use kvim_tui::__private::{EmbeddedEditor, RegionFocus, Theme, draw_file_row};
/// use kvim_ui::{RowKind, SidebarRow, SidebarState};
/// use ratatui::buffer::Buffer;
/// use ratatui::layout::Rect;
///
/// let root = std::sync::Arc::new(
///     kvim_path::WorktreeRoot::open(
///         std::env::current_dir().expect("the process holds a working directory"),
///     )
///     .expect("the working directory is a worktree"),
/// );
/// let area = Rect::new(0, 0, 24, 8);
/// let mut editor = EmbeddedEditor::builder(root, area)
///     .open()
///     .expect("the rectangle holds cells");
///
/// // The host owns the row geometry, so it holds one bounded sidebar state
/// // over the rows that the editor publishes. The first listing has not
/// // arrived here, so this list is still empty and the render draws nothing.
/// let rows = editor.file_rows();
/// let mut view = SidebarState::new(area.height);
/// view.set_rows(
///     (0..rows.len())
///         .map(|index| SidebarRow::single(index, RowKind::Selectable))
///         .collect(),
/// )
/// .expect("the tree bounds hold every row");
///
/// let mut cells = Buffer::empty(area);
/// let theme = Theme::new();
/// view.render(&mut cells, area, |canvas, placement| {
///     if let Some(row) = rows.get(placement.index()) {
///         draw_file_row(canvas, row, theme, FileTreeIcons::Hidden, RegionFocus::Focused);
///     }
/// })
/// .expect("every row stays inside the rectangle");
/// ```
pub fn draw_file_row(
    canvas: &mut SidebarCanvas<'_>,
    row: &FileRow,
    theme: Theme,
    icons: FileTreeIcons,
    focus: RegionFocus,
) {
    let style = theme
        .style(ThemeRole::Text)
        .patch(theme.style(row.state.role()));
    let style = if row.selected {
        style.patch(theme.style(ThemeRole::PopupSelection))
    } else {
        style
    };
    // The selection covers the complete row, so the reader finds it at any
    // indent depth.
    canvas.fill(style);
    // The Git mark owns the last cell of every row, so a long label never
    // covers it and no mark ever moves a label.
    let label_cells = canvas.width_cells().saturating_sub(GIT_MARK_CELLS);
    let text = row.text(icons);
    canvas.draw_clipped(0, 0, &text, label_cells, style);
    // The guides carry their own color, so they separate from the labels
    // without the state of the row changing their meaning.
    paint_span(
        canvas,
        MARK_CELLS,
        row.guides.chars().count(),
        style.patch(theme.style(ThemeRole::TreeIndentGuide)),
    );
    // The mark reports which row the keys move. An unfocused sidebar moves no
    // row, so it draws no mark and the fill alone reports the selection.
    if row.selected && focus == RegionFocus::Focused {
        canvas.draw_clipped(
            0,
            0,
            SELECTION_MARK,
            mark_cells(),
            style.patch(theme.style(ThemeRole::TreeSelectionMark)),
        );
    }
    draw_row_icon(canvas, row, icons, theme, style);
    if let Some(git) = row.git {
        // The label of a changed file takes the color of its state. A
        // directory keeps the title color, because its state rolls up from the
        // entries below it and names no change of the directory itself. A
        // dimmed row keeps its own color, so a quiet row stays quiet.
        if row.state == RowState::File {
            paint_span(
                canvas,
                label_offset_cells(row.depth),
                row.label.chars().count(),
                style.patch(theme.style(ThemeRole::TreeGit(git))),
            );
        }
    }
    // The search marks every match. The selected row carries the match that
    // `n` and `N` moved to, so it reads as the current one, exactly as the
    // match under the cursor does in a buffer window. The mark wins over every
    // dimmed style, so a match inside a held or generated entry stays readable
    // as one match.
    if let Some(matched) = row.matched {
        let role = if row.selected {
            ThemeRole::CurrentSearchMatch
        } else {
            ThemeRole::SearchMatch
        };
        paint_span(
            canvas,
            label_offset_cells(row.depth).saturating_add(matched.start),
            matched.len,
            style.patch(theme.style(role)),
        );
    }
    fade_clipped_text(canvas, &text, label_cells, row, theme, style);
    if let Some(git) = row.git {
        draw_git_mark(canvas, git, theme, style);
    }
}

/// Fades the final visible characters of clipped row text into its background.
///
/// One style covers every cell of a wide character. This keeps both halves of
/// that character together while the gradient still follows terminal cells.
fn fade_clipped_text(
    canvas: &mut SidebarCanvas<'_>,
    text: &str,
    available_cells: u16,
    row: &FileRow,
    theme: Theme,
    row_style: Style,
) {
    const FADE_CELLS: usize = 3;
    const FADE_STEPS: u16 = 4;

    let available_cells = usize::from(available_cells);
    if text_cells(text) <= available_cells {
        return;
    }
    let visible = clip_cells(text, available_cells);
    let visible_cells = text_cells(visible);
    let fade_start = visible_cells.saturating_sub(FADE_CELLS);
    let mut column = 0;
    for value in visible.chars() {
        let cells = value.width().unwrap_or(1);
        let end = column + cells;
        if cells > 0 && end > fade_start {
            let mut foreground_style = row_style;
            let label_start = label_offset_cells(row.depth);
            if let Some(git) = row.git.filter(|_| {
                row.state == RowState::File
                    && column >= label_start
                    && column < label_start.saturating_add(text_cells(&row.label))
            }) {
                foreground_style = row_style.patch(theme.style(ThemeRole::TreeGit(git)));
            }
            if let Some(matched) = row.matched {
                let match_start = label_start.saturating_add(matched.start);
                if column >= match_start && column < match_start.saturating_add(matched.len) {
                    let role = if row.selected {
                        ThemeRole::CurrentSearchMatch
                    } else {
                        ThemeRole::SearchMatch
                    };
                    foreground_style = row_style.patch(theme.style(role));
                }
            }
            let step = u16::try_from(end - fade_start).unwrap_or(FADE_STEPS);
            let faded = theme.fade_foreground(
                foreground_style,
                foreground_style.bg.or(row_style.bg),
                step.min(FADE_STEPS - 1),
                FADE_STEPS,
            );
            paint_span(canvas, column, cells, faded);
        }
        column = end;
    }
}

/// Paints the icon cell of one row with the color of its role.
///
/// The icon sits behind the mark cell and the indent guides, so its column
/// follows the depth of the row. A row whose icon falls outside the sidebar
/// keeps the clipped text that the row already wrote. The icon carries its own
/// color over the row style, so a selected row keeps its background behind the
/// glyph.
fn draw_row_icon(
    canvas: &mut SidebarCanvas<'_>,
    row: &FileRow,
    icons: FileTreeIcons,
    theme: Theme,
    style: Style,
) {
    if icons == FileTreeIcons::Hidden {
        return;
    }
    let Some(icon) = row.icon else {
        return;
    };
    let (Ok(offset), Ok(cells)) = (
        u16::try_from(glyph_offset_cells(row.depth)),
        u16::try_from(ICON_CELLS),
    ) else {
        debug_assert!(false, "the tree depth stays inside TREE_DEPTH_MAX");
        return;
    };
    if offset >= canvas.width_cells() {
        return;
    }
    canvas.draw_clipped(
        0,
        offset,
        icon.glyph,
        cells,
        style.patch(theme.style(ThemeRole::Icon(icon.role))),
    );
}

/// Paints the Git mark of one row at the right edge of the sidebar.
///
/// The mark reports the state of the entry, and of every entry below a
/// directory. A sidebar that holds no cell for the mark paints none, so a very
/// narrow sidebar still shows its labels. See `docs/git.md`.
pub(super) fn draw_git_mark(
    canvas: &mut SidebarCanvas<'_>,
    git: FileRowGit,
    theme: Theme,
    style: Style,
) {
    let Some(offset) = canvas.width_cells().checked_sub(GIT_MARK_CELLS) else {
        return;
    };
    canvas.draw_clipped(
        0,
        offset,
        git.glyph(),
        GIT_MARK_CELLS,
        style.patch(theme.style(ThemeRole::TreeGit(git))),
    );
}

/// One input that a host applies to the file sidebar of one embedded editor.
///
/// The sidebar runs no host command and opens no file. It reports what the
/// input means through [`FileSidebarOutcome`], and the host decides the effect.
///
/// # Examples
///
/// ```
/// use kvim_tui::__private::{FileSidebarInput, ListMotion};
///
/// let down = FileSidebarInput::Move(ListMotion::Down(1));
/// assert_eq!(down, FileSidebarInput::Move(ListMotion::Down(1)));
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum FileSidebarInput {
    /// Select the current selectable row that has this stable identity.
    ///
    /// A stale identity and a [`FileRowKind::Note`] identity leave the current
    /// selection unchanged and return [`FileSidebarOutcome::NotSelected`].
    Select(FileRowIdentity),
    /// Move the selection by one bounded move.
    ///
    /// The move stops at the first and the last row, so it never wraps. A
    /// [`FileRowKind::Note`] row takes no selection, so the move takes the
    /// nearest entry row in the direction of travel. [`ListMotion::Parent`]
    /// moves to the directory that holds the selected entry, the same
    /// directory that [`FileSidebarInput::Close`] selects on a row that
    /// holds no open directory of its own.
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
    /// Refresh all expanded directories and recorded Git state.
    Refresh,
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
    /// The requested row was absent or does not accept selection.
    ///
    /// The current selection remains unchanged.
    NotSelected,
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
    /// use kvim_tui::__private::{EditorEvent, FileSidebarOutcome};
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
            Self::Applied | Self::NotSelected => None,
            Self::Activated { path } => Some(EditorEvent::FileActivated { path: path.clone() }),
        }
    }

    /// Returns the file that the reader activated, if the input activated one.
    #[inline]
    #[must_use]
    pub const fn activated(&self) -> Option<&WorktreeRelativePath> {
        match self {
            Self::Applied | Self::NotSelected => None,
            Self::Activated { path } => Some(path),
        }
    }
}

#[cfg(test)]
#[path = "file_sidebar_tests.rs"]
mod tests;
