//! The semantic commands that the editor consumes.
//!
//! A command describes intent, never a key. The `editor`, `workspace`, and `tui`
//! modules act on a command and never compare a raw key.

use std::fmt;

use kvim_keymap::CommandMetadata;

/// Declares every semantic command from one table.
///
/// The table is the single source of the variant, the stable identifier, and the
/// short label, so the three cannot drift apart. The which-key overlay and any
/// help output read the label from this table only.
macro_rules! semantic_commands {
    ($($variant:ident => ($id:literal, $label:literal),)+) => {
        /// One editor intent.
        ///
        /// Every command carries a stable identifier and a short label.
        ///
        /// ```
        /// use kvim_input::Command;
        ///
        /// assert_eq!(Command::MoveLeft.id(), "move-left");
        /// assert_eq!(Command::MoveLeft.label(), "Move left");
        /// ```
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[non_exhaustive]
        pub enum Command {
            $(#[doc = $label] $variant,)+
        }

        impl Command {
            /// Every command, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// Returns the stable identifier of the command.
            ///
            /// The identifier never changes, because a later configuration
            /// loader binds keys by this name.
            #[inline]
            pub const fn id(self) -> &'static str {
                match self {
                    $(Self::$variant => $id,)+
                }
            }

            /// Returns the short label of the command.
            #[inline]
            pub const fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label,)+
                }
            }
        }
    };
}

