use super::{
    DocumentMapping, DocumentMirror, PositionEncoding, TextMirroring, byte_column, line_starts,
    utf16_column,
};
use crate::protocol::{DocumentPosition, LspError, ProtocolPosition};

/// A line of accented Latin text. Each character is two UTF-8 bytes and one
/// UTF-16 code unit.
const ACCENTED: &str = "\u{e9}\u{e9}";

/// A line that starts with one character above the Basic Multilingual
/// Plane. That character is four UTF-8 bytes and two UTF-16 code units.
const EMOJI: &str = "\u{1f600}ab";

/// Returns the byte column of one UTF-16 column, or fails the test.
fn byte_at(line: &str, column: u32) -> u32 {
    byte_column(line, column).expect("the column addresses a character boundary")
}

/// Returns the UTF-16 column of one byte column, or fails the test.
fn unit_at(line: &str, column: u32) -> u32 {
    utf16_column(line, column).expect("the column addresses a character boundary")
}

#[test]
fn an_absent_encoding_field_means_utf16() {
    assert_eq!(
        PositionEncoding::from_result(None).expect("the protocol defines a default"),
        PositionEncoding::Utf16
    );
    assert_eq!(
        PositionEncoding::from_result(Some("utf-8")).expect("the client offers UTF-8"),
        PositionEncoding::Utf8
    );
    assert_eq!(
        PositionEncoding::from_result(Some("utf-16")).expect("the client offers UTF-16"),
        PositionEncoding::Utf16
    );
    assert!(matches!(
        PositionEncoding::from_result(Some("utf-32")),
        Err(LspError::UnsupportedEncoding)
    ));
}

#[test]
fn ascii_columns_convert_without_change() {
    let line = "let value = 1;";
    for column in 0..=u32::try_from(line.len()).expect("the line is short") {
        assert_eq!(byte_at(line, column), column);
        assert_eq!(unit_at(line, column), column);
    }
}

#[test]
fn accented_latin_text_counts_two_bytes_for_one_unit() {
    assert_eq!(byte_at(ACCENTED, 0), 0);
    assert_eq!(byte_at(ACCENTED, 1), 2);
    assert_eq!(byte_at(ACCENTED, 2), 4);
    assert_eq!(unit_at(ACCENTED, 0), 0);
    assert_eq!(unit_at(ACCENTED, 2), 1);
    assert_eq!(unit_at(ACCENTED, 4), 2);
}

#[test]
fn a_character_above_the_basic_multilingual_plane_takes_two_units() {
    assert_eq!(byte_at(EMOJI, 0), 0);
    assert_eq!(byte_at(EMOJI, 2), 4);
    assert_eq!(byte_at(EMOJI, 3), 5);
    assert_eq!(unit_at(EMOJI, 0), 0);
    assert_eq!(unit_at(EMOJI, 4), 2);
    assert_eq!(unit_at(EMOJI, 5), 3);
}

#[test]
fn a_column_inside_a_character_is_a_typed_failure() {
    assert!(matches!(
        byte_column(EMOJI, 1),
        Err(LspError::InvalidPosition)
    ));
    assert!(matches!(
        utf16_column(EMOJI, 2),
        Err(LspError::InvalidPosition)
    ));
    assert!(matches!(
        utf16_column(ACCENTED, 1),
        Err(LspError::InvalidPosition)
    ));
}

#[test]
fn a_column_at_the_line_end_and_above_it_addresses_the_line_end() {
    assert_eq!(byte_at(EMOJI, 4), 6);
    assert_eq!(byte_at(EMOJI, 9999), 6);
    assert_eq!(unit_at(EMOJI, 6), 4);
    assert_eq!(unit_at(EMOJI, 9999), 4);
}

#[test]
fn an_empty_line_holds_only_its_start() {
    assert_eq!(byte_at("", 0), 0);
    assert_eq!(byte_at("", 12), 0);
    assert_eq!(unit_at("", 0), 0);
    assert_eq!(unit_at("", 12), 0);
}

#[test]
fn every_character_boundary_round_trips_in_both_directions() {
    let line = "let \u{1f600} = \"\u{e9}\u{e9}\"; // \u{6f22}\u{5b57}";
    for (offset, _) in line.char_indices().chain([(line.len(), ' ')]) {
        let byte = u32::try_from(offset).expect("the line is short");
        let unit = unit_at(line, byte);
        assert_eq!(
            byte_at(line, unit),
            byte,
            "byte column {byte} must survive the round trip"
        );
    }
}

#[test]
fn a_line_index_records_every_line_start() {
    assert_eq!(line_starts(""), vec![0]);
    assert_eq!(line_starts("one\ntwo\n"), vec![0, 4, 8]);
    assert_eq!(line_starts("one\ntwo"), vec![0, 4]);
}

#[test]
fn a_mirror_reads_one_line_without_its_line_feed() {
    let mirror = DocumentMirror::new("one\r\ntwo\n");
    assert_eq!(mirror.line(0).expect("the line exists"), "one\r");
    assert_eq!(mirror.line(1).expect("the line exists"), "two");
    assert_eq!(mirror.line(2).expect("the line exists"), "");
    assert!(matches!(mirror.line(3), Err(LspError::InvalidPosition)));
}

#[test]
fn a_utf8_mapping_copies_every_column() {
    let mapping = DocumentMapping::new(PositionEncoding::Utf8, TextMirroring::Absent, EMOJI);
    assert_eq!(
        mapping
            .to_protocol(DocumentPosition::new(0, 4))
            .expect("a direct mapping converts every column"),
        ProtocolPosition::new(0, 4)
    );
    assert_eq!(
        mapping
            .to_document(ProtocolPosition::new(0, 4))
            .expect("a direct mapping converts every column"),
        DocumentPosition::new(0, 4)
    );
}

#[test]
fn a_utf16_mapping_converts_the_line_that_the_position_names() {
    let mapping = DocumentMapping::new(
        PositionEncoding::Utf16,
        TextMirroring::Absent,
        "ascii\n\u{1f600}ab\n",
    );
    assert_eq!(
        mapping
            .to_document(ProtocolPosition::new(1, 2))
            .expect("the column addresses a character boundary"),
        DocumentPosition::new(1, 4)
    );
    assert_eq!(
        mapping
            .to_protocol(DocumentPosition::new(1, 4))
            .expect("the column addresses a character boundary"),
        ProtocolPosition::new(1, 2)
    );
    // The same column on a line of ASCII text converts to itself.
    assert_eq!(
        mapping
            .to_document(ProtocolPosition::new(0, 2))
            .expect("the column addresses a character boundary"),
        DocumentPosition::new(0, 2)
    );
}
