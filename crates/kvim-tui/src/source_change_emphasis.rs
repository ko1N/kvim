//! Generic source-change emphasis state.
//!
//! The model contains only facade-validated values. It changes no source text.

use kvim_path::WorktreeRelativePath;

/// Why a source-change emphasis operation changed no emphasis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceChangeEmphasisRefusal {
    /// The editor already closed.
    NoEditor,
    /// The current target buffer holds unsaved text.
    DirtyBuffer,
    /// Another active file holds unsaved text.
    DifferentDirtyBuffer,
    /// Another file operation already uses the bounded file lane.
    Busy,
    /// A range leaves the target buffer.
    RangeOutsideBuffer,
    /// The requested file could not be opened or reloaded.
    OpenFailed,
    /// A newer request or explicit clear superseded this request.
    Obsolete,
    /// The request identity space is exhausted for this editor.
    Exhausted,
}

/// The non-reusing identity lifecycle for source-change requests.
///
/// `None` is a permanent tombstone. Reaching it invalidates every queued
/// identity and refuses all future requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SourceChangeGeneration(Option<u64>);

impl SourceChangeGeneration {
    /// Creates the lifecycle before its first request.
    #[must_use]
    pub(super) const fn new() -> Self {
        Self(Some(0))
    }

    /// Allocates the next identity without wrapping or reuse.
    pub(super) fn allocate(&mut self) -> Result<u64, SourceChangeEmphasisRefusal> {
        let Some(current) = self.0 else {
            return Err(SourceChangeEmphasisRefusal::Exhausted);
        };
        let Some(next) = current.checked_add(1) else {
            self.0 = None;
            return Err(SourceChangeEmphasisRefusal::Exhausted);
        };
        self.0 = Some(next);
        Ok(next)
    }

    /// Invalidates every identity allocated before this transition.
    pub(super) fn invalidate(&mut self) {
        self.0 = self.0.and_then(|current| current.checked_add(1));
    }

    /// Reports whether one completion still owns the current identity.
    #[must_use]
    pub(super) fn is_current(self, generation: u64) -> bool {
        self.0 == Some(generation)
    }

    #[cfg(test)]
    pub(super) fn set_for_test(&mut self, generation: u64) {
        self.0 = Some(generation);
    }
}

/// One zero-based inclusive source line range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceChangeRange {
    first_line: usize,
    last_line: usize,
}

impl SourceChangeRange {
    /// Creates a range from facade-validated values.
    #[must_use]
    pub fn new(first_line: usize, last_line: usize) -> Self {
        debug_assert!(
            first_line <= last_line,
            "the facade validates ordered ranges"
        );
        Self {
            first_line,
            last_line,
        }
    }

    /// Returns the first zero-based line.
    #[must_use]
    pub const fn first_line(self) -> usize {
        self.first_line
    }

    /// Returns the last zero-based line.
    #[must_use]
    pub const fn last_line(self) -> usize {
        self.last_line
    }
}

/// One private validated emphasis request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceChangeEmphasis {
    path: WorktreeRelativePath,
    ranges: Vec<SourceChangeRange>,
}

impl SourceChangeEmphasis {
    /// Creates emphasis from facade-validated values.
    #[must_use]
    pub fn new(path: WorktreeRelativePath, ranges: Vec<SourceChangeRange>) -> Self {
        debug_assert!(!ranges.is_empty(), "the facade requires one change range");
        Self { path, ranges }
    }

    /// Returns the contained path.
    #[must_use]
    pub const fn path(&self) -> &WorktreeRelativePath {
        &self.path
    }

    /// Returns all emphasized ranges in supplied order.
    #[must_use]
    pub fn ranges(&self) -> &[SourceChangeRange] {
        &self.ranges
    }

    /// Reports whether all ranges fit one buffer.
    #[must_use]
    pub fn fits(&self, line_count: usize) -> bool {
        self.ranges.iter().all(|range| range.last_line < line_count)
    }
}
