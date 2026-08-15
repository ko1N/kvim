//! The indent policy of the buffer.
//!
//! The policy measures the existing indent of a line, renders an indent of a
//! wanted width, and moves an indent by one shift step. The `editor` module
//! builds the transactions for the Visual `<` and `>` commands and for the
//! automatic indent from these three primitives. Every indent value comes from
//! [`IndentSettings`], so no other module holds an indent constant.

use std::num::NonZeroU8;

use crate::settings::IndentSettings;

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
/// use kvim::core::{IndentPolicy, ShiftDirection};
/// use kvim::settings::IndentSettings;
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
    /// Reads the policy from the editor settings.
    ///
    /// The default policy uses four-space soft tabs, and the shift width follows
    /// the tab width.
    #[must_use]
    pub fn from_settings(settings: &IndentSettings) -> Self {
        Self {
            expand_tab: settings.expand_tab,
            tab_width: settings.tab_width,
            shift_width: settings.shift_width.resolve(settings.tab_width),
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
    /// use kvim::core::IndentPolicy;
    /// use kvim::settings::IndentSettings;
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
mod tests {
    use super::{INDENT_COLUMNS_MAX, IndentPolicy, ShiftDirection};
    use crate::settings::{IndentSettings, ShiftWidth};
    use std::num::NonZeroU8;

    fn cells(value: u8) -> NonZeroU8 {
        NonZeroU8::new(value).expect("the test value is not zero")
    }

    fn default_policy() -> IndentPolicy {
        IndentPolicy::from_settings(&IndentSettings::default())
    }

    #[test]
    fn the_default_policy_uses_four_space_soft_tabs() {
        let policy = default_policy();
        assert_eq!(policy.tab_width(), cells(4));
        assert_eq!(policy.shift_width(), cells(4));
        assert_eq!(policy.render(4), "    ");
    }

    #[test]
    fn the_shift_width_follows_an_explicit_setting() {
        let settings = IndentSettings {
            shift_width: ShiftWidth::Cells(cells(2)),
            ..IndentSettings::default()
        };
        let policy = IndentPolicy::from_settings(&settings);
        assert_eq!(policy.shift_width(), cells(2));
        assert_eq!(policy.shift_columns(4, ShiftDirection::Right), 6);
    }

    #[test]
    fn a_tab_advances_to_the_next_tab_stop() {
        let policy = default_policy();
        assert_eq!(policy.measure("\tvalue").columns, 4);
        assert_eq!(policy.measure(" \tvalue").columns, 4);
        assert_eq!(policy.measure(" \tvalue").char_len, 2);
        assert_eq!(policy.measure("     \tvalue").columns, 8);
    }

    #[test]
    fn measurement_stops_at_the_first_other_character() {
        let policy = default_policy();
        let indent = policy.measure("  a  b");
        assert_eq!(indent.char_len, 2);
        assert_eq!(indent.columns, 2);
        assert_eq!(policy.measure("").char_len, 0);
    }

    #[test]
    fn a_left_shift_stops_at_zero() {
        let policy = default_policy();
        assert_eq!(policy.shift_columns(5, ShiftDirection::Left), 1);
        assert_eq!(policy.shift_columns(2, ShiftDirection::Left), 0);
        assert_eq!(policy.shift_columns(0, ShiftDirection::Left), 0);
    }

    #[test]
    fn rendering_and_shifting_stay_bounded() {
        let policy = default_policy();
        assert_eq!(policy.render(usize::MAX).len(), INDENT_COLUMNS_MAX);
        assert_eq!(
            policy.shift_columns(usize::MAX, ShiftDirection::Right),
            INDENT_COLUMNS_MAX
        );
    }

    #[test]
    fn a_hard_tab_policy_renders_tabs_and_spaces() {
        let settings = IndentSettings {
            expand_tab: false,
            ..IndentSettings::default()
        };
        let policy = IndentPolicy::from_settings(&settings);
        assert_eq!(policy.render(9), "\t\t ");
        assert_eq!(policy.measure("\t\t code").columns, 9);
    }
}
