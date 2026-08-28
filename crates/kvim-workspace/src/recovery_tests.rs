use std::fs;
use std::path::Path;

use std::sync::Arc;

use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_settings::FileSettings;

use super::*;
use crate::DurableOutcome;
use crate::durable::{FailurePoint, RecoveryAction, inject_failure};
use crate::file;
use crate::temp::TempDir;

const MAX_BYTES: u64 = 128;

fn target(directory: &TempDir, name: &str) -> FileTarget {
    let root = Arc::new(WorktreeRoot::open(&directory.path).expect("the worktree exists"));
    let path = directory.join(name);
    let relative = WorktreeRelativePath::new(
        path.strip_prefix(&directory.path)
            .expect("the target is contained"),
    )
    .expect("the target is relative");
    file::load(&root, &relative, &FileSettings::default())
        .expect("a missing target loads")
        .target
}

fn record(target: &FileTarget) -> RecoveryRecord {
    RecoveryRecord::new(
        target,
        RecoveryBaseline::saved("saved\n"),
        7,
        "recovered\n".to_owned(),
        MAX_BYTES,
        MAX_BYTES,
    )
    .expect("the recovery text fits")
}

#[test]
fn recovery_record_round_trip_is_complete() {
    let directory = TempDir::new("recovery-round-trip");
    let target = target(&directory, "main.rs");
    let record = record(&target);

    assert_eq!(
        RecoveryRecord::decode(&record.encode(), &target, MAX_BYTES, MAX_BYTES),
        Some(record)
    );
}

#[test]
fn malformed_and_truncated_records_are_ignored() {
    let directory = TempDir::new("recovery-malformed");
    let target = target(&directory, "main.rs");
    let record = record(&target);
    let encoded = record.encode();

    assert!(RecoveryRecord::decode(&[], &target, MAX_BYTES, MAX_BYTES).is_none());
    assert!(
        RecoveryRecord::decode(&encoded[..encoded.len() - 1], &target, MAX_BYTES, MAX_BYTES)
            .is_none()
    );
    let mut malformed = encoded;
    malformed[0] = b'X';
    assert!(RecoveryRecord::decode(&malformed, &target, MAX_BYTES, MAX_BYTES).is_none());
}

#[test]
fn oversized_recovered_text_is_rejected() {
    let directory = TempDir::new("recovery-oversized");
    let target = target(&directory, "main.rs");
    assert!(matches!(
        RecoveryRecord::new(
            &target,
            RecoveryBaseline::Missing,
            1,
            "x".repeat((MAX_BYTES + 1) as usize),
            MAX_BYTES,
            MAX_BYTES,
        ),
        Err(RecoveryError::TooLarge { .. })
    ));
}

#[test]
fn target_mismatch_rejects_a_hash_collision_path() {
    let directory = TempDir::new("recovery-target-mismatch");
    let first = target(&directory, "first.rs");
    let second = target(&directory, "second.rs");
    let record = record(&first);

    assert!(
        RecoveryRecord::decode(&record.encode(), &second, MAX_BYTES, MAX_BYTES).is_none(),
        "the complete target prevents a colliding filename from selecting this record"
    );
}

#[test]
fn missing_baseline_round_trips() {
    let directory = TempDir::new("recovery-missing-baseline");
    let target = target(&directory, "main.rs");
    let record = RecoveryRecord::new(
        &target,
        RecoveryBaseline::Missing,
        1,
        "new\n".to_owned(),
        MAX_BYTES,
        MAX_BYTES,
    )
    .expect("the text fits");

    assert!(matches!(
        RecoveryRecord::decode(&record.encode(), &target, MAX_BYTES, MAX_BYTES)
            .expect("the record is valid")
            .baseline(),
        RecoveryBaseline::Missing
    ));
}

