//! The candidate menu of one prompt line: the model and the painter.
//!
//! A prompt line offers candidates for the text that the user typed. The menu
//! holds those candidates, names the selected one, and restores the typed text
//! when the user cancels it. [`draw_completion_menu`] paints the menu with the
//! appearance of kvim. The standalone editor draws through that one call, so a
//! host that uses it can never see a second appearance.
//!
//! The module is pure. It reads no clock, no filesystem, and no process, so one
//! typed text and one candidate list always produce one selection, and one menu
//! and one theme always produce the same cells.
//!
//! The model holds candidates, never their source. Two producers supply them in
//! the standalone editor. The parser supplies the command names of a line
//! without a blank, and the collected workspace files supply the path argument
//! of `:e`. Both reach this model as one candidate list, so the second source
//! adds data and no second mechanism.
//!
//! The command names match by prefix, and not by the fuzzy score of the picker.
//! The command line names the exact sets: `q` offers `quit` alone, while a
//! subsequence match would add `wq` as well. The path source of `:e` ranks with
//! the scorer of the picker instead, so one fuzzy rule serves the picker and the
//! paths.
//!
//! A candidate is the text of one line, and never the prompt prefix in front of
//! it. The prompt already shows that prefix, so a menu that repeated it would
//! show the prefix twice. The model holds line text alone and the painter draws
//! the model alone, so the prefix reaches no row.
//!
//! `examples/completion_menu.rs` opens one menu over host-owned candidates,
//! cycles it, cancels it, and draws it into cells that the host owns:
//!
//! ```sh
//! cargo run -p kvim-tui --example completion_menu
//! ```
//!
//! See `docs/input-actions.md` and `docs/windows.md`.

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;

use kvim_fuzzy::rank;
use kvim_input::CommandLineCommand;
use kvim_workspace::{Candidate, PICKER_QUERY_CHARS_MAX};

use super::cells::{text_cells, truncate_cells_left};
use super::overlay::{OVERFLOW_NOTE, fill};
use super::theme::{Theme, ThemeRole};

/// The largest number of candidates that one menu holds.
///
/// The bound keeps one keystroke of a large workspace proportional to this
/// number instead of the number of files. The menu refuses a longer list
/// instead of cutting it, because a cut menu hides candidates that no cycle
/// ever reaches and reports nothing. A producer that offers more candidates
/// ranks and shortens its own list first.
pub const COMPLETION_CANDIDATES_MAX: usize = 64;

/// The largest number of rows that the menu shows.
///
/// The menu covers the text below it while the user reads the prompt line, so a
/// long candidate set never fills the terminal. The menu reports the candidates
/// that it hides. See `docs/windows.md`.
pub const COMPLETION_ROWS_MAX: usize = 8;

/// The largest number of cells that the menu occupies.
///
/// A command name is short, and a path candidate is long, so the bound keeps a
/// wide menu off the text beside it. A narrower band bounds the menu further.
pub const COMPLETION_COLUMNS_MAX: u16 = 48;

/// The number of cells that the menu keeps beside its text.
///
/// The left cell puts a candidate above the text of the command line, which
/// follows the `:` prefix. The right cell frames the text with the same surface
/// color.
const COMPLETION_PADDING_CELLS: u16 = 1;

/// The number of cells that the padding of both sides occupies.
const COMPLETION_PADDING_TOTAL: u16 = COMPLETION_PADDING_CELLS.saturating_mul(2);

/// The direction of one completion cycle.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CompletionCycle {
    /// Select the candidate after the selected one.
    Next,
    /// Select the candidate before the selected one.
    Previous,
}

/// What one completion key left on the screen.
///
/// The outcome names what the user sees, because several candidates need a
/// choice and one candidate does not. See `docs/windows.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionOutcome {
    /// No candidate answered the line, so the line is unchanged.
    Missed,
    /// One candidate completed the line, and no menu is open.
    Completed,
    /// Several candidates match, so the menu shows them above the message line.
    Listed,
}

