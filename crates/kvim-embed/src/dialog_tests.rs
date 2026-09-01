use super::*;

fn request(area: Rect) -> DialogRequest {
    let cancel = DialogChoiceId::new(1);
    DialogRequest::new(
        "Continue?",
        ["Review the operation."],
        [
            DialogChoice::new(cancel, "Cancel").with_direct_key('n'),
            DialogChoice::new(DialogChoiceId::new(2), "Continue").with_direct_key('y'),
        ],
        cancel,
        cancel,
        area,
        DialogStyles::default(),
    )
    .expect("the fixture is valid")
}

fn pointer(area: Rect, identity: DialogChoiceId) -> PointerEvent {
    let mut host = DialogHost::new();
    host.open(request(area)).unwrap();
    let mut cells = Buffer::empty(area);
    host.render(&mut cells).unwrap();
    let snapshot = host.snapshot().unwrap();
    let choice = snapshot
        .placement()
        .unwrap()
        .choices
        .iter()
        .find(|choice| choice.identity == identity)
        .unwrap();
    PointerEvent::new(
        kvim_keymap::CellPosition::new(choice.area.x, choice.area.y),
        kvim_keymap::PointerModifiers::default(),
        PointerAction::Press(PointerButton::Left),
    )
}

#[test]
fn lifecycle_invalidates_placement_and_answers_once() {
    let area = Rect::new(3, 2, 40, 10);
    let mut host = DialogHost::new();
    host.open(request(area)).unwrap();
    assert_eq!(host.open(request(area)), Err(DialogOpenError::AlreadyOpen));
    let opened = host.snapshot().unwrap();
    assert!(opened.placement().is_none());

    let mut cells = Buffer::empty(Rect::new(0, 0, 50, 15));
    host.render(&mut cells).unwrap();
    let rendered = host.snapshot().unwrap();
    assert!(rendered.placement().is_some());
    assert_eq!(rendered.placement().unwrap().body_area, area);
    assert_eq!(rendered.placement().unwrap().choices.len(), 2);

    let mut too_small = Buffer::empty(Rect::new(0, 0, 2, 2));
    assert!(host.render(&mut too_small).is_err());
    assert!(host.snapshot().unwrap().placement().is_none());
    host.render(&mut cells).unwrap();

    assert_eq!(
        host.input(DialogInput::Key(Key::plain(KeyCode::Down))),
        DialogInputOutcome::Redraw
    );
    assert!(host.snapshot().unwrap().placement().is_none());
    assert!(host.snapshot().unwrap().generation() > rendered.generation());
    host.render(&mut cells).unwrap();
    assert_eq!(
        host.input(DialogInput::Key(Key::plain(KeyCode::Enter))),
        DialogInputOutcome::Answered
    );
    assert!(!host.is_open());
    assert_eq!(host.take_answer().unwrap().choice, DialogChoiceId::new(2));
    assert!(host.take_answer().is_none());
}

#[test]
fn every_input_is_owned_and_cancel_returns_cancel_identity() {
    let area = Rect::new(0, 0, 40, 10);
    let mut host = DialogHost::new();
    host.open(request(area)).unwrap();
    assert_eq!(host.input(DialogInput::Paste), DialogInputOutcome::Consumed);
    assert_eq!(
        host.input(DialogInput::Unsupported),
        DialogInputOutcome::Consumed
    );
    assert_eq!(
        host.input(DialogInput::Key(Key::plain(KeyCode::Tab))),
        DialogInputOutcome::Consumed
    );
    assert_eq!(
        host.input(DialogInput::Key(Key::plain(KeyCode::Esc))),
        DialogInputOutcome::Answered
    );
    assert_eq!(host.take_answer().unwrap().choice, DialogChoiceId::new(1));
}

#[test]
fn pointer_requires_current_published_placement() {
    let area = Rect::new(3, 2, 40, 10);
    let yes = DialogChoiceId::new(2);
    let click = pointer(area, yes);
    let mut host = DialogHost::new();
    host.open(request(area)).unwrap();
    assert_eq!(
        host.input(DialogInput::Pointer(click)),
        DialogInputOutcome::Consumed
    );
    let mut cells = Buffer::empty(Rect::new(0, 0, 50, 15));
    host.render(&mut cells).unwrap();
    assert_eq!(
        host.input(DialogInput::Pointer(click)),
        DialogInputOutcome::Answered
    );
    assert_eq!(host.take_answer().unwrap().choice, yes);
}

#[test]
fn explicit_close_emits_no_answer_and_pending_answer_blocks_reopen() {
    let area = Rect::new(0, 0, 40, 10);
    let mut host = DialogHost::new();
    host.open(request(area)).unwrap();
    assert!(host.close());
    assert!(host.take_answer().is_none());
    assert!(!host.close());

    host.open(request(area)).unwrap();
    assert_eq!(
        host.input(DialogInput::Key(Key::plain(KeyCode::Char('y')))),
        DialogInputOutcome::Answered
    );
    assert_eq!(
        host.open(request(area)),
        Err(DialogOpenError::AnswerPending)
    );
    host.take_answer();
    host.open(request(area)).unwrap();
}

#[test]
fn request_uses_shared_validation_and_geometry_errors() {
    let cancel = DialogChoiceId::new(1);
    assert!(matches!(
        DialogRequest::new(
            "question",
            std::iter::empty::<&str>(),
            [DialogChoice::new(cancel, "Cancel")],
            cancel,
            cancel,
            Rect::new(0, 0, 1, 1),
            DialogStyles::default(),
        ),
        Err(DialogOpenError::Invalid(UiError::BodyTooSmall { .. }))
    ));
}
