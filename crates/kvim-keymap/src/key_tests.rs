use super::{Chord, KEY_LABEL_BYTES_MAX, Key, KeyCode};

/// Every key code, with one character key for each label branch.
const CODES: &[KeyCode] = &[
    KeyCode::Char(' '),
    KeyCode::Char('d'),
    // Four bytes in UTF-8, which is the widest character a label can hold.
    KeyCode::Char('\u{1f600}'),
    KeyCode::Up,
    KeyCode::Down,
    KeyCode::Left,
    KeyCode::Right,
    KeyCode::Enter,
    KeyCode::ShiftEnter,
    KeyCode::Tab,
    KeyCode::BackTab,
    KeyCode::Backspace,
    KeyCode::Delete,
    KeyCode::Home,
    KeyCode::End,
    KeyCode::PageUp,
    KeyCode::PageDown,
    KeyCode::Esc,
];

#[test]
fn every_label_fits_the_label_bound() {
    for chord in [Chord::Plain, Chord::Ctrl, Chord::CtrlAlt] {
        for code in CODES {
            let label = Key::new(chord, *code).label().to_string();
            assert!(
                label.len() <= KEY_LABEL_BYTES_MAX,
                "`{label}` writes {} bytes, but the bound is {KEY_LABEL_BYTES_MAX}",
                label.len()
            );
        }
    }
}

#[test]
fn a_shifted_code_is_one_key_of_its_own() {
    // The model carries no Shift chord, so a shifted key that a terminal
    // reports distinctly is its own code. It therefore never compares equal to
    // the unmodified key, and a registry holds one entry for each.
    assert_ne!(Key::plain(KeyCode::ShiftEnter), Key::plain(KeyCode::Enter));
    assert_ne!(Key::plain(KeyCode::BackTab), Key::plain(KeyCode::Tab));
    assert_eq!(Key::plain(KeyCode::ShiftEnter).label().to_string(), "S-↵");
}

#[test]
fn a_control_chord_folds_the_character_case() {
    assert_eq!(Key::ctrl(KeyCode::Char('D')), Key::ctrl(KeyCode::Char('d')));
    assert_eq!(
        Key::ctrl_alt(KeyCode::Char('H')),
        Key::ctrl_alt(KeyCode::Char('h'))
    );
    // Shift is part of the character value without a control chord, so the
    // plain chord must keep the upper-case form.
    assert_ne!(
        Key::plain(KeyCode::Char('V')),
        Key::plain(KeyCode::Char('v'))
    );
}
