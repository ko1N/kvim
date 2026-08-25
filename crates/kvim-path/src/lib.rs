//! Canonical worktree roots and validated worktree-relative paths.
//!
//! [`WorktreeRoot`] owns a `cap-std` directory. File access through that
//! directory stays relative to one canonical root instead of using ambient
//! process paths. [`WorktreeRelativePath`] accepts only non-empty normal path
//! components.
//!
//! `crates/kvim-path/examples/confine_worktree_paths.rs` is the dedicated
//! example of this feature. It opens one root, resolves a present and an absent
//! target, re-checks an identity before a write, and refuses an escape:
//!
//! ```sh
//! cargo run -p kvim-path --example confine_worktree_paths
//! ```
//!
//! # Examples
//!
//! ```no_run
//! use kvim_path::{WorktreeRelativePath, WorktreeRoot};
//!
//! let root = WorktreeRoot::open("/absolute/path/to/worktree")?;
//! let source = WorktreeRelativePath::new("src/lib.rs")?;
//! let _file = root.directory().open(source.as_path())?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

// The crate is one supported external package. Every published item names
// its own contract, so no implementation API can reach a consumer by accident.
#![deny(missing_docs)]

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Component, Path, PathBuf};

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use thiserror::Error;

/// The maximum encoded bytes in a supplied or canonical worktree path.
pub const WORKTREE_PATH_BYTES_MAX: usize = 4096;

/// The maximum non-root components in a worktree path.
pub const WORKTREE_PATH_COMPONENTS_MAX: usize = 256;

/// The maximum number of symbolic links resolved for one target.
pub const WORKTREE_SYMLINKS_MAX: usize = 40;

/// An owned canonical worktree identity and its filesystem capability.
///
/// Construction requires an existing absolute directory. It canonicalizes the
/// path before it opens the capability, so symbolic-link aliases and lexical
/// aliases compare as one identity.
pub struct WorktreeRoot {
    path: PathBuf,
    directory: Dir,
}

