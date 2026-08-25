//! The registers and the shape that a paste follows.
//!
//! The `editor` module owns every register value. The `clipboard` module mirrors
//! the unnamed register into the system clipboard, and it never sees a named
//! register. A clipboard failure never removes the value that this module
//! holds. See `docs/clipboard.md`.

use std::collections::BTreeMap;

use kvim_core::LineEnding;

/// The largest text that one register holds, in bytes.
///
/// A yank reads one buffer, and the file settings bound one buffer at 4 MiB, so
/// a yank cannot reach this bound. The bound protects the register against an
/// external value that grew past the buffer bound.
pub const REGISTER_BYTES_MAX: usize = 4 * 1024 * 1024;

/// The largest number of named registers that one session holds.
///
/// The accepted names are the 26 ASCII letters and the 10 ASCII digits. The
/// `input` charter validates every name before it reaches this module, and an
/// upper-case name appends to its lower-case register, so it opens no further
/// entry. The map therefore cannot pass this bound.
pub const NAMED_REGISTERS_MAX: usize = 36;

/// The shape that a paste of one register value follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterShape {
    /// A run of characters. A paste puts the text beside the cursor.
    Characterwise,
    /// Complete lines. The text ends with one line ending.
    Linewise,
    /// A rectangle of columns. One register line belongs to one buffer line.
    Blockwise,
}

/// One register value: the stored text and the shape that a paste follows.
///
/// # Examples
///
/// ```
/// use kvim_core::LineEnding;
/// use kvim_editor::{RegisterShape, RegisterValue};
///
/// let value = RegisterValue::linewise("one", LineEnding::Lf);
/// assert_eq!(value.shape(), RegisterShape::Linewise);
/// // A linewise value always ends with one line ending.
/// assert_eq!(value.text(), "one\n");
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterValue {
    text: String,
    shape: RegisterShape,
}

impl RegisterValue {
    /// Creates a value with an explicit shape.
    ///
    /// A linewise text must already end with one line ending. Prefer
    /// [`RegisterValue::linewise`], which appends a missing line ending.
    #[must_use]
    pub fn new(text: impl Into<String>, shape: RegisterShape) -> Self {
        let text = text.into();
        debug_assert!(
            text.len() <= REGISTER_BYTES_MAX,
            "the buffer bound and the clipboard bound both stay below the register bound"
        );
        Self { text, shape }
    }

    /// Creates a characterwise value.
    #[must_use]
    pub fn characterwise(text: impl Into<String>) -> Self {
        Self::new(text, RegisterShape::Characterwise)
    }

    /// Creates a linewise value and appends a missing line ending.
    #[must_use]
    pub fn linewise(text: impl Into<String>, line_ending: LineEnding) -> Self {
        let mut text = text.into();
        if !text.ends_with(line_ending.as_str()) {
            text.push_str(line_ending.as_str());
        }
        Self::new(text, RegisterShape::Linewise)
    }

    /// Creates a blockwise value from one text for each selected line.
    ///
    /// A line that the block does not reach contributes an empty text, so the
    /// rectangle keeps one register line for each selected buffer line.
    #[must_use]
    pub fn blockwise(lines: &[String], line_ending: LineEnding) -> Self {
        Self::new(lines.join(line_ending.as_str()), RegisterShape::Blockwise)
    }

    /// Returns the stored text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the shape that a paste follows.
    #[must_use]
    pub const fn shape(&self) -> RegisterShape {
        self.shape
    }

    /// Reports whether the value holds no text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Returns one text for each line of a blockwise value.
    #[must_use]
    pub fn block_lines(&self, line_ending: LineEnding) -> Vec<&str> {
        self.text.split(line_ending.as_str()).collect()
    }

    /// Repeats the value, as a count before a paste command requests.
    ///
    /// A characterwise or linewise value repeats its complete text. A blockwise
    /// value repeats each block line, because the rectangle grows sideways. The
    /// value stays unchanged when the repeated text would pass
    /// [`REGISTER_BYTES_MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::LineEnding;
    /// use kvim_editor::RegisterValue;
    ///
    /// let value = RegisterValue::characterwise("ab").repeated(3, LineEnding::Lf);
    /// assert_eq!(value.text(), "ababab");
    /// ```
    #[must_use]
    pub fn repeated(&self, times: usize, line_ending: LineEnding) -> Self {
        if times <= 1 || self.text.len().saturating_mul(times) > REGISTER_BYTES_MAX {
            return self.clone();
        }
        let text = match self.shape {
            RegisterShape::Characterwise | RegisterShape::Linewise => self.text.repeat(times),
            RegisterShape::Blockwise => self
                .block_lines(line_ending)
                .iter()
                .map(|line| line.repeat(times))
                .collect::<Vec<_>>()
                .join(line_ending.as_str()),
        };
        Self::new(text, self.shape)
    }

