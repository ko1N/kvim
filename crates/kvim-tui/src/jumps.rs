//! The jump list of one window.
//!
//! The list records where the cursor stood *before* each jump, so one backward
//! step returns to that position and one forward step moves ahead again. The
//! module is deterministic and pure: it reads no clock, no filesystem, and no
//! terminal, it holds no buffer text, and it names no session. See
//! `docs/windows.md`.
//!
//! An entry names one [`BufferId`]. `kvim_workspace::Buffers::insert`
//! increments a counter that never returns to a released value, so an identity
//! is never reused. A recorded identity therefore either names the same buffer
//! or names no buffer at all, and a stale entry can never resolve to a
//! different buffer. The entry keeps the display path beside the identity, so a
//! jump into a buffer that the session already dropped reopens the file.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use kvim_workspace::BufferId;

/// The number of positions that one jump list keeps.
///
/// Vim keeps the same number. A push past the bound drops the oldest entry.
pub const JUMPS_MAX: usize = 100;

/// One recorded cursor position.
///
/// The entry holds no text and no viewport. A jump back resolves the line and
/// the column against the buffer of that moment, so an edit that shifts the
/// text moves the landing position and never invalidates the entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JumpEntry {
    buffer: BufferId,
    path: Option<PathBuf>,
    line: usize,
    column: usize,
}

impl JumpEntry {
    /// Records one position.
    ///
    /// `path` holds the display path of the buffer, and stays `None` for a
    /// buffer that has no file.
    #[must_use]
    pub fn new(buffer: BufferId, path: Option<PathBuf>, line: usize, column: usize) -> Self {
        Self {
            buffer,
            path,
            line,
            column,
        }
    }

    /// Returns the buffer that held the cursor.
    #[must_use]
    pub fn buffer(&self) -> BufferId {
        self.buffer
    }

    /// Returns the display path of that buffer, if it had one.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Returns the recorded line.
    #[must_use]
    pub fn line(&self) -> usize {
        self.line
    }

    /// Returns the recorded column.
    #[must_use]
    pub fn column(&self) -> usize {
        self.column
    }

    /// Reports whether both entries name the same line of the same buffer.
    ///
    /// The column is not part of the answer, because Vim keeps one entry for
    /// each line and a repeated jump to that line must not grow the list.
    fn repeats(&self, other: &Self) -> bool {
        self.buffer == other.buffer && self.line == other.line
    }
}

/// The direction of one step through the jump list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JumpDirection {
    /// Move to the next older entry.
    Backward,
    /// Move to the next newer entry.
    Forward,
}

/// The result of one step through the jump list.
///
/// The step names the end that it reached instead of returning nothing, so the
/// caller can report why the cursor did not move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JumpStep {
    /// The list moved, and the cursor belongs at this position.
    Moved(JumpEntry),
    /// The list already sat on the oldest entry, so nothing moved.
    AtOldest,
    /// The list already sat at the newest position, so nothing moved.
    AtNewest,
}

/// Where one traversal sits inside the recorded entries.
///
/// The two variants are exclusive by construction, so the list can never claim
/// both that it sits at the newest position and that it sits on an entry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum JumpCursor {
    /// The traversal sits past the newest entry, so no forward history exists.
    ///
    /// The position that the cursor holds now is not in the list yet.
    #[default]
    Newest,
    /// The traversal sits on the entry at `index`.
    ///
    /// Every entry after `index` is forward history.
    Inside { index: usize },
}

/// The bounded jump list of one window.
///
/// A push records the position that the cursor held before a jump, discards the
/// forward history, and replaces the newest entry when it repeats the same line
/// of the same buffer. A backward step taken from the newest position records
/// the position that the caller supplies first, so the matching forward step
/// returns to it.
///
/// # Sequence
///
/// The cursor stands on a call site and a jump moves it to a definition. The
/// session pushes the call site before the move. One backward step then names
/// the call site, and records the definition on the way, so the following
/// forward step names the definition again. A second forward step reports
/// [`JumpStep::AtNewest`], because the list holds nothing newer. The unit test
/// `a_backward_step_and_a_forward_step_return_to_the_start` runs that sequence.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JumpList {
    entries: VecDeque<JumpEntry>,
    cursor: JumpCursor,
}

