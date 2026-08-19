//! The bounded picker model: candidates, the query, the ranking, and the
//! selection.
//!
//! One picker framework serves the file search, the ripgrep search, and the
//! buffer search. The three differ only in the source of their candidates and
//! in what one accepted row opens. Every function of this file is a pure
//! transition over already collected candidates, so the terminal event loop
//! runs no filesystem and no process work. See `docs/files.md`.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use super::buffer::BufferId;
use super::fuzzy::score_candidate;

/// The largest number of candidates that one picker keeps.
///
/// One repository holds more files than a reader ever inspects. The bound keeps
/// one keystroke inside the latency budget of `docs/responsiveness.md`.
pub const PICKER_CANDIDATES_MAX: usize = 4096;

/// The largest number of characters that one picker query holds.
pub const PICKER_QUERY_CHARS_MAX: usize = 128;

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
    /// What the accepted row opens.
    target: CandidateTarget,
}

impl Candidate {
    /// Creates one candidate for one workspace file.
    #[must_use]
    pub fn file(root: &Path, path: PathBuf) -> Self {
        let (name, directory) = split_path(root, &path);
        Self {
            name,
            directory,
            target: CandidateTarget::File { path },
        }
    }

    /// Creates one candidate for one matched line.
    ///
    /// The line and the column are zero-based, as every editor position is.
    #[must_use]
    pub fn matched(
        root: &Path,
        path: PathBuf,
        line: usize,
        byte_column: usize,
        text: &str,
    ) -> Self {
        let (name, directory) = split_path(root, &path);
        Self {
            name,
            directory,
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
    /// use std::path::{Path, PathBuf};
    ///
    /// use kvim_workspace::Candidate;
    ///
    /// let root = Path::new("/workspace");
    /// let candidate = Candidate::file(root, PathBuf::from("/workspace/src/main.rs"));
    /// assert_eq!(candidate.relative_path(), Path::new("src").join("main.rs"));
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
    pub fn preview(&self) -> Option<(&Path, PreviewTarget)> {
        match &self.target {
            CandidateTarget::File { path } => Some((path.as_path(), PreviewTarget::Start)),
            CandidateTarget::Match { path, line, .. } => {
                Some((path.as_path(), PreviewTarget::Match { line: *line }))
            }
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

    /// Returns the number of characters that the ranking compares.
    fn width(&self) -> usize {
        self.name
            .chars()
            .count()
            .saturating_add(self.directory.chars().count())
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

/// The direction of one selection move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    /// Move away from the prompt.
    Next,
    /// Move toward the prompt.
    Previous,
}

/// The bounded picker: one query, one candidate list, and one stable selection.
///
/// # Examples
///
/// ```
/// use std::path::{Path, PathBuf};
///
/// use kvim_workspace::{Candidate, Picker, PickerKind};
///
/// let root = Path::new("/workspace");
/// let mut picker = Picker::new(PickerKind::Files, root.to_path_buf());
/// picker.set_candidates(
///     vec![
///         Candidate::file(root, PathBuf::from("/workspace/src/main.rs")),
///         Candidate::file(root, PathBuf::from("/workspace/README.md")),
///     ],
///     false,
/// );
///
/// // The best match sits at the top, next to the prompt.
/// picker.set_query("main");
/// let selected = picker.selected().expect("one candidate holds the query");
/// assert_eq!(selected.name(), "main.rs");
/// assert_eq!(selected.directory(), "src");
/// ```
#[derive(Debug)]
pub struct Picker {
    kind: PickerKind,
    root: PathBuf,
    query: String,
    candidates: Vec<Candidate>,
    /// The candidate indexes that the query keeps, with the best row first.
    matches: Vec<usize>,
    /// The candidate index of the selected row.
    ///
    /// The picker keeps the selected candidate across one refiltering while the
    /// query still matches it, so the selection never jumps under the reader.
    selected: Option<usize>,
    /// Reports whether the source found more candidates than the bound keeps.
    truncated: bool,
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
            matches: Vec::new(),
            selected: None,
            truncated: false,
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
        self.truncated
    }

    /// Replaces the candidates and ranks them again.
    ///
    /// The list stops at [`PICKER_CANDIDATES_MAX`], and a longer list reports
    /// the truncation.
    pub fn set_candidates(&mut self, candidates: Vec<Candidate>, truncated: bool) {
        self.truncated = truncated || candidates.len() > PICKER_CANDIDATES_MAX;
        self.candidates = candidates;
        self.candidates.truncate(PICKER_CANDIDATES_MAX);
        self.refilter();
    }

    /// Replaces the query and ranks the candidates again.
    ///
    /// The query stops at [`PICKER_QUERY_CHARS_MAX`] characters.
    pub fn set_query(&mut self, query: &str) {
        self.query = clip(query, PICKER_QUERY_CHARS_MAX);
        self.refilter();
    }

    /// Returns the candidate indexes that the query keeps, with the best first.
    #[must_use]
    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    /// Returns one candidate by its index.
    #[must_use]
    pub fn candidate(&self, index: usize) -> Option<&Candidate> {
        self.candidates.get(index)
    }

    /// Returns the selected candidate, or `None` while no row matches.
    #[must_use]
    pub fn selected(&self) -> Option<&Candidate> {
        self.candidates.get(self.selected?)
    }

    /// Returns the position of the selected row inside [`Picker::matches`].
    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        let selected = self.selected?;
        self.matches.iter().position(|index| *index == selected)
    }

    /// Moves the selection one row toward the end of the list.
    ///
    /// The list ends at both edges, because a wrap would move the reader past
    /// the best match without a key that says so.
    pub fn select_next(&mut self) {
        self.select(Step::Next);
    }

    /// Moves the selection one row toward the prompt.
    pub fn select_previous(&mut self) {
        self.select(Step::Previous);
    }

    /// Returns what the editor does with the selected row.
    #[must_use]
    pub fn accept(&self) -> Option<Acceptance> {
        self.selected().map(Candidate::acceptance)
    }

    /// Moves the selection by one step inside the matched rows.
    fn select(&mut self, step: Step) {
        let Some(row) = self.selected_row() else {
            self.selected = self.matches.first().copied();
            return;
        };
        let last = self.matches.len().saturating_sub(1);
        let next = match step {
            Step::Previous => row.saturating_sub(1),
            Step::Next => row.saturating_add(1).min(last),
        };
        self.selected = self.matches.get(next).copied();
    }

    /// Ranks every candidate against the query and keeps the selection.
    ///
    /// The search picker sends its query to `rg`, so its rows already answer
    /// that query. A second filter over the filenames would drop every matched
    /// line whose filename does not hold the pattern.
    fn refilter(&mut self) {
        let query = match self.kind {
            PickerKind::Search => "",
            PickerKind::Files | PickerKind::Buffers => self.query.as_str(),
        };
        self.matches = rank_candidates(query, &self.candidates);
        // The selection follows its candidate while the query still keeps it,
        // so a further character never moves the reader to another row.
        if !self
            .selected
            .is_some_and(|selected| self.matches.contains(&selected))
        {
            self.selected = self.matches.first().copied();
        }
    }
}

/// Returns the indexes of the candidates that `query` keeps, with the best
/// first.
///
/// The function is pure, so one query and one candidate list always produce one
/// order. The picker ranks its rows with it, and the command-line completion
/// ranks the path argument of `:e` with it, so one ranking rule serves both.
/// See `docs/files.md`.
///
/// The order is total, so two equal queries always produce one order:
///
/// 1. the higher score first,
/// 2. then the shorter row,
/// 3. then the earlier candidate of the source.
///
/// An empty query keeps every candidate and the order of the source, because
/// every candidate then holds the same score. The query stops at
/// [`PICKER_QUERY_CHARS_MAX`] characters.
///
/// # Examples
///
/// ```
/// use std::path::{Path, PathBuf};
///
/// use kvim_workspace::{Candidate, rank_candidates};
///
/// let root = Path::new("/workspace");
/// let candidates = [
///     Candidate::file(root, PathBuf::from("/workspace/src/session.rs")),
///     Candidate::file(root, PathBuf::from("/workspace/src/main.rs")),
/// ];
/// assert_eq!(rank_candidates("main", &candidates), [1]);
/// assert_eq!(rank_candidates("", &candidates), [0, 1]);
/// ```
#[must_use]
pub fn rank_candidates(query: &str, candidates: &[Candidate]) -> Vec<usize> {
    let query = clip(query, PICKER_QUERY_CHARS_MAX);
    let mut scored: Vec<(usize, i32)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let score = score_candidate(&query, &candidate.name, &candidate.directory)?;
            Some((index, score))
        })
        .collect();
    if !query.is_empty() {
        scored.sort_by_key(|(index, score)| {
            let width = candidates[*index].width();
            (Reverse(*score), width, *index)
        });
    }
    scored.into_iter().map(|(index, _)| index).collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Acceptance, BufferId, Candidate, Picker, PickerKind, PreviewTarget};