/// The open candidate menu of one prompt line.
///
/// The type holds two texts, because the text that the user typed and the
/// candidate that a cycle wrote into the line are different states. The
/// candidates stay anchored to `typed`, so one cycle never narrows them and a
/// cancel restores the typed text exactly.
///
/// The constructor establishes two invariants: the candidate list is never
/// empty, and the selection always names one of its candidates.
///
/// A candidate is the text of one line without the prompt prefix. The prompt
/// shows that prefix already, so the menu never repeats it.
///
/// # Examples
///
/// ```
/// use kvim_tui::{CompletionCycle, CompletionOutcome, LineCompletion};
///
/// // The host offers the candidates of the text that its reader typed. The
/// // candidates carry no `:` prefix, because the prompt line shows one.
/// let candidates = vec!["write".to_owned(), "wqall".to_owned()];
/// let mut menu = LineCompletion::open("w", candidates, 64, CompletionCycle::Next)
///     .expect("the host offers two candidates inside the bound");
///
/// assert_eq!(menu.outcome(), CompletionOutcome::Listed);
/// assert_eq!(menu.selected(), "write");
///
/// // The cycle wraps at both ends, so the reader reaches every candidate.
/// menu.cycle(CompletionCycle::Next);
/// assert_eq!(menu.selected(), "wqall");
/// menu.cycle(CompletionCycle::Next);
/// assert_eq!(menu.selected(), "write");
///
/// // A cancelled menu restores the text that the reader typed.
/// assert_eq!(menu.into_typed(), "w");
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineCompletion {
    /// The text that the user typed, which a cancel restores.
    typed: String,
    /// The candidates of `typed`, in the order of their producer.
    candidates: Vec<String>,
    /// The candidate that the prompt line shows.
    selected: usize,
}

impl LineCompletion {
    /// Opens one menu over the candidates of `typed`.
    ///
    /// The `typed` text is the line without the prompt prefix, and every
    /// candidate replaces that whole line. A candidate therefore names itself
    /// alone: the prompt already shows the prefix in front of the line.
    ///
    /// The `cycle` names the direction of the key that opened the menu.
    /// [`CompletionCycle::Next`] selects the first candidate, and
    /// [`CompletionCycle::Previous`] selects the last one, so a backward cycle
    /// from the typed text wraps to the end of the list.
    ///
    /// The function drops every candidate above `chars_max`, because the prompt
    /// line rejects a longer text.
    ///
    /// The function returns `None` in two cases, so an empty menu and an
    /// unbounded menu are both unrepresentable:
    ///
    /// - no candidate survives `chars_max`;
    /// - more than [`COMPLETION_CANDIDATES_MAX`] candidates survive it. The
    ///   menu refuses the whole list instead of cutting it, so no candidate
    ///   disappears without the caller learning of it. A caller that holds more
    ///   candidates ranks and shortens its own list first.
    #[must_use]
    pub fn open(
        typed: &str,
        candidates: Vec<String>,
        chars_max: usize,
        cycle: CompletionCycle,
    ) -> Option<Self> {
        let mut candidates = candidates;
        candidates.retain(|candidate| candidate.chars().count() <= chars_max);
        if candidates.len() > COMPLETION_CANDIDATES_MAX {
            return None;
        }
        let last = candidates.len().checked_sub(1)?;
        let selected = match cycle {
            CompletionCycle::Next => 0,
            CompletionCycle::Previous => last,
        };
        Some(Self {
            typed: typed.to_owned(),
            candidates,
            selected,
        })
    }

    /// Moves the selection one candidate in `cycle`.
    ///
    /// The selection wraps at both ends, so a forward cycle past the last
    /// candidate reaches the first one and a backward cycle past the first one
    /// reaches the last.
    pub fn cycle(&mut self, cycle: CompletionCycle) {
        debug_assert!(
            !self.candidates.is_empty(),
            "the constructor rejects an empty candidate list"
        );
        let Some(last) = self.candidates.len().checked_sub(1) else {
            return;
        };
        self.selected = match cycle {
            CompletionCycle::Next if self.selected >= last => 0,
            CompletionCycle::Next => self.selected + 1,
            CompletionCycle::Previous if self.selected == 0 => last,
            CompletionCycle::Previous => self.selected - 1,
        };
    }

    /// Returns the candidate that the prompt line shows.
    ///
    /// The text is the whole line without the prompt prefix, so the caller
    /// writes it after its own prefix.
    #[must_use]
    pub fn selected(&self) -> &str {
        let Some(candidate) = self.candidates.get(self.selected) else {
            debug_assert!(
                false,
                "the constructor and the cycle keep the selection inside the list"
            );
            return "";
        };
        candidate
    }

    /// Returns the candidates of the menu, in the order of their producer.
    ///
    /// The list is never empty, because the constructor rejects an empty one.
    /// [`draw_completion_menu`] paints these texts. See `docs/windows.md`.
    #[must_use]
    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    /// Returns the row of the candidate that the prompt line shows.
    ///
    /// The row always names one candidate of [`LineCompletion::candidates`],
    /// because the constructor and the cycle keep the selection inside the
    /// list.
    #[must_use]
    pub const fn selected_row(&self) -> usize {
        self.selected
    }

