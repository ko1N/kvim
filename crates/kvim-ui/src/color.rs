//! Deterministic terminal color operations.
//!
//! These operations do not select palette values. Callers supply all colors,
//! so the rules work for a standalone presentation and a host-owned surface.

use std::num::NonZeroU16;

use ratatui::style::Color;

/// Moves an RGB foreground one step toward an RGB background.
///
/// `steps` is nonzero and `step` must be smaller than `steps`. Step zero keeps
/// the foreground unchanged. The final valid step retains one `steps` share of
/// the foreground. The calculation truncates each channel toward zero.
///
/// The operation returns `None` unless both colors are [`Color::Rgb`]. This
/// includes [`Color::Reset`], indexed colors, and ANSI colors. A caller can
/// then preserve its existing no-foreground behavior.
///
/// ```
/// use std::num::NonZeroU16;
///
/// use kvim_ui::fade_foreground;
/// use ratatui::style::Color;
///
/// let faded = fade_foreground(
///     Some(Color::Rgb(200, 100, 0)),
///     Some(Color::Rgb(0, 0, 200)),
///     1,
///     NonZeroU16::new(4).expect("the literal four is not zero"),
/// );
///
/// assert_eq!(faded, Some(Color::Rgb(150, 75, 50)));
/// ```
#[must_use]
pub fn fade_foreground(
    foreground: Option<Color>,
    background: Option<Color>,
    step: u16,
    steps: NonZeroU16,
) -> Option<Color> {
    assert!(
        step < steps.get(),
        "the fade step must be smaller than its nonzero step count"
    );
    let (Some(Color::Rgb(red, green, blue)), Some(Color::Rgb(bg_red, bg_green, bg_blue))) =
        (foreground, background)
    else {
        return None;
    };
    let foreground_share = steps.get() - step;
    let blend = |foreground: u8, background: u8| {
        let value = u16::from(foreground) * foreground_share + u16::from(background) * step;
        u8::try_from(value / steps.get()).expect("the average of two bytes is one byte")
    };
    Some(Color::Rgb(
        blend(red, bg_red),
        blend(green, bg_green),
        blend(blue, bg_blue),
    ))
}

#[cfg(test)]
#[path = "color_tests.rs"]
mod tests;
