//! The screen rows of one aligned hunk.
//!
//! A hunk publishes one sequence of lines, where a removed line and the added
//! line that replaces it follow each other. A two-column view needs them beside
//! each other instead, so this module pairs them into rows.
//!
//! The module is pure. One hunk always produces one row sequence, and the rows
//! borrow the published lines instead of copying them. An inline view reads the
//! same rows and draws one column, so both views agree about what one hunk
//! holds. See `docs/diff-view.md`.

use super::diff::{DiffLine, DiffSide, Hunk, LineOrigin};

/// One screen row of one aligned hunk.
///
/// A context line stands on both sides, so both fields name the same published
/// line. A replaced line pairs the removed line with the added line that takes
/// its place. A surplus on either side pairs with nothing, and the empty side
/// draws as a gap.
///
/// # Examples
///
/// ```
/// use kvim_workspace::AlignedRow;
///
/// // The kind of one row follows from which sides it holds.
/// fn describes(row: &AlignedRow<'_>) -> &'static str {
///     match (row.old().is_some(), row.new().is_some()) {
///         (true, true) => "context or replacement",
///         (true, false) => "removal",
///         (false, true) => "addition",
///         (false, false) => "no row holds neither side",
///     }
/// }
/// # let _ = describes;
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignedRow<'a> {
    old: Option<&'a DiffLine>,
    new: Option<&'a DiffLine>,
}

impl<'a> AlignedRow<'a> {
    /// Returns the line that the old side draws, or `None` for a gap.
    #[must_use]
    pub const fn old(&self) -> Option<&'a DiffLine> {
        self.old
    }

    /// Returns the line that the new side draws, or `None` for a gap.
    #[must_use]
    pub const fn new(&self) -> Option<&'a DiffLine> {
        self.new
    }

    /// Returns the line that one side draws, or `None` for a gap.
    #[must_use]
    pub const fn side(&self, side: DiffSide) -> Option<&'a DiffLine> {
        match side {
            DiffSide::Old => self.old,
            DiffSide::New => self.new,
        }
    }

    /// Reports whether both sides draw the same unchanged line.
    ///
    /// An inline view draws such a row once. A two-column view draws it twice,
    /// once in each column.
    #[must_use]
    pub fn is_context(&self) -> bool {
        match (self.old, self.new) {
            (Some(old), Some(new)) => {
                std::ptr::eq(old, new) && matches!(old.origin(), LineOrigin::Context { .. })
            }
            _ => false,
        }
    }
}

/// Pairs the published lines of one hunk into screen rows.
///
/// A context line takes one row on both sides. A run of removed lines pairs one
/// for one with the run of added lines that follows it, because a replacement
/// reads best beside the text that it replaced. A surplus on either side takes
/// its own row with a gap opposite it.
///
/// The row count never passes the published line count of the longer side, so a
/// truncated hunk aligns exactly what it published and invents no row.
///
/// # Examples
///
/// ```
/// # use kvim_workspace::{
/// #     DiffLine, DiffLineText, Hunk, HunkId, LineEnding, LineOrigin, NewLine, NewLineRange,
/// #     OldLine, OldLineRange, align_hunk,
/// # };
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let line = |origin| Ok::<_, Box<dyn std::error::Error>>(
/// #     DiffLine::new(origin, DiffLineText::new(b"x".to_vec())?, LineEnding::Newline),
/// # );
/// let lines = vec![
///     line(LineOrigin::Removed { old: OldLine::new(1)? })?,
///     line(LineOrigin::Added { new: NewLine::new(1)? })?,
/// ];
/// let hunk = Hunk::new(
///     HunkId::new(1),
///     OldLineRange::new(OldLine::new(1)?, 1)?,
///     NewLineRange::new(NewLine::new(1)?, 1)?,
///     lines,
/// )?;
///
/// // The removal and the addition share one row, so the replacement reads
/// // beside the text that it replaced.
/// let rows = align_hunk(&hunk);
/// assert_eq!(rows.len(), 1);
/// assert!(rows[0].old().is_some() && rows[0].new().is_some());
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn align_hunk(hunk: &Hunk) -> Vec<AlignedRow<'_>> {
    let lines = hunk.lines();
    let mut rows: Vec<AlignedRow<'_>> = Vec::with_capacity(lines.len());
    let mut index = 0;

    while index < lines.len() {
        match lines[index].origin() {
            LineOrigin::Context { .. } => {
                let line = &lines[index];
                rows.push(AlignedRow {
                    old: Some(line),
                    new: Some(line),
                });
                index += 1;
            }
            LineOrigin::Removed { .. } | LineOrigin::Added { .. } => {
                let removed_start = index;
                while matches!(
                    lines.get(index).map(DiffLine::origin),
                    Some(LineOrigin::Removed { .. })
                ) {
                    index += 1;
                }
                let removed = &lines[removed_start..index];

                let added_start = index;
                while matches!(
                    lines.get(index).map(DiffLine::origin),
                    Some(LineOrigin::Added { .. })
                ) {
                    index += 1;
                }
                let added = &lines[added_start..index];

                pair_runs(&mut rows, removed, added);
            }
        }
    }
    rows
}

/// Pairs one run of removed lines with the run of added lines that follows it.
fn pair_runs<'a>(rows: &mut Vec<AlignedRow<'a>>, removed: &'a [DiffLine], added: &'a [DiffLine]) {
    let paired = removed.len().min(added.len());
    for offset in 0..paired {
        rows.push(AlignedRow {
            old: Some(&removed[offset]),
            new: Some(&added[offset]),
        });
    }
    for line in &removed[paired..] {
        rows.push(AlignedRow {
            old: Some(line),
            new: None,
        });
    }
    for line in &added[paired..] {
        rows.push(AlignedRow {
            old: None,
            new: Some(line),
        });
    }
}

#[cfg(test)]
#[path = "align_tests.rs"]
mod tests;
