use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};

use super::*;
use crate::Cell;

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

fn styles() -> DialogStyles {
    DialogStyles {
        dim: Style::default().bg(Color::Blue),
        surface: Style::default().bg(Color::Black),
        rail: Style::default().fg(Color::Cyan),
        icon: Style::default().fg(Color::Red),
        body: Style::default().fg(Color::Green),
        question: Style::default().fg(Color::White),
        footer: Style::default().bg(Color::DarkGray),
        choice: Style::default().fg(Color::Gray),
        default_choice: Style::default().fg(Color::Magenta),
        focused_choice: Style::default().fg(Color::Yellow),
    }
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
    assert_eq!(dialog.move_previous(), DialogOutcome::Focused(Id::Discard));
    assert_eq!(dialog.move_next(), DialogOutcome::Focused(Id::Keep));
    assert_eq!(dialog.move_next(), DialogOutcome::Focused(Id::Discard));
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
        icon: None,
    };
    assert_eq!(dialog.focus(&Id::Keep), Err(DialogError::AmbiguousChoice));
}

#[test]
fn placement_and_render_share_exact_popup_geometry() {
    let dialog = dialog();
    let body = Rect::new(4, 3, 40, 10);
    let pure = dialog
        .placement_for(body)
        .expect("the body holds the fixed dialog");
    let mut target = Buffer::empty(body);
    let rendered = dialog
        .render(&mut target, body, styles())
        .expect("the body is inside its buffer");
    assert_eq!(rendered, pure);
    assert_eq!(pure.body_area, body);
    assert_eq!(pure.popup, Rect::new(4, 5, 40, 6));
    assert_eq!(pure.rail, Rect::new(4, 5, 1, 6));
    assert_eq!(pure.content, Rect::new(6, 6, 38, 1));
    assert_eq!(pure.footer, Rect::new(5, 8, 39, 3));
    assert_eq!(pure.choices[0].area, Rect::new(6, 9, 14, 1));
    assert_eq!(pure.choices[1].area, Rect::new(22, 9, 17, 1));
}

#[test]
fn rendering_dims_only_the_supplied_body_and_keeps_the_rail_styled() {
    let dialog = dialog();
    let buffer = Rect::new(0, 0, 48, 16);
    let body = Rect::new(4, 3, 40, 10);
    let mut target = Buffer::empty(buffer);
    target.set_style(buffer, Style::default().bg(Color::Red));
    let placement = dialog
        .render(&mut target, body, styles())
        .expect("the body is inside its buffer");
    assert_eq!(
        target.cell((0, 0)).expect("buffer cell").style().bg,
        Some(Color::Red)
    );
    assert_eq!(
        target.cell((4, 3)).expect("body cell").style().bg,
        Some(Color::Blue)
    );
    for y in placement.popup.y..placement.popup.bottom() {
        let cell = target.cell((placement.rail.x, y)).expect("rail cell");
        assert_eq!(cell.symbol(), "▌");
        assert_eq!(cell.style().fg, Some(Color::Cyan));
        assert_eq!(cell.style().bg, Some(Color::Black));
    }
    let focused = target
        .cell((placement.choices[0].area.x, placement.choices[0].area.y))
        .expect("focused choice");
    assert_eq!(focused.style().fg, Some(Color::Yellow));
    assert_eq!(focused.style().bg, Some(Color::DarkGray));
    let rail_next_to_focused = target
        .cell((placement.rail.x, placement.choices[0].area.y))
        .expect("focused choice rail");
    assert_eq!(rail_next_to_focused.symbol(), "▌");
    assert_eq!(rail_next_to_focused.style().fg, Some(Color::Cyan));
    assert_eq!(rail_next_to_focused.style().bg, Some(Color::Black));
    assert_eq!(
        target
            .cell((placement.popup.x + 1, placement.popup.y))
            .expect("gap")
            .symbol(),
        " "
    );
    let footer_background = target
        .cell((placement.footer.x, placement.footer.y))
        .expect("footer background");
    assert_eq!(footer_background.style().bg, Some(Color::DarkGray));
}