semantic_commands! {
    // Modes.
    InsertBeforeCursor => ("insert-before-cursor", "Insert before the cursor"),
    InsertAtFirstNonBlank => ("insert-at-first-non-blank", "Insert at the first non-blank character"),
    InsertAfterCursor => ("insert-after-cursor", "Insert after the cursor"),
    InsertAtLineEnd => ("insert-at-line-end", "Insert at the end of the line"),
    OpenLineBelow => ("open-line-below", "Open a line below and insert"),
    OpenLineAbove => ("open-line-above", "Open a line above and insert"),
    EnterVisual => ("enter-visual", "Enter Visual mode"),
    EnterVisualLine => ("enter-visual-line", "Enter Visual Line mode"),
    EnterVisualBlock => ("enter-visual-block", "Enter Visual Block mode"),
    OpenCommandLine => ("open-command-line", "Open the command line"),
    ReturnToNormal => ("return-to-normal", "Return to Normal mode"),

    // Insert-mode text entry. A printable key reaches the text fallback of the
    // scope, so only the keys that type no character carry a command.
    InsertLineBreak => ("insert-line-break", "Insert a line break"),
    DeleteCharacterBefore => ("delete-character-before", "Delete the character before the cursor"),
    InsertIndent => ("insert-indent", "Insert one indent step"),

    // Motions.
    MoveLeft => ("move-left", "Move left"),
    MoveDown => ("move-down", "Move down"),
    MoveUp => ("move-up", "Move up"),
    MoveRight => ("move-right", "Move right"),
    MoveNextWordStart => ("move-next-word-start", "Move to the next word start"),
    MovePreviousWordStart => ("move-previous-word-start", "Move to the previous word start"),
    MoveNextWordEnd => ("move-next-word-end", "Move to the next word end"),
    MoveFirstColumn => ("move-first-column", "Move to the first column"),
    MoveFirstNonBlank => ("move-first-non-blank", "Move to the first non-blank character"),
    MoveLastNonBlank => ("move-last-non-blank", "Move to the last non-blank character"),
    MoveLineEnd => ("move-line-end", "Move to the end of the line"),
    MoveMatchingBracket => ("move-matching-bracket", "Move to the matching bracket"),
    MoveFirstLine => ("move-first-line", "Move to the first line"),
    MoveLastLine => ("move-last-line", "Move to the last line, or to the count line"),
    MoveHalfPageDown => ("move-half-page-down", "Move down one half page"),
    MoveHalfPageUp => ("move-half-page-up", "Move up one half page"),
    MoveFullPageDown => ("move-full-page-down", "Move down one full page"),
    MoveFullPageUp => ("move-full-page-up", "Move up one full page"),
    CenterCursorLine => ("center-cursor-line", "Center the cursor line in the window"),
    AlignCursorLineTop => ("align-cursor-line-top", "Align the cursor line to the window top"),
    AlignCursorLineBottom => ("align-cursor-line-bottom", "Align the cursor line to the window bottom"),

    // Count digits. A digit is a surface command, so it reaches the semantic
    // reducer through the shared registry instead of a second key table. `0` is
    // the first-column motion until a count is already open, so it keeps
    // `MoveFirstColumn` and the reducer reads it as the zero digit.
    CountDigitOne => ("count-digit-one", "Append one to the count"),
    CountDigitTwo => ("count-digit-two", "Append two to the count"),
    CountDigitThree => ("count-digit-three", "Append three to the count"),
    CountDigitFour => ("count-digit-four", "Append four to the count"),
    CountDigitFive => ("count-digit-five", "Append five to the count"),
    CountDigitSix => ("count-digit-six", "Append six to the count"),
    CountDigitSeven => ("count-digit-seven", "Append seven to the count"),
    CountDigitEight => ("count-digit-eight", "Append eight to the count"),
    CountDigitNine => ("count-digit-nine", "Append nine to the count"),

    // The prompt line. Every prompt reads the same keys, so one scope holds
    // them and printable input falls through to the prompt text.
    PromptAccept => ("prompt-accept", "Run the prompt line"),
    PromptCancel => ("prompt-cancel", "Cancel the prompt line"),
    PromptDeleteBackward => ("prompt-delete-backward", "Remove the character before the prompt cursor"),
    PromptCompleteNext => ("prompt-complete-next", "Write the next completion candidate"),
    PromptCompletePrevious => ("prompt-complete-previous", "Write the previous completion candidate"),

    // Operators, registers, and repeat.
    SelectRegister => ("select-register", "Select the register of the next operation"),
    DeleteOverMotion => ("delete-over-motion", "Delete over a motion"),
    ChangeOverMotion => ("change-over-motion", "Change over a motion"),
    YankOverMotion => ("yank-over-motion", "Yank over a motion"),
    DeleteSelection => ("delete-selection", "Delete the selection"),
    ChangeSelection => ("change-selection", "Change the selection"),
    YankSelection => ("yank-selection", "Yank the selection"),
    BlockInsertBefore => ("block-insert-before", "Insert before every selected line"),
    BlockInsertAfter => ("block-insert-after", "Insert after every selected line"),
    DeleteLine => ("delete-line", "Delete the current line"),
    ChangeLine => ("change-line", "Change the current line"),
    YankLine => ("yank-line", "Yank the current line"),
    DeleteToLineEnd => ("delete-to-line-end", "Delete to the end of the line"),
    ChangeToLineEnd => ("change-to-line-end", "Change to the end of the line"),
    PasteAfter => ("paste-after", "Paste after the cursor"),
    PasteBefore => ("paste-before", "Paste before the cursor"),
    Undo => ("undo", "Undo one transaction"),
    Redo => ("redo", "Redo one transaction"),
    RepeatChange => ("repeat-change", "Repeat the last repeatable change"),

    // Text objects. The open and the close delimiter name the same object, so
    // `i(` and `i)` reach one command.
    SelectInnerWord => ("select-inner-word", "Select the word"),
    SelectAroundWord => ("select-around-word", "Select the word and its blanks"),
    SelectInnerLongWord => ("select-inner-long-word", "Select the non-blank run"),
    SelectAroundLongWord => ("select-around-long-word", "Select the non-blank run and its blanks"),
    SelectInnerParen => ("select-inner-paren", "Select inside the round brackets"),
    SelectAroundParen => ("select-around-paren", "Select the round brackets"),
    SelectInnerBracket => ("select-inner-bracket", "Select inside the square brackets"),
    SelectAroundBracket => ("select-around-bracket", "Select the square brackets"),
    SelectInnerBrace => ("select-inner-brace", "Select inside the curly brackets"),
    SelectAroundBrace => ("select-around-brace", "Select the curly brackets"),
    SelectInnerAngle => ("select-inner-angle", "Select inside the angle brackets"),
    SelectAroundAngle => ("select-around-angle", "Select the angle brackets"),
    SelectInnerDoubleQuote => ("select-inner-double-quote", "Select inside the double quotes"),
    SelectAroundDoubleQuote => ("select-around-double-quote", "Select the double quotes"),
    SelectInnerSingleQuote => ("select-inner-single-quote", "Select inside the single quotes"),
    SelectAroundSingleQuote => ("select-around-single-quote", "Select the single quotes"),
    SelectInnerBacktick => ("select-inner-backtick", "Select inside the backticks"),
    SelectAroundBacktick => ("select-around-backtick", "Select the backticks"),

    // Search.
    OpenSearchPrompt => ("open-search-prompt", "Open the search prompt"),
    SearchNext => ("search-next", "Move to the next match"),
    SearchPrevious => ("search-previous", "Move to the previous match"),
    EndSearch => ("end-search", "End the active search"),

    // Visual selection.
    MoveSelectionDown => ("move-selection-down", "Move the selection down one line"),
    MoveSelectionUp => ("move-selection-up", "Move the selection up one line"),
    ShiftSelectionLeft => ("shift-selection-left", "Shift the selection left one shift width"),
    ShiftSelectionRight => ("shift-selection-right", "Shift the selection right one shift width"),

    // Files and buffers.
    SaveBuffer => ("save-buffer", "Save the active buffer"),
    RevealInFileTree => ("reveal-in-file-tree", "Reveal the active file in the file tree"),
    OpenBufferPicker => ("open-buffer-picker", "Open the buffer picker"),
    UnloadBuffer => ("unload-buffer", "Unload the active buffer"),
    OpenFilePicker => ("open-file-picker", "Open the file search picker"),
    OpenRipgrepPicker => ("open-ripgrep-picker", "Open the ripgrep search picker"),

    // The file tree.
    TreeOpenEntry => ("tree-open-entry", "Open the selected entry"),
    TreeToggleEntry => ("tree-toggle-entry", "Expand or collapse the selected directory"),
    TreeExpandEntry => ("tree-expand-entry", "Expand the selected directory, or open the selected file"),
    TreeCollapseEntry => ("tree-collapse-entry", "Collapse the selected directory, or select the parent directory"),
    TreeSelectParent => ("tree-select-parent", "Select the parent directory"),
    TreeRefresh => ("tree-refresh", "Read the workspace directories again"),
    TreeAddFile => ("tree-add-file", "Add one file"),
    TreeAddDirectory => ("tree-add-directory", "Add one directory"),
    TreeDelete => ("tree-delete", "Delete the selected entry"),
    TreeRename => ("tree-rename", "Rename the selected entry"),
    TreeCopyEntry => ("tree-copy-entry", "Copy the selected entry"),
    TreeCutEntry => ("tree-cut-entry", "Cut the selected entry"),
    TreePasteEntries => ("tree-paste-entries", "Paste the held entries"),
    TreeToggleHidden => ("tree-toggle-hidden", "Show or hide the hidden entries"),
    TreeSearch => ("tree-search", "Search the visible entries"),

    // The pickers.
    PickerSelectNext => ("picker-select-next", "Select the next result"),
    PickerSelectPrevious => ("picker-select-previous", "Select the previous result"),

    // Windows.
    FocusWindowLeft => ("focus-window-left", "Focus the window to the left"),
    FocusWindowDown => ("focus-window-down", "Focus the window below"),
    FocusWindowUp => ("focus-window-up", "Focus the window above"),
    FocusWindowRight => ("focus-window-right", "Focus the window to the right"),
    ResizeWindowLeft => ("resize-window-left", "Resize the window to the left"),
    ResizeWindowDown => ("resize-window-down", "Resize the window downward"),
    ResizeWindowUp => ("resize-window-up", "Resize the window upward"),
    ResizeWindowRight => ("resize-window-right", "Resize the window to the right"),
    SplitAdaptive => ("split-adaptive", "Split the window with the adaptive rule"),
    SplitInverseAdaptive => ("split-inverse-adaptive", "Split the window with the inverse adaptive rule"),
    CloseWindow => ("close-window", "Close the focused window"),

    // Language services.
    ToggleComment => ("toggle-comment", "Toggle the comment"),
    GoToDefinition => ("go-to-definition", "Go to the definition"),
    ShowHover => ("show-hover", "Show hover information"),
    ShowDiagnosticFloat => ("show-diagnostic-float", "Show the diagnostic float"),
    NextDiagnostic => ("next-diagnostic", "Move to the next diagnostic"),
    PreviousDiagnostic => ("previous-diagnostic", "Move to the previous diagnostic"),
    ToggleFormatOnSave => ("toggle-format-on-save", "Toggle format-on-save for the active buffer"),
}

