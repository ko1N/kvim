//! The pure domain model of one worktree diff and its review locations.
//!
//! Every value here is deterministic. No function of this module reads a
//! repository, starts a process, reads a clock, or touches the filesystem. The
//! bounded Git capture supplies the parts, and this module validates them.
//!
//! [`BaseRevision`] names the commit that a review compares against.
//! [`DiffTarget`] selects the complete worktree or one contained path.
//! [`WorktreeDiff`] holds the published candidate and its [`DiffRevision`].
//! [`ReviewAnchor`] names one durable place inside that candidate, and
//! [`relocate`] compares an anchor with a later candidate without guessing.
//!
//! Every constructor validates its bounds. A value that this module returns is
//! therefore inside every published limit. See `docs/git.md`.
//!
//! # Examples
//!
//! ```
//! use kvim_path::WorktreeRelativePath;
//! use kvim_workspace::{
//!     BaseRevision, CandidateAuthority, DiffChange, DiffContent, DiffLine, DiffLineText,
//!     DiffOldSide, DiffSide, DiffTarget, DiffTruncation, FileDiff, FileMode, FileSide, HeadAuthority,
//!     Hunk, HunkId, IndexAuthority, LineEnding, LineOrigin, NewLine, NewLineRange, OldLine,
//!     OldLineRange, TextDiff, WorktreeDiff,
//! };
//!
//! let base = BaseRevision::new("0123456789abcdef0123456789abcdef01234567")?;
//! let path = WorktreeRelativePath::new("src/lib.rs")?;
//! let side = FileSide::new(path.clone(), FileMode::Regular);
//!
//! // One line moves from the old side to the new side.
//! let hunk = Hunk::new(
//!     HunkId::new(0),
//!     OldLineRange::new(OldLine::new(1)?, 1)?,
//!     NewLineRange::new(NewLine::new(1)?, 1)?,
//!     vec![
//!         DiffLine::new(
//!             LineOrigin::Removed { old: OldLine::new(1)? },
//!             DiffLineText::new(*b"old")?,
//!             LineEnding::Newline,
//!         ),
//!         DiffLine::new(
//!             LineOrigin::Added { new: NewLine::new(1)? },
//!             DiffLineText::new(*b"new")?,
//!             LineEnding::EndOfFile,
//!         ),
//!     ],
//! )?;
//!
//! let text = TextDiff::new(vec![hunk], DiffTruncation::Complete)?;
//! let file = FileDiff::new(
//!     DiffChange::Modified { old: side.clone(), new: side },
//!     DiffContent::Text(text),
//! )?;
//!
//! let authority = CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest([0; 32]));
//! let diff = WorktreeDiff::new(
//!     DiffOldSide::Commit(base),
//!     DiffTarget::Worktree,
//!     &authority,
//!     vec![file],
//!     DiffTruncation::Complete,
//! )?;
//!
//! // The new side keeps its missing final line feed.
//! let published = diff.file(&path).expect("the candidate holds the path");
//! let DiffContent::Text(text) = published.content() else { unreachable!() };
//! assert_eq!(text.side_bytes(DiffSide::New), b"new");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::fmt;
use std::num::NonZeroU32;
use std::str;

use blake3::Hasher;
use kvim_path::WorktreeRelativePath;
use thiserror::Error;

/// The domain separator of every authority projection.
const PROJECTION_DOMAIN: &[u8] = b"kvim.diff.projection.v1";

/// The number of bytes in every digest of this module.
pub const DIGEST_BYTES: usize = 32;

/// The number of hexadecimal characters in a full SHA-1 object identifier.
pub const SHA1_HEX_CHARS: usize = 40;

/// The number of hexadecimal characters in a full SHA-256 object identifier.
pub const SHA256_HEX_CHARS: usize = 64;

/// The number of octal digits in one published Git file mode.
pub const FILE_MODE_DIGITS: usize = 6;

/// The largest line number that one diff addresses.
///
/// A file above this bound reports truncation rather than an unreachable line.
pub const DIFF_LINE_NUMBER_MAX: u32 = 1_000_000;

/// The largest number of bytes in one published diff line.
pub const DIFF_LINE_BYTES_MAX: usize = 4096;

/// The largest number of lines in one hunk.
pub const DIFF_HUNK_LINES_MAX: usize = 4096;

/// The largest number of hunks in one file diff.
pub const DIFF_FILE_HUNKS_MAX: usize = 512;

/// The largest number of changed files in one candidate.
pub const DIFF_FILES_MAX: usize = 2048;

/// The largest number of context lines that one anchor keeps on each side.
///
/// The context proves the identity of a selection after an edit. A larger
/// window costs more bytes for every stored anchor and relocates no better.
pub const REVIEW_CONTEXT_LINES_MAX: usize = 8;

/// The largest number of bytes in one review comment.
pub const REVIEW_COMMENT_BYTES_MAX: usize = 8 * 1024;

/// The largest number of candidate windows that one relocation compares.
///
/// The search stops here and reports an ambiguity, because the part that it did
/// not compare can still hold another match.
pub const RELOCATION_WINDOWS_MAX: usize = 100_000;

/// The domain separator of every revision digest of this module.
const REVISION_DOMAIN: &[u8] = b"kvim.diff.revision.v1";

/// The domain separator of every selection digest of this module.
const SELECTION_DOMAIN: &[u8] = b"kvim.diff.selection.v1";

// ---------------------------------------------------------------------------
// Revisions
// ---------------------------------------------------------------------------

/// The decoded bytes of one full Git commit object identifier.
///
/// The variant carries the whole digest, so no length outside the two published
/// object formats can exist.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CommitDigest {
    Sha1([u8; SHA1_HEX_CHARS / 2]),
    Sha256([u8; SHA256_HEX_CHARS / 2]),
}

/// One full Git commit object identifier that a review compares against.
///
/// The constructor accepts the two published object formats: 40 hexadecimal
/// characters for SHA-1 and 64 for SHA-256. It accepts no abbreviation, because
/// an abbreviated identifier can name more than one object, and a review base
/// must stay one object for the life of the review. It accepts either letter
/// case and keeps the lowercase form, which is the form that Git writes.
///
/// The same value type names `HEAD` inside [`HeadAuthority`], because Git
/// stores both under one object format.
///
/// # Examples
///
/// ```
/// use kvim_workspace::BaseRevision;
///
/// let base = BaseRevision::new("0123456789ABCDEF0123456789abcdef01234567")?;
/// assert_eq!(base.to_hex(), "0123456789abcdef0123456789abcdef01234567");
///
/// // An abbreviated identifier names no single object.
/// assert!(BaseRevision::new("0123456").is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BaseRevision(CommitDigest);

impl BaseRevision {
    /// Validates one full commit object identifier.
    pub fn new(hex: &str) -> Result<Self, BaseRevisionError> {
        let bytes = hex.as_bytes();
        match bytes.len() {
            SHA1_HEX_CHARS => {
                let mut digest = [0_u8; SHA1_HEX_CHARS / 2];
                decode_hex(bytes, &mut digest)?;
                Ok(Self(CommitDigest::Sha1(digest)))
            }
            SHA256_HEX_CHARS => {
                let mut digest = [0_u8; SHA256_HEX_CHARS / 2];
                decode_hex(bytes, &mut digest)?;
                Ok(Self(CommitDigest::Sha256(digest)))
            }
            actual => Err(BaseRevisionError::Length { actual }),
        }
    }

    /// Returns the decoded object identifier.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match &self.0 {
            CommitDigest::Sha1(digest) => digest,
            CommitDigest::Sha256(digest) => digest,
        }
    }

    /// Returns the lowercase hexadecimal form that Git commands accept.
    #[must_use]
    pub fn to_hex(&self) -> String {
        encode_hex(self.as_bytes())
    }
}

impl fmt::Display for BaseRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// The reasons that one commit object identifier is not usable.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BaseRevisionError {
    /// The value holds neither 40 nor 64 characters.
    #[error(
        "a commit object identifier holds {SHA1_HEX_CHARS} or {SHA256_HEX_CHARS} characters, not {actual}"
    )]
    Length {
        /// The number of characters that the value holds.
        actual: usize,
    },
    /// The value holds a character that is no hexadecimal digit.
    #[error("the character at position {position} is no hexadecimal digit")]
    NotHexadecimal {
        /// The zero-based position of the first character that is no digit.
        position: usize,
    },
}