    /// Returns what the open menu left on the screen.
    ///
    /// The value is never [`CompletionOutcome::Missed`], because the
    /// constructor rejects an empty candidate list. Only a caller that offered
    /// no candidate reports that outcome.
    #[must_use]
    pub fn outcome(&self) -> CompletionOutcome {
        if self.candidates.len() < 2 {
            return CompletionOutcome::Completed;
        }
        CompletionOutcome::Listed
    }

    /// Returns the text that the user typed and drops the menu.
    ///
    /// A cancelled menu writes this text back into the prompt line, so the line
    /// reads exactly as it did before the first completion key.
    #[must_use]
    pub fn into_typed(self) -> String {
        self.typed
    }
}

/// Paints one candidate menu into the last rows of `body`.
///
/// The painter owns the complete layout of the menu. It takes the last
/// [`COMPLETION_ROWS_MAX`] rows of `body`, it starts at the left edge of
/// `body`, and it keeps one cell beside its text on both sides. A menu with
/// more candidates than rows spends its last row on a note, so no candidate
/// disappears without a sign. A candidate that is wider than the menu loses its
/// start and a `<` marks the cut, because the end of a path names the file that
/// the reader looks for. Every measurement counts terminal cells, so no clip
/// splits a wide character.
///
/// The call draws nothing while the menu holds one candidate alone, because one
/// candidate needs no choice from the reader.
///
/// The caller supplies the palette, because a host holds one already. The
/// painter reads no other state: every fact that it draws comes from the menu.
/// kvim's own command line draws through this call, so a host that uses it can
/// never see a second appearance.
///
/// The painter draws [`LineCompletion::candidates`] and nothing else, so the
/// prompt prefix of the caller reaches no row.
///
/// See `docs/windows.md`.
///
/// # Examples
///
/// ```
/// use kvim_tui::{CompletionCycle, LineCompletion, Theme, draw_completion_menu};
/// use ratatui::buffer::Buffer;
/// use ratatui::layout::Rect;
///
/// let body = Rect::new(0, 0, 20, 6);
/// let candidates = vec!["write".to_owned(), "wqall".to_owned()];
/// let menu = LineCompletion::open("w", candidates, 64, CompletionCycle::Next)
///     .expect("the host offers two candidates inside the bound");
///
/// let mut cells = Buffer::empty(body);
/// draw_completion_menu(&mut cells, body, Theme::new(), &menu);
///
/// // The menu takes the last rows of the band, and its text starts one cell
/// // inside it. The entry names the candidate alone, without a `:` prefix.
/// let row = body.bottom() - 2;
/// let text: String = (0..5)
///     .filter_map(|offset| cells.cell((1 + offset, row)))
///     .map(|cell| cell.symbol().to_owned())
///     .collect();
/// assert_eq!(text, "write");
/// ```
pub fn draw_completion_menu(
    target: &mut CellBuffer,
    body: Rect,
    theme: Theme,
    completion: &LineCompletion,
) {
    match completion.outcome() {
        // One candidate answers the line alone, and no candidate changes it, so
        // neither outcome needs a choice from the user.
        CompletionOutcome::Missed | CompletionOutcome::Completed => return,
        CompletionOutcome::Listed => {}
    }
    if body.is_empty() {
        return;
    }
    let candidates = completion.candidates();
    // The row bound applies before the measurement, so a candidate that the
    // menu never shows cannot widen it.
    let rows = candidates
        .len()
        .min(usize::from(body.height).min(COMPLETION_ROWS_MAX));
    let hidden = rows < candidates.len();
    // A clipped menu spends its last row on the note, so the note never hides a
    // candidate without reporting the loss.
    let shown = if hidden { rows - 1 } else { rows };
    let first = completion_first_row(candidates.len(), completion.selected_row(), shown);
    let Some(painted) = candidates.get(first..first + shown) else {
        debug_assert!(
            false,
            "the window start keeps the shown rows inside the list"
        );
        return;
    };

    let text_cells_max = painted
        .iter()
        .map(|candidate| text_cells(candidate))
        .chain(hidden.then(|| text_cells(OVERFLOW_NOTE)))
        .max()
        .unwrap_or(0);
    let width = u16::try_from(text_cells_max)
        .unwrap_or(u16::MAX)
        .saturating_add(COMPLETION_PADDING_TOTAL)
        .clamp(1, body.width.min(COMPLETION_COLUMNS_MAX));
    let Ok(height) = u16::try_from(rows) else {
        debug_assert!(false, "the row bound keeps the menu height small");
        return;
    };
    let area = Rect::new(body.x, body.bottom() - height, width, height);
    // A row that is wider than the menu loses its start at this budget. The
    // file name at the end of a path names the file that the user looks for,
    // and every row of one path list starts with the same command name. The
    // budget counts terminal cells, so the clip never splits a wide character.
    // See `docs/windows.md`.
    let budget = usize::from(width.saturating_sub(COMPLETION_PADDING_TOTAL));
    let x = area.x.saturating_add(COMPLETION_PADDING_CELLS);
    let surface = theme.style(ThemeRole::Surface);
    let selected = surface.patch(theme.style(ThemeRole::PopupSelection));
    fill(target, area, " ");
    target.set_style(area, surface);
    for (offset, candidate) in painted.iter().enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            debug_assert!(false, "the row bound keeps the index small");
            break;
        };
        let y = area.y.saturating_add(offset);
        let style = if first + usize::from(offset) == completion.selected_row() {
            // The selected candidate is the text that the command line shows,
            // so its row carries the selection color of a popup list.
            target.set_style(Rect::new(area.x, y, area.width, 1), selected);
            selected
        } else {
            surface
        };
        let row = truncate_cells_left(candidate, budget);
        target.set_stringn(x, y, row, budget, style);
    }
    if !hidden {
        return;
    }
    target.set_stringn(
        x,
        area.bottom().saturating_sub(1),
        OVERFLOW_NOTE,
        budget,
        surface,
    );
}

