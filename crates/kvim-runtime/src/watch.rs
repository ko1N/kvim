//! The portable filesystem watcher of one workspace root.
//!
//! The service is the only place that speaks to `notify`. It converts every
//! platform event into one typed [`WatchEvent`], drops the paths that the caller
//! ignores, and coalesces the events of one burst into one [`WatchBatch`]. No
//! `notify` type leaves this module, so no consumer branches on a platform
//! detail. See `docs/files.md` and `docs/responsiveness.md`.
//!
//! One logical change writes many platform events: a compiler run writes
//! thousands of them, and one editor save writes several. The service therefore
//! collects events for [`WATCH_COALESCE_WINDOW`] before it publishes, so the
//! terminal event loop reads one value for one burst.
//!
//! Every queue is bounded. A full queue drops events and reports the drop as
//! [`WatchFidelity::Dropped`], so the consumer knows that its knowledge of the
//! change is incomplete and can read the complete state again.
//!
//! The registration runs inside the coalescing task, not inside
//! [`FileWatcher::start`], so the consumer draws its first frame before the
//! first watch exists. No watch covers that window, so the first published
//! burst reports [`WatchFidelity::Dropped`] as a full queue does.
//!
//! One registration can leave a part of the workspace without a watch. The
//! platform refuses a watch at a limit of the host, and a bound of this module
//! stops the walk of a very large tree. Every burst therefore carries one
//! [`WatchCoverage`], which names the gap and its cause, so the consumer keeps
//! every feature and still knows what the watch does not cover.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
#[cfg(test)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use notify::event::{EventKind, ModifyKind};
use notify::{Config, ErrorKind, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use kvim_path::{
    ResolvedTargetState, WorktreeConfinementError, WorktreeDirectoryPath, WorktreeRelativePath,
    WorktreeRelativePathError, WorktreeRoot,
};

/// The time that one burst collects events before the service publishes it.
///
/// One logical change writes many platform events, and a build writes
/// thousands. The window turns one burst into one published value. It stays
/// short enough that a file the user creates appears at once, and long enough
/// that a save of one editor, which writes a temporary file and renames it,
/// never produces two reads of one directory.
pub const WATCH_COALESCE_WINDOW: Duration = Duration::from_millis(200);

/// The largest number of platform events that wait for the coalescing task.
pub const WATCH_EVENT_QUEUE_MAX: usize = 1024;

/// The largest number of coalesced bursts that wait for the event loop.
pub const WATCH_BATCH_QUEUE_MAX: usize = 16;

/// The largest number of directories that one burst names.
pub const WATCH_BATCH_DIRECTORIES_MAX: usize = 64;

/// The largest number of platform events that one burst inspects.
pub const WATCH_BURST_EVENTS_MAX: usize = 4096;

/// The largest number of directories that one registration watches.
pub const WATCH_DIRECTORIES_MAX: usize = 4096;

/// The largest depth below the root that one registration reaches.
pub const WATCH_DEPTH_MAX: usize = 16;

/// The largest number of entries that one registration reads of one directory.
pub const WATCH_DIRECTORY_SCAN_MAX: usize = 4096;

/// What happened to one watched path.
///
/// The value names the effect on the directory that holds the path, never the
/// platform mechanism that produced it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchKind {
    /// One entry appeared in its directory.
    Created,
    /// One entry disappeared from its directory.
    Removed,
    /// One entry took another name, or another directory.
    Renamed,
    /// The content or the metadata of one entry changed.
    ///
    /// The directory of the entry keeps exactly the same entries.
    Modified,
    /// The platform reported a change without naming its kind.
    ///
    /// The directory of the path may hold other entries now, so a consumer must
    /// treat the value like a structural change.
    Unknown,
}

impl WatchKind {
    /// Reports whether the directory of the path may hold other entries now.
    #[must_use]
    pub const fn changes_listing(self) -> bool {
        match self {
            Self::Created | Self::Removed | Self::Renamed | Self::Unknown => true,
            Self::Modified => false,
        }
    }
}

/// One filesystem change of the watched tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchEvent {
    root: Arc<WorktreeRoot>,
    /// The absolute path that changed.
    pub path: PathBuf,
    /// The effect of the change.
    pub kind: WatchKind,
}

enum RawWatchEvent {
    Event { path: PathBuf, kind: WatchKind },
    Dropped,
}

impl WatchEvent {
    /// Validates one platform path against its watcher root.
    ///
    /// Existing symbolic links must resolve inside the root. Missing paths are
    /// accepted for removal events after their nearest existing parent passes
    /// confinement.
    pub fn new(
        root: Arc<WorktreeRoot>,
        path: PathBuf,
        kind: WatchKind,
    ) -> Result<Self, WatchEventError> {
        let relative = path
            .strip_prefix(root.as_path())
            .map_err(|_| WatchEventError::OutsideRoot)?;
        let relative = WorktreeRelativePath::new(relative)?;
        root.resolve_directory(&WorktreeDirectoryPath::Relative(relative.clone()))?;
        Ok(Self {
            path: root.as_path().join(relative.as_path()),
            root,
            kind,
        })
    }
}

