//! Terminal-independent key values and crossterm key normalization.

use crossterm::event::{KeyCode as CrosstermKeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// The modifier chord that a normalized key carries.
///
/// Kvim accepts three chords only. Shift is already folded into the character
/// value, and a plain `Alt` chord carries no Kvim binding. One chord value per
/// key keeps an invalid modifier combination unrepresentable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Chord {
    /// No control modifier.
    Plain,
    /// The `Ctrl` modifier.
    Ctrl,
    /// The `Ctrl` and `Alt` modifiers together.
    CtrlAlt,
}

/// The terminal-independent code of a normalized key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum KeyCode {
    /// A printable character. Shift is folded into the character value.
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Enter,
    Tab,
    /// The `Shift-Tab` key, which terminals report as one code.
    BackTab,
    Backspace,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Esc,
}

/// One normalized key press.
///
/// The type holds one invariant: a `Ctrl` or `Ctrl-Alt` chord over a character
/// always stores the lowercase ASCII form. `Ctrl-D` and `Ctrl-d` therefore
/// compare equal, and the mapping registry needs one entry for the chord.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Key {
    chord: Chord,
    code: KeyCode,
}

impl Key {
    /// Builds a key and establishes the lowercase invariant for control chords.
    #[inline]
    pub fn new(chord: Chord, code: KeyCode) -> Self {
        let code = match (chord, code) {
            (Chord::Ctrl | Chord::CtrlAlt, KeyCode::Char(value)) => {
                KeyCode::Char(value.to_ascii_lowercase())
            }
            (_, code) => code,
        };
        Self { chord, code }
    }

    /// Builds a key without a control modifier.
    #[inline]
    pub fn plain(code: KeyCode) -> Self {
        Self::new(Chord::Plain, code)
    }

    /// Builds a `Ctrl` chord key.
    #[inline]
    pub fn ctrl(code: KeyCode) -> Self {
        Self::new(Chord::Ctrl, code)
    }

    /// Builds a `Ctrl-Alt` chord key.
    #[inline]
    pub fn ctrl_alt(code: KeyCode) -> Self {
        Self::new(Chord::CtrlAlt, code)
    }

    /// Returns the modifier chord of the key.
    #[inline]
    pub const fn chord(self) -> Chord {
        self.chord
    }

    /// Returns the code of the key.
    #[inline]
    pub const fn code(self) -> KeyCode {
        self.code
    }

    /// Normalizes one crossterm key event.
    ///
    /// The function accepts a press event and a repeat event. It returns `None`
    /// for a release event and for a key that Kvim does not use.
    ///
    /// ```
    /// use crossterm::event::{KeyCode as CrosstermKeyCode, KeyEvent, KeyModifiers};
    /// use kvim::terminal::{Key, KeyCode};
    ///
    /// let event = KeyEvent::new(CrosstermKeyCode::Char('D'), KeyModifiers::CONTROL);
    /// assert_eq!(Key::from_key_event(event), Some(Key::ctrl(KeyCode::Char('d'))));
    /// ```
    pub fn from_key_event(event: KeyEvent) -> Option<Self> {
        if !matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return None;
        }
        let control = event.modifiers.contains(KeyModifiers::CONTROL);
        let alt = event.modifiers.contains(KeyModifiers::ALT);
        if alt
            && !control
            && let Some(key) = legacy_alt_chord(event.code)
        {
            return Some(key);
        }
        let chord = match (control, alt) {
            (true, true) => Chord::CtrlAlt,
            (true, false) => Chord::Ctrl,
            // A plain `Alt` chord carries no binding, so the key keeps its
            // unmodified form.
            (false, _) => Chord::Plain,
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
            _ => return None,
        };
        Some(Self::new(chord, code))
    }
}

