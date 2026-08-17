//! Tests for the file tree, the file-operation clipboard, and the workspace
//! mutations.
//!
//! Every test runs against one temporary directory that the test removes when
//! it finishes. The pump helper below performs the reads that the bounded
//! worker service performs in the running editor.

use std::fs;
use std::path::{Path, PathBuf};

use super::clipboard::{FILE_CLIPBOARD_PATHS_MAX, FileClipboard};
use super::mutation::{
    FileOperation, MUTATION_PATHS_MAX, MutationError, MutationPlan, OpenBuffer, TransferMode,
};
use super::temp::TempDir;
use super::tree::{
    DirectoryListing, EntryKind, Expansion, FileTree, LinkKind, RowContent, TREE_DEPTH_MAX,
    TREE_DIRECTORY_ENTRIES_MAX, TREE_ENTRIES_MAX, TREE_FILTER_CHARS_MAX, TreeEntry, Truncation,
    read_directory,
};
use super::tree_request::{MutateRequest, WorkspaceRequest, WorkspaceResult};
use crate::BufferId;

/// Returns the workspace root of one temporary directory.
///
/// The path is canonical, so it matches the paths that the tree builds from the
/// directory reads.
fn root_of(directory: &TempDir) -> PathBuf {
    fs::canonicalize(&directory.path).expect("the temporary directory exists")
}

/// Performs every pending directory read of the tree.
///
/// The running editor runs each read on the bounded worker service. The test
/// runs the same steps in order and returns the paths that it read.
fn pump(tree: &mut FileTree) -> Vec<PathBuf> {
    let mut read = Vec::new();
    while let Some(path) = tree.take_pending_read() {
        read.push(path.clone());
        match read_directory(&path) {
            Ok(listing) => tree.apply_listing(listing),
            Err(_) => tree.apply_read_failure(&path),
        }
    }
    read
}

/// Returns one label for every visible row.
///
/// A directory label ends with a slash, and a notice label is one exclamation
/// mark, so one comparison covers the row order and the row kinds.
fn labels(tree: &FileTree) -> Vec<String> {
    tree.rows()
        .iter()
        .map(|row| match &row.content {
            RowContent::File { name, .. } => name.clone(),
            RowContent::Directory { name, .. } => format!("{name}/"),
            RowContent::Notice(_) => "!".to_owned(),
        })
        .collect()
}

/// Returns the expansion state of one directory row.
fn expansion(tree: &FileTree, path: &Path) -> Option<Expansion> {
    tree.rows().iter().find_map(|row| match row.content {
        RowContent::Directory { expansion, .. } if row.path == path => Some(expansion),
        _ => None,
    })
}

/// Returns one loaded buffer that holds no unsaved change.
fn open_buffer(id: u32, path: &Path) -> OpenBuffer {
    OpenBuffer {
        id: BufferId::new(id),
        path: path.to_path_buf(),
        is_modified: false,
    }
}

#[test]
fn a_new_tree_reads_the_root_alone() {
    let directory = TempDir::new("tree-lazy");
    directory.file("src/lib.rs", "");
    directory.file("main.rs", "");
    let root = root_of(&directory);

    let mut tree = FileTree::new(root.clone());
    let read = pump(&mut tree);

    assert_eq!(read, vec![root.clone()]);
    assert_eq!(labels(&tree), vec!["src/", "main.rs"]);
    assert_eq!(
        expansion(&tree, &root.join("src")),
        Some(Expansion::Collapsed)
    );
}

#[test]
fn an_expansion_reads_the_named_directory_alone() {
    let directory = TempDir::new("tree-expand");
    directory.file("src/lib.rs", "");
    directory.file("docs/guide.md", "");
    let root = root_of(&directory);

    let mut tree = FileTree::new(root.clone());
    pump(&mut tree);
    tree.expand(&root.join("src"));
    let read = pump(&mut tree);

    assert_eq!(read, vec![root.join("src")]);
    assert_eq!(labels(&tree), vec!["docs/", "src/", "lib.rs"]);
}

