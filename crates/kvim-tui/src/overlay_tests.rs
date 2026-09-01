use kvim_input::{Key, KeyCode};
use ratatui::layout::{Position, Rect, Size};

use super::{breadcrumb, float_area};

/// One editor window that starts at the top left corner.
const WINDOW: Rect = Rect {
    x: 0,
    y: 0,
    width: 40,
    height: 20,
};

/// The right window of one vertical split, which starts inside the body.
const SPLIT: Rect = Rect {
    x: 20,
    y: 3,
    width: 20,
    height: 10,
};

#[test]
fn the_breadcrumb_joins_the_pressed_keys_in_their_help_form() {
    let leader = Key::plain(KeyCode::Char(' '));
    assert_eq!(
        breadcrumb(&[leader, Key::plain(KeyCode::Char('w'))]),
        "Space » w",
        "one marker points from each key to the next"
    );
    assert_eq!(
        breadcrumb(&[leader]),
        "Space",
        "one key writes its label alone"
    );
    assert_eq!(
        breadcrumb(&[]),
        "",
        "an overlay without a pending key shows no breadcrumb"
    );
}

#[test]
fn a_float_sits_below_the_cursor_line() {
    let area = float_area(WINDOW, Position::new(5, 2), Size::new(12, 4));
    assert_eq!(
        area,
        Rect::new(5, 3, 12, 4),
        "the float starts one row down"
    );
}

#[test]
fn a_float_flips_above_the_cursor_line_when_the_space_below_is_too_small() {
    // Three rows follow the cursor line, and the float needs four.
    let area = float_area(WINDOW, Position::new(5, 16), Size::new(12, 4));
    assert_eq!(area, Rect::new(5, 12, 12, 4));
    assert_eq!(area.bottom(), 16, "the float never covers the cursor line");
}

#[test]
fn a_float_takes_the_larger_side_and_clips_when_neither_side_holds_it() {
    let narrow = Rect::new(0, 0, 40, 7);
    // Two rows sit above the cursor line and four below it.
    let area = float_area(narrow, Position::new(5, 2), Size::new(12, 9));
    assert_eq!(area, Rect::new(5, 3, 12, 4));
    // Four rows sit above the cursor line and two below it.
    let area = float_area(narrow, Position::new(5, 4), Size::new(12, 9));
    assert_eq!(area, Rect::new(5, 0, 12, 4));
}

#[test]
fn a_float_moves_left_until_its_right_edge_sits_inside_the_window() {
    let area = float_area(WINDOW, Position::new(36, 2), Size::new(12, 4));
    assert_eq!(area, Rect::new(28, 3, 12, 4));
    assert_eq!(area.right(), WINDOW.right());
}

#[test]
fn a_float_that_is_wider_than_the_window_starts_at_the_window_edge() {
    let area = float_area(WINDOW, Position::new(36, 2), Size::new(60, 4));
    assert_eq!(area, Rect::new(0, 3, 40, 4));
}

#[test]
fn a_float_of_a_split_stays_inside_that_window() {
    // The cursor sits near the right edge and near the bottom of the split,
    // so both rules act, and both keep the float inside the split.
    let area = float_area(SPLIT, Position::new(38, 11), Size::new(14, 5));
    assert_eq!(area, Rect::new(26, 6, 14, 5));
    assert!(
        SPLIT.contains(Position::new(area.x, area.y))
            && area.right() <= SPLIT.right()
            && area.bottom() <= SPLIT.bottom(),
        "the float of a split never reaches outside that window"
    );
}

#[test]
fn a_window_of_one_row_leaves_no_space_beside_the_cursor_line() {
    let single = Rect::new(0, 0, 40, 1);
    let area = float_area(single, Position::new(0, 0), Size::new(12, 4));
    assert_eq!(area.height, 0, "no row remains beside the cursor line");
}