/// A platform event path rejected by its watcher root.
#[derive(Debug, Error)]
pub enum WatchEventError {
    /// The platform path belongs to another filesystem root.
    #[error("the watch event lies outside its worktree root")]
    OutsideRoot,
    /// The relative event path is structurally invalid.
    #[error("the watch event path is invalid")]
    InvalidPath(#[from] WorktreeRelativePathError),
    /// The event target does not remain confined to the worktree.
    #[error("the watch event target is not confined to the worktree")]
    Confinement(#[from] WorktreeConfinementError),
}

/// Whether one burst holds every event of its window.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WatchFidelity {
    /// The burst holds every event that the window produced.
    #[default]
    Complete,
    /// A bound dropped at least one event of the window, or no watch observed
    /// the window at all.
    ///
    /// The named directories are incomplete, so a consumer that needs the
    /// complete state must read every directory that it holds.
    Dropped,
}

/// Returns the name of the host setting that bounds the number of watches.
///
/// A host that refuses a watch needs an action of the user, so the report of
/// that refusal names the setting that holds the limit. This module is the one
/// portable boundary of the watcher, so the name never reaches a consumer as a
/// platform branch. A platform that publishes no such name returns `None`, and
/// the report then names the limit alone.
///
/// See `docs/files.md`.
#[must_use]
pub const fn watch_limit_setting() -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        Some("fs.inotify.max_user_watches")
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// The part of the workspace that carries no watch.
///
/// The default value names no gap, so a registration that covers the whole
/// workspace reports nothing. Every burst carries the coverage of the
/// registration that ran before it.
///
/// The two causes need two different actions. The host refuses a watch at a
/// limit of the host, which the user raises. A bound of this module stops the
/// walk of a very large tree, of a very deep tree, and of a very large
/// directory, which the user cannot raise and reads by hand.
///
/// # Examples
///
/// ```
/// use kvim_runtime::WatchCoverage;
///
/// assert!(WatchCoverage::default().is_complete());
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WatchCoverage {
    /// The number of directories that the platform refused to watch.
    ///
    /// A directory that another program removed between the walk and the watch
    /// call counts nothing. That directory holds no entry to watch, and its
    /// parent still reports the removal.
    pub refused: usize,
    /// Whether the watch limit of the host caused at least one refusal.
    ///
    /// [`watch_limit_setting`] names the setting that holds that limit.
    pub at_limit: bool,
    /// Whether a bound of this module stopped the walk of the workspace.
    pub truncated: bool,
}

impl WatchCoverage {
    /// Reports whether every directory of the workspace carries one watch.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.refused == 0 && !self.truncated
    }

    /// Adds every gap of `other` to the gaps of this value.
    const fn merge(&mut self, other: Self) {
        self.refused = self.refused.saturating_add(other.refused);
        self.at_limit |= other.at_limit;
        self.truncated |= other.truncated;
    }
}

/// The coalesced filesystem changes of one window.
///
/// The value is the whole result of one burst: the directories whose listing may
/// have changed, whether any watched content changed, and whether a bound
/// dropped an event.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use kvim_path::WorktreeRoot;
/// use kvim_runtime::{WatchBatch, WatchEvent, WatchFidelity, WatchKind};
///
/// let root = Arc::new(WorktreeRoot::open(std::env::current_dir()?)?);
/// let mut batch = WatchBatch::default();
/// batch.push(&WatchEvent::new(
///     Arc::clone(&root),
///     root.as_path().join("src/main.rs"),
///     WatchKind::Created,
/// )?);
/// // A second change of the same directory adds no second read.
/// batch.push(&WatchEvent::new(
///     Arc::clone(&root),
///     root.as_path().join("src/lib.rs"),
///     WatchKind::Created,
/// )?);
///
/// assert_eq!(batch.directories(), [root.as_path().join("src")]);
/// assert_eq!(batch.fidelity(), WatchFidelity::Complete);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WatchBatch {
    root: Option<Arc<WorktreeRoot>>,
    directories: BTreeSet<PathBuf>,
    content_changed: bool,
    fidelity: WatchFidelity,
    coverage: WatchCoverage,
}

impl WatchBatch {
    fn new(root: Arc<WorktreeRoot>) -> Self {
        Self {
            root: Some(root),
            ..Self::default()
        }
    }

    /// Adds one event to the burst.
    ///
    /// A change that keeps the entries of its directory records the content
    /// change alone, so a write inside one file never asks for a directory read.
    /// A burst above [`WATCH_BATCH_DIRECTORIES_MAX`] directories keeps the
    /// directories that it already holds and reports
    /// [`WatchFidelity::Dropped`].
    pub fn push(&mut self, event: &WatchEvent) {
        match self.root.as_ref() {
            Some(root) if root != &event.root => {
                self.fidelity = WatchFidelity::Dropped;
                return;
            }
            Some(_) => {}
            None => self.root = Some(Arc::clone(&event.root)),
        }
        if !event.kind.changes_listing() {
            self.content_changed = true;
            return;
        }
        let Some(directory) = event.path.parent() else {
            // A path without a parent is the filesystem root, which no
            // workspace watch reports.
            debug_assert!(false, "every watched path lies below the watched root");
            return;
        };
        if self.directories.contains(directory) {
            return;
        }
        if self.directories.len() >= WATCH_BATCH_DIRECTORIES_MAX {
            self.fidelity = WatchFidelity::Dropped;
            return;
        }
        self.directories.insert(directory.to_path_buf());
    }

