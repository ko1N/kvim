//! The bounded undo and redo history of one buffer.
//!
//! The history keeps applied transactions in the order that the buffer applied
//! them. The position marks the state between the undo entries below it and the
//! redo entries above it. A new transaction discards the redo entries.

use std::collections::VecDeque;

use super::coordinates::CharPosition;

/// The largest number of applied transactions that one buffer keeps.
///
/// The value matches the reference Neovim `undolevels` value.
pub const UNDO_HISTORY_ENTRIES_MAX: usize = 1_000;

/// The largest amount of replaced and inserted text that one history keeps.
///
/// The value is four times the default maximum file size, so a full-file
/// replacement stays undoable while the history memory stays bounded.
pub const UNDO_HISTORY_BYTES_MAX: usize = 16 * 1024 * 1024;

/// One applied change with the text on both sides of the change.
///
/// `start` addresses the text before the change. `new_start` addresses the text
/// after the change. The two differ when an earlier change of the same
/// transaction moved the position.
#[derive(Clone, Debug)]
pub(super) struct AppliedChange {
    pub(super) start: usize,
    pub(super) new_start: usize,
    pub(super) removed: String,
    pub(super) removed_chars: usize,
    pub(super) inserted: String,
    pub(super) inserted_chars: usize,
}

/// One applied transaction with the cursor position on both sides of the change.
#[derive(Clone, Debug)]
pub(super) struct AppliedTransaction {
    pub(super) changes: Vec<AppliedChange>,
    pub(super) cursor_before: CharPosition,
    pub(super) cursor_after: CharPosition,
}

impl AppliedTransaction {
    fn retained_bytes(&self) -> usize {
        self.changes
            .iter()
            .map(|change| change.removed.len() + change.inserted.len())
            .sum()
    }
}

#[derive(Clone, Debug)]
pub(super) struct UndoHistory {
    entries: VecDeque<AppliedTransaction>,
    position: usize,
    retained_bytes: usize,
    /// The position of the last save, while that position stays in the history.
    saved_position: Option<usize>,
}

impl UndoHistory {
    pub(super) fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            position: 0,
            retained_bytes: 0,
            saved_position: Some(0),
        }
    }

    /// Records one applied transaction and discards the redo entries above it.
    pub(super) fn push(&mut self, entry: AppliedTransaction) {
        if self
            .saved_position
            .is_some_and(|saved| saved > self.position)
        {
            // The discarded redo entries hold the saved state, so the buffer
            // cannot return to it by undo.
            self.saved_position = None;
        }
        for _ in self.position..self.entries.len() {
            let dropped = self
                .entries
                .pop_back()
                .expect("the loop runs once for each entry above the position");
            self.retained_bytes -= dropped.retained_bytes();
        }

        self.retained_bytes += entry.retained_bytes();
        self.entries.push_back(entry);
        self.position = self.entries.len();
        self.enforce_bounds();
    }

    /// Moves one step back and returns the transaction to reverse.
    pub(super) fn step_back(&mut self) -> Option<&AppliedTransaction> {
        self.position = self.position.checked_sub(1)?;
        Some(&self.entries[self.position])
    }

    /// Moves one step forward and returns the transaction to replay.
    pub(super) fn step_forward(&mut self) -> Option<&AppliedTransaction> {
        let entry = self.entries.get(self.position)?;
        self.position += 1;
        Some(entry)
    }

    /// Marks the current position as the saved state.
    pub(super) fn mark_saved(&mut self) {
        self.saved_position = Some(self.position);
    }

    /// Reports whether the buffer differs from the last saved state.
    pub(super) fn is_modified(&self) -> bool {
        self.saved_position != Some(self.position)
    }

    fn enforce_bounds(&mut self) {
        while self.entries.len() > UNDO_HISTORY_ENTRIES_MAX
            || (self.retained_bytes > UNDO_HISTORY_BYTES_MAX && self.entries.len() > 1)
        {
            let dropped = self
                .entries
                .pop_front()
                .expect("both bound checks require at least one entry");
            self.retained_bytes -= dropped.retained_bytes();
            self.position -= 1;
            // The oldest state left the history, so a saved state at that
            // position is no longer reachable.
            self.saved_position = match self.saved_position {
                Some(0) | None => None,
                Some(saved) => Some(saved - 1),
            };
        }

        debug_assert!(
            self.position <= self.entries.len(),
            "the position counts the entries below it, so it never exceeds the entry count"
        );
    }
}
