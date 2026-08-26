//! Tests for the sidebar rows, the selection, the scrolling, and the rendering.

use std::num::NonZeroU16;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::{
    ListMotion, ParentScanRow, RowKind, SIDEBAR_ACTION_CHARS_MAX, SIDEBAR_LABEL_CHARS_MAX,
    SIDEBAR_ROW_DEPTH_MAX, SIDEBAR_ROW_DRAWS_MAX, SIDEBAR_ROW_LINES_MAX, SIDEBAR_ROWS_MAX,
    SIDEBAR_SECTIONS_MAX, SidebarAction, SidebarError, SidebarEvent, SidebarInput, SidebarRow,
    SidebarState, parent_row,
};

/// The row identity that the host owns. The sidebar only compares the value.
type RowId = u32;

fn height(lines: u16) -> NonZeroU16 {
    NonZeroU16::new(lines).expect("the test names a non-zero height")
}

/// Returns one sidebar of `count` selectable rows of one terminal row each.
fn single_rows(viewport_rows: u16, count: u32) -> SidebarState<RowId> {
    let mut sidebar = SidebarState::new(viewport_rows);
    let rows = (0..count)
        .map(|id| SidebarRow::single(id, RowKind::Selectable))
        .collect();
    sidebar
        .set_rows(rows)
        .expect("the rows stay inside the bounds");
    sidebar
}

/// Returns one sidebar of `count` selectable rows of two terminal rows each.
fn double_rows(viewport_rows: u16, count: u32) -> SidebarState<RowId> {
    let mut sidebar = SidebarState::new(viewport_rows);
    let rows = (0..count)
        .map(|id| SidebarRow::new(id, height(2), RowKind::Selectable))
        .collect();
    sidebar
        .set_rows(rows)
        .expect("the rows stay inside the bounds");
    sidebar
}

/// Returns the two-directory tree that a collapsed-subtree test moves over.
///
/// `a` and `b` both hold one collapsed child, so `a/1`, `a/2`, and `b/1` are
/// all hidden. `a` and `b` stay visible, because a collapsed row hides only
/// the rows below it.
fn tree_with_two_collapsed_directories() -> SidebarState<RowId> {
    let mut sidebar = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable).with_collapsed(true), // a
            SidebarRow::single(1, RowKind::Selectable).with_depth(1),        // a/1, hidden
            SidebarRow::single(2, RowKind::Selectable).with_depth(1),        // a/2, hidden
            SidebarRow::single(3, RowKind::Selectable).with_collapsed(true), // b
            SidebarRow::single(4, RowKind::Selectable).with_depth(1),        // b/1, hidden
        ])
        .expect("the rows stay inside every bound");
    sidebar
}

/// Returns one sidebar of two sections: two rows in section 0 and one row in
/// section 1. Neither section is collapsed yet.
fn two_sections() -> SidebarState<RowId> {
    let mut sidebar = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable).with_section(0), // task one
            SidebarRow::single(1, RowKind::Selectable).with_section(0), // task two
            SidebarRow::single(2, RowKind::Selectable).with_section(1), // src
        ])
        .expect("three rows stay inside every bound");
    sidebar
}

/// Returns the visible part of every row as `(id, first_line, lines, top_row)`.
fn visible(sidebar: &SidebarState<RowId>) -> Vec<(RowId, u16, u16, u16)> {
    sidebar
        .placements()
        .iter()
        .map(|placement| {
            (
                *placement.row(),
                placement.first_line(),
                placement.lines(),
                placement.top_row(),
            )
        })
        .collect()
}

#[test]
fn a_viewport_clips_the_first_and_the_last_row_of_a_scrolled_list() {
    let mut sidebar = double_rows(5, 4);
    sidebar.select(&3);

    // Eight terminal rows do not fit into five, so the viewport shows the last
    // line of one row, two whole rows, and the first line of another row.
    assert_eq!(sidebar.total_lines(), 8);
    assert_eq!(sidebar.first_line(), 3);
    assert_eq!(
        visible(&sidebar),
        vec![(1, 1, 1, 0), (2, 0, 2, 1), (3, 0, 2, 3)],
    );
}

