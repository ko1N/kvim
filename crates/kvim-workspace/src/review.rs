//! The pure review state of one worktree diff and its bounded events.
//!
//! [`ReviewState`] holds one complete candidate, one cursor over the published
//! hunks, one optional [`ReviewAnchor`], and one bounded event queue. Every
//! function of this module is deterministic. No function reads a repository,
//! starts a process, reads a clock, or touches the filesystem. The bounded
//! capture supplies every candidate.
//!
//! Submission never trusts the candidate that the reader sees. The host
//! captures the target again, derives one [`TargetAuthority`] from the result,
//! and hands that value to [`ReviewState::submit_comment`]. The re-captured
//! base, target, path, mode, side-byte digest, and revision must all match the
//! active candidate. A changed target returns [`SubmitCommentError::Stale`] and
//! queues no event.
//!
//! kvim assigns no host meaning to a review comment and keeps no comment. One
//! successful submission queues one [`ReviewEvent::CommentSubmitted`] with the
//! durable anchor and the bounded body, and the host decides the effect. A full
//! queue returns [`SubmitCommentError::Saturated`] before the submission
//! starts, so the review drops no comment. See `docs/git.md`.
//!
//! `crates/kvim-tui/examples/worktree_diff_review.rs` runs the complete path
//! against one temporary repository: it captures one candidate, submits one
//! comment, edits the file, and relocates the anchor.
//!
//! # Examples
//!
//! ```
//! # use kvim_path::WorktreeRelativePath;
//! # use kvim_workspace::*;
//! # fn candidate(text: &[&str]) -> Result<WorktreeDiff, Box<dyn std::error::Error>> {
//! #     let side = FileSide::new(WorktreeRelativePath::new("a.txt")?, FileMode::Regular);
//! #     let lines = text
//! #         .iter()
//! #         .enumerate()
//! #         .map(|(index, value)| {
//! #             Ok(DiffLine::new(
//! #                 LineOrigin::Added {
//! #                     new: NewLine::new(u32::try_from(index)? + 1)?,
//! #                 },
//! #                 DiffLineText::new(value.as_bytes())?,
//! #                 LineEnding::Newline,
//! #             ))
//! #         })
//! #         .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
//! #     let hunk = Hunk::new(
//! #         HunkId::new(0),
//! #         OldLineRange::new(OldLine::new(1)?, 0)?,
//! #         NewLineRange::new(NewLine::new(1)?, u32::try_from(text.len())?)?,
//! #         lines,
//! #     )?;
//! #     let file = FileDiff::new(
//! #         DiffChange::Added { new: side },
//! #         DiffContent::Text(TextDiff::new(vec![hunk], DiffTruncation::Complete)?),
//! #     )?;
//! #     let authority =
//! #         CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest([7; 32]));
//! #     Ok(WorktreeDiff::new(
//! #         BaseRevision::new("0123456789abcdef0123456789abcdef01234567")?,
//! #         DiffTarget::Worktree,
//! #         &authority,
//! #         vec![file],
//! #         DiffTruncation::Complete,
//! #     )?)
//! # }
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let published = candidate(&["one", "two"])?;
//! let authority = TargetAuthority::of(&published);
//! let mut review = ReviewState::new(published);
//!
//! // The reader selects the second new-side line of the hunk at the cursor.
//! review.select(DiffSide::New, 2, 1)?;
//!
//! // The host captured the target again and found no change.
//! review.submit_comment(CommentBody::new("the name hides the unit")?, &authority)?;
//!
//! let Some(ReviewEvent::CommentSubmitted { anchor, body }) = review.take_event() else {
//!     unreachable!("one accepted submission queues one event")
//! };
//! assert_eq!(anchor.location().first(), 2);
//! assert_eq!(body.as_str(), "the name hides the unit");
//! # Ok(())
//! # }
//! ```

use std::collections::VecDeque;
use std::iter;

use kvim_path::WorktreeRelativePath;
use thiserror::Error;

use crate::diff::{
    AnchorLocation, BaseRevision, CommentBody, DiffContent, DiffLimit, DiffLine, DiffRevision,
    DiffSide, DiffTarget, DiffTruncation, FileDiff, Hunk, HunkId, LineNumberError, LineRangeError,
    NewLine, NewLineRange, OldLine, OldLineRange, Relocation, ReviewAnchor, ReviewAnchorError,
    TextDiff, WorktreeDiff, relocate,
};
use crate::diff_capture::AuthorityProjection;