/// A digest of the index state that one capture read.
///
/// Capture owns the exact input of this digest. The domain model treats it as
/// opaque bytes, so no pure function of this module reads an index.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IndexAuthority([u8; DIGEST_BYTES]);

impl IndexAuthority {
    /// Owns one computed index digest.
    #[must_use]
    pub const fn from_digest(digest: [u8; DIGEST_BYTES]) -> Self {
        Self(digest)
    }

    /// Returns the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }
}

/// The commit that `HEAD` named while capture ran.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HeadAuthority {
    /// The repository holds no commit on the checked-out branch.
    Unborn,
    /// `HEAD` names one commit.
    Commit(BaseRevision),
}

/// The repository authority that one candidate was captured against.
///
/// The revision of a candidate covers this authority, so two candidates with
/// equal file content but a different `HEAD` or index never share one revision.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CandidateAuthority {
    head: HeadAuthority,
    index: IndexAuthority,
}

impl CandidateAuthority {
    /// Owns one captured repository authority.
    #[must_use]
    pub const fn new(head: HeadAuthority, index: IndexAuthority) -> Self {
        Self { head, index }
    }

    /// Returns the commit that `HEAD` named.
    #[must_use]
    pub const fn head(&self) -> HeadAuthority {
        self.head
    }

    /// Returns the index digest.
    #[must_use]
    pub const fn index(&self) -> IndexAuthority {
        self.index
    }
}

/// The immutable identity of one published candidate.
///
/// The value is a BLAKE3 digest of the base commit, the current `HEAD`, the
/// index authority, the sorted paths, the file kinds and modes, and the exact
/// published side bytes with their line mapping. Two candidates therefore share
/// one revision only when a reader would see exactly the same review.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DiffRevision([u8; DIGEST_BYTES]);

impl DiffRevision {
    /// Derives the revision of one candidate from its complete authority.
    ///
    /// The caller passes the files in the published order. The digest covers
    /// that order, so a reordered candidate is a different revision.
    #[must_use]
    pub fn derive(
        old: DiffOldSide,
        authority: &CandidateAuthority,
        files: &[FileDiff],
        truncation: DiffTruncation,
    ) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(REVISION_DOMAIN);
        old.absorb_into(&mut hasher);
        match authority.head() {
            HeadAuthority::Unborn => {
                hasher.update(&[0]);
            }
            HeadAuthority::Commit(head) => {
                hasher.update(&[1]);
                absorb(&mut hasher, head.as_bytes());
            }
        }
        hasher.update(authority.index().as_bytes());
        hasher.update(&truncation.tag());
        absorb_count(&mut hasher, files.len());
        for file in files {
            file.absorb(&mut hasher);
        }
        Self(*hasher.finalize().as_bytes())
    }

    /// Returns the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }

    /// Returns the lowercase hexadecimal form of the digest.
    #[must_use]
    pub fn to_hex(&self) -> String {
        encode_hex(&self.0)
    }
}

impl fmt::Display for DiffRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

// ---------------------------------------------------------------------------
// File sides
// ---------------------------------------------------------------------------

/// The published Git mode of one file side.
///
/// The mode carries both the permission and the kind, because Git stores one
/// value for both. A symbolic link, a submodule, and a mode that this release
/// does not read therefore stay distinct from an ordinary file.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FileMode {
    /// `100644`, an ordinary file.
    Regular,
    /// `100755`, an executable file.
    Executable,
    /// `120000`, a symbolic link that holds its target as content.
    SymbolicLink,
    /// `160000`, a submodule that holds one commit identifier as content.
    Submodule,
    /// A published mode that this release does not read.
    Unsupported(UnsupportedMode),
}

impl FileMode {
    /// Validates one published mode of six octal digits.
    pub fn from_octal(octal: &str) -> Result<Self, FileModeError> {
        let bytes = octal.as_bytes();
        if bytes.len() != FILE_MODE_DIGITS {
            return Err(FileModeError::Digits {
                actual: bytes.len(),
            });
        }
        if let Some(position) = bytes.iter().position(|byte| !(b'0'..=b'7').contains(byte)) {
            return Err(FileModeError::NotOctal { position });
        }
        Ok(match octal {
            "100644" => Self::Regular,
            "100755" => Self::Executable,
            "120000" => Self::SymbolicLink,
            "160000" => Self::Submodule,
            _ => {
                let mut digits = [0_u8; FILE_MODE_DIGITS];
                digits.copy_from_slice(bytes);
                Self::Unsupported(UnsupportedMode(digits))
            }
        })
    }

    /// Returns the published octal digits of the mode.
    #[must_use]
    pub fn as_octal(&self) -> &str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
            Self::SymbolicLink => "120000",
            Self::Submodule => "160000",
            Self::Unsupported(mode) => mode.as_str(),
        }
    }

    /// Reports whether Git stores reviewable file content under this mode.
    #[must_use]
    pub const fn stores_text(self) -> bool {
        matches!(self, Self::Regular | Self::Executable)
    }
}

/// A published mode that this release does not read.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct UnsupportedMode([u8; FILE_MODE_DIGITS]);

impl UnsupportedMode {
    /// Returns the published octal digits of the mode.
    #[must_use]
    pub fn as_str(&self) -> &str {
        str::from_utf8(&self.0).expect("the constructor accepted ASCII octal digits only")
    }
}

/// The reasons that one published file mode is not usable.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FileModeError {
    /// The value holds a different number of digits.
    #[error("a file mode holds {FILE_MODE_DIGITS} digits, not {actual}")]
    Digits {
        /// The number of characters that the value holds.
        actual: usize,
    },
    /// The value holds a character that is no octal digit.
    #[error("the character at position {position} is no octal digit")]
    NotOctal {
        /// The zero-based position of the first character that is no digit.
        position: usize,
    },
}

/// One published side of one changed file.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FileSide {
    path: WorktreeRelativePath,
    mode: FileMode,
}

impl FileSide {
    /// Owns one validated path and its published mode.
    #[must_use]
    pub const fn new(path: WorktreeRelativePath, mode: FileMode) -> Self {
        Self { path, mode }
    }

    /// Returns the worktree-relative path of the side.
    #[must_use]
    pub const fn path(&self) -> &WorktreeRelativePath {
        &self.path
    }

    /// Returns the published mode of the side.
    #[must_use]
    pub const fn mode(&self) -> FileMode {
        self.mode
    }

    fn absorb(&self, hasher: &mut Hasher) {
        absorb(hasher, self.path.as_path().as_os_str().as_encoded_bytes());
        hasher.update(self.mode.as_octal().as_bytes());
    }
}

/// The two sides that one changed file publishes.
///
/// [`FileDiff::new`] validates the paths: a modification names one path on both
/// sides, and a rename names two different paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffChange {
    /// The candidate holds a file that the base does not hold.
    Added {
        /// The side that the candidate holds.
        new: FileSide,
    },
    /// The base holds a file that the candidate does not hold.
    Deleted {
        /// The side that the base holds.
        old: FileSide,
    },
    /// Both sides name one path, and the content or the mode changed.
    Modified {
        /// The side that the base holds.
        old: FileSide,
        /// The side that the candidate holds.
        new: FileSide,
    },
    /// The candidate holds the file under another path.
    Renamed {
        /// The side that the base holds.
        old: FileSide,
        /// The side that the candidate holds.
        new: FileSide,
    },
}

impl DiffChange {
    /// Returns the side that the base holds.
    #[must_use]
    pub const fn old_side(&self) -> Option<&FileSide> {
        match self {
            Self::Added { .. } => None,
            Self::Deleted { old } | Self::Modified { old, .. } | Self::Renamed { old, .. } => {
                Some(old)
            }
        }
    }

    /// Returns the side that the candidate holds.
    #[must_use]
    pub const fn new_side(&self) -> Option<&FileSide> {
        match self {
            Self::Deleted { .. } => None,
            Self::Added { new } | Self::Modified { new, .. } | Self::Renamed { new, .. } => {
                Some(new)
            }
        }
    }

    /// Returns the side of one direction.
    #[must_use]
    pub const fn side(&self, side: DiffSide) -> Option<&FileSide> {
        match side {
            DiffSide::Old => self.old_side(),
            DiffSide::New => self.new_side(),
        }
    }

    /// Returns the path that a reader names for the change.
    ///
    /// A deletion names its old path. Every other change names its new path.
    #[must_use]
    pub const fn path(&self) -> &WorktreeRelativePath {
        match self {
            Self::Deleted { old } => old.path(),
            Self::Added { new } | Self::Modified { new, .. } | Self::Renamed { new, .. } => {
                new.path()
            }
        }
    }