#[test]
fn a_scroll_counts_terminal_rows_instead_of_rows() {
    let mut sidebar = double_rows(3, 4);

    sidebar.select(&0);
    assert_eq!(sidebar.first_line(), 0);
    // One row down moves the offset by the height of that row, not by one row.
    sidebar.reduce(&SidebarInput::Move(ListMotion::Down(1)));
    assert_eq!(sidebar.first_line(), 1);
    sidebar.reduce(&SidebarInput::Move(ListMotion::Down(1)));
    assert_eq!(sidebar.first_line(), 3);
    sidebar.reduce(&SidebarInput::Move(ListMotion::LastRow));
    assert_eq!(sidebar.first_line(), 5);
}

#[test]
fn a_row_that_is_taller_than_the_viewport_shows_its_first_line() {
    let mut sidebar = SidebarState::new(1);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable),
            SidebarRow::new(1, height(4), RowKind::Selectable),
        ])
        .expect("the rows stay inside the bounds");

    sidebar.select(&1);
    assert_eq!(sidebar.first_line(), 1);
    assert_eq!(visible(&sidebar), vec![(1, 0, 1, 0)]);
}

#[test]
fn a_replacement_that_changes_the_row_heights_keeps_the_selection_visible() {
    let mut sidebar = single_rows(4, 8);
    sidebar.select(&7);
    assert_eq!(sidebar.first_line(), 4);

    let rows = (0..8)
        .map(|id| SidebarRow::new(id, height(2), RowKind::Selectable))
        .collect();
    sidebar
        .set_rows(rows)
        .expect("the rows stay inside the bounds");

    assert_eq!(sidebar.selected(), Some(&7));
    assert_eq!(sidebar.total_lines(), 16);
    assert_eq!(sidebar.first_line(), 12);
    assert_eq!(visible(&sidebar), vec![(6, 0, 2, 0), (7, 0, 2, 2)]);
}

#[test]
fn a_resize_scrolls_the_selected_row_back_into_the_viewport() {
    let mut sidebar = single_rows(4, 10);
    sidebar.select(&9);
    assert_eq!(sidebar.first_line(), 6);

    // A taller viewport shows more rows above the selection and never scrolls
    // past the last row.
    sidebar.set_height_rows(8);
    assert_eq!(sidebar.first_line(), 2);
    assert_eq!(sidebar.placements().len(), 8);

    // A shorter viewport keeps the selected row.
    sidebar.set_height_rows(2);
    assert_eq!(sidebar.first_line(), 8);
    assert_eq!(visible(&sidebar), vec![(8, 0, 1, 0), (9, 0, 1, 1)]);

    // A closed sidebar shows no row at all.
    sidebar.set_height_rows(0);
    assert_eq!(sidebar.first_line(), 0);
    assert!(sidebar.placements().is_empty());
}

#[test]
fn a_scroll_margin_keeps_rows_around_the_selection() {
    let mut sidebar = single_rows(9, 20);
    sidebar.set_scroll_margin(3);
    sidebar.select(&0);
    sidebar.reduce(&SidebarInput::Move(ListMotion::ToRow(5)));

    // The margin holds three rows below the selection inside the viewport.
    assert_eq!(sidebar.first_line(), 0);
    sidebar.reduce(&SidebarInput::Move(ListMotion::ToRow(6)));
    assert_eq!(sidebar.first_line(), 1);

    // The margin stops at the last row instead of scrolling past it.
    sidebar.reduce(&SidebarInput::Move(ListMotion::LastRow));
    assert_eq!(sidebar.first_line(), 11);
}

#[test]
fn a_replacement_keeps_the_selected_identity_at_its_new_position() {
    let mut sidebar = single_rows(4, 4);
    sidebar.select(&2);

    sidebar
        .set_rows(vec![
            SidebarRow::single(9, RowKind::Selectable),
            SidebarRow::single(2, RowKind::Selectable),
        ])
        .expect("the rows stay inside the bounds");

    assert_eq!(sidebar.selected(), Some(&2));
    assert_eq!(sidebar.selected_index(), Some(1));
}

