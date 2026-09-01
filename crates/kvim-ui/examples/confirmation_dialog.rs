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

use kvim_ui::{Dialog, DialogChoice, DialogOutcome};

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
    assert_eq!(
        dialog.next(),
        DialogOutcome::Focused(ChoiceId::DiscardChanges)
    );
    let answer = dialog.answer_focused();
    assert_eq!(answer, DialogOutcome::Answered(ChoiceId::DiscardChanges));
    println!("answer: {answer:?}");
    Ok(())
}
