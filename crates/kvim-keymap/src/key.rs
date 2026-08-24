//! Terminal-neutral key values and their help form.

use std::fmt;

/// The maximum number of bytes that one [`KeyLabel`] writes.
///
/// The longest chord prefix is `C-A-`, and the longest key name is `S-Tab`. A
/// character key writes at most four bytes after the prefix. The bound therefore
/// covers every label, and a help layout can reserve one fixed column width.
pub const KEY_LABEL_BYTES_MAX: usize = 9;

/// The modifier chord that a key carries.
///
/// A key accepts three chords only. Shift is already folded into the character
/// value, and a plain `Alt` chord carries no binding. One chord value per key
/// keeps an invalid modifier combination unrepresentable.
///
/// ```
/// use kvim_keymap::{Chord, Key, KeyCode};
///
/// assert_eq!(Key::ctrl(KeyCode::Char('d')).chord(), Chord::Ctrl);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Chord {
    /// No control modifier.
    Plain,
    /// The `Ctrl` modifier.
    Ctrl,
    /// The `Ctrl` and `Alt` modifiers together.
    CtrlAlt,
}

impl Chord {
    /// Returns the help prefix of the chord.
    ///
    /// ```
    /// use kvim_keymap::Chord;
    ///
    /// assert_eq!(Chord::Plain.prefix(), "");
    /// assert_eq!(Chord::CtrlAlt.prefix(), "C-A-");
    /// ```
    #[inline]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Plain => "",
            Self::Ctrl => "C-",
            Self::CtrlAlt => "C-A-",
        }
    }
}

/// The terminal-neutral code of one key.
///
/// The list holds the keys that a text interface binds. A terminal adapter
/// converts its own event into one of these codes, or it rejects the event.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum KeyCode {
    /// A printable character. Shift is folded into the character value.
    Char(char),
    /// The up arrow key.
    Up,
    /// The down arrow key.
    Down,
    /// The left arrow key.
    Left,
    /// The right arrow key.
    Right,
    /// The `Enter` key, which some terminals report as `Return`.
    Enter,
    /// The `Tab` key.
    Tab,
    /// The `Shift-Tab` key, which terminals report as one code.
    BackTab,
    /// The `Backspace` key, which removes the character before the cursor.
    Backspace,
    /// The `Delete` key, which removes the character under the cursor.
    Delete,
    /// The `Home` key.
    Home,
    /// The `End` key.
    End,
    /// The `Page Up` key.
    PageUp,
    /// The `Page Down` key.
    PageDown,
    /// The `Esc` key, which cancels the current input.
    Esc,
}

impl KeyCode {
    /// Returns the help name of the code, or `None` for a character.
    ///
    /// A character key writes its own value, so it carries no fixed name. The
    /// space character is invisible, so it carries the name `Space`.
    ///
    /// ```
    /// use kvim_keymap::KeyCode;
    ///
    /// assert_eq!(KeyCode::BackTab.name(), Some("S-Tab"));
    /// assert_eq!(KeyCode::Char(' ').name(), Some("Space"));
    /// assert_eq!(KeyCode::Char('d').name(), None);
    /// ```
    #[inline]
    pub const fn name(self) -> Option<&'static str> {
        let name = match self {
            Self::Char(' ') => "Space",
            Self::Char(_) => return None,
            Self::Up => "Up",
            Self::Down => "Down",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Enter => "Enter",
            Self::Tab => "Tab",
            Self::BackTab => "S-Tab",
            Self::Backspace => "BS",
            Self::Delete => "Del",
            Self::Home => "Home",
            Self::End => "End",
            Self::PageUp => "PgUp",
            Self::PageDown => "PgDn",
            Self::Esc => "Esc",
        };
        Some(name)
    }
}

/// One normalized key press.
///
/// The type holds one invariant: a `Ctrl` or `Ctrl-Alt` chord over a character
/// always stores the lowercase ASCII form. `Ctrl-D` and `Ctrl-d` therefore
/// compare equal, and a registry needs one entry for the chord.
///
/// ```
/// use kvim_keymap::{Key, KeyCode};
///
/// assert_eq!(Key::ctrl(KeyCode::Char('D')), Key::ctrl(KeyCode::Char('d')));
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Key {
    chord: Chord,
    code: KeyCode,
}