/// The largest number of review events that the queue holds at one time.
///
/// The host drains the queue after every reduction, so this many comments
/// between two drains is far above the rate that one reader produces. A full
/// queue returns [`SubmitCommentError::Saturated`] instead of dropping a
/// comment.
pub const REVIEW_EVENTS_MAX: usize = 64;

// ---------------------------------------------------------------------------
// The target authority
// ---------------------------------------------------------------------------

/// The identity of one captured review target.
///
/// The value names the base commit, the target, the authority projection, and
/// the revision of one candidate. The projection covers the path, the published
/// mode, and the exact side bytes of every collected file, and the revision
/// additionally covers the commit of `HEAD` and the index. Two captures of one
/// unchanged target therefore share one value.
///
/// [`ReviewState::submit_comment`] compares the value of the active candidate
/// with the value of a fresh capture, so a comment never reaches the host
/// against a location that moved. See `docs/git.md`.
///
/// # Examples
///
/// ```
/// # use kvim_workspace::{
/// #     BaseRevision, CandidateAuthority, DiffTarget, DiffTruncation, HeadAuthority,
/// #     IndexAuthority, TargetAuthority, WorktreeDiff,
/// # };
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let base = BaseRevision::new("0123456789abcdef0123456789abcdef01234567")?;
/// let authority =
///     CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest([0; 32]));
/// let empty = WorktreeDiff::new(
///     base,
///     DiffTarget::Worktree,
///     &authority,
///     Vec::new(),
///     DiffTruncation::Complete,
/// )?;
///
/// assert_eq!(TargetAuthority::of(&empty), TargetAuthority::of(&empty));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetAuthority {
    base: BaseRevision,
    target: DiffTarget,
    projection: AuthorityProjection,
    revision: DiffRevision,
}

impl TargetAuthority {
    /// Derives the authority of one captured candidate.
    #[must_use]
    pub fn of(candidate: &WorktreeDiff) -> Self {
        Self {
            base: candidate.base(),
            target: candidate.target().clone(),
            projection: AuthorityProjection::of(candidate),
            revision: candidate.revision(),
        }
    }

    /// Returns the commit that the capture compared against.
    #[must_use]
    pub const fn base(&self) -> BaseRevision {
        self.base
    }

    /// Returns the selection that produced the capture.
    #[must_use]
    pub const fn target(&self) -> &DiffTarget {
        &self.target
    }

    /// Returns the revision of the capture.
    #[must_use]
    pub const fn revision(&self) -> DiffRevision {
        self.revision
    }

    /// Names the first fact that a later authority no longer holds.
    ///
    /// The comparison runs from the widest fact to the narrowest one, so the
    /// answer names the reason that a reader can act on.
    fn drift(&self, later: &Self) -> Option<StaleLocation> {
        if self.base != later.base || self.target != later.target {
            return Some(StaleLocation::Target);
        }
        if self.projection != later.projection {
            return Some(StaleLocation::Content);
        }
        if self.revision != later.revision {
            return Some(StaleLocation::Revision);
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// One fact that the review publishes to the host.
///
/// kvim keeps no comment and gives no comment a host meaning. The host reads
/// the event and decides its effect. See `docs/git.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewEvent {
    /// One reader submitted one comment against one durable location.
    CommentSubmitted {
        /// The place that the comment names, inside the verified candidate.
        anchor: ReviewAnchor,
        /// The bounded text of the comment.
        body: CommentBody,
    },
}

/// The fact that a fresh capture no longer holds.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StaleLocation {
    /// The fresh capture names another base commit or another target.
    ///
    /// Such a capture proves nothing about the active candidate, so the
    /// submission stops.
    Target,
    /// The fresh capture publishes another path, another mode, or another side
    /// byte.
    Content,
    /// The fresh capture publishes another revision.
    Revision,
}

/// The reasons that one comment reached no event.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SubmitCommentError {
    /// The event queue holds no free slot.
    ///
    /// The review keeps every queued event and drops no comment. The host
    /// drains the queue and submits the comment again.
    #[error("the review event queue holds {REVIEW_EVENTS_MAX} events")]
    Saturated,
    /// The review holds no selected lines.
    #[error("a comment names one selection")]
    NoSelection,
    /// The fresh capture no longer holds the selected location.
    #[error("the review target changed before the submission")]
    Stale(StaleLocation),
}

