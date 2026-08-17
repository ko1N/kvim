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

/// The registry table that owns one key sequence.
///
/// An editor mode owns one table, the file-tree sidebar owns one more, and the
/// picker owns the last one. Only one scope is active, so one key sequence may
/// appear in several scopes with different commands.
///
/// ```
/// use kvim::input::{BindingScope, Mode};
///
/// assert!(BindingScope::Mode(Mode::Normal).accepts_count());
/// // The sidebar reads no count, because its keys act on one selected entry.
/// assert!(!BindingScope::Sidebar.accepts_count());
/// // The picker reads a query, so a digit belongs to that query.
/// assert!(!BindingScope::Picker.accepts_count());
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BindingScope {
    /// One editor mode owns the keys.
    Mode(Mode),
    /// The file-tree sidebar owns the keys.
    Sidebar,
    /// The picker owns the keys that its query does not hold.
    Picker,
}

impl BindingScope {
    /// The number of scopes. The mapping registry holds one table for each.
    pub const COUNT: usize = Mode::COUNT + 2;

    /// Every scope, in table order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Mode(Mode::Normal),
        Self::Mode(Mode::Insert),
        Self::Mode(Mode::Visual),
        Self::Mode(Mode::VisualLine),
        Self::Mode(Mode::VisualBlock),
        Self::Sidebar,
        Self::Picker,
    ];

    /// Returns the registry table index of the scope.
    ///
    /// The mode indexes fill the first [`Mode::COUNT`] tables, so the value
    /// stays inside [`BindingScope::COUNT`] by construction.
    #[inline]
    pub const fn index(self) -> usize {
        match self {
            Self::Mode(mode) => mode.index(),
            Self::Sidebar => Mode::COUNT,
            Self::Picker => Mode::COUNT + 1,
        }
    }

    /// Reports whether the scope accepts a decimal count before a sequence.
    #[inline]
    pub const fn accepts_count(self) -> bool {
        match self {
            Self::Mode(mode) => mode.accepts_count(),
            Self::Sidebar | Self::Picker => false,
        }
    }

    /// Returns the short name of the scope.
    #[inline]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mode(mode) => mode.label(),
            Self::Sidebar => "File Tree",
            Self::Picker => "Picker",
        }
    }

    /// Returns the input context in which this scope owns input.
    #[inline]
    pub const fn context(self) -> InputContext {
        match self {
            Self::Mode(mode) => InputContext::Mode(mode),
            Self::Sidebar => InputContext::Sidebar,
            Self::Picker => InputContext::Picker,
        }
    }
}

impl From<Mode> for BindingScope {
    /// Returns the scope of one editor mode.
    #[inline]
    fn from(mode: Mode) -> Self {
        Self::Mode(mode)
    }
}

impl fmt::Display for BindingScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// The file-tree operation that one prompt line names.
///
/// Each operation reads one line of text. The tree uses the prompt of the
/// message line, so it opens no second input mechanism. See `docs/files.md`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TreePrompt {
    /// Create one file inside the destination directory.
    AddFile,
    /// Create one directory inside the destination directory.
    AddDirectory,
    /// Give the selected entry another name.
    Rename,
    /// Narrow the visible rows to the names that hold the query.
    Filter,
}

impl TreePrompt {
    /// Returns the text that the prompt line shows before the input.
    #[inline]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::AddFile => "new file: ",
            Self::AddDirectory => "new directory: ",
            Self::Rename => "rename: ",
            Self::Filter => "filter: ",
        }
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
    /// One file-tree operation that needs a name or a query.
    Tree(TreePrompt),
    /// The query of one open picker.
    Picker,
}

impl PromptKind {
    /// Returns the text that the prompt line shows before the input.
    ///
    /// ```
    /// use kvim::input::{PromptKind, TreePrompt};
    ///
    /// assert_eq!(PromptKind::CommandLine.prefix(), ":");
    /// assert_eq!(PromptKind::Tree(TreePrompt::Rename).prefix(), "rename: ");
    /// ```
    #[inline]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::CommandLine => ":",
            Self::Search => "/",
            Self::Tree(prompt) => prompt.prefix(),
            Self::Picker => "> ",
        }
    }
}

