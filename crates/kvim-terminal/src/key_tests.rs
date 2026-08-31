use crossterm::event::{KeyEventState, ModifierKeyCode};

use super::*;

fn event(code: CrosstermKeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
    KeyEvent::new_with_kind_and_state(code, modifiers, kind, KeyEventState::NONE)
}

fn pressed(code: CrosstermKeyCode, modifiers: KeyModifiers) -> KeyEvent {
    event(code, modifiers, KeyEventKind::Press)
}

#[test]
fn press_and_repeat_normalize_but_release_does_not() {
    let expected = Ok(Key::plain(KeyCode::Char('j')));
    for kind in [KeyEventKind::Press, KeyEventKind::Repeat] {
        let sent = event(CrosstermKeyCode::Char('j'), KeyModifiers::NONE, kind);
        assert_eq!(normalize_key_event(sent), expected);
    }
    let released = event(
        CrosstermKeyCode::Char('j'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );
    assert_eq!(normalize_key_event(released), Err(KeyRejection::Release));
}

#[test]
fn legacy_alt_encodings_restore_the_control_alt_chord() {
    let cases = [
        (CrosstermKeyCode::Char('\u{8}'), 'h'),
        (CrosstermKeyCode::Backspace, 'h'),
        (CrosstermKeyCode::Char('\n'), 'j'),
        (CrosstermKeyCode::Enter, 'j'),
        (CrosstermKeyCode::Char('\u{b}'), 'k'),
        (CrosstermKeyCode::Char('\u{c}'), 'l'),
    ];
    for (code, target) in cases {
        let sent = pressed(code, KeyModifiers::ALT);
        assert_eq!(
            normalize_key_event(sent),
            Ok(Key::ctrl_alt(KeyCode::Char(target))),
            "the legacy Alt encoding {code:?} must restore Ctrl-Alt-{target}"
        );
    }
}

#[test]
fn a_true_control_alt_chord_bypasses_the_legacy_fixup() {
    let sent = pressed(
        CrosstermKeyCode::Char('H'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert_eq!(
        normalize_key_event(sent),
        Ok(Key::ctrl_alt(KeyCode::Char('h')))
    );
}

#[test]
fn an_arrow_key_carries_its_modifier_chord() {
    // The enhanced keyboard reporting flags of `lifecycle` keep the three
    // forms distinct, so the normalizer must not fold them together.
    let cases = [
        (
            CrosstermKeyCode::Left,
            KeyModifiers::NONE,
            Key::plain(KeyCode::Left),
        ),
        (
            CrosstermKeyCode::Right,
            KeyModifiers::NONE,
            Key::plain(KeyCode::Right),
        ),
        (
            CrosstermKeyCode::Up,
            KeyModifiers::NONE,
            Key::plain(KeyCode::Up),
        ),
        (
            CrosstermKeyCode::Down,
            KeyModifiers::NONE,
            Key::plain(KeyCode::Down),
        ),
        (
            CrosstermKeyCode::Left,
            KeyModifiers::CONTROL,
            Key::ctrl(KeyCode::Left),
        ),
        (
            CrosstermKeyCode::Right,
            KeyModifiers::CONTROL,
            Key::ctrl(KeyCode::Right),
        ),
        // macOS sends the `Option` chord as the `Alt` modifier, and both
        // word chords name one motion each.
        (
            CrosstermKeyCode::Left,
            KeyModifiers::ALT,
            Key::ctrl(KeyCode::Left),
        ),
        (
            CrosstermKeyCode::Right,
            KeyModifiers::ALT,
            Key::ctrl(KeyCode::Right),
        ),
    ];
    for (code, modifiers, expected) in cases {
        let sent = pressed(code, modifiers);
        assert_eq!(
            normalize_key_event(sent),
            Ok(expected),
            "{code:?} with {modifiers:?} must normalize to {expected:?}"
        );
    }
}

#[test]
fn shift_enter_normalizes_to_its_own_code_and_leaves_enter_alone() {
    // A terminal with enhanced keyboard reporting sends `Shift-Enter` as
    // `Enter` with the modifier. It becomes one key of its own, so a host can
    // bind it. See `docs/input-actions.md`.
    assert_eq!(
        normalize_key_event(pressed(CrosstermKeyCode::Enter, KeyModifiers::SHIFT)),
        Ok(Key::plain(KeyCode::ShiftEnter))
    );

    // A terminal without that capability sends the same bytes as `Enter`, so
    // the plain key is unchanged and never acquires the shifted meaning.
    assert_eq!(
        normalize_key_event(pressed(CrosstermKeyCode::Enter, KeyModifiers::NONE)),
        Ok(Key::plain(KeyCode::Enter))
    );

    // The `Alt-Enter` alias of `Ctrl-Alt-J` keeps its own meaning.
    assert_eq!(
        normalize_key_event(pressed(CrosstermKeyCode::Enter, KeyModifiers::ALT)),
        Ok(Key::ctrl_alt(KeyCode::Char('j')))
    );
}

#[test]
fn an_unsupported_modifier_is_rejected_and_never_loses_its_chord() {
    // `docs/input-actions.md` binds `w`, `Up`, and `Tab`. A key that keeps
    // an unsupported modifier must not reach any of those bindings.
    let cases = [
        (
            CrosstermKeyCode::Char('w'),
            KeyModifiers::ALT,
            UnsupportedModifier::Alt,
        ),
        (
            CrosstermKeyCode::Up,
            KeyModifiers::SHIFT,
            UnsupportedModifier::Shift,
        ),
        // `Shift-Enter` is one key of its own, but no chord over it names a
        // binding. Both chords therefore reject instead of losing `Shift` and
        // reaching the binding of `Enter` or of the `Alt-Enter` alias.
        (
            CrosstermKeyCode::Enter,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            UnsupportedModifier::Shift,
        ),
        (
            CrosstermKeyCode::Enter,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
            UnsupportedModifier::Shift,
        ),
        (
            CrosstermKeyCode::Char('d'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            UnsupportedModifier::Shift,
        ),
        (
            CrosstermKeyCode::Char('p'),
            KeyModifiers::SUPER,
            UnsupportedModifier::Super,
        ),
        (
            CrosstermKeyCode::Char('p'),
            KeyModifiers::HYPER,
            UnsupportedModifier::Hyper,
        ),
        (
            CrosstermKeyCode::Char('p'),
            KeyModifiers::META,
            UnsupportedModifier::Meta,
        ),
    ];
    for (code, modifiers, modifier) in cases {
        assert_eq!(
            normalize_key_event(pressed(code, modifiers)),
            Err(KeyRejection::UnsupportedModifier { modifier }),
            "{code:?} with {modifiers:?} must keep its chord and be rejected"
        );
    }
}

#[test]
fn window_chords_stay_distinct() {
    let split = pressed(CrosstermKeyCode::Enter, KeyModifiers::CONTROL);
    assert_eq!(
        normalize_key_event(split),
        Ok(Key::ctrl(KeyCode::Enter)),
        "docs/input-actions.md binds Ctrl-Enter to the adaptive split"
    );
    let inverse = pressed(CrosstermKeyCode::Char('\\'), KeyModifiers::CONTROL);
    assert_eq!(
        normalize_key_event(inverse),
        Ok(Key::ctrl(KeyCode::Char('\\'))),
        "docs/input-actions.md binds Ctrl-\\ to the inverse adaptive split"
    );
}

#[test]
fn shift_stays_folded_into_the_character_value() {
    let sent = pressed(CrosstermKeyCode::Char('V'), KeyModifiers::SHIFT);
    assert_eq!(
        normalize_key_event(sent),
        Ok(Key::plain(KeyCode::Char('V')))
    );
}

#[test]
fn back_tab_carries_the_shift_that_names_it() {
    let sent = pressed(CrosstermKeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(normalize_key_event(sent), Ok(Key::plain(KeyCode::BackTab)));
}

#[test]
fn an_unused_key_does_not_normalize() {
    let function = pressed(CrosstermKeyCode::F(5), KeyModifiers::NONE);
    assert_eq!(
        normalize_key_event(function),
        Err(KeyRejection::UnsupportedCode)
    );
    let modifier = pressed(
        CrosstermKeyCode::Modifier(ModifierKeyCode::LeftShift),
        KeyModifiers::NONE,
    );
    assert_eq!(
        normalize_key_event(modifier),
        Err(KeyRejection::UnsupportedCode)
    );
}