#[test]
fn a_removed_selection_moves_to_the_nearest_selectable_row() {
    let mut sidebar = single_rows(4, 4);
    sidebar.select(&2);

    // The identity disappears, so the nearest selectable row at or after the
    // old position takes the selection.
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable),
            SidebarRow::single(1, RowKind::Inert),
            SidebarRow::single(5, RowKind::Inert),
            SidebarRow::single(6, RowKind::Selectable),
        ])
        .expect("the rows stay inside the bounds");
    assert_eq!(sidebar.selected(), Some(&6));

    // Without a selectable row behind or ahead the sidebar holds no selection.
    sidebar
        .set_rows(vec![SidebarRow::single(4, RowKind::Inert)])
        .expect("the rows stay inside the bounds");
    assert_eq!(sidebar.selected(), None);

    // An empty list holds no selection and shows no row.
    sidebar
        .set_rows(Vec::new())
        .expect("an empty list is valid");
    assert_eq!(sidebar.selected(), None);
    assert_eq!(sidebar.first_line(), 0);
    assert!(sidebar.placements().is_empty());
}

#[test]
fn an_empty_sidebar_reduces_every_input_without_an_event() {
    let mut sidebar: SidebarState<RowId> = SidebarState::new(4);
    let action = SidebarAction::new("open").expect("the name stays inside the bound");

    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Down(2))),
        None
    );
    assert_eq!(sidebar.reduce(&SidebarInput::Activate), None);
    assert_eq!(sidebar.reduce(&SidebarInput::Request(action)), None);
    assert_eq!(sidebar.selected(), None);
}

#[test]
fn a_move_skips_the_inert_rows_and_stops_at_the_two_ends() {
    let mut sidebar = SidebarState::new(6);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Inert),
            SidebarRow::single(1, RowKind::Selectable),
            SidebarRow::single(2, RowKind::Inert),
            SidebarRow::single(3, RowKind::Inert),
            SidebarRow::single(4, RowKind::Selectable),
            SidebarRow::single(5, RowKind::Inert),
        ])
        .expect("the rows stay inside the bounds");

    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::ToRow(0))),
        Some(SidebarEvent::SelectionChanged { row: 1 }),
    );
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Down(2))),
        Some(SidebarEvent::SelectionChanged { row: 4 }),
    );
    // The last row is inert, so the move takes the nearest row behind it.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::LastRow)),
        None
    );
    assert_eq!(sidebar.selected(), Some(&4));
    // A move up stops at the first selectable row and never wraps.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Up(9))),
        Some(SidebarEvent::SelectionChanged { row: 1 }),
    );
    assert_eq!(sidebar.reduce(&SidebarInput::Move(ListMotion::Up(9))), None);
}

#[test]
fn a_reduction_reports_the_activation_and_the_action_of_the_selected_row() {
    let mut sidebar = single_rows(4, 3);
    sidebar.select(&1);
    let action = SidebarAction::new("delete").expect("the name stays inside the bound");

    assert_eq!(
        sidebar.reduce(&SidebarInput::Activate),
        Some(SidebarEvent::Activated { row: 1 }),
    );
    assert_eq!(
        sidebar.reduce(&SidebarInput::Request(action.clone())),
        Some(SidebarEvent::ActionRequested { row: 1, action }),
    );
    // The reduction changes the selection only, so the two inputs run nothing.
    assert_eq!(sidebar.selected(), Some(&1));
}

#[test]
fn an_action_name_reports_every_bound_that_it_passes() {
    let long = "x".repeat(SIDEBAR_ACTION_CHARS_MAX + 1);
    assert!(matches!(
        SidebarAction::new(&long),
        Err(SidebarError::Action { .. }),
    ));
    assert!(matches!(
        SidebarAction::new("open\nfile"),
        Err(SidebarError::Action { .. }),
    ));
}

