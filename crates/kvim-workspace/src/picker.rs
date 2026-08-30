//! The workspace picker: the file, search, and buffer vocabulary over the
//! domain-neutral selector of `kvim-ui`.
//!
//! One picker framework serves the file search, the ripgrep search, and the
//! buffer search. The three differ only in the source of their candidates and
//! in what one accepted row opens. [`Picker`] keeps the candidate list, the
//! accepted target, and the preview. Its query, its ranking, its match list,
//! and its selection live inside one `kvim_ui::Selector`. Every function of
//! this file is a pure transition over already collected candidates, so the
//! terminal event loop runs no filesystem and no process work. See
//! `docs/files.md`.

use std::path::{Path, PathBuf};

use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_ui::{ListMotion, Selector, SelectorCandidate};

use super::buffer::BufferId;

/// The largest number of candidates that one picker keeps.
///
/// One repository holds more files than a reader ever inspects. The bound keeps
/// one keystroke inside the latency budget of `docs/responsiveness.md`. The
/// value equals `kvim_ui::SELECTOR_CANDIDATES_MAX`, because the picker holds
/// its candidates inside one `Selector`.
pub const PICKER_CANDIDATES_MAX: usize = kvim_ui::SELECTOR_CANDIDATES_MAX;

/// The largest number of characters that one picker query holds.
///
/// The value equals `kvim_ui::SELECTOR_QUERY_CHARS_MAX`, because the picker
/// forwards its effective query to one `Selector`.
pub const PICKER_QUERY_CHARS_MAX: usize = kvim_ui::SELECTOR_QUERY_CHARS_MAX;

/// The largest number of characters that one matched line shows.
pub const PICKER_MATCH_CHARS_MAX: usize = 160;

/// The number of cells between two columns of one result row.
const COLUMN_GAP: &str = "  ";

/// The source of the candidates of one picker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PickerKind {
    /// Every file of the workspace, without the ignored files.
    Files,
    /// Every line of the workspace that ripgrep matched.
    Search,
    /// Every loaded buffer.
    Buffers,
}

impl PickerKind {
    /// Returns the title that the picker shows above its query.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Search => "Search",
            Self::Buffers => "Buffers",
        }
    }
}

/// What one accepted picker row opens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CandidateTarget {
    /// One workspace file.
    File {
        /// The absolute path of the file.
        path: PathBuf,
    },
    /// One matched line of one workspace file.
    Match {
        /// The absolute path of the file.
        path: PathBuf,
        /// The zero-based line of the match.
        line: usize,
        /// The zero-based byte column of the match.
        byte_column: usize,
        /// The text of the matched line, bounded by [`PICKER_MATCH_CHARS_MAX`].
        text: String,
    },
    /// One loaded buffer.
    Buffer {
        /// The identity of the buffer.
        buffer: BufferId,
    },
}

/// The line that one preview marks.
///
/// Only a search row names one matched line. A file row shows the start of its
/// file, so the preview marks no line at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewTarget {
    /// Show the start of the file and mark no line.
    Start,
    /// Show the region around the matched line and mark it.
    Match {
        /// The zero-based line of the match.
        line: usize,
    },
}

impl PreviewTarget {
    /// Returns the zero-based line that the preview shows.
    #[must_use]
    pub const fn line(self) -> usize {
        match self {
            Self::Start => 0,
            Self::Match { line } => line,
        }
    }

    /// Reports whether the preview marks the line.
    #[must_use]
    pub const fn marks(self, line: usize) -> bool {
        match self {
            Self::Start => false,
            Self::Match { line: marked } => marked == line,
        }
    }
}

/// What the editor does with one accepted picker row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Acceptance {
    /// Open one file and place the cursor at one position.
    OpenFile {
        /// The absolute path of the file.
        path: PathBuf,
        /// The zero-based line that receives the cursor.
        line: usize,
        /// The zero-based byte column that receives the cursor.
        byte_column: usize,
    },
    /// Show one loaded buffer in the focused window.
    ShowBuffer {
        /// The identity of the buffer.
        buffer: BufferId,
    },
}

/// One row of one picker.
///
/// The row shows the filename first and its directory after it, because the
/// reader searches for a name far more often than for a directory. See
/// `docs/files.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    /// The filename, or the buffer name.
    name: String,
    /// The directory of the file, relative to the workspace root.
    directory: String,
    /// The validated worktree path used by a file preview.
    relative: Option<WorktreeRelativePath>,
    /// What the accepted row opens.
    target: CandidateTarget,
}

impl Candidate {
    /// Creates one candidate for one workspace file.
    #[must_use]
    pub fn file(root: &WorktreeRoot, relative: WorktreeRelativePath) -> Self {
        let path = root.as_path().join(relative.as_path());
        let (name, directory) = split_path(root.as_path(), &path);
        Self {
            name,
            directory,
            relative: Some(relative),
            target: CandidateTarget::File { path },
        }
    }

