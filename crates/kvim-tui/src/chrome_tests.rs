use ratatui::layout::Rect;

use super::shell_areas;

#[test]
fn the_bands_cover_the_terminal_without_a_gap() {
    for height in 0..=8u16 {
        let terminal = Rect::new(0, 0, 40, height);
        let areas = shell_areas(terminal);
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
    let one = shell_areas(Rect::new(0, 0, 40, 1));
    assert_eq!(one.body.height, 0);
    assert_eq!(one.statusline.height, 0);
    assert_eq!(one.message.height, 1);
    let two = shell_areas(Rect::new(0, 0, 40, 2));
    assert_eq!(two.body.height, 0);
    assert_eq!(two.statusline.height, 1);
    let three = shell_areas(Rect::new(0, 0, 40, 3));
    assert_eq!(three.body.height, 1);
}