impl Key {
    /// Builds a key and establishes the lowercase invariant for control chords.
    ///
    /// ```
    /// use kvim_keymap::{Chord, Key, KeyCode};
    ///
    /// let key = Key::new(Chord::Ctrl, KeyCode::Char('W'));
    /// assert_eq!(key.code(), KeyCode::Char('w'));
    /// ```
    #[inline]
    #[must_use]
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
    ///
    /// ```
    /// use kvim_keymap::{Chord, Key, KeyCode};
    ///
    /// assert_eq!(Key::plain(KeyCode::Esc).chord(), Chord::Plain);
    /// ```
    #[inline]
    #[must_use]
    pub fn plain(code: KeyCode) -> Self {
        Self::new(Chord::Plain, code)
    }

    /// Builds a `Ctrl` chord key.
    ///
    /// ```
    /// use kvim_keymap::{Chord, Key, KeyCode};
    ///
    /// assert_eq!(Key::ctrl(KeyCode::Left).chord(), Chord::Ctrl);
    /// ```
    #[inline]
    #[must_use]
    pub fn ctrl(code: KeyCode) -> Self {
        Self::new(Chord::Ctrl, code)
    }

    /// Builds a `Ctrl-Alt` chord key.
    ///
    /// ```
    /// use kvim_keymap::{Chord, Key, KeyCode};
    ///
    /// assert_eq!(Key::ctrl_alt(KeyCode::Char('h')).chord(), Chord::CtrlAlt);
    /// ```
    #[inline]
    #[must_use]
    pub fn ctrl_alt(code: KeyCode) -> Self {
        Self::new(Chord::CtrlAlt, code)
    }

    /// Returns the modifier chord of the key.
    ///
    /// ```
    /// use kvim_keymap::{Chord, Key, KeyCode};
    ///
    /// assert_eq!(Key::plain(KeyCode::Tab).chord(), Chord::Plain);
    /// ```
    #[inline]
    #[must_use]
    pub const fn chord(self) -> Chord {
        self.chord
    }

    /// Returns the code of the key.
    ///
    /// ```
    /// use kvim_keymap::{Key, KeyCode};
    ///
    /// assert_eq!(Key::plain(KeyCode::Tab).code(), KeyCode::Tab);
    /// ```
    #[inline]
    #[must_use]
    pub const fn code(self) -> KeyCode {
        self.code
    }

    /// Returns the character that the key types, or `None` for a key that types
    /// no text.
    ///
    /// Only a plain chord types text. A control chord names a binding, so it
    /// never reaches a text fallback.
    ///
    /// ```
    /// use kvim_keymap::{Key, KeyCode};
    ///
    /// assert_eq!(Key::plain(KeyCode::Char('a')).typed_char(), Some('a'));
    /// assert_eq!(Key::ctrl(KeyCode::Char('a')).typed_char(), None);
    /// assert_eq!(Key::plain(KeyCode::Enter).typed_char(), None);
    /// ```
    #[inline]
    #[must_use]
    pub const fn typed_char(self) -> Option<char> {
        match (self.chord, self.code) {
            (Chord::Plain, KeyCode::Char(value)) => Some(value),
            _ => None,
        }
    }

    /// Returns the key in its help form.
    ///
    /// ```
    /// use kvim_keymap::{Key, KeyCode};
    ///
    /// assert_eq!(Key::ctrl(KeyCode::Char('d')).label().to_string(), "C-d");
    /// ```
    #[inline]
    #[must_use]
    pub const fn label(self) -> KeyLabel {
        KeyLabel(self)
    }
}

/// One key in the help form that a which-key overlay shows.
///
/// The form names a chord prefix and a key name, such as `C-d`, `Space`, or
/// `Enter`. It is help text, never a value that code compares. One label writes
/// at most [`KEY_LABEL_BYTES_MAX`] bytes.
///
/// ```
/// use kvim_keymap::{Key, KeyCode};
///
/// assert_eq!(Key::plain(KeyCode::Char(' ')).label().to_string(), "Space");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyLabel(Key);

impl fmt::Display for KeyLabel {
    /// Writes one key in its help form.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.chord().prefix())?;
        match self.0.code().name() {
            Some(name) => formatter.write_str(name),
            None => {
                let KeyCode::Char(value) = self.0.code() else {
                    debug_assert!(false, "only a character code answers `None` to `name`");
                    unreachable!();
                };
                write!(formatter, "{value}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
}