/// The group that one command belongs to.
///
/// The group is a property of the command, not of one view. The which-key
/// overlay reads it to pick the icon of a row, and it names no glyph and no
/// color of its own, because the interface layer owns every presentation
/// value. See `docs/input-actions.md`.
///
/// The group follows the section that declares the command. A section that no
/// named group covers falls to [`CommandGroup::Other`], which is also the group
/// of a key that reaches commands of several groups.
///
/// The enumeration is exhaustive on purpose. The interface layer holds one icon
/// for each group, and a new group must therefore fail to compile until it
/// carries one.
///
/// ```
/// use kvim_input::{Command, CommandGroup};
///
/// assert_eq!(Command::SearchNext.group(), CommandGroup::Search);
/// assert_eq!(Command::MoveLeft.group(), CommandGroup::Other);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CommandGroup {
    /// A command of the buffer search.
    Search,
    /// A command of the language services.
    Code,
    /// A command that acts on a window.
    Window,
    /// A command that acts on a file or a buffer.
    Buffer,
    /// A command that acts on the file tree.
    Tree,
    /// Every other command, such as a motion, a text change, or a mode switch.
    #[default]
    Other,
}

impl CommandGroup {
    /// Returns the group that covers both groups.
    ///
    /// One which-key row may stand for several commands. The row keeps the
    /// shared group while every command behind it agrees, and it falls to
    /// [`CommandGroup::Other`] as soon as two commands disagree.
    ///
    /// ```
    /// use kvim_input::CommandGroup;
    ///
    /// assert_eq!(
    ///     CommandGroup::Tree.merged(CommandGroup::Tree),
    ///     CommandGroup::Tree
    /// );
    /// assert_eq!(
    ///     CommandGroup::Tree.merged(CommandGroup::Window),
    ///     CommandGroup::Other
    /// );
    /// ```
    #[inline]
    #[must_use]
    pub const fn merged(self, other: Self) -> Self {
        if self as u8 == other as u8 {
            self
        } else {
            Self::Other
        }
    }
}

