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
///     let root = Arc::new(WorktreeRoot::open(std::env::current_dir()).unwrap());
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
mod tests {
    use super::*;

    use std::sync::atomic::AtomicU64;

    use tokio::time::timeout;

    /// The generated directory names that a workspace watch ignores.
    const IGNORED: [&str; 5] = [".direnv", ".git", "__pycache__", "node_modules", "target"];

    /// The time that one test waits for the platform to report one change.
    const EVENT_WAIT: Duration = Duration::from_secs(5);

    /// The counter that keeps two temporary trees of one process apart.
    static TREE_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// One temporary directory tree that disappears with the test.
    ///
    /// The crate holds its own helper, because a watcher test may not depend on
    /// a crate above this layer. See `docs/architecture.md`.
    struct TempTree {
        /// The canonical directory that holds every file of one test.
        path: PathBuf,
    }

    impl TempTree {
        /// Creates one empty tree under the temporary directory of the system.
        ///
        /// The path is always canonical, because a platform event names the
        /// canonical path. A host that reaches its temporary directory through
        /// a link, as macOS does with `/tmp`, would otherwise place every event
        /// outside the watched root.
        fn new(label: &str) -> Self {
            let counter = TREE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kvim-watch-{label}-{}-{counter}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("the temporary directory is writable");
            let path = fs::canonicalize(&path).expect("the new directory exists");
            Self { path }
        }

        /// Creates one directory, and every directory above it, inside the tree.
        fn dir(&self, name: &str) -> PathBuf {
            let path = self.path.join(name);
            fs::create_dir_all(&path).expect("the temporary directory is writable");
            path
        }

        /// Writes one file, and every directory above it, inside the tree.
        fn file(&self, name: &str, content: &str) -> PathBuf {
            let path = self.path.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("the temporary directory is writable");
            }
            fs::write(&path, content).expect("the temporary directory is writable");
            path
        }

        fn root(&self) -> Arc<WorktreeRoot> {
            Arc::new(WorktreeRoot::open(&self.path).expect("the temporary root exists"))
        }

        /// Returns the watched directories below the root, in ascending order.
        ///
        /// The root itself carries the empty name, so a caller reads the whole
        /// registration from one list.
        fn watched(&self) -> Vec<String> {
            self.unregistered(&BTreeSet::new())
        }

        /// Returns one complete walk of the root, with its truncation.
        fn walk(&self, registered: &BTreeSet<WorktreeDirectoryPath>) -> WalkOutcome {
            let root = self.root();
            unregistered_directories(
                &root,
                &WorktreeDirectoryPath::Root,
                &IGNORED,
                registered,
                WATCH_DIRECTORIES_MAX,
            )
        }

