//! The domain-neutral selector: a bounded query, a ranked candidate list, and
//! a selection that survives a refiltering.
//!
//! A host narrows its own kind of list with one query: a command, a branch, a
//! setting, or another entry of its own vocabulary. [`Selector<R>`] is one
//! bounded mechanism for every such list. It ranks a name against a container
//! string through `kvim-fuzzy`, and it keeps the identity of the selected
//! candidate while the query still matches it. It names no path, no buffer,
//! and no file, so any host that ranks a list of its own values can hold it.
//! See `docs/windows.md`.
//!
//! The selector also holds one [`ListViewport`] over its matched rows. A host
//! reads [`Selector::placements`] to paint a bounded list without computing an
//! offset of its own, the same way [`SidebarState`](crate::SidebarState) reads
//! its own placements. See `docs/windows.md`.
//!
//! The module is pure. It reads no clock, no filesystem, and no terminal.
//!
//! `examples/selector.rs` narrows one host-owned task board with one query. It
//! shows the ranking, the moves, the selection that survives a refiltering,
//! the window over a list larger than the overlay, and every bound:
//!
//! ```sh
//! cargo run -p kvim-ui --example selector
//! ```

use kvim_fuzzy::rank;
use ratatui::layout::Rect;

use crate::list::{ListItem, ListPlacement, ListViewport};

/// The largest number of candidates that one selector holds.
///
/// A host may offer more candidates than a reader ever narrows through one
/// query. The bound keeps one keystroke proportional to a fixed candidate
/// count, and a host that offers more candidates than this needs a narrower
/// source instead of a larger selector.
pub const SELECTOR_CANDIDATES_MAX: usize = 4096;

/// The largest number of characters that one selector query holds.
///
/// The bound keeps the ranking cost of one keystroke independent of how many
/// characters a host lets a reader type into the query.
pub const SELECTOR_QUERY_CHARS_MAX: usize = 128;

/// One candidate that one host offers to a selector.
///
/// The selector ranks the name and the container string through
/// `kvim-fuzzy`. A host gives its own meaning to each string: a command and
/// its group, a branch and its remote, an entry and its section. The selector
/// stores neither meaning and reads neither string beyond the ranking.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectorCandidate<R> {
    id: R,
    name: String,
    container: String,
}

impl<R> SelectorCandidate<R> {
    /// Creates one candidate with a host identity, a name, and a container
    /// string.
    #[must_use]
    pub fn new(id: R, name: impl Into<String>, container: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            container: container.into(),
        }
    }

    /// Returns the host identity of the candidate.
    #[must_use]
    pub const fn id(&self) -> &R {
        &self.id
    }

    /// Returns the name that the ranking compares first.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the container string that the ranking compares after the name.
    #[must_use]
    pub fn container(&self) -> &str {
        &self.container
    }
}

/// The visible part of one matched row, in the coordinates of the selector's
/// window.
///
/// [`SelectorPlacement::index`] names the position of the row inside
/// [`Selector::matches`], the row space that [`Selector::selected_row`] also
/// answers. It does not name a position inside the candidate list.
/// [`SelectorPlacement::candidate_index`] names that position instead. Pass it
/// to [`Selector::candidate`] to reach the matched candidate directly, with no
/// further lookup through [`Selector::matches`].
///
/// # Examples
///
/// ```
/// use kvim_ui::{Selector, SelectorCandidate};
///
/// let mut selector = Selector::default();
/// selector.set_candidates(vec![SelectorCandidate::new(7_u32, "one", "")], false);
/// selector.set_height_rows(2);
///
/// let placement = &selector.placements()[0];
/// assert_eq!(placement.index(), 0);
/// let candidate = selector
///     .candidate(placement.candidate_index())
///     .expect("the placement names one held candidate");
/// assert_eq!(candidate.name(), "one");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectorPlacement {
    candidate: usize,
    placement: ListPlacement,
}

impl SelectorPlacement {
    /// Returns the position of the row inside [`Selector::matches`].
    #[must_use]
    pub const fn index(&self) -> usize {
        self.placement.index()
    }

    /// Returns the position of the matched candidate inside the complete
    /// candidate list.
    ///
    /// Pass the value to [`Selector::candidate`] to reach the candidate.
    #[must_use]
    pub const fn candidate_index(&self) -> usize {
        self.candidate
    }

    /// Returns the first visible line of the row.
    #[must_use]
    pub const fn first_line(&self) -> u16 {
        self.placement.first_line()
    }

    /// Returns the number of visible lines of the row.
    #[must_use]
    pub const fn lines(&self) -> u16 {
        self.placement.lines()
    }

    /// Returns the offset of the row from the top of the window, in rows.
    #[must_use]
    pub const fn top_row(&self) -> u16 {
        self.placement.top_row()
    }

    /// Returns the rectangle of the visible part inside one selector
    /// rectangle.
    ///
    /// The rectangle never reaches outside the selector, so a placement of a
    /// taller window still writes inside a shorter rectangle.
    #[must_use]
    pub fn area(&self, selector: Rect) -> Rect {
        self.placement.area(selector)
    }
}

