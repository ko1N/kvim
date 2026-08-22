//! Tests for buffer identity, file loading, the atomic save, and the undo file.
//!
//! Every test runs against one temporary directory that the test removes when
//! it finishes. No test reads or writes the editor state directory of the user.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kvim_core::{CharRange, EditTransaction, TextBuffer, TextChange};
use kvim_path::{WorktreeConfinementError, WorktreeRelativePath, WorktreeRoot};
use kvim_settings::FileSettings;

use super::buffer::{Buffers, FileBuffer};
use super::file::{self, OpenError, SaveError};
use super::request::{FileRequest, FileResult, OpenRequest, SaveRequest};
use super::temp::TempDir;
use super::undo_file::{self, UNDO_FILE_STEPS_MAX, UndoRecord};

/// Returns settings that never touch the editor state directory.
fn files() -> FileSettings {
    FileSettings {
        undo_file: false,
        ..FileSettings::default()
    }
}

/// Returns one buffer with the given text.
fn buffer(text: &str) -> TextBuffer {
    TextBuffer::from_text(text, &files()).expect("the test text is small")
}

fn root(directory: &TempDir) -> Arc<WorktreeRoot> {
    Arc::new(WorktreeRoot::open(&directory.path).expect("the temporary worktree exists"))
}

fn relative(directory: &TempDir, path: &Path) -> WorktreeRelativePath {
    WorktreeRelativePath::new(
        path.strip_prefix(&directory.path)
            .expect("the test target is below its worktree"),
    )
    .expect("the test target is a valid relative path")
}

fn load_path(
    root: &Arc<WorktreeRoot>,
    directory: &TempDir,
    path: &Path,
    settings: &FileSettings,
) -> Result<file::LoadedFile, OpenError> {
    file::load(root, &relative(directory, path), settings)
}

/// Replaces one character range of a buffer as one transaction.
fn edit(text: &mut TextBuffer, start: usize, end: usize, replacement: &str) {
    let cursor = text.char_position(start).expect("the position exists");
    let range = CharRange::new(
        text.char_position(start).expect("the position exists"),
        text.char_position(end).expect("the position exists"),
    )
    .expect("the range ascends");
    text.apply(EditTransaction::single(
        cursor,
        TextChange::replace(range, replacement),
    ))
    .expect("the range fits the buffer");
}

#[test]
fn a_load_reports_the_text_and_the_file_identity() {
    let directory = TempDir::new("load");
    let path = directory.write("main.rs", "fn main() {}\n");
    let root = root(&directory);

    let loaded =
        load_path(&root, &directory, &path, &files()).expect("the file is a small UTF-8 file");
    assert_eq!(loaded.text, "fn main() {}\n");
    let identity = loaded.identity.expect("the file exists");
    assert_eq!(identity.len_bytes, 13);
}

#[test]
fn a_missing_path_loads_an_empty_buffer_without_an_identity() {
    let directory = TempDir::new("missing");
    let root = root(&directory);
    let loaded = load_path(&root, &directory, &directory.join("new.rs"), &files())
        .expect("a missing path is not a failure");
    assert_eq!(loaded.text, "");
    assert!(
        loaded.identity.is_none(),
        "the first save of a new file must not report a conflict"
    );
}

