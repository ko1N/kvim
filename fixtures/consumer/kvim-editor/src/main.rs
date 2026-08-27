use std::num::NonZeroU16;

use kvim_core::{BufferBytesMax, TextBuffer};
use kvim_editor::{CommandOutcome, EditContext, EditingState, Registers, Viewport, WindowState};
use kvim_input::Command;
use kvim_settings::EditorSettings;

fn main() {
    let mut buffer = TextBuffer::from_text("alpha beta\n", BufferBytesMax::default())
        .expect("the supplied text is bounded");
    let settings = EditorSettings::default();
    let mut registers = Registers::default();
    let mut context = EditContext {
        buffer: &mut buffer,
        settings: &settings,
        search: None,
        language_indent_width: None,
        registers: &mut registers,
    };
    let rows = NonZeroU16::new(4).expect("four is nonzero");
    let cells = NonZeroU16::new(40).expect("forty is nonzero");
    let mut window = WindowState::new(Viewport::new(rows, cells));
    let mut editing = EditingState::new();

    let pending = editing.apply(&mut context, &mut window, Command::DeleteOverMotion, None);
    assert_eq!(pending.outcome(), CommandOutcome::OperatorPending);
    let changed = editing.apply(&mut context, &mut window, Command::MoveNextWordStart, None);
    assert_eq!(changed.outcome(), CommandOutcome::Changed);
    assert_eq!(context.buffer.to_string(), "beta\n");

    editing.apply(&mut context, &mut window, Command::Undo, None);
    assert_eq!(context.buffer.to_string(), "alpha beta\n");
}
