//! The semantic theme roles and the one style lookup of the editor.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! A call site names one role. This module is the one place in the workspace
//! that holds a color value, so a palette change never reaches a call site.
//! To recolor the editor, edit the palette below and rebuild. The default
//! palette is tokyonight night with a darkened base color and surface color.
//! See `docs/windows.md`.

use std::num::NonZeroU16;

use kvim_ui::fade_foreground;
use ratatui::style::{Color, Modifier, Style};

#[cfg(feature = "editor")]
pub(crate) use super::file_sidebar::FileRowGit;
#[cfg(feature = "editor")]
use kvim_language::{MarkupRole, SyntaxRole};

/// The Git state used by pure review painting.
#[cfg(not(feature = "editor"))]
/// The source-control state used by review painting.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FileRowGit {
    /// Ignored by source control.
    Ignored,
    /// Not tracked by source control.
    Untracked,
    /// Changed in the index only.
    Staged,
    /// Changed in the worktree only.
    Modified,
    /// Changed in both the index and worktree.
    StagedAndModified,
    /// Holds an unresolved conflict.
    Conflicted,
}

/// The editor background.
const BASE: Color = Color::Rgb(0x11, 0x13, 0x17);

/// The background band of a pane, an overlay, and a statusline.
const SURFACE: Color = Color::Rgb(0x16, 0x1a, 0x20);

/// The normal text color.
const TEXT: Color = Color::Rgb(0xc0, 0xca, 0xf5);

/// The text color of the statusline.
const TEXT_DIM: Color = Color::Rgb(0xa9, 0xb1, 0xd6);

/// The text color of an unfocused region, a line number, and the sign column.
const TEXT_MUTED: Color = Color::Rgb(0x3b, 0x42, 0x61);

/// The color of a glyph that stands for absent text.
const NON_TEXT: Color = Color::Rgb(0x54, 0x5c, 0x7e);

/// The warm accent color of the cursor line number and the current match.
const ACCENT_WARM: Color = Color::Rgb(0xff, 0x9e, 0x64);

/// The color of a window title and of an overlay key.
const TITLE: Color = Color::Rgb(0x7a, 0xa2, 0xf7);

/// The background of a Visual selection.
const SELECTION_BACKGROUND: Color = Color::Rgb(0x28, 0x34, 0x57);

/// The background of one search match.
const SEARCH_BACKGROUND: Color = Color::Rgb(0x3d, 0x59, 0xa1);

/// The foreground of the current search match.
const CURRENT_SEARCH_FOREGROUND: Color = Color::Rgb(0x15, 0x16, 0x1e);

/// The background of the selected popup row.
const POPUP_SELECTION_BACKGROUND: Color = Color::Rgb(0x34, 0x3a, 0x55);

/// The background of the footer band of a modal dialog.
///
/// The band sits one step lighter than the popup surface, so the choice row
/// separates from the body above it, the way the reference popup separates
/// its button row from its body.
const DIALOG_FOOTER_BACKGROUND: Color = Color::Rgb(0x1e, 0x22, 0x2b);

/// The color of an error message.
const ERROR: Color = Color::Rgb(0xdb, 0x4b, 0x4b);

/// The band behind one added diff line.
const DIFF_ADDED_BACKGROUND: Color = Color::Rgb(0x1b, 0x2b, 0x25);

/// The band behind one removed diff line.
const DIFF_REMOVED_BACKGROUND: Color = Color::Rgb(0x2d, 0x1c, 0x20);

/// The color of a warning message.
const WARNING: Color = Color::Rgb(0xe0, 0xaf, 0x68);

/// The color of an informational message.
const INFO: Color = Color::Rgb(0x0d, 0xb9, 0xd7);

/// The color of a hint message.
const HINT: Color = Color::Rgb(0x1a, 0xbc, 0x9c);

/// The color of an attribute, a macro, and a preprocessor directive.
#[cfg(feature = "editor")]
const SYNTAX_ATTRIBUTE: Color = Color::Rgb(0x7d, 0xcf, 0xff);

/// The color of a comment.
#[cfg(feature = "editor")]
const SYNTAX_COMMENT: Color = Color::Rgb(0x56, 0x5f, 0x89);

/// The color of a constructor and of a statement.
#[cfg(feature = "editor")]
const SYNTAX_CONSTRUCTOR: Color = Color::Rgb(0xbb, 0x9a, 0xf7);

