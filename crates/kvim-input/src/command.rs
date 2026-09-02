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
    InsertBeforeCursor => ("insert-before-cursor", "Insert before cursor"),
    InsertAtFirstNonBlank => ("insert-at-first-non-blank", "Insert at first non-blank"),
    InsertAfterCursor => ("insert-after-cursor", "Insert after cursor"),
    InsertAtLineEnd => ("insert-at-line-end", "Insert at line end"),
    OpenLineBelow => ("open-line-below", "Open line below"),
    OpenLineAbove => ("open-line-above", "Open line above"),
    EnterVisual => ("enter-visual", "Enter Visual mode"),
    EnterVisualLine => ("enter-visual-line", "Enter Visual Line mode"),
    EnterVisualBlock => ("enter-visual-block", "Enter Visual Block mode"),
    OpenCommandLine => ("open-command-line", "Open command line"),
    ReturnToNormal => ("return-to-normal", "Return to Normal mode"),

    // Insert-mode text entry. A printable key reaches the text fallback of the
    // scope, so only the keys that type no character carry a command.
    InsertLineBreak => ("insert-line-break", "Insert line break"),
    DeleteCharacterBefore => ("delete-character-before", "Delete character before cursor"),
    DeleteWordBefore => ("delete-word-before", "Delete word before cursor"),
    InsertIndent => ("insert-indent", "Insert indent step"),

    // Motions.
    MoveLeft => ("move-left", "Move left"),
    MoveDown => ("move-down", "Move down"),
    MoveUp => ("move-up", "Move up"),
    MoveRight => ("move-right", "Move right"),
    MoveNextWordStart => ("move-next-word-start", "Move to next word start"),
    MovePreviousWordStart => ("move-previous-word-start", "Move to previous word start"),
    MoveNextWordEnd => ("move-next-word-end", "Move to next word end"),
    MoveFirstColumn => ("move-first-column", "Move to first column"),
    MoveFirstNonBlank => ("move-first-non-blank", "Move to first non-blank"),
    MoveLastNonBlank => ("move-last-non-blank", "Move to last non-blank"),
    MoveLineEnd => ("move-line-end", "Move to line end"),
    MoveMatchingBracket => ("move-matching-bracket", "Move to matching bracket"),
    MoveFirstLine => ("move-first-line", "Move to first line"),
    MoveLastLine => ("move-last-line", "Move to last or count line"),
    MoveHalfPageDown => ("move-half-page-down", "Move down half page"),
    MoveHalfPageUp => ("move-half-page-up", "Move up half page"),
    MoveFullPageDown => ("move-full-page-down", "Move down full page"),
    MoveFullPageUp => ("move-full-page-up", "Move up full page"),
    CenterCursorLine => ("center-cursor-line", "Center cursor line"),
    AlignCursorLineTop => ("align-cursor-line-top", "Align cursor line to top"),
    AlignCursorLineBottom => ("align-cursor-line-bottom", "Align cursor line to bottom"),

    // The jump list. A jump records the position that the cursor held before
    // it, and these two commands walk the recorded positions of the focused
    // window. An ordinary motion records nothing.
    JumpBack => ("jump-back", "Jump to previous position"),
    JumpForward => ("jump-forward", "Jump to next position"),

    // Count digits. A digit is a surface command, so it reaches the semantic
    // reducer through the shared registry instead of a second key table. `0` is
    // the first-column motion until a count is already open, so it keeps
    // `MoveFirstColumn` and the reducer reads it as the zero digit.
    CountDigitOne => ("count-digit-one", "Append one to count"),
    CountDigitTwo => ("count-digit-two", "Append two to count"),
    CountDigitThree => ("count-digit-three", "Append three to count"),
    CountDigitFour => ("count-digit-four", "Append four to count"),
    CountDigitFive => ("count-digit-five", "Append five to count"),
    CountDigitSix => ("count-digit-six", "Append six to count"),
    CountDigitSeven => ("count-digit-seven", "Append seven to count"),
    CountDigitEight => ("count-digit-eight", "Append eight to count"),
    CountDigitNine => ("count-digit-nine", "Append nine to count"),

    // The prompt line. Every prompt reads the same keys, so one scope holds
    // them and printable input falls through to the prompt text.
    PromptAccept => ("prompt-accept", "Run prompt line"),
    PromptCancel => ("prompt-cancel", "Cancel prompt line"),
    PromptDeleteBackward => ("prompt-delete-backward", "Delete character before prompt"),
    PromptDeleteWordBackward => ("prompt-delete-word-backward", "Delete word before prompt"),
    PromptCursorLeft => ("prompt-cursor-left", "Prompt cursor left"),
    PromptCursorRight => ("prompt-cursor-right", "Prompt cursor right"),
    PromptCursorWordBackward => ("prompt-cursor-word-backward", "Prompt cursor word back"),
    PromptCursorWordForward => ("prompt-cursor-word-forward", "Prompt cursor word forward"),
    PromptCursorLineStart => ("prompt-cursor-line-start", "Prompt cursor to line start"),
    PromptCursorLineEnd => ("prompt-cursor-line-end", "Prompt cursor to line end"),
    PromptCompleteNext => ("prompt-complete-next", "Next completion candidate"),
    PromptCompletePrevious => ("prompt-complete-previous", "Previous completion candidate"),

    // The review of one captured diff.
    OpenReview => ("open-review", "Show worktree changes"),
    CloseReview => ("close-review", "Close review"),
    ToggleReviewView => ("toggle-review-view", "Toggle inline view"),
    NextHunk => ("next-hunk", "Next hunk"),
    PreviousHunk => ("previous-hunk", "Previous hunk"),
    NextUnreadHunk => ("next-unread-hunk", "Next unread hunk"),
    PreviousUnreadHunk => ("previous-unread-hunk", "Previous unread hunk"),
    NextChangedFile => ("next-changed-file", "First hunk of next file"),
    PreviousChangedFile => ("previous-changed-file", "First hunk of previous file"),
    MarkHunkRead => ("mark-hunk-read", "Mark hunk read"),
    RefreshReview => ("refresh-review", "Refresh review"),
    NextReviewSection => ("next-review-section", "Next review section"),
    PreviousReviewSection => ("previous-review-section", "Previous review section"),
    OpenHunkFile => ("open-hunk-file", "Open hunk file"),

    // Operators, registers, and repeat.
    SelectRegister => ("select-register", "Select register"),
    DeleteOverMotion => ("delete-over-motion", "Delete over motion"),
    ChangeOverMotion => ("change-over-motion", "Change over motion"),
    YankOverMotion => ("yank-over-motion", "Yank over motion"),
    DeleteSelection => ("delete-selection", "Delete selection"),
    ChangeSelection => ("change-selection", "Change selection"),
    YankSelection => ("yank-selection", "Yank selection"),
    BlockInsertBefore => ("block-insert-before", "Insert before selected lines"),
    BlockInsertAfter => ("block-insert-after", "Insert after selected lines"),
    DeleteLine => ("delete-line", "Delete line"),
    ChangeLine => ("change-line", "Change line"),
    YankLine => ("yank-line", "Yank line"),
    DeleteToLineEnd => ("delete-to-line-end", "Delete to line end"),
    ChangeToLineEnd => ("change-to-line-end", "Change to line end"),
    PasteAfter => ("paste-after", "Paste after cursor"),
    PasteBefore => ("paste-before", "Paste before cursor"),
    Undo => ("undo", "Undo one change"),
    Redo => ("redo", "Redo one change"),
    RepeatChange => ("repeat-change", "Repeat last change"),
    ToggleCase => ("toggle-case", "Toggle case"),

    // Text objects. The open and the close delimiter name the same object, so
    // `i(` and `i)` reach one command.
    SelectInnerWord => ("select-inner-word", "Select word"),
    SelectAroundWord => ("select-around-word", "Select word and blanks"),
    SelectInnerLongWord => ("select-inner-long-word", "Select non-blank run"),
    SelectAroundLongWord => ("select-around-long-word", "Select run and blanks"),
    SelectInnerParen => ("select-inner-paren", "Select inside round brackets"),
    SelectAroundParen => ("select-around-paren", "Select round brackets"),
    SelectInnerBracket => ("select-inner-bracket", "Select inside square brackets"),
    SelectAroundBracket => ("select-around-bracket", "Select square brackets"),
    SelectInnerBrace => ("select-inner-brace", "Select inside curly brackets"),
    SelectAroundBrace => ("select-around-brace", "Select curly brackets"),
    SelectInnerAngle => ("select-inner-angle", "Select inside angle brackets"),
    SelectAroundAngle => ("select-around-angle", "Select angle brackets"),
    SelectInnerDoubleQuote => ("select-inner-double-quote", "Select inside double quotes"),
    SelectAroundDoubleQuote => ("select-around-double-quote", "Select double quotes"),
    SelectInnerSingleQuote => ("select-inner-single-quote", "Select inside single quotes"),
    SelectAroundSingleQuote => ("select-around-single-quote", "Select single quotes"),
    SelectInnerBacktick => ("select-inner-backtick", "Select inside backticks"),
    SelectAroundBacktick => ("select-around-backtick", "Select backticks"),

    // Search.
    OpenSearchPrompt => ("open-search-prompt", "Open search prompt"),
    SearchNext => ("search-next", "Next match"),
    SearchPrevious => ("search-previous", "Previous match"),
    EndSearch => ("end-search", "End search"),

    // Visual selection.
    MoveSelectionDown => ("move-selection-down", "Move selection down"),
    MoveSelectionUp => ("move-selection-up", "Move selection up"),
    ShiftSelectionLeft => ("shift-selection-left", "Shift selection left"),
    ShiftSelectionRight => ("shift-selection-right", "Shift selection right"),

    // Files and buffers.
    SaveBuffer => ("save-buffer", "Save buffer"),
    SaveBufferAndClose => ("save-buffer-and-close", "Save buffer and close"),
    SaveAllBuffers => ("save-all-buffers", "Save all buffers"),
    RevealInFileTree => ("reveal-in-file-tree", "Reveal file in tree"),
    OpenBufferPicker => ("open-buffer-picker", "Open buffer picker"),
    // `CloseBuffer` and `UnloadBuffer` differ only in what the last loaded
    // buffer does. `CloseBuffer` closes the editor, as Neovim's `:q` does.
    // `UnloadBuffer` opens the scratch buffer and keeps the editor open.
    CloseBuffer => ("close-buffer", "Close buffer"),
    UnloadBuffer => ("unload-buffer", "Unload buffer"),
    OpenFilePicker => ("open-file-picker", "Open file picker"),
    OpenRipgrepPicker => ("open-ripgrep-picker", "Open ripgrep picker"),

    // The file tree.
    TreeOpenEntry => ("tree-open-entry", "Open entry"),
    TreeToggleEntry => ("tree-toggle-entry", "Expand or collapse directory"),
    TreeExpandEntry => ("tree-expand-entry", "Expand or open entry"),
    TreeCollapseEntry => ("tree-collapse-entry", "Collapse or select parent"),
    TreeSelectParent => ("tree-select-parent", "Select parent directory"),
    TreeRefresh => ("tree-refresh", "Refresh tree"),
    TreeAddFile => ("tree-add-file", "Add file"),
    // `TreeAddFile` reads one path, so it creates a directory as well. This
    // command is therefore redundant, and it is a candidate for removal. It
    // stays because removing a published command breaks an embedding host.
    TreeAddDirectory => ("tree-add-directory", "Add directory"),
    TreeDelete => ("tree-delete", "Delete entry"),
    TreeRename => ("tree-rename", "Rename entry"),
    TreeCopyEntry => ("tree-copy-entry", "Copy entry"),
    TreeCutEntry => ("tree-cut-entry", "Cut entry"),
    TreePasteEntries => ("tree-paste-entries", "Paste held entries"),
    TreeToggleHidden => ("tree-toggle-hidden", "Toggle hidden entries"),
    TreeSearch => ("tree-search", "Search entries"),

    // The pickers.
    PickerSelectNext => ("picker-select-next", "Select next result"),
    PickerSelectPrevious => ("picker-select-previous", "Select previous result"),
    // The picker reads no buffer, so the half-page step of a buffer window
    // names no picker motion. These two commands carry it instead.
    PickerSelectPageNext => ("picker-select-page-next", "Select next half page"),
    PickerSelectPagePrevious => ("picker-select-page-previous", "Select previous half page"),

    // Windows.
    FocusWindowLeft => ("focus-window-left", "Focus window left"),
    FocusWindowDown => ("focus-window-down", "Focus window down"),
    FocusWindowUp => ("focus-window-up", "Focus window up"),
    FocusWindowRight => ("focus-window-right", "Focus window right"),
    ResizeWindowLeft => ("resize-window-left", "Resize window left"),
    ResizeWindowDown => ("resize-window-down", "Resize window down"),
    ResizeWindowUp => ("resize-window-up", "Resize window up"),
    ResizeWindowRight => ("resize-window-right", "Resize window right"),
    SplitAdaptive => ("split-adaptive", "Split window"),
    SplitInverseAdaptive => ("split-inverse-adaptive", "Split window (inverse)"),
    CloseWindow => ("close-window", "Close window"),

    // Language services.
    ToggleComment => ("toggle-comment", "Toggle comment"),
    GoToDefinition => ("go-to-definition", "Go to definition"),
    ShowHover => ("show-hover", "Show hover"),
    ShowDiagnosticFloat => ("show-diagnostic-float", "Show diagnostic"),
    NextDiagnostic => ("next-diagnostic", "Next diagnostic"),
    PreviousDiagnostic => ("previous-diagnostic", "Previous diagnostic"),
    ToggleFormatOnSave => ("toggle-format-on-save", "Toggle format-on-save"),
}

