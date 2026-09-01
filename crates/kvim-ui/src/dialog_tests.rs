use ratatui::layout::Rect;

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Id {
    Keep,
    Discard,
    Other,
}

fn choices() -> [DialogChoice<Id>; 2] {
    [
        DialogChoice::new(Id::Keep, "Keep editing"),
        DialogChoice::new(Id::Discard, "Discard changes").with_direct_key('d'),
    ]
}

fn dialog() -> Dialog<Id> {
    Dialog::new(
        "Discard unsaved changes?",
        std::iter::empty::<&str>(),
        choices(),
        Id::Keep,
        Id::Keep,
    )
    .expect("the fixed dialog stays inside every bound")
}

#[test]
fn starts_focused_on_the_safe_default() {
    let dialog = dialog();
    assert_eq!(dialog.focused_identity(), &Id::Keep);
    assert_eq!(dialog.default_identity(), &Id::Keep);
    assert_eq!(dialog.cancel_identity(), &Id::Keep);
}

#[test]
fn previous_and_next_wrap_between_choices() {
    let mut dialog = dialog();
    assert_eq!(dialog.previous(), DialogOutcome::Focused(Id::Discard));
    assert_eq!(dialog.next(), DialogOutcome::Focused(Id::Keep));
    assert_eq!(dialog.next(), DialogOutcome::Focused(Id::Discard));
}

#[test]
fn focus_and_answers_return_caller_identity() {
    let mut dialog = dialog();
    assert_eq!(
        dialog.focus(&Id::Discard),
        Ok(DialogOutcome::Focused(Id::Discard))
    );
    assert_eq!(
        dialog.answer_focused(),
        DialogOutcome::Answered(Id::Discard)
    );
    assert_eq!(dialog.answer_default(), DialogOutcome::Answered(Id::Keep));
    assert_eq!(dialog.answer_cancel(), DialogOutcome::Answered(Id::Keep));
    assert_eq!(
        dialog.answer_for_direct_key('d'),
        Some(DialogOutcome::Answered(Id::Discard))
    );
    assert_eq!(dialog.direct_key_identity('D'), Some(&Id::Discard));
}

#[test]
fn rejects_every_content_value_above_its_bound() {
    let question = "q".repeat(DIALOG_QUESTION_CHARS_MAX + 1);
    assert_eq!(
        Dialog::new(
            question,
            std::iter::empty::<&str>(),
            choices(),
            Id::Keep,
            Id::Keep,
        ),
        Err(DialogError::QuestionChars {
            chars: DIALOG_QUESTION_CHARS_MAX + 1,
            max: DIALOG_QUESTION_CHARS_MAX
        })
    );
    let body = vec!["body"; DIALOG_BODY_LINES_MAX + 1];
    assert!(matches!(
        Dialog::new("q", body, choices(), Id::Keep, Id::Keep),
        Err(DialogError::BodyLines { .. })
    ));
    let body = ["b".repeat(DIALOG_BODY_LINE_CHARS_MAX + 1)];
    assert!(matches!(
        Dialog::new("q", body, choices(), Id::Keep, Id::Keep),
        Err(DialogError::BodyLineChars { .. })
    ));
    let many: Vec<_> = (0..DIALOG_CHOICES_MAX + 1)
        .map(|index| DialogChoice::new(index, "choice"))
        .collect();
    assert!(matches!(
        Dialog::new("q", std::iter::empty::<&str>(), many, 0, 0),
        Err(DialogError::Choices { .. })
    ));
    let label = "c".repeat(DIALOG_CHOICE_LABEL_CHARS_MAX + 1);
    assert!(matches!(
        Dialog::new(
            "q",
            std::iter::empty::<&str>(),
            [DialogChoice::new(Id::Keep, label)],
            Id::Keep,
            Id::Keep
        ),
        Err(DialogError::ChoiceLabelChars { .. })
    ));
}

#[test]
fn accepts_every_content_value_at_its_bound() {
    let question = "q".repeat(DIALOG_QUESTION_CHARS_MAX);
    let body: Vec<_> = (0..DIALOG_BODY_LINES_MAX)
        .map(|_| "b".repeat(DIALOG_BODY_LINE_CHARS_MAX))
        .collect();
    let choices: Vec<_> = (0..DIALOG_CHOICES_MAX)
        .map(|index| {
            DialogChoice::new(index, "c".repeat(DIALOG_CHOICE_LABEL_CHARS_MAX))
                .with_direct_key(char::from(b'!' + index as u8))
        })
        .collect();
    assert!(Dialog::new(question, body, choices, 0, 0).is_ok());
}

