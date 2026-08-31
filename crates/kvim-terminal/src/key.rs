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
/// `BackTab` code is the `Shift-Tab` key itself, and `ShiftEnter` is the
/// `Shift-Enter` key itself, so both carry `Shift` too. Every other `Shift`
/// combination is a rejection.
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
        // The Shift modifier survives here as its own code. The rejection above
        // already refused every chord that this code cannot carry.
        CrosstermKeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::ShiftEnter,
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
    // Every other Shift combination would lose the modifier, so it is a
    // rejection.
    let folds_shift = match code {
        // A character key already carries Shift in its value, and `BackTab` is
        // the `Shift-Tab` key itself.
        CrosstermKeyCode::Char(_) | CrosstermKeyCode::BackTab => {
            !modifiers.contains(KeyModifiers::CONTROL)
        }
        // `Shift-Enter` is a key code of its own, exactly as `Shift-Tab` is. A
        // control or `Alt` chord over it carries no binding, and the `Alt`
        // alias of `Enter` names a different key, so both stay rejections
        // instead of losing the `Shift` modifier.
        CrosstermKeyCode::Enter => !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT),
        _ => false,
    };
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
#[path = "key_tests.rs"]
mod tests;
