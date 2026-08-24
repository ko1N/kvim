//! The position encoding of one language-server session.
//!
//! kvim measures every column in UTF-8 bytes. The Language Server Protocol
//! measures a column in UTF-16 code units unless the server confirms another
//! encoding. The values below negotiate the encoding and convert every column
//! at the session boundary, so no code above the session reads a protocol
//! column.
//!
//! A UTF-16 column indexes the line that its position names, so a conversion
//! needs the exact text of that line. A UTF-16 session therefore mirrors the
//! text that it sent to its server. A UTF-8 session mirrors no text and pays no
//! conversion cost.
//!
//! A session that sends a full synchronization also mirrors that text, because
//! it builds the complete text of every change from the mirror.
//!
//! Every function in this file is pure. It reads text and returns a value, and
//! it performs no input and no output. See `docs/language-services.md`.

use std::ops::Range;

use crate::document::ContentChange;
use crate::protocol::{DocumentPosition, LspError, ProtocolPosition, ProtocolSpan, SourceSpan};

/// The unit that one server counts a protocol column in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionEncoding {
    /// One column is one UTF-8 byte offset inside its line.
    Utf8,
    /// One column is one UTF-16 code-unit offset inside its line.
    Utf16,
}

impl PositionEncoding {
    /// The encodings that the client offers, in order of preference.
    ///
    /// UTF-8 stands first, so a server that supports UTF-8 still selects it and
    /// its session converts nothing.
    pub const OFFERED: [Self; 2] = [Self::Utf8, Self::Utf16];

    /// Returns the protocol name of this encoding.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16 => "utf-16",
        }
    }

    /// Reads the encoding that one `initialize` result confirmed.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::UnsupportedEncoding`] for a name that kvim never
    /// offered.
    pub fn from_result(answer: Option<&str>) -> Result<Self, LspError> {
        // The protocol defines UTF-16 for a result that names no encoding, so
        // an absent field is a valid answer and starts the server.
        let Some(name) = answer else {
            return Ok(Self::Utf16);
        };
        Self::OFFERED
            .into_iter()
            .find(|encoding| encoding.as_str() == name)
            .ok_or(LspError::UnsupportedEncoding)
    }
}

/// Whether one document mirrors the text that the server holds.
///
/// The negotiated encoding decides one part of the answer, and the
/// synchronization that the server asked for decides the other part. A full
/// synchronization sends the complete text of every change, so its session
/// builds that text from the mirror. See `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextMirroring {
    /// The session needs no mirror of this document.
    Absent,
    /// The session mirrors the exact text that the server holds.
    Present,
}

/// The position conversion of one open document.
///
/// The variant follows the negotiated encoding and the mirror that the session
/// needs, so a UTF-16 session cannot lose its mirror.
pub enum DocumentMapping {
    /// The server confirmed UTF-8 and the session needs no mirror, so one
    /// protocol column is already one byte column and the session converts
    /// nothing and mirrors nothing.
    Direct,
    /// The server confirmed UTF-8 and the session mirrors the text, so it
    /// converts no column and still holds the text that the server holds.
    Mirrored(DocumentMirror),
    /// The server counts UTF-16 code units, so the session mirrors the exact
    /// text that the server holds and converts every column against it.
    Utf16(DocumentMirror),
}

impl DocumentMapping {
    /// Creates the conversion of one document that the session opens.
    ///
    /// A UTF-16 session always mirrors the text, because every conversion reads
    /// the line that its column indexes. A UTF-8 session mirrors the text only
    /// when `mirroring` asks for it.
    #[must_use]
    pub fn new(encoding: PositionEncoding, mirroring: TextMirroring, text: &str) -> Self {
        match (encoding, mirroring) {
            (PositionEncoding::Utf8, TextMirroring::Absent) => Self::Direct,
            (PositionEncoding::Utf8, TextMirroring::Present) => {
                Self::Mirrored(DocumentMirror::new(text))
            }
            (PositionEncoding::Utf16, _) => Self::Utf16(DocumentMirror::new(text)),
        }
    }

    /// Updates the mirror with the changes of one accepted `didChange`.
    ///
    /// Call this only after the notification reached the server. A failed write
    /// then leaves the mirror on the content that the server still holds.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::InvalidPosition`] when one change does not address
    /// the exact mirrored text. The caller must then drop the document, because
    /// the mirror and the server copy hold different text.
    pub fn apply(&mut self, changes: &[ContentChange]) -> Result<(), LspError> {
        match self {
            Self::Direct => Ok(()),
            Self::Mirrored(mirror) | Self::Utf16(mirror) => mirror.apply(changes),
        }
    }