    /// Records that a bound dropped at least one event of this burst.
    pub const fn drop_events(&mut self) {
        self.fidelity = WatchFidelity::Dropped;
    }

    /// Records the part of the workspace that carries no watch.
    ///
    /// The registration that ran before this burst produces the value, so the
    /// burst that opens the stream carries the coverage of the first walk.
    pub const fn set_coverage(&mut self, coverage: WatchCoverage) {
        self.coverage = coverage;
    }

    /// Returns the part of the workspace that carries no watch.
    #[must_use]
    pub const fn coverage(&self) -> WatchCoverage {
        self.coverage
    }

    /// Returns the directories whose listing may have changed, in path order.
    #[must_use]
    pub fn directories(&self) -> Vec<PathBuf> {
        self.directories.iter().cloned().collect()
    }

    /// Returns the root that validated this burst, or `None` before any event.
    #[must_use]
    pub fn root(&self) -> Option<&WorktreeRoot> {
        self.root.as_deref()
    }

    /// Reports whether the burst changed the content or metadata of one entry.
    #[must_use]
    pub const fn changed_content(&self) -> bool {
        self.content_changed
    }

    /// Returns whether the burst holds every event of its window.
    #[must_use]
    pub const fn fidelity(&self) -> WatchFidelity {
        self.fidelity
    }
}

/// The reason that no watcher runs over the workspace.
///
/// A rejected watcher is a normal state. The caller keeps every other feature
/// and refreshes the tree by hand instead.
#[derive(Debug, Error)]
pub enum WatchError {
    /// The platform refused the watch.
    #[error("the filesystem watcher could not start")]
    Start(#[source] notify::Error),
}

/// Returns the typed kind of one platform event.
///
/// A read never changes an entry, so it produces no value at all.
const fn classify(kind: EventKind) -> Option<WatchKind> {
    match kind {
        EventKind::Create(_) => Some(WatchKind::Created),
        EventKind::Remove(_) => Some(WatchKind::Removed),
        EventKind::Modify(ModifyKind::Name(_)) => Some(WatchKind::Renamed),
        EventKind::Modify(_) => Some(WatchKind::Modified),
        EventKind::Access(_) => None,
        EventKind::Any | EventKind::Other => Some(WatchKind::Unknown),
    }
}

fn enqueue_notify_result(
    result: notify::Result<notify::Event>,
    events: &mpsc::Sender<RawWatchEvent>,
    root: &WorktreeRoot,
    ignored: &[&str],
    dropped: &AtomicUsize,
) {
    let event = match result {
        Ok(event) => event,
        Err(_) => {
            dropped.fetch_add(1, Ordering::Relaxed);
            let _ = events.try_send(RawWatchEvent::Dropped);
            return;
        }
    };
    let Some(kind) = classify(event.kind) else {
        return;
    };
    for path in event.paths {
        if path.starts_with(root.as_path()) && is_ignored(root.as_path(), &path, ignored) {
            continue;
        }
        if events
            .try_send(RawWatchEvent::Event { path, kind })
            .is_err()
        {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Reports whether one ignored name lies on the path below the watched root.
///
/// The comparison starts below the root, so a workspace whose own directory
/// carries an ignored name still reports every change inside it.
#[must_use]
pub fn is_ignored(root: &Path, path: &Path, ignored: &[&str]) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        // A path outside the watched root belongs to no workspace directory.
        return true;
    };
    relative.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| ignored.contains(&name))
    })
}

/// The directories of one walk, and whether a bound stopped that walk.
struct WalkOutcome {
    /// Every kept directory that the walk reached, in the order that it read
    /// them.
    directories: Vec<WorktreeDirectoryPath>,
    /// Whether a bound of this module left a kept directory out of the walk.
    ///
    /// The directory bound, the depth bound, and the scan bound of one
    /// directory read all report here. [`WATCH_DIRECTORY_SCAN_MAX`] reports a
    /// possible gap, not a certain one. It counts every entry, so it stops a
    /// read of many plain files as well, and the walk cannot then know whether
    /// a directory stands after the bound. The report stays conservative,
    /// because a silent loss costs the user more than one manual refresh.
    truncated: bool,
}