#[test]
fn choice_placements_are_horizontal_chips_that_cover_their_own_padding() {
    let mut dialog = Dialog::new(
        "Question",
        ["detail"],
        [
            DialogChoice::new(Id::Keep, "Keep"),
            DialogChoice::new(Id::Discard, "Discard"),
        ],
        Id::Keep,
        Id::Keep,
    )
    .expect("bounded dialog");
    dialog.move_next();
    let body = Rect::new(7, 5, 32, 10);
    let mut target = Buffer::empty(Rect::new(0, 0, 48, 20));
    target.set_style(*target.area(), Style::default().bg(Color::Red));
    let placement = dialog
        .render(&mut target, body, styles())
        .expect("valid render");
    assert_eq!(placement.choices[0].area, Rect::new(9, 12, 6, 1));
    assert_eq!(placement.choices[1].area, Rect::new(17, 12, 9, 1));
    assert_eq!(placement.choices[0].area.y, placement.choices[1].area.y);
    for choice in &placement.choices {
        let expected = match choice.identity {
            Id::Keep => " Keep ",
            Id::Discard => " Discard ",
            Id::Other => unreachable!("the test dialog has two choices"),
        };
        // The published chip rectangle covers the leading and trailing
        // padding cell, so a pointer press anywhere on the chip, including
        // its padding, lands inside the published area.
        assert_eq!(
            choice.area.width,
            u16::try_from(expected.len()).expect("short label")
        );
        let painted: String = (choice.area.x..choice.area.right())
            .map(|x| {
                target
                    .cell((x, choice.area.y))
                    .expect("choice cell")
                    .symbol()
            })
            .collect();
        assert_eq!(painted, expected);
        assert_eq!(
            target
                .cell((placement.rail.x, choice.area.y))
                .expect("rail")
                .symbol(),
            "▌"
        );
    }
    // The two-cell gap between chips holds no painted label content.
    assert_eq!(
        target
            .cell((
                placement.choices[0].area.right(),
                placement.choices[0].area.y
            ))
            .expect("gap")
            .symbol(),
        " "
    );
    assert_eq!(
        target
            .cell((placement.choices[1].area.x - 1, placement.choices[1].area.y))
            .expect("gap")
            .symbol(),
        " "
    );
}

#[test]
fn maximum_content_returns_a_typed_fit_refusal_under_popup_bounds() {
    let question = "q".repeat(DIALOG_QUESTION_CHARS_MAX);
    let body: Vec<_> = (0..DIALOG_BODY_LINES_MAX)
        .map(|_| "b".repeat(DIALOG_BODY_LINE_CHARS_MAX))
        .collect();
    let choices: Vec<_> = (0..DIALOG_CHOICES_MAX)
        .map(|index| DialogChoice::new(index, "c".repeat(DIALOG_CHOICE_LABEL_CHARS_MAX)))
        .collect();
    let dialog = Dialog::new(question, body, choices, 0, 0).expect("maximum constructor content");
    let body = Rect::new(10, 10, DIALOG_POPUP_COLUMNS_MAX, DIALOG_POPUP_ROWS_MAX);
    assert_eq!(
        dialog.placement_for(body),
        Err(DialogError::BodyTooSmall { body })
    );
}

#[test]
fn popup_stays_inside_the_supplied_body_and_has_top_padding_and_a_bottom_footer() {
    let dialog = dialog();
    let body = Rect::new(4, 3, 40, 10);
    let placement = dialog.placement_for(body).expect("body fits");
    assert_eq!(placement.body_area, body);
    assert!(placement.popup.x >= body.x);
    assert!(placement.popup.y >= body.y);
    assert!(placement.popup.right() <= body.right());
    assert!(placement.popup.bottom() <= body.bottom());
    assert_eq!(placement.rail.y, placement.popup.y);
    assert_eq!(placement.rail.bottom(), placement.popup.bottom());
    // One blank row opens the popup before the content region.
    assert_eq!(placement.content.y, placement.popup.y + 1);
    // One blank row closes the content region before the footer band.
    assert_eq!(placement.content.bottom() + 1, placement.footer.y);
    // The footer band reaches the popup's bottom edge; no row follows it.
    assert_eq!(placement.footer.bottom(), placement.popup.bottom());
}

#[test]
fn wraps_question_and_places_optional_body_after_it() {
    let dialog = Dialog::new(
        "a question that must wrap across several columns",
        ["detail one", "detail two"],
        [DialogChoice::new(Id::Keep, "Keep")],
        Id::Keep,
        Id::Keep,
    )
    .expect("bounded dialog");
    let placement = dialog
        .placement_for(Rect::new(5, 7, 16, 12))
        .expect("body fits");
    assert_eq!(placement.body_text.height, 2);
    assert!(placement.question.height > 1);
    // The body follows the question, separated by one blank row.
    assert_eq!(placement.body_text.y, placement.question.bottom() + 1);
    assert_eq!(placement.body_text.bottom(), placement.content.bottom());
    // The footer band, and inside it the choice row, follow the content
    // region after its closing blank row.
    assert_eq!(placement.footer.y, placement.content.bottom() + 1);
    assert_eq!(placement.choices[0].area.y, placement.footer.y + 1);
}