    /// Returns the text that the changes of one synchronization produce.
    ///
    /// A full synchronization carries the complete text of the document instead
    /// of one range for each change. The call changes nothing, so the caller
    /// sends this text first and applies the changes after the notification
    /// reached the server.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::InvalidPosition`] when one change does not address
    /// the exact mirrored text, and for a mapping that mirrors no text.
    pub fn projected(&self, changes: &[ContentChange]) -> Result<String, LspError> {
        match self {
            Self::Direct => {
                debug_assert!(false, "a full synchronization always mirrors its text");
                Err(LspError::InvalidPosition)
            }
            Self::Mirrored(mirror) | Self::Utf16(mirror) => mirror.projected(changes),
        }
    }

    /// Converts one editor position into the position that the session sends.
    ///
    /// # Errors
    ///
    /// Returns [`LspError::InvalidPosition`] for a position that the mirrored
    /// text does not hold.
    pub fn to_protocol(&self, position: DocumentPosition) -> Result<ProtocolPosition, LspError> {
        match self {
            Self::Direct | Self::Mirrored(_) => {
                Ok(ProtocolPosition::new(position.line, position.byte_column))
            }
            Self::Utf16(mirror) => {
                let line = mirror.line(position.line)?;
                let column = utf16_column(line, position.byte_column)?;
                Ok(ProtocolPosition::new(position.line, column))
            }
        }
    }

    /// Converts one received position into the position that the editor reads.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`DocumentMapping::to_protocol`].
    pub fn to_document(&self, position: ProtocolPosition) -> Result<DocumentPosition, LspError> {
        match self {
            Self::Direct | Self::Mirrored(_) => {
                Ok(DocumentPosition::new(position.line, position.character))
            }
            Self::Utf16(mirror) => {
                let line = mirror.line(position.line)?;
                let column = byte_column(line, position.character)?;
                Ok(DocumentPosition::new(position.line, column))
            }
        }
    }

    /// Converts one editor range into the range that the session sends.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`DocumentMapping::to_protocol`].
    pub fn span_to_protocol(&self, span: SourceSpan) -> Result<ProtocolSpan, LspError> {
        Ok(ProtocolSpan::new(
            self.to_protocol(span.start)?,
            self.to_protocol(span.end)?,
        ))
    }

    /// Converts one received range into the range that the editor reads.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`DocumentMapping::to_protocol`].
    pub fn span_to_document(&self, span: ProtocolSpan) -> Result<SourceSpan, LspError> {
        Ok(SourceSpan::new(
            self.to_document(span.start)?,
            self.to_document(span.end)?,
        ))
    }
}

/// The exact document text that one UTF-16 session converts against.
///
/// The mirror holds the text that the session sent to its server, so every
/// conversion reads the content of the revision that the column names.
pub struct DocumentMirror {
    text: String,
    /// The byte offset of the first byte of each line.
    ///
    /// The index makes one line lookup constant, so one conversion costs the
    /// length of its line and never a walk over the document.
    line_starts: Vec<usize>,
}

impl DocumentMirror {
    /// Mirrors the exact text of one document.
    fn new(text: &str) -> Self {
        Self {
            line_starts: line_starts(text),
            text: text.to_owned(),
        }
    }

    /// Returns the byte range of one line, without its line feed.
    ///
    /// A carriage return stays part of its line, because the server counts the
    /// bytes that it received.
    fn line_range(&self, index: u32) -> Result<Range<usize>, LspError> {
        let index = usize::try_from(index).map_err(|_| LspError::InvalidPosition)?;
        let start = *self
            .line_starts
            .get(index)
            .ok_or(LspError::InvalidPosition)?;
        let end = self
            .line_starts
            .get(index + 1)
            .map_or(self.text.len(), |next| next - 1);
        Ok(start..end)
    }

    /// Returns the text of one line, without its line feed.
    fn line(&self, index: u32) -> Result<&str, LspError> {
        let range = self.line_range(index)?;
        Ok(&self.text[range])
    }

    /// Returns the byte offset of one editor position inside the mirror.
    fn byte_offset(&self, position: DocumentPosition) -> Result<usize, LspError> {
        let range = self.line_range(position.line)?;
        let line = &self.text[range.clone()];
        let column =
            usize::try_from(position.byte_column).map_err(|_| LspError::InvalidPosition)?;
        if column > line.len() || !line.is_char_boundary(column) {
            return Err(LspError::InvalidPosition);
        }
        Ok(range.start + column)
    }

