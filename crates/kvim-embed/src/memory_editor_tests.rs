use super::*;
use std::time::Duration;

fn editor(text: &str, area: Rect) -> MemoryEditor {
    MemoryEditor::open(text, EditorSettings::default(), area).expect("the fixture is valid")
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
    assert_eq!(rendered.cursor().x, 4);
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
    assert_eq!(rendered.cursor(), Position::new(3, 0));
    assert_eq!(cells[(0, 0)].symbol(), "c");
}

#[test]
fn clipping_does_not_split_a_wide_character() {
    assert_eq!(clipped_cells("ab界c", 3), "ab");
    assert_eq!(clipped_cells("ab界c", 4), "ab界");
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