// ---------------------------------------------------------------------------
// Navigation and rendering
// ---------------------------------------------------------------------------

/// The outcome of one cursor step over the published hunks.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HunkStep {
    /// The cursor names another hunk, and the review holds no selection.
    Moved,
    /// The candidate publishes no further hunk in that direction.
    AtBorder,
}

/// The hunk that the review cursor names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReviewCursor<'a> {
    /// The file that publishes the hunk.
    pub file: &'a FileDiff,
    /// The hunk at the cursor.
    pub hunk: &'a Hunk,
}

/// One published row of the review.
///
/// The rows carry the exact published values. Omitted content publishes no
/// [`ReviewRow::Line`], so a reader sees the bound that stopped the collection
/// and can select nothing that the candidate does not hold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewRow<'a> {
    /// The header of one changed file.
    File {
        /// The changed file.
        file: &'a FileDiff,
    },
    /// The header of one hunk.
    Hunk {
        /// The path that publishes the hunk.
        path: &'a WorktreeRelativePath,
        /// The hunk.
        hunk: &'a Hunk,
    },
    /// One published line of one hunk.
    Line {
        /// The path that publishes the line.
        path: &'a WorktreeRelativePath,
        /// The hunk that publishes the line.
        hunk: HunkId,
        /// The line.
        line: &'a DiffLine,
    },
    /// One bound stopped the collection above this row.
    Truncated {
        /// The bound that stopped the collection.
        limit: DiffLimit,
    },
}

// ---------------------------------------------------------------------------
// The review state
// ---------------------------------------------------------------------------

/// The place of the cursor inside the published candidate.
///
/// Both indexes address the collections of one candidate, so every change of
/// the candidate rebuilds the value instead of moving it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HunkCursor {
    file: usize,
    hunk: usize,
}

/// The review of one published worktree diff candidate.
///
/// The state owns one complete candidate. [`ReviewState::reload`] replaces that
/// candidate as one value and relocates the selection through the pure
/// [`relocate`] API, so no part of an earlier candidate survives a reload.
#[derive(Clone, Debug)]
pub struct ReviewState {
    candidate: WorktreeDiff,
    authority: TargetAuthority,
    cursor: Option<HunkCursor>,
    selection: Option<ReviewAnchor>,
    events: VecDeque<ReviewEvent>,
}

impl ReviewState {
    /// Opens the review of one published candidate.
    ///
    /// The cursor names the first published hunk of the candidate. A candidate
    /// that publishes no hunk leaves the cursor empty.
    #[must_use]
    pub fn new(candidate: WorktreeDiff) -> Self {
        let authority = TargetAuthority::of(&candidate);
        let cursor = first_cursor(candidate.files(), 0);
        Self {
            candidate,
            authority,
            cursor,
            selection: None,
            events: VecDeque::new(),
        }
    }

    /// Returns the published candidate.
    #[must_use]
    pub const fn candidate(&self) -> &WorktreeDiff {
        &self.candidate
    }

    /// Returns the authority of the published candidate.
    #[must_use]
    pub const fn authority(&self) -> &TargetAuthority {
        &self.authority
    }

