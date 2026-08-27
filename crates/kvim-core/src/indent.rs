//! The indent policy of the buffer.
//!
//! The policy measures the existing indent of a line, renders an indent of a
//! wanted width, and moves an indent by one shift step. The `editor` module
//! builds the transactions for the Visual `<` and `>` commands and for the
//! automatic indent from these three primitives. This module holds no indent
//! constant of its own: the composition layer resolves the widths and gives the
//! policy only the three values that text operations need.

use std::num::NonZeroU8;

/// The largest indent width that [`IndentPolicy::render`] produces, in cells.
///
/// The bound keeps a wrong measurement or a damaged line from allocating an
/// unbounded indent.
pub const INDENT_COLUMNS_MAX: usize = 256;

/// The direction of one shift step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShiftDirection {
    /// Remove one shift width.
    Left,
    /// Add one shift width.
    Right,
}

/// The measured leading whitespace of one line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineIndent {
    /// The number of leading whitespace characters.
    ///
    /// The value is the length of the range that a shift transaction replaces.
    pub char_len: usize,
    /// The width of the leading whitespace, in cells, with expanded tabs.
    pub columns: usize,
}

/// The tab and shift policy of one buffer.
///
/// # Examples
///
/// ```
/// use std::num::NonZeroU8;
///
/// use kvim_core::{IndentPolicy, ShiftDirection};
///
/// let four = NonZeroU8::new(4).expect("four is not zero");
/// let policy = IndentPolicy::new(true, four, four);
/// let indent = policy.measure("    let value = 1;");
/// assert_eq!(indent.char_len, 4);
/// assert_eq!(indent.columns, 4);
///
/// let shifted = policy.shift_columns(indent.columns, ShiftDirection::Right);
/// assert_eq!(policy.render(shifted), "        ");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndentPolicy {
    expand_tab: bool,
    tab_width: NonZeroU8,
    shift_width: NonZeroU8,
}

impl IndentPolicy {
    /// Creates a policy from resolved composition values.
    ///
    /// `tab_width` measures tab stops. `shift_width` controls automatic indent
    /// and the `<` and `>` commands. The composition layer resolves language
    /// and user settings before it creates this value.
    #[must_use]
    pub const fn new(expand_tab: bool, tab_width: NonZeroU8, shift_width: NonZeroU8) -> Self {
        Self {
            expand_tab,
            tab_width,
            shift_width,
        }
    }

    /// Returns the number of cells that one tab character occupies.
    #[must_use]
    pub const fn tab_width(self) -> NonZeroU8 {
        self.tab_width
    }

    /// Returns the number of cells that one shift step moves.
    #[must_use]
    pub const fn shift_width(self) -> NonZeroU8 {
        self.shift_width
    }

    /// Measures the leading whitespace of one line.
    ///
    /// A tab advances to the next multiple of the tab width, like the terminal.
    #[must_use]
    pub fn measure(self, line: &str) -> LineIndent {
        let tab_width = usize::from(self.tab_width.get());
        let mut indent = LineIndent {
            char_len: 0,
            columns: 0,
        };
        for character in line.chars() {
            match character {
                ' ' => indent.columns += 1,
                '\t' => indent.columns += tab_width - (indent.columns % tab_width),
                _ => break,
            }
            indent.char_len += 1;
        }
        indent
    }

    /// Renders indent text of the wanted width.
    ///
    /// The text uses spaces while the settings expand the tab key. The width
    /// stays at or below [`INDENT_COLUMNS_MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::num::NonZeroU8;
    ///
    /// use kvim_core::IndentPolicy;
    ///
    /// let four = NonZeroU8::new(4).expect("four is not zero");
    /// let policy = IndentPolicy::new(false, four, four);
    /// assert_eq!(policy.render(6), "\t  ");
    /// ```
    #[must_use]
    pub fn render(self, columns: usize) -> String {
        let columns = columns.min(INDENT_COLUMNS_MAX);
        if self.expand_tab {
            return " ".repeat(columns);
        }

        let tab_width = usize::from(self.tab_width.get());
        let tabs = columns / tab_width;
        let spaces = columns % tab_width;
        let mut text = String::with_capacity(tabs + spaces);
        for _ in 0..tabs {
            text.push('\t');
        }
        for _ in 0..spaces {
            text.push(' ');
        }
        text
    }

    /// Moves an indent width by one shift step.
    ///
    /// A left shift below zero stops at zero, like Vim.
    #[must_use]
    pub fn shift_columns(self, columns: usize, direction: ShiftDirection) -> usize {
        let step = usize::from(self.shift_width.get());
        match direction {
            ShiftDirection::Left => columns.saturating_sub(step),
            ShiftDirection::Right => columns.saturating_add(step).min(INDENT_COLUMNS_MAX),
        }
    }
}

#[cfg(test)]
#[path = "indent_tests.rs"]
mod tests;