#[test]
fn a_load_rejects_every_unsupported_file() {
    let directory = TempDir::new("reject");
    let root = root(&directory);
    let child_directory = directory.dir("child");
    assert!(matches!(
        load_path(&root, &directory, &child_directory, &files()),
        Err(OpenError::Directory)
    ));

    let binary = directory.join("binary.rs");
    fs::write(&binary, [0x66, 0x6e, 0xff, 0x0a]).expect("the directory is writable");
    assert!(matches!(
        load_path(&root, &directory, &binary, &files()),
        Err(OpenError::NotUtf8 { valid_up_to: 2 })
    ));

    let mut small = files();
    small.max_file_bytes = 4;
    let large = directory.write("large.rs", "123456");
    assert!(matches!(
        load_path(&root, &directory, &large, &small),
        Err(OpenError::TooLarge {
            bytes: 6,
            max_bytes: 4
        })
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let device = directory.join("device");
        symlink("/dev/null", &device).expect("the temporary directory supports links");
        assert!(matches!(
            load_path(&root, &directory, &device, &files()),
            Err(OpenError::Confinement(WorktreeConfinementError::Escape))
        ));
    }
}

#[test]
fn a_save_replaces_the_file_and_keeps_its_permissions() {
    let directory = TempDir::new("save");
    let path = directory.write("main.rs", "old\n");
    let root = root(&directory);
    let loaded = load_path(&root, &directory, &path, &files()).expect("the file is small");

    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&path, Permissions::from_mode(0o640)).expect("the file is owned");
    }

    let saved = file::save(&loaded.target, "new content\n", loaded.identity, &files())
        .expect("the recorded identity still matches");
    assert_eq!(
        fs::read_to_string(&path).expect("the file exists"),
        "new content\n"
    );
    assert_eq!(saved.identity.len_bytes, 12);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(&path)
            .expect("the file exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o640, "the save keeps the file permissions");
    }
}

#[test]
fn a_save_leaves_no_temporary_file_behind() {
    let directory = TempDir::new("temporary");
    let path = directory.write("main.rs", "old\n");
    let root = root(&directory);
    let loaded = load_path(&root, &directory, &path, &files()).expect("the file is small");
    file::save(&loaded.target, "new\n", loaded.identity, &files()).expect("the save succeeds");

    let entries: Vec<PathBuf> = fs::read_dir(&directory.path)
        .expect("the directory exists")
        .map(|entry| entry.expect("the entry is readable").path())
        .collect();
    assert_eq!(entries.len(), 1, "the staged replacement removes its file");
}

#[cfg(unix)]
#[test]
fn a_temporary_collision_never_follows_truncates_or_removes_the_entry() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new("temporary-collision");
    let root = root(&directory);
    let capability = root
        .directory()
        .open_dir(".")
        .expect("the worktree root is a directory");
    let temporary_name = "collision.tmp";
    let temporary = Path::new(temporary_name);

    fs::write(directory.join(temporary_name), "existing\n")
        .expect("the collision file is writable");
    assert!(matches!(
        file::create_temporary(&capability, temporary),
        Err(SaveError::Write(error)) if error.kind() == std::io::ErrorKind::AlreadyExists
    ));
    assert_eq!(
        fs::read_to_string(directory.join(temporary_name)).expect("the collision file remains"),
        "existing\n"
    );

    fs::remove_file(directory.join(temporary_name)).expect("the collision file is removable");
    let target = directory.write("target.tmp", "target\n");
    symlink("target.tmp", directory.join(temporary_name))
        .expect("the temporary directory supports links");
    assert!(matches!(
        file::create_temporary(&capability, temporary),
        Err(SaveError::Write(error)) if error.kind() == std::io::ErrorKind::AlreadyExists
    ));
    assert!(
        fs::symlink_metadata(directory.join(temporary_name))
            .expect("the collision link remains")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(target).expect("the link target remains"),
        "target\n"
    );
}

#[test]
fn an_external_change_becomes_a_conflict_instead_of_an_overwrite() {
    let directory = TempDir::new("conflict");
    let path = directory.write("main.rs", "one\n");
    let root = root(&directory);
    let loaded = load_path(&root, &directory, &path, &files()).expect("the file is small");

    fs::write(&path, "another program wrote this\n").expect("the file is writable");
    assert!(matches!(
        file::save(
            &loaded.target,
            "kvim wrote this\n",
            loaded.identity,
            &files()
        ),
        Err(SaveError::Conflict)
    ));
    assert_eq!(
        fs::read_to_string(&path).expect("the file exists"),
        "another program wrote this\n",
        "a conflict never overwrites the file"
    );
}

#[test]
fn a_file_that_appeared_after_the_open_becomes_a_conflict() {
    let directory = TempDir::new("appeared");
    let path = directory.join("new.rs");
    let root = root(&directory);
    let loaded =
        load_path(&root, &directory, &path, &files()).expect("a missing path is not a failure");
    assert!(loaded.identity.is_none());

    fs::write(&path, "another program created this\n").expect("the directory is writable");
    assert!(matches!(
        file::save(
            &loaded.target,
            "kvim wrote this\n",
            loaded.identity,
            &files()
        ),
        Err(SaveError::Conflict)
    ));
}

