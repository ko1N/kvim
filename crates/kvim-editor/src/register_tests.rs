//! Unit tests for the named registers and the append rule.

use super::*;

#[test]
fn a_named_write_leaves_the_unnamed_register_and_the_revision_unchanged() {
    let mut registers = Registers::default();
    registers.set_unnamed(RegisterValue::characterwise("unnamed").expect("the value is bounded"));
    registers.write(
        Some('a'),
        RegisterValue::characterwise("named").expect("the value is bounded"),
        LineEnding::Lf,
    );

    assert_eq!(
        registers.unnamed().map(RegisterValue::text),
        Some("unnamed")
    );
    assert_eq!(
        registers.value(Some('a')).map(RegisterValue::text),
        Some("named")
    );
    // The system clipboard mirror reads this count, so a named write must not
    // move it.
    assert_eq!(registers.revision(), 1);
}

#[test]
fn the_quote_name_and_no_name_both_reach_the_unnamed_register() {
    let mut registers = Registers::default();
    registers.write(
        Some('"'),
        RegisterValue::characterwise("alpha").expect("the value is bounded"),
        LineEnding::Lf,
    );
    assert_eq!(registers.unnamed().map(RegisterValue::text), Some("alpha"));
    assert_eq!(
        registers.value(None).map(RegisterValue::text),
        Some("alpha")
    );
    assert_eq!(registers.revision(), 1);
}

#[test]
fn the_black_hole_register_discards_a_write_and_holds_no_value() {
    let mut registers = Registers::default();
    registers.set_unnamed(RegisterValue::characterwise("kept").expect("the value is bounded"));
    registers.write(
        Some('_'),
        RegisterValue::characterwise("dropped").expect("the value is bounded"),
        LineEnding::Lf,
    );

    assert_eq!(registers.unnamed().map(RegisterValue::text), Some("kept"));
    assert!(registers.value(Some('_')).is_none());
    assert_eq!(registers.revision(), 1);
}

#[test]
fn an_upper_case_name_appends_to_its_lower_case_register() {
    let mut registers = Registers::default();
    registers.write(
        Some('a'),
        RegisterValue::linewise("one", LineEnding::Lf).expect("the value is bounded"),
        LineEnding::Lf,
    );
    registers.write(
        Some('A'),
        RegisterValue::linewise("two", LineEnding::Lf).expect("the value is bounded"),
        LineEnding::Lf,
    );

    assert_eq!(
        registers.value(Some('a')).map(RegisterValue::text),
        Some("one\ntwo\n")
    );
    // Both names read one value.
    assert_eq!(
        registers.value(Some('A')).map(RegisterValue::text),
        Some("one\ntwo\n")
    );
}

#[test]
fn an_append_to_an_empty_register_stores_the_value() {
    let mut registers = Registers::default();
    registers.write(
        Some('Z'),
        RegisterValue::characterwise("first").expect("the value is bounded"),
        LineEnding::Lf,
    );
    assert_eq!(
        registers.value(Some('z')).map(RegisterValue::text),
        Some("first")
    );
}

#[test]
fn a_characterwise_append_joins_the_two_texts() {
    let value = RegisterValue::characterwise("ab")
        .expect("the value is bounded")
        .appended(
            &RegisterValue::characterwise("cd").expect("the value is bounded"),
            LineEnding::Lf,
        );
    assert_eq!(value.text(), "abcd");
    assert_eq!(value.shape(), RegisterShape::Characterwise);
}

#[test]
fn public_register_construction_rejects_oversized_and_malformed_values() {
    let oversized = "x".repeat(REGISTER_BYTES_MAX + 1);
    assert_eq!(
        RegisterValue::characterwise(oversized),
        Err(RegisterValueError::TooLarge {
            bytes: REGISTER_BYTES_MAX + 1,
        })
    );
    assert_eq!(
        RegisterValue::new("line without ending", RegisterShape::Linewise),
        Err(RegisterValueError::MalformedLinewise)
    );
}