    /// Returns the hunk that the cursor names.
    #[must_use]
    pub fn cursor(&self) -> Option<ReviewCursor<'_>> {
        let cursor = self.cursor?;
        let file = self.candidate.files().get(cursor.file)?;
        let hunk = hunks_of(file).get(cursor.hunk)?;
        Some(ReviewCursor { file, hunk })
    }

    /// Returns the selected location.
    #[must_use]
    pub const fn selection(&self) -> Option<&ReviewAnchor> {
        self.selection.as_ref()
    }

    /// Publishes every row of the candidate in reading order.
    ///
    /// The stream names each file, each hunk, each published line, and each
    /// bound that stopped a collection. Truncated content publishes no line, so
    /// a reader can select nothing that the candidate omitted.
    pub fn rows(&self) -> impl Iterator<Item = ReviewRow<'_>> {
        let files = self.candidate.files().iter().flat_map(|file| {
            let body = text_of(file).into_iter().flat_map(move |text| {
                let hunks = text.hunks().iter().flat_map(move |hunk| {
                    iter::once(ReviewRow::Hunk {
                        path: file.path(),
                        hunk,
                    })
                    .chain(hunk.lines().iter().map(move |line| {
                        ReviewRow::Line {
                            path: file.path(),
                            hunk: hunk.id(),
                            line,
                        }
                    }))
                });
                hunks.chain(truncated_row(text.truncation()))
            });
            iter::once(ReviewRow::File { file }).chain(body)
        });
        files.chain(truncated_row(self.candidate.truncation()))
    }

    /// Moves the cursor to the next published hunk of the candidate.
    ///
    /// The step crosses into the next file that publishes a hunk and skips
    /// every file that publishes none. A step clears the selection, because a
    /// selection always names the hunk at the cursor.
    pub fn next_hunk(&mut self) -> HunkStep {
        let Some(cursor) = self.cursor else {
            return HunkStep::AtBorder;
        };
        let files = self.candidate.files();
        let count = files
            .get(cursor.file)
            .map_or(0, |file| hunks_of(file).len());
        if cursor.hunk + 1 < count {
            return self.place(HunkCursor {
                file: cursor.file,
                hunk: cursor.hunk + 1,
            });
        }
        match first_cursor(files, cursor.file + 1) {
            Some(next) => self.place(next),
            None => HunkStep::AtBorder,
        }
    }

    /// Moves the cursor to the previous published hunk of the candidate.
    ///
    /// The step behaves like [`ReviewState::next_hunk`] in the other direction.
    pub fn previous_hunk(&mut self) -> HunkStep {
        let Some(cursor) = self.cursor else {
            return HunkStep::AtBorder;
        };
        if let Some(hunk) = cursor.hunk.checked_sub(1) {
            return self.place(HunkCursor {
                file: cursor.file,
                hunk,
            });
        }
        match last_cursor(self.candidate.files(), cursor.file) {
            Some(previous) => self.place(previous),
            None => HunkStep::AtBorder,
        }
    }

    /// Selects one run of lines on one side of the hunk at the cursor.
    ///
    /// The side decides which line numbers the run names, so an old-side run
    /// and a new-side run of one hunk stay separate coordinates. The candidate
    /// must publish every named line, so a run that reaches omitted content
    /// returns [`ReviewAnchorError::LinesMissing`].
    pub fn select(
        &mut self,
        side: DiffSide,
        first: u32,
        count: u32,
    ) -> Result<&ReviewAnchor, ReviewSelectError> {
        let cursor = self.cursor().ok_or(ReviewSelectError::NoHunk)?;
        let path = cursor.file.path().clone();
        let hunk = cursor.hunk.id();
        let location = match side {
            DiffSide::Old => AnchorLocation::Old {
                range: OldLineRange::new(OldLine::new(first)?, count)?,
            },
            DiffSide::New => AnchorLocation::New {
                range: NewLineRange::new(NewLine::new(first)?, count)?,
            },
        };

        let anchor = ReviewAnchor::select(&self.candidate, &path, hunk, location)?;
        Ok(self.selection.insert(anchor))
    }

    /// Drops the selection and keeps the cursor.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Replaces the candidate with one complete later candidate.
    ///
    /// The state takes the new candidate, its authority, and a cursor over its
    /// own hunks. A selection follows through [`relocate`]: an exact or a
    /// relocated match becomes the selection of the new candidate, and a
    /// missing or an ambiguous outcome clears it, because the review never
    /// guesses a place. The answer is [`None`] when the review held no
    /// selection.
    pub fn reload(&mut self, candidate: WorktreeDiff) -> Option<Relocation> {
        let outcome = self
            .selection
            .as_ref()
            .map(|anchor| relocate(anchor, &candidate));

        self.authority = TargetAuthority::of(&candidate);
        self.selection = match &outcome {
            Some(Relocation::Exact { anchor } | Relocation::Relocated { anchor }) => {
                Some(anchor.clone())
            }
            Some(Relocation::Missing | Relocation::Ambiguous(_)) | None => None,
        };
        self.cursor = match &self.selection {
            Some(anchor) => cursor_of(&candidate, anchor),
            None => first_cursor(candidate.files(), 0),
        };
        self.candidate = candidate;
        outcome
    }

    /// Submits one comment against the selected location.
    ///
    /// The caller passes the authority of a fresh capture of the same target.
    /// The method reserves the queue slot first, so a full queue answers
    /// [`SubmitCommentError::Saturated`] before the host starts the capture and
    /// no comment disappears. The re-captured base, target, path, mode,
    /// side-byte digest, and revision must then all match the active candidate.
    /// A changed target answers [`SubmitCommentError::Stale`] and queues no
    /// event.
    ///
    /// One accepted submission queues one
    /// [`ReviewEvent::CommentSubmitted`] with the durable anchor and the
    /// bounded body. kvim keeps no comment and gives it no host meaning.
    pub fn submit_comment(
        &mut self,
        body: CommentBody,
        captured: &TargetAuthority,
    ) -> Result<(), SubmitCommentError> {
        if self.events.len() >= REVIEW_EVENTS_MAX {
            return Err(SubmitCommentError::Saturated);
        }
        let anchor = self
            .selection
            .clone()
            .ok_or(SubmitCommentError::NoSelection)?;
        if let Some(drift) = self.authority.drift(captured) {
            return Err(SubmitCommentError::Stale(drift));
        }

        self.events
            .push_back(ReviewEvent::CommentSubmitted { anchor, body });
        debug_assert!(
            self.events.len() <= REVIEW_EVENTS_MAX,
            "the reservation above rejects a submission into a full queue"
        );
        Ok(())
    }

    /// Returns the number of queued events.
    #[must_use]
    pub fn queued_events(&self) -> usize {
        self.events.len()
    }

    /// Takes the oldest queued event.
    pub fn take_event(&mut self) -> Option<ReviewEvent> {
        self.events.pop_front()
    }

    /// Places the cursor and drops the selection of the earlier hunk.
    fn place(&mut self, cursor: HunkCursor) -> HunkStep {
        self.cursor = Some(cursor);
        self.selection = None;
        HunkStep::Moved
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Returns the published text of one file, or [`None`] for every other kind.
fn text_of(file: &FileDiff) -> Option<&TextDiff> {
    match file.content() {
        DiffContent::Text(text) => Some(text),
        DiffContent::Binary
        | DiffContent::SymbolicLink
        | DiffContent::Submodule
        | DiffContent::Unsupported => None,
    }
}

/// Returns the published hunks of one file.
fn hunks_of(file: &FileDiff) -> &[Hunk] {
    text_of(file).map_or(&[], TextDiff::hunks)
}

/// Returns the row of one bound that stopped a collection.
fn truncated_row<'a>(truncation: DiffTruncation) -> Option<ReviewRow<'a>> {
    match truncation {
        DiffTruncation::Complete => None,
        DiffTruncation::Truncated(limit) => Some(ReviewRow::Truncated { limit }),
    }
}

/// Returns the first hunk of the first file at or after one index.
fn first_cursor(files: &[FileDiff], from: usize) -> Option<HunkCursor> {
    files
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, file)| !hunks_of(file).is_empty())
        .map(|(file, _)| HunkCursor { file, hunk: 0 })
}

