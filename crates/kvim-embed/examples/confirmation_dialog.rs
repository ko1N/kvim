use kvim_embed::{
    DialogChoice, DialogChoiceId, DialogInput, DialogInputOutcome, DialogRequest, DialogStyles,
    MemoryEditor,
};
use kvim_keymap::{Key, KeyCode};
use kvim_settings::EditorSettings;
use ratatui::{buffer::Buffer, layout::Rect};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let area = Rect::new(0, 0, 48, 12);
    let mut editor = MemoryEditor::open("caller-owned text\n", EditorSettings::default(), area)?;
    let keep = DialogChoiceId::new(1);
    let discard = DialogChoiceId::new(2);
    editor.open_dialog(DialogRequest::new(
        "Discard the unsaved changes?",
        ["This operation cannot be undone."],
        [
            DialogChoice::new(keep, "Keep editing").with_direct_key('n'),
            DialogChoice::new(discard, "Discard").with_direct_key('y'),
        ],
        keep,
        keep,
        area,
        DialogStyles::default(),
    )?)?;

    let mut cells = Buffer::empty(area);
    editor.render(&mut cells)?;
    let snapshot = editor.dialog_snapshot().expect("the dialog is open");
    assert!(snapshot.placement().is_some());

    assert_eq!(
        editor.dialog_input(DialogInput::Key(Key::plain(KeyCode::Down))),
        DialogInputOutcome::Redraw
    );
    editor.render(&mut cells)?;
    assert_eq!(
        editor.dialog_input(DialogInput::Key(Key::plain(KeyCode::Enter))),
        DialogInputOutcome::Answered
    );
    let Some(kvim_embed::MemoryEditorEvent::DialogAnswered(answer)) = editor.take_event() else {
        panic!("one answer is queued");
    };
    assert_eq!(answer.choice, discard);
    assert!(editor.take_event().is_none());
    Ok(())
}