/// Returns `start` and every kept directory below it that `registered` misses.
///
/// The walk always reads `start`, so a directory that already carries a watch
/// still reports the directories that appeared inside it. It reports `start`
/// itself only when `registered` does not hold it. It skips a directory whose
/// name `ignored` holds, and it reads no directory below such a name.
///
/// The walk stops at a directory that `registered` already holds, because that
/// directory reports its own new entries through its own watch. One walk
/// therefore costs one directory read for each directory that a burst names.
///
/// The walk skips a directory that it cannot read, because an unreadable
/// subtree reports no change to any reader. It also skips a symbolic link,
/// because the type comes from the directory read and names the link itself, so
/// no link builds a cycle and no tree is watched twice.
///
/// The walk returns at most `limit` directories, it reaches at most
/// [`WATCH_DEPTH_MAX`] levels below the root, and it keeps at most
/// [`WATCH_DIRECTORY_SCAN_MAX`] entries of one directory, so a very large tree
/// costs bounded time.
///
/// The directory bound, the depth bound, and the scan bound all report
/// [`WalkOutcome::truncated`] when they leave one kept directory out. The walk
/// reads a directory at the depth bound to answer that question, and it stops
/// that read at the first kept directory below the bound, so an exactly deep
/// tree reports no gap of its own. The walk reads one entry past the scan bound
/// for the same reason, so a directory of exactly that many entries reports no
/// gap either.
///
/// A `start` outside `root` returns no directory. The platform names the
/// watched root itself when that root disappears, and the burst then names the
/// directory above the root, which no workspace watch covers.
fn unregistered_directories(
    root: &WorktreeRoot,
    start: &WorktreeDirectoryPath,
    ignored: &[&str],
    registered: &BTreeSet<WorktreeDirectoryPath>,
    limit: usize,
) -> WalkOutcome {
    let mut outcome = WalkOutcome {
        directories: Vec::new(),
        truncated: false,
    };
    let start_depth = start
        .relative_path()
        .map_or(0, |path| path.as_path().components().count());
    if start_depth > WATCH_DEPTH_MAX || limit == 0 {
        // A bound of this module stops the walk before it reads one directory.
        outcome.truncated = true;
        return outcome;
    }
    if !registered.contains(start)
        && !is_ignored(root.as_path(), &start.display_path(root), ignored)
    {
        outcome.directories.push(start.clone());
    }
    let mut queue: VecDeque<(WorktreeDirectoryPath, usize)> = VecDeque::new();
    queue.push_back((start.clone(), start_depth));
    while let Some((directory, depth)) = queue.pop_front() {
        let Ok(listing) = root.directory().read_dir(directory.capability_path()) else {
            // An unreadable directory reports no change of its own entries, and
            // its parent still reports every change of the directory itself.
            continue;
        };
        // The count holds every entry that the read returned, so an unreadable
        // entry costs the same bound as a readable one.
        let mut scanned = 0_usize;
        for entry in listing {
            if scanned >= WATCH_DIRECTORY_SCAN_MAX {
                // The scan bound ends this read. A directory after the bound
                // gets no watch of its own, and the walk reads no entry type
                // after the bound, so it reports a possible gap and not a
                // certain one. The user raises no bound of this module, so the
                // gap needs the same manual refresh as the other two.
                outcome.truncated = true;
                break;
            }
            scanned = scanned.saturating_add(1);
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(path) = watch_child_path(&directory, &entry.file_name()) else {
                outcome.truncated = true;
                continue;
            };
            let path = WorktreeDirectoryPath::Relative(path);
            // The registration and the callback ask the same question, so the
            // two filters can never disagree about one entry.
            if is_ignored(root.as_path(), &path.display_path(root), ignored) {
                continue;
            }
            if registered.contains(&path) {
                continue;
            }
            if depth >= WATCH_DEPTH_MAX {
                // The depth bound stops the walk here, and this directory names
                // the part of the tree that carries no watch.
                outcome.truncated = true;
                break;
            }
            if outcome.directories.len() >= limit {
                outcome.truncated = true;
                return outcome;
            }
            outcome.directories.push(path.clone());
            queue.push_back((path, depth.saturating_add(1)));
        }
    }
    outcome
}

fn watch_child_path(
    directory: &WorktreeDirectoryPath,
    name: &std::ffi::OsStr,
) -> Result<WorktreeRelativePath, WorktreeRelativePathError> {
    let path = directory.relative_path().map_or_else(
        || PathBuf::from(name),
        |directory| directory.as_path().join(name),
    );
    WorktreeRelativePath::new(path)
}

/// Returns the gap that one refused directory adds to the registration.
///
/// A path that no longer exists adds no gap. Another program may remove one
/// directory between the walk and the watch call, and the parent of that
/// directory still reports the removal.
///
/// The watch limit of the host is the one cause that the user raises, so the
/// value keeps that cause apart from every other refusal.
fn refusal(error: &notify::Error) -> WatchCoverage {
    match &error.kind {
        ErrorKind::PathNotFound => WatchCoverage::default(),
        ErrorKind::Io(failure) if failure.kind() == io::ErrorKind::NotFound => {
            WatchCoverage::default()
        }
        ErrorKind::MaxFilesWatch => WatchCoverage {
            refused: 1,
            at_limit: true,
            truncated: false,
        },
        _ => WatchCoverage {
            refused: 1,
            at_limit: false,
            truncated: false,
        },
    }
}