#[test]
fn a_refused_row_list_leaves_the_previous_rows_in_place() {
    let mut sidebar = single_rows(4, 2);
    sidebar.select(&1);

    let tall = vec![
        SidebarRow::single(7, RowKind::Selectable),
        SidebarRow::new(8, height(SIDEBAR_ROW_LINES_MAX + 1), RowKind::Selectable),
    ];
    assert_eq!(
        sidebar.set_rows(tall),
        Err(SidebarError::RowHeight {
            index: 1,
            height: SIDEBAR_ROW_LINES_MAX + 1,
            max: SIDEBAR_ROW_LINES_MAX,
        }),
    );

    let many = (0..=SIDEBAR_ROWS_MAX)
        .map(|id| SidebarRow::single(id as RowId, RowKind::Selectable))
        .collect();
    assert_eq!(
        sidebar.set_rows(many),
        Err(SidebarError::Rows {
            rows: SIDEBAR_ROWS_MAX + 1,
            max: SIDEBAR_ROWS_MAX,
        }),
    );

    assert_eq!(sidebar.rows().len(), 2);
    assert_eq!(sidebar.selected(), Some(&1));
}

#[test]
fn a_placement_never_reaches_outside_the_sidebar_rectangle() {
    let sidebar = double_rows(6, 3);
    let area = Rect::new(3, 2, 12, 4);

    for placement in sidebar.placements() {
        let row = placement.area(area);
        assert!(row.x >= area.x && row.right() <= area.right());
        assert!(row.y >= area.y && row.bottom() <= area.bottom());
    }
    // The viewport is taller than the rectangle, so the last placement is empty
    // instead of writing below the rectangle.
    let last = sidebar.placements().last().expect("the sidebar shows rows");
    assert!(last.area(area).is_empty());
}

#[test]
fn a_callback_draws_several_lines_and_the_render_clips_every_cell() {
    let area = Rect::new(1, 1, 6, 4);
    let mut target = Buffer::empty(Rect::new(0, 0, 8, 6));
    let sidebar = double_rows(4, 3);

    sidebar
        .render(&mut target, area, |canvas, placement| {
            canvas.fill(Style::default().add_modifier(Modifier::DIM));
            for line in 0..canvas.lines() {
                let row = *placement.row();
                canvas.draw(line, 0, &format!("{row}{line}--------"), Style::default());
            }
        })
        .expect("the callback stays inside every bound");

    let text = |y: u16| {
        (area.x..area.right())
            .map(|x| target.cell((x, y)).expect("the cell exists").symbol())
            .collect::<String>()
    };
    assert_eq!(text(1), "00----");
    assert_eq!(text(2), "01----");
    assert_eq!(text(3), "10----");
    assert_eq!(text(4), "11----");
    // The render writes inside its own rectangle only.
    for y in 0..target.area.height {
        for x in 0..target.area.width {
            if area.contains((x, y).into()) {
                continue;
            }
            assert_eq!(
                target.cell((x, y)).expect("the cell exists").symbol(),
                " ",
                "the render wrote outside its area at {x},{y}",
            );
        }
    }
}

#[test]
fn a_clipped_first_row_draws_only_its_visible_lines() {
    let area = Rect::new(0, 0, 4, 3);
    let mut target = Buffer::empty(area);
    let mut sidebar = double_rows(3, 3);
    sidebar.select(&1);
    assert_eq!(sidebar.first_line(), 1);

    let result = sidebar.render(&mut target, area, |canvas, placement| {
        // The visible part of the first row holds one line, so the second line
        // of that row is outside the canvas.
        canvas.draw(0, 0, &format!("r{}", placement.row()), Style::default());
        canvas.draw(1, 0, "over", Style::default());
    });

    assert_eq!(result, Err(SidebarError::Line { line: 1, lines: 1 }),);
    let text = |y: u16| {
        (0..area.width)
            .map(|x| target.cell((x, y)).expect("the cell exists").symbol())
            .collect::<String>()
    };
    // The refused draw wrote nothing, and every other row still reached the
    // buffer.
    assert_eq!(text(0), "r0  ");
    assert_eq!(text(1), "r1  ");
    assert_eq!(text(2), "over");
}