    /// Reports whether one path names either side of the change.
    #[must_use]
    pub fn names(&self, path: &WorktreeRelativePath) -> bool {
        self.old_side().is_some_and(|side| side.path() == path)
            || self.new_side().is_some_and(|side| side.path() == path)
    }

    const fn tag(&self) -> u8 {
        match self {
            Self::Added { .. } => 0,
            Self::Deleted { .. } => 1,
            Self::Modified { .. } => 2,
            Self::Renamed { .. } => 3,
        }
    }
}

/// The two states that one capture compares.
///
/// Git compares a pair of states, and the pair decides which section of a
/// review the capture publishes. The value names both states together, so a
/// pair that Git cannot compare cannot be requested. See `docs/git.md`.
///
/// # Examples
///
/// ```
/// use kvim_workspace::{BaseRevision, DiffComparison};
///
/// let base = BaseRevision::new("0123456789abcdef0123456789abcdef01234567")
///     .expect("the identifier is one full object name");
/// // The staged half of one review reads no worktree file.
/// assert!(!DiffComparison::CommitToIndex(base).reads_worktree());
/// // The unstaged half needs no revision at all.
/// assert!(DiffComparison::IndexToWorktree.reads_worktree());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffComparison {
    /// One commit against the worktree. Every change, staged or not.
    CommitToWorktree(BaseRevision),
    /// One commit against the index. The staged half against a chosen base.
    CommitToIndex(BaseRevision),
    /// The current `HEAD` against the index. The staged half of one review.
    ///
    /// Git resolves `HEAD` itself, so the caller names no revision. A
    /// repository without a commit has no `HEAD` to compare against, and the
    /// capture answers `BaseUnavailable`.
    HeadToIndex,
    /// The index against the worktree. The unstaged half of one review.
    IndexToWorktree,
    /// One commit against another commit.
    ///
    /// The range of one finished piece of work, reproducible from the
    /// repository alone, because neither side can change.
    CommitToCommit {
        /// The commit that the comparison starts from.
        old: BaseRevision,
        /// The commit that the comparison ends at.
        new: BaseRevision,
    },
}

impl DiffComparison {
    /// Returns the commit that the capture must prove names one commit object.
    ///
    /// [`DiffComparison::IndexToWorktree`] names no commit, so it returns
    /// `None` and the capture proves nothing.
    #[must_use]
    pub const fn old_commit(self) -> Option<BaseRevision> {
        match self {
            Self::CommitToWorktree(base)
            | Self::CommitToIndex(base)
            | Self::CommitToCommit { old: base, .. } => Some(base),
            Self::HeadToIndex | Self::IndexToWorktree => None,
        }
    }

    /// Returns the commit that the comparison ends at, when it ends at one.
    #[must_use]
    pub const fn new_commit(self) -> Option<BaseRevision> {
        match self {
            Self::CommitToCommit { new, .. } => Some(new),
            Self::CommitToWorktree(_)
            | Self::CommitToIndex(_)
            | Self::HeadToIndex
            | Self::IndexToWorktree => None,
        }
    }

    /// Reports whether the comparison ends at the worktree.
    ///
    /// An untracked file exists in the worktree alone, so only a comparison
    /// that ends there lists one.
    #[must_use]
    pub const fn reads_worktree(self) -> bool {
        matches!(self, Self::CommitToWorktree(_) | Self::IndexToWorktree)
    }
}

/// The state that one candidate compares against.
///
/// A commit names itself. The index names no commit, so the unstaged half of a
/// review records the index digest that the capture read. A durable anchor
/// therefore states which state its old lines came from, instead of naming a
/// commit that never held them. See `docs/git.md`.
///
/// # Examples
///
/// ```
/// use kvim_workspace::{BaseRevision, DiffOldSide};
///
/// let base = BaseRevision::new("0123456789abcdef0123456789abcdef01234567")
///     .expect("the identifier is one full object name");
/// assert_eq!(DiffOldSide::Commit(base).commit(), Some(base));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffOldSide {
    /// The old lines came from one commit.
    Commit(BaseRevision),
    /// The old lines came from the index that this digest names.
    Index(IndexAuthority),
}

impl DiffOldSide {
    /// Returns the commit that holds the old lines, when a commit holds them.
    #[must_use]
    pub const fn commit(self) -> Option<BaseRevision> {
        match self {
            Self::Commit(base) => Some(base),
            Self::Index(_) => None,
        }
    }

    /// Absorbs the value into one digest, with its own tag for each case.
    pub(crate) fn absorb_into(self, hasher: &mut Hasher) {
        match self {
            Self::Commit(base) => {
                hasher.update(&[0]);
                absorb(hasher, base.as_bytes());
            }
            Self::Index(index) => {
                hasher.update(&[1]);
                hasher.update(index.as_bytes());
            }
        }
    }
}

/// One direction of one changed file.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DiffSide {
    /// The side that the base commit holds.
    Old,
    /// The side that the candidate worktree holds.
    New,
}

/// The selection of one worktree diff.
///
/// A one-path target matches either side of a rename, so a renamed file returns
/// its complete pair of sides under either name.
///
/// # Examples
///
/// ```
/// use kvim_path::WorktreeRelativePath;
/// use kvim_workspace::{DiffChange, DiffTarget, FileMode, FileSide};
///
/// let old = FileSide::new(WorktreeRelativePath::new("src/old.rs")?, FileMode::Regular);
/// let new = FileSide::new(WorktreeRelativePath::new("src/new.rs")?, FileMode::Regular);
/// let rename = DiffChange::Renamed { old, new };
///
/// let by_old = DiffTarget::Path(WorktreeRelativePath::new("src/old.rs")?);
/// let by_new = DiffTarget::Path(WorktreeRelativePath::new("src/new.rs")?);
/// assert!(by_old.selects(&rename));
/// assert!(by_new.selects(&rename));
/// assert!(DiffTarget::Worktree.selects(&rename));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffTarget {
    /// Every changed file below the worktree root.
    Worktree,
    /// One contained path, under either side of a rename.
    Path(WorktreeRelativePath),
}

impl DiffTarget {
    /// Reports whether the target selects one changed file.
    #[must_use]
    pub fn selects(&self, change: &DiffChange) -> bool {
        match self {
            Self::Worktree => true,
            Self::Path(path) => change.names(path),
        }
    }
}

// ---------------------------------------------------------------------------
// Lines
// ---------------------------------------------------------------------------

/// A validated one-based line number inside one published bound.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct LineNumber(NonZeroU32);

impl LineNumber {
    fn new(number: u32) -> Result<Self, LineNumberError> {
        let number = NonZeroU32::new(number).ok_or(LineNumberError::Zero)?;
        if number.get() > DIFF_LINE_NUMBER_MAX {
            return Err(LineNumberError::Limit {
                actual: number.get(),
                max: DIFF_LINE_NUMBER_MAX,
            });
        }
        Ok(Self(number))
    }

    const fn get(self) -> u32 {
        self.0.get()
    }
}

/// One line number on the side that the base commit holds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OldLine(LineNumber);

impl OldLine {
    /// Validates one one-based line number of the old side.
    pub fn new(number: u32) -> Result<Self, LineNumberError> {
        LineNumber::new(number).map(Self)
    }

    /// Returns the one-based line number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// One line number on the side that the candidate worktree holds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NewLine(LineNumber);

impl NewLine {
    /// Validates one one-based line number of the new side.
    pub fn new(number: u32) -> Result<Self, LineNumberError> {
        LineNumber::new(number).map(Self)
    }

    /// Returns the one-based line number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// The reasons that one line number is not usable.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LineNumberError {
    /// A line number counts from one.
    #[error("a line number counts from one")]
    Zero,
    /// The number passed the addressable bound.
    #[error("line {actual} passes the bound of {max} lines")]
    Limit {
        /// The number that the caller supplied.
        actual: u32,
        /// The published bound.
        max: u32,
    },
}

/// The exact published bytes of one diff line, without its line terminator.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DiffLineText(Box<[u8]>);

impl DiffLineText {
    /// Validates the bytes of one published line.
    ///
    /// The value holds no line feed, because [`LineEnding`] owns the terminator
    /// of every line.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, DiffLineTextError> {
        let bytes = bytes.into();
        if bytes.len() > DIFF_LINE_BYTES_MAX {
            return Err(DiffLineTextError::Limit {
                actual: bytes.len(),
                max: DIFF_LINE_BYTES_MAX,
            });
        }
        if let Some(position) = bytes.iter().position(|byte| *byte == b'\n') {
            return Err(DiffLineTextError::LineFeed { position });
        }
        Ok(Self(bytes.into_boxed_slice()))
    }