#[test]
fn rejects_invalid_duplicate_and_excess_direct_keys() {
    let invalid = [DialogChoice::new(Id::Keep, "Keep").with_direct_key('\n')];
    assert_eq!(
        Dialog::new("q", std::iter::empty::<&str>(), invalid, Id::Keep, Id::Keep),
        Err(DialogError::InvalidDirectKey { key: '\n' })
    );
    let duplicate = [
        DialogChoice::new(Id::Keep, "Keep").with_direct_key('k'),
        DialogChoice::new(Id::Discard, "Discard").with_direct_key('K'),
    ];
    assert_eq!(
        Dialog::new(
            "q",
            std::iter::empty::<&str>(),
            duplicate,
            Id::Keep,
            Id::Keep,
        ),
        Err(DialogError::DuplicateDirectKey { key: 'K' })
    );
}

#[test]
fn rejects_empty_and_invalid_choice_roles() {
    assert_eq!(
        Dialog::new(
            "q",
            std::iter::empty::<&str>(),
            Vec::<DialogChoice<Id>>::new(),
            Id::Keep,
            Id::Keep,
        ),
        Err(DialogError::EmptyChoices)
    );
    assert!(
        Dialog::new(
            "q",
            std::iter::empty::<&str>(),
            choices(),
            Id::Discard,
            Id::Keep
        )
        .is_ok()
    );
    assert!(
        Dialog::new(
            "q",
            std::iter::empty::<&str>(),
            choices(),
            Id::Keep,
            Id::Discard
        )
        .is_ok()
    );
    let only_keep = [DialogChoice::new(Id::Keep, "Keep")];
    assert_eq!(
        Dialog::new(
            "q",
            std::iter::empty::<&str>(),
            only_keep,
            Id::Discard,
            Id::Keep
        ),
        Err(DialogError::UnknownDefault)
    );
    let only_keep = [DialogChoice::new(Id::Keep, "Keep")];
    assert_eq!(
        Dialog::new(
            "q",
            std::iter::empty::<&str>(),
            only_keep,
            Id::Keep,
            Id::Discard
        ),
        Err(DialogError::UnknownCancel)
    );
    let duplicated = [
        DialogChoice::new(Id::Keep, "Keep"),
        DialogChoice::new(Id::Keep, "Also keep"),
    ];
    assert_eq!(
        Dialog::new(
            "q",
            std::iter::empty::<&str>(),
            duplicated,
            Id::Keep,
            Id::Keep,
        ),
        Err(DialogError::AmbiguousDefault)
    );
    let duplicated = [
        DialogChoice::new(Id::Keep, "Keep"),
        DialogChoice::new(Id::Keep, "Also keep"),
        DialogChoice::new(Id::Discard, "Discard"),
    ];
    assert_eq!(
        Dialog::new(
            "q",
            std::iter::empty::<&str>(),
            duplicated,
            Id::Discard,
            Id::Keep,
        ),
        Err(DialogError::AmbiguousCancel)
    );
}

#[test]
fn rejects_unknown_and_ambiguous_focus_identities() {
    let mut dialog = dialog();
    assert_eq!(
        dialog.focus(&Id::Discard),
        Ok(DialogOutcome::Focused(Id::Discard))
    );
    assert_eq!(dialog.focus(&Id::Other), Err(DialogError::UnknownChoice));
    let duplicated = [
        DialogChoice::new(Id::Keep, "Keep"),
        DialogChoice::new(Id::Keep, "Also keep"),
    ];
    let mut dialog = Dialog {
        question: "q".into(),
        body: vec![],
        choices: duplicated.into(),
        default: 0,
        cancel: 0,
        focused: 0,
    };
    assert_eq!(dialog.focus(&Id::Keep), Err(DialogError::AmbiguousChoice));
}

#[test]
fn validates_popup_rectangle_bounds() {
    assert_eq!(
        Dialog::<Id>::validate_popup_area(Rect::new(0, 0, 0, 1)),
        Err(DialogError::EmptyPopup {
            area: Rect::new(0, 0, 0, 1)
        })
    );
    assert!(
        Dialog::<Id>::validate_popup_area(Rect::new(
            0,
            0,
            DIALOG_POPUP_COLUMNS_MAX,
            DIALOG_POPUP_ROWS_MAX
        ))
        .is_ok()
    );
    assert!(matches!(
        Dialog::<Id>::validate_popup_area(Rect::new(0, 0, DIALOG_POPUP_COLUMNS_MAX + 1, 1)),
        Err(DialogError::PopupColumns { .. })
    ));
    assert!(matches!(
        Dialog::<Id>::validate_popup_area(Rect::new(0, 0, 1, DIALOG_POPUP_ROWS_MAX + 1)),
        Err(DialogError::PopupRows { .. })
    ));
}
