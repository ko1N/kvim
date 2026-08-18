//! The workspace file tree: the lazy directory model and its visible rows.
//!
//! [`read_directory`] blocks and runs on the bounded worker service only. Every
//! other function of this file is a pure transition over already loaded
//! listings, so the terminal event loop never touches the filesystem. See
//! `docs/files.md` and `docs/responsiveness.md`.
//!
//! The tree reads a directory when the user expands it, when a reveal needs it,
//! or when a refresh asks for it. [`FileTree::take_pending_read`] hands the next
//! directory to the caller, and [`FileTree::apply_listing`] applies the result.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// The largest number of entries that the tree keeps for one directory.
///
/// A larger directory shows its first entries in the deterministic order below
/// and reports the truncation as one visible row.
pub const TREE_DIRECTORY_ENTRIES_MAX: usize = 512;

/// The largest number of names that one directory read inspects.
///
/// The read stops at this value, so a directory with a very large number of
/// names never costs unbounded time or memory.
pub const TREE_DIRECTORY_SCAN_MAX: usize = 4096;

/// The largest number of entries that the complete tree keeps loaded.
pub const TREE_ENTRIES_MAX: usize = 8192;

/// The largest depth below the workspace root that the tree expands.
///
/// The bound also stops a symbolic link that points at one of its own parents.
pub const TREE_DEPTH_MAX: usize = 16;

/// The largest number of directory reads that wait for the worker service.
pub const TREE_PENDING_READS_MAX: usize = 64;

/// The largest number of characters in one search query.
pub const TREE_SEARCH_CHARS_MAX: usize = 64;

/// The largest number of directories that one search reads.
///
/// A match may sit inside a directory that the tree never listed, so the search
/// asks for those listings. The bound stops one search from walking a complete
/// workspace.
pub const TREE_SEARCH_READS_MAX: usize = 64;

/// The largest number of matches that one search marks.
///
/// A short query matches many names. The bound keeps the marked rows small
/// enough for highlighting and for repeated match movement, as the buffer
/// search bounds its own match list.
pub const TREE_SEARCH_MATCHES_MAX: usize = 256;

/// The names that the tree hides while the hidden policy is [`HiddenPolicy::Hide`].
///
/// The list mirrors the reference editor setup. A name that starts with a full
/// stop is hidden by the same policy.
pub const HIDDEN_NAMES: [&str; 2] = [".DS_Store", "thumbs.db"];

/// The kind of one workspace entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// A directory, or a symbolic link whose target is a directory.
    Directory,
    /// A regular file, or any other supported non-directory entry.
    File,
}

impl EntryKind {
    /// Returns the sort rank of the kind. A directory sorts before a file.
    #[must_use]
    pub const fn order(self) -> u8 {
        match self {
            Self::Directory => 0,
            Self::File => 1,
        }
    }
}

/// Whether one entry is the named target or a symbolic link to it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkKind {
    /// The entry is the target itself.
    Direct,
    /// The entry is a symbolic link.
    Symlink,
}

/// One entry of one directory listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeEntry {
    /// The file name inside its directory.
    pub name: String,
    /// The kind of the entry, or of the target of a symbolic link.
    pub kind: EntryKind,
    /// Whether the entry is a symbolic link.
    pub link: LinkKind,
}

/// Whether one directory listing holds every entry of that directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Truncation {
    /// The listing holds every entry.
    Complete,
    /// The listing holds `shown` of `total` inspected entries.
    Truncated {
        /// The number of entries that the listing keeps.
        shown: usize,
        /// The number of entries that the read inspected.
        total: usize,
    },
}

/// One complete directory read.
#[derive(Clone, Debug)]
pub struct DirectoryListing {
    /// The directory that the read inspected.
    pub path: PathBuf,
    /// The entries, ordered by kind and then by name.
    pub entries: Vec<TreeEntry>,
    /// Whether the listing holds every entry of the directory.
    pub truncation: Truncation,
}

