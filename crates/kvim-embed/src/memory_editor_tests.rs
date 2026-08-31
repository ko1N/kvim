use super::*;
use kvim_editor::Selection;
use std::time::Duration;

fn editor(text: &str, area: Rect) -> MemoryEditor {
    MemoryEditor::open(text, EditorSettings::default(), area).expect("the fixture is valid")
}

fn pointer(column: u16, row: u16, action: PointerAction) -> PointerEvent {
    PointerEvent::new(
        CellPosition::new(column, row),
        PointerModifiers::default(),
        action,
    )
}

fn no_number_editor(text: &str, area: Rect) -> MemoryEditor {
    let mut settings = EditorSettings::default();
    settings.display.number = false;
    settings.display.relative_number = false;
    settings.display.sidescrolloff_cells = 0;
    MemoryEditor::open(text, settings, area).expect("the fixture is valid")
}

#[test]
fn supplied_text_edits_through_commands_and_literals() {
    let mut editor = editor("alpha\n", Rect::new(0, 0, 20, 3));
    assert_eq!(editor.text(), "alpha\n");
    assert_eq!(
        editor.command(Command::InsertAtLineEnd, None, None),
        Ok(CommandOutcome::Applied)
    );
    assert_eq!(editor.literal(" beta"), LiteralOutcome::Changed);
    editor
        .command(Command::ReturnToNormal, None, None)
        .expect("the command has no register");
    assert_eq!(editor.text(), "alpha beta\n");
    assert_eq!(editor.literal("refused"), LiteralOutcome::Refused);
    assert_eq!(editor.tick(Duration::from_secs(1)), TickOutcome::Unchanged);
}

#[test]
fn geometry_is_validated_before_render_changes_cells() {
    assert_eq!(
        MemoryEditor::open("text", EditorSettings::default(), Rect::new(0, 0, 0, 2)).unwrap_err(),
        MemoryEditorError::EmptyGeometry
    );

    let editor = editor("text", Rect::new(3, 1, 8, 2));
    let mut cells = Buffer::empty(Rect::new(0, 0, 8, 2));
    let before = cells.clone();
    assert!(matches!(
        editor.render(&mut cells),
        Err(MemoryEditorError::GeometryOutsideBuffer { .. })
    ));
    assert_eq!(cells, before);
}

#[test]
fn text_and_insert_bounds_refuse_without_mutation() {
    let limit = BufferBytesMax::new(5).expect("the limit is valid");
    let mut settings = EditorSettings::default();
    settings.files.max_file_bytes = limit.get();
    settings.files.recovery_max_bytes = limit.get();
    assert!(matches!(
        MemoryEditor::open("longer", settings, Rect::new(0, 0, 8, 2)),
        Err(MemoryEditorError::Text(LoadError::TooLarge { .. }))
    ));

    let mut editor = MemoryEditor::open("12345", settings, Rect::new(0, 0, 8, 2))
        .expect("the text fits exactly");
    editor
        .command(Command::InsertAtLineEnd, None, None)
        .expect("the command has no register");
    assert_eq!(editor.literal("6"), LiteralOutcome::Rejected);
    assert_eq!(editor.text(), "12345");
}

#[test]
fn invalid_register_is_atomic() {
    let mut editor = editor("alpha\n", Rect::new(0, 0, 12, 2));
    let before = editor.text();
    assert_eq!(
        editor.command(Command::DeleteLine, None, Some('!')),
        Err(MemoryEditorError::InvalidRegisterName { name: '!' })
    );
    assert_eq!(editor.text(), before);
    assert_eq!(editor.mode(), Mode::Normal);
}

#[test]
fn resize_accounts_for_the_line_number_gutter() {
    let mut editor = editor("0123456789\n", Rect::new(0, 0, 8, 2));
    editor
        .command(Command::MoveLineEnd, None, None)
        .expect("the command has no register");
    editor
        .resize(Rect::new(0, 0, 6, 2))
        .expect("the area is valid");
    let mut cells = Buffer::empty(Rect::new(0, 0, 6, 2));
    let rendered = editor.render(&mut cells).expect("the geometry fits");
    // The gutter and the reserved scrollbar column both leave the text, so the
    // cursor sits on the last text cell of the narrow surface.
    assert_eq!(rendered.cursor().x, 3);
    assert_eq!(cells[(0, 0)].symbol(), "1");
}

