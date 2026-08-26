use super::*;

/// Returns the texts of the segments that one band of this width keeps.
fn kept<'a>(band: &ChromeBand<'a>, width: u16) -> Vec<&'a str> {
    band.placements(Rect::new(0, 0, width, 1))
        .iter()
        .map(|placement| placement.segment.text)
        .collect()
}

/// Returns one band of three parts, ranked as kvim ranks its statusline.
///
/// The mode carries the highest rank, because the mode decides what the next
/// key does. The format-on-save state carries the lowest rank, so it goes
/// first. See `docs/windows.md`.
fn statusline() -> ChromeBand<'static> {
    ChromeBand::new(vec![
        BandSegment::left(" NORMAL ", BandRank::new(2)),
        BandSegment::right("auto ", BandRank::new(0)),
        BandSegment::right("12:34 ", BandRank::new(1)),
    ])
    .expect("three parts stay inside the bound")
}

#[test]
fn a_band_that_fits_keeps_every_segment() {
    let band = statusline();
    assert_eq!(kept(&band, 19), [" NORMAL ", "auto ", "12:34 "]);
    assert_eq!(kept(&band, 80), [" NORMAL ", "auto ", "12:34 "]);
}

#[test]
fn one_cell_too_narrow_sheds_the_lowest_rank_alone() {
    let band = statusline();
    assert_eq!(kept(&band, 18), [" NORMAL ", "12:34 "]);
}

#[test]
fn a_band_of_one_part_keeps_the_highest_rank() {
    let band = statusline();
    assert_eq!(kept(&band, 8), [" NORMAL "]);
}

#[test]
fn kvim_parts_shed_in_the_documented_order() {
    let band = statusline();
    // The state goes first, then the position, and the mode always survives.
    assert_eq!(kept(&band, 19), [" NORMAL ", "auto ", "12:34 "]);
    assert_eq!(kept(&band, 14), [" NORMAL ", "12:34 "]);
    assert_eq!(kept(&band, 13), [" NORMAL "]);
    assert_eq!(kept(&band, 7), Vec::<&str>::new());
}

#[test]
fn a_band_that_holds_nothing_sheds_everything() {
    let band = statusline();
    assert!(band.placements(Rect::new(0, 0, 0, 1)).is_empty());
    assert!(band.placements(Rect::new(0, 0, 40, 0)).is_empty());
    assert!(band.placements(Rect::new(0, 0, 1, 1)).is_empty());
}

#[test]
fn the_edges_place_the_parts_and_the_last_right_part_ends_at_the_last_cell() {
    let band = statusline();
    let area = Rect::new(4, 2, 30, 1);
    let placements = band.placements(area);
    assert_eq!(placements[0].area, Rect::new(4, 2, 8, 1));
    assert_eq!(placements[1].area, Rect::new(23, 2, 5, 1));
    assert_eq!(placements[2].area, Rect::new(28, 2, 6, 1));
    assert_eq!(placements[2].area.right(), area.right());
}

#[test]
fn a_wide_character_occupies_the_cells_it_occupies() {
    let band = ChromeBand::new(vec![BandSegment::right("日本 ", BandRank::new(0))])
        .expect("one part stays inside the bound");
    assert_eq!(band.segments()[0].cells(), 5);

    let area = Rect::new(0, 0, 8, 1);
    let placements = band.placements(area);
    assert_eq!(placements[0].area, Rect::new(3, 0, 5, 1));
    // A band of four cells cannot hold the part whole, so it holds none of it.
    assert!(band.placements(Rect::new(0, 0, 4, 1)).is_empty());
}

#[test]
fn one_rank_sheds_the_later_part_first() {
    let band = ChromeBand::new(vec![
        BandSegment::left("aa", BandRank::new(1)),
        BandSegment::left("bb", BandRank::new(1)),
    ])
    .expect("two parts stay inside the bound");
    assert_eq!(kept(&band, 2), ["aa"]);
}

#[test]
fn the_segment_bound_refuses_instead_of_cutting() {
    let segments = vec![BandSegment::left("x", BandRank::new(0)); BAND_SEGMENTS_MAX + 1];
    assert_eq!(
        ChromeBand::new(segments),
        Err(BandError::Limit {
            actual: BAND_SEGMENTS_MAX + 1,
            max: BAND_SEGMENTS_MAX,
        })
    );

    let segments = vec![BandSegment::left("x", BandRank::new(0)); BAND_SEGMENTS_MAX];
    let band = ChromeBand::new(segments).expect("the bound itself is inside the bound");
    assert_eq!(band.len(), BAND_SEGMENTS_MAX);
}