/// The owner of keyboard input.
///
/// One editor mode, the file-tree sidebar, or one line prompt owns input, never
/// two of them. The prompt variant holds the scope that regains input when the
/// prompt closes, so the editor cannot lose that scope while a prompt is open.
///
/// ```
/// use kvim::input::{BindingScope, InputContext, Mode, PromptKind};
///
/// let normal = InputContext::NORMAL;
/// let prompt = normal.open_prompt(PromptKind::CommandLine);
/// assert_eq!(prompt.prompt(), Some(PromptKind::CommandLine));
/// assert_eq!(prompt.scope(), BindingScope::Mode(Mode::Normal));
/// assert_eq!(prompt.close_prompt(), normal);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InputContext {
    /// One editor mode owns input.
    Mode(Mode),
    /// The file-tree sidebar owns input.
    Sidebar,
    /// One open picker owns input.
    Picker,
    /// One line prompt owns input.
    Prompt {
        /// The prompt that reads the line.
        kind: PromptKind,
        /// The scope that regains input when the prompt closes.
        return_to: BindingScope,
    },
}

impl InputContext {
    /// Normal mode owns input.
    pub const NORMAL: Self = Self::Mode(Mode::Normal);

    /// Returns the scope that owns input, or that regains it when the prompt
    /// closes.
    #[inline]
    pub const fn scope(self) -> BindingScope {
        match self {
            Self::Mode(mode) => BindingScope::Mode(mode),
            Self::Sidebar => BindingScope::Sidebar,
            Self::Picker => BindingScope::Picker,
            Self::Prompt { return_to, .. } => return_to,
        }
    }

    /// Returns the prompt that owns input.
    #[inline]
    pub const fn prompt(self) -> Option<PromptKind> {
        match self {
            Self::Mode(_) | Self::Sidebar | Self::Picker => None,
            Self::Prompt { kind, .. } => Some(kind),
        }
    }

    /// Opens a prompt over the current context.
    ///
    /// The return scope stays the scope that owned input before the first
    /// prompt, so one prompt that replaces another still restores it.
    #[inline]
    pub const fn open_prompt(self, kind: PromptKind) -> Self {
        Self::Prompt {
            kind,
            return_to: self.scope(),
        }
    }

    /// Closes an open prompt and restores the scope below it.
    ///
    /// The function returns a context without a prompt unchanged.
    #[inline]
    pub const fn close_prompt(self) -> Self {
        self.scope().context()
    }
}

impl Default for InputContext {
    fn default() -> Self {
        Self::NORMAL
    }
}

#[cfg(test)]
mod tests {
    use super::{BindingScope, InputContext, Mode, PromptKind, TreePrompt};

    #[test]
    fn scope_indexes_are_unique_and_bounded() {
        let mut seen = [false; BindingScope::COUNT];
        for scope in BindingScope::ALL {
            let index = scope.index();
            assert!(
                index < BindingScope::COUNT,
                "{scope} indexes outside the table"
            );
            assert!(!seen[index], "{scope} repeats a table index");
            seen[index] = true;
        }
    }

    #[test]
    fn a_second_prompt_keeps_the_original_return_scope() {
        let visual = InputContext::Mode(Mode::Visual);
        let search = visual.open_prompt(PromptKind::Search);
        let command = search.open_prompt(PromptKind::CommandLine);
        assert_eq!(command.scope(), BindingScope::Mode(Mode::Visual));
        assert_eq!(command.close_prompt(), visual);
    }

    #[test]
    fn a_tree_prompt_returns_input_to_the_sidebar() {
        let sidebar = InputContext::Sidebar;
        let prompt = sidebar.open_prompt(PromptKind::Tree(TreePrompt::Rename));
        assert_eq!(prompt.scope(), BindingScope::Sidebar);
        assert_eq!(prompt.close_prompt(), sidebar);
    }
}
