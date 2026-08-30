use kvim_embed::{
    CellPosition, MemoryEditor, PointerAction, PointerButton, PointerEvent, PointerModifiers,
};
use kvim_input::Command;
use kvim_settings::EditorSettings;
use ratatui::{buffer::Buffer, layout::Rect};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let area = Rect::new(0, 0, 40, 6);
    let mut editor = MemoryEditor::open("outside text\n", EditorSettings::default(), area)?;
    editor.pointer(PointerEvent::new(
        CellPosition::new(6, 0),
        PointerModifiers::default(),
        PointerAction::Press(PointerButton::Left),
    ));
    editor.command(Command::InsertAtLineEnd, None, None)?;
    editor.literal(" edited");
    editor.command(Command::ReturnToNormal, None, None)?;

    let frame = editor.render(&mut Buffer::empty(area))?;
    assert!(area.contains(frame.cursor()));
    assert_eq!(editor.text(), "outside text edited\n");
    drop(editor);
    Ok(())
}
