//! The bounded ripgrep search of the search picker.
//!
//! The editor never runs `rg` itself. It builds one [`ProcessRequest`], the
//! bounded process service runs it, and [`parse_matches`] turns the captured
//! output into candidates. The parser is pure and defensive: a malformed line
//! is dropped, never a panic. See `docs/files.md`.

use std::path::Path;
use std::str;
use std::time::Duration;

use kvim_runtime::ProcessRequest;

use super::picker::{Candidate, PICKER_MATCH_CHARS_MAX};

/// The external command that the search picker runs.
pub const RIPGREP_PROGRAM: &str = "rg";

/// The largest number of matches that one search keeps.
pub const RIPGREP_MATCHES_MAX: usize = 1024;

/// The largest number of matches that one search keeps for one file.
pub const RIPGREP_FILE_MATCHES_MAX: usize = 32;

/// The largest output that one search captures, in bytes.
pub const RIPGREP_OUTPUT_BYTES_MAX: usize = 1024 * 1024;

/// The deadline of one search.
pub const RIPGREP_DEADLINE: Duration = Duration::from_secs(5);

/// The largest number of characters that one matched line keeps.
///
/// The value also reaches `rg`, so a minified file sends no long line at all.
pub const RIPGREP_COLUMNS_MAX: usize = PICKER_MATCH_CHARS_MAX;

/// The number of fields that one result line holds.
const RESULT_FIELDS: usize = 4;

/// The prefix that `rg` writes before every path of the current directory.
const CURRENT_DIRECTORY_PREFIX: &str = "./";

/// Returns the bounded command of one search.
///
/// The command searches the workspace root and never reads standard input, so
/// the piped input of the process service cannot become its search target.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_workspace::{RIPGREP_PROGRAM, ripgrep_command};
///
/// let command = ripgrep_command(Path::new("/workspace"), "needle");
/// assert_eq!(command.program, RIPGREP_PROGRAM);
/// assert_eq!(command.current_dir.as_deref(), Some(Path::new("/workspace")));
/// ```
#[must_use]
pub fn ripgrep_command(root: &Path, query: &str) -> ProcessRequest {
    let mut request = ProcessRequest::new(RIPGREP_PROGRAM);
    request.args = vec![
        "--line-number".into(),
        "--column".into(),
        "--no-heading".into(),
        "--color=never".into(),
        "--smart-case".into(),
        format!("--max-columns={RIPGREP_COLUMNS_MAX}").into(),
        format!("--max-count={RIPGREP_FILE_MATCHES_MAX}").into(),
        // The pattern follows an explicit flag, so a query that starts with a
        // hyphen stays a pattern.
        "--regexp".into(),
        query.into(),
        // The path is explicit, because `rg` reads standard input when it
        // receives none, and the process service always pipes standard input.
        ".".into(),
    ];
    request.current_dir = Some(root.to_path_buf());
    request.output_bytes_max = RIPGREP_OUTPUT_BYTES_MAX;
    request.deadline = RIPGREP_DEADLINE;
    request
}

/// Turns the captured output of one search into candidates.
///
/// One result line holds the path, the line, the column, and the matched text,
/// separated by colons. The parser drops every line that does not hold those
/// four fields, every line that names line zero or column zero, and the last
/// line when the output stopped inside it. It keeps at most
/// [`RIPGREP_MATCHES_MAX`] matches and reports the truncation.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_workspace::parse_matches;
///
/// let output = b"./src/main.rs:12:5:let value = 1;\nbroken line\n";
/// let (candidates, truncated) = parse_matches(Path::new("/workspace"), output);
/// assert_eq!(candidates.len(), 1, "the malformed line is dropped");
/// assert_eq!(candidates[0].row(), "main.rs:12  src  let value = 1;");
/// assert!(!truncated);
/// ```
#[must_use]
pub fn parse_matches(root: &Path, stdout: &[u8]) -> (Vec<Candidate>, bool) {
    let mut candidates = Vec::new();
    let mut truncated = false;
    for line in stdout.split(|byte| *byte == b'\n') {
        // A line that the output limit cut in half holds no complete record,
        // and a line that is not UTF-8 names no path that the editor can open.
        let Ok(line) = str::from_utf8(line) else {
            continue;
        };
        let Some(candidate) = parse_line(root, line) else {
            continue;
        };
        if candidates.len() >= RIPGREP_MATCHES_MAX {
            truncated = true;
            break;
        }
        candidates.push(candidate);
    }
    (candidates, truncated)
}