    /// Returns the exact published bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the line as text when its bytes are valid UTF-8.
    ///
    /// Git publishes exact source bytes. A file in another encoding therefore
    /// keeps its bytes and returns [`None`] here.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        str::from_utf8(&self.0).ok()
    }
}

/// The reasons that one published line is not usable.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DiffLineTextError {
    /// The line passed the published byte bound.
    #[error("the line holds {actual} bytes; the limit is {max} bytes")]
    Limit {
        /// The number of bytes that the caller supplied.
        actual: usize,
        /// The published bound.
        max: usize,
    },
    /// The line holds a line feed.
    #[error("the line holds a line feed at position {position}")]
    LineFeed {
        /// The zero-based position of the first line feed.
        position: usize,
    },
}

/// The terminator of one published line.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LineEnding {
    /// The line ends with a line feed.
    Newline,
    /// The line is the last line of its side and holds no final line feed.
    ///
    /// Git writes `\ No newline at end of file` after such a line.
    EndOfFile,
}

impl LineEnding {
    const fn tag(self) -> u8 {
        match self {
            Self::Newline => 0,
            Self::EndOfFile => 1,
        }
    }
}

/// The sides that one published line belongs to.
///
/// The variant carries the line number of every side that holds the line, so no
/// value can name a side that does not hold it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LineOrigin {
    /// Both sides hold the line.
    Context {
        /// The line number on the old side.
        old: OldLine,
        /// The line number on the new side.
        new: NewLine,
    },
    /// Only the old side holds the line.
    Removed {
        /// The line number on the old side.
        old: OldLine,
    },
    /// Only the new side holds the line.
    Added {
        /// The line number on the new side.
        new: NewLine,
    },
}

impl LineOrigin {
    /// Returns the line number on the old side.
    ///
    /// An added line exists on the new side alone, so it returns `None`.
    #[must_use]
    pub const fn old_line(self) -> Option<OldLine> {
        match self {
            Self::Context { old, .. } | Self::Removed { old } => Some(old),
            Self::Added { .. } => None,
        }
    }

    /// Returns the line number on the new side.
    ///
    /// A removed line exists on the old side alone, so it returns `None`.
    #[must_use]
    pub const fn new_line(self) -> Option<NewLine> {
        match self {
            Self::Context { new, .. } | Self::Added { new } => Some(new),
            Self::Removed { .. } => None,
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::Context { .. } => 0,
            Self::Removed { .. } => 1,
            Self::Added { .. } => 2,
        }
    }
}

/// One published line of one hunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLine {
    origin: LineOrigin,
    text: DiffLineText,
    ending: LineEnding,
}

impl DiffLine {
    /// Owns one validated line and its exact mapping onto both sides.
    #[must_use]
    pub const fn new(origin: LineOrigin, text: DiffLineText, ending: LineEnding) -> Self {
        Self {
            origin,
            text,
            ending,
        }
    }

    /// Returns the sides that hold the line.
    #[must_use]
    pub const fn origin(&self) -> LineOrigin {
        self.origin
    }

    /// Returns the exact published bytes of the line.
    #[must_use]
    pub const fn text(&self) -> &DiffLineText {
        &self.text
    }

    /// Returns the terminator of the line.
    #[must_use]
    pub const fn ending(&self) -> LineEnding {
        self.ending
    }

    /// Returns the line number on one side.
    #[must_use]
    pub const fn number(&self, side: DiffSide) -> Option<u32> {
        match side {
            DiffSide::Old => match self.origin.old_line() {
                Some(old) => Some(old.get()),
                None => None,
            },
            DiffSide::New => match self.origin.new_line() {
                Some(new) => Some(new.get()),
                None => None,
            },
        }
    }

    /// Reports whether one side publishes the line.
    #[must_use]
    pub const fn appears_on(&self, side: DiffSide) -> bool {
        self.number(side).is_some()
    }

    fn absorb(&self, hasher: &mut Hasher) {
        hasher.update(&[self.origin.tag(), self.ending.tag()]);
        absorb(hasher, self.text.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// Ranges
// ---------------------------------------------------------------------------

/// A contiguous run of lines that starts at one validated line number.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct LineRange {
    first: LineNumber,
    count: u32,
}

impl LineRange {
    fn new(first: LineNumber, count: u32) -> Result<Self, LineRangeError> {
        let max = u32::try_from(DIFF_HUNK_LINES_MAX).expect("the hunk bound fits one u32");
        if count > max {
            return Err(LineRangeError::Limit { actual: count, max });
        }
        let last = first.get().saturating_add(count.saturating_sub(1));
        if last > DIFF_LINE_NUMBER_MAX {
            return Err(LineRangeError::Limit {
                actual: last,
                max: DIFF_LINE_NUMBER_MAX,
            });
        }
        Ok(Self { first, count })
    }

    /// The first line number after the range.
    const fn end(self) -> u32 {
        self.first.get() + self.count
    }
}

/// A run of lines on the side that the base commit holds.
///
/// An empty range names the place where the candidate inserts lines: `first` is
/// the old line that follows the insertion.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OldLineRange(LineRange);

impl OldLineRange {
    /// Validates one run of old-side lines.
    pub fn new(first: OldLine, count: u32) -> Result<Self, LineRangeError> {
        LineRange::new(first.0, count).map(Self)
    }

    /// Returns the first line number of the range.
    #[must_use]
    pub const fn first(self) -> OldLine {
        OldLine(self.0.first)
    }

    /// Returns the number of lines in the range.
    #[must_use]
    pub const fn count(self) -> u32 {
        self.0.count
    }

    /// Reports whether the range covers no line.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0.count == 0
    }
}

/// A run of lines on the side that the candidate worktree holds.
///
/// An empty range names the place where the candidate removed lines: `first` is
/// the new line that follows the removal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NewLineRange(LineRange);

impl NewLineRange {
    /// Validates one run of new-side lines.
    pub fn new(first: NewLine, count: u32) -> Result<Self, LineRangeError> {
        LineRange::new(first.0, count).map(Self)
    }

    /// Returns the first line number of the range.
    #[must_use]
    pub const fn first(self) -> NewLine {
        NewLine(self.0.first)
    }

    /// Returns the number of lines in the range.
    #[must_use]
    pub const fn count(self) -> u32 {
        self.0.count
    }

    /// Reports whether the range covers no line.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0.count == 0
    }
}

/// The reasons that one line range is not usable.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LineRangeError {
    /// The range passed a published bound.
    #[error("the range reaches {actual}; the limit is {max}")]
    Limit {
        /// The value that the range reaches.
        actual: u32,
        /// The published bound.
        max: u32,
    },
}

// ---------------------------------------------------------------------------
// Hunks
// ---------------------------------------------------------------------------

/// The identity of one hunk inside one file of one candidate.
///
/// The value is the zero-based position of the hunk. A later candidate can hold
/// another hunk under the same identity, so [`ReviewAnchor`] also keeps the
/// revision that published it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HunkId(u32);

impl HunkId {
    /// Owns one zero-based hunk position.
    #[must_use]
    pub const fn new(position: u32) -> Self {
        Self(position)
    }

    /// Returns the zero-based hunk position.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One contiguous run of changed and surrounding lines.
///
/// The constructor proves that the lines realize both ranges exactly: every old
/// line number rises by one from the start of the old range, every new line
/// number rises by one from the start of the new range, and both counts match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hunk {
    id: HunkId,
    old_range: OldLineRange,
    new_range: NewLineRange,
    lines: Vec<DiffLine>,
}