#[test]
fn narrow_bodies_require_a_complete_choice_row() {
    // The narrowest single choice needs one leading and one trailing
    // padding cell plus its label, and the fixed six-row popup (top pad,
    // one question row, the closing pad row, and the three-row footer).
    let narrow_dialog = Dialog::new(
        "q",
        std::iter::empty::<&str>(),
        [DialogChoice::new(Id::Keep, "x")],
        Id::Keep,
        Id::Keep,
    )
    .expect("short dialog");
    let narrow = Rect::new(9, 4, 5, 6);
    let placement = narrow_dialog
        .placement_for(narrow)
        .expect("the narrowest complete choice row fits");
    assert_eq!(placement.content.width, 3);
    assert_eq!(placement.choices[0].area.width, 3);
    assert_eq!(
        narrow_dialog.placement_for(Rect::new(9, 4, 4, 6)),
        Err(DialogError::BodyTooSmall {
            body: Rect::new(9, 4, 4, 6)
        })
    );
    assert_eq!(
        narrow_dialog.placement_for(Rect::new(9, 4, 5, 5)),
        Err(DialogError::BodyTooSmall {
            body: Rect::new(9, 4, 5, 5)
        })
    );
    let wide = Dialog::new(
        "q",
        std::iter::empty::<&str>(),
        [DialogChoice::new(Id::Keep, "界")],
        Id::Keep,
        Id::Keep,
    )
    .expect("wide label is bounded");
    assert_eq!(
        wide.placement_for(Rect::new(9, 4, 5, 6)),
        Err(DialogError::BodyTooSmall {
            body: Rect::new(9, 4, 5, 6)
        })
    );
    let body_line = Dialog::new(
        "q",
        ["wide body"],
        [DialogChoice::new(Id::Keep, "x")],
        Id::Keep,
        Id::Keep,
    )
    .expect("bounded dialog");
    assert_eq!(
        body_line.placement_for(Rect::new(0, 0, 10, 8)),
        Err(DialogError::BodyTooSmall {
            body: Rect::new(0, 0, 10, 8)
        })
    );
}

#[test]
fn footer_band_is_published_and_never_overlaps_the_content_region() {
    let dialog = dialog();
    let placement = dialog
        .placement_for(Rect::new(4, 3, 40, 10))
        .expect("body fits");
    // The footer starts strictly below the content region's last row, so the
    // two rectangles' row ranges never intersect.
    assert!(placement.footer.y > placement.content.bottom());
    assert_eq!(placement.footer.bottom(), placement.popup.bottom());
    assert_eq!(placement.footer, Rect::new(5, 8, 39, 3));
}

#[test]
fn question_wrap_indents_continuation_lines_under_the_icon() {
    let dialog = Dialog::new(
        "a question that must wrap across several columns",
        std::iter::empty::<&str>(),
        [DialogChoice::new(Id::Keep, "Keep")],
        Id::Keep,
        Id::Keep,
    )
    .expect("bounded dialog")
    .with_icon('⚠')
    .expect("a warning glyph occupies one cell");
    let body = Rect::new(2, 1, 16, 9);
    let mut target = Buffer::empty(body);
    let placement = dialog
        .render(&mut target, body, styles())
        .expect("the icon and question both fit");
    assert!(
        placement.question.height > 1,
        "the question wraps across several lines"
    );
    let icon_cell = target
        .cell((placement.content.x, placement.question.y))
        .expect("icon cell");
    assert_eq!(icon_cell.symbol(), "⚠");
    assert_eq!(icon_cell.style().fg, Some(Color::Red));
    // The title row's question text starts two columns past the icon glyph:
    // one cell for the glyph and one blank separator cell.
    let title_text_x = placement.content.x + 2;
    assert_eq!(
        target
            .cell((title_text_x, placement.question.y))
            .expect("title text cell")
            .symbol(),
        "a"
    );
    // A continuation line paints no glyph: its icon column stays the blank
    // surface, and its text still starts at the same indented column.
    let continuation_y = placement.question.y + 1;
    let blank_under_icon = target
        .cell((placement.content.x, continuation_y))
        .expect("blank under icon");
    assert_eq!(blank_under_icon.symbol(), " ");
    assert_eq!(blank_under_icon.style().bg, Some(Color::Black));
    assert_ne!(blank_under_icon.style().fg, Some(Color::Red));
    assert_eq!(
        target
            .cell((title_text_x, continuation_y))
            .expect("continuation text cell")
            .symbol(),
        "h"
    );
}