/// The values that one registration needs, before that registration runs.
///
/// [`FileWatcher::start`] builds the value and hands it to the coalescing task,
/// which places every watch beside the terminal event loop. The value therefore
/// keeps the walk and the platform call off the path to the first frame.
struct PendingRegistration {
    /// The platform watcher, which holds no watch yet.
    watcher: RecommendedWatcher,
    /// The absolute workspace root of every watch.
    root: Arc<WorktreeRoot>,
    /// The directory names that carry no watch and produce no event.
    ignored: &'static [&'static str],
}

/// The platform watcher and the directories that its watches already cover.
///
/// The coalescing task owns the value, so every watch call runs beside the
/// terminal event loop and never on it. The platform callback holds no part of
/// the value, so it needs no lock and always returns at once.
struct Registration {
    /// The absolute workspace root of every watch.
    root: Arc<WorktreeRoot>,
    /// The directory names that carry no watch and produce no event.
    ignored: &'static [&'static str],
    /// Every directory that the registration covers.
    ///
    /// The set also holds a directory that the platform refused, so one refused
    /// directory costs one attempt instead of one attempt for each burst.
    directories: BTreeSet<WorktreeDirectoryPath>,
    /// The platform watcher. Dropping it ends its callback thread.
    watcher: RecommendedWatcher,
}

impl Registration {
    /// Adds one watch for the root and for every directory that stays.
    ///
    /// Every watch covers one directory alone, so the platform adds no watch of
    /// its own below an ignored name.
    ///
    /// Returns the registration and the part of the workspace that carries no
    /// watch, so the burst that follows the registration names every gap.
    ///
    /// # Errors
    ///
    /// Returns [`WatchError::Start`] when the platform refuses the root or the
    /// batch. A refused directory below the root loses the events of that
    /// directory alone, and its parent still reports every change of the
    /// directory itself.
    fn start(
        watcher: RecommendedWatcher,
        root: Arc<WorktreeRoot>,
        ignored: &'static [&'static str],
    ) -> Result<(Self, WatchCoverage), WatchError> {
        let walk = unregistered_directories(
            &root,
            &WorktreeDirectoryPath::Root,
            ignored,
            &BTreeSet::new(),
            WATCH_DIRECTORIES_MAX,
        );
        let mut registration = Self {
            root,
            ignored,
            directories: BTreeSet::new(),
            watcher,
        };
        let mut coverage = registration
            .add(&walk.directories)
            .map_err(WatchError::Start)?;
        coverage.truncated |= walk.truncated;
        Ok((registration, coverage))
    }

    /// Adds one watch for each kept directory that appeared below `changed`.
    ///
    /// A watch covers one directory alone, so a directory that appeared after
    /// the last walk carries no watch and reports no change inside it. The call
    /// reads every directory that the burst named, walks each new subtree, and
    /// adds every new directory in one batch.
    ///
    /// The registration stays at or below [`WATCH_DIRECTORIES_MAX`]
    /// directories, and the walk reaches at most [`WATCH_DEPTH_MAX`] levels
    /// under the root. A tree above either bound keeps the watches that it
    /// already holds and receives no further watch.
    ///
    /// A directory that disappeared between the burst and the batch produces no
    /// entry, because its read fails and its parent still reports its removal.
    ///
    /// Returns the part of this batch that carries no watch, so the burst of
    /// the same window names every gap that the batch left.
    fn extend(&mut self, changed: &[PathBuf]) -> WatchCoverage {
        let mut coverage = WatchCoverage::default();
        let mut additions: Vec<WorktreeDirectoryPath> = Vec::new();
        let mut known = self.directories.clone();
        for directory in changed {
            let Some(directory) = directory_path(&self.root, directory) else {
                coverage.truncated = true;
                continue;
            };
            let limit = WATCH_DIRECTORIES_MAX.saturating_sub(known.len());
            if limit == 0 {
                coverage.truncated = true;
                break;
            }
            let walk =
                unregistered_directories(&self.root, &directory, self.ignored, &known, limit);
            coverage.truncated |= walk.truncated;
            for directory in walk.directories {
                if known.insert(directory.clone()) {
                    additions.push(directory);
                }
            }
        }
        // A failed batch adds no directory to the set, so the next burst that
        // names the same parent tries again.
        let Ok(refused) = self.add(&additions) else {
            // The platform refused the whole batch, so every directory of this
            // batch stays without a watch.
            coverage.refused = coverage.refused.saturating_add(additions.len());
            return coverage;
        };
        coverage.merge(refused);
        coverage
    }

    /// Applies one batch of watches and records the directories that it covers.
    ///
    /// The call adds every path in one batch and applies the batch once, because
    /// a platform that keeps one event stream stops that stream while the batch
    /// is open and rebuilds it on the apply. An empty batch opens no stream.
    ///
    /// Returns the directories that the platform refused, and whether the watch
    /// limit of the host caused one of those refusals. A directory that
    /// disappeared between the walk and this call names no gap.
    ///
    /// # Errors
    ///
    /// Returns the error of the root, because a root without a watch reports no
    /// change at all, and the error of the applied batch.
    fn add(
        &mut self,
        directories: &[WorktreeDirectoryPath],
    ) -> Result<WatchCoverage, notify::Error> {
        let mut coverage = WatchCoverage::default();
        if directories.is_empty() {
            return Ok(coverage);
        }
        let mut paths = self.watcher.paths_mut();
        let mut recorded = Vec::with_capacity(directories.len());
        for directory in directories {
            if matches!(directory, WorktreeDirectoryPath::Relative(_)) {
                match self.root.resolve_directory(directory) {
                    Ok(resolved)
                        if resolved.state() == ResolvedTargetState::Existing
                            && resolved.path() == directory
                            && !resolved.followed_link() => {}
                    Ok(resolved) if resolved.state() == ResolvedTargetState::Missing => continue,
                    Ok(_)
                    | Err(WorktreeConfinementError::Escape)
                    | Err(WorktreeConfinementError::DanglingLink)
                    | Err(WorktreeConfinementError::LinkLoop)
                    | Err(WorktreeConfinementError::NotDirectory)
                    | Err(WorktreeConfinementError::Replaced)
                    | Err(WorktreeConfinementError::InvalidResolvedPath(_)) => {
                        coverage.truncated = true;
                        continue;
                    }
                    Err(WorktreeConfinementError::Access { source })
                        if source.kind() == io::ErrorKind::NotFound =>
                    {
                        continue;
                    }
                    Err(WorktreeConfinementError::Access { .. }) => {
                        coverage.refused = coverage.refused.saturating_add(1);
                        continue;
                    }
                }
            }
            let absolute = directory.display_path(&self.root);
            if absolute.to_str().is_none() {
                if matches!(directory, WorktreeDirectoryPath::Root) {
                    return Err(notify::Error::io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "the watch root is not UTF-8",
                    )));
                }
                coverage.truncated = true;
                continue;
            }
            let Err(error) = paths.add(&absolute, RecursiveMode::NonRecursive) else {
                recorded.push(directory.clone());
                continue;
            };
            if matches!(directory, WorktreeDirectoryPath::Root) {
                return Err(error);
            }
            // Another program may remove one directory between the walk and
            // this call. The parent of that directory still reports the
            // removal, so that directory names no gap.
            let gap = refusal(&error);
            coverage.merge(gap);
            if !gap.is_complete() {
                recorded.push(directory.clone());
            }
        }
        paths.commit()?;
        self.directories.extend(recorded);
        Ok(coverage)
    }
}

