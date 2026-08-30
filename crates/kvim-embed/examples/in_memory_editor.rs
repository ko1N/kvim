use kvim_embed::{
    CellPosition, MemoryEditor, PointerAction, PointerButton, PointerEvent, PointerModifiers,
};
use kvim_input::Command;
use kvim_settings::EditorSettings;
use ratatui::{buffer::Buffer, layout::Rect};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let area = Rect::new(0, 0, 40, 6);
    let settings = EditorSettings::default();
    let mut editor = MemoryEditor::open("A host supplied this text.\n", settings, area)?;

    editor.pointer(PointerEvent::new(
        CellPosition::new(8, 0),
        PointerModifiers::default(),
        PointerAction::Press(PointerButton::Left),
    ));

    editor.command(Command::InsertAtLineEnd, None, None)?;
    editor.literal(" Edited in memory.");
    editor.command(Command::ReturnToNormal, None, None)?;

    let mut cells = Buffer::empty(area);
    let frame = editor.render(&mut cells)?;
    assert!(area.contains(frame.cursor()));
    assert_eq!(
        editor.text(),
        "A host supplied this text. Edited in memory.\n"
    );

    drop(editor);
    Ok(())
}
