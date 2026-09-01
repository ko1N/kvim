//! Editor modes and the owner of keyboard input.

use std::fmt;

use kvim_keymap::{CommandOwner, Scope, TextFallback, UnboundInput};

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
    /// use kvim_input::Mode;
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
    /// use kvim_input::Mode;
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
    /// use kvim_input::Mode;
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
/// An editor mode owns one table, the file-tree sidebar owns one more, the
/// picker owns another, and a waiting operator owns the last one. Only one scope
/// is active, so one key sequence may appear in several scopes with different
/// commands.
///
/// ```
/// use kvim_input::{BindingScope, Mode};
///
/// assert!(BindingScope::Mode(Mode::Normal).accepts_count());
/// // `d2w` deletes two words, so an operator still reads a count.
/// assert!(BindingScope::OperatorPending.accepts_count());
/// // `5j` moves five rows in the file tree, so the sidebar reads a count too.
/// assert!(BindingScope::Sidebar.accepts_count());
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
    /// An operator waits for its target and owns the keys.
    ///
    /// Vim reads the keys after `d`, `c`, and `y` in its own mode. `i` and `a`
    /// start a text object there instead of Insert mode, so this table repeats
    /// the motions and adds the text objects. The resolver selects the scope
    /// from the operator command that it emitted itself.
    OperatorPending,
    /// One open line prompt owns the keys.
    ///
    /// Every prompt reads the same keys, so one table holds them. A printable
    /// key falls through to the prompt text.
    Prompt,
    /// One open confirmation dialog owns all input.
    ///
    /// The dialog facade resolves its named choices before the mapping
    /// registry. This scope publishes ownership and no free-text behavior.
    Confirmation,
    /// The register selection waits for the name of a register.
    ///
    /// The scope holds no binding. The next printable key names the register,
    /// and every other key cancels the selection. The scope states that cancel
    /// through [`BindingScope::unbound_input`], so a host that owns the
    /// resolver reads the same rule.
    RegisterSelection,
    /// The open review of one captured diff owns the keys.
    ///
    /// The review reads no text and edits nothing, so it holds its own small
    /// table of navigation and view keys. See `docs/diff-view.md`.
    Review,
}

/// The number of binding scopes.
///
/// The inherent constant and the [`Scope`] constant both read this value, so
/// the two counts cannot drift apart.
const SCOPE_COUNT: usize = Mode::COUNT + 7;

impl BindingScope {
    /// The number of scopes. The mapping registry holds one table for each.
    pub const COUNT: usize = SCOPE_COUNT;