impl Hunk {
    /// Validates one hunk against its two ranges.
    pub fn new(
        id: HunkId,
        old_range: OldLineRange,
        new_range: NewLineRange,
        lines: Vec<DiffLine>,
    ) -> Result<Self, HunkError> {
        if lines.is_empty() {
            return Err(HunkError::Empty);
        }
        if lines.len() > DIFF_HUNK_LINES_MAX {
            return Err(HunkError::LinesLimit {
                actual: lines.len(),
                max: DIFF_HUNK_LINES_MAX,
            });
        }

        let mut expected = [old_range.first().get(), new_range.first().get()];
        let mut seen = [0_u32, 0_u32];
        let mut ended = [false, false];
        for line in &lines {
            for (index, side) in [DiffSide::Old, DiffSide::New].into_iter().enumerate() {
                let Some(number) = line.number(side) else {
                    continue;
                };
                if ended[index] {
                    return Err(HunkError::LineAfterFinalLine { side });
                }
                if number != expected[index] {
                    return Err(HunkError::LineMismatch {
                        side,
                        expected: expected[index],
                        actual: number,
                    });
                }
                expected[index] = expected[index].saturating_add(1);
                seen[index] += 1;
                ended[index] = line.ending() == LineEnding::EndOfFile;
            }
        }

        for (index, (side, count)) in [
            (DiffSide::Old, old_range.count()),
            (DiffSide::New, new_range.count()),
        ]
        .into_iter()
        .enumerate()
        {
            if seen[index] != count {
                return Err(HunkError::CountMismatch {
                    side,
                    expected: count,
                    actual: seen[index],
                });
            }
        }

        Ok(Self {
            id,
            old_range,
            new_range,
            lines,
        })
    }

    /// Returns the identity of the hunk inside its file.
    #[must_use]
    pub const fn id(&self) -> HunkId {
        self.id
    }

    /// Returns the run of old-side lines that the hunk covers.
    #[must_use]
    pub const fn old_range(&self) -> OldLineRange {
        self.old_range
    }

    /// Returns the run of new-side lines that the hunk covers.
    #[must_use]
    pub const fn new_range(&self) -> NewLineRange {
        self.new_range
    }

    /// Returns every published line of the hunk, in order.
    #[must_use]
    pub fn lines(&self) -> &[DiffLine] {
        &self.lines
    }

    /// Returns the lines that one side publishes, in order.
    pub fn side_lines(&self, side: DiffSide) -> impl Iterator<Item = &DiffLine> {
        self.lines.iter().filter(move |line| line.appears_on(side))
    }

    fn first_line_of(&self, side: DiffSide) -> u32 {
        match side {
            DiffSide::Old => self.old_range.first().get(),
            DiffSide::New => self.new_range.first().get(),
        }
    }
}

/// The reasons that one hunk does not realize its ranges.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HunkError {
    /// The hunk holds no line.
    #[error("a hunk holds at least one line")]
    Empty,
    /// The hunk passed the published line bound.
    #[error("the hunk holds {actual} lines; the limit is {max} lines")]
    LinesLimit {
        /// The number of lines that the caller supplied.
        actual: usize,
        /// The published bound.
        max: usize,
    },
    /// One line number does not follow the previous one.
    #[error("the {side:?} side expected line {expected} and holds line {actual}")]
    LineMismatch {
        /// The side that holds the mismatch.
        side: DiffSide,
        /// The line number that the range demands.
        expected: u32,
        /// The line number that the caller supplied.
        actual: u32,
    },
    /// One line follows the last line of its side.
    #[error("the {side:?} side holds a line after its last line")]
    LineAfterFinalLine {
        /// The side that holds the extra line.
        side: DiffSide,
    },
    /// The lines of one side do not fill its range.
    #[error("the {side:?} range covers {expected} lines and the hunk holds {actual}")]
    CountMismatch {
        /// The side that holds the mismatch.
        side: DiffSide,
        /// The number of lines that the range covers.
        expected: u32,
        /// The number of lines that the hunk holds.
        actual: u32,
    },
}

// ---------------------------------------------------------------------------
// File content
// ---------------------------------------------------------------------------

/// The bound that stopped one collection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DiffLimit {
    /// The candidate holds more changed files than the bound allows.
    Files,
    /// The file holds more hunks than the bound allows.
    Hunks,
    /// The hunk holds more lines than the bound allows.
    Lines,
    /// One line holds more bytes than the bound allows.
    LineBytes,
    /// The captured source passed its byte bound.
    SourceBytes,
}

impl DiffLimit {
    const fn tag(self) -> u8 {
        match self {
            Self::Files => 0,
            Self::Hunks => 1,
            Self::Lines => 2,
            Self::LineBytes => 3,
            Self::SourceBytes => 4,
        }
    }
}

/// Whether one collection published every part that it found.
///
/// Truncated data stays visibly truncated, and the published part stays exact.
/// Omitted content carries no line, so it can receive no review comment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DiffTruncation {
    /// Every part of the collection is present.
    Complete,
    /// One bound stopped the collection.
    Truncated(DiffLimit),
}

impl DiffTruncation {
    /// Reports whether a bound stopped the collection.
    #[must_use]
    pub const fn is_truncated(self) -> bool {
        matches!(self, Self::Truncated(_))
    }

    const fn tag(self) -> [u8; 2] {
        match self {
            Self::Complete => [0, 0],
            Self::Truncated(limit) => [1, limit.tag()],
        }
    }
}

/// The published hunks of one text file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextDiff {
    hunks: Vec<Hunk>,
    truncation: DiffTruncation,
}

impl TextDiff {
    /// Validates the hunks of one text file.
    ///
    /// The hunk identities count from zero, and the ranges of both sides rise
    /// without an overlap.
    pub fn new(hunks: Vec<Hunk>, truncation: DiffTruncation) -> Result<Self, TextDiffError> {
        if hunks.len() > DIFF_FILE_HUNKS_MAX {
            return Err(TextDiffError::HunksLimit {
                actual: hunks.len(),
                max: DIFF_FILE_HUNKS_MAX,
            });
        }

        let mut ends = [1_u32, 1_u32];
        for (position, hunk) in hunks.iter().enumerate() {
            let expected = u32::try_from(position).expect("the hunk bound fits one u32");
            if hunk.id().get() != expected {
                return Err(TextDiffError::HunkIdMismatch {
                    expected,
                    actual: hunk.id().get(),
                });
            }
            for (index, (side, range)) in [
                (DiffSide::Old, hunk.old_range().0),
                (DiffSide::New, hunk.new_range().0),
            ]
            .into_iter()
            .enumerate()
            {
                if range.first.get() < ends[index] {
                    return Err(TextDiffError::RangeOverlap {
                        side,
                        first: range.first.get(),
                        previous_end: ends[index],
                    });
                }
                ends[index] = range.end();
            }
        }

        Ok(Self { hunks, truncation })
    }

    /// Returns every published hunk, in order.
    #[must_use]
    pub fn hunks(&self) -> &[Hunk] {
        &self.hunks
    }

    /// Returns one hunk by its identity.
    #[must_use]
    pub fn hunk(&self, id: HunkId) -> Option<&Hunk> {
        self.hunks.get(usize::try_from(id.get()).ok()?)
    }

    /// Reports whether a bound stopped the collection of this file.
    #[must_use]
    pub const fn truncation(&self) -> DiffTruncation {
        self.truncation
    }

    /// Rebuilds the exact bytes that one side publishes.
    ///
    /// The result holds every published line of that side in order, with the
    /// line feed of each line. The last line of a side that holds no final line
    /// feed therefore ends the result without one.
    #[must_use]
    pub fn side_bytes(&self, side: DiffSide) -> Vec<u8> {
        let mut bytes = Vec::new();
        for line in self.hunks.iter().flat_map(|hunk| hunk.side_lines(side)) {
            bytes.extend_from_slice(line.text().as_bytes());
            if line.ending() == LineEnding::Newline {
                bytes.push(b'\n');
            }
        }
        bytes
    }
}

/// The reasons that one set of hunks is not usable.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TextDiffError {
    /// The file passed the published hunk bound.
    #[error("the file holds {actual} hunks; the limit is {max} hunks")]
    HunksLimit {
        /// The number of hunks that the caller supplied.
        actual: usize,
        /// The published bound.
        max: usize,
    },
    /// One hunk identity does not match its position.
    #[error("the hunk at position {expected} names identity {actual}")]
    HunkIdMismatch {
        /// The position of the hunk.
        expected: u32,
        /// The identity that the caller supplied.
        actual: u32,
    },
    /// Two hunks cover one line of the same side.
    #[error(
        "the {side:?} hunk starts at line {first} and the previous hunk ends at {previous_end}"
    )]
    RangeOverlap {
        /// The side that holds the overlap.
        side: DiffSide,
        /// The first line of the later hunk.
        first: u32,
        /// The first line after the earlier hunk.
        previous_end: u32,
    },
}

