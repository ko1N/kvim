//! One bounded band of parts that sheds them in a stated order.
//!
//! A statusline, a winbar, and any other one-row band of a terminal hold parts
//! that a narrow terminal cannot all show. Which part goes first is a rule, not
//! a layout detail: a reader that loses the wrong part loses the answer that the
//! band exists to give.
//!
//! The band publishes that rule and nothing else. A segment carries text that
//! the caller already rendered, the edge that it sits against, and one rank. A
//! band too narrow to hold every segment sheds the lowest rank first, and the
//! highest rank survives every shed. The band names no subject, no color, and no
//! glyph, so a host fills it with its own parts and keeps this precedence. See
//! `docs/windows.md`.
//!
//! The module is pure. It reads no clock, no filesystem, and no terminal, and it
//! paints no cell: [`ChromeBand::placements`] answers where every kept segment
//! sits, and the caller draws it with its own theme.
//!
//! `crates/kvim-ui/examples/chrome_band.rs` is one complete host of one band: it
//! ranks three parts of its own, sheds them at three widths, and prints the row
//! that each width draws.

use std::cmp::Reverse;

use ratatui::layout::Rect;
use thiserror::Error;

use crate::cells::text_cells;

/// The largest number of segments that one band holds.
///
/// One row of a terminal shows far fewer parts than this bound. The bound keeps
/// the shedding scan finite, and a caller that needs more parts than this needs
/// a second band instead of a longer one.
pub const BAND_SEGMENTS_MAX: usize = 16;

/// The edge of the band that one segment sits against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BandSide {
    /// The segment sits against the left edge, after every earlier left segment.
    Left,
    /// The segment sits against the right edge, before every later right
    /// segment. The right segments that survive one shed end at the last cell
    /// of the band together.
    Right,
}

/// How long one segment survives a band that cannot hold every part.
///
/// A larger rank survives longer, so the part that a reader must never lose
/// carries the largest rank of the band. Two segments of one rank shed in the
/// reverse of the order that the caller listed them, so the earlier segment
/// survives longer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BandRank(u8);

impl BandRank {
    /// Returns one rank of a stated importance.
    #[must_use]
    pub const fn new(rank: u8) -> Self {
        Self(rank)
    }

    /// Returns the stated importance of the rank.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// One part of one band: the rendered text, its edge, and its rank.
///
/// The text is the text that the caller already rendered, including every blank
/// that it wants around the part. The band measures terminal cells and inserts
/// no separator of its own, because a separator is a presentation value that the
/// caller owns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandSegment<'a> {
    /// The text that the caller rendered for this part.
    pub text: &'a str,
    /// The edge that the segment sits against.
    pub side: BandSide,
    /// How long the segment survives a narrow band.
    pub rank: BandRank,
}

impl<'a> BandSegment<'a> {
    /// Returns one segment against the left edge of the band.
    #[must_use]
    pub const fn left(text: &'a str, rank: BandRank) -> Self {
        Self {
            text,
            side: BandSide::Left,
            rank,
        }
    }

    /// Returns one segment against the right edge of the band.
    #[must_use]
    pub const fn right(text: &'a str, rank: BandRank) -> Self {
        Self {
            text,
            side: BandSide::Right,
            rank,
        }
    }

    /// Returns the number of terminal cells that the segment occupies.
    ///
    /// The measurement counts cells, never characters, so a wide character
    /// occupies the two cells that it occupies on the terminal.
    #[must_use]
    pub fn cells(&self) -> usize {
        text_cells(self.text)
    }
}

/// The place of one kept segment inside one drawn band.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandPlacement<'a> {
    /// The segment that the placement draws.
    pub segment: BandSegment<'a>,
    /// The rectangle of one row that the segment occupies.
    pub area: Rect,
}

/// Why one band refused its segments.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BandError {
    /// The caller listed more segments than one band holds.
    ///
    /// The band refuses the whole list instead of cutting it, because a cut
    /// would drop a part that the caller ranked and never reported the loss.
    #[error("a band holds {max} segments, and this one holds {actual}")]
    Limit {
        /// The number of segments that the caller listed.
        actual: usize,
        /// The bound that a band stays inside.
        max: usize,
    },
}

