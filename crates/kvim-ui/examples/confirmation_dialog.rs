//! Open, navigate, and answer a caller-owned dialog.
//!
//! The example uses stable caller identities. `kvim-ui` validates the content
//! and returns those identities. The host maps them to actions after an answer.
//!
//! Run it with:
//!
//! ```sh
//! cargo run -p kvim-ui --example confirmation_dialog
//! ```

use kvim_ui::{Dialog, DialogChoice, DialogOutcome, DialogStyles};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChoiceId {
    KeepEditing,
    DiscardChanges,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let choices = [
        DialogChoice::new(ChoiceId::KeepEditing, "Keep editing"),
        DialogChoice::new(ChoiceId::DiscardChanges, "Discard changes").with_direct_key('d'),
    ];
    let mut dialog = Dialog::new(
        "Discard unsaved changes?",
        ["This action cannot be undone."],
        choices,
        ChoiceId::KeepEditing,
        ChoiceId::KeepEditing,
    )?;

    println!("focused: {:?}", dialog.focused_identity());
    let body = Rect::new(0, 0, 48, 12);
    let mut target = Buffer::empty(body);
    let placement = dialog.render(
        &mut target,
        body,
        DialogStyles {
            dim: Style::default().bg(Color::Black),
            surface: Style::default().bg(Color::DarkGray),
            rail: Style::default().fg(Color::Cyan),
            body: Style::default(),
            question: Style::default(),
            choice: Style::default(),
            default_choice: Style::default().fg(Color::Green),
            focused_choice: Style::default().fg(Color::Yellow),
        },
    )?;
    println!(
        "body: {:?}, popup: {:?}, choices: {:?}",
        placement.body_area, placement.popup, placement.choices
    );
    assert_eq!(
        dialog.next(),
        DialogOutcome::Focused(ChoiceId::DiscardChanges)
    );
    let answer = dialog.answer_focused();
    assert_eq!(answer, DialogOutcome::Answered(ChoiceId::DiscardChanges));
    println!("answer: {answer:?}");
    Ok(())
}