#[test]
fn a_failed_save_keeps_the_original_file() {
    let directory = TempDir::new("failure");
    let blocker = directory.write("blocker", "not a directory\n");
    // The parent of the target is a file, so no write can succeed.
    let target = blocker.join("main.rs");

    let root = root(&directory);
    let failure = file::load(&root, &relative(&directory, &target), &files());
    assert!(matches!(failure, Err(OpenError::Confinement(_))));
    assert_eq!(
        fs::read_to_string(&blocker).expect("the file exists"),
        "not a directory\n"
    );
}

#[cfg(unix)]
#[test]
fn a_save_through_a_symlink_replaces_the_target_file() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new("symlink");
    let target = directory.write("target.rs", "old\n");
    let link = directory.join("link.rs");
    symlink(&target, &link).expect("the directory supports symlinks");

    let root = root(&directory);
    let loaded = load_path(&root, &directory, &link, &files()).expect("the target is a small file");
    let saved =
        file::save(&loaded.target, "new\n", loaded.identity, &files()).expect("the save succeeds");

    assert_eq!(
        fs::read_to_string(&target).expect("the target exists"),
        "new\n"
    );
    assert!(
        fs::symlink_metadata(&link)
            .expect("the link exists")
            .file_type()
            .is_symlink(),
        "the save replaces the target, not the link"
    );
    assert_eq!(
        saved.target.as_path(),
        fs::canonicalize(&target).expect("the target exists")
    );
}

#[test]
fn an_open_request_builds_one_buffer_and_a_save_request_writes_it() {
    let directory = TempDir::new("request");
    let path = directory.write("main.rs", "fn main() {}\n");
    let root = root(&directory);

    let opened = FileRequest::Open(OpenRequest {
        root,
        path: relative(&directory, &path),
        files: files(),
    })
    .run();
    let FileResult::Opened { outcome, .. } = opened else {
        panic!("an open request returns an open result");
    };
    let mut file = outcome.expect("the file is a small UTF-8 file");
    assert_eq!(file.text.to_string(), "fn main() {}\n");
    assert!(!file.text.is_modified());

    edit(&mut file.text, 0, 0, "// note\n");
    assert!(file.text.is_modified());

    let saved = FileRequest::Save(SaveRequest {
        buffer: super::BufferId::new(1),
        target: file.target.clone(),
        content: file.text.to_string(),
        version: file.text.version(),
        expected: file.identity,
        snapshot: file.text.clone(),
        files: files(),
    })
    .run();
    let FileResult::Saved { outcome, .. } = saved else {
        panic!("a save request returns a save result");
    };
    let report = outcome.expect("the recorded identity still matches");
    assert_eq!(report.bytes, 21);
    assert_eq!(
        fs::read_to_string(&path).expect("the file exists"),
        "// note\nfn main() {}\n"
    );
}

#[test]
fn a_save_that_changes_nothing_writes_the_bytes_that_the_file_held() {
    // The buffer terminates its last line so that the editor can reach it. The
    // save writes the file end that the file held, so an unchanged file keeps
    // every byte, with and without a final line ending.
    let directory = TempDir::new("save-round-trip");
    let root = root(&directory);
    let contents = [
        "one\ntwo\n",
        "one\ntwo",
        "one\n",
        "one",
        "\n",
        "",
        "one\r\ntwo\r\n",
        "one\r\ntwo",
    ];
    for (index, content) in contents.iter().enumerate() {
        let path = directory.write(&format!("file{index}.txt"), content);
        let opened = FileRequest::Open(OpenRequest {
            root: Arc::clone(&root),
            path: relative(&directory, &path),
            files: files(),
        })
        .run();
        let FileResult::Opened { outcome, .. } = opened else {
            panic!("an open request returns an open result");
        };
        let file = outcome.expect("the file is a small UTF-8 file");
        assert!(!file.text.is_modified(), "{content:?}");

        let saved = FileRequest::Save(SaveRequest {
            buffer: super::BufferId::new(1),
            target: file.target.clone(),
            content: file::render_content(&file.text),
            version: file.text.version(),
            expected: file.identity,
            snapshot: file.text.clone(),
            files: files(),
        })
        .run();
        let FileResult::Saved { outcome, .. } = saved else {
            panic!("a save request returns a save result");
        };
        outcome.expect("the recorded identity still matches");
        assert_eq!(
            fs::read_to_string(&path).expect("the file exists"),
            *content,
            "the save of {content:?} changed the file"
        );
    }
}