#[test]
fn horizontal_unicode_scroll_uses_source_columns_and_terminal_cells() {
    let mut settings = EditorSettings::default();
    settings.display.number = false;
    settings.display.relative_number = false;
    settings.display.sidescrolloff_cells = 0;
    let mut editor = MemoryEditor::open("a界bcdef\n", settings, Rect::new(0, 0, 4, 2))
        .expect("the fixture is valid");
    editor
        .command(Command::MoveLineEnd, None, None)
        .expect("the command has no register");
    let mut cells = Buffer::empty(Rect::new(0, 0, 4, 2));
    let rendered = editor.render(&mut cells).expect("the geometry fits");
    // The surface holds four cells and reserves one of them for the scrollbar,
    // so three text cells follow the cursor over the wide character.
    assert_eq!(rendered.cursor(), Position::new(2, 0));
    assert_eq!(cells[(0, 0)].symbol(), "d");
}

#[test]
fn clipping_does_not_split_a_wide_character() {
    let row = layout_row("ab界c", 4, 0, 3);
    assert_eq!(
        row.into_iter().map(|cell| cell.symbol).collect::<Vec<_>>(),
        vec![RowSymbol::Char('a'), RowSymbol::Char('b'), RowSymbol::Blank]
    );
    let row = layout_row("ab界c", 4, 0, 4);
    assert_eq!(
        row.into_iter().map(|cell| cell.symbol).collect::<Vec<_>>(),
        vec![
            RowSymbol::Char('a'),
            RowSymbol::Char('b'),
            RowSymbol::Char('界'),
            RowSymbol::WideTail,
        ]
    );
}

#[test]
fn rendering_draws_text_numbers_and_cursor() {
    let editor = editor("alpha\nbeta\n", Rect::new(2, 1, 12, 3));
    let mut cells = Buffer::empty(Rect::new(0, 0, 16, 5));
    let outcome = editor.render(&mut cells).expect("the geometry fits");
    let first: String = (2..14).map(|x| cells[(x, 1)].symbol()).collect();
    let second: String = (2..14).map(|x| cells[(x, 2)].symbol()).collect();
    assert!(first.contains("1 alpha"));
    assert!(second.contains("1 beta"));
    assert_eq!(outcome.cursor(), Position::new(4, 1));
}

#[test]
fn pointer_click_wheel_and_drag_use_surface_cells() {
    let mut editor = no_number_editor("zero\none\ntwo\nthree\nfour\n", Rect::new(4, 2, 8, 2));

    assert_eq!(
        editor.pointer(pointer(6, 2, PointerAction::Press(PointerButton::Left))),
        PointerOutcome::Changed
    );
    assert_eq!(editor.window.cursor().column().get(), 2);
    assert_eq!(
        editor.pointer(pointer(
            6,
            2,
            PointerAction::Wheel(PointerWheel::new(PointerWheelDirection::Down, 1).unwrap())
        )),
        PointerOutcome::Changed
    );
    assert!(editor.window.first_line() > 0);

    editor.pointer(pointer(4, 2, PointerAction::Press(PointerButton::Left)));
    editor.pointer(pointer(7, 3, PointerAction::Drag(PointerButton::Left)));
    assert_eq!(editor.mode(), Mode::Visual);
    assert!(
        editor
            .editing
            .selection(&editor.buffer, &editor.window)
            .is_some()
    );
}

#[test]
fn memory_scrollbar_draws_proportional_glyphs_and_is_pointer_inert() {
    let mut editor = no_number_editor("zero\none\ntwo\nthree\nfour\nfive\n", Rect::new(0, 0, 6, 3));
    let mut cells = Buffer::empty(Rect::new(0, 0, 6, 3));
    editor.render(&mut cells).unwrap();
    assert_eq!(cells[(5, 0)].symbol(), "┃");
    assert_eq!(cells[(5, 1)].symbol(), "│");
    assert_eq!(cells[(5, 2)].symbol(), "│");

    let cursor = editor.window.cursor();
    assert_eq!(
        editor.pointer(pointer(5, 1, PointerAction::Press(PointerButton::Left))),
        PointerOutcome::Ignored
    );
    assert_eq!(editor.window.cursor(), cursor);
    assert_eq!(editor.pointer_drag, PointerDragState::Idle);

    assert_eq!(
        editor.pointer(pointer(
            5,
            1,
            PointerAction::Wheel(PointerWheel::new(PointerWheelDirection::Down, 1).unwrap())
        )),
        PointerOutcome::Changed
    );
    assert!(editor.window.first_line() > 0);
}