fn directory_path(root: &WorktreeRoot, path: &Path) -> Option<WorktreeDirectoryPath> {
    if path == root.as_path() {
        return Some(WorktreeDirectoryPath::Root);
    }
    let relative = path.strip_prefix(root.as_path()).ok()?;
    WorktreeRelativePath::new(relative)
        .ok()
        .map(WorktreeDirectoryPath::Relative)
}

/// Watches one directory tree and publishes coalesced bursts of its changes.
///
/// The value owns the platform watcher, its callback thread, and one bounded
/// coalescing task. The task reads the tree once, to place its watches, and it
/// reads every directory that one burst names, to place the watches of a
/// directory that appeared after that walk. The service performs no other
/// filesystem read: it names the directories that changed and leaves every read
/// to the caller.
///
/// Every read runs inside that task, so [`FileWatcher::start`] returns before
/// the first watch exists and the consumer draws its first frame at once.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
///
/// use kvim_path::WorktreeRoot;
/// use kvim_runtime::FileWatcher;
///
/// # let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
/// #     .worker_threads(1)
/// #     .enable_all()
/// #     .build()
/// #     .unwrap();
/// tokio_runtime.block_on(async {
///     let current_dir = std::env::current_dir().unwrap();
///     let root = Arc::new(WorktreeRoot::open(current_dir).unwrap());
///     let mut watcher = FileWatcher::start(root, &["target"])
///         .expect("the root is a readable directory");
///
///     if let Some(batch) = watcher.recv().await {
///         for directory in batch.directories() {
///             println!("{} changed", directory.display());
///         }
///     }
///     watcher.shutdown().await;
/// });
/// ```
pub struct FileWatcher {
    batches: mpsc::Receiver<WatchBatch>,
    cancellation: CancellationToken,
    /// The coalescing task, which owns the platform watcher and its watches.
    task: Option<JoinHandle<()>>,
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl fmt::Debug for FileWatcher {
    /// Names the service without its platform watcher, which reports nothing.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileWatcher")
            .finish_non_exhaustive()
    }
}