/// Returns the first candidate that the bounded menu shows.
///
/// The function is pure: `candidates` counts the candidates of the menu,
/// `selected` names the candidate that the prompt line shows, and `shown`
/// counts the rows that the menu spends on candidates.
///
/// The shown candidates always hold the selected one, so a cycle past the last
/// shown row moves the window instead of hiding the selection. The window stays
/// at the end of the list once it reaches it, so the last rows never repeat a
/// candidate.
fn completion_first_row(candidates: usize, selected: usize, shown: usize) -> usize {
    debug_assert!(
        selected < candidates || candidates == 0,
        "the completion keeps its selection inside its candidate list"
    );
    let Some(last_start) = candidates.checked_sub(shown) else {
        return 0;
    };
    let Some(first) = selected.checked_sub(shown.saturating_sub(1)) else {
        return 0;
    };
    first.min(last_start)
}

/// Returns the completion candidates of one command line.
///
/// The candidates name kvim's own commands and the files of kvim's own
/// workspace, so this producer stays inside the crate. A host holds neither
/// vocabulary and supplies candidates of its own to [`LineCompletion::open`].
///
/// The first blank of the line ends the command name and opens its argument, so
/// a line without a blank completes a name and a line with one completes a
/// path. The parser owns both rules, because it owns the declared abbreviation
/// of each name and the commands that take a path, so the parser and the
/// completion can never disagree.
///
/// A path candidate keeps the command name that the user typed, so the
/// completed line still runs the command that the user chose. No candidate
/// carries the `:` prefix, because the command line shows that prefix already.
///
/// The `files` hold the workspace files that the bounded walk of the open
/// command line collected. The list is empty while that walk runs, and after a
/// cancelled or timed out walk, so the completion then offers no path and the
/// command line stays usable. Every file comes from a walk of the workspace
/// root, so no candidate reaches outside that root. See `docs/files.md`.
pub(super) fn command_line_candidates(line: &str, files: &[Candidate]) -> Vec<String> {
    let Some(argument) = CommandLineCommand::path_argument(line) else {
        return CommandLineCommand::names_matching(line);
    };
    let query: String = argument
        .typed
        .chars()
        .take(PICKER_QUERY_CHARS_MAX)
        .collect();
    rank(
        &query,
        files
            .iter()
            .map(|candidate| (candidate.name(), candidate.directory())),
    )
    .into_iter()
    // The menu refuses a longer list, so the producer ranks its files and
    // offers the best candidates instead of an unbounded set.
    .take(COMPLETION_CANDIDATES_MAX)
    .filter_map(|index| files.get(index))
    .map(|candidate| format!("{} {}", argument.name, candidate.relative_path().display()))
    .collect()
}

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