    /// The workspace root of every test picker.
    fn root() -> PathBuf {
        PathBuf::from("/workspace")
    }

    fn picker(names: &[&str]) -> Picker {
        let root = root();
        let mut picker = Picker::new(PickerKind::Files, root.clone());
        let candidates = names
            .iter()
            .map(|name| Candidate::file(&root, root.join(name)))
            .collect();
        picker.set_candidates(candidates, false);
        picker
    }

    fn rows(picker: &Picker) -> Vec<String> {
        picker
            .matches()
            .iter()
            .filter_map(|index| picker.candidate(*index))
            .map(Candidate::row)
            .collect()
    }

    #[test]
    fn the_row_shows_the_filename_before_its_directory() {
        let candidate = Candidate::file(&root(), root().join("src/tui/picker.rs"));
        assert_eq!(candidate.row(), "picker.rs  src/tui");
        assert_eq!(candidate.name(), "picker.rs");
        assert_eq!(candidate.directory(), "src/tui");
    }

    #[test]
    fn a_matched_row_shows_the_line_and_its_text() {
        let candidate =
            Candidate::matched(&root(), root().join("src/main.rs"), 41, 4, "  fn main()");
        assert_eq!(candidate.row(), "main.rs:42  src  fn main()");
        assert_eq!(
            candidate.acceptance(),
            Acceptance::OpenFile {
                path: root().join("src/main.rs"),
                line: 41,
                byte_column: 4,
            }
        );
    }

