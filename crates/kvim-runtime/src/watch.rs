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

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use notify::event::{EventKind, ModifyKind};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;

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
    /// The absolute path that changed.
    pub path: PathBuf,
    /// The effect of the change.
    pub kind: WatchKind,
}

/// Whether one burst holds every event of its window.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WatchFidelity {
    /// The burst holds every event that the window produced.
    #[default]
    Complete,
    /// A bound dropped at least one event of the window.
    ///
    /// The named directories are incomplete, so a consumer that needs the
    /// complete state must read every directory that it holds.
    Dropped,
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
/// use std::path::PathBuf;
///
/// use kvim_runtime::{WatchBatch, WatchEvent, WatchFidelity, WatchKind};
///
/// let mut batch = WatchBatch::default();
/// batch.push(&WatchEvent {
///     path: PathBuf::from("/work/src/main.rs"),
///     kind: WatchKind::Created,
/// });
/// // A second change of the same directory adds no second read.
/// batch.push(&WatchEvent {
///     path: PathBuf::from("/work/src/lib.rs"),
///     kind: WatchKind::Created,
/// });
///
/// assert_eq!(batch.directories(), [PathBuf::from("/work/src")]);
/// assert_eq!(batch.fidelity(), WatchFidelity::Complete);
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WatchBatch {
    directories: BTreeSet<PathBuf>,
    content_changed: bool,
    fidelity: WatchFidelity,
}