    /// Every scope, in table order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Mode(Mode::Normal),
        Self::Mode(Mode::Insert),
        Self::Mode(Mode::Visual),
        Self::Mode(Mode::VisualLine),
        Self::Mode(Mode::VisualBlock),
        Self::Sidebar,
        Self::Picker,
        Self::OperatorPending,
        Self::Prompt,
        Self::Confirmation,
        Self::RegisterSelection,
        Self::Review,
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
            Self::OperatorPending => Mode::COUNT + 2,
            Self::Prompt => Mode::COUNT + 3,
            Self::Confirmation => Mode::COUNT + 4,
            Self::RegisterSelection => Mode::COUNT + 5,
            Self::Review => Mode::COUNT + 6,
        }
    }

    /// Reports whether the scope binds the `i` and `a` text objects.
    ///
    /// A waiting operator takes the object as its target, and a Visual mode
    /// takes it as its selection. The semantic reducer reads the answer to
    /// publish its text-object phase.
    ///
    /// ```
    /// use kvim_input::{BindingScope, Mode};
    ///
    /// assert!(BindingScope::OperatorPending.binds_text_objects());
    /// assert!(BindingScope::Mode(Mode::Visual).binds_text_objects());
    /// assert!(!BindingScope::Mode(Mode::Normal).binds_text_objects());
    /// ```
    #[inline]
    #[must_use]
    pub const fn binds_text_objects(self) -> bool {
        matches!(
            self,
            Self::OperatorPending | Self::Mode(Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
        )
    }

    /// Returns the owner that takes printable input as literal text.
    ///
    /// Insert mode, a prompt, and the register selection each read text. The
    /// editor owns every one of them, so the fallback always names the focused
    /// surface. Every other scope leaves printable input unbound.
    ///
    /// ```
    /// use kvim_input::{BindingScope, Mode};
    /// use kvim_keymap::{CommandOwner, TextFallback};
    ///
    /// assert_eq!(
    ///     BindingScope::Mode(Mode::Insert).text_fallback(),
    ///     TextFallback::Typed(CommandOwner::Surface)
    /// );
    /// assert_eq!(
    ///     BindingScope::Mode(Mode::Normal).text_fallback(),
    ///     TextFallback::None
    /// );
    /// ```
    #[inline]
    #[must_use]
    pub const fn text_fallback(self) -> TextFallback {
        match self {
            Self::Mode(Mode::Insert) | Self::Prompt | Self::RegisterSelection => {
                TextFallback::Typed(CommandOwner::Surface)
            }
            Self::Mode(_)
            | Self::Sidebar
            | Self::Picker
            | Self::OperatorPending
            | Self::Confirmation
            | Self::Review => TextFallback::None,
        }
    }

    /// Returns what the scope does with input that nothing takes.
    ///
    /// The register selection waits for one register name that it does not
    /// bind, so any input that neither a binding nor the text fallback takes
    /// ends it. Every other scope keeps its state and leaves such input
    /// unbound.
    ///
    /// The rule belongs to the scope, not to the editor, so a host that owns
    /// the shared resolver reaches it through
    /// [`kvim_keymap::InputContextSnapshot`].
    ///
    /// ```
    /// use kvim_input::{BindingScope, Mode};
    /// use kvim_keymap::UnboundInput;
    ///
    /// assert_eq!(
    ///     BindingScope::RegisterSelection.unbound_input(),
    ///     UnboundInput::Cancels
    /// );
    /// assert_eq!(
    ///     BindingScope::Mode(Mode::Normal).unbound_input(),
    ///     UnboundInput::Ignored
    /// );
    /// ```
    #[inline]
    #[must_use]
    pub const fn unbound_input(self) -> UnboundInput {
        match self {
            Self::RegisterSelection => UnboundInput::Cancels,
            // A prompt binds its own cancel keys. A dialog resolves all input
            // before the registry. Unbound input leaves either surface open.
            Self::Mode(_)
            | Self::Sidebar
            | Self::Picker
            | Self::OperatorPending
            | Self::Prompt
            | Self::Confirmation
            | Self::Review => UnboundInput::Ignored,
        }
    }

    /// Reports whether the scope accepts a decimal count before a sequence.
    #[inline]
    pub const fn accepts_count(self) -> bool {
        match self {
            Self::Mode(mode) => mode.accepts_count(),
            // `d2w` deletes two words, so the count between the operator and
            // its target belongs to this scope.
            Self::OperatorPending => true,
            // The sidebar moves with the buffer navigation keys, so `5j` and
            // `12G` name a row count and a row there as well.
            Self::Sidebar => true,
            // The review walks hunks and files, and a count before a walk names
            // how many to pass, exactly as it does for a motion.
            Self::Review => true,
            // The picker and prompt read text. The confirmation dialog resolves
            // choices before this registry. None of these scopes accepts a count.
            Self::Picker | Self::Prompt | Self::Confirmation | Self::RegisterSelection => false,
        }
    }

    /// Returns the short name of the scope.
    #[inline]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mode(mode) => mode.label(),
            Self::Sidebar => "File Tree",
            Self::Picker => "Picker",
            Self::OperatorPending => "Operator Pending",
            Self::Prompt => "Prompt",
            Self::Confirmation => "Confirmation",
            Self::RegisterSelection => "Register Selection",
            Self::Review => "Review",
        }
    }

    /// Returns the input context in which this scope owns input.
    ///
    /// An operator waits inside Normal mode, and the resolver selects the
    /// operator-pending table from its own state, never from the context. A
    /// prompt, a confirmation, and a register selection all open over another
    /// scope in the same way. Each of these four scopes therefore answers with
    /// Normal mode, and none of them is ever the return scope of a prompt.
    #[inline]
    pub const fn context(self) -> InputContext {
        match self {
            Self::Mode(mode) => InputContext::Mode(mode),
            Self::Sidebar => InputContext::Sidebar,
            Self::Picker => InputContext::Picker,
            Self::Review => InputContext::Review,
            Self::OperatorPending | Self::Prompt | Self::Confirmation | Self::RegisterSelection => {
                InputContext::NORMAL
            }
        }
    }
}

impl Scope for BindingScope {
    /// The mapping registry holds one table for each scope.
    const COUNT: usize = SCOPE_COUNT;
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
    /// Create one entry inside the destination directory.
    ///
    /// The line reads one path, not only one name. A separator at the end of
    /// the path creates a directory, and the names before the last one create
    /// the directories that the workspace does not hold yet. See
    /// `docs/files.md`.
    AddFile,
    /// Create one directory inside the destination directory.
    ///
    /// [`TreePrompt::AddFile`] reads the same path and creates a directory
    /// too, so this prompt is redundant and it is a candidate for removal. It
    /// stays because removing a published prompt breaks an embedding host.
    AddDirectory,
    /// Give the selected entry another name.
    Rename,
    /// Mark the rows whose name holds the query.
    Search,
}