#[test]
fn a_file_without_a_final_line_ending_keeps_that_end_through_an_edit() {
    let mut text = buffer("one\ntwo");
    edit(&mut text, 0, 0, "zero\n");
    assert_eq!(file::render_content(&text), "zero\none\ntwo");

    let mut terminated = buffer("one\ntwo\n");
    edit(&mut terminated, 0, 0, "zero\n");
    assert_eq!(file::render_content(&terminated), "zero\none\ntwo\n");
}

#[test]
fn one_path_reaches_one_buffer() {
    let settings = files();
    let (mut buffers, scratch) = Buffers::new(FileBuffer::scratch(&settings));
    let directory = TempDir::new("buffer-identity");
    let path = directory.write("kvim-example.rs", "one\n");
    let root = root(&directory);
    let loaded = load_path(&root, &directory, &path, &settings).expect("the file is valid");
    let id = buffers
        .insert(FileBuffer::loaded(
            buffer("one\n"),
            loaded.target.clone(),
            loaded.identity,
        ))
        .expect("the list holds room");

    assert_eq!(buffers.find_target(&loaded.target), Some(id));
    assert_ne!(id, scratch);
    assert_eq!(
        buffers.get(id).map(FileBuffer::name),
        Some("kvim-example.rs")
    );

    buffers.remove(id);
    assert_eq!(buffers.find_target(&loaded.target), None);
    assert_eq!(buffers.len(), 1);
}

#[test]
fn an_undo_record_restores_the_history_of_the_saved_file() {
    let mut text = buffer("one\n");
    edit(&mut text, 0, 0, "zero\n");
    edit(&mut text, 9, 9, "two\n");
    let content = text.to_string();
    assert_eq!(content, "zero\none\ntwo\n");

    let record = UndoRecord::capture(&text);
    assert_eq!(record.len(), 2);

    let encoded = record.encode(&content);
    let decoded = UndoRecord::decode(&encoded, &content).expect("the record matches the content");
    assert_eq!(decoded, record);

    let mut restored = decoded
        .restore(&content, &files())
        .expect("the replay reproduces the content");
    assert_eq!(restored.to_string(), content);
    assert!(
        !restored.is_modified(),
        "a restored buffer starts at the saved state"
    );

    assert!(restored.undo().is_some());
    assert_eq!(restored.to_string(), "zero\none\n");
    assert!(restored.undo().is_some());
    assert_eq!(restored.to_string(), "one\n");
    assert!(restored.undo().is_none());
    assert!(restored.is_modified(), "the buffer left the saved state");
}

#[test]
fn a_buffer_without_history_records_no_undo_step() {
    let text = buffer("one\n");
    let record = UndoRecord::capture(&text);
    assert!(record.is_empty());
}

#[test]
fn the_undo_record_bounds_its_step_count() {
    let mut text = buffer("");
    for index in 0..UNDO_FILE_STEPS_MAX + 10 {
        let end = text.len_chars();
        edit(&mut text, end, end, &format!("line {index}\n"));
    }
    assert_eq!(UndoRecord::capture(&text).len(), UNDO_FILE_STEPS_MAX);
}

#[test]
fn every_invalid_undo_file_is_ignored() {
    let mut text = buffer("one\n");
    edit(&mut text, 0, 0, "zero\n");
    let content = text.to_string();
    let encoded = UndoRecord::capture(&text).encode(&content);

    assert!(
        UndoRecord::decode(&encoded, "other content\n").is_none(),
        "another file content invalidates the record"
    );
    assert!(
        UndoRecord::decode(&encoded[..encoded.len() - 1], &content).is_none(),
        "a truncated record is unreadable"
    );
    assert!(
        UndoRecord::decode(&[], &content).is_none(),
        "an empty file holds no header"
    );

    let mut magic = encoded.clone();
    magic[0] = b'X';
    assert!(
        UndoRecord::decode(&magic, &content).is_none(),
        "another magic value is unreadable"
    );

    let mut version = encoded.clone();
    version[8] = version[8].wrapping_add(1);
    assert!(
        UndoRecord::decode(&version, &content).is_none(),
        "another format version is unsupported"
    );

    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(
        UndoRecord::decode(&trailing, &content).is_none(),
        "a record with extra bytes is unreadable"
    );
}