    /// Creates one candidate for one matched line.
    ///
    /// The line and the column are zero-based, as every editor position is.
    #[must_use]
    pub fn matched(
        root: &WorktreeRoot,
        relative: WorktreeRelativePath,
        line: usize,
        byte_column: usize,
        text: &str,
    ) -> Self {
        let path = root.as_path().join(relative.as_path());
        let (name, directory) = split_path(root.as_path(), &path);
        Self {
            name,
            directory,
            relative: Some(relative),
            target: CandidateTarget::Match {
                path,
                line,
                byte_column,
                text: clip(text.trim(), PICKER_MATCH_CHARS_MAX),
            },
        }
    }

    /// Creates one candidate for one loaded buffer.
    #[must_use]
    pub fn buffer(root: &Path, buffer: BufferId, path: Option<&Path>, name: &str) -> Self {
        let directory = path.map_or_else(String::new, |path| split_path(root, path).1);
        Self {
            name: name.to_owned(),
            directory,
            relative: None,
            target: CandidateTarget::Buffer { buffer },
        }
    }

    /// Returns the filename of the row.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the directory of the row, or an empty text.
    #[must_use]
    pub fn directory(&self) -> &str {
        &self.directory
    }

    /// Returns the path of the row, relative to the workspace root.
    ///
    /// The row splits one relative path into its directory and its filename,
    /// and this function joins the two again. A row of a buffer without a file
    /// therefore returns its buffer name alone.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use kvim_path::{WorktreeRelativePath, WorktreeRoot};
    /// use kvim_workspace::Candidate;
    ///
    /// let root = WorktreeRoot::open(std::env::current_dir()?)?;
    /// let candidate = Candidate::file(
    ///     &root,
    ///     WorktreeRelativePath::new("src/main.rs")?,
    /// );
    /// assert_eq!(candidate.relative_path(), Path::new("src").join("main.rs"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn relative_path(&self) -> PathBuf {
        Path::new(&self.directory).join(&self.name)
    }

    /// Returns what the accepted row opens.
    #[must_use]
    pub const fn target(&self) -> &CandidateTarget {
        &self.target
    }

    /// Returns the text of the row, with the filename first.
    #[must_use]
    pub fn row(&self) -> String {
        let mut row = match &self.target {
            CandidateTarget::Match { line, .. } => {
                // The reader counts lines from one, so the row shows the human
                // form of the zero-based position.
                format!("{}:{}", self.name, line.saturating_add(1))
            }
            CandidateTarget::Buffer { .. } | CandidateTarget::File { .. } => self.name.clone(),
        };
        if !self.directory.is_empty() {
            row.push_str(COLUMN_GAP);
            row.push_str(&self.directory);
        }
        if let CandidateTarget::Match { text, .. } = &self.target
            && !text.is_empty()
        {
            row.push_str(COLUMN_GAP);
            row.push_str(text);
        }
        row
    }

    /// Returns the file and the region that the preview of this row shows.
    #[must_use]
    pub fn preview(&self) -> Option<(&WorktreeRelativePath, &Path, PreviewTarget)> {
        let relative = self.relative.as_ref()?;
        match &self.target {
            CandidateTarget::File { path } => {
                Some((relative, path.as_path(), PreviewTarget::Start))
            }
            CandidateTarget::Match { path, line, .. } => Some((
                relative,
                path.as_path(),
                PreviewTarget::Match { line: *line },
            )),
            // A loaded buffer already holds its text, so it needs no file read.
            CandidateTarget::Buffer { .. } => None,
        }
    }

    /// Returns what the editor does with this row.
    #[must_use]
    pub fn acceptance(&self) -> Acceptance {
        match &self.target {
            CandidateTarget::File { path } => Acceptance::OpenFile {
                path: path.clone(),
                line: 0,
                byte_column: 0,
            },
            CandidateTarget::Match {
                path,
                line,
                byte_column,
                ..
            } => Acceptance::OpenFile {
                path: path.clone(),
                line: *line,
                byte_column: *byte_column,
            },
            CandidateTarget::Buffer { buffer } => Acceptance::ShowBuffer { buffer: *buffer },
        }
    }
}