#[test]
fn a_collapse_hides_the_children_and_drops_the_listing() {
    let directory = TempDir::new("tree-collapse");
    directory.file("src/lib.rs", "");
    let root = root_of(&directory);

    let mut tree = FileTree::new(root.clone());
    pump(&mut tree);
    tree.expand(&root.join("src"));
    pump(&mut tree);
    tree.collapse(&root.join("src"));

    assert_eq!(labels(&tree), vec!["src/"]);
    tree.expand(&root.join("src"));
    assert_eq!(pump(&mut tree), vec![root.join("src")]);
}

#[test]
fn the_order_puts_directories_first_and_then_sorts_by_name() {
    let directory = TempDir::new("tree-order");
    directory.dir("zebra");
    directory.dir("alpha");
    directory.file("z.rs", "");
    directory.file("a.rs", "");
    directory.file("b.rs", "");
    let root = root_of(&directory);

    let mut tree = FileTree::new(root);
    pump(&mut tree);

    assert_eq!(
        labels(&tree),
        vec!["alpha/", "zebra/", "a.rs", "b.rs", "z.rs"]
    );
}

#[test]
fn the_default_filter_hides_dotfiles_and_the_named_files() {
    let directory = TempDir::new("tree-hidden");
    directory.file(".env", "");
    directory.file(".DS_Store", "");
    directory.file("thumbs.db", "");
    directory.file("main.rs", "");
    let root = root_of(&directory);

    let mut tree = FileTree::new(root);
    pump(&mut tree);
    assert_eq!(labels(&tree), vec!["main.rs"]);

    tree.toggle_hidden();
    assert_eq!(
        labels(&tree),
        vec![".DS_Store", ".env", "main.rs", "thumbs.db"]
    );

    tree.toggle_hidden();
    assert_eq!(labels(&tree), vec!["main.rs"]);
}

#[test]
fn a_query_keeps_the_matching_rows_and_the_directories_above_them() {
    let directory = TempDir::new("tree-query");
    directory.file("src/lib.rs", "");
    directory.file("src/notes.txt", "");
    directory.file("README.md", "");
    let root = root_of(&directory);

    let mut tree = FileTree::new(root.clone());
    pump(&mut tree);
    tree.expand(&root.join("src"));
    pump(&mut tree);

    tree.set_query("LIB");
    assert_eq!(labels(&tree), vec!["src/", "lib.rs"]);

    tree.set_query("");
    assert_eq!(
        labels(&tree),
        vec!["src/", "lib.rs", "notes.txt", "README.md"]
    );
}

#[test]
fn a_reveal_loads_the_directories_of_the_path_alone() {
    let directory = TempDir::new("tree-reveal");
    directory.file("a/b/target.rs", "");
    directory.file("other/ignored.rs", "");
    let root = root_of(&directory);
    let target = root.join("a").join("b").join("target.rs");

    let mut tree = FileTree::new(root.clone());
    pump(&mut tree);
    tree.reveal(&target);
    let read = pump(&mut tree);

    assert_eq!(read, vec![root.join("a"), root.join("a").join("b")]);
    assert_eq!(tree.selected(), Some(target.as_path()));
    assert_eq!(
        expansion(&tree, &root.join("other")),
        Some(Expansion::Collapsed)
    );
}

#[test]
fn a_refresh_keeps_the_expansion_and_the_selection() {
    let directory = TempDir::new("tree-refresh");
    directory.file("src/lib.rs", "");
    let root = root_of(&directory);
    let selected = root.join("src").join("lib.rs");

    let mut tree = FileTree::new(root.clone());
    pump(&mut tree);
    tree.expand(&root.join("src"));
    pump(&mut tree);
    tree.select(&selected);

    directory.file("src/added.rs", "");
    tree.refresh(&root.join("src"));
    pump(&mut tree);

    assert_eq!(tree.selected(), Some(selected.as_path()));
    assert_eq!(
        expansion(&tree, &root.join("src")),
        Some(Expansion::Expanded)
    );
    assert_eq!(labels(&tree), vec!["src/", "added.rs", "lib.rs"]);
}

