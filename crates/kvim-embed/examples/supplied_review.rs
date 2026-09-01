use kvim_embed::{
    DiffLimit, DiffTruncation, ReviewCandidate, ReviewCandidateId, ReviewCommand, ReviewConfig,
    ReviewFile, ReviewFileChange, ReviewHunk, ReviewInput, ReviewLine, ReviewLineOrigin,
    ReviewSection, ReviewSurface,
};
use ratatui::{buffer::Buffer, layout::Rect};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let lines = vec![ReviewLine::new(
        ReviewLineOrigin::Added { new: 1 },
        "host supplied this line",
    )?];
    let hunk = ReviewHunk::new(1, 0, 1, 1, &lines)?;
    let file = ReviewFile::with_truncation(
        kvim_path::WorktreeRelativePath::new("src/lib.rs")?,
        ReviewFileChange::Added,
        &[hunk],
        DiffTruncation::Truncated(DiffLimit::Lines),
    )?;
    let candidate = ReviewCandidate::new(
        ReviewCandidateId::new("candidate-1")?,
        ReviewSection::Unstaged,
        &[file],
    )?;
    let area = Rect::new(0, 0, 72, 16);
    let mut review = ReviewSurface::from_candidates(
        &[candidate],
        ReviewConfig::new(area).with_root_label("Supplied review")?,
    )?;

    let panel = review.panel_snapshot();
    assert_eq!(panel.root_label(), "Supplied review");
    assert!(
        panel
            .rows()
            .iter()
            .any(|row| { row.truncation() == Some(DiffLimit::Lines) && !row.is_complete() })
    );
    for placement in panel.placements() {
        let row = &panel.rows()[placement.row()];
        assert!(placement.area().width <= panel.rows_area().width);
        println!("{}", row.text());
    }

    let _ = review.input(ReviewInput::command(ReviewCommand::MarkRead))?;
    let mut cells = Buffer::empty(area);
    let _ = review.render(&mut cells)?;
    let snapshot = review.snapshot();
    assert_eq!(snapshot.anchor_count(), 2);
    Ok(())
}