/// The color of a delimiter and of an operator.
#[cfg(feature = "editor")]
const SYNTAX_OPERATOR: Color = Color::Rgb(0x89, 0xdd, 0xff);

/// The color of a keyword.
#[cfg(feature = "editor")]
const SYNTAX_KEYWORD: Color = Color::Rgb(0x9d, 0x7c, 0xd8);

/// The color of a property.
#[cfg(feature = "editor")]
const SYNTAX_PROPERTY: Color = Color::Rgb(0x73, 0xda, 0xca);

/// The color of a string literal.
#[cfg(feature = "editor")]
const SYNTAX_STRING: Color = Color::Rgb(0x9e, 0xce, 0x6a);

/// The color of a type name.
#[cfg(feature = "editor")]
const SYNTAX_TYPE: Color = Color::Rgb(0x2a, 0xc3, 0xde);

/// The meaning of one icon.
///
/// The role names what an icon stands for, never its color, so the theme keeps
/// every color value. An icon is presentation data: it selects no parser, no
/// indent rule, no comment token, and no language server. See
/// `docs/architecture.md`.
///
/// The file tree names an entry kind, and the which-key overlay names a command
/// group. Both read from the one icon table of `docs/files.md`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum IconRole {
    /// A directory, open or closed.
    Directory,
    /// A source file of a programming language.
    Code,
    /// A configuration or structured data file.
    Configuration,
    /// A prose document.
    Document,
    /// An executable script.
    Script,
    /// A file of the version-control system.
    VersionControl,
    /// A file that a tool generates, such as a lock file.
    Generated,
    /// An image or another binary asset.
    Media,
    /// Every other file.
    Unknown,
    /// A command of the buffer search.
    CommandSearch,
    /// A command of the language services.
    CommandCode,
    /// A command that acts on a window.
    CommandWindow,
    /// A command that acts on a file or a buffer.
    CommandBuffer,
    /// A command that acts on the file tree.
    CommandTree,
    /// The icon of one review command.
    CommandReview,
    /// Every other command.
    CommandOther,
}

