use std::error::Error;

use kvim_embed::{
    ReviewCandidate, ReviewCandidateId, ReviewCommand, ReviewCommentBody, ReviewConfig, ReviewEvent,
    ReviewFile, ReviewFileChange, ReviewHunk, ReviewInput, ReviewLine, ReviewLineOrigin,
    ReviewSection, ReviewSurface,
};
use kvim_path::WorktreeRelativePath;
use ratatui::{buffer::Buffer, layout::Rect};

fn main() -> Result<(), Box<dyn Error>> {
    let line = ReviewLine::new(ReviewLineOrigin::Added { new: 1 }, "supplied")?;
    let hunk = ReviewHunk::new(1, 0, 1, 1, &[line])?;
    let file = ReviewFile::new(
        WorktreeRelativePath::new("src/lib.rs")?,
        ReviewFileChange::Added,
        &[hunk],
    )?;
    let candidate = ReviewCandidate::new(
        ReviewCandidateId::new("host-candidate")?,
        ReviewSection::Unstaged,
        &[file],
    )?;
    let area = Rect::new(0, 0, 60, 12);
    let mut review = ReviewSurface::from_candidates(&[candidate], ReviewConfig::new(area))?;
    review.input(ReviewInput::command(ReviewCommand::MarkRead))?;
    review.input(ReviewInput::command(ReviewCommand::SubmitComment(
        ReviewCommentBody::new("host-owned comment")?,
    )))?;
    assert!(matches!(
        review.event(),
        Some(ReviewEvent::ReadStateChanged)
    ));
    assert!(matches!(review.event(), Some(ReviewEvent::Redraw)));
    assert!(matches!(
        review.event(),
        Some(ReviewEvent::CommentSubmitted { .. })
    ));
    review.render(&mut Buffer::empty(area))?;
    let before = review.snapshot();
    assert_eq!(before.anchor_count(), 2);

    let replacement_line = ReviewLine::new(ReviewLineOrigin::Added { new: 1 }, "reloaded")?;
    let replacement_hunk = ReviewHunk::new(1, 0, 1, 1, &[replacement_line])?;
    let replacement_file = ReviewFile::new(
        WorktreeRelativePath::new("src/lib.rs")?,
        ReviewFileChange::Added,
        &[replacement_hunk],
    )?;
    let replacement = ReviewCandidate::new(
        ReviewCandidateId::new("replacement")?,
        ReviewSection::Unstaged,
        &[replacement_file],
    )?;
    review.reload(&[replacement])?;
    assert!(matches!(
        review.event(),
        Some(ReviewEvent::ReplacedCandidate)
    ));
    review.render(&mut Buffer::empty(area))?;
    Ok(())
}