/// The direction of one selection move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    /// Move away from the query.
    Next,
    /// Move toward the query.
    Previous,
}

/// The bounded selector: one query, one candidate list, and one stable
/// selection.
///
/// The selector owns the query, the candidates, the ranking, and the
/// selection. It runs no host command and names no host meaning: a host reads
/// [`Selector::selected`] and decides what an activation means.
///
/// # Examples
///
/// ```
/// use kvim_ui::{Selector, SelectorCandidate};
///
/// #[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// enum Command {
///     Save,
///     Quit,
///     OpenSettings,
/// }
///
/// let mut selector = Selector::default();
/// selector.set_candidates(
///     vec![
///         SelectorCandidate::new(Command::Save, "Save", "General"),
///         SelectorCandidate::new(Command::Quit, "Quit", "General"),
///         SelectorCandidate::new(Command::OpenSettings, "Open Settings", "Preferences"),
///     ],
///     false,
/// );
///
/// // The best match sits at the top, next to the query.
/// selector.set_query("open");
/// let selected = selector.selected().expect("one candidate holds the query");
/// assert_eq!(selected.name(), "Open Settings");
/// assert_eq!(selected.container(), "Preferences");
///
/// // The selection survives a refiltering while the query still matches it.
/// selector.set_query("open s");
/// assert_eq!(
///     selector.selected().map(SelectorCandidate::name),
///     Some("Open Settings")
/// );
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selector<R> {
    query: String,
    candidates: Vec<SelectorCandidate<R>>,
    /// The candidate indexes that the query keeps, with the best row first.
    matches: Vec<usize>,
    /// The candidate index of the selected row.
    ///
    /// The selector keeps the selected candidate across one refiltering while
    /// the query still matches it, so the selection never jumps under the
    /// reader.
    selected: Option<usize>,
    /// Reports whether the host offered more candidates than the bound keeps.
    truncated: bool,
    /// The one window over the matched rows. It owns the window height, the
    /// scroll margin, the first visible row, the total row count, and the
    /// rule that keeps the selected row inside the window. See
    /// [`ListViewport`].
    viewport: ListViewport,
    placements: Vec<SelectorPlacement>,
}

impl<R> Default for Selector<R> {
    /// Returns one selector with an empty query, no candidate, and a window
    /// of zero rows.
    fn default() -> Self {
        Self {
            query: String::new(),
            candidates: Vec::new(),
            matches: Vec::new(),
            selected: None,
            truncated: false,
            viewport: ListViewport::default(),
            placements: Vec::new(),
        }
    }
}

impl<R> Selector<R> {
    /// Returns the query that the selector holds.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Reports whether the host offered more candidates than the bound keeps.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Returns the number of candidates that the selector holds.
    ///
    /// The count names the complete candidate list, not [`Selector::matches`].
    /// A host reads this count to tell two empty cases apart. Zero names a
    /// list with no candidate at all. A positive count beside an empty
    /// [`Selector::matches`] names a query that keeps nothing.
    #[must_use]
    pub const fn candidates_len(&self) -> usize {
        self.candidates.len()
    }

    /// Replaces the candidates and ranks them again.
    ///
    /// The list stops at [`SELECTOR_CANDIDATES_MAX`], and a longer list
    /// reports the truncation.
    pub fn set_candidates(&mut self, candidates: Vec<SelectorCandidate<R>>, truncated: bool) {
        self.truncated = truncated || candidates.len() > SELECTOR_CANDIDATES_MAX;
        self.candidates = candidates;
        self.candidates.truncate(SELECTOR_CANDIDATES_MAX);
        self.refilter();
    }

    /// Replaces the query and ranks the candidates again.
    ///
    /// The query stops at [`SELECTOR_QUERY_CHARS_MAX`] characters.
    pub fn set_query(&mut self, query: &str) {
        self.query = clip(query, SELECTOR_QUERY_CHARS_MAX);
        self.refilter();
    }

    /// Returns the candidate indexes that the query keeps, with the best
    /// first.
    #[must_use]
    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    /// Returns one candidate by its index.
    #[must_use]
    pub fn candidate(&self, index: usize) -> Option<&SelectorCandidate<R>> {
        self.candidates.get(index)
    }

    /// Returns the selected candidate, or `None` while no row matches.
    #[must_use]
    pub fn selected(&self) -> Option<&SelectorCandidate<R>> {
        self.candidates.get(self.selected?)
    }

