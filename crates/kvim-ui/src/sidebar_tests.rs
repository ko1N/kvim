//! Tests for the sidebar rows, the selection, the scrolling, and the rendering.

use std::num::NonZeroU16;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::{
    RowKind, SIDEBAR_ACTION_CHARS_MAX, SIDEBAR_LABEL_CHARS_MAX, SIDEBAR_ROW_DRAWS_MAX,
    SIDEBAR_ROW_LINES_MAX, SIDEBAR_ROWS_MAX, SidebarAction, SidebarError, SidebarEvent,
    SidebarInput, SidebarMotion, SidebarRow, SidebarState,
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
    sidebar.reduce(&SidebarInput::Move(SidebarMotion::Down(1)));
    assert_eq!(sidebar.first_line(), 1);
    sidebar.reduce(&SidebarInput::Move(SidebarMotion::Down(1)));
    assert_eq!(sidebar.first_line(), 3);
    sidebar.reduce(&SidebarInput::Move(SidebarMotion::LastRow));
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
    sidebar.reduce(&SidebarInput::Move(SidebarMotion::ToRow(5)));

    // The margin holds three rows below the selection inside the viewport.
    assert_eq!(sidebar.first_line(), 0);
    sidebar.reduce(&SidebarInput::Move(SidebarMotion::ToRow(6)));
    assert_eq!(sidebar.first_line(), 1);

    // The margin stops at the last row instead of scrolling past it.
    sidebar.reduce(&SidebarInput::Move(SidebarMotion::LastRow));
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
        sidebar.reduce(&SidebarInput::Move(SidebarMotion::Down(2))),
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
        sidebar.reduce(&SidebarInput::Move(SidebarMotion::ToRow(0))),
        Some(SidebarEvent::SelectionChanged { row: 1 }),
    );
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(SidebarMotion::Down(2))),
        Some(SidebarEvent::SelectionChanged { row: 4 }),
    );
    // The last row is inert, so the move takes the nearest row behind it.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(SidebarMotion::LastRow)),
        None
    );
    assert_eq!(sidebar.selected(), Some(&4));
    // A move up stops at the first selectable row and never wraps.
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(SidebarMotion::Up(9))),
        Some(SidebarEvent::SelectionChanged { row: 1 }),
    );
    assert_eq!(
        sidebar.reduce(&SidebarInput::Move(SidebarMotion::Up(9))),
        None
    );
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
