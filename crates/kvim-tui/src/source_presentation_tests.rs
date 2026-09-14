use super::*;

/// The source rows of the file that the reveal tests read.
const TEST_TOTAL_ROWS: usize = 40;

/// The visible source rows of the viewport that the reveal tests read.
const TEST_VISIBLE_ROWS: usize = 10;

#[test]
fn a_reveal_keeps_the_context_rows_above_a_range_in_the_middle() {
    let first = TEST_TOTAL_ROWS / 2;
    let revealed = source_viewport_first_row(first, first, TEST_TOTAL_ROWS, TEST_VISIBLE_ROWS);
    assert_eq!(first - revealed, SOURCE_PRESENTATION_CONTEXT_ROWS);
    assert!(
        revealed + TEST_VISIBLE_ROWS <= TEST_TOTAL_ROWS,
        "the viewport stays inside the file"
    );
}

#[test]
fn a_reveal_near_the_file_start_shows_the_file_start() {
    let first = SOURCE_PRESENTATION_CONTEXT_ROWS - 1;
    assert_eq!(
        source_viewport_first_row(first, first, TEST_TOTAL_ROWS, TEST_VISIBLE_ROWS),
        0
    );
}

#[test]
fn a_reveal_near_the_file_end_clamps_to_the_last_full_viewport() {
    let first = TEST_TOTAL_ROWS - 1;
    let revealed = source_viewport_first_row(first, first, TEST_TOTAL_ROWS, TEST_VISIBLE_ROWS);
    assert_eq!(revealed, TEST_TOTAL_ROWS - TEST_VISIBLE_ROWS);
    assert!(
        first - revealed > SOURCE_PRESENTATION_CONTEXT_ROWS,
        "the file end leaves no viewport below the context rule"
    );
}

#[test]
fn a_file_shorter_than_the_viewport_reveals_its_first_row() {
    let total = TEST_VISIBLE_ROWS - 1;
    let first = total - 1;
    assert_eq!(
        source_viewport_first_row(first, first, total, TEST_VISIBLE_ROWS),
        0
    );
}

#[test]
fn a_viewport_shorter_than_the_context_rows_still_shows_the_range_start() {
    let first = TEST_TOTAL_ROWS / 2;
    for visible in 1..=SOURCE_PRESENTATION_CONTEXT_ROWS {
        let revealed = source_viewport_first_row(first, first, TEST_TOTAL_ROWS, visible);
        assert_eq!(
            first - revealed,
            visible - 1,
            "a viewport of {visible} rows keeps every row it has above the range"
        );
        assert!(first < revealed + visible, "the range start stays visible");
    }
}

#[test]
fn a_range_that_fits_gives_up_context_rows_to_show_its_last_row() {
    let first = TEST_TOTAL_ROWS / 2;
    let exact = first + TEST_VISIBLE_ROWS - 1;
    let revealed = source_viewport_first_row(first, exact, TEST_TOTAL_ROWS, TEST_VISIBLE_ROWS);
    assert_eq!(
        revealed, first,
        "a range that exactly fills the viewport leaves no row above it"
    );
    assert!(
        exact < revealed + TEST_VISIBLE_ROWS,
        "the last row is visible"
    );

    let with_slack =
        source_viewport_first_row(first, exact - 1, TEST_TOTAL_ROWS, TEST_VISIBLE_ROWS);
    assert_eq!(
        first - with_slack,
        1,
        "one spare viewport row buys one context row"
    );
}

#[test]
fn a_range_taller_than_the_viewport_keeps_its_context_rows() {
    let first = TEST_TOTAL_ROWS / 2;
    let last = first + TEST_VISIBLE_ROWS;
    let revealed = source_viewport_first_row(first, last, TEST_TOTAL_ROWS, TEST_VISIBLE_ROWS);
    assert_eq!(first - revealed, SOURCE_PRESENTATION_CONTEXT_ROWS);
    assert!(
        revealed + TEST_VISIBLE_ROWS <= last,
        "no offset shows the last row of a range this tall"
    );
}

#[test]
fn model_navigation_is_bounded_and_non_wrapping() {
    let path = WorktreeRelativePath::new("src/lib.rs").unwrap();
    let mut presentation = SourcePresentation::new(
        path,
        vec![
            SourceAnnotation::new(1, 2, "first".to_owned()),
            SourceAnnotation::new(4, 4, "second".to_owned()),
        ],
    );
    assert_eq!(presentation.selected_index(), 0);
    assert_eq!(
        presentation.select_previous(),
        Err(SourcePresentationRefusal::AtFirst)
    );
    presentation.select_next().unwrap();
    assert_eq!(presentation.selected().message(), "second");
    assert_eq!(
        presentation.select_next(),
        Err(SourcePresentationRefusal::AtLast)
    );
}

#[test]
fn panel_reserves_only_a_safe_body_row() {
    assert_eq!(
        source_area(Rect::new(0, 0, 4, 1), true),
        (Rect::new(0, 0, 4, 1), None)
    );
    assert_eq!(
        source_area(Rect::new(2, 3, 4, 2), true),
        (Rect::new(2, 3, 4, 1), Some(Rect::new(2, 4, 4, 1)))
    );
}

#[test]
fn panel_keeps_the_counter_in_narrow_geometry() {
    let area = Rect::new(0, 0, 5, 1);
    let mut target = Buffer::empty(area);
    render_panel(
        &mut target,
        area,
        Theme::default(),
        SourcePresentationView {
            message: "message",
            current: 2,
            total: 3,
        },
    );
    let painted: String = (0..5)
        .map(|x| target[(x, 0)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(painted.ends_with("2/3"), "{painted:?}");
}
