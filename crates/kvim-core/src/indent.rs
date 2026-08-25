//! The indent policy of the buffer.
//!
//! The policy measures the existing indent of a line, renders an indent of a
//! wanted width, and moves an indent by one shift step. The `editor` module
//! builds the transactions for the Visual `<` and `>` commands and for the
//! automatic indent from these three primitives. This module holds no indent
//! constant of its own: [`IndentSettings`] resolves the width of one indent
//! level against the language of the buffer, and the policy renders it.

use std::num::NonZeroU8;

use kvim_settings::IndentSettings;

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
/// use kvim_core::{IndentPolicy, ShiftDirection};
/// use kvim_settings::IndentSettings;
///
/// let policy = IndentPolicy::from_settings(&IndentSettings::default());
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
    /// Reads the policy of a buffer that no language adapter serves.
    ///
    /// The default policy uses four-space soft tabs, and the shift width follows
    /// the tab width.
    #[must_use]
    pub fn from_settings(settings: &IndentSettings) -> Self {
        Self::for_language(settings, None)
    }

    /// Reads the policy of a buffer of one language.
    ///
    /// `language_width` is the number of cells that one indent level takes in
    /// the language of the buffer, or `None` for a buffer that no adapter
    /// serves. The resolved width becomes the shift width, so the automatic
    /// indent and the `<` and `>` commands step by the same number of cells,
    /// as they do in Vim. [`IndentSettings::indent_columns`] owns the
    /// resolution order.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::num::NonZeroU8;
    ///
    /// use kvim_core::IndentPolicy;
    /// use kvim_settings::IndentSettings;
    ///
    /// let cells = NonZeroU8::new(2).expect("the literal 2 is not zero");
    /// let policy = IndentPolicy::for_language(&IndentSettings::default(), Some(cells));
    /// assert_eq!(policy.shift_width(), cells);
    /// assert_eq!(policy.tab_width().get(), 4);
    /// ```
    #[must_use]
    pub fn for_language(settings: &IndentSettings, language_width: Option<NonZeroU8>) -> Self {
        Self {
            expand_tab: settings.expand_tab,
            tab_width: settings.tab_width,
            shift_width: settings.indent_columns(language_width),
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
    /// use kvim_core::IndentPolicy;
    /// use kvim_settings::IndentSettings;
    ///
    /// let mut settings = IndentSettings::default();
    /// settings.expand_tab = false;
    /// let policy = IndentPolicy::from_settings(&settings);
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