impl Command {
    /// Returns the group that the command belongs to.
    ///
    /// The match is exhaustive, so a new command cannot reach the overlay
    /// without a group decision.
    #[inline]
    #[must_use]
    pub const fn group(self) -> CommandGroup {
        match self {
            Self::OpenSearchPrompt | Self::SearchNext | Self::SearchPrevious | Self::EndSearch => {
                CommandGroup::Search
            }

            Self::ToggleComment
            | Self::GoToDefinition
            | Self::ShowHover
            | Self::ShowDiagnosticFloat
            | Self::NextDiagnostic
            | Self::PreviousDiagnostic
            | Self::ToggleFormatOnSave => CommandGroup::Code,

            Self::FocusWindowLeft
            | Self::FocusWindowDown
            | Self::FocusWindowUp
            | Self::FocusWindowRight
            | Self::ResizeWindowLeft
            | Self::ResizeWindowDown
            | Self::ResizeWindowUp
            | Self::ResizeWindowRight
            | Self::SplitAdaptive
            | Self::SplitInverseAdaptive
            | Self::CloseWindow => CommandGroup::Window,

            Self::SaveBuffer
            | Self::RevealInFileTree
            | Self::OpenBufferPicker
            | Self::UnloadBuffer
            | Self::OpenFilePicker
            | Self::OpenRipgrepPicker => CommandGroup::Buffer,

            Self::TreeOpenEntry
            | Self::TreeToggleEntry
            | Self::TreeExpandEntry
            | Self::TreeCollapseEntry
            | Self::TreeSelectParent
            | Self::TreeRefresh
            | Self::TreeAddFile
            | Self::TreeAddDirectory
            | Self::TreeDelete
            | Self::TreeRename
            | Self::TreeCopyEntry
            | Self::TreeCutEntry
            | Self::TreePasteEntries
            | Self::TreeToggleHidden
            | Self::TreeSearch => CommandGroup::Tree,

            Self::CountDigitOne
            | Self::CountDigitTwo
            | Self::CountDigitThree
            | Self::CountDigitFour
            | Self::CountDigitFive
            | Self::CountDigitSix
            | Self::CountDigitSeven
            | Self::CountDigitEight
            | Self::CountDigitNine
            | Self::PromptAccept
            | Self::PromptCancel
            | Self::PromptDeleteBackward
            | Self::PromptCompleteNext
            | Self::PromptCompletePrevious
            | Self::SelectRegister
            | Self::InsertLineBreak
            | Self::DeleteCharacterBefore
            | Self::InsertIndent
            | Self::InsertBeforeCursor
            | Self::InsertAtFirstNonBlank
            | Self::InsertAfterCursor
            | Self::InsertAtLineEnd
            | Self::OpenLineBelow
            | Self::OpenLineAbove
            | Self::EnterVisual
            | Self::EnterVisualLine
            | Self::EnterVisualBlock
            | Self::OpenCommandLine
            | Self::ReturnToNormal
            | Self::MoveLeft
            | Self::MoveDown
            | Self::MoveUp
            | Self::MoveRight
            | Self::MoveNextWordStart
            | Self::MovePreviousWordStart
            | Self::MoveNextWordEnd
            | Self::MoveFirstColumn
            | Self::MoveFirstNonBlank
            | Self::MoveLastNonBlank
            | Self::MoveLineEnd
            | Self::MoveMatchingBracket
            | Self::MoveFirstLine
            | Self::MoveLastLine
            | Self::MoveHalfPageDown
            | Self::MoveHalfPageUp
            | Self::MoveFullPageDown
            | Self::MoveFullPageUp
            | Self::CenterCursorLine
            | Self::AlignCursorLineTop
            | Self::AlignCursorLineBottom
            | Self::DeleteOverMotion
            | Self::ChangeOverMotion
            | Self::YankOverMotion
            | Self::DeleteSelection
            | Self::ChangeSelection
            | Self::YankSelection
            | Self::BlockInsertBefore
            | Self::BlockInsertAfter
            | Self::DeleteLine
            | Self::ChangeLine
            | Self::YankLine
            | Self::DeleteToLineEnd
            | Self::ChangeToLineEnd
            | Self::PasteAfter
            | Self::PasteBefore
            | Self::Undo
            | Self::Redo
            | Self::RepeatChange
            | Self::SelectInnerWord
            | Self::SelectAroundWord
            | Self::SelectInnerLongWord
            | Self::SelectAroundLongWord
            | Self::SelectInnerParen
            | Self::SelectAroundParen
            | Self::SelectInnerBracket
            | Self::SelectAroundBracket
            | Self::SelectInnerBrace
            | Self::SelectAroundBrace
            | Self::SelectInnerAngle
            | Self::SelectAroundAngle
            | Self::SelectInnerDoubleQuote
            | Self::SelectAroundDoubleQuote
            | Self::SelectInnerSingleQuote
            | Self::SelectAroundSingleQuote
            | Self::SelectInnerBacktick
            | Self::SelectAroundBacktick
            | Self::MoveSelectionDown
            | Self::MoveSelectionUp
            | Self::ShiftSelectionLeft
            | Self::ShiftSelectionRight
            | Self::PickerSelectNext
            | Self::PickerSelectPrevious => CommandGroup::Other,
        }
    }