#[test]
fn a_draw_reports_the_cell_the_label_and_the_visible_output_bounds() {
    let area = Rect::new(0, 0, 4, 1);
    let mut target = Buffer::empty(area);
    let sidebar = single_rows(1, 1);

    let column = sidebar.render(&mut target, area, |canvas, _| {
        canvas.draw(0, area.width, "outside", Style::default());
    });
    assert_eq!(
        column,
        Err(SidebarError::Cell {
            column: area.width,
            width: area.width,
        }),
    );

    let label = sidebar.render(&mut target, area, |canvas, _| {
        canvas.draw(
            0,
            0,
            &"x".repeat(SIDEBAR_LABEL_CHARS_MAX + 1),
            Style::default(),
        );
    });
    assert_eq!(
        label,
        Err(SidebarError::Label {
            chars: SIDEBAR_LABEL_CHARS_MAX + 1,
            max: SIDEBAR_LABEL_CHARS_MAX,
        }),
    );

    let draws = sidebar.render(&mut target, area, |canvas, _| {
        for _ in 0..=SIDEBAR_ROW_DRAWS_MAX {
            canvas.style_span(0, 0, 1, Style::default());
        }
    });
    assert_eq!(
        draws,
        Err(SidebarError::VisibleOutput {
            max: SIDEBAR_ROW_DRAWS_MAX,
        }),
    );
}

#[test]
fn a_rectangle_outside_the_buffer_returns_the_error_and_changes_no_cell() {
    let buffer = Rect::new(0, 0, 6, 2);
    let mut target = Buffer::empty(buffer);
    let untouched = target.clone();
    let sidebar = single_rows(2, 2);

    // The rectangle starts below the last buffer row, which is the shape that
    // a host produces from a stale frame size.
    let area = Rect::new(0, 4, 6, 2);
    let outcome = sidebar.render(&mut target, area, |canvas, _placement| {
        canvas.draw(0, 0, "x", Style::default());
    });
    assert_eq!(outcome, Err(SidebarError::Area { area, buffer }));
    assert_eq!(target, untouched, "a refused rectangle paints no cell");
}

#[test]
fn a_collapsed_row_contributes_no_line_and_no_placement() {
    let sidebar = tree_with_two_collapsed_directories();

    // Only `a` and `b` are visible, so they are the only rows that count
    // toward the scroll and the only rows that receive a placement.
    assert_eq!(sidebar.total_lines(), 2);
    let placed: Vec<RowId> = sidebar
        .placements()
        .iter()
        .map(|placement| *placement.row())
        .collect();
    assert_eq!(placed, vec![0, 3]);
}

#[test]
fn a_downward_move_over_a_collapsed_parent_lands_on_the_next_visible_row() {
    let mut sidebar = tree_with_two_collapsed_directories();
    sidebar.select(&0);

    // The move counts visible rows only, so it skips both hidden children of
    // `a` and lands directly on `b`, the next visible row.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Down(1))),
        Some(SidebarEvent::SelectionChanged { row: 3 }),
    );
    // The same move back up returns to `a` in one step.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Up(1))),
        Some(SidebarEvent::SelectionChanged { row: 0 }),
    );
}

#[test]
fn to_row_on_a_hidden_row_resolves_like_an_inert_row() {
    let mut sidebar = tree_with_two_collapsed_directories();

    // Row 1 is `a/1`, hidden below the collapsed `a`. The move takes the
    // nearest visible, selectable row in the direction of travel.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::ToRow(1))),
        Some(SidebarEvent::SelectionChanged { row: 3 }),
    );
}

#[test]
fn last_row_moves_to_the_last_visible_selectable_row() {
    let mut sidebar = tree_with_two_collapsed_directories();
    sidebar.select(&0);

    // Row 4, `b/1`, is the last row of the flat list and it is hidden below
    // the collapsed `b`, so the last visible row, `b`, takes the selection.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::LastRow)),
        Some(SidebarEvent::SelectionChanged { row: 3 }),
    );
}

#[test]
fn a_row_deeper_than_the_depth_bound_is_refused() {
    let mut sidebar: SidebarState<RowId> = SidebarState::new(4);
    let rows = vec![
        SidebarRow::single(0, RowKind::Selectable),
        SidebarRow::single(1, RowKind::Selectable).with_depth(SIDEBAR_ROW_DEPTH_MAX + 1),
    ];

    assert_eq!(
        sidebar.set_rows(rows),
        Err(SidebarError::Depth {
            index: 1,
            depth: SIDEBAR_ROW_DEPTH_MAX + 1,
            max: SIDEBAR_ROW_DEPTH_MAX,
        }),
    );
    assert!(
        sidebar.rows().is_empty(),
        "the refused list changes nothing"
    );
}

