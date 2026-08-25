use std::error::Error as _;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{
    UnsupportedPathComponent, WORKTREE_PATH_BYTES_MAX, WORKTREE_PATH_COMPONENTS_MAX,
    WorktreeConfinementError, WorktreeDirectoryPath, WorktreeRelativePath,
    WorktreeRelativePathError, WorktreeRoot, WorktreeRootError,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kvim-path-test-{label}-{}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("the temporary directory is writable");
        Self(fs::canonicalize(path).expect("the temporary directory exists"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn root_rejects_relative_path() {
    assert!(matches!(
        WorktreeRoot::open("worktree"),
        Err(WorktreeRootError::Relative)
    ));
}

#[test]
fn root_rejects_missing_path_and_preserves_io_source() {
    let directory = TempDir::new("missing-root");
    let missing = directory.path().join("missing");
    let error = WorktreeRoot::open(&missing).expect_err("the root does not exist");

    assert!(error.source().is_some());
    match error {
        WorktreeRootError::Canonicalize { path, source } => {
            assert_eq!(path, missing);
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("expected canonicalization failure, got {other:?}"),
    }
}

#[test]
fn root_rejects_file_and_preserves_capability_io_source() {
    let directory = TempDir::new("file-root");
    let file = directory.path().join("file.txt");
    fs::write(&file, "not a directory").expect("the temporary directory is writable");
    let error = WorktreeRoot::open(&file).expect_err("a worktree root must be a directory");

    assert!(error.source().is_some());
    assert!(matches!(error, WorktreeRootError::OpenCapability { .. }));
}

#[test]
fn root_rejects_path_byte_limit_exhaustion() {
    let path = Path::new("/").join("x".repeat(WORKTREE_PATH_BYTES_MAX));
    assert!(matches!(
        WorktreeRoot::open(path),
        Err(WorktreeRootError::PathBytesLimit { .. })
    ));
}

#[test]
fn root_rejects_component_limit_exhaustion() {
    let mut path = PathBuf::from("/");
    for _ in 0..=WORKTREE_PATH_COMPONENTS_MAX {
        path.push("x");
    }
    assert!(matches!(
        WorktreeRoot::open(path),
        Err(WorktreeRootError::PathComponentsLimit { .. })
    ));
}

#[test]
fn equivalent_root_spellings_have_one_identity() {
    let directory = TempDir::new("root-alias");
    let child = directory.path().join("child");
    fs::create_dir(&child).expect("the child directory is writable");

    let direct = WorktreeRoot::open(directory.path()).expect("the root exists");
    let lexical_alias = WorktreeRoot::open(child.join("..")).expect("the alias exists");

    assert_eq!(direct, lexical_alias);
    assert_eq!(direct.as_path(), directory.path());
}

#[cfg(unix)]
#[test]
fn canonicalizes_macos_style_private_directory_spelling() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new("macos-canonical-spelling");
    let private = directory.path().join("private");
    let canonical_root = private.join("tmp").join("project");
    fs::create_dir_all(&canonical_root).expect("the private directory is writable");
    symlink("private/tmp", directory.path().join("tmp"))
        .expect("the symbolic-link alias is writable");

    let alias = directory.path().join("tmp").join("project");
    let root = WorktreeRoot::open(alias).expect("the alias resolves inside the fixture");

    assert_eq!(root.as_path(), canonical_root);
}

#[cfg(unix)]
#[test]
fn absolute_link_alias_is_accepted_when_its_canonical_target_is_contained() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new("absolute-link-alias");
    let private = directory.path().join("private");
    let canonical_root = private.join("tmp").join("project");
    fs::create_dir_all(&canonical_root).expect("the canonical root is writable");
    fs::write(canonical_root.join("file.rs"), "contained\n")
        .expect("the contained file is writable");
    symlink("private/tmp", directory.path().join("tmp"))
        .expect("the symbolic-link alias is writable");
    symlink(
        directory.path().join("tmp/project/file.rs"),
        canonical_root.join("alias.rs"),
    )
    .expect("the absolute target alias is writable");
    let root = WorktreeRoot::open(&canonical_root).expect("the worktree root exists");

    let resolved = root
        .resolve(&WorktreeRelativePath::new("alias.rs").expect("the path is valid"))
        .expect("the canonical absolute target stays inside the root");

    assert_eq!(resolved.path().as_path(), Path::new("file.rs"));
}

#[cfg(unix)]
#[test]
fn directory_links_can_resolve_to_the_worktree_root() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new("directory-root-alias");
    fs::create_dir(directory.path().join("nested")).expect("the nested directory is writable");
    symlink(".", directory.path().join("alias")).expect("the temporary directory supports links");
    symlink("..", directory.path().join("nested/back"))
        .expect("the temporary directory supports links");
    let root = WorktreeRoot::open(directory.path()).expect("the root exists");

    for requested in ["alias", "nested/back"] {
        let requested = WorktreeRelativePath::new(requested).expect("the path is valid");
        let resolved = root
            .resolve_directory(&WorktreeDirectoryPath::Relative(requested.clone()))
            .expect("the directory link remains contained");
        assert_eq!(resolved.path(), &WorktreeDirectoryPath::Root);
        assert!(resolved.followed_link());
        assert!(matches!(
            root.resolve(&requested),
            Err(WorktreeConfinementError::InvalidResolvedPath(
                WorktreeRelativePathError::Empty
            ))
        ));
    }
}

#[test]
fn root_owns_a_capability_that_opens_a_child() {
    let directory = TempDir::new("capability-open");
    fs::write(directory.path().join("file.txt"), "capability")
        .expect("the temporary directory is writable");
    let root = WorktreeRoot::open(directory.path()).expect("the root exists");

    let mut file = root
        .directory()
        .open("file.txt")
        .expect("the child is below the capability root");
    let mut content = String::new();
    file.read_to_string(&mut content)
        .expect("the child file is readable");

    assert_eq!(content, "capability");
}

#[test]
fn relative_path_accepts_and_normalizes_normal_components() {
    let path = WorktreeRelativePath::new("src//model/lib.rs/")
        .expect("the path has only normal components");
    assert_eq!(path.as_path(), Path::new("src/model/lib.rs"));
}

#[test]
fn relative_path_rejects_empty_path() {
    assert_eq!(
        WorktreeRelativePath::new(""),
        Err(WorktreeRelativePathError::Empty)
    );
}

#[test]
fn relative_path_rejects_absolute_path() {
    assert_eq!(
        WorktreeRelativePath::new("/src/lib.rs"),
        Err(WorktreeRelativePathError::Absolute)
    );
}

#[test]
fn relative_path_rejects_parent_traversal_in_every_position() {
    for path in ["..", "../file", "src/../file", "src/.."] {
        assert_eq!(
            WorktreeRelativePath::new(path),
            Err(WorktreeRelativePathError::ParentTraversal),
            "path: {path}"
        );
    }
}

#[test]
fn relative_path_rejects_current_directory_components() {
    for path in [".", "./file", "src/./file", "src/."] {
        assert_eq!(
            WorktreeRelativePath::new(path),
            Err(WorktreeRelativePathError::UnsupportedComponent {
                component: UnsupportedPathComponent::CurrentDirectory,
            }),
            "path: {path}"
        );
    }
}

#[test]
fn relative_path_rejects_path_byte_limit_exhaustion() {
    let path = "x".repeat(WORKTREE_PATH_BYTES_MAX + 1);
    assert!(matches!(
        WorktreeRelativePath::new(path),
        Err(WorktreeRelativePathError::PathBytesLimit { .. })
    ));
}

#[test]
fn relative_path_rejects_component_limit_exhaustion() {
    let path = std::iter::repeat_n("x", WORKTREE_PATH_COMPONENTS_MAX + 1)
        .collect::<Vec<_>>()
        .join("/");
    assert!(matches!(
        WorktreeRelativePath::new(path),
        Err(WorktreeRelativePathError::PathComponentsLimit { .. })
    ));
}
