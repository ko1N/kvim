use std::num::NonZeroU8;

use super::{INDENT_COLUMNS_MAX, IndentPolicy, ShiftDirection};

fn cells(value: u8) -> NonZeroU8 {
    NonZeroU8::new(value).expect("the test value is not zero")
}

fn default_policy() -> IndentPolicy {
    IndentPolicy::new(true, cells(4), cells(4))
}

#[test]
fn the_default_policy_uses_four_space_soft_tabs() {
    let policy = default_policy();
    assert_eq!(policy.tab_width(), cells(4));
    assert_eq!(policy.shift_width(), cells(4));
    assert_eq!(policy.render(4), "    ");
}

#[test]
fn the_shift_width_uses_the_resolved_input() {
    let policy = IndentPolicy::new(true, cells(4), cells(2));
    assert_eq!(policy.shift_width(), cells(2));
    assert_eq!(policy.shift_columns(4, ShiftDirection::Right), 6);
}

#[test]
fn a_tab_advances_to_the_next_tab_stop() {
    let policy = default_policy();
    assert_eq!(policy.measure("\tvalue").columns, 4);
    assert_eq!(policy.measure(" \tvalue").columns, 4);
    assert_eq!(policy.measure(" \tvalue").char_len, 2);
    assert_eq!(policy.measure("     \tvalue").columns, 8);
}

#[test]
fn measurement_stops_at_the_first_other_character() {
    let policy = default_policy();
    let indent = policy.measure("  a  b");
    assert_eq!(indent.char_len, 2);
    assert_eq!(indent.columns, 2);
    assert_eq!(policy.measure("").char_len, 0);
}

#[test]
fn a_left_shift_stops_at_zero() {
    let policy = default_policy();
    assert_eq!(policy.shift_columns(5, ShiftDirection::Left), 1);
    assert_eq!(policy.shift_columns(2, ShiftDirection::Left), 0);
    assert_eq!(policy.shift_columns(0, ShiftDirection::Left), 0);
}

#[test]
fn rendering_and_shifting_stay_bounded() {
    let policy = default_policy();
    assert_eq!(policy.render(usize::MAX).len(), INDENT_COLUMNS_MAX);
    assert_eq!(
        policy.shift_columns(usize::MAX, ShiftDirection::Right),
        INDENT_COLUMNS_MAX
    );
}

#[test]
fn a_hard_tab_policy_renders_tabs_and_spaces() {
    let policy = IndentPolicy::new(false, cells(4), cells(4));
    assert_eq!(policy.render(9), "\t\t ");
    assert_eq!(policy.measure("\t\t code").columns, 9);
}