    /// Returns the decimal digit that the command appends to the count.
    ///
    /// `0` names the first-column motion until a count is already open, so it
    /// carries no count command of its own. The semantic reducer reads
    /// [`Command::MoveFirstColumn`] as the zero digit while a count is open.
    ///
    /// ```
    /// use kvim_input::Command;
    ///
    /// assert_eq!(Command::CountDigitThree.count_digit(), Some(3));
    /// assert_eq!(Command::MoveDown.count_digit(), None);
    /// ```
    #[inline]
    #[must_use]
    pub const fn count_digit(self) -> Option<u8> {
        let digit = match self {
            Self::CountDigitOne => 1,
            Self::CountDigitTwo => 2,
            Self::CountDigitThree => 3,
            Self::CountDigitFour => 4,
            Self::CountDigitFive => 5,
            Self::CountDigitSix => 6,
            Self::CountDigitSeven => 7,
            Self::CountDigitEight => 8,
            Self::CountDigitNine => 9,
            _ => return None,
        };
        Some(digit)
    }

    /// Reports whether the command starts an operator that waits for a target.
    ///
    /// The resolver reads its own answer here: while an operator waits, the
    /// keys belong to [`BindingScope::OperatorPending`], where `i` and `a`
    /// start a text object instead of Insert mode.
    ///
    /// ```
    /// use kvim_input::Command;
    ///
    /// assert!(Command::DeleteOverMotion.starts_operator_pending());
    /// // A Visual operator acts on the selection at once, so it waits for
    /// // nothing.
    /// assert!(!Command::DeleteSelection.starts_operator_pending());
    /// ```
    ///
    /// [`BindingScope::OperatorPending`]: crate::BindingScope::OperatorPending
    #[inline]
    #[must_use]
    pub const fn starts_operator_pending(self) -> bool {
        matches!(
            self,
            Self::DeleteOverMotion | Self::ChangeOverMotion | Self::YankOverMotion
        )
    }
}

