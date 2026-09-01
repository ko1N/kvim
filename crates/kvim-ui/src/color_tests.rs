use std::num::NonZeroU16;

use ratatui::style::Color;

use super::fade_foreground;

fn steps() -> NonZeroU16 {
    NonZeroU16::new(4).expect("the literal four is not zero")
}

#[test]
fn fade_keeps_the_foreground_at_the_first_step() {
    assert_eq!(
        fade_foreground(
            Some(Color::Rgb(200, 100, 0)),
            Some(Color::Rgb(0, 0, 200)),
            0,
            steps(),
        ),
        Some(Color::Rgb(200, 100, 0))
    );
}

#[test]
fn fade_retains_one_foreground_share_at_the_final_step() {
    assert_eq!(
        fade_foreground(
            Some(Color::Rgb(200, 100, 0)),
            Some(Color::Rgb(0, 0, 200)),
            3,
            steps(),
        ),
        Some(Color::Rgb(50, 25, 150))
    );
}

#[test]
fn fade_rejects_non_rgb_colors() {
    for color in [Color::Reset, Color::Indexed(42), Color::Blue] {
        assert_eq!(
            fade_foreground(Some(color), Some(Color::Rgb(1, 2, 3)), 1, steps()),
            None,
            "a non-RGB foreground has no deterministic RGB fade"
        );
        assert_eq!(
            fade_foreground(Some(Color::Rgb(1, 2, 3)), Some(color), 1, steps()),
            None,
            "a non-RGB background has no deterministic RGB fade"
        );
    }
}

#[test]
#[should_panic(expected = "the fade step must be smaller than its nonzero step count")]
fn fade_rejects_a_step_outside_its_bounds() {
    let _ = fade_foreground(
        Some(Color::Rgb(1, 2, 3)),
        Some(Color::Rgb(4, 5, 6)),
        steps().get(),
        steps(),
    );
}