/// One semantic interface role.
///
/// The role names the meaning of a region, never its color.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ThemeRole {
    /// Buffer text on the editor background.
    Text,
    /// A glyph that stands for absent text.
    NonText,
    /// The marker on the rows below the last buffer line.
    EndOfBuffer,
    /// The cell that holds the cursor.
    Cursor,
    /// A cell inside the Visual selection.
    Selection,
    /// A cell inside one search match.
    SearchMatch,
    /// A cell inside the search match that holds the cursor.
    CurrentSearchMatch,
    /// A cell of the bracket pair that the cursor stands on.
    MatchingBracket,
    /// A line number that is not the cursor line.
    LineNumber,
    /// The absolute number of the cursor line.
    CursorLineNumber,
    /// The sign column beside the line numbers.
    SignColumn,
    /// The subtle track of one editor-pane scrollbar.
    ScrollbarTrack,
    /// The brighter thumb of one editor-pane scrollbar.
    ScrollbarThumb,
    /// The dimmed editor body behind a modal dialog.
    DialogDim,
    /// The full-height rail of a modal dialog.
    DialogRail,
    /// The optional severity glyph on the title row of a modal dialog.
    DialogIcon,
    /// The optional detail text of a modal dialog.
    DialogBody,
    /// The question text of a modal dialog.
    DialogQuestion,
    /// The footer band that holds the choice row of a modal dialog.
    DialogFooter,
    /// One unfocused dialog choice.
    DialogChoice,
    /// The safe default dialog choice while it is unfocused.
    DialogDefaultChoice,
    /// The focused dialog choice.
    DialogFocusedChoice,
    /// The background band of a floating surface or a popup.
    Surface,
    /// The statusline text.
    Statusline,
    /// The quiet part of the statusline, such as the format-on-save state.
    StatuslineMuted,
    /// The winbar band above one window.
    Winbar,
    /// The title of a focused window or of an overlay.
    Title,
    /// The title of an unfocused window.
    TitleMuted,
    /// The selected row of a popup list.
    PopupSelection,
    /// The filled selection band of the picker's selected result row.
    PickerSelection,
    /// The muted picker chrome text: the query placeholder, the `esc` title
    /// hint, and the descriptions of the key hint row.
    PickerMuted,
    /// The key name of one picker hint, such as `esc` or the arrow glyphs.
    PickerHintKey,
    /// The state of one running item of the notification overlay.
    NotificationRunning,
    /// The state of one finished item of the notification overlay.
    NotificationDone,
    /// The message of one item of the notification overlay.
    NotificationMessage,
    /// The group title and the spinner of the notification overlay.
    NotificationGroup,
    /// The workspace root path in the header row of the file tree.
    TreeRoot,
    /// The name of one directory in the file tree.
    TreeDirectory,
    /// An entry that holds machine output, or that a file operation holds.
    TreeMuted,
    /// A file-tree row that counts the entries the tree keeps out of its rows.
    TreeNotice,
    /// A file-tree row that reports a bounded or a failed directory read.
    TreeIncomplete,
    /// One indent guide of the file tree.
    TreeIndentGuide,
    /// The mark at the left edge of the selected file-tree row.
    TreeSelectionMark,
    /// One recorded Git state of a file-tree entry.
    ///
    /// The role names the published [`FileRowGit`], so a host colors a file
    /// sidebar row of its own without a package that `docs/architecture.md`
    /// keeps out of the supported set.
    TreeGit(FileRowGit),
    /// One unchanged line of a diff, which both sides hold.
    DiffContext,
    /// One line that the new side added.
    DiffAdded,
    /// One line that the old side lost.
    DiffRemoved,
    /// One column that draws no line, opposite a removal or an addition.
    DiffGap,
    /// The line number beside one diff line.
    DiffLineNumber,
    /// The header of one changed file or one hunk.
    DiffHeader,
    /// The tab that owns its strip.
    TabActive,
    /// One tab that another tab owns the strip over.
    TabInactive,
    /// An error message.
    Error,
    /// A warning message.
    Warning,
    /// An informational message.
    Info,
    /// A hint message.
    Hint,
    /// One icon of the file tree or of the which-key overlay.
    Icon(IconRole),
    /// One markup role of one server answer.
    #[cfg(feature = "editor")]
    Markup(MarkupRole),
    /// One glyph that the float draws for a markup document, such as a
    /// thematic break or the marker of a list item.
    MarkupStructure,
    /// One syntax role of a language adapter.
    #[cfg(feature = "editor")]
    Syntax(SyntaxRole),
}

/// The one style lookup of the editor.
///
/// The default `editor` feature publishes this internal palette through the
/// normal lower-level API. Pure review hosts use it only through `kvim-embed`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Theme {
    base: Color,
    surface: Color,
}