impl WatchBatch {
    /// Adds one event to the burst.
    ///
    /// A change that keeps the entries of its directory records the content
    /// change alone, so a write inside one file never asks for a directory read.
    /// A burst above [`WATCH_BATCH_DIRECTORIES_MAX`] directories keeps the
    /// directories that it already holds and reports
    /// [`WatchFidelity::Dropped`].
    pub fn push(&mut self, event: &WatchEvent) {
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

    /// Returns the directories whose listing may have changed, in path order.
    #[must_use]
    pub fn directories(&self) -> Vec<PathBuf> {
        self.directories.iter().cloned().collect()
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
    /// The root is not an absolute path, so no relative event could be placed.
    #[error("the watched root must be an absolute path")]
    RelativeRoot,
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

/// Watches one directory tree and publishes coalesced bursts of its changes.
///
/// The value owns the platform watcher, its callback thread, and one bounded
/// coalescing task. It performs no filesystem read of its own: it names the
/// directories that changed and leaves every read to the caller.
///
/// # Examples
///
/// ```no_run
/// use std::path::PathBuf;
///
/// use kvim_runtime::FileWatcher;
///
/// # let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
/// #     .worker_threads(1)
/// #     .enable_all()
/// #     .build()
/// #     .unwrap();
/// tokio_runtime.block_on(async {
///     let mut watcher = FileWatcher::start(PathBuf::from("/work"), &["target"])
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
    task: JoinHandle<()>,
    /// The platform watcher. Dropping it ends its callback thread.
    watcher: RecommendedWatcher,
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
    /// Starts one recursive watch over `root`.
    ///
    /// `ignored` names the directory names that produce no event, such as a
    /// build output directory. The filter runs inside the platform callback,
    /// before any queue, so an ignored subtree costs no queue space and no
    /// later work.
    ///
    /// # Errors
    ///
    /// Returns [`WatchError::RelativeRoot`] for a root that is not absolute, and
    /// [`WatchError::Start`] when the platform refuses the watch.
    ///
    /// # Panics
    ///
    /// Panics when no Tokio runtime is running, because the coalescing task
    /// needs one.
    pub fn start(root: PathBuf, ignored: &'static [&'static str]) -> Result<Self, WatchError> {
        if !root.is_absolute() {
            return Err(WatchError::RelativeRoot);
        }
        let (events, raw) = mpsc::channel(WATCH_EVENT_QUEUE_MAX);
        let dropped = Arc::new(AtomicUsize::new(0));
        let callback_root = root.clone();
        let callback_dropped = Arc::clone(&dropped);
        let mut watcher = RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| {
                let Ok(event) = result else {
                    // A failed platform read loses changes that the watch never
                    // reports, so the next burst names the loss.
                    callback_dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                };
                let Some(kind) = classify(event.kind) else {
                    return;
                };
                for path in event.paths {
                    if is_ignored(&callback_root, &path, ignored) {
                        continue;
                    }
                    // The send never waits, because the callback thread of the
                    // platform must return at once.
                    if events.try_send(WatchEvent { path, kind }).is_err() {
                        callback_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            Config::default(),
        )
        .map_err(WatchError::Start)?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(WatchError::Start)?;

        let (publisher, batches) = mpsc::channel(WATCH_BATCH_QUEUE_MAX);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(coalesce(raw, dropped, publisher, cancellation.clone()));
        Ok(Self {
            batches,
            cancellation,
            task,
            watcher,
        })
    }

    /// Waits for the next coalesced burst.
    ///
    /// The call is cancel safe, so the terminal event loop may use it inside its
    /// own `select` beside the terminal event stream. Returns `None` after the
    /// coalescing task ended.
    pub async fn recv(&mut self) -> Option<WatchBatch> {
        self.batches.recv().await
    }

    /// Stops the watch and waits for the coalescing task to finish.
    ///
    /// The operation consumes the value, so no caller can read after it.
    pub async fn shutdown(self) {
        let Self {
            cancellation,
            task,
            watcher,
            ..
        } = self;
        // The platform watcher ends its callback thread on drop, so no further
        // event reaches the queue while the task finishes.
        drop(watcher);
        cancellation.cancel();
        let _ = task.await;
    }
}

/// Collects the platform events of one window into one published burst.
///
/// The task owns every event between the platform callback and the consumer, so
/// a consumer that cancels its wait loses no collected event.
async fn coalesce(
    mut raw: mpsc::Receiver<WatchEvent>,
    dropped: Arc<AtomicUsize>,
    publisher: mpsc::Sender<WatchBatch>,
    cancellation: CancellationToken,
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
        let mut batch = WatchBatch {
            fidelity: carried,
            ..WatchBatch::default()
        };
        carried = WatchFidelity::Complete;
        batch.push(&first);

        let deadline = Instant::now() + WATCH_COALESCE_WINDOW;
        // The bound keeps one window finite even while a build writes without
        // pause, so the consumer always receives its burst.
        for _ in 1..WATCH_BURST_EVENTS_MAX {
            let next = tokio::select! {
                event = timeout_at(deadline, raw.recv()) => event,
                () = cancellation.cancelled() => return,
            };
            match next {
                Ok(Some(event)) => batch.push(&event),
                // The window closed, or the platform watcher ended.
                Ok(None) | Err(_) => break,
            }
        }
        if dropped.swap(0, Ordering::Relaxed) > 0 {
            batch.drop_events();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated directory names that a workspace watch ignores.
    const IGNORED: [&str; 5] = [".direnv", ".git", "__pycache__", "node_modules", "target"];

    fn event(path: &str, kind: WatchKind) -> WatchEvent {
        WatchEvent {
            path: PathBuf::from(path),
            kind,
        }
    }

    #[test]
    fn a_created_entry_asks_for_the_read_of_its_directory() {
        let mut batch = WatchBatch::default();
        batch.push(&event("/work/src/main.rs", WatchKind::Created));
        assert_eq!(batch.directories(), [PathBuf::from("/work/src")]);
        assert!(!batch.changed_content());
    }

    #[test]
    fn a_removed_entry_asks_for_the_read_of_its_directory() {
        let mut batch = WatchBatch::default();
        batch.push(&event("/work/src/main.rs", WatchKind::Removed));
        assert_eq!(batch.directories(), [PathBuf::from("/work/src")]);
    }

    #[test]
    fn a_renamed_entry_asks_for_both_directories_that_it_touched() {
        let mut batch = WatchBatch::default();
        batch.push(&event("/work/src/old.rs", WatchKind::Renamed));
        batch.push(&event("/work/docs/new.rs", WatchKind::Renamed));
        assert_eq!(
            batch.directories(),
            [PathBuf::from("/work/docs"), PathBuf::from("/work/src")]
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
        assert_eq!(batch.directories(), [PathBuf::from("/work/src")]);
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
        assert_eq!(batch.directories(), [PathBuf::from("/work/src")]);
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
    fn a_relative_root_starts_no_watcher() {
        let error = FileWatcher::start(PathBuf::from("relative/root"), &IGNORED)
            .expect_err("a relative root places no event");
        assert!(matches!(error, WatchError::RelativeRoot));
    }

    #[tokio::test]
    async fn a_root_that_does_not_exist_starts_no_watcher() {
        let root = PathBuf::from("/kvim-watch-root-that-never-exists");
        let error =
            FileWatcher::start(root, &IGNORED).expect_err("the platform refuses a missing root");
        assert!(matches!(error, WatchError::Start(_)));
    }
}
