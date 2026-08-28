use ratatui::layout::Rect;

use super::{EditorPresentation, shell_areas};

#[test]
fn every_chrome_ownership_combination_removes_its_rows() {
    for command_line_embedded in [false, true] {
        for statusline_embedded in [false, true] {
            let presentation =
                EditorPresentation::new(command_line_embedded, statusline_embedded, true, true);
            for height in 1u16..=4 {
                let area = Rect::new(7, 11, 20, height);
                let bands = shell_areas(area, presentation);
                let expected_message = u16::from(command_line_embedded).min(height);
                let expected_status =
                    u16::from(statusline_embedded).min(height.saturating_sub(expected_message));
                assert_eq!(bands.message.height, expected_message);
                assert_eq!(bands.statusline.height, expected_status);
                assert_eq!(
                    bands.body.height + bands.statusline.height + bands.message.height,
                    height
                );
                assert_eq!(bands.body.x, area.x);
                assert_eq!(bands.body.width, area.width);
            }
        }
    }
}

#[test]
fn the_bands_cover_the_terminal_without_a_gap() {
    for height in 0..=8u16 {
        let terminal = Rect::new(0, 0, 40, height);
        let areas = shell_areas(terminal, EditorPresentation::default());
        let covered = areas.body.height + areas.statusline.height + areas.message.height;
        assert_eq!(
            covered, height,
            "a terminal of {height} rows keeps every row"
        );
        assert_eq!(areas.body.y, terminal.y);
        if height > 0 {
            assert_eq!(
                areas.message.bottom(),
                terminal.bottom(),
                "the message line always ends the terminal"
            );
        }
    }
}

#[test]
fn a_short_terminal_drops_the_body_before_the_message_line() {
    let one = shell_areas(Rect::new(0, 0, 40, 1), EditorPresentation::default());
    assert_eq!(one.body.height, 0);
    assert_eq!(one.statusline.height, 0);
    assert_eq!(one.message.height, 1);
    let two = shell_areas(Rect::new(0, 0, 40, 2), EditorPresentation::default());
    assert_eq!(two.body.height, 0);
    assert_eq!(two.statusline.height, 1);
    let three = shell_areas(Rect::new(0, 0, 40, 3), EditorPresentation::default());
    assert_eq!(three.body.height, 1);
}

#[test]
fn the_popup_region_ends_on_the_statusline_row() {
    for height in 0..=8u16 {
        let areas = shell_areas(Rect::new(0, 0, 40, height), EditorPresentation::default());
        let region = areas.above_command_line();
        assert_eq!(
            region.height,
            areas.body.height + areas.statusline.height,
            "the region holds exactly the body and the statusline rows"
        );
        assert_eq!(region.x, areas.body.x);
        assert_eq!(region.y, areas.body.y);
        if height > 1 {
            assert_eq!(
                region.bottom(),
                areas.statusline.bottom(),
                "the region ends where the statusline ends, directly above the message line"
            );
        }
    }
}

#[test]
fn a_terminal_of_height_two_gives_the_popup_region_one_row() {
    let areas = shell_areas(Rect::new(0, 0, 40, 2), EditorPresentation::default());
    let region = areas.above_command_line();
    assert_eq!(
        region,
        Rect::new(0, 0, 40, 1),
        "the empty body and the one-row statusline compose to one row"
    );
}
