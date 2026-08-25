use std::num::NonZeroU8;

use super::{IndentSettings, IndentWidth, ShiftWidth, SplitRatio};

fn cells(value: u8) -> NonZeroU8 {
    NonZeroU8::new(value).expect("the test value is not zero")
}

#[test]
fn follow_tab_width_resolves_to_the_tab_width() {
    assert_eq!(ShiftWidth::FollowTabWidth.resolve(cells(4)), cells(4));
    assert_eq!(ShiftWidth::FollowTabWidth.resolve(cells(8)), cells(8));
}

#[test]
fn explicit_cells_resolve_to_themselves() {
    assert_eq!(ShiftWidth::Cells(cells(2)).resolve(cells(8)), cells(2));
}

#[test]
fn the_language_width_wins_while_no_override_exists() {
    let settings = IndentSettings::default();
    assert_eq!(settings.indent_width, IndentWidth::FollowLanguage);
    assert_eq!(settings.indent_columns(Some(cells(2))), cells(2));
    assert_eq!(settings.indent_columns(Some(cells(4))), cells(4));
}

#[test]
fn a_buffer_without_a_language_takes_the_settings_width() {
    let settings = IndentSettings::default();
    assert_eq!(settings.indent_columns(None), cells(4));

    // The shift width is the settings width, so an explicit shift width
    // answers a buffer that no adapter serves.
    let shifted = IndentSettings {
        shift_width: ShiftWidth::Cells(cells(3)),
        ..IndentSettings::default()
    };
    assert_eq!(shifted.indent_columns(None), cells(3));
}

#[test]
fn an_explicit_override_beats_every_language() {
    let settings = IndentSettings {
        indent_width: IndentWidth::Cells(cells(8)),
        ..IndentSettings::default()
    };
    assert_eq!(settings.indent_columns(Some(cells(2))), cells(8));
    assert_eq!(settings.indent_columns(Some(cells(4))), cells(8));
    assert_eq!(settings.indent_columns(None), cells(8));
}

#[test]
fn split_ratio_rejects_values_outside_its_domain() {
    assert!(SplitRatio::new(f32::NAN).is_none());
    assert!(SplitRatio::new(f32::INFINITY).is_none());
    assert!(SplitRatio::new(0.0).is_none());
    assert!(SplitRatio::new(-1.0).is_none());
}