/// What one command can durably change.
///
/// The authority is a property of the command, not of one editor instance. An
/// embedded editor with view-only access refuses every command above
/// [`CommandAuthority::Read`] before that command reaches the buffer or the
/// workspace. See `docs/embedding.md`.
///
/// The match is exhaustive, so a new command cannot reach an editor without an
/// authority decision.
///
/// ```
/// use kvim_input::{Command, CommandAuthority};
///
/// assert_eq!(Command::MoveLeft.authority(), CommandAuthority::Read);
/// assert_eq!(Command::DeleteLine.authority(), CommandAuthority::Text);
/// assert_eq!(Command::SaveBuffer.authority(), CommandAuthority::Workspace);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CommandAuthority {
    /// The command changes no buffer text and no workspace entry.
    Read,
    /// The command can change the text of a buffer.
    Text,
    /// The command can write a file or change a workspace entry.
    Workspace,
}

impl Command {
    /// Returns what the command can durably change.
    ///
    /// A mode switch that opens Insert mode counts as [`CommandAuthority::Text`],
    /// because the mode exists only to type text into the buffer.
    ///
    /// ```
    /// use kvim_input::{Command, CommandAuthority};
    ///
    /// assert_eq!(Command::YankLine.authority(), CommandAuthority::Read);
    /// assert_eq!(Command::InsertBeforeCursor.authority(), CommandAuthority::Text);
    /// assert_eq!(Command::TreeDelete.authority(), CommandAuthority::Workspace);
    /// ```
    #[inline]
    #[must_use]
    pub const fn authority(self) -> CommandAuthority {
        match self {
            Self::InsertBeforeCursor
            | Self::InsertAtFirstNonBlank
            | Self::InsertAfterCursor
            | Self::InsertAtLineEnd
            | Self::OpenLineBelow
            | Self::OpenLineAbove
            | Self::InsertLineBreak
            | Self::DeleteCharacterBefore
            | Self::DeleteWordBefore
            | Self::InsertIndent
            | Self::DeleteOverMotion
            | Self::ChangeOverMotion
            | Self::DeleteSelection
            | Self::ChangeSelection
            | Self::BlockInsertBefore
            | Self::BlockInsertAfter
            | Self::DeleteLine
            | Self::ChangeLine
            | Self::DeleteToLineEnd
            | Self::ChangeToLineEnd
            | Self::PasteAfter
            | Self::PasteBefore
            | Self::Undo
            | Self::Redo
            | Self::RepeatChange
            | Self::ToggleCase
            | Self::MoveSelectionDown
            | Self::MoveSelectionUp
            | Self::ShiftSelectionLeft
            | Self::ShiftSelectionRight
            | Self::ToggleComment => CommandAuthority::Text,

            Self::SaveBuffer
            | Self::SaveBufferAndClose
            | Self::SaveAllBuffers
            | Self::TreeAddFile
            | Self::TreeAddDirectory
            | Self::TreeDelete
            | Self::TreeRename
            | Self::TreePasteEntries => CommandAuthority::Workspace,

            Self::OpenReview
            | Self::CloseReview
            | Self::ToggleReviewView
            | Self::NextHunk
            | Self::PreviousHunk
            | Self::NextUnreadHunk
            | Self::PreviousUnreadHunk
            | Self::NextChangedFile
            | Self::PreviousChangedFile
            | Self::MarkHunkRead
            | Self::RefreshReview
            | Self::NextReviewSection
            | Self::PreviousReviewSection
            | Self::OpenHunkFile
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
            | Self::JumpBack
            | Self::JumpForward
            | Self::CountDigitOne
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
            | Self::PromptDeleteWordBackward
            | Self::PromptCursorLeft
            | Self::PromptCursorRight
            | Self::PromptCursorWordBackward
            | Self::PromptCursorWordForward
            | Self::PromptCursorLineStart
            | Self::PromptCursorLineEnd
            | Self::PromptCompleteNext
            | Self::PromptCompletePrevious
            | Self::SelectRegister
            | Self::YankOverMotion
            | Self::YankSelection
            | Self::YankLine
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
            | Self::OpenSearchPrompt
            | Self::SearchNext
            | Self::SearchPrevious
            | Self::EndSearch
            | Self::RevealInFileTree
            | Self::OpenBufferPicker
            // Closing and unloading write nothing durably, exactly as
            // `CloseWindow` writes nothing.
            | Self::CloseBuffer
            | Self::UnloadBuffer
            | Self::OpenFilePicker
            | Self::OpenRipgrepPicker
            | Self::TreeOpenEntry
            | Self::TreeToggleEntry
            | Self::TreeExpandEntry
            | Self::TreeCollapseEntry
            | Self::TreeSelectParent
            | Self::TreeRefresh
            | Self::TreeCopyEntry
            | Self::TreeCutEntry
            | Self::TreeToggleHidden
            | Self::TreeSearch
            | Self::PickerSelectNext
            | Self::PickerSelectPrevious
            | Self::PickerSelectPageNext
            | Self::PickerSelectPagePrevious
            | Self::FocusWindowLeft
            | Self::FocusWindowDown
            | Self::FocusWindowUp
            | Self::FocusWindowRight
            | Self::ResizeWindowLeft
            | Self::ResizeWindowDown
            | Self::ResizeWindowUp
            | Self::ResizeWindowRight
            | Self::SplitAdaptive
            | Self::SplitInverseAdaptive
            | Self::CloseWindow
            | Self::GoToDefinition
            | Self::ShowHover
            | Self::ShowDiagnosticFloat
            | Self::NextDiagnostic
            | Self::PreviousDiagnostic
            | Self::ToggleFormatOnSave => CommandAuthority::Read,
        }
    }
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
    /// The review of one captured diff.
    Review,
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
            | Self::SaveBufferAndClose
            | Self::SaveAllBuffers
            | Self::RevealInFileTree
            | Self::OpenBufferPicker
            | Self::CloseBuffer
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

