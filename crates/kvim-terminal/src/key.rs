//! Conversion from one crossterm key event into a terminal-neutral key.
//!
//! The conversion accepts a key that kvim can bind and rejects every other key
//! with a typed reason. It never removes a modifier that it does not support:
//! an unsupported chord must not reach an unmodified binding.

use std::fmt;

use crossterm::event::{KeyCode as CrosstermKeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use kvim_keymap::{Chord, Key, KeyCode};
use thiserror::Error;

/// One terminal modifier that a normalized key cannot carry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UnsupportedModifier {
    /// `Shift` over a key that does not fold it into its value.
    Shift,
    /// `Alt` without `Ctrl` and without a legacy chord alias.
    Alt,
    /// The `Super` modifier, which no binding uses.
    Super,
    /// The `Hyper` modifier, which no binding uses.
    Hyper,
    /// The `Meta` modifier, which no binding uses.
    Meta,
}

impl fmt::Display for UnsupportedModifier {
    /// Writes the modifier name that a rejection message shows.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Shift => "Shift",
            Self::Alt => "Alt",
            Self::Super => "Super",
            Self::Hyper => "Hyper",
            Self::Meta => "Meta",
        })
    }
}

/// The reason that one crossterm key event carries no normalized key.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum KeyRejection {
    /// The terminal reported a key release, which carries no input.
    #[error("the terminal reported a key release")]
    Release,
    /// The terminal reported a modifier that no binding carries.
    ///
    /// The conversion rejects the key instead of removing the modifier, so a
    /// modified key never reaches the binding of the unmodified key.
    #[error("the terminal reported the unsupported {modifier} modifier")]
    UnsupportedModifier {
        /// The modifier that the key carried.
        modifier: UnsupportedModifier,
    },
    /// The terminal reported a key code that kvim does not bind.
    #[error("the terminal reported a key code that kvim does not bind")]
    UnsupportedCode,
}

/// Normalizes one crossterm key event.
///
/// The function accepts a press event and a repeat event. It rejects a release
/// event, an unsupported modifier, and a key code that kvim does not bind.
///
/// `Shift` is part of the character value, so a character key carries it. The
/// `BackTab` code is the `Shift-Tab` key itself, so it carries `Shift` too.
/// Every other `Shift` combination is a rejection.
///
/// ```
/// use crossterm::event::{KeyCode as CrosstermKeyCode, KeyEvent, KeyModifiers};
/// use kvim_terminal::{Key, KeyCode, normalize_key_event};
///
/// let event = KeyEvent::new(CrosstermKeyCode::Char('D'), KeyModifiers::CONTROL);
/// assert_eq!(normalize_key_event(event), Ok(Key::ctrl(KeyCode::Char('d'))));
/// ```
pub fn normalize_key_event(event: KeyEvent) -> Result<Key, KeyRejection> {
    if !matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return Err(KeyRejection::Release);
    }
    let modifiers = event.modifiers;
    if let Some(modifier) = unsupported_modifier(modifiers, event.code) {
        return Err(KeyRejection::UnsupportedModifier { modifier });
    }
    let control = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    if alt && !control {
        return alt_chord_alias(event.code).ok_or(KeyRejection::UnsupportedModifier {
            modifier: UnsupportedModifier::Alt,
        });
    }
    let chord = if control {
        if alt { Chord::CtrlAlt } else { Chord::Ctrl }
    } else {
        Chord::Plain
    };
    let code = match event.code {
        CrosstermKeyCode::Char(value) => KeyCode::Char(value),
        CrosstermKeyCode::Up => KeyCode::Up,
        CrosstermKeyCode::Down => KeyCode::Down,
        CrosstermKeyCode::Left => KeyCode::Left,
        CrosstermKeyCode::Right => KeyCode::Right,
        CrosstermKeyCode::Enter => KeyCode::Enter,
        CrosstermKeyCode::Tab => KeyCode::Tab,
        CrosstermKeyCode::BackTab => KeyCode::BackTab,
        CrosstermKeyCode::Backspace => KeyCode::Backspace,
        CrosstermKeyCode::Delete => KeyCode::Delete,
        CrosstermKeyCode::Home => KeyCode::Home,
        CrosstermKeyCode::End => KeyCode::End,
        CrosstermKeyCode::PageUp => KeyCode::PageUp,
        CrosstermKeyCode::PageDown => KeyCode::PageDown,
        CrosstermKeyCode::Esc => KeyCode::Esc,
        _ => return Err(KeyRejection::UnsupportedCode),
    };
    Ok(Key::new(chord, code))
}

/// Returns the modifier that makes the event unsupported.
///
/// `Alt` alone stays open here, because a legacy chord alias may still name a
/// bound key. [`normalize_key_event`] rejects the remaining `Alt` keys.
fn unsupported_modifier(
    modifiers: KeyModifiers,
    code: CrosstermKeyCode,
) -> Option<UnsupportedModifier> {
    if modifiers.contains(KeyModifiers::SUPER) {
        return Some(UnsupportedModifier::Super);
    }
    if modifiers.contains(KeyModifiers::HYPER) {
        return Some(UnsupportedModifier::Hyper);
    }
    if modifiers.contains(KeyModifiers::META) {
        return Some(UnsupportedModifier::Meta);
    }
    if !modifiers.contains(KeyModifiers::SHIFT) {
        return None;
    }
    // A character key already carries Shift in its value, and `BackTab` is the
    // `Shift-Tab` key itself. Every other Shift combination would lose the
    // modifier, so it is a rejection.
    let folds_shift = matches!(code, CrosstermKeyCode::Char(_) | CrosstermKeyCode::BackTab)
        && !modifiers.contains(KeyModifiers::CONTROL);
    if folds_shift {
        None
    } else {
        Some(UnsupportedModifier::Shift)
    }
}

/// Folds the `Alt` keys that kvim binds under another chord into that chord.
///
/// Two sources produce an `Alt` chord that names a bound key. Several terminals
/// and terminal multiplexers send `Ctrl-Alt-H`, `Ctrl-Alt-J`, `Ctrl-Alt-K`, and
/// `Ctrl-Alt-L` as `Alt` over a control character, or as `Alt-Backspace` and
/// `Alt-Enter`. macOS sends the `Option` chord as the `Alt` modifier, so
/// `Option-Left` and `Option-Right` arrive as `Alt-Left` and `Alt-Right`.
///
/// Both word chords name one motion each, so the alias folds the `Alt` arrows
/// into the `Ctrl` arrows and the mapping registry holds one entry for each word
/// motion. The alias returns `None` for every other `Alt` key.
fn alt_chord_alias(code: CrosstermKeyCode) -> Option<Key> {
    let aliased = match code {
        CrosstermKeyCode::Left => Key::ctrl(KeyCode::Left),
        CrosstermKeyCode::Right => Key::ctrl(KeyCode::Right),
        CrosstermKeyCode::Char('\u{8}') | CrosstermKeyCode::Backspace => {
            Key::ctrl_alt(KeyCode::Char('h'))
        }
        CrosstermKeyCode::Char('\n') | CrosstermKeyCode::Enter => Key::ctrl_alt(KeyCode::Char('j')),
        CrosstermKeyCode::Char('\u{b}') => Key::ctrl_alt(KeyCode::Char('k')),
        CrosstermKeyCode::Char('\u{c}') => Key::ctrl_alt(KeyCode::Char('l')),
        _ => return None,
    };
    Some(aliased)
}

#[cfg(test)]
mod tests {
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
}