    #[test]
    fn an_empty_query_keeps_the_order_of_the_source() {
        let picker = picker(&["src/zebra.rs", "alpha.rs", "src/main.rs"]);
        assert_eq!(
            rows(&picker),
            vec!["zebra.rs  src", "alpha.rs", "main.rs  src"]
        );
    }

    #[test]
    fn the_best_match_sits_at_the_top_of_the_list() {
        let mut picker = picker(&["src/domain.rs", "src/main.rs", "docs/manual.md"]);
        picker.set_query("main");
        assert_eq!(
            rows(&picker).first().map(String::as_str),
            Some("main.rs  src")
        );
    }

    #[test]
    fn two_equal_scores_keep_one_deterministic_order() {
        // Both names hold the query at the same positions, so the shorter row
        // wins, and the earlier candidate wins an equal width.
        let mut picker = picker(&["ab_long_name.rs", "ab.rs", "ab2.rs"]);
        picker.set_query("ab");
        let first = rows(&picker);
        picker.set_query("");
        picker.set_query("ab");
        assert_eq!(first, rows(&picker), "one query always produces one order");
        assert_eq!(first.first().map(String::as_str), Some("ab.rs"));
    }

    #[test]
    fn the_selection_follows_its_candidate_across_one_refiltering() {
        let mut picker = picker(&["src/main.rs", "src/mode.rs", "src/motion.rs"]);
        picker.set_query("mo");
        picker.select_next();
        let selected = picker
            .selected()
            .expect("the query keeps three rows")
            .name()
            .to_owned();
        picker.set_query("mot");
        assert_eq!(
            picker.selected().map(Candidate::name),
            Some(selected.as_str()),
            "the selected row still matches, so it stays selected"
        );
    }

    #[test]
    fn a_selection_that_the_query_drops_returns_to_the_best_row() {
        let mut picker = picker(&["src/main.rs", "src/mode.rs"]);
        picker.set_query("m");
        picker.select_next();
        assert_eq!(picker.selected().map(Candidate::name), Some("mode.rs"));
        picker.set_query("main");
        assert_eq!(picker.selected().map(Candidate::name), Some("main.rs"));
    }

    #[test]
    fn the_selection_stops_at_both_ends_of_the_list() {
        let mut picker = picker(&["a.rs", "b.rs"]);
        picker.select_previous();
        assert_eq!(picker.selected_row(), Some(0));
        picker.select_next();
        picker.select_next();
        picker.select_next();
        assert_eq!(picker.selected_row(), Some(1));
    }

    #[test]
    fn a_candidate_list_above_the_bound_reports_the_truncation() {
        let root = root();
        let mut picker = Picker::new(PickerKind::Files, root.clone());
        let candidates = (0..super::PICKER_CANDIDATES_MAX + 8)
            .map(|index| Candidate::file(&root, root.join(format!("file{index}.rs"))))
            .collect();
        picker.set_candidates(candidates, false);
        assert!(picker.is_truncated());
        assert_eq!(picker.matches().len(), super::PICKER_CANDIDATES_MAX);
    }

    #[test]
    fn the_query_stops_at_the_character_bound() {
        let mut picker = picker(&["a.rs"]);
        picker.set_query(&"x".repeat(super::PICKER_QUERY_CHARS_MAX + 32));
        assert_eq!(
            picker.query().chars().count(),
            super::PICKER_QUERY_CHARS_MAX
        );
    }

    #[test]
    fn a_file_candidate_previews_the_start_and_marks_no_line() {
        let candidate = Candidate::file(&root(), root().join("a.rs"));
        let (path, target) = candidate.preview().expect("a file row shows a preview");
        assert_eq!(path, root().join("a.rs"));
        assert_eq!(target, PreviewTarget::Start);
        assert!(!target.marks(0), "a file row marks no line");

        let matched = Candidate::matched(&root(), root().join("a.rs"), 4, 0, "text");
        let (_, target) = matched.preview().expect("a match row shows a preview");
        assert!(target.marks(4), "a match row marks its own line");
    }

    #[test]
    fn a_buffer_candidate_needs_no_file_read() {
        let candidate = Candidate::buffer(
            &root(),
            BufferId::new(3),
            Some(Path::new("/workspace/src/main.rs")),
            "main.rs",
        );
        assert_eq!(candidate.preview(), None);
        assert_eq!(candidate.row(), "main.rs  src");
    }
}