impl JumpList {
    /// Records the position that the cursor held before one jump.
    ///
    /// The push discards every entry ahead of the traversal, so a jump taken
    /// from inside the history replaces that history. The push then returns the
    /// traversal to the newest position.
    pub fn push(&mut self, entry: JumpEntry) {
        if let JumpCursor::Inside { index } = self.cursor {
            debug_assert!(
                index < self.entries.len(),
                "a step only names an entry that exists, and a push clears the traversal"
            );
            self.entries.truncate(index + 1);
        }
        self.cursor = JumpCursor::Newest;
        self.record(entry);
        self.debug_check();
    }

    /// Moves one entry through the list and returns the position to jump to.
    ///
    /// `current` is the position that the cursor holds now. A backward step
    /// taken from the newest position records it first, so the matching forward
    /// step returns to it. One entry point takes both directions, because a
    /// caller must never be able to step backward without naming that position.
    pub fn step(&mut self, direction: JumpDirection, current: JumpEntry) -> JumpStep {
        let step = match direction {
            JumpDirection::Backward => self.step_backward(current),
            JumpDirection::Forward => self.step_forward(),
        };
        self.debug_check();
        step
    }

    /// Moves to the next older entry.
    fn step_backward(&mut self, current: JumpEntry) -> JumpStep {
        let index = match self.cursor {
            JumpCursor::Inside { index } => index,
            JumpCursor::Newest => {
                if self.entries.is_empty() {
                    return JumpStep::AtOldest;
                }
                self.record(current);
                debug_assert!(
                    !self.entries.is_empty(),
                    "record either replaces the newest entry or appends one, so it always leaves one"
                );
                // The recorded position now sits at the newest end, so the
                // traversal stands on it and the next older entry lies before
                // it. A record that replaced the only entry leaves nothing
                // older.
                self.entries.len() - 1
            }
        };
        let Some(older) = index.checked_sub(1) else {
            return JumpStep::AtOldest;
        };
        self.cursor = JumpCursor::Inside { index: older };
        JumpStep::Moved(self.entries[older].clone())
    }

    /// Moves to the next newer entry.
    fn step_forward(&mut self) -> JumpStep {
        let JumpCursor::Inside { index } = self.cursor else {
            return JumpStep::AtNewest;
        };
        let newer = index + 1;
        if newer >= self.entries.len() {
            return JumpStep::AtNewest;
        }
        self.cursor = JumpCursor::Inside { index: newer };
        JumpStep::Moved(self.entries[newer].clone())
    }

    /// Adds one entry at the newest end and holds the bound.
    ///
    /// Every held entry that names the same line of the same buffer drops
    /// first, so the list never holds one line twice and a walk never stops on
    /// that line twice. Vim keeps one entry for each line in the same way. The
    /// scan is bounded by [`JUMPS_MAX`] and runs once for one jump, never once
    /// for one key.
    ///
    /// Only a caller that holds no traversal index may call this function,
    /// because the drop moves the remaining entries.
    fn record(&mut self, entry: JumpEntry) {
        self.entries.retain(|held| !held.repeats(&entry));
        if self.entries.len() == JUMPS_MAX {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// Checks the invariants that construction alone guarantees.
    fn debug_check(&self) {
        debug_assert!(
            self.entries.len() <= JUMPS_MAX,
            "record drops the oldest entry before every append, so the list cannot pass the bound"
        );
        if let JumpCursor::Inside { index } = self.cursor {
            debug_assert!(
                index < self.entries.len(),
                "a step names an existing entry, and only a push shortens the list after it clears the traversal"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{JUMPS_MAX, JumpDirection, JumpEntry, JumpList, JumpStep};

    use kvim_workspace::BufferId;

    /// Returns one entry that names a buffer and a line.
    fn entry(buffer: u32, line: usize) -> JumpEntry {
        JumpEntry::new(BufferId::new(buffer), None, line, 0)
    }

    #[test]
    fn a_push_discards_the_forward_history() {
        let mut jumps = JumpList::default();
        jumps.push(entry(1, 10));
        jumps.push(entry(1, 20));
        jumps.push(entry(1, 30));

        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 40)),
            JumpStep::Moved(entry(1, 30))
        );
        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 30)),
            JumpStep::Moved(entry(1, 20))
        );