impl WorktreeRoot {
    /// Opens an existing absolute directory as one worktree root.
    ///
    /// The constructor rejects relative paths and paths above the public byte
    /// or component limits. It preserves operating-system failures from both
    /// canonicalization and capability opening.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::fs;
    ///
    /// use kvim_path::WorktreeRoot;
    ///
    /// let path = std::env::temp_dir().join(format!("kvim-doc-open-{}", std::process::id()));
    /// fs::create_dir_all(&path)?;
    ///
    /// let root = WorktreeRoot::open(&path)?;
    /// assert!(root.as_path().is_absolute());
    ///
    /// // A relative root names no single directory of the host.
    /// assert!(WorktreeRoot::open("worktree").is_err());
    ///
    /// fs::remove_dir_all(&path)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WorktreeRootError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(WorktreeRootError::Relative);
        }

        let bytes = path.as_os_str().as_encoded_bytes().len();
        if bytes > WORKTREE_PATH_BYTES_MAX {
            return Err(WorktreeRootError::PathBytesLimit {
                actual: bytes,
                max: WORKTREE_PATH_BYTES_MAX,
            });
        }

        let components = non_root_component_count(path);
        if components > WORKTREE_PATH_COMPONENTS_MAX {
            return Err(WorktreeRootError::PathComponentsLimit {
                actual: components,
                max: WORKTREE_PATH_COMPONENTS_MAX,
            });
        }

        let path = fs::canonicalize(path).map_err(|source| WorktreeRootError::Canonicalize {
            path: path.to_path_buf(),
            source,
        })?;

        let bytes = path.as_os_str().as_encoded_bytes().len();
        if bytes > WORKTREE_PATH_BYTES_MAX {
            return Err(WorktreeRootError::CanonicalPathBytesLimit {
                actual: bytes,
                max: WORKTREE_PATH_BYTES_MAX,
            });
        }

        let components = non_root_component_count(&path);
        if components > WORKTREE_PATH_COMPONENTS_MAX {
            return Err(WorktreeRootError::CanonicalPathComponentsLimit {
                actual: components,
                max: WORKTREE_PATH_COMPONENTS_MAX,
            });
        }

        debug_assert!(
            path.is_absolute(),
            "std::fs::canonicalize returns an absolute path"
        );
        let directory = Dir::open_ambient_dir(&path, ambient_authority()).map_err(|source| {
            WorktreeRootError::OpenCapability {
                path: path.clone(),
                source,
            }
        })?;

        Ok(Self { path, directory })
    }

    /// Returns the canonical absolute path that identifies this worktree.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Returns the directory capability rooted at this worktree.
    #[must_use]
    pub fn directory(&self) -> &Dir {
        &self.directory
    }

    /// Resolves one target without permitting access outside this root.
    ///
    /// Existing targets return their canonical worktree-relative identity. A
    /// missing target returns the same identity after resolving its nearest
    /// existing parent. Symbolic links can point only to contained targets.
    ///
    /// # Errors
    ///
    /// Returns a typed confinement failure for an escaping, dangling, looping,
    /// inaccessible, or structurally invalid target.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::fs;
    ///
    /// use kvim_path::{ResolvedTargetState, WorktreeRelativePath, WorktreeRoot};
    ///
    /// let path = std::env::temp_dir().join(format!("kvim-doc-resolve-{}", std::process::id()));
    /// fs::create_dir_all(path.join("src"))?;
    /// fs::write(path.join("src").join("lib.rs"), "// one file\n")?;
    /// let root = WorktreeRoot::open(&path)?;
    ///
    /// let present = root.resolve(&WorktreeRelativePath::new("src/lib.rs")?)?;
    /// assert_eq!(present.state(), ResolvedTargetState::Existing);
    ///
    /// // A target that no file holds yet still resolves, so a save can create it.
    /// let absent = root.resolve(&WorktreeRelativePath::new("src/new.rs")?)?;
    /// assert_eq!(absent.state(), ResolvedTargetState::Missing);
    ///
    /// fs::remove_dir_all(&path)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn resolve(
        &self,
        requested: &WorktreeRelativePath,
    ) -> Result<ResolvedWorktreePath, WorktreeConfinementError> {
        let pending = requested
            .as_path()
            .components()
            .filter_map(|component| match component {
                Component::Normal(component) => Some(ResolutionComponent::Normal {
                    value: component.to_os_string(),
                    from_link_target: false,
                }),
                _ => None,
            })
            .collect();
        let resolved = self.resolve_components(pending)?;
        Ok(ResolvedWorktreePath {
            path: validated_resolved_path(resolved.path)?,
            state: resolved.state,
            links: resolved.links,
        })
    }

    /// Resolves one root-or-relative target for directory traversal.
    ///
    /// Unlike [`WorktreeRoot::resolve`], this operation can represent a
    /// contained link whose target is the worktree root. File operations keep
    /// using [`WorktreeRoot::resolve`] and therefore still reject an empty file
    /// identity.
    pub fn resolve_directory(
        &self,
        requested: &WorktreeDirectoryPath,
    ) -> Result<ResolvedWorktreeDirectory, WorktreeConfinementError> {
        let WorktreeDirectoryPath::Relative(requested) = requested else {
            return Ok(ResolvedWorktreeDirectory {
                path: WorktreeDirectoryPath::Root,
                state: ResolvedTargetState::Existing,
                links: Vec::new(),
            });
        };
        let pending = requested
            .as_path()
            .components()
            .filter_map(|component| match component {
                Component::Normal(component) => Some(ResolutionComponent::Normal {
                    value: component.to_os_string(),
                    from_link_target: false,
                }),
                _ => None,
            })
            .collect();
        let resolved = self.resolve_components(pending)?;
        let path = if resolved.path.as_os_str().is_empty() {
            WorktreeDirectoryPath::Root
        } else {
            WorktreeDirectoryPath::Relative(validated_resolved_path(resolved.path)?)
        };
        Ok(ResolvedWorktreeDirectory {
            path,
            state: resolved.state,
            links: resolved.links,
        })
    }

    /// Confirms that a requested path still has the same resolved identity.
    ///
    /// This check includes every symbolic link in the resolution chain. A link
    /// replacement cannot publish a load or reach a save commit unnoticed.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeConfinementError::Replaced`] when the resolved identity
    /// changed. Other typed resolution failures remain intact.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::fs;
    ///
    /// use kvim_path::{WorktreeConfinementError, WorktreeRelativePath, WorktreeRoot};
    ///
    /// let path = std::env::temp_dir().join(format!("kvim-doc-revalidate-{}", std::process::id()));
    /// fs::create_dir_all(&path)?;
    /// fs::write(path.join("notes.md"), "one line\n")?;
    ///
    /// let root = WorktreeRoot::open(&path)?;
    /// let target = WorktreeRelativePath::new("notes.md")?;
    /// let opened = root.resolve(&target)?;
    /// root.revalidate(&target, &opened)?;
    ///
    /// // A save must not commit through an identity that changed under it.
    /// let missing = root.resolve(&WorktreeRelativePath::new("absent.md")?)?;
    /// assert!(matches!(
    ///     root.revalidate(&target, &missing),
    ///     Err(WorktreeConfinementError::Replaced)
    /// ));
    ///
    /// fs::remove_dir_all(&path)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn revalidate(
        &self,
        requested: &WorktreeRelativePath,
        expected: &ResolvedWorktreePath,
    ) -> Result<(), WorktreeConfinementError> {
        let current = self.resolve(requested)?;
        if &current == expected {
            Ok(())
        } else {
            Err(WorktreeConfinementError::Replaced)
        }
    }

    /// Confirms that a directory target still has the same resolved identity.
    pub fn revalidate_directory(
        &self,
        requested: &WorktreeDirectoryPath,
        expected: &ResolvedWorktreeDirectory,
    ) -> Result<(), WorktreeConfinementError> {
        let current = self.resolve_directory(requested)?;
        if &current == expected {
            Ok(())
        } else {
            Err(WorktreeConfinementError::Replaced)
        }
    }

    fn resolve_components(
        &self,
        mut pending: VecDeque<ResolutionComponent>,
    ) -> Result<RawResolvedWorktreePath, WorktreeConfinementError> {
        let mut resolved = PathBuf::new();
        let mut links = Vec::new();
        while let Some(component) = pending.pop_front() {
            match component {
                ResolutionComponent::Parent => {
                    if !resolved.pop() {
                        return Err(WorktreeConfinementError::Escape);
                    }
                }
                ResolutionComponent::Normal {
                    value: component,
                    from_link_target,
                } => {
                    let candidate = resolved.join(&component);
                    let metadata = match self.directory.symlink_metadata(&candidate) {
                        Ok(metadata) => metadata,
                        Err(source) if source.kind() == io::ErrorKind::NotFound => {
                            if from_link_target {
                                return Err(WorktreeConfinementError::DanglingLink);
                            }
                            resolved.push(component);
                            append_missing_components(&mut resolved, pending)?;
                            return Ok(RawResolvedWorktreePath {
                                path: resolved,
                                state: ResolvedTargetState::Missing,
                                links,
                            });
                        }
                        Err(source) if is_symlink_loop(&source) => {
                            return Err(WorktreeConfinementError::LinkLoop);
                        }
                        Err(source) => {
                            return Err(WorktreeConfinementError::Access { source });
                        }
                    };

                    if metadata.file_type().is_symlink() {
                        if links.len() >= WORKTREE_SYMLINKS_MAX {
                            return Err(WorktreeConfinementError::LinkLoop);
                        }
                        let target =
                            self.directory
                                .read_link_contents(&candidate)
                                .map_err(|source| {
                                    if source.kind() == io::ErrorKind::NotFound {
                                        WorktreeConfinementError::DanglingLink
                                    } else if is_symlink_loop(&source) {
                                        WorktreeConfinementError::LinkLoop
                                    } else {
                                        WorktreeConfinementError::Access { source }
                                    }
                                })?;
                        links.push(LinkIdentity::new(candidate, target.clone(), &metadata));
                        prepend_link_target(self, &mut resolved, &mut pending, &target)?;
                        continue;
                    }

                    if !pending.is_empty() && !metadata.is_dir() {
                        return Err(WorktreeConfinementError::NotDirectory);
                    }
                    resolved.push(component);
                }
            }
        }

        Ok(RawResolvedWorktreePath {
            path: resolved,
            state: ResolvedTargetState::Existing,
            links,
        })
    }
}