#[test]
fn a_hidden_row_never_takes_the_selection_or_the_selection_focus() {
    let mut sidebar = tree_with_two_collapsed_directories();

    // Selecting a hidden identity leaves the selection where it was, exactly
    // as selecting an inert row does.
    assert_eq!(sidebar.select(&1), None);
    assert_eq!(sidebar.selected(), None);
}

#[test]
fn a_collapsed_section_shows_its_first_row_and_hides_the_rest() {
    let mut sidebar = two_sections();

    sidebar
        .set_sections(vec![true, false])
        .expect("two sections stay inside the bound");

    // `task one` is the first row of section 0, so it stays visible, exactly
    // as a collapsed tree row stays visible and hides only the rows below
    // it. `task two` is the second row of the same section, so it hides.
    assert_eq!(sidebar.total_lines(), 2);
    let placed: Vec<RowId> = sidebar
        .placements()
        .iter()
        .map(|placement| *placement.row())
        .collect();
    assert_eq!(placed, vec![0, 2]);
}

#[test]
fn a_move_crosses_a_collapsed_section_and_lands_on_its_first_row() {
    let mut sidebar = two_sections();
    sidebar
        .set_sections(vec![true, false])
        .expect("two sections stay inside the bound");

    // A downward move from no selection skips the one hidden task and lands
    // directly on the next visible row.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Down(1))),
        Some(SidebarEvent::SelectionChanged { row: 2 }),
    );
    // The same move back up crosses the collapsed section and stops on its
    // own first row, `task one`, because that row is selectable.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Up(1))),
        Some(SidebarEvent::SelectionChanged { row: 0 }),
    );
}

#[test]
fn a_move_skips_a_collapsed_section_whose_first_row_is_inert() {
    // The section's first row stays visible, but an inert row never takes
    // the selection, so a host that draws an inert heading and collapses the
    // section leaves that row on screen with no motion able to reach it.
    let mut sidebar: SidebarState<RowId> = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Inert).with_section(0), // heading
            SidebarRow::single(1, RowKind::Selectable).with_section(0), // task
            SidebarRow::single(2, RowKind::Selectable).with_section(1), // src
        ])
        .expect("three rows stay inside every bound");
    sidebar
        .set_sections(vec![true, false])
        .expect("two sections stay inside the bound");
    sidebar.select(&2);

    assert_eq!(sidebar.reduce(&SidebarInput::Move(ListMotion::Up(1))), None);
    assert_eq!(sidebar.selected(), Some(&2));
}

#[test]
fn a_selected_row_that_a_section_collapses_moves_to_a_visible_row() {
    let mut sidebar = two_sections();
    sidebar.select(&1); // task two

    sidebar
        .set_sections(vec![true, false])
        .expect("two sections stay inside the bound");

    // Task two hides, so the selection moves to the nearest visible row
    // ahead of its old position.
    assert_eq!(sidebar.selected(), Some(&2));
}

#[test]
fn a_collapsed_sections_line_count_is_the_height_of_its_first_row() {
    let mut sidebar: SidebarState<RowId> = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::new(0, height(2), RowKind::Selectable).with_section(0), // heading
            SidebarRow::single(1, RowKind::Selectable).with_section(0),         // task, hides
            SidebarRow::single(2, RowKind::Selectable).with_section(1),         // src
        ])
        .expect("three rows stay inside every bound");

    sidebar
        .set_sections(vec![true, false])
        .expect("two sections stay inside the bound");

    // The collapsed section contributes the two terminal rows of its own
    // first row, plus the one row of the open section.
    assert_eq!(sidebar.total_lines(), 3);
}

#[test]
fn a_section_that_no_row_carries_still_collapses_correctly() {
    // Section 0's flag stays reachable even while no row of section 0
    // exists yet, so a host may declare a section before it publishes a row.
    // Collapsing an empty section hides no row and changes nothing.
    let mut sidebar: SidebarState<RowId> = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable).with_section(1),
        ])
        .expect("one row stays inside every bound");

    sidebar
        .set_sections(vec![true, false])
        .expect("two sections stay inside the bound");

    assert_eq!(sidebar.total_lines(), 1);
    assert!(!sidebar.placements().is_empty());
}