#[test]
fn the_constructor_rejects_an_icon_with_no_printable_width() {
    // A refused layout paints no popup while the dialog still holds the keys,
    // so an unpaintable glyph must fail before the dialog exists.
    let refused = Dialog::new(
        "q",
        std::iter::empty::<&str>(),
        [DialogChoice::new(Id::Keep, "x")],
        Id::Keep,
        Id::Keep,
    )
    .expect("short dialog")
    .with_icon('\u{0301}');
    assert_eq!(refused, Err(DialogError::InvalidIcon { icon: '\u{0301}' }));
}

#[test]
fn invalid_bodies_return_typed_errors_without_rendering() {
    let dialog = dialog();
    let too_small = Rect::new(0, 0, 2, 3);
    assert_eq!(
        dialog.placement_for(too_small),
        Err(DialogError::BodyTooSmall { body: too_small })
    );
    let mut small_target = Buffer::empty(Rect::new(0, 0, 10, 4));
    let small_untouched = small_target.clone();
    assert_eq!(
        dialog.render(&mut small_target, too_small, styles()),
        Err(DialogError::BodyTooSmall { body: too_small })
    );
    assert_eq!(small_target, small_untouched);
    let mut target = Buffer::empty(Rect::new(0, 0, 10, 4));
    let buffer = *target.area();
    let untouched = target.clone();
    let outside = Rect::new(8, 0, 3, 4);
    assert_eq!(
        dialog.render(&mut target, outside, styles()),
        Err(DialogError::TargetArea {
            body: outside,
            buffer
        })
    );
    assert_eq!(target, untouched);
    let impossible = Rect {
        x: u16::MAX,
        y: 0,
        width: 1,
        height: 1,
    };
    assert_eq!(
        dialog.placement_for(impossible),
        Err(DialogError::InvalidBodyArea { body: impossible })
    );
}

fn placement(dialog: &Dialog<Id>) -> DialogPlacement<Id> {
    dialog
        .placement_for(Rect::new(11, 7, 40, 10))
        .expect("the fixed dialog has a published placement")
}

#[test]
fn keyboard_enter_answers_initial_safe_default() {
    let mut dialog = dialog();
    assert_eq!(
        dialog.drive_key(DialogKey::Enter),
        DialogKeyOutcome::Interaction(DialogOutcome::Answered(Id::Keep))
    );
}

#[test]
fn keyboard_driving_answers_navigates_and_consumes_all_keys() {
    let mut dialog = dialog();
    assert_eq!(
        dialog.drive_key(DialogKey::Char('h')),
        DialogKeyOutcome::Interaction(DialogOutcome::Focused(Id::Discard))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Char('k')),
        DialogKeyOutcome::Interaction(DialogOutcome::Focused(Id::Keep))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Left),
        DialogKeyOutcome::Interaction(DialogOutcome::Focused(Id::Discard))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Up),
        DialogKeyOutcome::Interaction(DialogOutcome::Focused(Id::Keep))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Char('j')),
        DialogKeyOutcome::Interaction(DialogOutcome::Focused(Id::Discard))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Char('l')),
        DialogKeyOutcome::Interaction(DialogOutcome::Focused(Id::Keep))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Right),
        DialogKeyOutcome::Interaction(DialogOutcome::Focused(Id::Discard))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Down),
        DialogKeyOutcome::Interaction(DialogOutcome::Focused(Id::Keep))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Enter),
        DialogKeyOutcome::Interaction(DialogOutcome::Answered(Id::Keep))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Esc),
        DialogKeyOutcome::Interaction(DialogOutcome::Answered(Id::Keep))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::CtrlC),
        DialogKeyOutcome::Interaction(DialogOutcome::Answered(Id::Keep))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Char('z')),
        DialogKeyOutcome::Consumed
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Unsupported),
        DialogKeyOutcome::Consumed
    );
}

#[test]
fn direct_keys_precede_movement_aliases() {
    let mut dialog = Dialog::new(
        "Question",
        std::iter::empty::<&str>(),
        [
            DialogChoice::new(Id::Keep, "Keep").with_direct_key('j'),
            DialogChoice::new(Id::Discard, "Discard").with_direct_key('n'),
        ],
        Id::Keep,
        Id::Keep,
    )
    .expect("valid direct keys");
    assert_eq!(
        dialog.drive_key(DialogKey::Char('j')),
        DialogKeyOutcome::Interaction(DialogOutcome::Answered(Id::Keep))
    );
    assert_eq!(
        dialog.drive_key(DialogKey::Char('n')),
        DialogKeyOutcome::Interaction(DialogOutcome::Answered(Id::Discard))
    );
}