/// A rejected directory read.
#[derive(Debug, Error)]
pub enum ReadError {
    /// The path names no directory.
    #[error("the path is not a directory")]
    NotADirectory,
    /// The directory could not be read.
    #[error("the directory could not be read")]
    Read(#[source] io::Error),
}

/// Reads one directory into a bounded and ordered listing.
///
/// The call blocks. Run it on the bounded worker service only.
///
/// # Errors
///
/// Returns [`ReadError`] for a path that names no directory and for an
/// unreadable directory.
pub fn read_directory(path: &Path) -> Result<DirectoryListing, ReadError> {
    let metadata = fs::metadata(path).map_err(ReadError::Read)?;
    if !metadata.is_dir() {
        return Err(ReadError::NotADirectory);
    }
    let reader = fs::read_dir(path).map_err(ReadError::Read)?;

    let mut entries = Vec::new();
    let mut total = 0usize;
    for entry in reader.take(TREE_DIRECTORY_SCAN_MAX) {
        let entry = entry.map_err(ReadError::Read)?;
        // A name that is not UTF-8 has no place in the tree, because every path
        // that the editor shows and stores is UTF-8 text.
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        total += 1;
        let link = if file_type.is_symlink() {
            LinkKind::Symlink
        } else {
            LinkKind::Direct
        };
        // A symbolic link takes the kind of its target, so an expanded link to a
        // directory shows that directory. A broken link stays a file.
        let kind = if file_type.is_dir()
            || (file_type.is_symlink() && fs::metadata(entry.path()).is_ok_and(|it| it.is_dir()))
        {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        entries.push(TreeEntry { name, kind, link });
    }

    entries.sort_by(|left, right| {
        left.kind
            .order()
            .cmp(&right.kind.order())
            .then_with(|| left.name.cmp(&right.name))
    });
    let truncation = if entries.len() > TREE_DIRECTORY_ENTRIES_MAX {
        entries.truncate(TREE_DIRECTORY_ENTRIES_MAX);
        Truncation::Truncated {
            shown: TREE_DIRECTORY_ENTRIES_MAX,
            total,
        }
    } else {
        Truncation::Complete
    };
    Ok(DirectoryListing {
        path: path.to_path_buf(),
        entries,
        truncation,
    })
}

/// Whether the tree shows dotfiles and the names of [`HIDDEN_NAMES`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HiddenPolicy {
    /// Hide dotfiles and the named files. This is the default.
    Hide,
    /// Show every entry.
    Show,
}

impl HiddenPolicy {
    /// Returns the other policy.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Hide => Self::Show,
            Self::Show => Self::Hide,
        }
    }
}

/// Reports whether the hidden-entry policy keeps one name.
fn keeps(hidden: HiddenPolicy, name: &str) -> bool {
    match hidden {
        HiddenPolicy::Show => true,
        HiddenPolicy::Hide => !name.starts_with('.') && !HIDDEN_NAMES.contains(&name),
    }
}

/// The characters of one entry name that the active search matched.
///
/// The values count characters of the name, never bytes, so a renderer places
/// the highlight at one cell column of the row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NameMatch {
    /// The first matched character of the name.
    pub start: usize,
    /// The number of matched characters.
    pub len: usize,
}

/// Whether one directory row holds its children.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expansion {
    /// The directory is closed.
    Collapsed,
    /// The directory is open and its listing is loaded.
    Expanded,
    /// The directory is open and waits for its listing.
    Pending,
}

/// One row that reports a bounded or failed directory read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Notice {
    /// The listing holds `shown` of `total` inspected entries.
    Truncated {
        /// The number of entries that the listing keeps.
        shown: usize,
        /// The number of entries that the read inspected.
        total: usize,
    },
    /// The directory could not be read.
    Unreadable,
}