impl Default for Theme {
    /// Returns the palette of this module.
    ///
    /// The derived default would fill both fields with [`Color::Reset`], which
    /// is not the palette, so the default delegates to [`Theme::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl Theme {
    /// Creates the theme over the palette of this module.
    ///
    /// The palette is compiled in, so a recolored editor is one edit of this
    /// module and one rebuild. See `docs/windows.md`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            base: BASE,
            surface: SURFACE,
        }
    }

    /// Moves one RGB foreground toward its effective RGB background.
    pub(crate) fn fade_foreground(
        self,
        style: Style,
        background: Option<Color>,
        step: u16,
        steps: NonZeroU16,
    ) -> Style {
        fade_foreground(style.fg, background, step, steps)
            .map_or_else(Style::new, |foreground| Style::new().fg(foreground))
    }
    /// Returns the style of one semantic role.
    ///
    /// A role that only decorates existing text returns the decoration alone,
    /// so a call site applies it with [`Style::patch`] over the style below it.
    #[must_use]
    pub fn style(self, role: ThemeRole) -> Style {
        match role {
            ThemeRole::Text => Style::new().fg(TEXT).bg(self.base),
            ThemeRole::NonText => Style::new().fg(NON_TEXT).bg(self.base),
            // The reference palette paints the marker in the background color
            // and hides it. kvim marks the rows that hold no text instead, so
            // the marker takes the color of a glyph that stands for absent
            // text and stays readable without drawing the reader's eye.
            ThemeRole::EndOfBuffer => Style::new().fg(NON_TEXT).bg(self.base),
            // The cursor inverts the cell below it, so it needs no color of its
            // own and stays correct over text, a selection, and a match.
            ThemeRole::Cursor => Style::new().add_modifier(Modifier::REVERSED),
            ThemeRole::Selection => Style::new().bg(SELECTION_BACKGROUND),
            ThemeRole::SearchMatch => Style::new().fg(TEXT).bg(SEARCH_BACKGROUND),
            ThemeRole::CurrentSearchMatch => {
                Style::new().fg(CURRENT_SEARCH_FOREGROUND).bg(ACCENT_WARM)
            }
            // The reference configuration marks a matching bracket with the
            // warm accent and the bold modifier. The role carries no background
            // of its own, so the selection band and the search band stay
            // visible under it and keep their own meaning.
            ThemeRole::MatchingBracket => Style::new().fg(ACCENT_WARM).add_modifier(Modifier::BOLD),
            ThemeRole::LineNumber => Style::new().fg(TEXT_MUTED).bg(self.base),
            ThemeRole::CursorLineNumber => Style::new()
                .fg(ACCENT_WARM)
                .bg(self.base)
                .add_modifier(Modifier::BOLD),
            ThemeRole::SignColumn => Style::new().fg(TEXT_MUTED).bg(self.base),
            ThemeRole::ScrollbarTrack => Style::new().fg(TEXT_MUTED),
            ThemeRole::ScrollbarThumb => Style::new().fg(TEXT_DIM),
            ThemeRole::DialogDim => Style::new().fg(TEXT_MUTED).bg(self.base),
            // The rail keeps the accent of this palette, so the popup reads as
            // part of the editor and not as a guest from another theme. The
            // severity glyph beside it takes the warning color instead,
            // because it names the risk of the question, not the popup.
            ThemeRole::DialogRail => Style::new().fg(TITLE).bg(self.surface),
            ThemeRole::DialogIcon => Style::new().fg(WARNING).bg(self.surface),
            ThemeRole::DialogBody => Style::new().fg(TEXT_DIM).bg(self.surface),
            ThemeRole::DialogQuestion => Style::new()
                .fg(TEXT)
                .bg(self.surface)
                .add_modifier(Modifier::BOLD),
            // The footer band paints its own background, distinct from the
            // popup surface, so the choice row reads as a separated band.
            ThemeRole::DialogFooter => Style::new().fg(TEXT).bg(DIALOG_FOOTER_BACKGROUND),
            // An unfocused chip reads as quiet text on the footer band, so it
            // carries no background of its own; render patches it over the
            // footer style. It stays at the absent-text color rather than the
            // line-number color, because a choice the reader can select must
            // stay legible against the band.
            ThemeRole::DialogChoice => Style::new().fg(NON_TEXT),
            // The safe default stays distinguishable from a plain choice
            // without competing with the filled focused chip, so it reads
            // brighter than a plain choice but carries no bold weight.
            ThemeRole::DialogDefaultChoice => Style::new().fg(TEXT_DIM),
            // The focused chip fills with the palette accent behind the editor
            // background, which reads as text on a filled band.
            ThemeRole::DialogFocusedChoice => Style::new()
                .fg(self.base)
                .bg(TITLE)
                .add_modifier(Modifier::BOLD),
            ThemeRole::Surface => Style::new().fg(TEXT).bg(self.surface),
            ThemeRole::Statusline => Style::new().fg(TEXT_DIM).bg(self.surface),
            ThemeRole::StatuslineMuted => Style::new().fg(TEXT_MUTED).bg(self.surface),
            ThemeRole::Winbar => Style::new().fg(TEXT).bg(self.surface),
            ThemeRole::Title => Style::new()
                .fg(TITLE)
                .bg(self.surface)
                .add_modifier(Modifier::BOLD),
            ThemeRole::TitleMuted => Style::new().fg(TEXT_MUTED).bg(self.surface),
            ThemeRole::PopupSelection => Style::new().bg(POPUP_SELECTION_BACKGROUND),
            // The picker marks its one selected row with the same filled
            // accent band that the focused dialog chip uses, so the two
            // popups read as one visual vocabulary.
            ThemeRole::PickerSelection => Style::new()
                .fg(self.base)
                .bg(TITLE)
                .add_modifier(Modifier::BOLD),
            // Picker chrome text carries a foreground color only, so it
            // patches over the surface band that the title row, the query
            // row, and the hint row already paint.
            ThemeRole::PickerMuted => Style::new().fg(TEXT_MUTED),
            ThemeRole::PickerHintKey => Style::new().fg(TEXT_DIM),
            // The notification overlay paints text alone over the buffer, as
            // the reference configuration does, so every role of it carries a
            // foreground color only. A background would hide the buffer text
            // and the end-of-buffer markers behind the overlay. See
            // `docs/language-services.md`.
            ThemeRole::NotificationRunning => {
                Style::new().fg(ACCENT_WARM).add_modifier(Modifier::ITALIC)
            }
            ThemeRole::NotificationDone => Style::new().fg(HINT).add_modifier(Modifier::ITALIC),
            ThemeRole::NotificationMessage => Style::new().fg(TEXT).add_modifier(Modifier::ITALIC),
            ThemeRole::NotificationGroup => Style::new()
                .fg(TITLE)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            ThemeRole::TreeRoot => Style::new()
                .fg(TITLE)
                .bg(self.surface)
                .add_modifier(Modifier::ITALIC),
            // Every file-tree role below decorates the row behind it, so it
            // carries a foreground color only and keeps the selection band.
            ThemeRole::TreeDirectory => Style::new().fg(TITLE),
            ThemeRole::TreeMuted => Style::new().fg(TEXT_MUTED),
            ThemeRole::TreeNotice => Style::new().fg(TEXT_MUTED).add_modifier(Modifier::ITALIC),
            // An incomplete read keeps entries that the reader expects out of
            // the rows, so it warns instead of reading as one quiet report.
            ThemeRole::TreeIncomplete => Style::new().fg(WARNING).add_modifier(Modifier::ITALIC),
            ThemeRole::TreeIndentGuide => Style::new().fg(TEXT_MUTED),
            ThemeRole::TreeSelectionMark => Style::new().fg(TEXT_DIM),
            // The Git roles decorate the row behind them as well. A change of
            // the working tree takes the warm accent that the reference
            // configuration paints, and the index takes the calm color of a
            // recorded state. An ignored entry dims like a generated one, so
            // one rule decides the color of every quiet row.
            ThemeRole::TreeGit(FileRowGit::Modified | FileRowGit::StagedAndModified) => {
                Style::new().fg(ACCENT_WARM)
            }
            ThemeRole::TreeGit(FileRowGit::Staged) => Style::new().fg(HINT),
            ThemeRole::TreeGit(FileRowGit::Untracked) => Style::new().fg(INFO),
            ThemeRole::TreeGit(FileRowGit::Ignored) => Style::new().fg(TEXT_MUTED),
            ThemeRole::TreeGit(FileRowGit::Conflicted) => Style::new().fg(ERROR),
            // A diff paints the change and not the syntax, so a context line
            // keeps the ordinary text color and the two changed sides take the
            // colors that every reviewer already reads as added and removed.
            ThemeRole::DiffContext => Style::new().fg(TEXT).bg(self.base),
            ThemeRole::DiffAdded => Style::new().fg(HINT).bg(DIFF_ADDED_BACKGROUND),
            ThemeRole::DiffRemoved => Style::new().fg(ERROR).bg(DIFF_REMOVED_BACKGROUND),
            // A gap holds no line at all, so it draws as a quiet band instead
            // of an empty row that reads like unchanged text.
            ThemeRole::DiffGap => Style::new().fg(NON_TEXT).bg(SURFACE),
            ThemeRole::DiffLineNumber => Style::new().fg(TEXT_MUTED).bg(self.base),
            ThemeRole::DiffHeader => Style::new().fg(TITLE).bg(SURFACE),
            // The active tab takes the background of the body below it, so the
            // tab connects to its own content and the bar stays above them
            // both. A band lighter than the bar reads washed out instead,
            // because the bar and the text then sit at the same weight.
            ThemeRole::TabActive => Style::new().fg(TEXT).bg(self.base),
            ThemeRole::TabInactive => Style::new().fg(TEXT_MUTED).bg(self.surface),
            // An icon decorates the row below it, so it carries a foreground
            // color only and keeps the row background and the selection.
            ThemeRole::Icon(role) => Style::new().fg(icon_color(role)),
            // A markup role decorates the surface band of the float, so it
            // carries a foreground color and a modifier only. A background of
            // its own would cut a hole into that band. See `docs/windows.md`.
            #[cfg(feature = "editor")]
            ThemeRole::Markup(role) => markup_style(role),
            ThemeRole::MarkupStructure => Style::new().fg(NON_TEXT),
            ThemeRole::Error => Style::new().fg(ERROR),
            ThemeRole::Warning => Style::new().fg(WARNING),
            ThemeRole::Info => Style::new().fg(INFO),
            ThemeRole::Hint => Style::new().fg(HINT),
            #[cfg(feature = "editor")]
            ThemeRole::Syntax(syntax) => syntax_style(syntax),
        }
    }
}