#[test]
fn memory_scrollbar_draws_only_the_track_when_the_buffer_fully_fits() {
    let editor = no_number_editor("zero\none\ntwo\n", Rect::new(0, 0, 6, 3));
    let mut cells = Buffer::empty(Rect::new(0, 0, 6, 3));
    editor.render(&mut cells).unwrap();

    for y in 0..3 {
        assert_eq!(cells[(5, y)].symbol(), "│");
    }
}

#[test]
fn disabling_memory_scrollbar_restores_the_full_text_width() {
    let mut settings = EditorSettings::default();
    settings.display.number = false;
    settings.display.relative_number = false;
    settings.display.scrollbar = false;
    settings.display.sidescrolloff_cells = 0;
    let editor = MemoryEditor::open("abcdef\n", settings, Rect::new(0, 0, 6, 1)).unwrap();
    let mut cells = Buffer::empty(Rect::new(0, 0, 6, 1));
    editor.render(&mut cells).unwrap();
    assert_eq!(cells[(5, 0)].symbol(), "f");
    assert_eq!(editor.source_at_cell(CellPosition::new(5, 0)).column, 5);
}

#[test]
fn plain_click_cancels_visual_and_keeps_a_fresh_drag_anchor() {
    let mut editor = no_number_editor("abcdef\n", Rect::new(0, 0, 8, 1));
    editor.pointer(pointer(0, 0, PointerAction::Press(PointerButton::Left)));
    editor.pointer(pointer(2, 0, PointerAction::Drag(PointerButton::Left)));
    assert_eq!(editor.mode(), Mode::Visual);

    editor.pointer(pointer(4, 0, PointerAction::Press(PointerButton::Left)));
    assert_eq!(editor.mode(), Mode::Normal);
    assert!(
        editor
            .editing
            .selection(&editor.buffer, &editor.window)
            .is_none()
    );

    editor.pointer(pointer(5, 0, PointerAction::Drag(PointerButton::Left)));
    assert_eq!(editor.mode(), Mode::Visual);
    let Selection::Characterwise(range) = editor
        .editing
        .selection(&editor.buffer, &editor.window)
        .expect("the later drag creates a fresh selection")
    else {
        panic!("a pointer drag creates a characterwise selection");
    };
    assert_eq!((range.start().get(), range.end().get()), (4, 6));
}

#[test]
fn reverse_drag_retains_the_original_press_anchor() {
    let mut editor = no_number_editor("abcdef\n", Rect::new(0, 0, 8, 1));
    editor.pointer(pointer(2, 0, PointerAction::Press(PointerButton::Left)));
    editor.pointer(pointer(5, 0, PointerAction::Drag(PointerButton::Left)));
    editor.pointer(pointer(0, 0, PointerAction::Drag(PointerButton::Left)));

    assert_eq!(editor.window.cursor().column().get(), 0);
    let selection = editor
        .editing
        .selection(&editor.buffer, &editor.window)
        .expect("dragging enters Visual mode");
    let Selection::Characterwise(range) = selection else {
        panic!("a pointer drag creates a characterwise selection");
    };
    assert_eq!((range.start().get(), range.end().get()), (0, 3));
}

#[test]
fn source_mapping_owns_wide_tab_and_combining_cells() {
    let editor = no_number_editor("a界z\n", Rect::new(0, 0, 8, 1));
    assert_eq!(editor.source_at_cell(CellPosition::new(1, 0)).column, 1);
    assert_eq!(editor.source_at_cell(CellPosition::new(2, 0)).column, 1);

    let editor = no_number_editor("\txe\u{301}z\n", Rect::new(0, 0, 10, 1));
    for cell in 0..4 {
        assert_eq!(editor.source_at_cell(CellPosition::new(cell, 0)).column, 0);
    }
    assert_eq!(editor.source_at_cell(CellPosition::new(4, 0)).column, 1);
    assert_eq!(editor.source_at_cell(CellPosition::new(5, 0)).column, 2);
    assert_eq!(editor.source_at_cell(CellPosition::new(6, 0)).column, 4);
}

