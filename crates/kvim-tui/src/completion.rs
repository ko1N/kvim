//! The candidate model of the command-line completion.
//!
//! The module is pure. It reads no clock, no filesystem, and no process, so one
//! typed text and one candidate list always produce one selection.
//!
//! The model holds candidates, never their source. One producer supplies the
//! command names of the parser today. A later producer supplies the path
//! candidates of `:e` and reuses this model unchanged, so a new source adds data
//! and no second mechanism.
//!
//! The command names match by prefix, and not by the fuzzy score of the picker.
//! The command line names the exact sets: `q` offers `quit` and `quit!` alone,
//! while a subsequence match would add `wq` as well. The path source of `:e`
//! still ranks with the scorer of the picker, so one fuzzy rule serves the
//! picker and the paths.
//!
//! See `docs/input-actions.md`.

use kvim_input::CommandLineCommand;

/// The largest number of candidates that one completion holds.
///
/// The bound keeps one keystroke of a large workspace proportional to this
/// number instead of the number of files. A longer source list keeps its first
/// candidates, because the producer orders them by rank.
pub(super) const COMPLETION_CANDIDATES_MAX: usize = 64;

/// The direction of one completion cycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompletionCycle {
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
pub(super) enum CompletionOutcome {
    /// No candidate answered the line, so the line is unchanged.
    Missed,
    /// One candidate completed the line, and no list is open.
    Completed,
    /// Several candidates match, so the list shows them above the message line.
    Listed,
}

/// The open completion of one prompt line.
///
/// The type holds two texts, because the text that the user typed and the
/// candidate that a cycle wrote into the line are different states. The
/// candidates stay anchored to `typed`, so one cycle never narrows them and a
/// cancel restores the typed text exactly.
///
/// The constructor establishes two invariants: the candidate list is never
/// empty, and the selection always names one of its candidates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LineCompletion {
    /// The text that the user typed, which a cancel restores.
    typed: String,
    /// The candidates of `typed`, in the order of their producer.
    candidates: Vec<String>,
    /// The candidate that the prompt line shows.
    selected: usize,
}

impl LineCompletion {
    /// Opens one completion over the candidates of `typed`.
    ///
    /// The `cycle` names the direction of the key that opened the completion.
    /// [`CompletionCycle::Next`] selects the first candidate, and
    /// [`CompletionCycle::Previous`] selects the last one, so a backward cycle
    /// from the typed text wraps to the end of the list.
    ///
    /// The function drops every candidate above `chars_max`, because the prompt
    /// line rejects a longer text. It returns `None` while no candidate
    /// survives, so an empty completion is unrepresentable.
    pub(super) fn open(
        typed: &str,
        candidates: Vec<String>,
        chars_max: usize,
        cycle: CompletionCycle,
    ) -> Option<Self> {
        let mut candidates = candidates;
        candidates.retain(|candidate| candidate.chars().count() <= chars_max);
        candidates.truncate(COMPLETION_CANDIDATES_MAX);
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
    pub(super) fn cycle(&mut self, cycle: CompletionCycle) {
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
    pub(super) fn selected(&self) -> &str {
        let Some(candidate) = self.candidates.get(self.selected) else {
            debug_assert!(
                false,
                "the constructor and the cycle keep the selection inside the list"
            );
            return "";
        };
        candidate
    }

    /// Returns what the open completion left on the screen.
    ///
    /// The value is never [`CompletionOutcome::Missed`], because the
    /// constructor rejects an empty candidate list. Only a caller that offered
    /// no candidate reports that outcome.
    pub(super) fn outcome(&self) -> CompletionOutcome {
        if self.candidates.len() < 2 {
            return CompletionOutcome::Completed;
        }
        CompletionOutcome::Listed
    }

    /// Returns the text that the user typed and drops the completion.
    pub(super) fn into_typed(self) -> String {
        self.typed
    }
}

/// Returns the completion candidates of one command line.
///
/// Only a command name completes today, so the producer asks the parser which
/// full names the line abbreviates. The parser owns that rule, because it also
/// owns the declared abbreviation of each name, so the two can never disagree.
pub(super) fn command_line_candidates(line: &str) -> Vec<String> {
    CommandLineCommand::names_matching(line)
}

#[cfg(test)]
mod tests {
    use super::{COMPLETION_CANDIDATES_MAX, CompletionCycle, CompletionOutcome, LineCompletion};

    /// The character bound of a prompt that accepts every test candidate.
    const CHARS_MAX: usize = 16;

    #[test]
    fn the_completion_bounds_its_candidates_and_rejects_an_empty_list() {
        let none = LineCompletion::open("q", Vec::new(), CHARS_MAX, CompletionCycle::Next);
        assert!(none.is_none(), "an empty completion is unrepresentable");

        // The prompt rejects a longer text, so the completion never writes one.
        let long = "a".repeat(CHARS_MAX + 1);
        let dropped = LineCompletion::open(
            "a",
            vec![long, "ab".to_owned()],
            CHARS_MAX,
            CompletionCycle::Next,
        )
        .expect("one candidate fits the bound");
        assert_eq!(dropped.selected(), "ab");
        assert_eq!(dropped.outcome(), CompletionOutcome::Completed);

        // The bound drops the tail of a longer source, so the cycle returns to
        // the first candidate after exactly the bounded number of steps.
        let many: Vec<String> = (0..COMPLETION_CANDIDATES_MAX + 8)
            .map(|index| format!("c{index}"))
            .collect();
        let mut bounded = LineCompletion::open("c", many, CHARS_MAX, CompletionCycle::Next)
            .expect("the source holds candidates");
        assert_eq!(bounded.outcome(), CompletionOutcome::Listed);
        assert_eq!(bounded.selected(), "c0");
        for _ in 0..COMPLETION_CANDIDATES_MAX - 1 {
            bounded.cycle(CompletionCycle::Next);
        }
        let last = format!("c{}", COMPLETION_CANDIDATES_MAX - 1);
        assert_eq!(bounded.selected(), last, "the bound drops every later row");
        bounded.cycle(CompletionCycle::Next);
        assert_eq!(bounded.selected(), "c0", "the cycle wraps at the bound");
    }
}