/// Returns the candidate of one result line, or `None` for a malformed line.
fn parse_line(root: &Path, line: &str) -> Option<Candidate> {
    let mut fields = line.splitn(RESULT_FIELDS, ':');
    let path = fields.next()?;
    let number = fields.next()?;
    let column = fields.next()?;
    let text = fields.next()?;
    if path.is_empty() {
        return None;
    }
    // `rg` counts lines and columns from one, and the editor counts from zero.
    let number = number.parse::<usize>().ok()?.checked_sub(1)?;
    let column = column.parse::<usize>().ok()?.checked_sub(1)?;
    let relative = path.strip_prefix(CURRENT_DIRECTORY_PREFIX).unwrap_or(path);
    Some(Candidate::matched(
        root,
        root.join(relative),
        number,
        column,
        text,
    ))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{RIPGREP_MATCHES_MAX, parse_matches, ripgrep_command};

    /// The workspace root of every test.
    fn root() -> PathBuf {
        PathBuf::from("/workspace")
    }

    fn rows(output: &[u8]) -> Vec<String> {
        parse_matches(&root(), output)
            .0
            .iter()
            .map(super::Candidate::row)
            .collect()
    }

    #[test]
    fn one_result_line_becomes_one_candidate() {
        assert_eq!(
            rows(b"./src/main.rs:12:5:let value = 1;\n"),
            vec!["main.rs:12  src  let value = 1;"]
        );
    }

    #[test]
    fn a_malformed_line_is_dropped_without_a_panic() {
        let output = concat!(
            "no-colons-at-all\n",
            "./a.rs:not-a-number:1:text\n",
            "./a.rs:3:not-a-number:text\n",
            "./a.rs:0:1:a line number starts at one\n",
            "./a.rs:1:0:a column starts at one\n",
            ":4:1:the path is empty\n",
            "./a.rs:5:1\n",
            "\n",
            "./a.rs:6:1:the only valid line\n",
        );
        assert_eq!(
            rows(output.as_bytes()),
            vec!["a.rs:6  the only valid line"],
            "every malformed line is dropped"
        );
    }

    #[test]
    fn a_truncated_last_line_is_dropped() {
        // The output limit can stop inside one line, so the last record may
        // hold no column and no text.
        assert_eq!(
            rows(b"./a.rs:1:1:complete\n./b.rs:2:"),
            vec!["a.rs:1  complete"]
        );
    }

    #[test]
    fn a_text_that_holds_colons_stays_one_field() {
        assert_eq!(
            rows(b"./a.rs:1:1:let map: Map<K, V> = Map::new();\n"),
            vec!["a.rs:1  let map: Map<K, V> = Map::new();"]
        );
    }

    #[test]
    fn the_match_list_stops_at_the_result_bound() {
        let mut output = String::new();
        for index in 1..=RIPGREP_MATCHES_MAX + 8 {
            output.push_str(&format!("./a.rs:{index}:1:line\n"));
        }
        let (candidates, truncated) = parse_matches(&root(), output.as_bytes());
        assert_eq!(candidates.len(), RIPGREP_MATCHES_MAX);
        assert!(truncated);
    }

    #[test]
    fn the_command_never_reads_standard_input() {
        let command = ripgrep_command(Path::new("/workspace"), "-needle");
        let args: Vec<String> = command
            .args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&".".to_owned()), "the search names its path");
        let pattern = args
            .iter()
            .position(|value| value == "--regexp")
            .map(|index| args[index + 1].clone());
        assert_eq!(
            pattern,
            Some("-needle".to_owned()),
            "a query that starts with a hyphen stays a pattern"
        );
        assert!(command.stdin.is_empty());
    }
}
