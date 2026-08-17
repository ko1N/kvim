//! The unnamed register and the shape that a paste follows.
//!
//! The `editor` module owns every register value. The `clipboard` module mirrors
//! the unnamed register into the system clipboard. A clipboard failure never
//! removes the value that this module holds. See `docs/clipboard.md`.

use kvim_core::LineEnding;

/// The largest text that one register holds, in bytes.
///
/// A yank reads one buffer, and the file settings bound one buffer at 4 MiB, so
/// a yank cannot reach this bound. The bound protects the register against an
/// external value that grew past the buffer bound.
pub const REGISTER_BYTES_MAX: usize = 4 * 1024 * 1024;

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
}

/// The registers that one editor session holds.
///
/// The first release keeps the unnamed register only. The revision counts every
/// write, so the composition root knows when the system clipboard needs the new
/// value.
///
/// # Examples
///
/// ```
/// use kvim_editor::{RegisterValue, Registers};
///
/// let mut registers = Registers::default();
/// assert!(registers.unnamed().is_none());
///
/// registers.set_unnamed(RegisterValue::characterwise("alpha"));
/// assert_eq!(registers.unnamed().map(RegisterValue::text), Some("alpha"));
/// assert_eq!(registers.revision(), 1);
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Registers {
    unnamed: Option<RegisterValue>,
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

    /// Returns the number of writes that the unnamed register received.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}