#[test]
fn a_row_without_a_declared_section_stays_visible_by_default() {
    // A sidebar that never calls `set_sections` hides no row through the
    // section axis, so every present consumer keeps its current behavior.
    let sidebar = single_rows(4, 3);

    assert!(sidebar.sections().is_empty());
    assert_eq!(sidebar.total_lines(), 3);
}

#[test]
fn a_refused_section_list_leaves_the_previous_sections_and_selection_in_place() {
    let mut sidebar = two_sections();
    sidebar.select(&2);

    let too_many = vec![false; SIDEBAR_SECTIONS_MAX + 1];
    assert_eq!(
        sidebar.set_sections(too_many),
        Err(SidebarError::Sections {
            sections: SIDEBAR_SECTIONS_MAX + 1,
            max: SIDEBAR_SECTIONS_MAX,
        }),
    );

    assert!(sidebar.sections().is_empty());
    assert_eq!(sidebar.selected(), Some(&2));
}

#[test]
fn a_collapsed_section_and_a_collapsed_row_hide_together() {
    // A section carries its own collapsed rows too. `a` is the first row of
    // section 0, so it stays visible whether or not the section collapses,
    // exactly as it stays visible under its own collapsed flag.
    let mut sidebar: SidebarState<RowId> = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable)
                .with_section(0)
                .with_collapsed(true), // a, collapsed, first row of section 0
            SidebarRow::single(1, RowKind::Selectable)
                .with_section(0)
                .with_depth(1), // a/1, hidden below `a` alone
            SidebarRow::single(2, RowKind::Selectable).with_section(1), // src
        ])
        .expect("three rows stay inside every bound");

    // Before the section collapses, `a` stays visible and `a/1` is already
    // hidden below its own collapsed row.
    assert_eq!(sidebar.total_lines(), 2);

    sidebar
        .set_sections(vec![true, false])
        .expect("two sections stay inside the bound");

    // The section collapse hides no further row: `a` was already visible as
    // the section's own first row, and `a/1` was already hidden below `a`.
    assert_eq!(sidebar.total_lines(), 2);
}

#[test]
fn a_shared_sidebar_answers_the_window_of_a_height_it_never_stored() {
    let mut sidebar = single_rows(3, 10);
    sidebar.set_scroll_margin(1);
    sidebar
        .select(&9)
        .expect("the last row takes the selection");

    // The stored height answers the stored window, so the two rules agree.
    let stored = sidebar.window_for_height(sidebar.height_rows(), 1);
    assert_eq!(stored.first_line(), sidebar.first_line());
    assert_eq!(stored.total_lines(), sidebar.total_lines());
    assert_eq!(stored.placements(), sidebar.placements());

    // A taller rectangle answers more rows without a mutable borrow, and the
    // sidebar keeps the height that it stored.
    let tree: &SidebarState<RowId> = &sidebar;
    let taller = tree.window_for_height(6, 1);
    assert_eq!(taller.placements().len(), 6);
    assert_eq!(taller.first_line(), 4);
    assert_eq!(sidebar.height_rows(), 3);
}

#[test]
fn a_parent_motion_climbs_one_depth_at_a_time_and_stops_at_the_top() {
    let mut sidebar = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable), // depth 0
            SidebarRow::single(1, RowKind::Selectable).with_depth(1), // depth 1
            SidebarRow::single(2, RowKind::Selectable).with_depth(2), // depth 2
        ])
        .expect("three rows stay inside every bound");
    sidebar.select(&2);

    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Parent)),
        Some(SidebarEvent::SelectionChanged { row: 1 }),
    );
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Parent)),
        Some(SidebarEvent::SelectionChanged { row: 0 }),
    );
    // A top-level row holds no parent, so the motion produces no event and
    // leaves the selection where it is.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Parent)),
        None
    );
    assert_eq!(sidebar.selected(), Some(&0));
}