/// The reviewable content of one changed file.
///
/// [`FileDiff::new`] validates the content against the published modes, so a
/// binary file, a symbolic link, a submodule, and an unsupported mode stay
/// distinct and can never carry text hunks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffContent {
    /// Exact text hunks for the published sides.
    Text(TextDiff),
    /// Git reported binary content on at least one side.
    ///
    /// Kvim publishes no line, so no byte of a binary file can receive a
    /// review comment.
    Binary,
    /// A side holds a symbolic-link target instead of file content.
    SymbolicLink,
    /// A side holds a submodule commit identifier instead of file content.
    Submodule,
    /// A side holds a published mode that this release does not read.
    Unsupported,
}

impl DiffContent {
    const fn tag(&self) -> u8 {
        match self {
            Self::Text(_) => 0,
            Self::Binary => 1,
            Self::SymbolicLink => 2,
            Self::Submodule => 3,
            Self::Unsupported => 4,
        }
    }
}

/// One changed file of one candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileDiff {
    change: DiffChange,
    content: DiffContent,
}

impl FileDiff {
    /// Validates the sides and the content of one changed file.
    ///
    /// A modification names one path on both sides. A rename names two
    /// different paths. Text and binary content demands a mode that stores file
    /// content on every published side, and every other content demands the
    /// mode that explains it.
    pub fn new(change: DiffChange, content: DiffContent) -> Result<Self, FileDiffError> {
        match &change {
            DiffChange::Modified { old, new } if old.path() != new.path() => {
                return Err(FileDiffError::SidePathMismatch);
            }
            DiffChange::Renamed { old, new } if old.path() == new.path() => {
                return Err(FileDiffError::RenameToSelf);
            }
            _ => {}
        }

        let modes = [
            change.old_side().map(FileSide::mode),
            change.new_side().map(FileSide::mode),
        ];
        let mut published = modes.into_iter().flatten();
        let matches = match &content {
            DiffContent::Text(_) | DiffContent::Binary => published.all(FileMode::stores_text),
            DiffContent::SymbolicLink => published.any(|mode| mode == FileMode::SymbolicLink),
            DiffContent::Submodule => published.any(|mode| mode == FileMode::Submodule),
            DiffContent::Unsupported => {
                published.any(|mode| matches!(mode, FileMode::Unsupported(_)))
            }
        };
        if !matches {
            return Err(FileDiffError::ContentModeMismatch);
        }

        Ok(Self { change, content })
    }

    /// Returns the two published sides of the change.
    #[must_use]
    pub const fn change(&self) -> &DiffChange {
        &self.change
    }

    /// Returns the reviewable content of the change.
    #[must_use]
    pub const fn content(&self) -> &DiffContent {
        &self.content
    }

    /// Returns the path that a reader names for the change.
    #[must_use]
    pub const fn path(&self) -> &WorktreeRelativePath {
        self.change.path()
    }

    fn absorb(&self, hasher: &mut Hasher) {
        hasher.update(&[self.change.tag(), self.content.tag()]);
        for side in [self.change.old_side(), self.change.new_side()] {
            match side {
                None => {
                    hasher.update(&[0]);
                }
                Some(side) => {
                    hasher.update(&[1]);
                    side.absorb(hasher);
                }
            }
        }
        if let DiffContent::Text(text) = &self.content {
            hasher.update(&text.truncation().tag());
            absorb_count(hasher, text.hunks().len());
            for hunk in text.hunks() {
                for range in [hunk.old_range().0, hunk.new_range().0] {
                    hasher.update(&range.first.get().to_le_bytes());
                    hasher.update(&range.count.to_le_bytes());
                }
                absorb_count(hasher, hunk.lines().len());
                for line in hunk.lines() {
                    line.absorb(hasher);
                }
            }
        }
    }
}

/// The reasons that one changed file is not usable.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FileDiffError {
    /// A modification names two different paths.
    #[error("a modification names one path on both sides")]
    SidePathMismatch,
    /// A rename names one path on both sides.
    #[error("a rename names two different paths")]
    RenameToSelf,
    /// The content does not match the published modes.
    #[error("the published modes do not explain the content of the change")]
    ContentModeMismatch,
}

// ---------------------------------------------------------------------------
// Candidates
// ---------------------------------------------------------------------------

/// One immutable published candidate of one worktree diff.
///
/// The files rise by path, no path repeats, and every file belongs to the
/// target. The revision covers the complete published authority, so a candidate
/// with the same revision always shows the same review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeDiff {
    old: DiffOldSide,
    target: DiffTarget,
    files: Vec<FileDiff>,
    truncation: DiffTruncation,
    revision: DiffRevision,
}

impl WorktreeDiff {
    /// Validates one complete candidate and derives its revision.
    pub fn new(
        old: DiffOldSide,
        target: DiffTarget,
        authority: &CandidateAuthority,
        files: Vec<FileDiff>,
        truncation: DiffTruncation,
    ) -> Result<Self, WorktreeDiffError> {
        if files.len() > DIFF_FILES_MAX {
            return Err(WorktreeDiffError::FilesLimit {
                actual: files.len(),
                max: DIFF_FILES_MAX,
            });
        }

        let mut previous: Option<&WorktreeRelativePath> = None;
        for file in &files {
            if !target.selects(file.change()) {
                return Err(WorktreeDiffError::OutsideTarget {
                    path: file.path().as_path().display().to_string(),
                });
            }
            if previous.is_some_and(|earlier| earlier >= file.path()) {
                return Err(WorktreeDiffError::PathOrder {
                    path: file.path().as_path().display().to_string(),
                });
            }
            previous = Some(file.path());
        }

        let revision = DiffRevision::derive(old, authority, &files, truncation);
        Ok(Self {
            old,
            target,
            files,
            truncation,
            revision,
        })
    }

    /// Returns the state that the candidate compares against.
    ///
    /// The unstaged half of one review compares against the index, which is no
    /// commit, so the value names the state instead of a revision.
    #[must_use]
    pub const fn old_side(&self) -> DiffOldSide {
        self.old
    }

    /// Returns the selection that produced the candidate.
    #[must_use]
    pub const fn target(&self) -> &DiffTarget {
        &self.target
    }

    /// Returns the immutable identity of the candidate.
    #[must_use]
    pub const fn revision(&self) -> DiffRevision {
        self.revision
    }

    /// Returns every changed file, ordered by path.
    #[must_use]
    pub fn files(&self) -> &[FileDiff] {
        &self.files
    }

    /// Returns a BLAKE3 projection of all published candidate content.
    ///
    /// Capture authority and review freshness use this one canonical value.
    #[must_use]
    pub(crate) fn authority_projection(&self) -> [u8; DIGEST_BYTES] {
        let mut hasher = Hasher::new();
        hasher.update(PROJECTION_DOMAIN);
        self.old_side().absorb_into(&mut hasher);
        hasher.update(&[u8::from(self.truncation().is_truncated())]);
        absorb_count(&mut hasher, self.files().len());
        for file in self.files() {
            for side in [DiffSide::Old, DiffSide::New] {
                let Some(published) = file.change().side(side) else {
                    hasher.update(&[0]);
                    continue;
                };
                hasher.update(&[1]);
                absorb(
                    &mut hasher,
                    published.path().as_path().as_os_str().as_encoded_bytes(),
                );
                hasher.update(published.mode().as_octal().as_bytes());
                match file.content() {
                    DiffContent::Text(text) => absorb(&mut hasher, &text.side_bytes(side)),
                    DiffContent::Binary
                    | DiffContent::SymbolicLink
                    | DiffContent::Submodule
                    | DiffContent::Unsupported => absorb(&mut hasher, &[]),
                }
            }
        }
        *hasher.finalize().as_bytes()
    }

    /// Reports whether a bound stopped the collection of this candidate.
    #[must_use]
    pub const fn truncation(&self) -> DiffTruncation {
        self.truncation
    }

    /// Returns the changed file that one path names on either side.
    ///
    /// A rename answers under its old path and under its new path, and both
    /// answers hold the complete pair of sides.
    #[must_use]
    pub fn file(&self, path: &WorktreeRelativePath) -> Option<&FileDiff> {
        self.files.iter().find(|file| file.change().names(path))
    }
}

/// The reasons that one candidate is not usable.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum WorktreeDiffError {
    /// The candidate passed the published file bound.
    #[error("the candidate holds {actual} files; the limit is {max} files")]
    FilesLimit {
        /// The number of files that the caller supplied.
        actual: usize,
        /// The published bound.
        max: usize,
    },
    /// The target does not select one of the files.
    #[error("the target does not select {path}")]
    OutsideTarget {
        /// The path of the file that the target does not select.
        path: String,
    },
    /// The files do not rise by path.
    #[error("{path} does not follow the previous path")]
    PathOrder {
        /// The path that breaks the order.
        path: String,
    },
}