            Self::OpenReview
            | Self::CloseReview
            | Self::ToggleReviewView
            | Self::NextHunk
            | Self::PreviousHunk
            | Self::NextUnreadHunk
            | Self::PreviousUnreadHunk
            | Self::NextChangedFile
            | Self::PreviousChangedFile
            | Self::MarkHunkRead
            | Self::RefreshReview
            | Self::NextReviewSection
            | Self::PreviousReviewSection
            | Self::OpenHunkFile => CommandGroup::Review,

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
            | Self::PromptDeleteWordBackward
            | Self::PromptCursorLeft
            | Self::PromptCursorRight
            | Self::PromptCursorWordBackward
            | Self::PromptCursorWordForward
            | Self::PromptCursorLineStart
            | Self::PromptCursorLineEnd
            | Self::PromptCompleteNext
            | Self::PromptCompletePrevious
            | Self::SelectRegister
            | Self::InsertLineBreak
            | Self::DeleteCharacterBefore
            | Self::DeleteWordBefore
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
            | Self::JumpBack
            | Self::JumpForward
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
            | Self::ToggleCase
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
            | Self::PickerSelectPrevious
            | Self::PickerSelectPageNext
            | Self::PickerSelectPagePrevious => CommandGroup::Other,
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
#[path = "command_tests.rs"]
mod tests;