    /// Returns this value with `other` appended to it.
    ///
    /// An upper-case register name appends, which is the rule that Vim follows.
    /// The result keeps the shape of this value, and a linewise result keeps its
    /// final line ending, so a later paste still opens whole lines. The value
    /// stays unchanged when the joined text would pass [`REGISTER_BYTES_MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::LineEnding;
    /// use kvim_editor::RegisterValue;
    ///
    /// let first = RegisterValue::linewise("one", LineEnding::Lf);
    /// let joined = first.appended(&RegisterValue::linewise("two", LineEnding::Lf), LineEnding::Lf);
    /// assert_eq!(joined.text(), "one\ntwo\n");
    /// ```
    #[must_use]
    pub fn appended(&self, other: &Self, line_ending: LineEnding) -> Self {
        if self.text.len().saturating_add(other.text.len()) > REGISTER_BYTES_MAX {
            return self.clone();
        }
        let mut text = self.text.clone();
        text.push_str(&other.text);
        match self.shape {
            RegisterShape::Linewise => Self::linewise(text, line_ending),
            RegisterShape::Characterwise | RegisterShape::Blockwise => Self::new(text, self.shape),
        }
    }
}

/// The register that one operation writes to or reads from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegisterTarget {
    /// The unnamed register, which the system clipboard mirrors.
    Unnamed,
    /// One named register, which the system clipboard never sees.
    Named(char),
    /// One named register that a write appends to.
    Append(char),
    /// The black-hole register: a write discards, and a read holds nothing.
    BlackHole,
}

impl RegisterTarget {
    /// Returns the target that one resolved register name selects.
    ///
    /// `None` and `"` both name the unnamed register, because an operation
    /// without a name and an operation that names `"` reach the same place.
    fn of(register: Option<char>) -> Self {
        match register {
            None | Some('"') => Self::Unnamed,
            Some('_') => Self::BlackHole,
            Some(name) if name.is_ascii_uppercase() => Self::Append(name.to_ascii_lowercase()),
            Some(name) if name.is_ascii_alphanumeric() => Self::Named(name),
            // The input charter validates every name before it arrives here.
            Some(_) => Self::Unnamed,
        }
    }
}

/// The registers that one editor session holds.
///
/// The unnamed register answers every operation that names no register. A named
/// register answers one operation that names it, and the system clipboard never
/// sees it, because [`Registers::revision`] counts the unnamed writes alone. See
/// `docs/clipboard.md`.
///
/// # Examples
///
/// ```
/// use kvim_core::LineEnding;
/// use kvim_editor::{RegisterValue, Registers};
///
/// let mut registers = Registers::default();
/// assert!(registers.unnamed().is_none());
///
/// registers.set_unnamed(RegisterValue::characterwise("alpha"));
/// assert_eq!(registers.unnamed().map(RegisterValue::text), Some("alpha"));
/// assert_eq!(registers.revision(), 1);
///
/// // A named write leaves the unnamed register and the revision unchanged.
/// registers.write(Some('a'), RegisterValue::characterwise("beta"), LineEnding::Lf);
/// assert_eq!(registers.value(Some('a')).map(RegisterValue::text), Some("beta"));
/// assert_eq!(registers.revision(), 1);
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Registers {
    unnamed: Option<RegisterValue>,
    named: BTreeMap<char, RegisterValue>,
    revision: u64,
}

impl Registers {
    /// Returns the unnamed register value.
    #[must_use]
    pub const fn unnamed(&self) -> Option<&RegisterValue> {
        self.unnamed.as_ref()
    }

    /// Writes the unnamed register value.
    pub fn set_unnamed(&mut self, value: RegisterValue) {
        self.unnamed = Some(value);
        self.revision = self.revision.wrapping_add(1);
    }

    /// Returns the value that one resolved register name holds.
    ///
    /// `None` and `"` both read the unnamed register. `_` is the black-hole
    /// register and holds nothing. An upper-case name reads its lower-case
    /// register, because the two names write one value.
    #[must_use]
    pub fn value(&self, register: Option<char>) -> Option<&RegisterValue> {
        match RegisterTarget::of(register) {
            RegisterTarget::Unnamed => self.unnamed(),
            RegisterTarget::Named(name) | RegisterTarget::Append(name) => self.named.get(&name),
            RegisterTarget::BlackHole => None,
        }
    }

    /// Writes the value that one resolved register name selects.
    ///
    /// A write to `_` discards the value, which is what the black-hole register
    /// means. An upper-case name appends to its lower-case register. Every other
    /// name replaces the stored value.
    ///
    /// The line ending belongs to the buffer that produced the value, and the
    /// append needs it, because a linewise value keeps its final line ending.
    pub fn write(&mut self, register: Option<char>, value: RegisterValue, line_ending: LineEnding) {
        match RegisterTarget::of(register) {
            RegisterTarget::Unnamed => self.set_unnamed(value),
            RegisterTarget::Named(name) => {
                self.named.insert(name, value);
            }
            RegisterTarget::Append(name) => {
                let joined = match self.named.get(&name) {
                    Some(stored) => stored.appended(&value, line_ending),
                    None => value,
                };
                self.named.insert(name, joined);
            }
            RegisterTarget::BlackHole => {}
        }
        debug_assert!(
            self.named.len() <= NAMED_REGISTERS_MAX,
            "the accepted names bound the map"
        );
    }

    /// Returns the number of writes that the unnamed register received.
    ///
    /// A named write never changes this count, so the system clipboard mirror
    /// sees the unnamed register alone.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

#[cfg(test)]
#[path = "register_tests.rs"]
mod tests;
