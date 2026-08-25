//! Unit tests for the two views of one hunk.

use super::*;

use kvim_workspace::{
    DiffLineText, HunkId, LineEnding, LineOrigin, NewLine, NewLineRange, OldLine, OldLineRange,
};

fn line(origin: LineOrigin, text: &[u8]) -> DiffLine {
    DiffLine::new(
        origin,
        DiffLineText::new(text.to_vec()).expect("the fixture text is short"),
        LineEnding::Newline,
    )
}

fn context(old: u32, new: u32, text: &str) -> DiffLine {
    line(
        LineOrigin::Context {
            old: OldLine::new(old).expect("the fixture number is one line number"),
            new: NewLine::new(new).expect("the fixture number is one line number"),
        },
        text.as_bytes(),
    )
}

fn removed(old: u32, text: &str) -> DiffLine {
    line(
        LineOrigin::Removed {
            old: OldLine::new(old).expect("the fixture number is one line number"),
        },
        text.as_bytes(),
    )
}

fn added(new: u32, text: &str) -> DiffLine {
    line(
        LineOrigin::Added {
            new: NewLine::new(new).expect("the fixture number is one line number"),
        },
        text.as_bytes(),
    )
}

fn hunk(lines: Vec<DiffLine>) -> Hunk {
    // The ranges must realize the published lines, so the fixture derives both
    // from the lines instead of assuming that they start at one.
    let numbers = |side| {
        let published: Vec<u32> = lines.iter().filter_map(|line| line.number(side)).collect();
        let first = published.first().copied().unwrap_or(1);
        let count = u32::try_from(published.len()).expect("the fixture is short");
        (first, count)
    };
    let (old_first, old_count) = numbers(DiffSide::Old);
    let (new_first, new_count) = numbers(DiffSide::New);
    Hunk::new(
        HunkId::new(0),
        OldLineRange::new(
            OldLine::new(old_first).expect("the fixture number is one line number"),
            old_count,
        )
        .expect("the fixture range is valid"),
        NewLineRange::new(
            NewLine::new(new_first).expect("the fixture number is one line number"),
            new_count,
        )
        .expect("the fixture range is valid"),
        lines,
    )
    .expect("the fixture lines realize both ranges")
}

#[test]
fn both_views_draw_one_hunk_from_the_same_rows() {
    let hunk = hunk(vec![
        context(1, 1, "keep"),
        removed(2, "old"),
        added(2, "new"),
    ]);

    // Two columns pair the replacement onto one row.
    let side = side_rows(&hunk);
    assert_eq!(side.len(), 2);
    assert_eq!(side[0].old.text, "keep");
    assert_eq!(side[0].new.text, "keep");
    assert_eq!(side[1].old.text, "old");
    assert_eq!(side[1].new.text, "new");

    // One column writes the removal before the addition, as a unified diff does.
    let inline = inline_rows(&hunk);
    assert_eq!(inline.len(), 3);
    assert_eq!(
        (inline[0].marker, inline[0].cell.text.as_str()),
        (' ', "keep")
    );
    assert_eq!(
        (inline[1].marker, inline[1].cell.text.as_str()),
        ('-', "old")
    );
    assert_eq!(
        (inline[2].marker, inline[2].cell.text.as_str()),
        ('+', "new")
    );
}

#[test]
fn each_column_keeps_the_line_number_of_its_own_side() {
    let hunk = hunk(vec![context(7, 4, "same"), added(5, "fresh")]);
    let rows = side_rows(&hunk);

    assert_eq!(rows[0].old.number, Some(7));
    assert_eq!(rows[0].new.number, Some(4));
    // An addition draws against a gap, which holds no number at all.
    assert_eq!(rows[1].old.number, None);
    assert_eq!(rows[1].new.number, Some(5));
}

#[test]
fn a_gap_draws_as_its_own_role_and_not_as_unchanged_text() {
    let hunk = hunk(vec![removed(1, "gone")]);
    let rows = side_rows(&hunk);

    assert_eq!(rows[0].old.role, ThemeRole::DiffRemoved);
    assert_eq!(rows[0].new.role, ThemeRole::DiffGap);
    assert!(rows[0].new.text.is_empty());
}