        /// Returns the directories that one walk of the root would still add.
        ///
        /// The names are relative to the root, and the root itself carries the
        /// empty name, so a caller reads the whole addition from one list.
        fn unregistered(&self, registered: &BTreeSet<WorktreeDirectoryPath>) -> Vec<String> {
            let root = self.root();
            let mut names: Vec<String> = self
                .walk(registered)
                .directories
                .iter()
                .map(|path| {
                    path.display_path(&root)
                        .strip_prefix(&self.path)
                        .expect("every watched directory lies below the root")
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for TempTree {
        /// Removes the tree, so no test leaves a directory behind.
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Starts one watcher and waits for the burst that opens its stream.
    ///
    /// The registration runs after the start, so the first burst always reports
    /// the window that no watch covered. The wait also proves that every watch
    /// stands before the caller changes one file.
    async fn started(root: &Path) -> FileWatcher {
        let (watcher, _) = opened(root).await;
        watcher
    }

    /// Starts one watcher and returns it with the burst that opens its stream.
    ///
    /// The registration runs after the start, so the first burst always reports
    /// the window that no watch covered and the coverage of that registration.
    /// The wait also proves that every watch stands before the caller changes
    /// one file.
    async fn opened(root: &Path) -> (FileWatcher, WatchBatch) {
        let root = Arc::new(WorktreeRoot::open(root).expect("the test root exists"));
        let mut watcher =
            FileWatcher::start(root, &IGNORED).expect("the start accepts an absolute root");
        let opening = timeout(EVENT_WAIT, watcher.recv())
            .await
            .expect("the registration finishes")
            .expect("the task opens the stream with one burst");
        assert_eq!(
            opening.fidelity(),
            WatchFidelity::Dropped,
            "no watch covered the window between the start and the registration"
        );
        assert!(
            opening.directories().is_empty(),
            "the opening burst names no directory, so the consumer reads every one of them"
        );
        (watcher, opening)
    }

    /// Returns one chain of `levels` directories, each below the one before it.
    fn chain(levels: usize) -> String {
        let mut name = String::from("d0");
        for level in 1..levels {
            name.push_str(&format!("/d{level}"));
        }
        name
    }

    fn event(path: &str, kind: WatchKind) -> WatchEvent {
        let root = Arc::new(
            WorktreeRoot::open(std::env::current_dir().expect("the test process has a directory"))
                .expect("the repository root exists"),
        );
        let relative = path
            .strip_prefix("/work/")
            .expect("the test path uses /work");
        WatchEvent::new(Arc::clone(&root), root.as_path().join(relative), kind)
            .expect("the event lies below its root")
    }

    fn event_directory(path: &str) -> PathBuf {
        let root =
            WorktreeRoot::open(std::env::current_dir().expect("the test process has a directory"))
                .expect("the repository root exists");
        root.as_path()
            .join(path.strip_prefix("/work/").unwrap_or(path))
    }

    #[test]
    fn a_created_entry_asks_for_the_read_of_its_directory() {
        let mut batch = WatchBatch::default();
        batch.push(&event("/work/src/main.rs", WatchKind::Created));
        assert_eq!(batch.directories(), [event_directory("src")]);
        assert!(!batch.changed_content());
    }

    #[test]
    fn a_removed_entry_asks_for_the_read_of_its_directory() {
        let mut batch = WatchBatch::default();
        batch.push(&event("/work/src/main.rs", WatchKind::Removed));
        assert_eq!(batch.directories(), [event_directory("src")]);
    }

    #[test]
    fn a_renamed_entry_asks_for_both_directories_that_it_touched() {
        let mut batch = WatchBatch::default();
        batch.push(&event("/work/src/old.rs", WatchKind::Renamed));
        batch.push(&event("/work/docs/new.rs", WatchKind::Renamed));
        assert_eq!(
            batch.directories(),
            [event_directory("docs"), event_directory("src")]
        );
    }

    #[test]
    fn a_modified_entry_asks_for_no_directory_read() {
        let mut batch = WatchBatch::default();
        batch.push(&event("/work/src/main.rs", WatchKind::Modified));
        assert!(batch.directories().is_empty());
        assert!(batch.changed_content());
    }

    #[test]
    fn an_unnamed_platform_change_still_asks_for_the_directory_read() {
        let mut batch = WatchBatch::default();
        batch.push(&event("/work/src/main.rs", WatchKind::Unknown));
        assert_eq!(batch.directories(), [event_directory("src")]);
    }

    #[test]
    fn one_burst_of_many_events_names_each_directory_once() {
        let mut batch = WatchBatch::default();
        for index in 0..4096 {
            batch.push(&event(
                &format!("/work/src/file{index}.rs"),
                WatchKind::Created,
            ));
        }
        assert_eq!(batch.directories(), [event_directory("src")]);
        assert_eq!(batch.fidelity(), WatchFidelity::Complete);
    }

    #[test]
    fn a_burst_above_the_directory_bound_reports_the_drop() {
        let mut batch = WatchBatch::default();
        for index in 0..=WATCH_BATCH_DIRECTORIES_MAX {
            batch.push(&event(
                &format!("/work/dir{index}/file.rs"),
                WatchKind::Created,
            ));
        }
        assert_eq!(batch.directories().len(), WATCH_BATCH_DIRECTORIES_MAX);
        assert_eq!(batch.fidelity(), WatchFidelity::Dropped);
    }

    #[test]
    fn an_ignored_directory_name_keeps_every_path_below_it_out() {
        let root = Path::new("/work");
        assert!(is_ignored(
            root,
            Path::new("/work/target/debug/kvim"),
            &IGNORED
        ));
        assert!(is_ignored(root, Path::new("/work/.git/index"), &IGNORED));
        assert!(is_ignored(
            root,
            Path::new("/work/src/node_modules/left-pad/index.js"),
            &IGNORED
        ));
        assert!(!is_ignored(root, Path::new("/work/src/main.rs"), &IGNORED));
    }

    #[test]
    fn a_root_that_carries_an_ignored_name_still_reports_its_own_entries() {
        let root = Path::new("/home/reader/target");
        assert!(!is_ignored(
            root,
            Path::new("/home/reader/target/main.rs"),
            &IGNORED
        ));
        assert!(is_ignored(
            root,
            Path::new("/home/reader/target/target/debug"),
            &IGNORED
        ));
    }

    #[test]
    fn a_path_outside_the_watched_root_never_reaches_a_burst() {
        assert!(is_ignored(
            Path::new("/work"),
            Path::new("/other/main.rs"),
            &IGNORED
        ));
    }

    #[test]
    fn a_read_of_one_entry_produces_no_event() {
        use notify::event::AccessKind;

        assert_eq!(classify(EventKind::Access(AccessKind::Read)), None);
    }

    #[test]
    fn an_outside_platform_event_is_rejected_by_its_root() {
        let tree = TempTree::new("outside-event");
        let other = TempTree::new("outside-event-other");
        let error = WatchEvent::new(tree.root(), other.path.join("main.rs"), WatchKind::Created)
            .expect_err("an event of another root is rejected");
        assert!(matches!(error, WatchEventError::OutsideRoot));
    }

    #[test]
    fn one_batch_drops_an_event_from_another_root() {
        let first = TempTree::new("event-root-first");
        let second = TempTree::new("event-root-second");
        first.dir("src");
        second.dir("src");
        let first_event = WatchEvent::new(
            first.root(),
            first.path.join("src/main.rs"),
            WatchKind::Created,
        )
        .expect("the first event lies below its root");
        let second_event = WatchEvent::new(
            second.root(),
            second.path.join("src/main.rs"),
            WatchKind::Created,
        )
        .expect("the second event lies below its root");

        let mut batch = WatchBatch::default();
        batch.push(&first_event);
        batch.push(&second_event);

        assert_eq!(batch.directories(), [first.path.join("src")]);
        assert_eq!(batch.fidelity(), WatchFidelity::Dropped);
        assert_eq!(
            batch.root().map(WorktreeRoot::as_path),
            Some(first.path.as_path())
        );
    }

    #[test]
    fn an_outside_raw_event_publishes_dropped_fidelity() {
        let first = TempTree::new("raw-event-root");
        let second = TempTree::new("raw-event-other");
        let batch = validate_events(
            first.root(),
            vec![RawWatchEvent::Event {
                path: second.path.join("main.rs"),
                kind: WatchKind::Created,
            }],
            WatchFidelity::Complete,
            false,
        );

        assert!(batch.directories().is_empty());
        assert_eq!(batch.fidelity(), WatchFidelity::Dropped);
    }

    #[cfg(unix)]
    #[test]
    fn a_contained_symlink_event_refreshes_its_lexical_parent() {
        let tree = TempTree::new("symlink-event");
        tree.dir("target");
        tree.dir("links");
        std::os::unix::fs::symlink("../target", tree.path.join("links/alias"))
            .expect("the temporary directory supports links");

        let event = WatchEvent::new(
            tree.root(),
            tree.path.join("links/alias"),
            WatchKind::Created,
        )
        .expect("the link target remains contained");
        let mut batch = WatchBatch::default();
        batch.push(&event);

        assert_eq!(event.path, tree.path.join("links/alias"));
        assert_eq!(batch.directories(), [tree.path.join("links")]);
    }

    #[test]
    fn a_callback_error_enqueues_a_dropped_wake_event() {
        let tree = TempTree::new("callback-error");
        let (sender, mut receiver) = mpsc::channel(1);
        let dropped = AtomicUsize::new(0);

        enqueue_notify_result(
            Err(notify::Error::new(ErrorKind::MaxFilesWatch)),
            &sender,
            &tree.root(),
            &IGNORED,
            &dropped,
        );

        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        let event = receiver
            .try_recv()
            .expect("the callback wakes the coalescer");
        let batch = validate_events(
            tree.root(),
            vec![event],
            WatchFidelity::Complete,
            dropped.swap(0, Ordering::Relaxed) > 0,
        );
        assert_eq!(batch.fidelity(), WatchFidelity::Dropped);
    }

    #[test]
    fn the_registration_adds_no_watch_below_an_ignored_name() {
        let tree = TempTree::new("ignored");
        tree.file("src/main.rs", "fn main() {}\n");
        tree.dir("src/tui");
        tree.dir("target/debug/build/kvim");
        tree.dir(".git/objects/ab");
        tree.dir("node_modules/left-pad");
        tree.dir("crates/kvim-runtime/src");

        assert_eq!(
            tree.watched(),
            vec![
                "",
                "crates",
                "crates/kvim-runtime",
                "crates/kvim-runtime/src",
                "src",
                "src/tui",
            ],
            "the root and every kept directory carry one watch, and no ignored name does"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_registration_never_follows_a_directory_link() {
        let tree = TempTree::new("directory-link");
        tree.dir("real/inner");
        std::os::unix::fs::symlink("real", tree.path.join("alias"))
            .expect("the temporary directory supports links");

        let watched = tree.watched();
        assert_eq!(watched, vec!["", "real", "real/inner"]);
        assert!(!watched.iter().any(|path| path.starts_with("alias")));
    }

    #[cfg(unix)]
    #[test]
    fn a_confinement_skipped_directory_remains_unregistered() {
        let tree = TempTree::new("registration-confinement-skip");
        let outside = TempTree::new("registration-confinement-skip-outside");
        std::os::unix::fs::symlink(&outside.path, tree.path.join("escape"))
            .expect("the temporary directory supports links");
        let watcher =
            RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
                .expect("the platform builds one watcher");
        let (mut registration, _) = Registration::start(watcher, tree.root(), &IGNORED)
            .expect("the platform watches the root");
        let escaped = WorktreeDirectoryPath::Relative(
            WorktreeRelativePath::new("escape").expect("the fixture path is valid"),
        );

        let coverage = registration
            .add(std::slice::from_ref(&escaped))
            .expect("a skipped descendant keeps the root registration");

        assert!(coverage.truncated);
        assert!(!registration.directories.contains(&escaped));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_descendant_is_reported_and_remains_retryable() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let tree = TempTree::new("registration-non-utf8-child");
        let name = OsString::from_vec(vec![0xff]);
        if fs::create_dir(tree.path.join(&name)).is_err() {
            // Some macOS filesystems reject non-UTF-8 names before registration.
            return;
        }
        let watcher =
            RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
                .expect("the platform builds one watcher");

        let (registration, coverage) = Registration::start(watcher, tree.root(), &IGNORED)
            .expect("the UTF-8 root remains watchable");

        assert!(coverage.truncated);
        assert_eq!(
            registration.directories,
            [WorktreeDirectoryPath::Root].into()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_root_returns_the_typed_start_failure() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let parent = TempTree::new("registration-non-utf8-root");
        let path = parent.path.join(OsString::from_vec(vec![0xff]));
        if fs::create_dir(&path).is_err() {
            // Some macOS filesystems reject non-UTF-8 names before registration.
            return;
        }
        let root = Arc::new(WorktreeRoot::open(&path).expect("the root capability opens"));
        let watcher =
            RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
                .expect("the platform builds one watcher");

        let Err(error) = Registration::start(watcher, root, &IGNORED) else {
            panic!("notify receives no non-UTF-8 root path");
        };

        assert!(matches!(error, WatchError::Start(_)));
    }

    #[test]
    fn a_root_that_carries_an_ignored_name_still_watches_itself() {
        let tree = TempTree::new("target");
        tree.dir("src");
        tree.dir("target");

        assert_eq!(tree.watched(), vec!["", "src"]);
    }

    #[test]
    fn the_registration_stops_at_the_depth_bound() {
        let tree = TempTree::new("deep");
        tree.dir(&chain(WATCH_DEPTH_MAX + 4));

        let watched = tree.watched();
        let deepest = watched
            .last()
            .expect("the registration always watches the root");
        assert_eq!(
            deepest.split('/').count(),
            WATCH_DEPTH_MAX,
            "the deepest watched directory is `{deepest}`"
        );
        assert_eq!(watched.len(), WATCH_DEPTH_MAX + 1);
    }

    #[test]
    fn a_later_walk_adds_only_the_directories_that_the_registration_misses() {
        let tree = TempTree::new("registered");
        tree.dir("src/tui/render");
        tree.dir("target/debug");
        tree.dir("docs");
        let registered: BTreeSet<WorktreeDirectoryPath> = [
            WorktreeDirectoryPath::Root,
            WorktreeDirectoryPath::Relative(
                WorktreeRelativePath::new("src").expect("the fixture path is valid"),
            ),
            WorktreeDirectoryPath::Relative(
                WorktreeRelativePath::new("src/tui").expect("the fixture path is valid"),
            ),
        ]
        .into_iter()
        .collect();

        assert_eq!(
            tree.unregistered(&registered),
            vec!["docs"],
            "a registered directory reports its own new entries, and an ignored name reports none"
        );
    }

    #[test]
    fn a_walk_that_starts_above_the_root_adds_no_watch() {
        let tree = TempTree::new("above-root");
        let above = tree
            .path
            .parent()
            .expect("the tree lies below the temporary directory");

        assert!(
            directory_path(&tree.root(), above).is_none(),
            "a removed root names the directory above it, which no watch covers"
        );
    }

    #[test]
    fn a_repeated_addition_of_one_directory_changes_no_watch() {
        let tree = TempTree::new("repeat");
        tree.dir("src");
        let watcher =
            RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
                .expect("the platform builds one watcher");
        let (mut registration, coverage) = Registration::start(watcher, tree.root(), &IGNORED)
            .expect("the platform watches a readable root");
        assert!(
            coverage.is_complete(),
            "the platform watches every directory of a small readable tree"
        );
        let registered = registration.directories.clone();
        assert_eq!(
            registered.len(),
            2,
            "the root and `src` carry one watch each"
        );

        let coverage = registration.extend(std::slice::from_ref(&tree.path));

        assert!(
            coverage.is_complete(),
            "the batch stays empty, so it refuses nothing"
        );
        assert_eq!(
            registration.directories, registered,
            "the set holds every watched directory, so the batch stays empty and no stream rebuilds"
        );
    }

    #[test]
    fn overlapping_changed_directories_add_each_new_watch_once() {
        let tree = TempTree::new("overlapping-additions");
        tree.dir("src");
        let watcher =
            RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
                .expect("the platform builds one watcher");
        let (mut registration, coverage) = Registration::start(watcher, tree.root(), &IGNORED)
            .expect("the platform watches a readable root");
        assert!(coverage.is_complete());
        let nested = tree.dir("src/new/inner");
        let new = nested.parent().expect("the nested directory has a parent");
        let src = new.parent().expect("the new directory has a parent");

        let coverage = registration.extend(&[src.to_path_buf(), new.to_path_buf()]);

        assert!(coverage.is_complete());
        assert_eq!(
            registration.directories.len(),
            4,
            "the root, src, new, and inner directories each hold one watch"
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_unreadable_directory_keeps_the_rest_of_the_registration() {
        use std::os::unix::fs::PermissionsExt;

        let tree = TempTree::new("unreadable");
        tree.dir("src");
        let locked = tree.dir("locked/inner");
        let locked = locked
            .parent()
            .expect("the created directory lies below the root")
            .to_path_buf();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))
            .expect("the temporary directory is writable");
        let readable = fs::read_dir(&locked).is_ok();
        let watched = tree.watched();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755))
            .expect("the temporary directory is writable");

        if readable {
            // A process that ignores the mode, such as one that runs as the
            // superuser, proves nothing about an unreadable directory.
            return;
        }
        assert_eq!(
            watched,
            vec!["", "locked", "src"],
            "the unreadable directory keeps its own watch and loses its subtree"
        );
    }

    #[tokio::test]
    async fn a_change_of_one_watched_file_reaches_one_burst() {
        let tree = TempTree::new("events");
        tree.file("src/main.rs", "fn main() {}\n");
        tree.dir("target/debug");
        let mut watcher = started(&tree.path).await;

        // The ignored write reaches no queue, and the kept write does.
        tree.file("target/debug/kvim", "\n");
        tree.file("src/lib.rs", "\n");

        let batch = timeout(EVENT_WAIT, watcher.recv())
            .await
            .expect("the platform reports the change of one watched file")
            .expect("the coalescing task publishes the burst");
        let directories = batch.directories();
        assert!(
            directories.contains(&tree.path.join("src")),
            "the burst names the watched src directory; got {directories:?}"
        );
        let target = tree.path.join("target");
        assert!(
            !directories
                .iter()
                .any(|directory| directory.starts_with(&target)),
            "the ignored target subtree names no changed directory; got {directories:?}"
        );
        watcher.shutdown().await;
    }

    #[tokio::test]
    async fn a_change_of_one_root_entry_reaches_one_burst() {
        let tree = TempTree::new("root-events");
        let mut watcher = started(&tree.path).await;

        tree.file("README.md", "\n");

        let batch = timeout(EVENT_WAIT, watcher.recv())
            .await
            .expect("the platform reports the change of one root entry")
            .expect("the coalescing task publishes the burst");
        let directories = batch.directories();
        assert!(
            directories.contains(&tree.path),
            "the burst names the workspace root; got {directories:?}"
        );
        watcher.shutdown().await;
    }

    #[tokio::test]
    async fn a_directory_that_appears_after_the_walk_reports_a_change_inside_it() {
        let tree = TempTree::new("new-directory-events");
        tree.dir("src");
        let mut watcher = started(&tree.path).await;

        tree.dir("src/tui");
        let created = timeout(EVENT_WAIT, watcher.recv())
            .await
            .expect("the platform reports the new directory")
            .expect("the coalescing task publishes the burst");
        let directories = created.directories();
        assert!(
            directories.contains(&tree.path.join("src")),
            "the new-directory burst names src; got {directories:?}"
        );

        tree.file("src/tui/render.rs", "\n");

        let batch = timeout(EVENT_WAIT, watcher.recv())
            .await
            .expect("the platform reports the change inside the new directory")
            .expect("the coalescing task publishes the burst");
        let directories = batch.directories();
        assert!(
            directories.contains(&tree.path.join("src/tui")),
            "the directory that appeared after the walk carries its own watch; got {directories:?}"
        );
        watcher.shutdown().await;
    }

    #[tokio::test]
    async fn an_ignored_directory_that_appears_after_the_walk_still_carries_no_watch() {
        let tree = TempTree::new("new-ignored");
        let mut watcher = started(&tree.path).await;
        let target = tree.path.join("target");

        // The creation of the ignored directory reaches no queue, so the kept
        // directory produces the burst that adds the later watches.
        tree.dir("target/debug");
        tree.dir("docs");
        let created = timeout(EVENT_WAIT, watcher.recv())
            .await
            .expect("the platform reports the new directory")
            .expect("the coalescing task publishes the burst");
        let directories = created.directories();
        assert!(
            directories.contains(&tree.path),
            "the kept directory creation names the workspace root; got {directories:?}"
        );
        assert!(
            !directories
                .iter()
                .any(|directory| directory.starts_with(&target)),
            "the ignored target subtree names no changed directory; got {directories:?}"
        );

        tree.file("target/debug/kvim", "\n");
        tree.file("docs/files.md", "\n");

        let batch = timeout(EVENT_WAIT, watcher.recv())
            .await
            .expect("the platform reports the change of the kept directory")
            .expect("the coalescing task publishes the burst");
        let directories = batch.directories();
        assert!(
            directories.contains(&tree.path.join("docs")),
            "the kept docs directory carries a watch; got {directories:?}"
        );
        assert!(
            !directories
                .iter()
                .any(|directory| directory.starts_with(&target)),
            "the ignored target subtree carries no watch; got {directories:?}"
        );
        watcher.shutdown().await;
    }

    #[tokio::test]
    async fn a_root_that_does_not_exist_ends_the_published_stream() {
        let tree = TempTree::new("missing-registration");
        let root = tree.root();
        fs::remove_dir_all(&tree.path).expect("the fixture root exists");
        let mut watcher = FileWatcher::start(root, &IGNORED)
            .expect("the start places no watch, so it refuses no readable root");

        let ended = timeout(EVENT_WAIT, watcher.recv())
            .await
            .expect("the refused registration ends the task");

        assert!(
            ended.is_none(),
            "the refused registration closes the stream, and the consumer then reports the loss"
        );
        watcher.shutdown().await;
    }

    #[test]
    fn the_start_places_no_watch_before_the_task_runs() {
        let tree = TempTree::new("deferred");
        tree.dir("src/tui");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("the test builds one runtime");
        let guard = runtime.enter();

        let mut watcher =
            FileWatcher::start(tree.root(), &IGNORED).expect("the start accepts an absolute root");

        // The runtime polled no task yet, so the registration cannot have run.
        // A caller therefore reaches its first frame before the first watch.
        assert!(
            watcher.batches.try_recv().is_err(),
            "the start publishes no burst, because it places no watch"
        );
        drop(guard);
        let opening = runtime
            .block_on(async { timeout(EVENT_WAIT, watcher.recv()).await })
            .expect("the registration finishes")
            .expect("the task opens the stream with one burst");
        assert_eq!(
            opening.fidelity(),
            WatchFidelity::Dropped,
            "the burst reports the window that no watch covered"
        );
        runtime.block_on(watcher.shutdown());
    }

    #[test]
    fn dropping_a_watcher_requests_best_effort_cancellation() {
        let tree = TempTree::new("drop-cancellation");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("the test builds one runtime");
        let guard = runtime.enter();
        let watcher = FileWatcher::start(tree.root(), &IGNORED)
            .expect("the start accepts the root capability");
        let cancellation = watcher.cancellation.clone();

        drop(watcher);

        assert!(cancellation.is_cancelled());
        drop(guard);
        runtime.block_on(async { tokio::task::yield_now().await });
    }

    #[test]
    fn a_complete_walk_reports_no_gap() {
        let tree = TempTree::new("covered");
        tree.dir("src/tui");
        tree.dir("target/debug");
        tree.dir(&chain(WATCH_DEPTH_MAX));

        let walk = tree.walk(&BTreeSet::new());

        assert!(
            !walk.truncated,
            "every kept directory of the tree carries one watch"
        );
    }

    #[test]
    fn a_walk_below_the_depth_bound_reports_the_gap() {
        let tree = TempTree::new("deep-gap");
        tree.dir(&chain(WATCH_DEPTH_MAX + 1));

        let walk = tree.walk(&BTreeSet::new());

        assert_eq!(
            walk.directories.len(),
            WATCH_DEPTH_MAX + 1,
            "the walk keeps the root and every level down to the bound"
        );
        assert!(
            walk.truncated,
            "the depth bound left the deepest directory without a watch"
        );
    }

    #[test]
    fn a_walk_above_the_directory_bound_reports_the_gap() {
        let tree = TempTree::new("wide-gap");
        tree.dir("one");
        tree.dir("two");

        let root = tree.root();
        let walk = unregistered_directories(
            &root,
            &WorktreeDirectoryPath::Root,
            &IGNORED,
            &BTreeSet::new(),
            2,
        );

        assert_eq!(walk.directories.len(), 2, "the walk stops at its limit");
        assert!(
            walk.truncated,
            "the directory bound left one directory without a watch"
        );
    }

    #[test]
    fn a_walk_of_a_directory_above_the_scan_bound_reports_the_gap() {
        let tree = TempTree::new("wide-directory");
        let wide = tree.dir("wide");
        for index in 0..WATCH_DIRECTORY_SCAN_MAX {
            fs::write(wide.join(index.to_string()), "")
                .expect("the temporary directory is writable");
        }

        // One read returns every entry of a directory at exactly the bound, so
        // that directory loses nothing and reports no gap.
        assert!(
            !tree.walk(&BTreeSet::new()).truncated,
            "a directory of exactly the scan bound keeps every entry"
        );

        fs::write(wide.join(WATCH_DIRECTORY_SCAN_MAX.to_string()), "")
            .expect("the temporary directory is writable");
        let walk = tree.walk(&BTreeSet::new());

        assert!(
            walk.truncated,
            "the scan bound left the entries after the bound out of the walk"
        );
        assert_eq!(
            walk.directories.len(),
            2,
            "the walk keeps the root and the wide directory below it"
        );
    }

    #[test]
    fn a_directory_that_disappeared_before_the_batch_reports_no_gap() {
        let tree = TempTree::new("disappeared");
        let watcher =
            RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
                .expect("the platform builds one watcher");
        let (mut registration, coverage) = Registration::start(watcher, tree.root(), &IGNORED)
            .expect("the platform watches a readable root");
        assert!(coverage.is_complete());

        // The walk found this directory, and another program removed it before
        // the batch. The root still reports that removal.
        let gone = WorktreeDirectoryPath::Relative(
            WorktreeRelativePath::new("gone").expect("the fixture path is valid"),
        );
        let coverage = registration
            .add(std::slice::from_ref(&gone))
            .expect("a refused directory below the root keeps the registration");

        assert!(
            coverage.is_complete(),
            "a directory that no longer exists holds no entry to watch"
        );
        assert!(!registration.directories.contains(&gone));
    }

    #[test]
    fn the_watch_limit_of_the_host_names_its_own_cause() {
        let limit = refusal(&notify::Error::new(ErrorKind::MaxFilesWatch));
        assert_eq!(
            limit,
            WatchCoverage {
                refused: 1,
                at_limit: true,
                truncated: false,
            },
            "the host limit is the one refusal that the user raises"
        );

        let other = refusal(&notify::Error::io(io::Error::from(
            io::ErrorKind::PermissionDenied,
        )));
        assert_eq!(
            other,
            WatchCoverage {
                refused: 1,
                at_limit: false,
                truncated: false,
            },
            "another refusal still leaves the directory without a watch"
        );

        assert!(
            refusal(&notify::Error::path_not_found()).is_complete(),
            "a directory that no longer exists names no gap"
        );
        assert!(
            refusal(&notify::Error::io(io::Error::from(io::ErrorKind::NotFound))).is_complete(),
            "the platform names the same removal as one input failure"
        );
    }

    #[tokio::test]
    async fn the_opening_burst_of_a_complete_registration_reports_no_gap() {
        let tree = TempTree::new("covered-burst");
        tree.dir("src/tui");

        let (watcher, opening) = opened(&tree.path).await;

        assert!(
            opening.coverage().is_complete(),
            "every directory of the workspace carries one watch"
        );
        watcher.shutdown().await;
    }

    #[tokio::test]
    async fn the_opening_burst_of_a_truncated_registration_reports_the_gap() {
        let tree = TempTree::new("truncated-burst");
        tree.dir(&chain(WATCH_DEPTH_MAX + 2));

        let (mut watcher, opening) = opened(&tree.path).await;

        assert!(
            opening.coverage().truncated,
            "the depth bound left a part of the workspace without a watch"
        );
        assert_eq!(
            opening.coverage().refused,
            0,
            "the bound of this module is no refusal of the host"
        );

        // The registration reported its gap, and the watch of every covered
        // directory still reports the changes of that directory.
        tree.file("d0/main.rs", "\n");
        let batch = timeout(EVENT_WAIT, watcher.recv())
            .await
            .expect("the platform reports the change of one watched file")
            .expect("the coalescing task publishes the burst");
        let directories = batch.directories();
        assert!(
            directories.contains(&tree.path.join("d0")),
            "the burst names the covered d0 directory; got {directories:?}"
        );
        watcher.shutdown().await;
    }

    #[tokio::test]
    async fn a_shutdown_during_the_registration_ends_the_watch() {
        let tree = TempTree::new("shutdown-early");
        tree.dir("src/tui/render");
        // The start returns before the registration runs, so this shutdown
        // reaches the watcher while that registration is still open.
        let watcher =
            FileWatcher::start(tree.root(), &IGNORED).expect("the start accepts an absolute root");

        timeout(EVENT_WAIT, watcher.shutdown())
            .await
            .expect("the shutdown waits for the registration and then returns");
    }
}
