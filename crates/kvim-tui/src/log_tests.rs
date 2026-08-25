use std::time::Duration;

use super::super::session::MessageLevel;
use super::{EditorLog, LOG_ENTRIES_MAX, LOG_ENTRY_CHARS_MAX, LogSource};

/// Records one message-line report at a whole number of seconds.
fn record(log: &mut EditorLog, seconds: u64, text: &str) {
    log.record(
        Duration::from_secs(seconds),
        LogSource::MessageLine,
        MessageLevel::Info,
        text,
    );
}

/// Records one background-job outcome at a whole number of seconds.
fn record_job(log: &mut EditorLog, seconds: u64, text: &str) {
    log.record(
        Duration::from_secs(seconds),
        LogSource::BackgroundJob,
        MessageLevel::Info,
        text,
    );
}

#[test]
fn one_entry_names_its_time_its_severity_and_its_source() {
    let mut log = EditorLog::default();
    log.record(
        Duration::from_millis(72_345),
        LogSource::MessageLine,
        MessageLevel::Error,
        "the file does not exist",
    );
    log.record(
        Duration::from_millis(73_001),
        LogSource::MessageLine,
        MessageLevel::Info,
        "\"main.rs\" 42L, 900B",
    );
    assert_eq!(
        log.snapshot(),
        concat!(
            "01:12.345 ERROR MESSAGE the file does not exist\n",
            "01:13.001 INFO  MESSAGE \"main.rs\" 42L, 900B\n"
        ),
        "the newest entry is the last row"
    );
}

#[test]
fn the_log_drops_the_oldest_entry_at_its_bound() {
    let mut log = EditorLog::default();
    for index in 0..LOG_ENTRIES_MAX + 8 {
        record(&mut log, 0, &format!("report {index}"));
    }
    let snapshot = log.snapshot();
    let rows: Vec<&str> = snapshot.lines().collect();
    assert_eq!(rows.len(), LOG_ENTRIES_MAX, "the log stays at its bound");
    assert!(
        rows[0].ends_with("report 8"),
        "the first eight reports left the log, but the first row is {:?}",
        rows[0]
    );
    assert!(
        rows[LOG_ENTRIES_MAX - 1].ends_with(&format!("report {}", LOG_ENTRIES_MAX + 7)),
        "the newest report is the last row"
    );
}

#[test]
fn one_entry_stays_one_bounded_line() {
    let mut log = EditorLog::default();
    record(&mut log, 0, "first\nsecond\tthird\r\n");
    record(&mut log, 0, &"x".repeat(LOG_ENTRY_CHARS_MAX + 64));
    let snapshot = log.snapshot();
    let rows: Vec<&str> = snapshot.lines().collect();
    assert_eq!(rows.len(), 2, "one report is one row");
    assert!(
        rows[0].ends_with("first second third  "),
        "every control character becomes one blank, but the row is {:?}",
        rows[0]
    );
    assert_eq!(
        rows[1].chars().filter(|value| *value == 'x').count(),
        LOG_ENTRY_CHARS_MAX,
        "one long report is clipped to the entry bound"
    );
}

#[test]
fn a_repeated_report_collapses_into_one_entry_with_a_count() {
    let mut log = EditorLog::default();
    record_job(&mut log, 12, "analysis rejected: the buffer changed");
    for second in 13..16 {
        record_job(&mut log, second, "analysis rejected: the buffer changed");
    }
    assert_eq!(
        log.snapshot(),
        "00:12.000 INFO  JOB     analysis rejected: the buffer changed (x4)\n",
        "four reports of one outcome cost one entry that keeps its first time"
    );
}

#[test]
fn another_report_ends_one_collapsed_group() {
    let mut log = EditorLog::default();
    record_job(&mut log, 1, "walk was cancelled");
    record_job(&mut log, 2, "walk was cancelled");
    record(&mut log, 3, "written");
    record_job(&mut log, 4, "walk was cancelled");
    let snapshot = log.snapshot();
    let rows: Vec<&str> = snapshot.lines().collect();
    assert_eq!(
        rows.len(),
        3,
        "the message separates two groups in {rows:?}"
    );
    assert!(
        rows[0].ends_with("walk was cancelled (x2)"),
        "{:?}",
        rows[0]
    );
    assert!(rows[1].ends_with("written"), "{:?}", rows[1]);
    assert!(
        rows[2].ends_with("walk was cancelled"),
        "a later repeat starts a new entry with no count, not {:?}",
        rows[2]
    );
}

#[test]
fn one_report_before_a_burst_survives_the_burst() {
    let mut log = EditorLog::default();
    log.record(
        Duration::from_secs(1),
        LogSource::LanguageServer,
        MessageLevel::Error,
        "rust/rust-analyzer failed: no such file or directory",
    );

    // A fast typist rejects one obsolete analysis for every keystroke.
    for second in 2..2 + (LOG_ENTRIES_MAX as u64 * 8) {
        record_job(&mut log, second, "analysis rejected: the buffer changed");
    }

    let snapshot = log.snapshot();
    let rows: Vec<&str> = snapshot.lines().collect();
    assert_eq!(
        rows.len(),
        2,
        "the burst costs one entry, not {}",
        rows.len()
    );
    assert!(
        rows[0].contains("rust/rust-analyzer failed"),
        "the report that names the cause survives the burst, but the first row is {:?}",
        rows[0]
    );
    assert!(
        rows[1].ends_with(&format!(" (x{})", LOG_ENTRIES_MAX * 8)),
        "one entry counts every report of the burst, not {:?}",
        rows[1]
    );
}