impl TreePrompt {
    /// Returns the text that the prompt line shows before the input.
    #[inline]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::AddFile => "new file: ",
            Self::AddDirectory => "new directory: ",
            Self::Rename => "rename: ",
            Self::Search => "search: ",
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
    /// Reports whether `Backspace` cancels this prompt when its line is empty.
    ///
    /// The command line, searches, picker, and file-tree add and search prompts
    /// use Vim-style cancellation. Rename stays open, so a user can clear its
    /// seed before entering a replacement name.
    ///
    /// ```
    /// use kvim_input::{PromptKind, TreePrompt};
    ///
    /// assert!(PromptKind::CommandLine.cancels_on_empty_backspace());
    /// assert!(!PromptKind::Tree(TreePrompt::Rename).cancels_on_empty_backspace());
    /// ```
    #[inline]
    #[must_use]
    pub const fn cancels_on_empty_backspace(self) -> bool {
        !matches!(self, Self::Tree(TreePrompt::Rename))
    }

    /// Returns the text that the prompt line shows before the input.
    ///
    /// ```
    /// use kvim_input::{PromptKind, TreePrompt};
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
/// One editor mode, the file-tree sidebar, one line prompt, or one confirmation
/// owns input, never two of them. The prompt variant and the confirmation
/// variant hold the scope that regains input when they close, so the editor
/// cannot lose that scope while either one is open.
///
/// ```
/// use kvim_input::{BindingScope, InputContext, Mode, PromptKind};
///
/// let normal = InputContext::NORMAL;
/// let prompt = normal.open_prompt(PromptKind::CommandLine);
/// assert_eq!(prompt.prompt(), Some(PromptKind::CommandLine));
/// assert_eq!(prompt.scope(), BindingScope::Mode(Mode::Normal));
/// assert_eq!(prompt.close_prompt(), normal);
///
/// let confirmation = normal.open_confirmation();
/// assert_eq!(confirmation.prompt(), None);
/// assert_eq!(confirmation.close_prompt(), normal);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InputContext {
    /// One editor mode owns input.
    Mode(Mode),
    /// The file-tree sidebar owns input.
    Sidebar,
    /// One open picker owns input.
    Picker,
    /// The open review of one captured diff owns input.
    Review,
    /// One line prompt owns input.
    Prompt {
        /// The prompt that reads the line.
        kind: PromptKind,
        /// The scope that regains input when the prompt closes.
        return_to: BindingScope,
    },
    /// One open confirmation dialog owns input.
    ///
    /// The dialog resolves its choices before the mapping registry. A question
    /// can open over a prompt without changing that prompt. The variant holds
    /// no prompt kind. See `docs/input-actions.md`.
    Confirmation {
        /// The scope that regains input when the confirmation closes.
        return_to: BindingScope,
    },
}

impl InputContext {
    /// Normal mode owns input.
    pub const NORMAL: Self = Self::Mode(Mode::Normal);

    /// Returns the scope that owns input, or that regains it when the prompt or
    /// the confirmation closes.
    #[inline]
    pub const fn scope(self) -> BindingScope {
        match self {
            Self::Mode(mode) => BindingScope::Mode(mode),
            Self::Sidebar => BindingScope::Sidebar,
            Self::Picker => BindingScope::Picker,
            Self::Review => BindingScope::Review,
            Self::Prompt { return_to, .. } | Self::Confirmation { return_to } => return_to,
        }
    }

    /// Returns the scope that owns the keys right now.
    ///
    /// A prompt owns one binding table. A confirmation dialog resolves input
    /// before the registry. This answer differs from [`InputContext::scope`],
    /// which names the scope that regains input when either surface closes.
    ///
    /// ```
    /// use kvim_input::{BindingScope, InputContext, PromptKind};
    ///
    /// let prompt = InputContext::NORMAL.open_prompt(PromptKind::CommandLine);
    /// assert_eq!(prompt.owning_scope(), BindingScope::Prompt);
    /// assert_eq!(prompt.scope(), BindingScope::Mode(kvim_input::Mode::Normal));
    /// ```
    #[inline]
    pub const fn owning_scope(self) -> BindingScope {
        match self {
            Self::Mode(mode) => BindingScope::Mode(mode),
            Self::Sidebar => BindingScope::Sidebar,
            Self::Picker => BindingScope::Picker,
            Self::Review => BindingScope::Review,
            Self::Prompt { .. } => BindingScope::Prompt,
            Self::Confirmation { .. } => BindingScope::Confirmation,
        }
    }

    /// Returns the prompt that owns input.
    ///
    /// A confirmation dialog is not a prompt, so it reports no prompt kind.
    #[inline]
    pub const fn prompt(self) -> Option<PromptKind> {
        match self {
            Self::Mode(_)
            | Self::Sidebar
            | Self::Picker
            | Self::Review
            | Self::Confirmation { .. } => None,
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

    /// Opens a confirmation over the current context.
    ///
    /// The return scope stays the scope that owned input before the
    /// confirmation, so the answer returns the keys to that scope.
    #[inline]
    pub const fn open_confirmation(self) -> Self {
        Self::Confirmation {
            return_to: self.scope(),
        }
    }

    /// Closes an open prompt or an open confirmation and restores the scope
    /// below it.
    ///
    /// The function returns a context without either one unchanged.
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
#[path = "mode_tests.rs"]
mod tests;
