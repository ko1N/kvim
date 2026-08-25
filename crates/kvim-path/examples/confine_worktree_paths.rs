//! Opens one worktree root, resolves targets below it, and refuses an escape.
//!
//! This is the whole confinement workflow that a consumer of `kvim-path`
//! starts from: take an absolute directory, turn every later path into a
//! validated identity, and re-check that identity before a write commits.
//!
//! Run it with `cargo run -p kvim-path --example confine_worktree_paths`.

use std::error::Error;
use std::fs;
use std::path::PathBuf;

use kvim_path::{
    ResolvedTargetState, WorktreeConfinementError, WorktreeRelativePath, WorktreeRoot,
};

fn main() -> Result<(), Box<dyn Error>> {
    let worktree = build_worktree()?;

    // 1. The root is the one capability. Every later path is relative to it.
    let root = WorktreeRoot::open(&worktree)?;
    println!("root: {}", root.as_path().display());

    // 2. A relative path is validated once and carries the invariant onward.
    let source = WorktreeRelativePath::new("src/lib.rs")?;
    let resolved = root.resolve(&source)?;
    println!(
        "{} -> {:?}, followed a link: {}",
        source.as_path().display(),
        resolved.state(),
        resolved.followed_link()
    );

    // 3. A target that no file holds yet still resolves, so a save can create it.
    let planned = root.resolve(&WorktreeRelativePath::new("src/new.rs")?)?;
    assert_eq!(planned.state(), ResolvedTargetState::Missing);
    println!("src/new.rs is absent, and a save may still create it");

    // 4. Re-checking before a commit catches a link that changed underneath.
    root.revalidate(&source, &resolved)?;
    println!("src/lib.rs still has the identity that the load read");

    // 5. Traversal outside the root never becomes a path at all.
    match WorktreeRelativePath::new("../etc/passwd") {
        Ok(path) => panic!("a parent traversal must not validate: {path:?}"),
        Err(error) => println!("refused `../etc/passwd`: {error}"),
    }

    // 6. A link that leaves the root resolves to a typed confinement failure.
    let escape = WorktreeRelativePath::new("escape")?;
    match root.resolve(&escape) {
        Err(WorktreeConfinementError::Escape) => println!("refused the escaping link `escape`"),
        other => panic!("an escaping link must not resolve: {other:?}"),
    }

    fs::remove_dir_all(&worktree)?;
    Ok(())
}

/// Creates one throwaway worktree with a source file and an escaping link.
fn build_worktree() -> Result<PathBuf, Box<dyn Error>> {
    let worktree = std::env::temp_dir().join(format!("kvim-example-path-{}", std::process::id()));
    let _ = fs::remove_dir_all(&worktree);
    fs::create_dir_all(worktree.join("src"))?;
    fs::write(worktree.join("src").join("lib.rs"), "// one file\n")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc", worktree.join("escape"))?;
    Ok(worktree)
}
