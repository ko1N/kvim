//! Tests for the list viewport: the offset rule, the end of the list, the
//! placements, and the agreement between the pure rule and the stored one.

use super::*;

/// The number of items of the long uniform list that the sweeps walk.
const ITEMS: usize = 10;

/// Returns one list of `count` visible items of one line each.
fn uniform(count: usize) -> impl Iterator<Item = ListItem> + Clone {
    std::iter::repeat_n(ListItem::single(), count)
}

/// Returns one height that is not zero.
fn lines(count: u16) -> NonZeroU16 {
    NonZeroU16::new(count).expect("every test height is above zero")
}

/// Reports whether the window shows the item of the named position.
fn shows(viewport: &ListViewport, index: usize) -> bool {
    viewport
        .placements()
        .iter()
        .any(|placement| placement.index() == index)
}

/// Returns the number of terminal lines that the placements cover.
fn covered_lines(viewport: &ListViewport) -> u32 {
    viewport
        .placements()
        .iter()
        .map(|placement| u32::from(placement.lines()))
        .sum()
}

#[test]
fn the_window_shows_the_selection_at_every_position_of_every_height() {
    for height in 1..=6_u16 {
        for margin in [0_u16, 1, 3] {
            let mut viewport = ListViewport::new(height);
            viewport.set_scroll_margin(margin);
            let last_start = u32::try_from(ITEMS).expect("ten fits u32") - u32::from(height);
            // The sweep walks to the end of the list and back, because the
            // clamp at the end and the clamp at the start meet different arms
            // of the same chain.
            let down = 0..ITEMS;
            let up = (0..ITEMS).rev();
            for selected in down.chain(up) {
                viewport.reconcile(uniform(ITEMS), Some(selected));
                assert!(
                    shows(&viewport, selected),
                    "height {height}, margin {margin}, item {selected} left the window"
                );
                assert!(
                    viewport.first_line() <= last_start,
                    "height {height}, margin {margin}, item {selected} scrolled past the end"
                );
                assert_eq!(
                    covered_lines(&viewport),
                    u32::from(height),
                    "height {height}, margin {margin}, item {selected} left a gap"
                );
            }
        }
    }
}

#[test]
fn the_last_item_stops_the_window_at_the_end_of_the_list() {
    for height in 1..=12_u16 {
        let mut viewport = ListViewport::new(height);
        viewport.reconcile(uniform(ITEMS), Some(ITEMS - 1));
        let total = u32::try_from(ITEMS).expect("ten fits u32");
        assert_eq!(
            viewport.first_line(),
            total.saturating_sub(u32::from(height)),
            "the window of {height} rows stops at the end of the list"
        );
        assert!(shows(&viewport, ITEMS - 1), "the last item stays visible");
    }
}

#[test]
fn a_scroll_margin_stops_at_the_end_of_the_list_instead_of_scrolling_past_it() {
    let mut viewport = ListViewport::new(6);
    viewport.set_scroll_margin(3);

    // The margin holds two rows below the selection, because it stops at half
    // the window.
    viewport.reconcile(uniform(12), Some(8));
    assert_eq!(viewport.first_line(), 5);

    // The last item cannot hold a margin below itself, so the window stops at
    // the last line instead.
    viewport.reconcile(uniform(12), Some(11));
    assert_eq!(viewport.first_line(), 6);
    assert!(shows(&viewport, 11));
}

#[test]
fn a_taller_item_clips_the_first_and_the_last_placement() {
    let mut viewport = ListViewport::new(3);
    let items = || std::iter::repeat_n(ListItem::new(lines(2)), 5);

    viewport.reconcile(items(), Some(4));
    assert_eq!(viewport.total_lines(), 10);
    assert_eq!(viewport.first_line(), 7);

    let placed: Vec<(usize, u16, u16, u16)> = viewport
        .placements()
        .iter()
        .map(|placement| {
            (
                placement.index(),
                placement.first_line(),
                placement.lines(),
                placement.top_row(),
            )
        })
        .collect();
    assert_eq!(placed, vec![(3, 1, 1, 0), (4, 0, 2, 1)]);
}