/// Returns the filename and the directory of one path below the root.
///
/// A path outside the root keeps its complete directory, so the reader still
/// sees where the file lives.
fn split_path(root: &Path, path: &Path) -> (String, String) {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let name = relative.file_name().map_or_else(
        || relative.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let directory = relative
        .parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_default();
    (name, directory)
}

/// Returns the first characters of one text.
fn clip(text: &str, chars_max: usize) -> String {
    text.chars().take(chars_max).collect()
}

/// The bounded picker: the file, search, and buffer vocabulary over one
/// `kvim_ui::Selector`.
///
/// The picker keeps its own candidate list, because a candidate names a path,
/// a matched line, or a buffer identity that the selector never names. The
/// query, the ranking, the match list, and the selection all live inside the
/// selector.
///
/// # Examples
///
/// ```
/// use kvim_path::{WorktreeRelativePath, WorktreeRoot};
/// use kvim_workspace::{Candidate, Picker, PickerKind};
///
/// let root = WorktreeRoot::open(std::env::current_dir()?)?;
/// let mut picker = Picker::new(PickerKind::Files, root.as_path().to_path_buf());
/// picker.set_candidates(
///     vec![
///         Candidate::file(&root, WorktreeRelativePath::new("src/main.rs")?),
///         Candidate::file(&root, WorktreeRelativePath::new("README.md")?),
///     ],
///     false,
/// );
///
/// // The best match sits at the top, next to the prompt.
/// picker.set_query("main");
/// let selected = picker.selected().expect("one candidate holds the query");
/// assert_eq!(selected.name(), "main.rs");
/// assert_eq!(selected.directory(), "src");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Picker {
    kind: PickerKind,
    root: PathBuf,
    /// The text that the reader typed into the prompt.
    ///
    /// [`Picker::query`] always returns this text, even for
    /// [`PickerKind::Search`]. [`Picker::effective_query`] decides what the
    /// selector sees.
    query: String,
    candidates: Vec<Candidate>,
    /// The query, the ranking, the match list, and the selection.
    ///
    /// The selector holds one [`SelectorCandidate<usize>`] for each candidate
    /// of `candidates`, at the same position, with the position itself as the
    /// host identity. A match or a selection therefore names one position of
    /// `candidates` directly.
    selector: Selector<usize>,
}

impl Picker {
    /// Creates one empty picker over one workspace root.
    #[must_use]
    pub fn new(kind: PickerKind, root: PathBuf) -> Self {
        Self {
            kind,
            root,
            query: String::new(),
            candidates: Vec::new(),
            selector: Selector::default(),
        }
    }

    /// Returns the source of the candidates.
    #[must_use]
    pub const fn kind(&self) -> PickerKind {
        self.kind
    }

    /// Returns the workspace root of the picker.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the query that the prompt holds.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Reports whether the source found more candidates than the bound keeps.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.selector.is_truncated()
    }

    /// Replaces the candidates and ranks them again.
    ///
    /// The list stops at [`PICKER_CANDIDATES_MAX`], and a longer list reports
    /// the truncation.
    pub fn set_candidates(&mut self, candidates: Vec<Candidate>, truncated: bool) {
        let truncated = truncated || candidates.len() > PICKER_CANDIDATES_MAX;
        self.candidates = candidates;
        self.candidates.truncate(PICKER_CANDIDATES_MAX);
        let selector_candidates = self
            .candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                SelectorCandidate::new(index, candidate.name.clone(), candidate.directory.clone())
            })
            .collect();
        self.selector.set_candidates(selector_candidates, truncated);
    }

    /// Replaces the query and ranks the candidates again.
    ///
    /// The query stops at [`PICKER_QUERY_CHARS_MAX`] characters.
    pub fn set_query(&mut self, query: &str) {
        self.query = clip(query, PICKER_QUERY_CHARS_MAX);
        self.selector
            .set_query(Self::effective_query(self.kind, &self.query));
    }

    /// Returns the candidate indexes that the query keeps, with the best first.
    #[must_use]
    pub fn matches(&self) -> &[usize] {
        self.selector.matches()
    }

    /// Returns one candidate by its index.
    #[must_use]
    pub fn candidate(&self, index: usize) -> Option<&Candidate> {
        self.candidates.get(index)
    }

    /// Returns the selected candidate, or `None` while no row matches.
    #[must_use]
    pub fn selected(&self) -> Option<&Candidate> {
        let index = *self.selector.selected()?.id();
        self.candidates.get(index)
    }

    /// Returns the position of the selected row inside [`Picker::matches`].
    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        self.selector.selected_row()
    }

    /// Selects one ranked row directly.
    pub fn select_row(&mut self, row: usize) {
        self.selector.apply_motion(ListMotion::ToRow(row));
    }

    /// Moves the selection one row toward the end of the list.
    ///
    /// The list ends at both edges, because a wrap would move the reader past
    /// the best match without a key that says so.
    pub fn select_next(&mut self) {
        self.selector.select_next();
    }

    /// Moves the selection one row toward the prompt.
    pub fn select_previous(&mut self) {
        self.selector.select_previous();
    }

    /// Returns what the editor does with the selected row.
    #[must_use]
    pub fn accept(&self) -> Option<Acceptance> {
        self.selected().map(Candidate::acceptance)
    }

    /// Returns the query that the selector ranks against, for one kind.
    ///
    /// The search picker sends its query to `rg`, so its rows already answer
    /// that query. A second filter over the filenames would drop every matched
    /// line whose filename does not hold the pattern, so the selector sees an
    /// empty query while [`Picker::query`] still returns the typed text.
    fn effective_query(kind: PickerKind, query: &str) -> &str {
        match kind {
            PickerKind::Search => "",
            PickerKind::Files | PickerKind::Buffers => query,
        }
    }
}

#[cfg(test)]
#[path = "picker_tests.rs"]
mod tests;