#[test]
fn changed_baseline_does_not_match_current_disk_text() {
    let baseline = RecoveryBaseline::saved("saved\n");
    assert!(baseline.matches_disk(Some("saved\n")));
    assert!(!baseline.matches_disk(Some("changed\n")));
    assert!(!baseline.matches_disk(None));
    assert!(!RecoveryBaseline::Missing.matches_disk(Some("new\n")));
    assert!(RecoveryBaseline::Missing.matches_disk(None));
}

#[test]
fn recovery_write_cleanup_failure_is_indeterminate() {
    let directory = TempDir::new("recovery-write-cleanup");
    let target = target(&directory, "main.rs");
    let path = directory.join("state/kvim/recovery/main.kvr");

    inject_failure(FailurePoint::RecoveryWriteAndCleanup);
    let outcome = write_recovery_record(&path, &record(&target));

    let DurableOutcome::Indeterminate(report) = outcome else {
        panic!("a failed temporary cleanup cannot prove the durable state is unchanged");
    };
    assert!(matches!(report.primary(), RecoveryError::Write(_)));
    assert_eq!(report.recovery_failures().len(), 1);
    let cleanup = &report.recovery_failures()[0];
    assert_eq!(cleanup.action(), RecoveryAction::RemoveTemporary);
    assert_eq!(cleanup.path().parent(), path.parent());
    assert!(
        report
            .affected_paths()
            .iter()
            .any(|path| path == cleanup.path()),
        "the indeterminate report names the temporary path that cleanup could not remove"
    );
}

#[test]
fn recovery_rename_cleanup_failure_is_indeterminate() {
    let directory = TempDir::new("recovery-rename-cleanup");
    let target = target(&directory, "main.rs");
    let path = directory.join("state/kvim/recovery/main.kvr");

    inject_failure(FailurePoint::RecoveryRenameAndCleanup);
    let outcome = write_recovery_record(&path, &record(&target));

    let DurableOutcome::Indeterminate(report) = outcome else {
        panic!("a failed temporary cleanup cannot prove the durable state is unchanged");
    };
    assert!(matches!(report.primary(), RecoveryError::Replace(_)));
    assert_eq!(report.recovery_failures().len(), 1);
    let cleanup = &report.recovery_failures()[0];
    assert_eq!(cleanup.action(), RecoveryAction::RemoveTemporary);
    assert_eq!(cleanup.path().parent(), path.parent());
    assert!(
        report
            .affected_paths()
            .iter()
            .any(|path| path == cleanup.path())
    );
}

#[test]
fn interrupted_write_keeps_the_previous_record() {
    let directory = TempDir::new("recovery-interrupted-write");
    let target = target(&directory, "main.rs");
    let path = directory.join("state/kvim/recovery/main.kvr");
    let previous = record(&target);
    assert!(matches!(
        write_recovery_record(&path, &previous),
        DurableOutcome::Committed(())
    ));

    let replacement = RecoveryRecord::new(
        &target,
        RecoveryBaseline::Missing,
        8,
        "replacement\n".to_owned(),
        MAX_BYTES,
        MAX_BYTES,
    )
    .expect("the replacement fits");
    let temporary = path
        .parent()
        .expect("the recovery path has a parent")
        .join(".main.kvr.kvim-interrupted.tmp");
    fs::write(&temporary, replacement.encode()).expect("the temporary record writes");

    assert_eq!(
        read_recovery_record(&path, &target, MAX_BYTES, MAX_BYTES),
        Some(previous),
        "an interrupted temporary write does not replace the current record"
    );
    assert!(
        temporary.exists(),
        "the simulated interrupted temporary remains separate"
    );
}

#[test]
fn recovery_path_uses_the_injected_state_directory() {
    let directory = TempDir::new("recovery-path");
    let target = target(&directory, "main.rs");
    let path = recovery_record_path(Path::new("/state"), &target);
    assert!(path.starts_with("/state"));
    assert!(
        path.parent()
            .is_some_and(|parent| parent.ends_with("kvim/recovery"))
    );
    assert!(path.extension().is_some_and(|extension| extension == "kvr"));
}
