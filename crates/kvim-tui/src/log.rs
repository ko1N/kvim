//! The bounded log of every report that the editor made.
//!
//! The message line shows one message, and a second message replaces it. The
//! log keeps the replaced message, so the user still reads it after the message
//! line moved on. The log is a history, never a replacement of the message
//! line. See `docs/windows.md`.
//!
//! The module is a pure value. It reads no clock, so the caller passes the
//! elapsed time that the event loop reported. See `docs/responsiveness.md`.

use std::collections::VecDeque;
use std::time::Duration;

use super::session::{MESSAGE_CHARS_MAX, MessageLevel};

/// The largest number of entries that the editor log keeps.
///
/// A reader opens the log after one failure and looks for the reports around
/// it. This number holds every report of a normal session, and it still holds
/// several groups of reports from a component that fails and starts again. That
/// case is the one that a reader opens the log for, and its first report usually
/// names the cause. See `docs/windows.md`.
pub(super) const LOG_ENTRIES_MAX: usize = 256;

/// The largest text that one log entry keeps, in characters.
///
/// The bound keeps one long report from filling the log. The message line clips
/// at the same number of characters, so an entry of that source loses no
/// character that the message line showed.
pub(super) const LOG_ENTRY_CHARS_MAX: usize = MESSAGE_CHARS_MAX;

/// The name of the buffer that holds one snapshot of the log.
pub(super) const LOG_BUFFER_NAME: &str = "[Log]";

/// The part of the editor that made one report.
///
/// A later source adds one variant and one label. It adds no second store and
/// no second entry shape. See `docs/windows.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LogSource {
    /// The report reached the message line.
    MessageLine,
}

impl LogSource {
    /// Returns the field that names the source in the log buffer.
    const fn label(self) -> &'static str {
        match self {
            Self::MessageLine => "MESSAGE",
        }
    }
}

impl MessageLevel {
    /// Returns the field that names the severity in the log buffer.
    ///
    /// The label lives beside the entry format, so the whole row shape stays in
    /// one file.
    const fn log_label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warning => "WARN",
            Self::Info => "INFO",
        }
    }
}

/// One recorded editor report.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LogEntry {
    /// The elapsed time since the editor started.
    at: Duration,
    /// The severity of the report.
    level: MessageLevel,
    /// The part of the editor that made the report.
    source: LogSource,
    /// The text of the report, as one line of at most
    /// [`LOG_ENTRY_CHARS_MAX`] characters.
    text: String,
}

impl LogEntry {
    /// Creates one entry from a report of any length and any shape.
    fn new(at: Duration, source: LogSource, level: MessageLevel, text: &str) -> Self {
        Self {
            at,
            level,
            source,
            text: one_bounded_line(text),
        }
    }

    /// Returns the entry as one row of the log buffer.
    fn row(&self) -> String {
        let seconds = self.at.as_secs();
        format!(
            "{:02}:{:02}.{:03} {:<5} {:<7} {}",
            seconds / 60,
            seconds % 60,
            self.at.subsec_millis(),
            self.level.log_label(),
            self.source.label(),
            self.text
        )
    }
}

/// Returns `text` as one line of at most [`LOG_ENTRY_CHARS_MAX`] characters.
///
/// A report may hold a line break or a tab. One entry is one row of the log
/// buffer, so every control character becomes one blank and a search reaches
/// every entry.
fn one_bounded_line(text: &str) -> String {
    text.chars()
        .take(LOG_ENTRY_CHARS_MAX)
        .map(|value| if value.is_control() { ' ' } else { value })
        .collect()
}

/// The bounded log of the editor reports, oldest entry first.
#[derive(Debug, Default)]
pub(super) struct EditorLog {
    entries: VecDeque<LogEntry>,
}

impl EditorLog {
    /// Records one report.
    ///
    /// The `at` is the elapsed time that the event loop reported last. A log
    /// that already holds [`LOG_ENTRIES_MAX`] entries drops its oldest entry
    /// first, so the newest reports always survive.
    pub(super) fn record(
        &mut self,
        at: Duration,
        source: LogSource,
        level: MessageLevel,
        text: &str,
    ) {
        debug_assert!(
            self.entries.len() <= LOG_ENTRIES_MAX,
            "every earlier record left the log inside its bound"
        );
        if self.entries.len() >= LOG_ENTRIES_MAX {
            self.entries.pop_front();
        }
        self.entries
            .push_back(LogEntry::new(at, source, level, text));
    }

    /// Returns one snapshot of the log as buffer text, newest entry last.
    ///
    /// The snapshot is a value. A later report changes no snapshot that a
    /// buffer already holds, and an edit of that buffer changes no entry. An
    /// empty log returns an empty text, because the editor reported nothing.
    pub(super) fn snapshot(&self) -> String {
        let mut text = String::new();
        for entry in &self.entries {
            text.push_str(&entry.row());
            text.push('\n');
        }
        text
    }
}

#[cfg(test)]
mod tests {
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
}
