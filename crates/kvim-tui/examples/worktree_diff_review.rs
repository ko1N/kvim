//! Reviews one worktree diff of one temporary repository from end to end.
//!
//! The example is one complete consumer of the review boundary. It needs no
//! checkout of its own, no network, and no terminal. It builds one temporary
//! repository, records one base commit, changes one file, and captures the
//! candidate through the bounded process service of the editor. The terminal
//! event loop of kvim runs the same steps and runs no `git` command itself.
//!
//! The run proves four facts of `docs/git.md`:
//!
//! - the review publishes the exact lines of the candidate and its truncation;
//! - one submission captures the target again before it emits a comment;
//! - one accepted submission emits one comment event with the durable anchor
//!   and the bounded body, and kvim gives that comment no host meaning;
//! - a later candidate relocates the anchor through the pure relocation API.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p kvim-tui --example worktree_diff_review
//! ```

use std::error::Error;
use std::path::Path;
use std::sync::Arc;

use kvim_path::WorktreeRoot;
use kvim_runtime::{
    ProcessOutput, ProcessRequest, PublicationGate, RequestSlot, Runtime, RuntimeLimits,
};
use kvim_workspace::temp::TempRepository;
use kvim_workspace::{
    BaseRevision, CommentBody, DiffSide, DiffTarget, Relocation, ReviewEvent, ReviewRow,
    ReviewState, TargetAuthority, WorktreeDiff, WorktreeDiffRead, WorktreeDiffRequest,
};

/// The file that the example reviews.
const DOCUMENT: &str = "src/main.rs";

/// The exact text that the base commit holds.
const BASE_TEXT: &str = "fn main() {\n    let timeout = 30;\n}\n";

/// The exact text that the reviewed candidate holds.
const REVIEWED_TEXT: &str = "fn main() {\n    let timeout = 90;\n}\n";

/// The exact text that the later candidate holds.
///
/// The change inserts one line above the reviewed line, so the anchored line
/// keeps its bytes and takes another number.
const EDITED_TEXT: &str = "//! One entry point.\nfn main() {\n    let timeout = 90;\n}\n";

/// The exact reviewed line, with its indentation.
///
/// A published line holds the exact bytes of its file, so the search below
/// names the whole line and not a part of it.
const REVIEWED_LINE: &str = "    let timeout = 90;";

/// The comment that the reader submits.
const COMMENT: &str = "the number hides its unit; name it timeout_seconds";

/// The largest number of commands that one complete capture runs.
///
/// One capture runs one base check and, for every attempt, one collection pass
/// between two authority passes. The bound stops the driver below, so no
/// refused capture can loop.
const CAPTURE_COMMANDS_MAX: usize = 64;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let repository = TempRepository::new("worktree-diff-review");
    repository.file(DOCUMENT, BASE_TEXT);
    repository.commit("the review base");
    let base = BaseRevision::new(&repository.head())?;

    // The working tree holds one unstaged change against the base commit.
    repository.file(DOCUMENT, REVIEWED_TEXT);
    let candidate = capture(repository.path(), base, DiffTarget::Worktree).await?;
    println!("captured revision {}", candidate.revision());

    let mut review = ReviewState::new(candidate);
    print_rows(&review);

    // The cursor opens on the first published hunk. The reader selects the new
    // side of the changed line, so the anchor names the line that the worktree
    // holds and not the line that the base commit holds.
    let changed = new_line_of(&review, REVIEWED_LINE)?;
    let anchor = review.select(DiffSide::New, changed, 1)?.clone();
    println!(
        "selected {} line {} of {}",
        match anchor.side() {
            DiffSide::Old => "old",
            DiffSide::New => "new",
        },
        anchor.location().first(),
        anchor.path().as_path().display()
    );

    // The submission never trusts the candidate that the reader saw. The host
    // captures the target again and hands the authority to the review.
    let verified = capture(repository.path(), base, DiffTarget::Worktree).await?;
    review.submit_comment(CommentBody::new(COMMENT)?, &TargetAuthority::of(&verified))?;

    // kvim publishes the comment as one domain-neutral event. It stores no
    // comment and gives the text no host meaning.
    let Some(ReviewEvent::CommentSubmitted { anchor, body }) = review.take_event() else {
        return Err("one accepted submission emits one comment event".into());
    };
    println!(
        "event: comment on line {} of {}: {}",
        anchor.location().first(),
        anchor.path().as_path().display(),
        body.as_str()
    );
    println!("queued events after the drain: {}", review.queued_events());

    // The reader edits the file. One line above the anchored line moves it
    // down, and the pure relocation API finds it again without guessing.
    repository.file(DOCUMENT, EDITED_TEXT);
    let later = capture(repository.path(), base, DiffTarget::Worktree).await?;
    let later_revision = later.revision();
    match review.reload(later) {
        Some(Relocation::Relocated { anchor }) => println!(
            "relocated the anchor to line {} of candidate {}",
            anchor.location().first(),
            anchor.candidate()
        ),
        other => return Err(format!("the edit moves the anchored line: {other:?}").into()),
    }
    println!("the review now holds candidate {later_revision}");

    Ok(())
}