/// Restores the `Ctrl-Alt-H`, `Ctrl-Alt-J`, `Ctrl-Alt-K`, and `Ctrl-Alt-L`
/// chords from the legacy `Alt` encoding.
///
/// Several terminals and terminal multiplexers send these chords as `Alt` over a
/// control character, or as `Alt-Backspace` and `Alt-Enter`. The fixup keeps
/// directional pane resizing usable when the terminal cannot report the true
/// chord. It returns `None` for every other `Alt` key.
fn legacy_alt_chord(code: CrosstermKeyCode) -> Option<Key> {
    let target = match code {
        CrosstermKeyCode::Char('\u{8}') | CrosstermKeyCode::Backspace => 'h',
        CrosstermKeyCode::Char('\n') | CrosstermKeyCode::Enter => 'j',
        CrosstermKeyCode::Char('\u{b}') => 'k',
        CrosstermKeyCode::Char('\u{c}') => 'l',
        _ => return None,
    };
    Some(Key::ctrl_alt(KeyCode::Char(target)))
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEventState, ModifierKeyCode};

    use super::*;

    fn event(code: CrosstermKeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        KeyEvent::new_with_kind_and_state(code, modifiers, kind, KeyEventState::NONE)
    }

    #[test]
    fn press_and_repeat_normalize_but_release_does_not() {
        let expected = Some(Key::plain(KeyCode::Char('j')));
        for kind in [KeyEventKind::Press, KeyEventKind::Repeat] {
            let pressed = event(CrosstermKeyCode::Char('j'), KeyModifiers::NONE, kind);
            assert_eq!(Key::from_key_event(pressed), expected);
        }
        let released = event(
            CrosstermKeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(Key::from_key_event(released), None);
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
            let sent = event(code, KeyModifiers::ALT, KeyEventKind::Press);
            assert_eq!(
                Key::from_key_event(sent),
                Some(Key::ctrl_alt(KeyCode::Char(target))),
                "the legacy Alt encoding {code:?} must restore Ctrl-Alt-{target}"
            );
        }
    }

    #[test]
    fn a_true_control_alt_chord_bypasses_the_legacy_fixup() {
        let sent = event(
            CrosstermKeyCode::Char('H'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            KeyEventKind::Press,
        );
        assert_eq!(
            Key::from_key_event(sent),
            Some(Key::ctrl_alt(KeyCode::Char('h')))
        );
    }

    #[test]
    fn a_plain_alt_key_keeps_its_unmodified_form() {
        let sent = event(
            CrosstermKeyCode::Char('w'),
            KeyModifiers::ALT,
            KeyEventKind::Press,
        );
        assert_eq!(
            Key::from_key_event(sent),
            Some(Key::plain(KeyCode::Char('w')))
        );
    }

    #[test]
    fn window_chords_stay_distinct() {
        let split = event(
            CrosstermKeyCode::Enter,
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        assert_eq!(
            Key::from_key_event(split),
            Some(Key::ctrl(KeyCode::Enter)),
            "docs/input-actions.md binds Ctrl-Enter to the adaptive split"
        );
        let inverse = event(
            CrosstermKeyCode::Char('\\'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        assert_eq!(
            Key::from_key_event(inverse),
            Some(Key::ctrl(KeyCode::Char('\\'))),
            "docs/input-actions.md binds Ctrl-\\ to the inverse adaptive split"
        );
    }

    #[test]
    fn shift_stays_folded_into_the_character_value() {
        let sent = event(
            CrosstermKeyCode::Char('V'),
            KeyModifiers::SHIFT,
            KeyEventKind::Press,
        );
        assert_eq!(
            Key::from_key_event(sent),
            Some(Key::plain(KeyCode::Char('V')))
        );
    }

    #[test]
    fn an_unused_key_does_not_normalize() {
        let function = event(
            CrosstermKeyCode::F(5),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
        assert_eq!(Key::from_key_event(function), None);
        let modifier = event(
            CrosstermKeyCode::Modifier(ModifierKeyCode::LeftShift),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
        assert_eq!(Key::from_key_event(modifier), None);
    }
}