// ---------------------------------------------------------------------------
// Review anchors
// ---------------------------------------------------------------------------

/// A digest of the exact lines that one anchor selects.
///
/// The digest covers the bytes and the terminator of every selected line. It
/// covers no line number, so a selection keeps its identity after the lines
/// above it move.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SelectionDigest([u8; DIGEST_BYTES]);

impl SelectionDigest {
    /// Derives the digest of one run of selected lines.
    #[must_use]
    pub fn of<'a>(lines: impl IntoIterator<Item = &'a DiffLine>) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(SELECTION_DOMAIN);
        for line in lines {
            hasher.update(&[line.ending().tag()]);
            absorb(&mut hasher, line.text().as_bytes());
        }
        Self(*hasher.finalize().as_bytes())
    }

    /// Returns the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }
}

/// The place that one anchor names, on one side of one file.
///
/// The variant carries the range of its own side, so no anchor can name a range
/// of the side that it does not select.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AnchorLocation {
    /// A run of lines on the side that the base commit holds.
    Old {
        /// The selected run.
        range: OldLineRange,
    },
    /// A run of lines on the side that the candidate worktree holds.
    New {
        /// The selected run.
        range: NewLineRange,
    },
}

impl AnchorLocation {
    /// Returns the side that the location names.
    #[must_use]
    pub const fn side(self) -> DiffSide {
        match self {
            Self::Old { .. } => DiffSide::Old,
            Self::New { .. } => DiffSide::New,
        }
    }

    /// Returns the first selected line number.
    #[must_use]
    pub const fn first(self) -> u32 {
        match self {
            Self::Old { range } => range.first().get(),
            Self::New { range } => range.first().get(),
        }
    }

    /// Returns the number of selected lines.
    #[must_use]
    pub const fn count(self) -> u32 {
        match self {
            Self::Old { range } => range.count(),
            Self::New { range } => range.count(),
        }
    }

    fn build(side: DiffSide, first: u32, count: u32) -> Result<Self, LineNumberError> {
        Ok(match side {
            DiffSide::Old => Self::Old {
                range: OldLineRange::new(OldLine::new(first)?, count)
                    .expect("a hunk publishes no run above the range bound"),
            },
            DiffSide::New => Self::New {
                range: NewLineRange::new(NewLine::new(first)?, count)
                    .expect("a hunk publishes no run above the range bound"),
            },
        })
    }
}

/// The bounded lines around one selection.
///
/// The context proves the identity of a selection that moved. It stops at the
/// borders of the hunk that published the selection, so a selection at a border
/// keeps a shorter context.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnchorContext {
    before: Vec<DiffLineText>,
    after: Vec<DiffLineText>,
}

impl AnchorContext {
    /// Validates the bounded lines around one selection.
    pub fn new(
        before: Vec<DiffLineText>,
        after: Vec<DiffLineText>,
    ) -> Result<Self, AnchorContextError> {
        for lines in [&before, &after] {
            if lines.len() > REVIEW_CONTEXT_LINES_MAX {
                return Err(AnchorContextError::Limit {
                    actual: lines.len(),
                    max: REVIEW_CONTEXT_LINES_MAX,
                });
            }
        }
        Ok(Self { before, after })
    }

    /// Returns the lines above the selection, in file order.
    #[must_use]
    pub fn before(&self) -> &[DiffLineText] {
        &self.before
    }

    /// Returns the lines below the selection, in file order.
    #[must_use]
    pub fn after(&self) -> &[DiffLineText] {
        &self.after
    }
}

/// The reasons that one anchor context is not usable.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AnchorContextError {
    /// The context passed the published line bound.
    #[error("the context holds {actual} lines; the limit is {max} lines")]
    Limit {
        /// The number of lines that the caller supplied.
        actual: usize,
        /// The published bound.
        max: usize,
    },
}

/// One durable place inside one candidate.
///
/// The anchor names the base revision, the candidate revision, the path, the
/// selected side, the hunk identity, the line range, a digest of the selected
/// lines, and the bounded lines around them. [`relocate`] compares it with a
/// later candidate.
///
/// # Examples
///
/// ```
/// # use kvim_path::WorktreeRelativePath;
/// # use kvim_workspace::*;
/// # fn candidate() -> Result<WorktreeDiff, Box<dyn std::error::Error>> {
/// #     let side = FileSide::new(WorktreeRelativePath::new("a.txt")?, FileMode::Regular);
/// #     let hunk = Hunk::new(
/// #         HunkId::new(0),
/// #         OldLineRange::new(OldLine::new(1)?, 0)?,
/// #         NewLineRange::new(NewLine::new(1)?, 1)?,
/// #         vec![DiffLine::new(
/// #             LineOrigin::Added { new: NewLine::new(1)? },
/// #             DiffLineText::new(*b"hello")?,
/// #             LineEnding::EndOfFile,
/// #         )],
/// #     )?;
/// #     let file = FileDiff::new(
/// #         DiffChange::Added { new: side },
/// #         DiffContent::Text(TextDiff::new(vec![hunk], DiffTruncation::Complete)?),
/// #     )?;
/// #     let authority =
/// #         CandidateAuthority::new(HeadAuthority::Unborn, IndexAuthority::from_digest([7; 32]));
/// #     Ok(WorktreeDiff::new(
/// #         DiffOldSide::Commit(BaseRevision::new(
/// #             "0123456789abcdef0123456789abcdef01234567",
/// #         )?),
/// #         DiffTarget::Worktree,
/// #         &authority,
/// #         vec![file],
/// #         DiffTruncation::Complete,
/// #     )?)
/// # }
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let diff = candidate()?;
/// let path = WorktreeRelativePath::new("a.txt")?;
/// let location = AnchorLocation::New {
///     range: NewLineRange::new(NewLine::new(1)?, 1)?,
/// };
///
/// let anchor = ReviewAnchor::select(&diff, &path, HunkId::new(0), location)?;
/// assert_eq!(anchor.candidate(), diff.revision());
/// assert!(matches!(relocate(&anchor, &diff), Relocation::Exact { .. }));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewAnchor {
    old: DiffOldSide,
    candidate: DiffRevision,
    path: WorktreeRelativePath,
    hunk: HunkId,
    location: AnchorLocation,
    selection: SelectionDigest,
    context: AnchorContext,
}

impl ReviewAnchor {
    /// Anchors one selection inside one candidate.
    ///
    /// The candidate must publish the named file as text, the named hunk, and
    /// every selected line. The constructor derives the selection digest and
    /// the bounded context from those lines.
    pub fn select(
        candidate: &WorktreeDiff,
        path: &WorktreeRelativePath,
        hunk: HunkId,
        location: AnchorLocation,
    ) -> Result<Self, ReviewAnchorError> {
        let file = candidate.file(path).ok_or(ReviewAnchorError::FileMissing)?;
        let DiffContent::Text(text) = file.content() else {
            return Err(ReviewAnchorError::NoText);
        };
        let hunk_value = text.hunk(hunk).ok_or(ReviewAnchorError::HunkMissing)?;

        let side = location.side();
        let count = usize::try_from(location.count()).expect("a bounded count fits one usize");
        if count == 0 {
            return Err(ReviewAnchorError::EmptySelection);
        }

        let lines: Vec<&DiffLine> = hunk_value.side_lines(side).collect();
        let start = usize::try_from(
            location
                .first()
                .checked_sub(hunk_value.first_line_of(side))
                .ok_or(ReviewAnchorError::LinesMissing)?,
        )
        .expect("a bounded offset fits one usize");
        let end = start
            .checked_add(count)
            .ok_or(ReviewAnchorError::LinesMissing)?;
        if end > lines.len() {
            return Err(ReviewAnchorError::LinesMissing);
        }

        Ok(Self {
            old: candidate.old_side(),
            candidate: candidate.revision(),
            path: path.clone(),
            hunk,
            location,
            selection: SelectionDigest::of(lines[start..end].iter().copied()),
            context: context_around(&lines, start, end),
        })
    }

    /// Returns the state that the review compares against.
    ///
    /// The unstaged half of one review compares against the index, so the
    /// anchor names the index digest that held its old lines rather than a
    /// commit that never held them.
    #[must_use]
    pub const fn old_side(&self) -> DiffOldSide {
        self.old
    }

