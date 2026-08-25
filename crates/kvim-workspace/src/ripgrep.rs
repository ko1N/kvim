//! The bounded ripgrep search of the search picker.
//!
//! The editor never runs `rg` itself. It builds one [`ProcessRequest`], the
//! bounded process service runs it, and [`parse_matches`] turns the captured
//! output into candidates. The parser is pure and defensive: a malformed line
//! is dropped, never a panic. See `docs/files.md`.

use std::str;
use std::time::Duration;

use kvim_path::{ResolvedTargetState, WorktreeRelativePath, WorktreeRoot};
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
/// use kvim_path::WorktreeRoot;
/// use kvim_workspace::{RIPGREP_PROGRAM, ripgrep_command};
///
/// let root = WorktreeRoot::open(std::env::current_dir()?)?;
/// let command = ripgrep_command(&root, "needle");
/// assert_eq!(command.program, RIPGREP_PROGRAM);
/// assert_eq!(command.current_dir.as_deref(), Some(root.as_path()));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn ripgrep_command(root: &WorktreeRoot, query: &str) -> ProcessRequest {
    let mut request = ProcessRequest::new(RIPGREP_PROGRAM);
    request.args = vec![
        "--line-number".into(),
        "--column".into(),
        "--no-heading".into(),
        "--color=never".into(),
        "--smart-case".into(),
        "--no-config".into(),
        "--no-follow".into(),
        "--no-ignore-parent".into(),
        "--no-ignore-global".into(),
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
    request.current_dir = Some(root.as_path().to_path_buf());
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
/// use std::fs;
///
/// use kvim_path::WorktreeRoot;
/// use kvim_workspace::parse_matches;
///
/// // The parser keeps only a match that names an existing file under the
/// // root, so the example owns one small worktree of its own.
/// let directory = std::env::temp_dir()
///     .join(format!("kvim-doc-parse-matches-{}", std::process::id()));
/// fs::create_dir_all(directory.join("src"))?;
/// fs::write(directory.join("src").join("main.rs"), "let value = 1;\n")?;
/// let root = WorktreeRoot::open(&directory)?;
///
/// let output = b"./src/main.rs:12:5:let value = 1;\nbroken line\n";
/// let (candidates, truncated) = parse_matches(&root, output);
/// assert_eq!(candidates.len(), 1, "the malformed line is dropped");
/// assert_eq!(candidates[0].row(), "main.rs:12  src  let value = 1;");
/// assert!(!truncated);
/// # fs::remove_dir_all(&directory)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn parse_matches(root: &WorktreeRoot, stdout: &[u8]) -> (Vec<Candidate>, bool) {
    let mut candidates = Vec::new();
    let mut truncated = stdout.len() >= RIPGREP_OUTPUT_BYTES_MAX;
    for line in stdout.split(|byte| *byte == b'\n') {
        // A line that the output limit cut in half holds no complete record,
        // and a line that is not UTF-8 names no path that the editor can open.
        let Ok(line) = str::from_utf8(line) else {
            continue;
        };
        let candidate = match parse_line(root, line) {
            Ok(Some(candidate)) => candidate,
            Ok(None) => continue,
            Err(()) => {
                truncated = true;
                continue;
            }
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
fn parse_line(root: &WorktreeRoot, line: &str) -> Result<Option<Candidate>, ()> {
    let mut fields = line.splitn(RESULT_FIELDS, ':');
    let Some(path) = fields.next() else {
        return Ok(None);
    };
    let Some(number) = fields.next() else {
        return Ok(None);
    };
    let Some(column) = fields.next() else {
        return Ok(None);
    };
    let Some(text) = fields.next() else {
        return Ok(None);
    };
    if path.is_empty() {
        return Ok(None);
    }
    // `rg` counts lines and columns from one, and the editor counts from zero.
    let Some(number) = number
        .parse::<usize>()
        .ok()
        .and_then(|value| value.checked_sub(1))
    else {
        return Ok(None);
    };
    let Some(column) = column
        .parse::<usize>()
        .ok()
        .and_then(|value| value.checked_sub(1))
    else {
        return Ok(None);
    };
    let relative = path.strip_prefix(CURRENT_DIRECTORY_PREFIX).unwrap_or(path);
    let relative = WorktreeRelativePath::new(relative).map_err(|_| ())?;
    let resolved = root.resolve(&relative).map_err(|_| ())?;
    if resolved.state() != ResolvedTargetState::Existing {
        return Err(());
    }
    let observed = root
        .directory()
        .metadata(resolved.path().as_path())
        .map_err(|_| ())?;
    if !observed.is_file() {
        return Err(());
    }
    let observed = metadata_identity(&observed);
    root.revalidate(&relative, &resolved).map_err(|_| ())?;
    let current = root
        .directory()
        .metadata(resolved.path().as_path())
        .map_err(|_| ())?;
    if metadata_identity(&current) != observed {
        return Err(());
    }
    Ok(Some(Candidate::matched(
        root,
        resolved.path().clone(),
        number,
        column,
        text,
    )))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MetadataIdentity {
    device: u64,
    inode: u64,
}

fn metadata_identity(metadata: &cap_std::fs::Metadata) -> MetadataIdentity {
    use cap_std::fs::MetadataExt as _;

    MetadataIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(test)]
#[path = "ripgrep_tests.rs"]
mod tests;
