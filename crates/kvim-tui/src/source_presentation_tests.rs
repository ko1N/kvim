use super::*;

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