#[test]
fn a_record_that_replays_into_other_text_is_ignored() {
    let mut text = buffer("one\n");
    edit(&mut text, 0, 0, "zero\n");
    let record = UndoRecord::capture(&text);
    assert!(
        record.restore(&text.to_string(), &files()).is_some(),
        "the matching content restores the history"
    );

    // A record whose replay reaches other text must not reach a buffer, even
    // when a caller skips the header check.
    assert!(
        record.restore("zero\none\ntwo\n", &files()).is_none(),
        "the replay must reproduce the file content exactly"
    );
}

#[test]
fn the_undo_file_round_trip_uses_the_filesystem() {
    let directory = TempDir::new("undo");
    let undo_path = directory.join("state").join("buffer.kvu");

    let mut text = buffer("one\n");
    edit(&mut text, 0, 0, "zero\n");
    let content = text.to_string();
    let record = UndoRecord::capture(&text);

    assert!(
        undo_file::read_record(&undo_path, &content).is_none(),
        "a missing undo file is not a failure"
    );

    undo_file::write_record(&undo_path, &record, &content);
    let read = undo_file::read_record(&undo_path, &content).expect("the record matches");
    assert_eq!(read, record);

    fs::write(&undo_path, b"not an undo file").expect("the directory is writable");
    assert!(
        undo_file::read_record(&undo_path, &content).is_none(),
        "an unreadable undo file is not a failure"
    );

    // A buffer without history removes the stale file instead of keeping it.
    undo_file::write_record(&undo_path, &UndoRecord::capture(&buffer("one\n")), &content);
    assert!(!undo_path.exists());
}

#[test]
fn the_undo_file_lives_under_the_state_directory() {
    // The test names its own state directory, so the rule holds on a host
    // without one and the assertions always run.
    let state = Path::new("/state");
    let first = TempDir::new("undo-key-first");
    let first_root = root(&first);
    let first_target = load_path(
        &first_root,
        &first,
        &first.join("kvim-example.rs"),
        &files(),
    )
    .expect("a new target is valid")
    .target;
    let path = undo_file::undo_file_path_in(state, &first_target);

    assert!(path.starts_with(state));
    assert!(path.extension().is_some_and(|value| value == "kvu"));
    assert!(
        path.parent()
            .is_some_and(|parent| parent.ends_with("kvim/undo"))
    );
    assert_ne!(
        undo_file::undo_file_path_in(
            state,
            &load_path(&first_root, &first, &first.join("kvim-other.rs"), &files(),)
                .expect("a new target is valid")
                .target,
        ),
        path,
        "two paths reach two undo files"
    );
}

#[cfg(unix)]
mod confinement {
    use std::fs;
    use std::os::unix::fs::symlink;

    use kvim_path::WorktreeConfinementError;

    use super::{file, files, load_path, relative, root};
    use crate::temp::TempDir;
    use crate::{OpenError, SaveError, undo_file};

    #[test]
    fn contained_links_and_direct_paths_have_one_identity() {
        let directory = TempDir::new("contained-link-identity");
        let target = directory.file("src/main.rs", "fn main() {}\n");
        let link = directory.join("main.rs");
        symlink("src/main.rs", &link).expect("the temporary directory supports links");
        let root = root(&directory);

        let direct = load_path(&root, &directory, &target, &files()).expect("the file is valid");
        let linked = load_path(&root, &directory, &link, &files()).expect("the link is contained");

        assert_eq!(direct.target, linked.target);
        assert_eq!(
            linked.target.relative_path().as_path(),
            relative(&directory, &target).as_path()
        );
    }