        // The jump taken from inside the history drops lines 30 and 40.
        jumps.push(entry(1, 25));
        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 25)),
            JumpStep::Moved(entry(1, 20))
        );
        assert_eq!(
            jumps.step(JumpDirection::Forward, entry(1, 20)),
            JumpStep::Moved(entry(1, 25))
        );
        assert_eq!(
            jumps.step(JumpDirection::Forward, entry(1, 25)),
            JumpStep::AtNewest
        );
    }

    #[test]
    fn a_push_that_repeats_the_current_line_replaces_the_entry() {
        let buffer = BufferId::new(1);
        let mut jumps = JumpList::default();
        jumps.push(JumpEntry::new(buffer, None, 10, 0));
        jumps.push(JumpEntry::new(buffer, None, 10, 7));

        // The list holds the second column and one entry, not two.
        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 50)),
            JumpStep::Moved(JumpEntry::new(buffer, None, 10, 7))
        );
        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 10)),
            JumpStep::AtOldest
        );
    }

    #[test]
    fn a_push_drops_an_older_entry_that_names_the_same_line() {
        let mut jumps = JumpList::default();
        jumps.push(entry(1, 10));
        jumps.push(entry(1, 20));
        // Line 10 already sits in the list behind line 20, so the list keeps
        // the new entry alone and a walk stops on line 10 exactly once.
        jumps.push(entry(1, 10));

        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 99)),
            JumpStep::Moved(entry(1, 10))
        );
        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 10)),
            JumpStep::Moved(entry(1, 20))
        );
        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 20)),
            JumpStep::AtOldest
        );
    }

    #[test]
    fn the_bound_drops_the_oldest_entry() {
        let mut jumps = JumpList::default();
        for line in 1..=JUMPS_MAX {
            jumps.push(entry(1, line));
        }
        jumps.push(entry(1, JUMPS_MAX + 1));

        // The first step repeats the newest line, so it records nothing new and
        // the walk sees the list exactly as the pushes left it.
        let mut current = entry(1, JUMPS_MAX + 1);
        let mut steps = 0_usize;
        let mut oldest = None;
        loop {
            match jumps.step(JumpDirection::Backward, current) {
                JumpStep::Moved(reached) => {
                    steps += 1;
                    assert!(steps <= JUMPS_MAX, "the bound must stop the walk");
                    current = reached.clone();
                    oldest = Some(reached);
                }
                JumpStep::AtOldest => break,
                JumpStep::AtNewest => panic!("a backward step never reaches the newest end"),
            }
        }

        assert_eq!(steps, JUMPS_MAX - 1);
        assert_eq!(oldest.map(|reached| reached.line()), Some(2));
    }

    #[test]
    fn a_backward_step_and_a_forward_step_return_to_the_start() {
        let mut jumps = JumpList::default();
        jumps.push(entry(1, 10));

        let start = entry(2, 50);
        assert_eq!(
            jumps.step(JumpDirection::Backward, start.clone()),
            JumpStep::Moved(entry(1, 10))
        );
        assert_eq!(
            jumps.step(JumpDirection::Forward, entry(1, 10)),
            JumpStep::Moved(start)
        );
    }

    #[test]
    fn a_backward_step_at_the_oldest_entry_reports_that_end() {
        let mut jumps = JumpList::default();
        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 5)),
            JumpStep::AtOldest
        );

        jumps.push(entry(1, 10));
        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 50)),
            JumpStep::Moved(entry(1, 10))
        );
        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 10)),
            JumpStep::AtOldest
        );
    }

    #[test]
    fn a_forward_step_at_the_newest_entry_reports_that_end() {
        let mut jumps = JumpList::default();
        jumps.push(entry(1, 10));
        assert_eq!(
            jumps.step(JumpDirection::Forward, entry(1, 50)),
            JumpStep::AtNewest
        );

        assert_eq!(
            jumps.step(JumpDirection::Backward, entry(1, 50)),
            JumpStep::Moved(entry(1, 10))
        );
        assert_eq!(
            jumps.step(JumpDirection::Forward, entry(1, 10)),
            JumpStep::Moved(entry(1, 50))
        );
        assert_eq!(
            jumps.step(JumpDirection::Forward, entry(1, 50)),
            JumpStep::AtNewest
        );
    }
}
