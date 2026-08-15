//! Editor modes and the owner of keyboard input.

use std::fmt;

/// One editor mode.
///
/// The mode is one typed value. A mode change resets pending input. See
/// `docs/input-actions.md`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Mode {
    /// Motions, operators, and commands act on the buffer.
    #[default]
    Normal,
    /// Printable keys insert text through edit transactions.
    Insert,
    /// A characterwise selection follows the cursor.
    Visual,
    /// A linewise selection follows the cursor.
    VisualLine,
    /// A rectangular selection follows the cursor.
    VisualBlock,
}

impl Mode {
    /// Every mode, in declaration order.
    pub const ALL: [Self; 5] = [
        Self::Normal,
        Self::Insert,
        Self::Visual,
        Self::VisualLine,
        Self::VisualBlock,
    ];

    /// The number of modes. The mapping registry holds one table for each mode.
    pub const COUNT: usize = Self::ALL.len();

    /// Returns the registry table index of the mode.
    ///
    /// The index is the declaration order of [`Mode::ALL`], so it stays inside
    /// [`Mode::COUNT`] by construction.
    ///
    /// ```
    /// use kvim::input::Mode;
    ///
    /// assert!(Mode::ALL.iter().all(|mode| mode.index() < Mode::COUNT));
    /// ```
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Reports whether the mode accepts a decimal count before a sequence.
    ///
    /// A count prefix belongs to Normal mode and the three Visual modes. Insert
    /// mode holds no count, because a digit is buffer text there. See
    /// `docs/input-actions.md`.
    ///
    /// ```
    /// use kvim::input::Mode;
    ///
    /// assert!(Mode::Normal.accepts_count());
    /// assert!(Mode::VisualBlock.accepts_count());
    /// assert!(!Mode::Insert.accepts_count());
    /// ```
    #[inline]
    pub const fn accepts_count(self) -> bool {
        match self {
            Self::Normal | Self::Visual | Self::VisualLine | Self::VisualBlock => true,
            Self::Insert => false,
        }
    }

    /// Returns the short name of the mode.
    ///
    /// ```
    /// use kvim::input::Mode;
    ///
    /// assert_eq!(Mode::VisualBlock.label(), "Visual Block");
    /// ```
    #[inline]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Insert => "Insert",
            Self::Visual => "Visual",
            Self::VisualLine => "Visual Line",
            Self::VisualBlock => "Visual Block",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// One line prompt that reads a query instead of a key sequence.
///
/// A prompt is not a mode. See `docs/input-actions.md`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PromptKind {
    /// The command line that `:` opens.
    CommandLine,
    /// The search prompt that `/` opens.
    Search,
}

/// The owner of keyboard input.
///
/// One editor mode or one line prompt owns input, never both. The variant holds
/// the mode that regains input when the prompt closes, so the editor cannot lose
/// the mode while a prompt is open.
///
/// ```
/// use kvim::input::{InputContext, Mode, PromptKind};
///
/// let normal = InputContext::NORMAL;
/// let prompt = normal.open_prompt(PromptKind::CommandLine);
/// assert_eq!(prompt.prompt(), Some(PromptKind::CommandLine));
/// assert_eq!(prompt.mode(), Mode::Normal);
/// assert_eq!(prompt.close_prompt(), normal);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InputContext {
    /// One editor mode owns input.
    Mode(Mode),
    /// One line prompt owns input.
    Prompt {
        /// The prompt that reads the line.
        kind: PromptKind,
        /// The mode that regains input when the prompt closes.
        return_mode: Mode,
    },
}

impl InputContext {
    /// Normal mode owns input.
    pub const NORMAL: Self = Self::Mode(Mode::Normal);

    /// Returns the mode that owns input, or that regains it when the prompt
    /// closes.
    #[inline]
    pub const fn mode(self) -> Mode {
        match self {
            Self::Mode(mode) => mode,
            Self::Prompt { return_mode, .. } => return_mode,
        }
    }

    /// Returns the prompt that owns input.
    #[inline]
    pub const fn prompt(self) -> Option<PromptKind> {
        match self {
            Self::Mode(_) => None,
            Self::Prompt { kind, .. } => Some(kind),
        }
    }

    /// Opens a prompt over the current context.
    ///
    /// The return mode stays the mode that owned input before the first prompt,
    /// so one prompt that replaces another still restores the editor mode.
    #[inline]
    pub const fn open_prompt(self, kind: PromptKind) -> Self {
        Self::Prompt {
            kind,
            return_mode: self.mode(),
        }
    }

    /// Closes an open prompt and restores the mode.
    ///
    /// The function returns a mode context unchanged.
    #[inline]
    pub const fn close_prompt(self) -> Self {
        Self::Mode(self.mode())
    }
}

impl Default for InputContext {
    fn default() -> Self {
        Self::NORMAL
    }
}

#[cfg(test)]
mod tests {
    use super::{InputContext, Mode, PromptKind};

    #[test]
    fn mode_indexes_are_unique_and_bounded() {
        let mut seen = [false; Mode::COUNT];
        for mode in Mode::ALL {
            let index = mode.index();
            assert!(index < Mode::COUNT, "{mode} indexes outside the table");
            assert!(!seen[index], "{mode} repeats a table index");
            seen[index] = true;
        }
    }

    #[test]
    fn a_second_prompt_keeps_the_original_return_mode() {
        let visual = InputContext::Mode(Mode::Visual);
        let search = visual.open_prompt(PromptKind::Search);
        let command = search.open_prompt(PromptKind::CommandLine);
        assert_eq!(command.mode(), Mode::Visual);
        assert_eq!(command.close_prompt(), visual);
    }
}