/// What one visible row shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowContent {
    /// One file entry.
    File {
        /// The file name.
        name: String,
        /// Whether the entry is a symbolic link.
        link: LinkKind,
    },
    /// One directory entry.
    Directory {
        /// The directory name.
        name: String,
        /// Whether the entry is a symbolic link.
        link: LinkKind,
        /// Whether the directory shows its children.
        expansion: Expansion,
    },
    /// One report about the directory of the row. The row holds no entry.
    Notice(Notice),
}

/// One visible row of the tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeRow {
    /// The absolute path of the entry, or of the directory of a notice.
    pub path: PathBuf,
    /// The number of directories between the workspace root and the row.
    pub depth: usize,
    /// What the row shows.
    pub content: RowContent,
    /// The characters that the active search matched, or `None`.
    pub matched: Option<NameMatch>,
}

impl TreeRow {
    /// Reports whether the selection may rest on this row.
    #[must_use]
    pub const fn is_selectable(&self) -> bool {
        !matches!(self.content, RowContent::Notice(_))
    }

    /// Returns the entry name, or `None` for a notice row.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match &self.content {
            RowContent::File { name, .. } | RowContent::Directory { name, .. } => Some(name),
            RowContent::Notice(_) => None,
        }
    }

    /// Returns the kind of the entry, or `None` for a notice row.
    #[must_use]
    pub const fn kind(&self) -> Option<EntryKind> {
        match self.content {
            RowContent::File { .. } => Some(EntryKind::File),
            RowContent::Directory { .. } => Some(EntryKind::Directory),
            RowContent::Notice(_) => None,
        }
    }
}

/// Who opened one directory of the tree.
///
/// The tree records the owner before it opens anything, so the end of a search
/// closes exactly the directories that the search opened and keeps every
/// directory that the user opened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Opened {
    /// The user opened the directory, or a reveal opened it for the user.
    User,
    /// The active search opened the directory to show a match below it.
    Search,
}

/// One active file-tree search.
///
/// The search removes no row. It marks the names that hold the query, and it
/// opens the directories that hold a match. See `docs/files.md`.
#[derive(Clone, Debug)]
struct TreeSearch {
    /// The query in lowercase, bounded by [`TREE_SEARCH_CHARS_MAX`] characters.
    query: String,
    /// The directories that this search asked the caller to read.
    read: BTreeSet<PathBuf>,
}

/// The loaded state of one directory.
#[derive(Clone, Debug)]
enum DirectoryState {
    /// The read finished.
    Listed {
        entries: Vec<TreeEntry>,
        truncation: Truncation,
    },
    /// The read failed.
    Unreadable,
}

/// The lazy directory tree of one workspace.
///
/// The tree owns the loaded listings, the expansion set, the selection, and the
/// filter. It performs no filesystem work. The caller runs
/// [`read_directory`] for every path that [`FileTree::take_pending_read`]
/// returns and applies the outcome.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
///
/// use kvim_workspace::{DirectoryListing, EntryKind, FileTree, LinkKind, TreeEntry, Truncation};
///
/// let root = PathBuf::from("/workspace");
/// let mut tree = FileTree::new(root.clone());
///
/// // The tree opens with one pending read for the workspace root.
/// assert_eq!(tree.take_pending_read(), Some(root.clone()));
/// tree.apply_listing(DirectoryListing {
///     path: root.clone(),
///     entries: vec![TreeEntry {
///         name: "main.rs".to_owned(),
///         kind: EntryKind::File,
///         link: LinkKind::Direct,
///     }],
///     truncation: Truncation::Complete,
/// });
///
/// assert_eq!(tree.rows().len(), 1);
/// assert_eq!(tree.selected(), Some(root.join("main.rs").as_path()));
/// ```
#[derive(Debug)]
pub struct FileTree {
    root: PathBuf,
    directories: BTreeMap<PathBuf, DirectoryState>,
    /// Every open directory and the owner that opened it.
    expanded: BTreeMap<PathBuf, Opened>,
    pending: VecDeque<PathBuf>,
    selected: Option<PathBuf>,
    reveal: Option<PathBuf>,
    hidden: HiddenPolicy,
    search: Option<TreeSearch>,
    rows: Vec<TreeRow>,
}