#[test]
fn a_refresh_after_a_removal_selects_the_closest_visible_ancestor() {
    let directory = TempDir::new("tree-removed");
    let removed = directory.file("src/lib.rs", "");
    let root = root_of(&directory);

    let mut tree = FileTree::new(root.clone());
    pump(&mut tree);
    tree.expand(&root.join("src"));
    pump(&mut tree);
    tree.select(&root.join("src").join("lib.rs"));

    fs::remove_file(&removed).expect("the test file exists");
    tree.refresh(&root.join("src"));
    pump(&mut tree);

    assert_eq!(tree.selected(), Some(root.join("src").as_path()));
}

#[test]
fn a_refresh_drops_the_state_of_a_directory_that_disappeared() {
    let directory = TempDir::new("tree-gone");
    directory.file("old/inner.rs", "");
    let root = root_of(&directory);

    let mut tree = FileTree::new(root.clone());
    pump(&mut tree);
    tree.expand(&root.join("old"));
    pump(&mut tree);

    fs::remove_dir_all(root.join("old")).expect("the test directory exists");
    tree.refresh(&root);
    pump(&mut tree);

    assert!(labels(&tree).is_empty());
    assert_eq!(tree.take_pending_read(), None);
    assert_eq!(tree.selected(), None);
}

#[test]
fn a_navigation_step_skips_the_notice_row() {
    let directory = TempDir::new("tree-navigate");
    directory.file("a.rs", "");
    directory.file("b.rs", "");
    let root = root_of(&directory);

    let mut tree = FileTree::new(root.clone());
    pump(&mut tree);

    assert_eq!(tree.selected(), Some(root.join("a.rs").as_path()));
    tree.select_next();
    assert_eq!(tree.selected(), Some(root.join("b.rs").as_path()));
    tree.select_next();
    assert_eq!(tree.selected(), Some(root.join("b.rs").as_path()));
    tree.select_previous();
    assert_eq!(tree.selected(), Some(root.join("a.rs").as_path()));
}

#[test]
fn the_directory_bound_reports_the_truncation() {
    let directory = TempDir::new("tree-bound");
    for index in 0..TREE_DIRECTORY_ENTRIES_MAX + 3 {
        directory.file(&format!("file-{index:05}.rs"), "");
    }
    let root = root_of(&directory);

    let listing = read_directory(&root).expect("the directory is readable");
    assert_eq!(listing.entries.len(), TREE_DIRECTORY_ENTRIES_MAX);
    assert_eq!(
        listing.truncation,
        Truncation::Truncated {
            shown: TREE_DIRECTORY_ENTRIES_MAX,
            total: TREE_DIRECTORY_ENTRIES_MAX + 3,
        }
    );

    let mut tree = FileTree::new(root);
    pump(&mut tree);
    assert_eq!(tree.rows().len(), TREE_DIRECTORY_ENTRIES_MAX + 1);
    assert_eq!(labels(&tree).last().map(String::as_str), Some("!"));
}

#[test]
fn the_tree_bound_truncates_a_later_listing() {
    let root = PathBuf::from("/workspace");
    let mut tree = FileTree::new(root.clone());
    assert_eq!(tree.take_pending_read(), Some(root.clone()));

    let mut entries = vec![TreeEntry {
        name: "sub".to_owned(),
        kind: EntryKind::Directory,
        link: LinkKind::Direct,
    }];
    for index in 1..TREE_ENTRIES_MAX {
        entries.push(TreeEntry {
            name: format!("file-{index:05}.rs"),
            kind: EntryKind::File,
            link: LinkKind::Direct,
        });
    }
    tree.apply_listing(DirectoryListing {
        path: root.clone(),
        entries,
        truncation: Truncation::Complete,
    });

    tree.expand(&root.join("sub"));
    assert_eq!(tree.take_pending_read(), Some(root.join("sub")));
    tree.apply_listing(DirectoryListing {
        path: root.join("sub"),
        entries: vec![TreeEntry {
            name: "late.rs".to_owned(),
            kind: EntryKind::File,
            link: LinkKind::Direct,
        }],
        truncation: Truncation::Complete,
    });

    // The tree holds its maximum, so the later listing shows a notice instead
    // of a partial directory.
    assert_eq!(labels(&tree)[1], "!");
    assert!(!labels(&tree).contains(&"late.rs".to_owned()));
}

