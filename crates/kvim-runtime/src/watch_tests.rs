use super::*;

use std::sync::atomic::AtomicU64;

use tokio::sync::Mutex;
use tokio::time::timeout;

/// The generated directory names that a workspace watch ignores.
const IGNORED: [&str; 5] = [".direnv", ".git", "__pycache__", "node_modules", "target"];

/// The time that one test waits for the platform to report one change.
const EVENT_WAIT: Duration = Duration::from_secs(5);

/// The gate that gives one test the platform watcher of the whole process.
///
/// A platform watcher costs one event stream, and `notify` rebuilds that whole
/// stream for every batch of paths. The host serializes those rebuilds, and one
/// of them needs about half a second on macOS. Ten tests that register at the
/// same time therefore pay ten of those latencies one after the other, and the
/// last of them waits longer than [`EVENT_WAIT`]. That is why the watcher tests
/// failed under the default thread count while every one of them passed alone.
///
/// The gate hands the resource to one test at a time, so each registration
/// costs its own latency and [`EVENT_WAIT`] stays as it is. It costs almost no
/// wall-clock time, because the host already performs this work one stream at a
/// time. Every test that builds a platform watcher takes the gate first, so the
/// guard drops after the watcher of that test.
///
/// The gate is a Tokio mutex, because an asynchronous test holds it across an
/// await, and it needs no poison recovery: a test that panics under the gate
/// releases it for the next one.
static PLATFORM_WATCHER: Mutex<()> = Mutex::const_new(());

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
    let _platform_watcher = PLATFORM_WATCHER.blocking_lock();
    let tree = TempTree::new("registration-confinement-skip");
    let outside = TempTree::new("registration-confinement-skip-outside");
    std::os::unix::fs::symlink(&outside.path, tree.path.join("escape"))
        .expect("the temporary directory supports links");
    let watcher = RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
        .expect("the platform builds one watcher");
    let (mut registration, _) =
        Registration::start(watcher, tree.root(), &IGNORED).expect("the platform watches the root");
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

    let _platform_watcher = PLATFORM_WATCHER.blocking_lock();
    let tree = TempTree::new("registration-non-utf8-child");
    let name = OsString::from_vec(vec![0xff]);
    if fs::create_dir(tree.path.join(&name)).is_err() {
        // Some macOS filesystems reject non-UTF-8 names before registration.
        return;
    }
    let watcher = RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
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

    let _platform_watcher = PLATFORM_WATCHER.blocking_lock();
    let parent = TempTree::new("registration-non-utf8-root");
    let path = parent.path.join(OsString::from_vec(vec![0xff]));
    if fs::create_dir(&path).is_err() {
        // Some macOS filesystems reject non-UTF-8 names before registration.
        return;
    }
    let root = Arc::new(WorktreeRoot::open(&path).expect("the root capability opens"));
    let watcher = RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
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
    let _platform_watcher = PLATFORM_WATCHER.blocking_lock();
    let tree = TempTree::new("repeat");
    tree.dir("src");
    let watcher = RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
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
    let _platform_watcher = PLATFORM_WATCHER.blocking_lock();
    let tree = TempTree::new("overlapping-additions");
    tree.dir("src");
    let watcher = RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
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
    let _platform_watcher = PLATFORM_WATCHER.lock().await;
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
    let _platform_watcher = PLATFORM_WATCHER.lock().await;
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
    let _platform_watcher = PLATFORM_WATCHER.lock().await;
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
    let _platform_watcher = PLATFORM_WATCHER.lock().await;
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
    let _platform_watcher = PLATFORM_WATCHER.lock().await;
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
    let _platform_watcher = PLATFORM_WATCHER.blocking_lock();
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
    let _platform_watcher = PLATFORM_WATCHER.blocking_lock();
    let tree = TempTree::new("drop-cancellation");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the test builds one runtime");
    let guard = runtime.enter();
    let watcher =
        FileWatcher::start(tree.root(), &IGNORED).expect("the start accepts the root capability");
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
        fs::write(wide.join(index.to_string()), "").expect("the temporary directory is writable");
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
    let _platform_watcher = PLATFORM_WATCHER.blocking_lock();
    let tree = TempTree::new("disappeared");
    let watcher = RecommendedWatcher::new(|_: notify::Result<notify::Event>| {}, Config::default())
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
    let _platform_watcher = PLATFORM_WATCHER.lock().await;
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
    let _platform_watcher = PLATFORM_WATCHER.lock().await;
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
    let _platform_watcher = PLATFORM_WATCHER.lock().await;
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