impl FileWatcher {
    /// Starts one watch over every directory of `root` that stays.
    ///
    /// The call reads no directory and places no watch. It starts the
    /// coalescing task, and that task walks the tree, skips a directory whose
    /// name `ignored` holds, and adds one watch for each directory that stays.
    /// The caller therefore reaches its first frame before the first watch.
    ///
    /// No watch covers the window between this call and the completed
    /// registration. The task publishes one burst of [`WatchFidelity::Dropped`]
    /// as it opens the stream, so the consumer reads the whole workspace again
    /// and a change inside the window reaches it through that read.
    ///
    /// That burst also carries the [`WatchCoverage`] of the registration. A
    /// host that refuses a watch, and a workspace above the bounds of this
    /// module, both leave a part of the tree without a watch. The consumer
    /// reads the gap and its cause from the first published value, and it stays
    /// fully usable in both cases.
    ///
    /// `ignored` names the directory names that produce no event, such as a
    /// build output directory. The table limits the registration and the
    /// events. The platform reads no ignored subtree and holds no watch inside
    /// it. The task adds the watches of a directory that appears after the
    /// walk, so the table also limits every later registration. The filter runs
    /// inside the platform callback as well, before any queue, so an ignored
    /// subtree costs no queue space and no later work.
    ///
    /// # Errors
    ///
    /// Returns [`WatchError::Start`] when the platform builds no watcher at all. A
    /// platform that refuses the registration itself ends the published stream,
    /// so [`FileWatcher::recv`] then returns `None`.
    ///
    /// # Panics
    ///
    /// Panics when no Tokio runtime is running, because the coalescing task
    /// needs one.
    pub fn start(
        root: Arc<WorktreeRoot>,
        ignored: &'static [&'static str],
    ) -> Result<Self, WatchError> {
        let (events, raw) = mpsc::channel(WATCH_EVENT_QUEUE_MAX);
        let dropped = Arc::new(AtomicUsize::new(0));
        let callback_root = Arc::clone(&root);
        let callback_dropped = Arc::clone(&dropped);
        let watcher = RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| {
                enqueue_notify_result(result, &events, &callback_root, ignored, &callback_dropped);
            },
            Config::default(),
        )
        .map_err(WatchError::Start)?;

        let (publisher, batches) = mpsc::channel(WATCH_BATCH_QUEUE_MAX);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(watch(
            raw,
            dropped,
            publisher,
            cancellation.clone(),
            PendingRegistration {
                watcher,
                root,
                ignored,
            },
        ));
        Ok(Self {
            batches,
            cancellation,
            task: Some(task),
        })
    }

    /// Waits for the next coalesced burst.
    ///
    /// The call is cancel safe, so the terminal event loop may use it inside its
    /// own `select` beside the terminal event stream.
    ///
    /// Returns `None` after the coalescing task ended, which happens when the
    /// platform refused the registration and after a shutdown. No further burst
    /// can arrive, so a consumer that reads `None` while it runs must report
    /// that no watcher observes the workspace.
    pub async fn recv(&mut self) -> Option<WatchBatch> {
        self.batches.recv().await
    }

    /// Stops the watch and waits for the coalescing task to finish.
    ///
    /// The operation consumes the value, so no caller can read after it. The
    /// coalescing task owns the platform watcher and drops it as it ends, which
    /// ends the platform callback thread. The call therefore returns only after
    /// no further event can reach any queue.
    ///
    /// A shutdown during the registration waits for that registration, because
    /// the blocking thread holds the platform watcher until it returns. The
    /// registration is bounded, so the wait is bounded as well.
    pub async fn shutdown(mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

/// Places every watch of the workspace and then publishes its bursts.
///
/// The coalescing task performs the registration itself, so
/// [`FileWatcher::start`] returns before the first watch exists and the consumer
/// draws its first frame at once.
///
/// No watch covers the window between the start and the completed registration.
/// The task therefore opens the stream with one burst of
/// [`WatchFidelity::Dropped`], which asks the consumer to read the whole
/// workspace again. The burst follows the registration, so every change after
/// it reaches a watch, and a change inside the window reaches the consumer
/// through that read.
///
/// The opening burst also carries the [`WatchCoverage`] of the registration,
/// so a registration that covers a part of the workspace reaches the consumer
/// with the first published value, before any change of the user. Every later
/// burst carries the coverage of its own batch, so a later registration reports
/// its gap as well.
///
/// A registration that the platform refuses ends the task, which closes the
/// published stream. The consumer then reports that no watcher runs.
async fn watch(
    raw: mpsc::Receiver<RawWatchEvent>,
    dropped: Arc<AtomicUsize>,
    publisher: mpsc::Sender<WatchBatch>,
    cancellation: CancellationToken,
    pending: PendingRegistration,
) {
    let Some((registration, coverage)) = register(&cancellation, pending).await else {
        // The registration dropped the platform watcher, which ends its
        // callback thread. This task then drops the publisher, so the consumer
        // reads the end of the stream after that thread ended.
        return;
    };
    let mut opening = WatchBatch::new(Arc::clone(&registration.root));
    opening.drop_events();
    opening.set_coverage(coverage);
    if publisher.try_send(opening).is_err() {
        // The queue is empty here, so only a gone consumer refuses the burst.
        return;
    }
    coalesce(raw, dropped, publisher, cancellation, registration).await;
}

/// Walks the workspace once and places the watch of every directory that stays.
///
/// The walk and the platform call both block, so they run on a blocking thread.
/// The terminal event loop performs neither, and the caller of
/// [`FileWatcher::start`] waits for neither.
///
/// Returns the registration and the part of the workspace that carries no
/// watch, so the burst that opens the stream names every gap of the first walk.
///
/// Returns `None` when the platform refuses the root, when the blocking thread
/// ends without a value, and after a cancellation. Every path drops the platform
/// watcher, which ends its callback thread.
///
/// The call waits for the blocking thread even after a cancellation, because
/// that thread holds the platform watcher until it returns. A shutdown during
/// the registration therefore still ends the callback thread.
async fn register(
    cancellation: &CancellationToken,
    pending: PendingRegistration,
) -> Option<(Registration, WatchCoverage)> {
    let PendingRegistration {
        watcher,
        root,
        ignored,
    } = pending;
    let started = tokio::task::spawn_blocking(move || Registration::start(watcher, root, ignored));
    let Ok(outcome) = started.await else {
        // The blocking thread ended without a value, which drops the platform
        // watcher, so no event can reach any queue.
        return None;
    };
    let (registration, coverage) = outcome.ok()?;
    if cancellation.is_cancelled() {
        // The caller stopped the watch while the registration ran, so the
        // registration drops here and its watches end with it.
        return None;
    }
    Some((registration, coverage))
}

/// Collects the platform events of one window into one published burst.
///
/// The task owns every event between the platform callback and the consumer, so
/// a consumer that cancels its wait loses no collected event.
///
/// The task also owns the registration, so it adds the watches of a directory
/// that appeared after the start. It adds them before it publishes the burst
/// that named the parent of that directory. The consumer then reads the parent
/// while the new watch already runs, so no change escapes both the read and the
/// watch. The burst carries the coverage of that batch, so a later registration
/// that the host refuses reaches the consumer as the first one does.
async fn coalesce(
    mut raw: mpsc::Receiver<RawWatchEvent>,
    dropped: Arc<AtomicUsize>,
    publisher: mpsc::Sender<WatchBatch>,
    cancellation: CancellationToken,
    mut registration: Registration,
) {
    // A drop that a full publication queue caused belongs to the next burst,
    // because the burst that it displaced never reached the consumer.
    let mut carried = WatchFidelity::Complete;
    loop {
        let first = tokio::select! {
            event = raw.recv() => event,
            () = cancellation.cancelled() => return,
        };
        let Some(first) = first else {
            return;
        };
        let batch_fidelity = carried;
        carried = WatchFidelity::Complete;
        let mut events = Vec::with_capacity(WATCH_BURST_EVENTS_MAX);
        events.push(first);

        let deadline = Instant::now() + WATCH_COALESCE_WINDOW;
        // The bound keeps one window finite even while a build writes without
        // pause, so the consumer always receives its burst.
        for _ in 1..WATCH_BURST_EVENTS_MAX {
            let next = tokio::select! {
                event = timeout_at(deadline, raw.recv()) => event,
                () = cancellation.cancelled() => return,
            };
            match next {
                Ok(Some(event)) => events.push(event),
                // The window closed, or the platform watcher ended.
                Ok(None) | Err(_) => break,
            }
        }
        let queue_dropped = dropped.swap(0, Ordering::Relaxed) > 0;
        let event_root = Arc::clone(&registration.root);
        let Ok(mut batch) = tokio::task::spawn_blocking(move || {
            validate_events(event_root, events, batch_fidelity, queue_dropped)
        })
        .await
        else {
            return;
        };

        let changed = batch.directories();
        if !changed.is_empty() {
            // The walk and the platform call both block, so they run on a
            // blocking thread. The terminal event loop performs neither.
            let Ok((next, coverage)) = tokio::task::spawn_blocking(move || {
                let coverage = registration.extend(&changed);
                (registration, coverage)
            })
            .await
            else {
                // The blocking thread ended without a value, which drops the
                // platform watcher, so no further event can reach this task.
                return;
            };
            registration = next;
            batch.set_coverage(coverage);
        }

        if publisher.try_send(batch).is_err() {
            // The consumer is behind, or gone. A displaced burst loses the
            // directories that it named, so the next burst reports the loss.
            carried = WatchFidelity::Dropped;
            if publisher.is_closed() {
                return;
            }
        }
    }
}

fn validate_events(
    root: Arc<WorktreeRoot>,
    events: Vec<RawWatchEvent>,
    fidelity: WatchFidelity,
    queue_dropped: bool,
) -> WatchBatch {
    let mut batch = WatchBatch {
        root: Some(Arc::clone(&root)),
        fidelity,
        ..WatchBatch::default()
    };
    for event in events {
        match event {
            RawWatchEvent::Event { path, kind } => {
                match WatchEvent::new(Arc::clone(&root), path, kind) {
                    Ok(event) => batch.push(&event),
                    Err(_) => batch.drop_events(),
                }
            }
            RawWatchEvent::Dropped => batch.drop_events(),
        }
    }
    if queue_dropped {
        batch.drop_events();
    }
    batch
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