/// Returns the style of one syntax role.
///
/// The comment role and the keyword role carry the italic modifier of the
/// reference configuration. Every other role carries a color only.
///
/// The role vocabulary is non-exhaustive, so a role that a later release of
/// `kvim-syntax` adds paints as plain text until this theme names it.
#[cfg(feature = "editor")]
fn syntax_style(role: SyntaxRole) -> Style {
    let style = Style::new();
    match role {
        SyntaxRole::Attribute | SyntaxRole::Macro | SyntaxRole::Preprocessor => {
            style.fg(SYNTAX_ATTRIBUTE)
        }
        SyntaxRole::Boolean | SyntaxRole::Constant | SyntaxRole::Number => style.fg(ACCENT_WARM),
        SyntaxRole::Bracket => style.fg(TEXT_DIM),
        SyntaxRole::Comment => style.fg(SYNTAX_COMMENT).add_modifier(Modifier::ITALIC),
        SyntaxRole::Constructor | SyntaxRole::Statement => style.fg(SYNTAX_CONSTRUCTOR),
        SyntaxRole::Delimiter | SyntaxRole::Operator => style.fg(SYNTAX_OPERATOR),
        SyntaxRole::Function => style.fg(TITLE),
        SyntaxRole::Keyword => style.fg(SYNTAX_KEYWORD).add_modifier(Modifier::ITALIC),
        SyntaxRole::Parameter => style.fg(WARNING),
        SyntaxRole::Property => style.fg(SYNTAX_PROPERTY),
        SyntaxRole::String => style.fg(SYNTAX_STRING),
        SyntaxRole::Type => style.fg(SYNTAX_TYPE),
        SyntaxRole::Variable => style.fg(TEXT),
        _ => style.fg(TEXT),
    }
}