#[test]
fn pointer_driving_uses_only_published_placement_and_consumes_background() {
    let mut dialog = dialog();
    let placement = placement(&dialog);
    let keep = placement.choices[0].area;
    let discard = placement.choices[1].area;
    assert_eq!(
        dialog.drive_pointer(
            DialogPointerEvent {
                cell: Cell::new(discard.x, discard.y),
                action: DialogPointerAction::Motion,
            },
            &placement,
        ),
        DialogPointerOutcome::Interaction(DialogOutcome::Focused(Id::Discard))
    );
    assert_eq!(
        dialog.drive_pointer(
            DialogPointerEvent {
                cell: Cell::new(keep.x, keep.y),
                action: DialogPointerAction::Press(DialogPointerButton::Primary),
            },
            &placement,
        ),
        DialogPointerOutcome::Interaction(DialogOutcome::Answered(Id::Keep))
    );
    for cell in [
        Cell::new(placement.rail.x, keep.y),
        Cell::new(placement.popup.x + 1, placement.popup.y),
        Cell::new(placement.content.x, placement.popup.y),
    ] {
        assert_eq!(
            dialog.drive_pointer(
                DialogPointerEvent {
                    cell,
                    action: DialogPointerAction::Motion,
                },
                &placement,
            ),
            DialogPointerOutcome::Consumed
        );
    }
    assert_eq!(
        dialog.drive_pointer(
            DialogPointerEvent {
                cell: Cell::new(placement.popup.right(), placement.popup.y),
                action: DialogPointerAction::Motion,
            },
            &placement,
        ),
        DialogPointerOutcome::OutsidePopup
    );
    for action in [
        DialogPointerAction::Press(DialogPointerButton::Secondary),
        DialogPointerAction::Release(DialogPointerButton::Primary),
        DialogPointerAction::Drag(DialogPointerButton::Primary),
        DialogPointerAction::Wheel,
    ] {
        assert_eq!(
            dialog.drive_pointer(
                DialogPointerEvent {
                    cell: Cell::new(keep.x, keep.y),
                    action,
                },
                &placement,
            ),
            DialogPointerOutcome::Consumed
        );
    }
}

#[test]
fn pointer_driving_rejects_modified_geometry() {
    let dialog = dialog();
    let original = placement(&dialog);

    let mut modified_popup = original.clone();
    modified_popup.popup.x += 1;
    assert_eq!(
        dialog.clone().drive_pointer(
            DialogPointerEvent {
                cell: Cell::new(original.popup.x, original.popup.y),
                action: DialogPointerAction::Motion,
            },
            &modified_popup,
        ),
        DialogPointerOutcome::PlacementMismatch
    );

    let mut modified_choice = original.clone();
    modified_choice.choices[0].area.x += 1;
    assert_eq!(
        dialog.clone().drive_pointer(
            DialogPointerEvent {
                cell: Cell::new(original.choices[0].area.x, original.choices[0].area.y),
                action: DialogPointerAction::Motion,
            },
            &modified_choice,
        ),
        DialogPointerOutcome::PlacementMismatch
    );
}

#[test]
fn pointer_driving_rejects_stale_body_geometry() {
    let mut dialog = dialog();
    let mut placement = placement(&dialog);
    placement.body_area = Rect::new(12, 7, 30, 10);
    assert_eq!(
        dialog.clone().drive_pointer(
            DialogPointerEvent {
                cell: Cell::new(placement.popup.x, placement.popup.y),
                action: DialogPointerAction::Motion,
            },
            &placement,
        ),
        DialogPointerOutcome::PlacementMismatch
    );

    placement.body_area = Rect::new(11, 7, 1, 1);
    assert_eq!(
        dialog.drive_pointer(
            DialogPointerEvent {
                cell: Cell::new(placement.popup.x, placement.popup.y),
                action: DialogPointerAction::Motion,
            },
            &placement,
        ),
        DialogPointerOutcome::PlacementMismatch
    );
}

#[test]
fn pointer_driving_rejects_stale_placements() {
    let mut dialog = dialog();
    let mut placement = placement(&dialog);
    placement.choices[0].identity = Id::Other;
    assert_eq!(
        dialog.drive_pointer(
            DialogPointerEvent {
                cell: Cell::new(placement.popup.x, placement.popup.y),
                action: DialogPointerAction::Motion,
            },
            &placement,
        ),
        DialogPointerOutcome::PlacementMismatch
    );
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