impl FileTree {
    /// Creates one tree over a workspace root and asks for the first read.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root: root.clone(),
            directories: BTreeMap::new(),
            expanded: BTreeMap::new(),
            pending: VecDeque::new(),
            selected: None,
            reveal: None,
            hidden: HiddenPolicy::Hide,
            search: None,
            rows: Vec::new(),
        };
        tree.expanded.insert(root.clone(), Opened::User);
        tree.pending.push_back(root);
        tree
    }

    /// Returns the workspace root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the visible rows in display order.
    #[must_use]
    pub fn rows(&self) -> &[TreeRow] {
        &self.rows
    }

    /// Returns the hidden-entry policy.
    #[must_use]
    pub const fn hidden(&self) -> HiddenPolicy {
        self.hidden
    }

    /// Returns the selected path, or `None` while the tree shows no row.
    #[must_use]
    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// Returns the selected row, or `None` while the tree shows no row.
    #[must_use]
    pub fn selected_row(&self) -> Option<&TreeRow> {
        self.selected_index().map(|index| &self.rows[index])
    }

    /// Returns the directory that receives a create or a paste.
    ///
    /// The directory is the selected directory, the directory of the selected
    /// file, or the workspace root while nothing is selected.
    #[must_use]
    pub fn destination_directory(&self) -> PathBuf {
        match self.selected_row() {
            Some(row) => match row.content {
                RowContent::Directory { .. } => row.path.clone(),
                RowContent::File { .. } | RowContent::Notice(_) => row
                    .path
                    .parent()
                    .map_or_else(|| self.root.clone(), Path::to_path_buf),
            },
            None => self.root.clone(),
        }
    }

    /// Returns the next directory that needs a read, or `None`.
    ///
    /// The caller runs [`read_directory`] on the bounded worker service and
    /// applies the outcome with [`FileTree::apply_listing`] or
    /// [`FileTree::apply_read_failure`].
    pub fn take_pending_read(&mut self) -> Option<PathBuf> {
        while let Some(path) = self.pending.pop_front() {
            if self.expanded.contains_key(&path) || self.searched(&path) {
                return Some(path);
            }
        }
        None
    }

    /// Applies one completed directory read.
    ///
    /// The reconciliation keeps every expanded directory and the selection while
    /// their entries still exist. It drops the state of an entry that
    /// disappeared.
    pub fn apply_listing(&mut self, listing: DirectoryListing) {
        if !self.expanded.contains_key(&listing.path) && !self.searched(&listing.path) {
            // The user collapsed the directory, or the search ended, while the
            // read ran.
            return;
        }
        let names: BTreeSet<&str> = listing
            .entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::Directory)
            .map(|entry| entry.name.as_str())
            .collect();
        let stale = self.stale_descendants(&listing.path, &names);
        for path in stale {
            self.expanded.remove(&path);
            self.directories.remove(&path);
        }

        let requested = listing.entries.len();
        let entries = self.fit_entries(&listing.path, listing.entries);
        let truncation = if entries.len() < requested {
            let total = match listing.truncation {
                Truncation::Complete => requested,
                Truncation::Truncated { total, .. } => total,
            };
            Truncation::Truncated {
                shown: entries.len(),
                total,
            }
        } else {
            listing.truncation
        };
        self.directories.insert(
            listing.path,
            DirectoryState::Listed {
                entries,
                truncation,
            },
        );
        self.rebuild();
    }

    /// Records that one directory read failed.
    pub fn apply_read_failure(&mut self, path: &Path) {
        if !self.expanded.contains_key(path) && !self.searched(path) {
            return;
        }
        self.directories
            .insert(path.to_path_buf(), DirectoryState::Unreadable);
        self.rebuild();
    }

    /// Opens one directory and asks for its listing when it holds none.
    pub fn expand(&mut self, path: &Path) {
        if !self.accepts(path) {
            return;
        }
        // A directory that the user opens survives the end of a search, even
        // when the search opened it first.
        self.expanded.insert(path.to_path_buf(), Opened::User);
        if !self.directories.contains_key(path) {
            self.push_pending(path);
        }
        self.rebuild();
    }

    /// Closes one directory and drops its loaded listing.
    ///
    /// The tree drops the listing, so the loaded entries stay inside
    /// [`TREE_ENTRIES_MAX`] and a later expansion reads current entries.
    pub fn collapse(&mut self, path: &Path) {
        let stale: Vec<PathBuf> = self
            .expanded
            .keys()
            .filter(|loaded| loaded.starts_with(path))
            .cloned()
            .collect();
        for loaded in stale {
            if loaded != self.root {
                self.expanded.remove(&loaded);
            }
            self.directories.remove(&loaded);
        }
        if path == self.root {
            // The root always stays open, so it needs its listing again.
            self.push_pending(&self.root.clone());
        }
        self.rebuild();
    }

    /// Opens one closed directory and closes one open directory.
    pub fn toggle(&mut self, path: &Path) {
        if self.expanded.contains_key(path) {
            self.collapse(path);
        } else {
            self.expand(path);
        }
    }

    /// Asks for a new read of one expanded directory.
    pub fn refresh(&mut self, path: &Path) {
        if !self.expanded.contains_key(path) {
            return;
        }
        self.push_pending(path);
    }

    /// Asks for a new read of every expanded directory, from the root down.
    pub fn refresh_all(&mut self) {
        for path in self.expanded.keys().cloned().collect::<Vec<PathBuf>>() {
            self.push_pending(&path);
        }
    }

    /// Selects one path and expands every directory above it.
    ///
    /// The tree loads only the directories on the path. The selection follows
    /// when the last listing arrives.
    pub fn reveal(&mut self, path: &Path) {
        if !path.starts_with(&self.root) {
            return;
        }
        let mut directory = self.root.clone();
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return;
        };
        let mut components: Vec<_> = relative.components().collect();
        // The last component is the revealed entry itself, not a parent.
        components.pop();
        for component in components {
            directory.push(component);
            self.expand(&directory);
        }
        self.reveal = Some(path.to_path_buf());
        self.rebuild();
    }

    /// Selects one visible path.
    pub fn select(&mut self, path: &Path) {
        if self
            .rows
            .iter()
            .any(|row| row.is_selectable() && row.path == path)
        {
            self.selected = Some(path.to_path_buf());
        }
    }

    /// Moves the selection to the next selectable row.
    pub fn select_next(&mut self) {
        self.select_step(Step::Next);
    }

    /// Moves the selection to the previous selectable row.
    pub fn select_previous(&mut self) {
        self.select_step(Step::Previous);
    }

    /// Moves the selection to the directory that holds the selected entry.
    pub fn select_parent(&mut self) {
        let Some(parent) = self
            .selected
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
        else {
            return;
        };
        self.select(&parent);
    }

    /// Shows dotfiles and the named files, or hides them again.
    pub fn toggle_hidden(&mut self) {
        self.hidden = self.hidden.toggled();
        self.rebuild();
    }

    /// Starts one search, or refines the query of the active one.
    ///
    /// The search keeps every row. It marks the names that hold the query and
    /// opens the directories that hold a match, so the reader keeps the tree
    /// that the search found the match in. An empty query ends the search.
    ///
    /// The tree keeps the first [`TREE_SEARCH_CHARS_MAX`] characters and
    /// compares without case.
    pub fn start_search(&mut self, query: &str) {
        let query: String = query
            .chars()
            .take(TREE_SEARCH_CHARS_MAX)
            .flat_map(char::to_lowercase)
            .collect();
        if query.is_empty() {
            self.end_search();
            return;
        }
        match self.search.as_mut() {
            // A refined query reuses the listings that this search already
            // read, so a longer query never asks for one directory twice.
            Some(search) => search.query = query,
            None => {
                self.search = Some(TreeSearch {
                    query,
                    read: BTreeSet::new(),
                });
            }
        }
        self.rebuild();
    }

    /// Ends the active search and restores the expansion of the user.
    ///
    /// The expansion map carries the owner of every open directory, so the
    /// restore is one commit over that map instead of a rollback: every
    /// directory that the search opened closes, and every directory that the
    /// user opened stays open. A directory that disappeared meanwhile holds no
    /// listing and no row, so it leaves the same consistent state behind.
    pub fn end_search(&mut self) {
        if self.search.take().is_none() {
            return;
        }
        self.expanded.retain(|_, opened| *opened == Opened::User);
        // A listing outside the expansion set would hold the entry bound while
        // it shows no row, so the tree drops it exactly as a collapse does.
        let expanded = &self.expanded;
        self.directories
            .retain(|path, _| expanded.contains_key(path));
        self.rebuild();
    }

    /// Returns the query of the active search, or `None`.
    #[must_use]
    pub fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|search| search.query.as_str())
    }

    /// Returns the index of the selected row.
    fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        self.rows
            .iter()
            .position(|row| row.is_selectable() && row.path == selected)
    }

    /// Moves the selection by one selectable row.
    fn select_step(&mut self, step: Step) {
        let Some(current) = self.selected_index() else {
            self.selected = self.first_selectable();
            return;
        };
        let next = match step {
            Step::Next => self
                .rows
                .iter()
                .skip(current + 1)
                .find(|row| row.is_selectable()),
            Step::Previous => self.rows[..current]
                .iter()
                .rev()
                .find(|row| row.is_selectable()),
        };
        if let Some(row) = next {
            self.selected = Some(row.path.clone());
        }
    }

    /// Returns the path of the first selectable row.
    fn first_selectable(&self) -> Option<PathBuf> {
        self.rows
            .iter()
            .find(|row| row.is_selectable())
            .map(|row| row.path.clone())
    }

    /// Reports whether the active search asked for one directory listing.
    fn searched(&self, path: &Path) -> bool {
        self.search
            .as_ref()
            .is_some_and(|search| search.read.contains(path))
    }

    /// Opens every loaded directory that holds a match of the active search.
    ///
    /// A match may sit below a closed directory, so the search opens that
    /// directory and every directory above it. The bound on the holders bounds
    /// the expansion work of one search.
    fn open_matched_directories(&mut self) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let mut holders: Vec<PathBuf> = Vec::new();
        for (directory, state) in &self.directories {
            if holders.len() >= TREE_SEARCH_MATCHES_MAX {
                break;
            }
            let DirectoryState::Listed { entries, .. } = state else {
                continue;
            };
            let holds = entries.iter().any(|entry| {
                keeps(self.hidden, &entry.name) && name_match(&entry.name, &search.query).is_some()
            });
            if holds {
                holders.push(directory.clone());
            }
        }
        for holder in holders {
            self.open_for_search(&holder);
        }
    }

    /// Opens one directory and every directory above it for the search.
    ///
    /// The call never replaces the owner of a directory that the user opened,
    /// so the end of the search keeps that directory open.
    fn open_for_search(&mut self, directory: &Path) {
        let Ok(relative) = directory.strip_prefix(&self.root) else {
            return;
        };
        let mut path = self.root.clone();
        for component in relative.components() {
            path.push(component);
            if !self.accepts(&path) {
                return;
            }
            self.expanded.entry(path.clone()).or_insert(Opened::Search);
        }
    }

    /// Asks for the listings that the active search still needs.
    ///
    /// A match may sit inside a directory that the tree never listed, so the
    /// search reads the directories of every loaded listing. The read set grows
    /// one listing at a time and stops at [`TREE_SEARCH_READS_MAX`], so one
    /// search never walks a complete workspace.
    fn queue_search_reads(&mut self) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        if search.read.len() >= TREE_SEARCH_READS_MAX {
            return;
        }
        let mut wanted: Vec<PathBuf> = Vec::new();
        for (directory, state) in &self.directories {
            let DirectoryState::Listed { entries, .. } = state else {
                continue;
            };
            for entry in entries {
                if entry.kind != EntryKind::Directory || !keeps(self.hidden, &entry.name) {
                    continue;
                }
                let path = directory.join(&entry.name);
                if self.directories.contains_key(&path)
                    || search.read.contains(&path)
                    || self.pending.iter().any(|queued| *queued == path)
                    || !self.accepts(&path)
                {
                    continue;
                }
                wanted.push(path);
            }
        }
        let Some(search) = self.search.as_mut() else {
            debug_assert!(false, "the guard above returned without an active search");
            return;
        };
        for path in wanted {
            if search.read.len() >= TREE_SEARCH_READS_MAX
                || self.pending.len() >= TREE_PENDING_READS_MAX
            {
                return;
            }
            search.read.insert(path.clone());
            self.pending.push_back(path);
        }
    }

    /// Marks the rows whose name holds the query of the active search.
    fn mark_matches(&self, rows: &mut [TreeRow]) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let mut found = 0usize;
        for row in rows {
            if found >= TREE_SEARCH_MATCHES_MAX {
                return;
            }
            let Some(matched) = row.name().and_then(|name| name_match(name, &search.query)) else {
                continue;
            };
            row.matched = Some(matched);
            found += 1;
        }
    }

    /// Reports whether one path may hold loaded state.
    fn accepts(&self, path: &Path) -> bool {
        self.depth_of(path)
            .is_some_and(|depth| depth < TREE_DEPTH_MAX)
    }

    /// Returns the number of directories between the root and one path.
    fn depth_of(&self, path: &Path) -> Option<usize> {
        path.strip_prefix(&self.root)
            .ok()
            .map(|relative| relative.components().count())
    }

    /// Adds one directory to the bounded read queue.
    fn push_pending(&mut self, path: &Path) {
        if self.pending.len() >= TREE_PENDING_READS_MAX || self.pending.iter().any(|it| it == path)
        {
            return;
        }
        self.pending.push_back(path.to_path_buf());
    }

    /// Returns the loaded state of every entry that one new listing removed.
    fn stale_descendants(&self, directory: &Path, names: &BTreeSet<&str>) -> Vec<PathBuf> {
        self.expanded
            .keys()
            .chain(self.directories.keys())
            .filter(|path| path.as_path() != directory && path.starts_with(directory))
            .filter(|path| {
                let Ok(relative) = path.strip_prefix(directory) else {
                    return false;
                };
                relative
                    .components()
                    .next()
                    .and_then(|first| first.as_os_str().to_str())
                    .is_none_or(|first| !names.contains(first))
            })
            .cloned()
            .collect()
    }

    /// Truncates one listing to the free capacity of the tree.
    fn fit_entries(&self, directory: &Path, mut entries: Vec<TreeEntry>) -> Vec<TreeEntry> {
        let loaded: usize = self
            .directories
            .iter()
            .filter(|(path, _)| path.as_path() != directory)
            .map(|(_, state)| match state {
                DirectoryState::Listed { entries, .. } => entries.len(),
                DirectoryState::Unreadable => 0,
            })
            .sum();
        let free = TREE_ENTRIES_MAX.saturating_sub(loaded);
        entries.truncate(free);
        entries
    }

    /// Rebuilds the visible rows and reconciles the selection.
    fn rebuild(&mut self) {
        // A search may open one directory, so it runs before the rows exist.
        self.open_matched_directories();
        self.queue_search_reads();

        let mut rows = Vec::new();
        self.collect_rows(&self.root.clone(), 0, &mut rows);
        self.mark_matches(&mut rows);
        self.rows = rows;

        if let Some(target) = self.reveal.clone()
            && self.rows.iter().any(|row| row.path == target)
        {
            self.selected = Some(target);
            self.reveal = None;
        }
        if self.selected_index().is_some() {
            return;
        }
        // The selected entry disappeared. Keep the closest visible ancestor, so
        // a refresh after a deletion never jumps to an unrelated row.
        let ancestor = self.selected.as_deref().and_then(|selected| {
            selected
                .ancestors()
                .skip(1)
                .find(|path| {
                    self.rows
                        .iter()
                        .any(|row| row.is_selectable() && row.path == *path)
                })
                .map(Path::to_path_buf)
        });
        self.selected = ancestor.or_else(|| self.first_selectable());
    }

    /// Appends the rows of one expanded directory.
    ///
    /// Every entry that the hidden policy keeps becomes one row. A search marks
    /// the matching rows instead of removing the others, so the visible tree
    /// stays the tree that the reader navigated.
    fn collect_rows(&self, directory: &Path, depth: usize, rows: &mut Vec<TreeRow>) {
        debug_assert!(
            depth <= TREE_DEPTH_MAX,
            "expand refuses a directory at or below the depth bound"
        );
        let state = self.directories.get(directory);
        let entries = match state {
            Some(DirectoryState::Listed { entries, .. }) => entries.as_slice(),
            Some(DirectoryState::Unreadable) | None => &[],
        };

        for entry in entries {
            if !keeps(self.hidden, &entry.name) {
                continue;
            }
            let path = directory.join(&entry.name);
            let content = match entry.kind {
                EntryKind::File => RowContent::File {
                    name: entry.name.clone(),
                    link: entry.link,
                },
                EntryKind::Directory => RowContent::Directory {
                    name: entry.name.clone(),
                    link: entry.link,
                    expansion: self.expansion_of(&path),
                },
            };
            let open = matches!(
                content,
                RowContent::Directory {
                    expansion: Expansion::Expanded,
                    ..
                }
            );
            rows.push(TreeRow {
                path: path.clone(),
                depth,
                content,
                matched: None,
            });
            if open {
                self.collect_rows(&path, depth + 1, rows);
            }
        }

        // The notice reports a bounded or failed read instead of showing a
        // partial directory without a reason.
        let notice = match state {
            Some(DirectoryState::Unreadable) => Some(Notice::Unreadable),
            Some(DirectoryState::Listed {
                truncation: Truncation::Truncated { shown, total },
                ..
            }) => Some(Notice::Truncated {
                shown: *shown,
                total: *total,
            }),
            _ => None,
        };
        if let Some(notice) = notice {
            rows.push(TreeRow {
                path: directory.to_path_buf(),
                depth,
                content: RowContent::Notice(notice),
                matched: None,
            });
        }
    }

    /// Returns the expansion state of one directory path.
    fn expansion_of(&self, path: &Path) -> Expansion {
        if !self.expanded.contains_key(path) {
            return Expansion::Collapsed;
        }
        if self.directories.contains_key(path) {
            Expansion::Expanded
        } else {
            Expansion::Pending
        }
    }
}

/// Returns the characters of one name that hold the query.
///
/// The comparison runs over characters, never over bytes, and it ignores the
/// case, exactly as the buffer search does. The result therefore indexes the
/// characters of the original name.
fn name_match(name: &str, query: &str) -> Option<NameMatch> {
    debug_assert!(
        !query.is_empty(),
        "an empty query ends the search instead of matching every name"
    );
    let needle: Vec<char> = query.chars().collect();
    let haystack: Vec<char> = name.chars().collect();
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|start| {
            haystack[*start..]
                .iter()
                .zip(&needle)
                .all(|(left, right)| left == right || left.to_lowercase().eq(right.to_lowercase()))
        })
        .map(|start| NameMatch {
            start,
            len: needle.len(),
        })
}

/// The direction of one selection move.
enum Step {
    Next,
    Previous,
}
