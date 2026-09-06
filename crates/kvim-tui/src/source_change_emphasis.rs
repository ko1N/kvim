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