struct RawResolvedWorktreePath {
    path: PathBuf,
    state: ResolvedTargetState,
    links: Vec<LinkIdentity>,
}

impl fmt::Debug for WorktreeRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorktreeRoot")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl PartialEq for WorktreeRoot {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for WorktreeRoot {}

impl Hash for WorktreeRoot {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.path.hash(state);
    }
}

/// The resolved state of one worktree target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedTargetState {
    /// The complete target exists.
    Existing,
    /// The target is absent below its nearest existing parent.
    Missing,
}

/// One descriptor-relative resolution below a worktree root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedWorktreePath {
    path: WorktreeRelativePath,
    state: ResolvedTargetState,
    links: Vec<LinkIdentity>,
}

impl ResolvedWorktreePath {
    /// Returns the canonical worktree-relative target identity.
    #[must_use]
    pub const fn path(&self) -> &WorktreeRelativePath {
        &self.path
    }

    /// Returns whether the complete target exists.
    #[must_use]
    pub const fn state(&self) -> ResolvedTargetState {
        self.state
    }

    /// Reports whether resolution followed a symbolic link.
    #[must_use]
    pub fn followed_link(&self) -> bool {
        !self.links.is_empty()
    }
}

/// One resolved root-or-relative identity used by directory traversal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedWorktreeDirectory {
    path: WorktreeDirectoryPath,
    state: ResolvedTargetState,
    links: Vec<LinkIdentity>,
}