#[test]
fn the_depth_bound_stops_the_expansion() {
    let directory = TempDir::new("tree-depth");
    let mut relative = String::new();
    for level in 0..=TREE_DEPTH_MAX {
        relative.push_str(&format!("d{level}/"));
    }
    directory.file(&format!("{relative}leaf.rs"), "");
    let root = root_of(&directory);
    let mut deepest = root.clone();
    for level in 0..=TREE_DEPTH_MAX {
        deepest.push(format!("d{level}"));
    }

    let mut tree = FileTree::new(root);
    pump(&mut tree);
    tree.reveal(&deepest.join("leaf.rs"));
    pump(&mut tree);

    // The deepest row sits at the depth bound. The tree refuses to expand it,
    // so the entries below it stay unloaded.
    let last = deepest
        .parent()
        .expect("the path holds every level")
        .to_path_buf();
    assert!(!labels(&tree).contains(&"leaf.rs".to_owned()));
    assert!(tree.rows().iter().all(|row| row.depth < TREE_DEPTH_MAX));
    assert_eq!(expansion(&tree, &last), Some(Expansion::Collapsed));
}

#[test]
fn the_query_bound_keeps_the_first_characters() {
    let mut tree = FileTree::new(PathBuf::from("/workspace"));
    tree.set_query(&"a".repeat(TREE_FILTER_CHARS_MAX * 2));

    assert_eq!(tree.filter().query().chars().count(), TREE_FILTER_CHARS_MAX);
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_to_a_directory_expands_like_a_directory() {
    let directory = TempDir::new("tree-symlink");
    directory.file("real/inner.rs", "");
    let root = root_of(&directory);
    std::os::unix::fs::symlink(root.join("real"), root.join("link"))
        .expect("the temporary directory is writable");

    let mut tree = FileTree::new(root.clone());
    pump(&mut tree);
    tree.expand(&root.join("link"));
    pump(&mut tree);

    assert_eq!(labels(&tree), vec!["link/", "inner.rs", "real/"]);
    let row = tree
        .rows()
        .iter()
        .find(|row| row.path == root.join("link"))
        .expect("the link is one row");
    assert!(matches!(
        row.content,
        RowContent::Directory {
            link: LinkKind::Symlink,
            ..
        }
    ));
}

#[test]
fn a_create_adds_the_entry_and_selects_it() {
    let directory = TempDir::new("mutate-create");
    let root = root_of(&directory);
    let path = root.join("new.rs");

    let outcome = MutationPlan::stage(
        &FileOperation::Create {
            path: path.clone(),
            kind: EntryKind::File,
        },
        &root,
        &[],
    )
    .expect("the path is free")
    .apply()
    .expect("the directory is writable");

    assert!(path.is_file());
    assert_eq!(outcome.selection, Some(path));
    assert_eq!(outcome.changed, vec![root]);
}

#[test]
fn a_create_refuses_an_existing_path() {
    let directory = TempDir::new("mutate-create-collision");
    let path = directory.file("main.rs", "kept");
    let root = root_of(&directory);

    let error = MutationPlan::stage(
        &FileOperation::Create {
            path: root.join("main.rs"),
            kind: EntryKind::File,
        },
        &root,
        &[],
    )
    .expect_err("the path holds a file");

    assert!(matches!(error, MutationError::Collision { .. }));
    assert_eq!(fs::read_to_string(path).expect("the file exists"), "kept");
}

#[test]
fn a_rename_moves_the_file_and_retargets_its_buffer() {
    let directory = TempDir::new("mutate-rename");
    directory.file("old.rs", "content");
    let root = root_of(&directory);
    let from = root.join("old.rs");
    let to = root.join("new.rs");

    let outcome = MutationPlan::stage(
        &FileOperation::Rename {
            from: from.clone(),
            to: to.clone(),
        },
        &root,
        &[open_buffer(1, &from)],
    )
    .expect("the destination is free")
    .apply()
    .expect("the directory is writable");

    assert!(!from.exists());
    assert_eq!(fs::read_to_string(&to).expect("the file exists"), "content");
    assert_eq!(outcome.updates.len(), 1);
    assert_eq!(outcome.updates[0].buffer, BufferId::new(1));
    assert_eq!(outcome.updates[0].path, to);
}

#[test]
fn a_move_retargets_the_buffer_of_a_file_inside_the_directory() {
    let directory = TempDir::new("mutate-move");
    directory.file("src/lib.rs", "content");
    directory.dir("dest");
    let root = root_of(&directory);
    let buffer_path = root.join("src").join("lib.rs");

    let outcome = MutationPlan::stage(
        &FileOperation::Transfer {
            mode: TransferMode::Move,
            sources: vec![root.join("src")],
            destination: root.join("dest"),
        },
        &root,
        &[open_buffer(4, &buffer_path)],
    )
    .expect("the destination is free")
    .apply()
    .expect("the directory is writable");

    let moved = root.join("dest").join("src").join("lib.rs");
    assert!(moved.is_file());
    assert!(!root.join("src").exists());
    assert_eq!(outcome.updates[0].path, moved);
}

#[test]
fn a_copy_keeps_the_source_and_changes_no_buffer() {
    let directory = TempDir::new("mutate-copy");
    directory.file("src/lib.rs", "content");
    directory.dir("dest");
    let root = root_of(&directory);
    let buffer_path = root.join("src").join("lib.rs");

    let outcome = MutationPlan::stage(
        &FileOperation::Transfer {
            mode: TransferMode::Copy,
            sources: vec![root.join("src")],
            destination: root.join("dest"),
        },
        &root,
        &[open_buffer(2, &buffer_path)],
    )
    .expect("the destination is free")
    .apply()
    .expect("the directory is writable");

    assert!(buffer_path.is_file());
    assert_eq!(
        fs::read_to_string(root.join("dest").join("src").join("lib.rs")).expect("the copy exists"),
        "content"
    );
    assert!(outcome.updates.is_empty());
}

#[test]
fn a_transfer_refuses_a_destination_collision() {
    let directory = TempDir::new("mutate-collision");
    directory.file("lib.rs", "source");
    directory.file("dest/lib.rs", "kept");
    let root = root_of(&directory);

    let error = MutationPlan::stage(
        &FileOperation::Transfer {
            mode: TransferMode::Move,
            sources: vec![root.join("lib.rs")],
            destination: root.join("dest"),
        },
        &root,
        &[],
    )
    .expect_err("the destination holds the name");

    assert!(matches!(error, MutationError::Collision { .. }));
    assert!(root.join("lib.rs").is_file());
    assert_eq!(
        fs::read_to_string(root.join("dest").join("lib.rs")).expect("the file exists"),
        "kept"
    );
}

#[test]
fn two_sources_with_one_name_collide_before_any_change() {
    let directory = TempDir::new("mutate-duplicate");
    directory.file("a/lib.rs", "first");
    directory.file("b/lib.rs", "second");
    directory.dir("dest");
    let root = root_of(&directory);

    let error = MutationPlan::stage(
        &FileOperation::Transfer {
            mode: TransferMode::Copy,
            sources: vec![root.join("a").join("lib.rs"), root.join("b").join("lib.rs")],
            destination: root.join("dest"),
        },
        &root,
        &[],
    )
    .expect_err("both sources hold one name");

    assert!(matches!(error, MutationError::Collision { .. }));
    assert_eq!(
        fs::read_dir(root.join("dest"))
            .expect("the directory exists")
            .count(),
        0
    );
}

#[test]
fn a_directory_cannot_move_into_its_own_descendant() {
    let directory = TempDir::new("mutate-descendant");
    directory.dir("src/inner");
    let root = root_of(&directory);

    let error = MutationPlan::stage(
        &FileOperation::Transfer {
            mode: TransferMode::Move,
            sources: vec![root.join("src")],
            destination: root.join("src").join("inner"),
        },
        &root,
        &[],
    )
    .expect_err("the destination lies inside the source");

    assert!(matches!(error, MutationError::IntoDescendant { .. }));
    assert!(root.join("src").join("inner").is_dir());
}

#[test]
fn a_mutation_refuses_a_path_outside_the_workspace() {
    let directory = TempDir::new("mutate-outside");
    let outside = directory.file("outside.rs", "");
    let root = directory.dir("workspace");
    let root = fs::canonicalize(root).expect("the directory exists");

    let error = MutationPlan::stage(
        &FileOperation::Transfer {
            mode: TransferMode::Move,
            sources: vec![outside],
            destination: root.clone(),
        },
        &root,
        &[],
    )
    .expect_err("the source lies outside the workspace");

    assert!(matches!(error, MutationError::Outside { .. }));
}

#[test]
fn the_path_bound_refuses_an_oversized_operation() {
    let directory = TempDir::new("mutate-bound");
    let root = root_of(&directory);
    let paths: Vec<PathBuf> = (0..=MUTATION_PATHS_MAX)
        .map(|index| root.join(format!("file-{index}.rs")))
        .collect();

    let error = MutationPlan::stage(&FileOperation::Delete { paths }, &root, &[])
        .expect_err("the operation names too many entries");

    assert!(matches!(
        error,
        MutationError::TooManyPaths {
            max: MUTATION_PATHS_MAX,
            ..
        }
    ));
}

#[test]
fn a_delete_removes_the_entries() {
    let directory = TempDir::new("mutate-delete");
    directory.file("src/lib.rs", "");
    directory.file("main.rs", "");
    let root = root_of(&directory);

    MutationPlan::stage(
        &FileOperation::Delete {
            paths: vec![root.join("src"), root.join("main.rs")],
        },
        &root,
        &[],
    )
    .expect("both entries exist")
    .apply()
    .expect("the directory is writable");

    assert!(!root.join("src").exists());
    assert!(!root.join("main.rs").exists());
}

#[test]
fn a_delete_refuses_an_entry_with_unsaved_changes() {
    let directory = TempDir::new("mutate-delete-dirty");
    directory.file("src/lib.rs", "content");
    let root = root_of(&directory);
    let mut buffer = open_buffer(3, &root.join("src").join("lib.rs"));
    buffer.is_modified = true;

    let error = MutationPlan::stage(
        &FileOperation::Delete {
            paths: vec![root.join("src")],
        },
        &root,
        &[buffer],
    )
    .expect_err("the buffer holds unsaved changes");

    assert!(matches!(error, MutationError::DirtyBuffer { .. }));
    assert!(root.join("src").join("lib.rs").is_file());
}

#[test]
fn a_failed_copy_leaves_no_partial_result() {
    let directory = TempDir::new("mutate-partial-copy");
    directory.file("first.rs", "first");
    directory.file("second.rs", "second");
    directory.dir("dest");
    let root = root_of(&directory);

    let plan = MutationPlan::stage(
        &FileOperation::Transfer {
            mode: TransferMode::Copy,
            sources: vec![root.join("first.rs"), root.join("second.rs")],
            destination: root.join("dest"),
        },
        &root,
        &[],
    )
    .expect("both sources exist");

    // The second source disappears between the validation and the copy, so the
    // commit cannot finish.
    fs::remove_file(root.join("second.rs")).expect("the file exists");
    let error = plan.apply().expect_err("the second source is gone");

    assert!(matches!(error, MutationError::Filesystem { .. }));
    assert_eq!(
        fs::read_dir(root.join("dest"))
            .expect("the directory exists")
            .count(),
        0
    );
    assert!(root.join("first.rs").is_file());
}

#[test]
fn a_failed_move_restores_every_staged_source() {
    let directory = TempDir::new("mutate-partial-move");
    directory.file("first.rs", "first");
    directory.file("second.rs", "second");
    directory.dir("dest");
    let root = root_of(&directory);

    let plan = MutationPlan::stage(
        &FileOperation::Transfer {
            mode: TransferMode::Move,
            sources: vec![root.join("first.rs"), root.join("second.rs")],
            destination: root.join("dest"),
        },
        &root,
        &[],
    )
    .expect("both sources exist");

    fs::remove_file(root.join("second.rs")).expect("the file exists");
    let error = plan.apply().expect_err("the second source is gone");

    assert!(matches!(error, MutationError::Filesystem { .. }));
    assert_eq!(
        fs::read_to_string(root.join("first.rs")).expect("the source returned"),
        "first"
    );
    assert_eq!(
        fs::read_dir(root.join("dest"))
            .expect("the directory exists")
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn a_copy_recreates_a_symbolic_link() {
    let directory = TempDir::new("mutate-symlink");
    directory.file("src/inner.rs", "content");
    directory.dir("dest");
    let root = root_of(&directory);
    std::os::unix::fs::symlink(
        root.join("src").join("inner.rs"),
        root.join("src").join("link.rs"),
    )
    .expect("the temporary directory is writable");

    MutationPlan::stage(
        &FileOperation::Transfer {
            mode: TransferMode::Copy,
            sources: vec![root.join("src")],
            destination: root.join("dest"),
        },
        &root,
        &[],
    )
    .expect("the destination is free")
    .apply()
    .expect("the directory is writable");

    let copied = root.join("dest").join("src").join("link.rs");
    assert!(
        fs::symlink_metadata(&copied)
            .expect("the copy exists")
            .is_symlink()
    );
}

#[test]
fn the_clipboard_builds_one_paste_of_the_held_entries() {
    let directory = TempDir::new("clipboard-paste");
    directory.file("lib.rs", "content");
    directory.dir("dest");
    let root = root_of(&directory);

    let mut clipboard = FileClipboard::default();
    assert!(clipboard.is_empty());
    assert!(clipboard.paste(&root).is_none());

    clipboard.hold(TransferMode::Move, vec![root.join("lib.rs")]);
    assert_eq!(clipboard.mode(), Some(TransferMode::Move));
    let operation = clipboard
        .paste(&root.join("dest"))
        .expect("the clipboard holds one entry");

    let result = WorkspaceRequest::Mutate(MutateRequest {
        operation,
        root: root.clone(),
        buffers: Vec::new(),
    })
    .run();
    let WorkspaceResult::Mutated { outcome } = result else {
        panic!("the request was one mutation");
    };
    outcome.expect("the destination is free");

    clipboard.clear();
    assert!(clipboard.is_empty());
    assert!(root.join("dest").join("lib.rs").is_file());
    assert!(!root.join("lib.rs").exists());
}

#[test]
fn the_clipboard_holds_no_more_than_the_bound() {
    let paths: Vec<PathBuf> = (0..FILE_CLIPBOARD_PATHS_MAX + 5)
        .map(|index| PathBuf::from(format!("/workspace/file-{index}.rs")))
        .collect();

    let mut clipboard = FileClipboard::default();
    clipboard.hold(TransferMode::Copy, paths);

    assert_eq!(clipboard.paths().len(), FILE_CLIPBOARD_PATHS_MAX);
}

#[test]
fn a_directory_request_returns_the_listing_of_the_named_directory() {
    let directory = TempDir::new("request-read");
    directory.file("main.rs", "");
    let root = root_of(&directory);

    let result = WorkspaceRequest::ReadDirectory { path: root.clone() }.run();

    let WorkspaceResult::Directory { path, outcome } = result else {
        panic!("the request was one directory read");
    };
    assert_eq!(path, root);
    let listing = outcome.expect("the directory is readable");
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].name, "main.rs");
}

#[test]
fn a_directory_request_rejects_a_file() {
    let directory = TempDir::new("request-file");
    let path = directory.file("main.rs", "");

    let WorkspaceResult::Directory { outcome, .. } = WorkspaceRequest::ReadDirectory { path }.run()
    else {
        panic!("the request was one directory read");
    };

    assert!(outcome.is_err());
}