/// Prints every published row of the review.
///
/// Omitted content publishes no line row, so the reader always sees the bound
/// that stopped a collection instead of missing content without a notice.
fn print_rows(review: &ReviewState) {
    for row in review.rows() {
        match row {
            ReviewRow::File { file } => {
                println!("file {}", file.path().as_path().display());
            }
            ReviewRow::Hunk { hunk, .. } => {
                println!(
                    "  hunk {} covers {} old and {} new lines",
                    hunk.id().get(),
                    hunk.old_range().count(),
                    hunk.new_range().count()
                );
            }
            ReviewRow::Line { line, .. } => {
                let text = line.text().as_str().unwrap_or("<not text>");
                match (line.number(DiffSide::Old), line.number(DiffSide::New)) {
                    (Some(old), None) => println!("    -{old:>4} {text}"),
                    (None, Some(new)) => println!("    +{new:>4} {text}"),
                    (old, new) => println!("     {:>4} {text}", new.or(old).unwrap_or(0)),
                }
            }
            ReviewRow::Truncated { limit } => {
                println!("  content is missing: the {limit:?} bound stopped the collection");
            }
        }
    }
}

/// Returns the new-side number of the first published line with one text.
fn new_line_of(review: &ReviewState, text: &str) -> Result<u32, Box<dyn Error>> {
    review
        .rows()
        .find_map(|row| match row {
            ReviewRow::Line { line, .. } if line.text().as_str() == Some(text) => {
                line.number(DiffSide::New)
            }
            _ => None,
        })
        .ok_or_else(|| format!("the candidate publishes the changed line {text:?}").into())
}

/// Captures one worktree diff through the bounded process service.
///
/// The capture builds one command at a time. The example submits it, hands the
/// captured output back, and repeats until the capture publishes one consistent
/// candidate or returns a typed failure.
async fn capture(
    root: &Path,
    base: BaseRevision,
    target: DiffTarget,
) -> Result<WorktreeDiff, Box<dyn Error>> {
    let root = Arc::new(WorktreeRoot::open(root)?);
    let mut request = WorktreeDiffRequest::new(root, base, target);
    for _ in 0..CAPTURE_COMMANDS_MAX {
        let output = run(request.command()).await;
        // The capture classifies every refusal into one typed outcome, so this
        // boundary reports the variant and reads no message text.
        match request.publish(&output) {
            Ok(WorktreeDiffRead::Pending(next)) => request = *next,
            Ok(WorktreeDiffRead::Published(candidate)) => return Ok(*candidate),
            Err(failure) => return Err(format!("the capture returned {failure:?}").into()),
        }
    }
    Err(format!("one capture stays inside {CAPTURE_COMMANDS_MAX} commands").into())
}

/// Runs one bounded command through the process service of the editor.
async fn run(command: ProcessRequest) -> ProcessOutput {
    let limits = RuntimeLimits::new(1, 1, 1).expect("every capacity is nonzero");
    let (runtime, mut events) = Runtime::<ProcessOutput>::with_limits(limits);
    let handle =
        PublicationGate::default().begin(RequestSlot::new(1), &runtime.cancellation_root());
    runtime
        .submit_process(handle, command, |output| output)
        .expect("the isolated runtime holds one free permit");
    let event = events
        .recv()
        .await
        .expect("every accepted request produces one result");
    let output = event.result.expect("the host provides the git command");
    runtime.shutdown().await;
    output
}