/// One bounded band of parts that sheds them in a stated order.
///
/// The band holds the segments in the order that the caller listed them, which
/// is the order that it draws them along each edge. The rank of each segment,
/// not that order, decides which part a narrow band loses.
///
/// # Examples
///
/// ```
/// use ratatui::layout::Rect;
///
/// use kvim_ui::{BandRank, BandSegment, ChromeBand};
///
/// // The host names its own subjects. The band names none of them.
/// let state = BandSegment::left(" ONLINE ", BandRank::new(2));
/// let room = BandSegment::right("#general ", BandRank::new(1));
/// let unread = BandSegment::right("3 unread ", BandRank::new(0));
/// let band = ChromeBand::new(vec![state, room, unread])?;
///
/// // A band that holds every part places each one against its own edge, and
/// // the last right segment ends at the last cell of the band.
/// let wide = Rect::new(0, 0, 40, 1);
/// let placements = band.placements(wide);
/// assert_eq!(placements.len(), 3);
/// assert_eq!(placements[0].area, Rect::new(0, 0, 8, 1));
/// assert_eq!(placements[2].area.right(), wide.right());
///
/// // A narrower band sheds the lowest rank first.
/// let kept: Vec<&str> = band
///     .placements(Rect::new(0, 0, 17, 1))
///     .iter()
///     .map(|placement| placement.segment.text)
///     .collect();
/// assert_eq!(kept, [" ONLINE ", "#general "]);
///
/// // The highest rank survives every shed.
/// let kept: Vec<&str> = band
///     .placements(Rect::new(0, 0, 8, 1))
///     .iter()
///     .map(|placement| placement.segment.text)
///     .collect();
/// assert_eq!(kept, [" ONLINE "]);
/// # Ok::<(), kvim_ui::BandError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChromeBand<'a> {
    segments: Vec<BandSegment<'a>>,
}

impl<'a> ChromeBand<'a> {
    /// Returns one band of the listed segments.
    ///
    /// # Errors
    ///
    /// Returns [`BandError::Limit`] when the list holds more than
    /// [`BAND_SEGMENTS_MAX`] segments. The band refuses the list instead of
    /// cutting it.
    pub fn new(segments: Vec<BandSegment<'a>>) -> Result<Self, BandError> {
        if segments.len() > BAND_SEGMENTS_MAX {
            return Err(BandError::Limit {
                actual: segments.len(),
                max: BAND_SEGMENTS_MAX,
            });
        }
        Ok(Self { segments })
    }

    /// Returns every segment, in the order that the caller listed them.
    #[must_use]
    pub fn segments(&self) -> &[BandSegment<'a>] {
        &self.segments
    }

    /// Returns the number of segments that the band holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Reports whether the band holds no segment.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Returns the place of every segment that one band of this size keeps.
    ///
    /// A band that cannot hold every part sheds the lowest rank first, and
    /// repeats that shed until the kept parts fit. A band that fits no part at
    /// all keeps none, so a row of no cell answers an empty list.
    ///
    /// The left segments run from the left edge in list order. The right
    /// segments end at the last cell of the band together, also in list order.
    /// The answer holds the kept segments in the order that the caller listed
    /// them.
    #[must_use]
    pub fn placements(&self, area: Rect) -> Vec<BandPlacement<'a>> {
        if area.is_empty() {
            return Vec::new();
        }
        let width = usize::from(area.width);

        // Each pass removes one segment, and the band holds at most
        // `BAND_SEGMENTS_MAX` of them, so the shed is finite.
        let mut kept: Vec<usize> = (0..self.segments.len()).collect();
        while !kept.is_empty() && self.kept_cells(&kept) > width {
            kept.remove(self.lowest_rank_position(&kept));
        }

        let right_cells = kept
            .iter()
            .map(|index| self.segments[*index])
            .filter(|segment| segment.side == BandSide::Right)
            .map(|segment| segment.cells())
            .sum::<usize>();
        let mut left_x = area.x;
        // The kept parts fit the band, so the right group starts inside it.
        let mut right_x = area
            .right()
            .saturating_sub(u16::try_from(right_cells).unwrap_or(u16::MAX));

        let mut placements = Vec::with_capacity(kept.len());
        for index in kept {
            let segment = self.segments[index];
            let cells = u16::try_from(segment.cells()).unwrap_or(u16::MAX);
            let cursor = match segment.side {
                BandSide::Left => &mut left_x,
                BandSide::Right => &mut right_x,
            };
            let x = *cursor;
            *cursor = cursor.saturating_add(cells);
            placements.push(BandPlacement {
                segment,
                area: Rect::new(x, area.y, cells, 1),
            });
        }
        debug_assert!(
            placements
                .iter()
                .all(|placement| placement.area.right() <= area.right()),
            "the shed keeps only the parts that the band holds whole"
        );
        placements
    }

    /// Returns the cells that the kept segments occupy together.
    fn kept_cells(&self, kept: &[usize]) -> usize {
        kept.iter().map(|index| self.segments[*index].cells()).sum()
    }

    /// Returns the position in `kept` of the segment that sheds next.
    ///
    /// The lowest rank sheds first. Two segments of one rank shed in the reverse
    /// of the order that the caller listed them, so the later one goes first.
    fn lowest_rank_position(&self, kept: &[usize]) -> usize {
        debug_assert!(!kept.is_empty(), "a band of no part sheds nothing");
        kept.iter()
            .enumerate()
            .min_by_key(|(position, index)| (self.segments[**index].rank, Reverse(*position)))
            .map_or(0, |(position, _)| position)
    }
}

#[cfg(test)]
#[path = "band_tests.rs"]
mod tests;