/// Returns the style of one markup role.
///
/// The heading and the strong role carry the bold modifier, the emphasis role
/// carries the italic one, and the link role carries the underline, so a reader
/// separates them without a color of their own. A code span takes the color of
/// a string literal, because both hold source text. See `docs/windows.md`.
#[cfg(feature = "editor")]
fn markup_style(role: MarkupRole) -> Style {
    let style = Style::new();
    match role {
        MarkupRole::Text => style.fg(TEXT),
        MarkupRole::Heading => style.fg(TITLE).add_modifier(Modifier::BOLD),
        MarkupRole::Emphasis => style.fg(TEXT).add_modifier(Modifier::ITALIC),
        MarkupRole::Strong => style.fg(TEXT).add_modifier(Modifier::BOLD),
        MarkupRole::InlineCode => style.fg(SYNTAX_STRING),
        MarkupRole::Link => style.fg(INFO).add_modifier(Modifier::UNDERLINED),
        MarkupRole::Quote => style.fg(TEXT_DIM).add_modifier(Modifier::ITALIC),
    }
}

/// Returns the color of one file-tree icon role.
///
/// Every value comes from the palette that the interface roles already use, so
/// the icons add no new color to the theme.
const fn icon_color(role: IconRole) -> Color {
    match role {
        IconRole::Directory => TITLE,
        IconRole::Code => ACCENT_WARM,
        IconRole::Configuration => WARNING,
        IconRole::Document => TEXT,
        IconRole::Script => HINT,
        IconRole::VersionControl => ERROR,
        IconRole::Generated => TEXT_MUTED,
        IconRole::Media => INFO,
        IconRole::Unknown => NON_TEXT,
        IconRole::CommandSearch => WARNING,
        IconRole::CommandCode => ACCENT_WARM,
        IconRole::CommandWindow => INFO,
        IconRole::CommandBuffer => TEXT,
        IconRole::CommandTree => TITLE,
        IconRole::CommandReview => ACCENT_WARM,
        IconRole::CommandOther => HINT,
    }
}

#[cfg(all(test, feature = "editor"))]
#[path = "theme_tests.rs"]
mod tests;