#[test]
fn a_hidden_item_contributes_no_line_and_no_placement() {
    let mut viewport = ListViewport::new(2);
    let items = || {
        [
            ListItem::single(),
            ListItem::single().with_visible(false),
            ListItem::single(),
            ListItem::single(),
        ]
        .into_iter()
    };

    viewport.reconcile(items(), Some(3));
    assert_eq!(viewport.total_lines(), 3);
    assert_eq!(viewport.first_line(), 1);
    let placed: Vec<usize> = viewport
        .placements()
        .iter()
        .map(ListPlacement::index)
        .collect();
    assert_eq!(placed, vec![2, 3]);
}

#[test]
fn a_list_without_a_selection_keeps_the_offset_inside_the_list() {
    let mut viewport = ListViewport::new(4);
    viewport.reconcile(uniform(ITEMS), Some(ITEMS - 1));
    assert_eq!(viewport.first_line(), 6);

    // A shorter list without a selection pulls the window back to the end.
    viewport.reconcile(uniform(7), None);
    assert_eq!(viewport.first_line(), 3);
    assert_eq!(covered_lines(&viewport), 4);
}

#[test]
fn an_empty_list_and_a_window_of_no_row_place_nothing() {
    let mut viewport = ListViewport::new(4);
    viewport.reconcile(uniform(0), None);
    assert_eq!(viewport.total_lines(), 0);
    assert_eq!(viewport.first_line(), 0);
    assert!(viewport.placements().is_empty());

    viewport.set_height_rows(0);
    viewport.reconcile(uniform(ITEMS), Some(9));
    assert_eq!(viewport.total_lines(), 10);
    assert_eq!(viewport.first_line(), 0);
    assert!(viewport.placements().is_empty());
}

#[test]
fn the_pure_window_and_the_stored_window_answer_the_same_thing() {
    for height in 1..=6_u16 {
        for margin in [0_u16, 1, 3] {
            let mut viewport = ListViewport::new(height);
            viewport.set_scroll_margin(margin);
            // The pure rule carries no offset of its own, so the sweep hands
            // it the offset that the previous answer left behind. That is
            // exactly what the viewport hands its own call.
            let mut previous = 0_u32;
            let down = 0..ITEMS;
            let up = (0..ITEMS).rev();
            for selected in down.chain(up).map(Some).chain([None]) {
                let pure =
                    ListWindow::reconciled(uniform(ITEMS), selected, height, margin, previous);
                viewport.reconcile(uniform(ITEMS), selected);
                assert_eq!(
                    &pure,
                    viewport.window(),
                    "height {height}, margin {margin}, item {selected:?} answered two windows"
                );
                previous = pure.first_line();
            }
        }
    }
}

#[test]
fn a_shared_list_reads_the_window_of_every_height_without_an_offset_of_its_own() {
    // A host that stores no offset passes zero and still reads a window that
    // shows the selection, at whatever height it learns while it draws.
    for height in 1..=12_u16 {
        let window = ListWindow::reconciled(uniform(ITEMS), Some(ITEMS - 1), height, 0, 0);
        let total = u32::try_from(ITEMS).expect("ten fits u32");
        assert_eq!(
            window.first_line(),
            total.saturating_sub(u32::from(height)),
            "the window of {height} rows stops at the end of the list"
        );
        assert_eq!(window.total_lines(), total);
        assert!(
            window
                .placements()
                .iter()
                .any(|placement| placement.index() == ITEMS - 1),
            "the window of {height} rows shows the last item"
        );
    }
}

#[test]
fn the_pure_window_of_no_row_and_of_an_empty_list_places_nothing() {
    let empty = ListWindow::reconciled(uniform(0), None, 4, 0, 0);
    assert_eq!(empty.total_lines(), 0);
    assert_eq!(empty.first_line(), 0);
    assert!(empty.placements().is_empty());

    // A window of no row still measures the list, so a host reads the total
    // before it knows a height.
    let no_row = ListWindow::reconciled(uniform(ITEMS), Some(9), 0, 0, 0);
    assert_eq!(no_row.total_lines(), 10);
    assert_eq!(no_row.first_line(), 0);
    assert!(no_row.placements().is_empty());
}