#[test]
fn a_parent_motion_climbs_past_an_inert_parent_to_the_nearest_selectable_ancestor() {
    let mut sidebar = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable), // depth 0, grandparent
            SidebarRow::single(1, RowKind::Inert).with_depth(1), // depth 1, inert parent
            SidebarRow::single(2, RowKind::Selectable).with_depth(2), // depth 2, selected row
        ])
        .expect("three rows stay inside every bound");
    sidebar.select(&2);

    // The diff view's changes panel marks a directory row inert, so the
    // climb must reach past it instead of refusing the motion outright.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Parent)),
        Some(SidebarEvent::SelectionChanged { row: 0 }),
    );
}

#[test]
fn a_parent_motion_ignores_an_unrelated_collapsed_row_earlier_in_the_list() {
    let mut sidebar = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable).with_collapsed(true), // a, collapsed
            SidebarRow::single(1, RowKind::Selectable).with_depth(1),        // a/1, hidden
            SidebarRow::single(2, RowKind::Selectable),                      // b
            SidebarRow::single(3, RowKind::Selectable).with_depth(1),        // b/1
        ])
        .expect("four rows stay inside every bound");
    sidebar.select(&3);

    // The collapsed directory `a` sits above `b` in the row list, but it
    // holds no row of `b/1`, so the climb never reaches it.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Parent)),
        Some(SidebarEvent::SelectionChanged { row: 2 }),
    );
}

#[test]
fn a_parent_motion_never_crosses_into_an_earlier_section() {
    let mut sidebar = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable).with_section(0), // task, section 0
            SidebarRow::single(1, RowKind::Selectable)
                .with_section(1)
                .with_depth(1), // src/main.rs, section 1, no shallower row of its own
        ])
        .expect("two rows stay inside every bound");
    sidebar.select(&1);

    // The only row of a strictly smaller depth sits in the previous section,
    // so the climb finds no parent instead of crossing into it.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Parent)),
        None
    );
    assert_eq!(sidebar.selected(), Some(&1));
}

#[test]
fn a_parent_motion_never_lands_on_a_hidden_row_of_a_collapsed_section() {
    let mut sidebar: SidebarState<RowId> = SidebarState::new(4);
    sidebar
        .set_rows(vec![
            SidebarRow::single(0, RowKind::Selectable).with_section(0), // heading, first row
            SidebarRow::single(1, RowKind::Selectable)
                .with_section(0)
                .with_depth(1), // task, hides once the section collapses
            SidebarRow::single(2, RowKind::Selectable)
                .with_section(0)
                .with_depth(2), // task detail, hides once the section collapses
        ])
        .expect("three rows stay inside every bound");
    sidebar.select(&2);

    // Before the section collapses, Parent climbs one depth at a time.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Parent)),
        Some(SidebarEvent::SelectionChanged { row: 1 }),
    );

    sidebar
        .set_sections(vec![true])
        .expect("one section stays inside the bound");

    // Row 1 and row 2 hide, so the collapse already moved the selection to
    // the section's own first row, row 0.
    assert_eq!(sidebar.selected(), Some(&0));
    // Parent from the section's own first row finds none, never the hidden
    // row 1 that used to be its parent.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(ListMotion::Parent)),
        None
    );
    assert_eq!(sidebar.selected(), Some(&0));
}

#[test]
fn parent_row_climbs_past_an_unacceptable_row_to_its_own_parent() {
    // A hidden row and an inert row both refuse the climb the same way, so
    // one acceptable flag stands for either. Depth 0 is the acceptable
    // grandparent, depth 1 is the unacceptable parent, and depth 2 is the
    // row itself.
    let rows = [
        ParentScanRow::new(0, 0, true),
        ParentScanRow::new(1, 0, false),
        ParentScanRow::new(2, 0, true),
    ];

    assert_eq!(parent_row(rows.iter().copied(), 2), Some(0));
}

#[test]
fn parent_row_stops_at_a_section_boundary() {
    let rows = [
        ParentScanRow::new(0, 0, true),
        ParentScanRow::new(1, 1, true),
    ];

    // Row 1's only shallower row sits in section 0, so the climb finds none.
    assert_eq!(parent_row(rows.iter().copied(), 1), None);
}

#[test]
fn parent_row_reports_no_parent_for_a_position_past_the_end_of_the_rows() {
    let rows = [ParentScanRow::new(0, 0, true)];

    assert_eq!(parent_row(rows.iter().copied(), 1), None);
}