    /// Replaces the changed ranges and rebuilds the line index.
    ///
    /// Call this only after the notification reached the server, so a failed
    /// write leaves the mirror on the text that the server still holds.
    fn apply(&mut self, changes: &[ContentChange]) -> Result<(), LspError> {
        if changes.is_empty() {
            return Ok(());
        }
        let text = self.projected(changes)?;
        self.line_starts = line_starts(&text);
        self.text = text;
        Ok(())
    }

    /// Returns the text that the changed ranges produce.
    ///
    /// The session sends the changes in descending order, so one ascending walk
    /// builds the new text in one pass. The walk never moves an offset twice,
    /// so a large transaction stays linear in the document length.
    fn projected(&self, changes: &[ContentChange]) -> Result<String, LspError> {
        let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(changes.len());
        for change in changes.iter().rev() {
            let start = self.byte_offset(change.span.start)?;
            let end = self.byte_offset(change.span.end)?;
            let ascends = start <= end;
            let follows = ranges.last().is_none_or(|&(_, last)| last <= start);
            if !ascends || !follows {
                return Err(LspError::InvalidPosition);
            }
            ranges.push((start, end));
        }
        let mut text = String::with_capacity(self.text.len());
        let mut cursor = 0_usize;
        for (&(start, end), change) in ranges.iter().zip(changes.iter().rev()) {
            text.push_str(&self.text[cursor..start]);
            text.push_str(&change.text);
            cursor = end;
        }
        text.push_str(&self.text[cursor..]);
        Ok(text)
    }
}

/// Returns the byte offset of the first byte of each line.
///
/// A line feed terminates its line, so a text that ends with one carries one
/// more empty line. The result therefore matches a split on the line feed.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(text.len() / LINE_BYTES_TYPICAL + 1);
    starts.push(0);
    starts.extend(text.match_indices('\n').map(|(offset, _)| offset + 1));
    starts
}

/// The line length that the line index reserves capacity for, in bytes.
///
/// The value only sizes one allocation. A longer or shorter line still records
/// its start, so no bound depends on this number.
const LINE_BYTES_TYPICAL: usize = 32;

/// Returns the UTF-16 code units of one character, which is one or two.
///
/// A character above the Basic Multilingual Plane needs a surrogate pair, so it
/// counts as one code point, two UTF-16 code units, and up to four UTF-8 bytes.
const fn utf16_width(character: char) -> u32 {
    if (character as u32) >= 0x1_0000 { 2 } else { 1 }
}

/// Returns the UTF-16 code units of one text.
fn utf16_units(text: &str) -> u32 {
    text.chars().fold(0_u32, |units, character| {
        units.saturating_add(utf16_width(character))
    })
}

/// Converts one UTF-16 code-unit column into a UTF-8 byte column.
///
/// A column above the end of its line becomes the end of that line, which is
/// the rule that the protocol defines.
///
/// # Errors
///
/// Returns [`LspError::InvalidPosition`] for a column between the two code
/// units of one character. Such a column would build an edit that splits the
/// character and corrupts the buffer.
fn byte_column(line: &str, column: u32) -> Result<u32, LspError> {
    debug_assert!(
        u32::try_from(line.len()).is_ok(),
        "the file size bound of settings keeps every line below 32 bits"
    );
    let mut units = 0_u32;
    for (offset, character) in line.char_indices() {
        if units == column {
            return Ok(u32::try_from(offset).unwrap_or(u32::MAX));
        }
        if units > column {
            return Err(LspError::InvalidPosition);
        }
        units = units.saturating_add(utf16_width(character));
    }
    if units > column {
        return Err(LspError::InvalidPosition);
    }
    Ok(u32::try_from(line.len()).unwrap_or(u32::MAX))
}

/// Converts one UTF-8 byte column into a UTF-16 code-unit column.
///
/// The clamp rule of [`byte_column`] holds in this direction too.
///
/// # Errors
///
/// Returns [`LspError::InvalidPosition`] for a column inside one character.
fn utf16_column(line: &str, column: u32) -> Result<u32, LspError> {
    let column = usize::try_from(column).map_err(|_| LspError::InvalidPosition)?;
    if column >= line.len() {
        return Ok(utf16_units(line));
    }
    if !line.is_char_boundary(column) {
        return Err(LspError::InvalidPosition);
    }
    Ok(utf16_units(&line[..column]))
}

#[cfg(test)]
mod tests {
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
}
