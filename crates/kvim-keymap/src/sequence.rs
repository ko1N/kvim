//! The validated key sequence that a registry stores.

use std::borrow::Borrow;
use std::fmt;

use thiserror::Error;

use crate::key::Key;

/// The largest sequence length that any registry accepts.
///
/// A pending sequence lives in one interface instance, and a host chooses its
/// own shorter limit. This ceiling keeps that choice finite, so no host can ask
/// a registry to store a sequence that a pending buffer could never hold.
pub const SEQUENCE_KEYS_MAX: u8 = 8;

/// A rejected key sequence.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SequenceError {
    /// The sequence held no key.
    #[error("a key sequence holds no key")]
    Empty,
    /// The sequence held more keys than the caller's limit allows.
    #[error("a key sequence holds {keys} keys, but the limit is {keys_max}")]
    TooLong {
        /// The number of keys in the rejected sequence.
        keys: usize,
        /// The limit that the sequence broke.
        keys_max: u8,
    },
}

/// A non-empty key sequence that fits one caller-supplied length limit.
///
/// The type holds both bounds, so a registry cannot store a sequence that a
/// pending buffer could never complete.
///
/// ```
/// use kvim_keymap::{Key, KeyCode, KeySequence};
///
/// let keys = [Key::plain(KeyCode::Char('g')), Key::plain(KeyCode::Char('d'))];
/// let sequence = KeySequence::new(&keys, 4).expect("two keys fit the limit");
/// assert_eq!(sequence.to_string(), "g d");
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeySequence(Vec<Key>);

impl KeySequence {
    /// Builds a sequence and checks both bounds.
    ///
    /// # Errors
    ///
    /// Returns [`SequenceError::Empty`] for an empty slice and
    /// [`SequenceError::TooLong`] for a slice above `keys_max`.
    ///
    /// ```
    /// use kvim_keymap::{Key, KeyCode, KeySequence, SequenceError};
    ///
    /// assert_eq!(KeySequence::new(&[], 4), Err(SequenceError::Empty));
    /// let keys = [Key::plain(KeyCode::Esc); 3];
    /// assert!(matches!(
    ///     KeySequence::new(&keys, 2),
    ///     Err(SequenceError::TooLong { keys: 3, keys_max: 2 })
    /// ));
    /// ```
    pub fn new(keys: &[Key], keys_max: u8) -> Result<Self, SequenceError> {
        if keys.is_empty() {
            return Err(SequenceError::Empty);
        }
        if keys.len() > usize::from(keys_max) {
            return Err(SequenceError::TooLong {
                keys: keys.len(),
                keys_max,
            });
        }
        Ok(Self(keys.to_vec()))
    }

    /// Returns the keys of the sequence.
    ///
    /// ```
    /// use kvim_keymap::{Key, KeyCode, KeySequence};
    ///
    /// let sequence = KeySequence::new(&[Key::plain(KeyCode::Esc)], 4)
    ///     .expect("one key fits the limit");
    /// assert_eq!(sequence.keys(), &[Key::plain(KeyCode::Esc)]);
    /// ```
    #[inline]
    #[must_use]
    pub fn keys(&self) -> &[Key] {
        &self.0
    }
}

impl Borrow<[Key]> for KeySequence {
    /// Lets a lookup use a plain key slice.
    ///
    /// The ordering of `[Key]` equals the ordering of the wrapped `Vec<Key>`, so
    /// the borrowed form keeps the map ordering valid.
    #[inline]
    fn borrow(&self) -> &[Key] {
        &self.0
    }
}

impl fmt::Display for KeySequence {
    /// Writes the keys separated by one space.
    ///
    /// A named key such as `S-Tab` is several characters wide, so a separator
    /// keeps `S-Tab f` distinct from a single key.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, key) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(" ")?;
            }
            write!(formatter, "{}", key.label())?;
        }
        Ok(())
    }
}