/// Returns the last hunk of the last file before one index.
fn last_cursor(files: &[FileDiff], before: usize) -> Option<HunkCursor> {
    files
        .iter()
        .enumerate()
        .take(before)
        .rfind(|(_, file)| !hunks_of(file).is_empty())
        .map(|(file, published)| HunkCursor {
            file,
            hunk: hunks_of(published).len() - 1,
        })
}

/// Returns the cursor of the hunk that one anchor names.
fn cursor_of(candidate: &WorktreeDiff, anchor: &ReviewAnchor) -> Option<HunkCursor> {
    let file = candidate
        .files()
        .iter()
        .position(|file| file.change().names(anchor.path()))?;
    let hunk = hunks_of(&candidate.files()[file])
        .iter()
        .position(|hunk| hunk.id() == anchor.hunk())?;
    Some(HunkCursor { file, hunk })
}

/// The reasons that one candidate cannot hold a selection.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ReviewSelectError {
    /// The cursor names no hunk of the candidate.
    #[error("the candidate publishes no hunk at the cursor")]
    NoHunk,
    /// The run starts outside the published line numbers.
    #[error(transparent)]
    Line(#[from] LineNumberError),
    /// The run is not one usable range.
    #[error(transparent)]
    Range(#[from] LineRangeError),
    /// The candidate cannot anchor the run.
    #[error(transparent)]
    Anchor(#[from] ReviewAnchorError),
}

#[cfg(test)]
mod tests {
    use kvim_path::WorktreeRelativePath;

    use crate::diff::{
        CandidateAuthority, DiffChange, DiffContent, DiffLine, DiffLineText, DiffTruncation,
        FileMode, FileSide, HeadAuthority, Hunk, HunkId, IndexAuthority, LineEnding, LineOrigin,
        NewLine, NewLineRange, OldLine, OldLineRange, TextDiff,
    };

    use super::*;

    const BASE_HEX: &str = "0123456789abcdef0123456789abcdef01234567";

    fn path(value: &str) -> WorktreeRelativePath {
        WorktreeRelativePath::new(value).expect("the fixture names one contained path")
    }

    /// Builds one added text file whose new side holds the given lines.
    fn added(name: &str, lines: &[&str], truncation: DiffTruncation) -> FileDiff {
        let body: Vec<DiffLine> = lines
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let number = u32::try_from(index).expect("the fixture holds few lines") + 1;
                DiffLine::new(
                    LineOrigin::Added {
                        new: NewLine::new(number).expect("the fixture stays inside the bound"),
                    },
                    DiffLineText::new(text.as_bytes()).expect("the fixture holds few bytes"),
                    LineEnding::Newline,
                )
            })
            .collect();
        let count = u32::try_from(body.len()).expect("the fixture holds few lines");
        let hunk = Hunk::new(
            HunkId::new(0),
            OldLineRange::new(OldLine::new(1).expect("one is inside the bound"), 0)
                .expect("an empty old range is usable"),
            NewLineRange::new(NewLine::new(1).expect("one is inside the bound"), count)
                .expect("the fixture range is usable"),
            body,
        )
        .expect("the fixture hunk realizes its ranges");
        FileDiff::new(
            DiffChange::Added {
                new: FileSide::new(path(name), FileMode::Regular),
            },
            DiffContent::Text(
                TextDiff::new(vec![hunk], truncation).expect("the fixture text is usable"),
            ),
        )
        .expect("the fixture file is usable")
    }

    /// Builds one binary file, which publishes no hunk and no selectable line.
    fn binary(name: &str) -> FileDiff {
        FileDiff::new(
            DiffChange::Added {
                new: FileSide::new(path(name), FileMode::Regular),
            },
            DiffContent::Binary,
        )
        .expect("the fixture file is usable")
    }

    fn candidate(files: Vec<FileDiff>, index: [u8; 32]) -> WorktreeDiff {
        let authority =
            CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest(index));
        WorktreeDiff::new(
            BaseRevision::new(BASE_HEX).expect("the fixture names one full identifier"),
            DiffTarget::Worktree,
            &authority,
            files,
            DiffTruncation::Complete,
        )
        .expect("the fixture candidate is usable")
    }

    fn one_file(lines: &[&str]) -> WorktreeDiff {
        candidate(
            vec![added("a.txt", lines, DiffTruncation::Complete)],
            [7; 32],
        )
    }

    fn body(text: &str) -> CommentBody {
        CommentBody::new(text).expect("the fixture comment holds few bytes")
    }

    #[test]
    fn navigation_skips_every_file_without_a_hunk() {
        let published = candidate(
            vec![
                binary("a.bin"),
                added("b.txt", &["one"], DiffTruncation::Complete),
                binary("c.bin"),
                added("d.txt", &["two"], DiffTruncation::Complete),
            ],
            [1; 32],
        );
        let mut review = ReviewState::new(published);

        assert_eq!(
            review
                .cursor()
                .expect("the candidate publishes a hunk")
                .file
                .path(),
            &path("b.txt")
        );
        assert_eq!(review.next_hunk(), HunkStep::Moved);
        assert_eq!(
            review
                .cursor()
                .expect("the candidate publishes a hunk")
                .file
                .path(),
            &path("d.txt")
        );
        assert_eq!(review.next_hunk(), HunkStep::AtBorder);
        assert_eq!(review.previous_hunk(), HunkStep::Moved);
        assert_eq!(
            review
                .cursor()
                .expect("the candidate publishes a hunk")
                .file
                .path(),
            &path("b.txt")
        );
        assert_eq!(review.previous_hunk(), HunkStep::AtBorder);
    }

    #[test]
    fn a_candidate_without_a_hunk_holds_no_cursor() {
        let mut review = ReviewState::new(candidate(vec![binary("a.bin")], [2; 32]));

        assert!(review.cursor().is_none());
        assert_eq!(review.next_hunk(), HunkStep::AtBorder);
        assert_eq!(
            review.select(DiffSide::New, 1, 1),
            Err(ReviewSelectError::NoHunk)
        );
    }

    #[test]
    fn a_step_drops_the_selection_of_the_earlier_hunk() {
        let published = candidate(
            vec![
                added("a.txt", &["one"], DiffTruncation::Complete),
                added("b.txt", &["two"], DiffTruncation::Complete),
            ],
            [3; 32],
        );
        let mut review = ReviewState::new(published);
        review
            .select(DiffSide::New, 1, 1)
            .expect("the hunk holds the line");

        assert_eq!(review.next_hunk(), HunkStep::Moved);
        assert!(review.selection().is_none());
    }

    #[test]
    fn each_side_keeps_its_own_line_numbers() {
        let published = candidate(vec![modified()], [4; 32]);
        let mut review = ReviewState::new(published);

        let old = review
            .select(DiffSide::Old, 1, 1)
            .expect("the hunk publishes the old line")
            .clone();
        let new = review
            .select(DiffSide::New, 1, 1)
            .expect("the hunk publishes the new line")
            .clone();

        assert_eq!(old.side(), DiffSide::Old);
        assert_eq!(new.side(), DiffSide::New);
        assert_ne!(old.selection(), new.selection());
    }

    /// Builds one modified file whose two sides hold different bytes.
    fn modified() -> FileDiff {
        let side = FileSide::new(path("a.txt"), FileMode::Regular);
        let hunk = Hunk::new(
            HunkId::new(0),
            OldLineRange::new(OldLine::new(1).expect("one is inside the bound"), 1)
                .expect("the fixture range is usable"),
            NewLineRange::new(NewLine::new(1).expect("one is inside the bound"), 1)
                .expect("the fixture range is usable"),
            vec![
                DiffLine::new(
                    LineOrigin::Removed {
                        old: OldLine::new(1).expect("one is inside the bound"),
                    },
                    DiffLineText::new(*b"old").expect("the fixture holds few bytes"),
                    LineEnding::Newline,
                ),
                DiffLine::new(
                    LineOrigin::Added {
                        new: NewLine::new(1).expect("one is inside the bound"),
                    },
                    DiffLineText::new(*b"new").expect("the fixture holds few bytes"),
                    LineEnding::Newline,
                ),
            ],
        )
        .expect("the fixture hunk realizes its ranges");
        FileDiff::new(
            DiffChange::Modified {
                old: side.clone(),
                new: side,
            },
            DiffContent::Text(
                TextDiff::new(vec![hunk], DiffTruncation::Complete)
                    .expect("the fixture text is usable"),
            ),
        )
        .expect("the fixture file is usable")
    }

    #[test]
    fn omitted_content_is_visible_and_cannot_be_selected() {
        let published = candidate(
            vec![added(
                "a.txt",
                &["one"],
                DiffTruncation::Truncated(DiffLimit::Lines),
            )],
            [5; 32],
        );
        let mut review = ReviewState::new(published);

        assert!(review.rows().any(|row| row
            == ReviewRow::Truncated {
                limit: DiffLimit::Lines
            }));
        assert_eq!(
            review
                .rows()
                .filter(|row| matches!(row, ReviewRow::Line { .. }))
                .count(),
            1
        );
        assert_eq!(
            review.select(DiffSide::New, 2, 1),
            Err(ReviewSelectError::Anchor(ReviewAnchorError::LinesMissing))
        );
    }

    #[test]
    fn a_reload_relocates_the_selection_into_the_later_candidate() {
        let mut review = ReviewState::new(one_file(&["one", "two"]));
        review
            .select(DiffSide::New, 2, 1)
            .expect("the hunk publishes the line");

        let later = one_file(&["added", "one", "two"]);
        let outcome = review.reload(later.clone());

        assert!(matches!(outcome, Some(Relocation::Relocated { .. })));
        let anchor = review
            .selection()
            .expect("the later candidate holds the lines");
        assert_eq!(anchor.location().first(), 3);
        assert_eq!(anchor.candidate(), later.revision());
        assert_eq!(review.candidate(), &later);
    }

    #[test]
    fn a_reload_drops_a_selection_that_the_later_candidate_lost() {
        let mut review = ReviewState::new(one_file(&["one", "two"]));
        review
            .select(DiffSide::New, 2, 1)
            .expect("the hunk publishes the line");

        let outcome = review.reload(one_file(&["one", "three"]));

        assert_eq!(outcome, Some(Relocation::Missing));
        assert!(review.selection().is_none());
    }

    #[test]
    fn one_submission_publishes_the_anchor_and_the_body() {
        let published = one_file(&["one", "two"]);
        let captured = TargetAuthority::of(&published);
        let mut review = ReviewState::new(published);
        review
            .select(DiffSide::New, 2, 1)
            .expect("the hunk publishes the line");

        review
            .submit_comment(body("the name hides the unit"), &captured)
            .expect("the fresh capture holds the location");

        let Some(ReviewEvent::CommentSubmitted { anchor, body }) = review.take_event() else {
            unreachable!("one accepted submission queues one event")
        };
        assert_eq!(anchor.location().first(), 2);
        assert_eq!(body.as_str(), "the name hides the unit");
        assert_eq!(review.queued_events(), 0);
    }

    #[test]
    fn a_submission_without_a_selection_publishes_nothing() {
        let published = one_file(&["one"]);
        let captured = TargetAuthority::of(&published);
        let mut review = ReviewState::new(published);

        assert_eq!(
            review.submit_comment(body("no place"), &captured),
            Err(SubmitCommentError::NoSelection)
        );
        assert_eq!(review.queued_events(), 0);
    }

    #[test]
    fn changed_content_publishes_no_comment() {
        let mut review = ReviewState::new(one_file(&["one", "two"]));
        review
            .select(DiffSide::New, 2, 1)
            .expect("the hunk publishes the line");
        let captured = TargetAuthority::of(&one_file(&["one", "TWO"]));

        assert_eq!(
            review.submit_comment(body("late"), &captured),
            Err(SubmitCommentError::Stale(StaleLocation::Content))
        );
        assert_eq!(review.queued_events(), 0);
    }

    #[test]
    fn a_changed_revision_publishes_no_comment() {
        let mut review = ReviewState::new(one_file(&["one", "two"]));
        review
            .select(DiffSide::New, 2, 1)
            .expect("the hunk publishes the line");

        // The staged authority changed while every published byte stayed, so
        // only the revision separates the two captures.
        let staged = candidate(
            vec![added("a.txt", &["one", "two"], DiffTruncation::Complete)],
            [9; 32],
        );
        let captured = TargetAuthority::of(&staged);

        assert_eq!(
            review.submit_comment(body("late"), &captured),
            Err(SubmitCommentError::Stale(StaleLocation::Revision))
        );
        assert_eq!(review.queued_events(), 0);
    }

    #[test]
    fn a_capture_of_another_target_publishes_no_comment() {
        let mut review = ReviewState::new(one_file(&["one", "two"]));
        review
            .select(DiffSide::New, 2, 1)
            .expect("the hunk publishes the line");

        let authority =
            CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest([7; 32]));
        let other = WorktreeDiff::new(
            BaseRevision::new(BASE_HEX).expect("the fixture names one full identifier"),
            DiffTarget::Path(path("a.txt")),
            &authority,
            vec![added("a.txt", &["one", "two"], DiffTruncation::Complete)],
            DiffTruncation::Complete,
        )
        .expect("the fixture candidate is usable");

        assert_eq!(
            review.submit_comment(body("late"), &TargetAuthority::of(&other)),
            Err(SubmitCommentError::Stale(StaleLocation::Target))
        );
        assert_eq!(review.queued_events(), 0);
    }

    #[test]
    fn a_full_queue_answers_before_the_submission_starts() {
        let published = one_file(&["one", "two"]);
        let captured = TargetAuthority::of(&published);
        let mut review = ReviewState::new(published);
        review
            .select(DiffSide::New, 2, 1)
            .expect("the hunk publishes the line");

        for _ in 0..REVIEW_EVENTS_MAX {
            review
                .submit_comment(body("queued"), &captured)
                .expect("the queue holds one free slot");
        }
        assert_eq!(review.queued_events(), REVIEW_EVENTS_MAX);

        // The stale authority would refuse the submission on its own. The full
        // queue answers first, so the host learns that it must drain before it
        // captures the target again.
        let stale = TargetAuthority::of(&one_file(&["one", "TWO"]));
        assert_eq!(
            review.submit_comment(body("dropped"), &stale),
            Err(SubmitCommentError::Saturated)
        );

        // The queue kept every earlier comment, so none disappeared.
        assert_eq!(review.queued_events(), REVIEW_EVENTS_MAX);
        assert!(review.take_event().is_some());
        review
            .submit_comment(body("accepted"), &captured)
            .expect("the drain freed one slot");
        assert_eq!(review.queued_events(), REVIEW_EVENTS_MAX);
    }
}