    #[test]
    fn escaping_dangling_and_looping_links_have_typed_failures() {
        let directory = TempDir::new("link-failures");
        let outside = TempDir::new("link-failures-outside");
        let outside_file = outside.write("outside.rs", "outside\n");
        symlink(&outside_file, directory.join("escape.rs"))
            .expect("the temporary directory supports links");
        symlink("missing.rs", directory.join("dangling.rs"))
            .expect("the temporary directory supports links");
        symlink("loop-b.rs", directory.join("loop-a.rs"))
            .expect("the temporary directory supports links");
        symlink("loop-a.rs", directory.join("loop-b.rs"))
            .expect("the temporary directory supports links");
        let root = root(&directory);

        assert!(matches!(
            load_path(&root, &directory, &directory.join("escape.rs"), &files()),
            Err(OpenError::Confinement(WorktreeConfinementError::Escape))
        ));
        assert!(matches!(
            load_path(&root, &directory, &directory.join("dangling.rs"), &files()),
            Err(OpenError::Confinement(
                WorktreeConfinementError::DanglingLink
            ))
        ));
        assert!(matches!(
            load_path(&root, &directory, &directory.join("loop-a.rs"), &files()),
            Err(OpenError::Confinement(WorktreeConfinementError::LinkLoop))
        ));
    }

    #[test]
    fn a_new_target_resolves_its_nearest_existing_parent() {
        let directory = TempDir::new("new-target-parent");
        directory.dir("actual/nested");
        symlink("actual", directory.join("alias")).expect("the temporary directory supports links");
        let root = root(&directory);

        let loaded = load_path(
            &root,
            &directory,
            &directory.join("alias/nested/new.rs"),
            &files(),
        )
        .expect("the contained parent link is valid");

        assert_eq!(
            loaded.target.relative_path().as_path(),
            std::path::Path::new("actual/nested/new.rs")
        );
        file::save(&loaded.target, "new\n", None, &files())
            .expect("the validated existing parent accepts the new file");
        assert_eq!(
            fs::read_to_string(directory.join("actual/nested/new.rs"))
                .expect("the new file exists"),
            "new\n"
        );
    }

    #[test]
    fn a_replaced_contained_target_link_changes_no_file() {
        let directory = TempDir::new("replaced-target-link");
        let target = directory.write("target.rs", "original\n");
        let replacement = directory.write("replacement.rs", "replacement\n");
        let root = root(&directory);
        let loaded = load_path(&root, &directory, &target, &files()).expect("the file is valid");
        fs::remove_file(&target).expect("the target can be replaced");
        symlink("replacement.rs", &target).expect("the temporary directory supports links");

        assert!(matches!(
            file::save(&loaded.target, "editor\n", loaded.identity, &files()),
            Err(SaveError::Confinement(WorktreeConfinementError::Replaced))
        ));
        assert_eq!(
            fs::read_to_string(replacement).expect("the replacement remains readable"),
            "replacement\n"
        );
        assert!(
            fs::symlink_metadata(target)
                .expect("the link remains")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn equal_relative_paths_under_two_roots_have_distinct_identity_and_undo_keys() {
        let first = TempDir::new("root-identity-first");
        let second = TempDir::new("root-identity-second");
        let first_path = first.file("src/main.rs", "same\n");
        let second_path = second.file("src/main.rs", "same\n");
        let first_root = root(&first);
        let second_root = root(&second);
        let first_target = load_path(&first_root, &first, &first_path, &files())
            .expect("the first file is valid")
            .target;
        let second_target = load_path(&second_root, &second, &second_path, &files())
            .expect("the second file is valid")
            .target;

        assert_ne!(first_target, second_target);
        let expected_name = {
            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            for byte in first_target.as_path().as_os_str().as_encoded_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            format!("{hash:016x}.kvu")
        };
        assert_eq!(
            undo_file::undo_file_path_in(std::path::Path::new("/state"), &first_target).file_name(),
            Some(std::ffi::OsStr::new(&expected_name)),
            "the undo key hashes the absolute target bytes"
        );
        assert_ne!(
            undo_file::undo_file_path_in(std::path::Path::new("/state"), &first_target),
            undo_file::undo_file_path_in(std::path::Path::new("/state"), &second_target)
        );
    }
}
