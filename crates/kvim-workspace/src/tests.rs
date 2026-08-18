//! Tests for buffer identity, file loading, the atomic save, and the undo file.
//!
//! Every test runs against one temporary directory that the test removes when
//! it finishes. No test reads or writes the editor state directory of the user.

use std::fs;
use std::path::{Path, PathBuf};

use kvim_core::{CharRange, EditTransaction, TextBuffer, TextChange};
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

    let loaded = file::load(&path, &files()).expect("the file is a small UTF-8 file");
    assert_eq!(loaded.text, "fn main() {}\n");
    let identity = loaded.identity.expect("the file exists");
    assert_eq!(identity.len_bytes, 13);
}

#[test]
fn a_missing_path_loads_an_empty_buffer_without_an_identity() {
    let directory = TempDir::new("missing");
    let loaded =
        file::load(&directory.join("new.rs"), &files()).expect("a missing path is not a failure");
    assert_eq!(loaded.text, "");
    assert!(
        loaded.identity.is_none(),
        "the first save of a new file must not report a conflict"
    );
}

#[test]
fn a_load_rejects_every_unsupported_file() {
    let directory = TempDir::new("reject");
    assert!(matches!(
        file::load(&directory.path, &files()),
        Err(OpenError::Directory)
    ));

    let binary = directory.join("binary.rs");
    fs::write(&binary, [0x66, 0x6e, 0xff, 0x0a]).expect("the directory is writable");
    assert!(matches!(
        file::load(&binary, &files()),
        Err(OpenError::NotUtf8 { valid_up_to: 2 })
    ));

    let mut small = files();
    small.max_file_bytes = 4;
    let large = directory.write("large.rs", "123456");
    assert!(matches!(
        file::load(&large, &small),
        Err(OpenError::TooLarge {
            bytes: 6,
            max_bytes: 4
        })
    ));

    // A device file is a supported path on macOS and Linux, and no editor
    // buffer may hold it.
    let device = Path::new("/dev/null");
    if device.exists() {
        assert!(matches!(
            file::load(device, &files()),
            Err(OpenError::UnsupportedKind)
        ));
    }
}

#[test]
fn a_save_replaces_the_file_and_keeps_its_permissions() {
    let directory = TempDir::new("save");
    let path = directory.write("main.rs", "old\n");
    let loaded = file::load(&path, &files()).expect("the file is small");

    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&path, Permissions::from_mode(0o640)).expect("the file is owned");
    }

    let saved = file::save(&path, "new content\n", loaded.identity, &files())
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
    let loaded = file::load(&path, &files()).expect("the file is small");
    file::save(&path, "new\n", loaded.identity, &files()).expect("the save succeeds");

    let entries: Vec<PathBuf> = fs::read_dir(&directory.path)
        .expect("the directory exists")
        .map(|entry| entry.expect("the entry is readable").path())
        .collect();
    assert_eq!(entries.len(), 1, "the staged replacement removes its file");
}

#[test]
fn an_external_change_becomes_a_conflict_instead_of_an_overwrite() {
    let directory = TempDir::new("conflict");
    let path = directory.write("main.rs", "one\n");
    let loaded = file::load(&path, &files()).expect("the file is small");

    fs::write(&path, "another program wrote this\n").expect("the file is writable");
    assert!(matches!(
        file::save(&path, "kvim wrote this\n", loaded.identity, &files()),
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
    let loaded = file::load(&path, &files()).expect("a missing path is not a failure");
    assert!(loaded.identity.is_none());

    fs::write(&path, "another program created this\n").expect("the directory is writable");
    assert!(matches!(
        file::save(&path, "kvim wrote this\n", loaded.identity, &files()),
        Err(SaveError::Conflict)
    ));
}

#[test]
fn a_failed_save_keeps_the_original_file() {
    let directory = TempDir::new("failure");
    let blocker = directory.write("blocker", "not a directory\n");
    // The parent of the target is a file, so no write can succeed.
    let target = blocker.join("main.rs");

    let failure = file::save(&target, "content\n", None, &files());
    assert!(matches!(failure, Err(SaveError::Write(_))));
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

    let loaded = file::load(&link, &files()).expect("the target is a small file");
    let saved = file::save(&link, "new\n", loaded.identity, &files()).expect("the save succeeds");

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
        saved.path,
        fs::canonicalize(&target).expect("the target exists")
    );
}

#[test]
fn an_open_request_builds_one_buffer_and_a_save_request_writes_it() {
    let directory = TempDir::new("request");
    let path = directory.write("main.rs", "fn main() {}\n");

    let opened = FileRequest::Open(OpenRequest {
        path: path.clone(),
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
        path: file.path.clone(),
        content: file.text.to_string(),
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
            path: path.clone(),
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
            path: file.path.clone(),
            content: file::render_content(&file.text),
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
    let path = PathBuf::from("/tmp/kvim-example.rs");
    let id = buffers
        .insert(FileBuffer::loaded(buffer("one\n"), path.clone(), None))
        .expect("the list holds room");

    assert_eq!(buffers.find_path(&path), Some(id));
    assert_ne!(id, scratch);
    assert_eq!(
        buffers.get(id).map(FileBuffer::name),
        Some("kvim-example.rs")
    );

    buffers.remove(id);
    assert_eq!(buffers.find_path(&path), None);
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
    let path = undo_file::undo_file_path_in(state, Path::new("/tmp/kvim-example.rs"));

    assert!(path.starts_with(state));
    assert!(path.extension().is_some_and(|value| value == "kvu"));
    assert!(
        path.parent()
            .is_some_and(|parent| parent.ends_with("kvim/undo"))
    );
    assert_ne!(
        undo_file::undo_file_path_in(state, Path::new("/tmp/kvim-other.rs")),
        path,
        "two paths reach two undo files"
    );
}