#[test]
fn a_line_that_holds_no_text_states_it_instead_of_guessing() {
    // One invalid byte sequence is exactly what the capture published, so the
    // view names the state rather than inventing characters for it.
    let invalid = line(
        LineOrigin::Added {
            new: NewLine::new(1).expect("one is one line number"),
        },
        &[0xff, 0xfe],
    );
    let rows = side_rows(&hunk(vec![invalid]));

    assert_eq!(rows[0].new.text, NO_TEXT_MARKER);
}

#[test]
fn a_narrow_window_draws_inline_whatever_the_setting_asks() {
    let settings = DiffSettings::default();
    assert_eq!(settings.view, DiffView::SideBySide);

    let wide = two_column_cells_min(settings);
    assert_eq!(view_of(settings, wide), DiffView::SideBySide);
    assert_eq!(view_of(settings, wide - 1), DiffView::Inline);

    // The inline setting stays inline at every width.
    let inline = DiffSettings {
        view: DiffView::Inline,
        ..settings
    };
    assert_eq!(view_of(inline, wide), DiffView::Inline);
}

#[test]
fn the_two_columns_share_the_width_and_keep_one_gap() {
    let hunk = hunk(vec![removed(1, "old"), added(1, "new")]);
    let rows = side_rows(&hunk);
    let area = Rect::new(0, 0, 61, 1);
    let mut cells = CellBuffer::empty(area);

    draw_side_rows(&mut cells, area, Theme::default(), &rows, RowBand::Plain);

    // The old column starts at the left edge and the new column starts after
    // the gap, so both hold the same number of cells.
    let row: String = (0..area.width)
        .map(|x| cells[(x, 0)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(row.contains("old"), "the old column drew: {row:?}");
    assert!(row.contains("new"), "the new column drew: {row:?}");
    let old_at = row.find("old").expect("the old column drew its text");
    let new_at = row.find("new").expect("the new column drew its text");
    assert!(old_at < 30 && new_at >= 30, "{old_at} then {new_at}");
}

#[test]
fn a_text_that_passes_its_column_clips_instead_of_wrapping() {
    let long = "x".repeat(200);
    let hunk = hunk(vec![added(1, &long)]);
    let rows = side_rows(&hunk);
    let area = Rect::new(0, 0, 61, 2);
    let mut cells = CellBuffer::empty(area);

    draw_side_rows(&mut cells, area, Theme::default(), &rows, RowBand::Plain);

    // The second row stays empty, so the clipped text wrapped onto no row.
    let second: String = (0..area.width)
        .map(|x| cells[(x, 1)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert_eq!(second.trim(), "");
}

#[test]
fn the_inline_view_draws_the_marker_before_the_text() {
    let hunk = hunk(vec![added(1, "fresh")]);
    let rows = inline_rows(&hunk);
    let area = Rect::new(0, 0, 40, 1);
    let mut cells = CellBuffer::empty(area);

    draw_inline_rows(&mut cells, area, Theme::default(), &rows, RowBand::Plain);

    let row: String = (0..area.width)
        .map(|x| cells[(x, 0)].symbol().chars().next().unwrap_or(' '))
        .collect();
    let marker = row.find('+').expect("the row drew its marker");
    let text = row.find("fresh").expect("the row drew its text");
    assert!(marker < text, "the marker stands before the text");
}

#[test]
fn a_selected_row_carries_its_band_across_the_whole_width() {
    // The cursor row reads like a Visual-line selection, so the band reaches
    // every cell instead of marking one edge.
    let hunk = hunk(vec![added(1, "fresh")]);
    let rows = side_rows(&hunk);
    let area = Rect::new(0, 0, 61, 1);
    let mut plain = CellBuffer::empty(area);
    let mut selected = CellBuffer::empty(area);

    draw_side_rows(&mut plain, area, Theme::default(), &rows, RowBand::Plain);
    draw_side_rows(
        &mut selected,
        area,
        Theme::default(),
        &rows,
        RowBand::Selected,
    );

    let band = Theme::default().style(ThemeRole::PopupSelection).bg;
    for x in 0..area.width {
        assert_eq!(
            selected[(x, 0)].bg,
            band.expect("the selection names one background"),
            "the band covers cell {x}"
        );
    }
    // The foreground still names the change, so an added line reads as added.
    assert_eq!(selected[(0, 0)].fg, plain[(0, 0)].fg);
}