    /// Returns the candidate that published the selection.
    #[must_use]
    pub const fn candidate(&self) -> DiffRevision {
        self.candidate
    }

    /// Returns the worktree-relative path of the selection.
    #[must_use]
    pub const fn path(&self) -> &WorktreeRelativePath {
        &self.path
    }

    /// Returns the hunk that published the selection.
    #[must_use]
    pub const fn hunk(&self) -> HunkId {
        self.hunk
    }

    /// Returns the side and the range of the selection.
    #[must_use]
    pub const fn location(&self) -> AnchorLocation {
        self.location
    }

    /// Returns the side that the selection names.
    #[must_use]
    pub const fn side(&self) -> DiffSide {
        self.location.side()
    }

    /// Returns the digest of the selected lines.
    #[must_use]
    pub const fn selection(&self) -> SelectionDigest {
        self.selection
    }

    /// Returns the bounded lines around the selection.
    #[must_use]
    pub const fn context(&self) -> &AnchorContext {
        &self.context
    }
}

/// The reasons that one candidate cannot anchor a selection.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ReviewAnchorError {
    /// The candidate publishes no file under the path.
    #[error("the candidate publishes no file under the path")]
    FileMissing,
    /// The file publishes no reviewable text.
    #[error("the file publishes no reviewable line")]
    NoText,
    /// The file publishes no hunk under the identity.
    #[error("the file publishes no hunk under the identity")]
    HunkMissing,
    /// The selection covers no line.
    #[error("a selection covers at least one line")]
    EmptySelection,
    /// The hunk publishes no line of the selected range.
    #[error("the hunk publishes no line of the selected range")]
    LinesMissing,
}

/// The reason that one relocation found no single place.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AmbiguityReason {
    /// More than one place of the candidate matches the anchor.
    MultipleMatches,
    /// The search bound stopped before the whole file was compared.
    ///
    /// The part that the search did not compare can still hold another match,
    /// so the outcome names an ambiguity rather than a place.
    SearchLimit,
}

/// The outcome of one relocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Relocation {
    /// The candidate holds the selection at the same place.
    Exact {
        /// The anchor against the later candidate.
        anchor: ReviewAnchor,
    },
    /// The candidate holds the selection at another place.
    Relocated {
        /// The anchor against the later candidate, at its new place.
        anchor: ReviewAnchor,
    },
    /// The candidate holds the selection nowhere.
    Missing,
    /// The candidate holds no single place for the selection.
    Ambiguous(AmbiguityReason),
}

/// Compares one anchor with a later candidate.
///
/// The search compares the selection digest and the bounded context of every
/// window of the anchored side. The context of a window agrees with the anchor
/// when both hold the same lines outward from the selection, for as many lines
/// as both hold. A later candidate that publishes a shorter context therefore
/// still matches, and a disagreement inside the shared part never does.
///
/// The function returns one place only when exactly one window matches and the
/// whole file was compared. It therefore never guesses among matches.
#[must_use]
pub fn relocate(anchor: &ReviewAnchor, candidate: &WorktreeDiff) -> Relocation {
    let Some(file) = candidate.file(anchor.path()) else {
        return Relocation::Missing;
    };
    let DiffContent::Text(text) = file.content() else {
        return Relocation::Missing;
    };

    let side = anchor.side();
    let count = usize::try_from(anchor.location().count()).expect("a bounded count fits one usize");
    if count == 0 {
        return Relocation::Missing;
    }

    let mut examined = 0_usize;
    let mut found: Option<(HunkId, u32)> = None;
    for hunk in text.hunks() {
        let lines: Vec<&DiffLine> = hunk.side_lines(side).collect();
        let Some(last_start) = lines.len().checked_sub(count) else {
            continue;
        };
        for start in 0..=last_start {
            examined += 1;
            if examined > RELOCATION_WINDOWS_MAX {
                return Relocation::Ambiguous(AmbiguityReason::SearchLimit);
            }
            let end = start + count;
            if SelectionDigest::of(lines[start..end].iter().copied()) != anchor.selection() {
                continue;
            }
            if !context_matches(anchor.context(), &context_around(&lines, start, end)) {
                continue;
            }
            if found.is_some() {
                return Relocation::Ambiguous(AmbiguityReason::MultipleMatches);
            }
            let first = lines[start]
                .number(side)
                .expect("a side line carries the number of its own side");
            found = Some((hunk.id(), first));
        }
    }

    let Some((hunk, first)) = found else {
        return Relocation::Missing;
    };
    let count = u32::try_from(count).expect("a bounded count fits one u32");
    let location = AnchorLocation::build(side, first, count)
        .expect("the candidate published the matched line numbers");
    let relocated = ReviewAnchor::select(candidate, anchor.path(), hunk, location)
        .expect("the candidate published the matched lines");
    if hunk == anchor.hunk() && location == anchor.location() {
        Relocation::Exact { anchor: relocated }
    } else {
        Relocation::Relocated { anchor: relocated }
    }
}

/// The text of one review comment.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CommentBody(Box<str>);

impl CommentBody {
    /// Validates the text of one review comment.
    ///
    /// The text holds at least one character that is no whitespace, and it
    /// stays inside the published byte bound.
    pub fn new(text: impl Into<String>) -> Result<Self, CommentBodyError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(CommentBodyError::Empty);
        }
        if text.len() > REVIEW_COMMENT_BYTES_MAX {
            return Err(CommentBodyError::Limit {
                actual: text.len(),
                max: REVIEW_COMMENT_BYTES_MAX,
            });
        }
        Ok(Self(text.into_boxed_str()))
    }

    /// Returns the text of the comment.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        &self.0
    }
}

/// The reasons that one comment text is not usable.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CommentBodyError {
    /// The comment holds no character that is no whitespace.
    #[error("a comment holds at least one character")]
    Empty,
    /// The comment passed the published byte bound.
    #[error("the comment holds {actual} bytes; the limit is {max} bytes")]
    Limit {
        /// The number of bytes that the caller supplied.
        actual: usize,
        /// The published bound.
        max: usize,
    },
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Collects the bounded context around one window of side lines.
fn context_around(lines: &[&DiffLine], start: usize, end: usize) -> AnchorContext {
    let before_start = start.saturating_sub(REVIEW_CONTEXT_LINES_MAX);
    let after_end = end
        .saturating_add(REVIEW_CONTEXT_LINES_MAX)
        .min(lines.len());
    let collect = |range: &[&DiffLine]| -> Vec<DiffLineText> {
        range.iter().map(|line| line.text().clone()).collect()
    };
    AnchorContext {
        before: collect(&lines[before_start..start]),
        after: collect(&lines[end..after_end]),
    }
}

/// Reports whether one found context agrees with one recorded context.
///
/// The comparison runs outward from the selection and stops where either side
/// ends, so a context that a later candidate shortened still agrees.
fn context_matches(recorded: &AnchorContext, found: &AnchorContext) -> bool {
    let before = recorded
        .before()
        .iter()
        .rev()
        .zip(found.before().iter().rev())
        .all(|(recorded, found)| recorded == found);
    let after = recorded
        .after()
        .iter()
        .zip(found.after().iter())
        .all(|(recorded, found)| recorded == found);
    before && after
}

/// Absorbs one length-prefixed byte string, so two fields never run together.
pub(crate) fn absorb(hasher: &mut Hasher, bytes: &[u8]) {
    absorb_count(hasher, bytes.len());
    hasher.update(bytes);
}

pub(crate) fn absorb_count(hasher: &mut Hasher, count: usize) {
    let count = u64::try_from(count).expect("a bounded count fits one u64");
    hasher.update(&count.to_le_bytes());
}

fn decode_hex(hex: &[u8], digest: &mut [u8]) -> Result<(), BaseRevisionError> {
    debug_assert_eq!(
        hex.len(),
        digest.len() * 2,
        "the caller sized the digest from the accepted hexadecimal length"
    );
    for (index, byte) in digest.iter_mut().enumerate() {
        let high = hex_value(hex[index * 2]).ok_or(BaseRevisionError::NotHexadecimal {
            position: index * 2,
        })?;
        let low = hex_value(hex[index * 2 + 1]).ok_or(BaseRevisionError::NotHexadecimal {
            position: index * 2 + 1,
        })?;
        *byte = (high << 4) | low;
    }
    Ok(())
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(DIGITS[usize::from(byte >> 4)] as char);
        hex.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    hex
}

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;