impl ResolvedWorktreeDirectory {
    /// Returns the resolved root-or-relative identity.
    #[must_use]
    pub const fn path(&self) -> &WorktreeDirectoryPath {
        &self.path
    }

    /// Returns whether the complete target exists.
    #[must_use]
    pub const fn state(&self) -> ResolvedTargetState {
        self.state
    }

    /// Reports whether resolution followed a symbolic link.
    #[must_use]
    pub fn followed_link(&self) -> bool {
        !self.links.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LinkIdentity {
    path: PathBuf,
    target: PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl LinkIdentity {
    fn new(path: PathBuf, target: PathBuf, metadata: &cap_std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use cap_std::fs::MetadataExt as _;

        Self {
            path,
            target,
            len: metadata.len(),
            modified: metadata.modified().ok().map(|time| time.into_std()),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

/// A failure to resolve a target below a [`WorktreeRoot`].
#[derive(Debug, Error)]
pub enum WorktreeConfinementError {
    /// A symbolic link or parent component leaves the capability root.
    #[error("the target escapes the worktree root")]
    Escape,
    /// A symbolic link has no existing target.
    #[error("the target contains a dangling symbolic link")]
    DanglingLink,
    /// Symbolic-link resolution loops or exceeds [`WORKTREE_SYMLINKS_MAX`].
    #[error("the target contains a symbolic-link loop")]
    LinkLoop,
    /// An existing non-directory occurs before the final component.
    #[error("a target parent is not a directory")]
    NotDirectory,
    /// A path changed after its first validated resolution.
    #[error("the target was replaced during the operation")]
    Replaced,
    /// The operating system refused a descriptor-relative access.
    #[error("the target could not be accessed through the worktree capability")]
    Access {
        /// The operating-system failure.
        #[source]
        source: io::Error,
    },
    /// A resolved symbolic-link target exceeds the public path limits.
    #[error("the resolved target is not a valid worktree-relative path")]
    InvalidResolvedPath(#[source] WorktreeRelativePathError),
}

/// A failure to construct a [`WorktreeRoot`].
#[derive(Debug, Error)]
pub enum WorktreeRootError {
    /// The supplied root is not absolute.
    #[error("the worktree root must be absolute")]
    Relative,
    /// The supplied root exceeds [`WORKTREE_PATH_BYTES_MAX`].
    #[error("the worktree root has {actual} bytes, above the limit of {max}")]
    PathBytesLimit {
        /// The supplied encoded byte count.
        actual: usize,
        /// The accepted encoded byte count.
        max: usize,
    },
    /// The supplied root exceeds [`WORKTREE_PATH_COMPONENTS_MAX`].
    #[error("the worktree root has {actual} components, above the limit of {max}")]
    PathComponentsLimit {
        /// The supplied component count.
        actual: usize,
        /// The accepted component count.
        max: usize,
    },
    /// The root could not be resolved to an existing canonical path.
    #[error("failed to canonicalize worktree root {path:?}")]
    Canonicalize {
        /// The supplied absolute root.
        path: PathBuf,
        /// The operating-system failure.
        #[source]
        source: io::Error,
    },
    /// The canonical root exceeds [`WORKTREE_PATH_BYTES_MAX`].
    #[error("the canonical worktree root has {actual} bytes, above the limit of {max}")]
    CanonicalPathBytesLimit {
        /// The canonical encoded byte count.
        actual: usize,
        /// The accepted encoded byte count.
        max: usize,
    },
    /// The canonical root exceeds [`WORKTREE_PATH_COMPONENTS_MAX`].
    #[error("the canonical worktree root has {actual} components, above the limit of {max}")]
    CanonicalPathComponentsLimit {
        /// The canonical component count.
        actual: usize,
        /// The accepted component count.
        max: usize,
    },
    /// The canonical directory could not be opened as a capability.
    #[error("failed to open capability directory for worktree root {path:?}")]
    OpenCapability {
        /// The canonical absolute root.
        path: PathBuf,
        /// The operating-system failure.
        #[source]
        source: io::Error,
    },
}

/// An owned non-empty path relative to one worktree.
///
/// The value contains normal components only. It cannot contain a root,
/// platform prefix, current-directory component, or parent traversal.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorktreeRelativePath(PathBuf);

impl WorktreeRelativePath {
    /// Validates and owns one path below a worktree root.
    ///
    /// Repeated separators are normalized. All other non-normal components are
    /// rejected rather than resolved against ambient process state.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use kvim_path::WorktreeRelativePath;
    ///
    /// let source = WorktreeRelativePath::new("src//lib.rs")?;
    /// assert_eq!(source.as_path(), Path::new("src/lib.rs"));
    ///
    /// // The type carries the confinement invariant, so no caller re-checks it.
    /// assert!(WorktreeRelativePath::new("../outside").is_err());
    /// assert!(WorktreeRelativePath::new("").is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(path: impl AsRef<Path>) -> Result<Self, WorktreeRelativePathError> {
        let path = path.as_ref();
        if path.is_absolute() {
            return Err(WorktreeRelativePathError::Absolute);
        }

        let bytes = path.as_os_str().as_encoded_bytes();
        if bytes.is_empty() {
            return Err(WorktreeRelativePathError::Empty);
        }
        if bytes.len() > WORKTREE_PATH_BYTES_MAX {
            return Err(WorktreeRelativePathError::PathBytesLimit {
                actual: bytes.len(),
                max: WORKTREE_PATH_BYTES_MAX,
            });
        }
        if bytes
            .split(|byte| *byte == std::path::MAIN_SEPARATOR as u8)
            .any(|component| component == b".")
        {
            return Err(WorktreeRelativePathError::UnsupportedComponent {
                component: UnsupportedPathComponent::CurrentDirectory,
            });
        }

        let mut normalized = PathBuf::new();
        let mut count = 0_usize;
        for component in path.components() {
            match component {
                Component::Normal(component) => {
                    count = count
                        .checked_add(1)
                        .expect("a path cannot hold more components than addressable memory");
                    if count > WORKTREE_PATH_COMPONENTS_MAX {
                        return Err(WorktreeRelativePathError::PathComponentsLimit {
                            actual: count,
                            max: WORKTREE_PATH_COMPONENTS_MAX,
                        });
                    }
                    normalized.push(component);
                }
                Component::ParentDir => {
                    return Err(WorktreeRelativePathError::ParentTraversal);
                }
                Component::CurDir => {
                    return Err(WorktreeRelativePathError::UnsupportedComponent {
                        component: UnsupportedPathComponent::CurrentDirectory,
                    });
                }
                Component::RootDir => {
                    return Err(WorktreeRelativePathError::UnsupportedComponent {
                        component: UnsupportedPathComponent::RootDirectory,
                    });
                }
                Component::Prefix(_) => {
                    return Err(WorktreeRelativePathError::UnsupportedComponent {
                        component: UnsupportedPathComponent::PlatformPrefix,
                    });
                }
            }
        }

        if normalized.as_os_str().is_empty() {
            return Err(WorktreeRelativePathError::Empty);
        }
        debug_assert!(
            normalized
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "the constructor appends only validated normal components"
        );

        Ok(Self(normalized))
    }

    /// Returns the validated relative path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for WorktreeRelativePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

/// One directory target at or below a worktree root.
///
/// [`WorktreeRelativePath`] is intentionally non-empty, so the root directory
/// needs its own variant. Every descendant still carries a validated relative
/// path.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_path::{WorktreeDirectoryPath, WorktreeRelativePath};
///
/// let root = WorktreeDirectoryPath::Root;
/// assert_eq!(root.relative_path(), None);
/// assert_eq!(root.capability_path(), Path::new("."));
///
/// let nested = WorktreeDirectoryPath::Relative(WorktreeRelativePath::new("src/ui")?);
/// assert_eq!(nested.capability_path(), Path::new("src/ui"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WorktreeDirectoryPath {
    /// The capability root itself.
    Root,
    /// One non-empty directory path below the root.
    Relative(WorktreeRelativePath),
}

impl WorktreeDirectoryPath {
    /// Returns the descendant path, or `None` for the root directory.
    #[must_use]
    pub const fn relative_path(&self) -> Option<&WorktreeRelativePath> {
        match self {
            Self::Root => None,
            Self::Relative(path) => Some(path),
        }
    }

    /// Returns the path used with the root capability.
    #[must_use]
    pub fn capability_path(&self) -> &Path {
        match self {
            Self::Root => Path::new("."),
            Self::Relative(path) => path.as_path(),
        }
    }

    /// Returns the root-derived absolute display path of this spelling.
    #[must_use]
    pub fn display_path(&self, root: &WorktreeRoot) -> PathBuf {
        match self {
            Self::Root => root.as_path().to_path_buf(),
            Self::Relative(path) => root.as_path().join(path.as_path()),
        }
    }
}

/// A non-normal path component rejected by [`WorktreeRelativePath::new`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedPathComponent {
    /// A `.` component.
    CurrentDirectory,
    /// A filesystem root component.
    RootDirectory,
    /// A platform path prefix.
    PlatformPrefix,
}

impl fmt::Display for UnsupportedPathComponent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory => formatter.write_str("current directory"),
            Self::RootDirectory => formatter.write_str("root directory"),
            Self::PlatformPrefix => formatter.write_str("platform prefix"),
        }
    }
}

/// A failure to construct a [`WorktreeRelativePath`].
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum WorktreeRelativePathError {
    /// The supplied child path is empty.
    #[error("the worktree-relative path must not be empty")]
    Empty,
    /// The supplied child path is absolute.
    #[error("the worktree-relative path must not be absolute")]
    Absolute,
    /// The supplied child path contains `..`.
    #[error("the worktree-relative path must not contain parent traversal")]
    ParentTraversal,
    /// The supplied child path contains another non-normal component.
    #[error("the worktree-relative path contains an unsupported {component} component")]
    UnsupportedComponent {
        /// The rejected component category.
        component: UnsupportedPathComponent,
    },
    /// The supplied child path exceeds [`WORKTREE_PATH_BYTES_MAX`].
    #[error("the worktree-relative path has {actual} bytes, above the limit of {max}")]
    PathBytesLimit {
        /// The supplied encoded byte count.
        actual: usize,
        /// The accepted encoded byte count.
        max: usize,
    },
    /// The supplied child path exceeds [`WORKTREE_PATH_COMPONENTS_MAX`].
    #[error("the worktree-relative path has {actual} components, above the limit of {max}")]
    PathComponentsLimit {
        /// The supplied component count at rejection.
        actual: usize,
        /// The accepted component count.
        max: usize,
    },
}

#[derive(Debug)]
enum ResolutionComponent {
    Normal {
        value: OsString,
        from_link_target: bool,
    },
    Parent,
}

fn prepend_link_target(
    root: &WorktreeRoot,
    resolved: &mut PathBuf,
    pending: &mut VecDeque<ResolutionComponent>,
    target: &Path,
) -> Result<(), WorktreeConfinementError> {
    let canonical_target;
    let target = if target.is_absolute() {
        canonical_target = fs::canonicalize(target).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                WorktreeConfinementError::DanglingLink
            } else if is_symlink_loop(&source) {
                WorktreeConfinementError::LinkLoop
            } else {
                WorktreeConfinementError::Access { source }
            }
        })?;
        resolved.clear();
        canonical_target
            .strip_prefix(root.as_path())
            .map_err(|_| WorktreeConfinementError::Escape)?
    } else {
        target
    };
    let mut components = Vec::new();
    for component in target.components() {
        match component {
            Component::Normal(component) => components.push(ResolutionComponent::Normal {
                value: component.to_os_string(),
                from_link_target: true,
            }),
            Component::CurDir => {}
            Component::ParentDir => components.push(ResolutionComponent::Parent),
            Component::RootDir | Component::Prefix(_) => {
                return Err(WorktreeConfinementError::Escape);
            }
        }
    }
    for component in components.into_iter().rev() {
        pending.push_front(component);
    }
    Ok(())
}

fn append_missing_components(
    resolved: &mut PathBuf,
    pending: VecDeque<ResolutionComponent>,
) -> Result<(), WorktreeConfinementError> {
    for component in pending {
        match component {
            ResolutionComponent::Normal { value, .. } => resolved.push(value),
            ResolutionComponent::Parent => {
                if !resolved.pop() {
                    return Err(WorktreeConfinementError::Escape);
                }
            }
        }
    }
    Ok(())
}

fn validated_resolved_path(
    path: PathBuf,
) -> Result<WorktreeRelativePath, WorktreeConfinementError> {
    WorktreeRelativePath::new(path).map_err(WorktreeConfinementError::InvalidResolvedPath)
}

#[cfg(target_os = "linux")]
fn is_symlink_loop(source: &io::Error) -> bool {
    source.raw_os_error() == Some(40)
}

#[cfg(target_os = "macos")]
fn is_symlink_loop(source: &io::Error) -> bool {
    source.raw_os_error() == Some(62)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn is_symlink_loop(_source: &io::Error) -> bool {
    false
}

fn non_root_component_count(path: &Path) -> usize {
    path.components()
        .filter(|component| !matches!(component, Component::RootDir | Component::Prefix(_)))
        .count()
}

#[cfg(test)]
mod tests;