    /// Returns the position of the selected row inside [`Selector::matches`].
    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        let selected = self.selected?;
        self.matches.iter().position(|index| *index == selected)
    }

    /// Returns the height of the window, in terminal rows.
    #[must_use]
    pub const fn height_rows(&self) -> u16 {
        self.viewport.height_rows()
    }

    /// Sets the height of the window, in terminal rows.
    ///
    /// The selector scrolls the selected row back into the window, so a
    /// resize never hides it.
    pub fn set_height_rows(&mut self, height_rows: u16) {
        self.viewport.set_height_rows(height_rows);
        self.reconcile_viewport();
    }

    /// Returns the number of rows that the selection keeps above and below
    /// itself.
    #[must_use]
    pub const fn scroll_margin(&self) -> u16 {
        self.viewport.scroll_margin()
    }

    /// Sets the number of rows that the selection keeps above and below
    /// itself.
    ///
    /// The margin stops at half the window, so a short window still shows the
    /// selected row.
    pub fn set_scroll_margin(&mut self, margin_rows: u16) {
        self.viewport.set_scroll_margin(margin_rows);
        self.reconcile_viewport();
    }

    /// Returns the first visible row of the matched list.
    #[must_use]
    pub const fn first_line(&self) -> u32 {
        self.viewport.first_line()
    }

    /// Returns the number of terminal rows that every matched row occupies
    /// together.
    #[must_use]
    pub const fn total_lines(&self) -> u32 {
        self.viewport.total_lines()
    }

    /// Returns the visible part of every matched row that the window shows,
    /// in match order.
    ///
    /// The placements cover the window from its first row without a gap
    /// while the matched rows fill it. A host paints a bounded selector from
    /// this list alone, without computing an offset of its own.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_ui::{Selector, SelectorCandidate};
    ///
    /// let mut selector = Selector::default();
    /// selector.set_candidates(
    ///     (0..10)
    ///         .map(|index| SelectorCandidate::new(index, format!("row {index}"), ""))
    ///         .collect(),
    ///     false,
    /// );
    /// selector.set_height_rows(4);
    ///
    /// // The window follows the selection to the last row and stops there,
    /// // instead of scrolling past the end of the list.
    /// for _ in 0..9 {
    ///     selector.select_next();
    /// }
    /// assert_eq!(selector.selected_row(), Some(9));
    /// assert_eq!(selector.first_line(), 6);
    /// assert_eq!(selector.placements().len(), 4);
    ///
    /// // The selection always sits inside the published window.
    /// let selected_row = selector.selected_row().expect("one row is selected");
    /// assert!(
    ///     selector
    ///         .placements()
    ///         .iter()
    ///         .any(|placement| placement.index() == selected_row)
    /// );
    /// ```
    #[must_use]
    pub fn placements(&self) -> &[SelectorPlacement] {
        &self.placements
    }

    /// Moves the selection one row toward the end of the list.
    ///
    /// The list ends at both edges, because a wrap would move the reader past
    /// the best match without a key that says so.
    pub fn select_next(&mut self) {
        self.select(Step::Next);
    }

    /// Moves the selection one row toward the query.
    pub fn select_previous(&mut self) {
        self.select(Step::Previous);
    }

    /// Moves the selection by one step inside the matched rows.
    fn select(&mut self, step: Step) {
        let Some(row) = self.selected_row() else {
            self.selected = self.matches.first().copied();
            self.reconcile_viewport();
            return;
        };
        let last = self.matches.len().saturating_sub(1);
        let next = match step {
            Step::Previous => row.saturating_sub(1),
            Step::Next => row.saturating_add(1).min(last),
        };
        self.selected = self.matches.get(next).copied();
        self.reconcile_viewport();
    }

    /// Moves the window until it shows the selection, then names the matched
    /// rows that it places.
    ///
    /// [`ListViewport`] owns the offset rule and the clipping. Every matched
    /// row holds one line, so this method hands it one [`ListItem::single`]
    /// for each row of [`Selector::matches`], at the position
    /// [`Selector::selected_row`] answers. It resolves the candidate index of
    /// each returned placement from [`Selector::matches`], so a caller reaches
    /// the candidate through [`SelectorPlacement::candidate_index`] with no
    /// further lookup.
    fn reconcile_viewport(&mut self) {
        let selected_row = self.selected_row();
        self.viewport.reconcile(
            std::iter::repeat_n(ListItem::single(), self.matches.len()),
            selected_row,
        );
        self.placements.clear();
        self.placements
            .extend(
                self.viewport
                    .placements()
                    .iter()
                    .map(|placement| SelectorPlacement {
                        candidate: self.matches[placement.index()],
                        placement: *placement,
                    }),
            );
    }

    /// Ranks every candidate against the query and keeps the selection.
    ///
    /// The ranking rule itself lives in [`kvim_fuzzy::rank`], the one shared
    /// rule that every bounded candidate list in kvim uses. See
    /// `docs/architecture.md`.
    fn refilter(&mut self) {
        self.matches = rank(
            &self.query,
            self.candidates
                .iter()
                .map(|candidate| (candidate.name(), candidate.container())),
        );
        // The selection follows its candidate while the query still keeps it,
        // so a further character never moves the reader to another row.
        if !self
            .selected
            .is_some_and(|selected| self.matches.contains(&selected))
        {
            self.selected = self.matches.first().copied();
        }
        self.reconcile_viewport();
    }
}

/// Returns the first characters of one text.
fn clip(text: &str, chars_max: usize) -> String {
    text.chars().take(chars_max).collect()
}

#[cfg(test)]
#[path = "selector_tests.rs"]
mod tests;