impl CommandMetadata for Command {
    /// Returns the stable identifier that a registry checks and a configuration
    /// file names.
    #[inline]
    fn id(&self) -> &str {
        (*self).id()
    }

    /// Returns the short label that a which-key overlay shows.
    #[inline]
    fn label(&self) -> &str {
        (*self).label()
    }
}

impl fmt::Display for Command {
    /// Writes the stable identifier of the command.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Command, CommandGroup};

    #[test]
    fn identifiers_and_labels_stay_unique() {
        let ids = Command::ALL
            .iter()
            .map(|command| command.id())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            ids.len(),
            Command::ALL.len(),
            "a later configuration loader binds keys by the identifier, so it must be unique"
        );
        let labels = Command::ALL
            .iter()
            .map(|command| command.label())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            labels.len(),
            Command::ALL.len(),
            "the which-key overlay shows one label for each command"
        );
    }

    #[test]
    fn identifiers_use_lowercase_kebab_case() {
        for command in Command::ALL {
            let id = command.id();
            assert!(
                !id.is_empty()
                    && id
                        .chars()
                        .all(|value| value.is_ascii_lowercase() || value == '-'),
                "{id} is not a stable kebab-case identifier"
            );
        }
    }

    #[test]
    fn each_section_of_the_command_table_reaches_its_own_group() {
        let cases = [
            (Command::SearchNext, CommandGroup::Search),
            (Command::GoToDefinition, CommandGroup::Code),
            (Command::CloseWindow, CommandGroup::Window),
            (Command::SaveBuffer, CommandGroup::Buffer),
            (Command::TreeRename, CommandGroup::Tree),
            (Command::MoveLeft, CommandGroup::Other),
        ];
        for (command, group) in cases {
            assert_eq!(command.group(), group, "{command} carries another group");
        }
    }

    #[test]
    fn every_file_tree_command_carries_the_tree_group() {
        for command in Command::ALL {
            assert_eq!(
                command.group() == CommandGroup::Tree,
                command.id().starts_with("tree-"),
                "{command} names the file tree, or it does not"
            );
        }
    }

    #[test]
    fn one_row_over_two_groups_falls_to_the_default_group() {
        assert_eq!(
            CommandGroup::Search.merged(CommandGroup::Window),
            CommandGroup::Other,
            "no single icon can name two groups"
        );
        assert_eq!(
            CommandGroup::Search.merged(CommandGroup::Search),
            CommandGroup::Search
        );
    }
}