#[test]
fn source_mapping_honors_horizontal_offset_and_clamps_line_end() {
    // The surface reserves its last column for the scrollbar, so it holds four
    // text cells over the four source columns of the line.
    let mut editor = no_number_editor("a界bc\n", Rect::new(0, 0, 5, 1));
    editor
        .editing
        .move_to(&editor.buffer, &mut editor.window, 0, 1);
    editor.window = editor.window.with_left_column(1);
    assert_eq!(editor.source_at_cell(CellPosition::new(0, 0)).column, 1);
    assert_eq!(editor.source_at_cell(CellPosition::new(1, 0)).column, 1);
    assert_eq!(editor.source_at_cell(CellPosition::new(2, 0)).column, 2);
    assert_eq!(editor.source_at_cell(CellPosition::new(3, 0)).column, 3);

    let editor = no_number_editor("x\n", Rect::new(0, 0, 5, 1));
    assert_eq!(editor.source_at_cell(CellPosition::new(4, 0)).column, 1);
}

#[test]
fn gutter_press_positions_without_starting_drag() {
    let mut editor = editor("alpha\nbeta\n", Rect::new(0, 0, 10, 2));
    editor.pointer(pointer(0, 1, PointerAction::Press(PointerButton::Left)));
    assert_eq!(editor.window.cursor().line().get(), 1);
    assert_eq!(editor.window.cursor().column().get(), 0);
    assert_eq!(editor.pointer_drag, PointerDragState::Idle);
    assert_eq!(
        editor.pointer(pointer(8, 1, PointerAction::Drag(PointerButton::Left))),
        PointerOutcome::Ignored
    );
    assert_eq!(editor.mode(), Mode::Normal);
}

#[test]
fn edge_drag_scrolls_once_and_maps_the_updated_edge() {
    let text = (0..20)
        .map(|line| format!("line-{line}\n"))
        .collect::<String>();
    let mut editor = no_number_editor(&text, Rect::new(0, 0, 10, 2));
    editor.pointer(pointer(0, 0, PointerAction::Press(PointerButton::Left)));
    let before = editor.window.first_line();
    editor.pointer(pointer(3, 2, PointerAction::Drag(PointerButton::Left)));
    assert_eq!(
        editor.window.first_line().saturating_sub(before),
        usize::from(editor.settings.mouse.scroll_rows)
    );
    assert_eq!(
        editor.window.cursor().line().get(),
        editor.window.first_line() + 1
    );
}

#[test]
fn drag_capture_cancels_and_release_keeps_visual_mode() {
    let start_drag = || {
        let mut editor = no_number_editor("alpha\nbeta\n", Rect::new(0, 0, 8, 2));
        editor.pointer(pointer(0, 0, PointerAction::Press(PointerButton::Left)));
        editor
    };

    let mut command = start_drag();
    command.command(Command::MoveRight, None, None).unwrap();
    assert_eq!(command.pointer_drag, PointerDragState::Idle);

    let mut literal = start_drag();
    assert_eq!(literal.literal("x"), LiteralOutcome::Refused);
    assert_eq!(literal.pointer_drag, PointerDragState::Idle);

    let mut resized = start_drag();
    resized.resize(Rect::new(0, 0, 9, 2)).unwrap();
    assert_eq!(resized.pointer_drag, PointerDragState::Idle);

    let mut wheel = start_drag();
    wheel.pointer(pointer(
        0,
        0,
        PointerAction::Wheel(PointerWheel::new(PointerWheelDirection::Down, 1).unwrap()),
    ));
    assert_eq!(wheel.pointer_drag, PointerDragState::Idle);

    for action in [
        PointerAction::Motion,
        PointerAction::Press(PointerButton::Right),
        PointerAction::Drag(PointerButton::Middle),
    ] {
        let mut unsupported = start_drag();
        unsupported.pointer(pointer(0, 0, action));
        assert_eq!(unsupported.pointer_drag, PointerDragState::Idle);
    }

    let mut outside = start_drag();
    outside.pointer(pointer(9, 0, PointerAction::Press(PointerButton::Left)));
    assert_eq!(outside.pointer_drag, PointerDragState::Idle);

    let mut released = start_drag();
    released.pointer(pointer(3, 1, PointerAction::Drag(PointerButton::Left)));
    released.pointer(pointer(3, 1, PointerAction::Release(PointerButton::Left)));
    assert_eq!(released.mode(), Mode::Visual);
    assert_eq!(released.pointer_drag, PointerDragState::Idle);
}
