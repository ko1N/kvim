//! The semantic theme roles and the one style lookup of the editor.
//! Adapted from ReviewGraph (MIT), src/tui.rs.
//!
//! A call site names one role. This module is the one place in the workspace
//! that holds a color value, so a palette change never reaches a call site.
//! To recolor the editor, edit the palette below and rebuild. The default
//! palette is tokyonight night with a darkened base color and surface color.
//! See `docs/windows.md`.

use kvim_language::{MarkupRole, SyntaxRole};
use kvim_workspace::GitStatus;
use ratatui::style::{Color, Modifier, Style};

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

/// The color of an error message.
const ERROR: Color = Color::Rgb(0xdb, 0x4b, 0x4b);

/// The color of a warning message.
const WARNING: Color = Color::Rgb(0xe0, 0xaf, 0x68);

/// The color of an informational message.
const INFO: Color = Color::Rgb(0x0d, 0xb9, 0xd7);

/// The color of a hint message.
const HINT: Color = Color::Rgb(0x1a, 0xbc, 0x9c);

/// The color of an attribute, a macro, and a preprocessor directive.
const SYNTAX_ATTRIBUTE: Color = Color::Rgb(0x7d, 0xcf, 0xff);

/// The color of a comment.
const SYNTAX_COMMENT: Color = Color::Rgb(0x56, 0x5f, 0x89);

/// The color of a constructor and of a statement.
const SYNTAX_CONSTRUCTOR: Color = Color::Rgb(0xbb, 0x9a, 0xf7);

/// The color of a delimiter and of an operator.
const SYNTAX_OPERATOR: Color = Color::Rgb(0x89, 0xdd, 0xff);

/// The color of a keyword.
const SYNTAX_KEYWORD: Color = Color::Rgb(0x9d, 0x7c, 0xd8);

/// The color of a property.
const SYNTAX_PROPERTY: Color = Color::Rgb(0x73, 0xda, 0xca);

/// The color of a string literal.
const SYNTAX_STRING: Color = Color::Rgb(0x9e, 0xce, 0x6a);

/// The color of a type name.
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
    TreeGit(GitStatus),
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
    Markup(MarkupRole),
    /// One glyph that the float draws for a markup document, such as a
    /// thematic break or the marker of a list item.
    MarkupStructure,
    /// One syntax role of a language adapter.
    Syntax(SyntaxRole),
}

/// The one style lookup of the editor.
///
/// # Examples
///
/// ```
/// use kvim_tui::{Theme, ThemeRole};
///
/// let theme = Theme::new();
/// // The editor background is the darkened base color of the palette.
/// let text = theme.style(ThemeRole::Text);
/// assert_eq!(text.bg, Some(ratatui::style::Color::Rgb(0x11, 0x13, 0x17)));
/// // The winbar band uses the darkened surface color instead.
/// let winbar = theme.style(ThemeRole::Winbar);
/// assert_eq!(winbar.bg, Some(ratatui::style::Color::Rgb(0x16, 0x1a, 0x20)));
/// ```
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
            ThemeRole::TreeGit(GitStatus::Modified | GitStatus::StagedAndModified) => {
                Style::new().fg(ACCENT_WARM)
            }
            ThemeRole::TreeGit(GitStatus::Staged) => Style::new().fg(HINT),
            ThemeRole::TreeGit(GitStatus::Untracked) => Style::new().fg(INFO),
            ThemeRole::TreeGit(GitStatus::Ignored) => Style::new().fg(TEXT_MUTED),
            ThemeRole::TreeGit(GitStatus::Conflicted) => Style::new().fg(ERROR),
            // An icon decorates the row below it, so it carries a foreground
            // color only and keeps the row background and the selection.
            ThemeRole::Icon(role) => Style::new().fg(icon_color(role)),
            // A markup role decorates the surface band of the float, so it
            // carries a foreground color and a modifier only. A background of
            // its own would cut a hole into that band. See `docs/windows.md`.
            ThemeRole::Markup(role) => markup_style(role),
            ThemeRole::MarkupStructure => Style::new().fg(NON_TEXT),
            ThemeRole::Error => Style::new().fg(ERROR),
            ThemeRole::Warning => Style::new().fg(WARNING),
            ThemeRole::Info => Style::new().fg(INFO),
            ThemeRole::Hint => Style::new().fg(HINT),
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
        IconRole::CommandOther => HINT,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use kvim_language::{MarkupRole, SyntaxRole};
    use ratatui::style::Modifier;

    use super::{BASE, SURFACE, Theme, ThemeRole};

    fn theme() -> Theme {
        Theme::new()
    }

    #[test]
    fn the_two_background_colors_reach_every_background_band() {
        let theme = theme();
        let base = BASE;
        let surface = SURFACE;
        for role in [
            ThemeRole::Text,
            ThemeRole::NonText,
            ThemeRole::EndOfBuffer,
            ThemeRole::LineNumber,
            ThemeRole::CursorLineNumber,
            ThemeRole::SignColumn,
        ] {
            assert_eq!(theme.style(role).bg, Some(base), "{role:?} uses the base");
        }
        for role in [
            ThemeRole::Surface,
            ThemeRole::Statusline,
            ThemeRole::StatuslineMuted,
            ThemeRole::Winbar,
            ThemeRole::Title,
            ThemeRole::TitleMuted,
            ThemeRole::TreeRoot,
        ] {
            assert_eq!(
                theme.style(role).bg,
                Some(surface),
                "{role:?} uses the surface"
            );
        }
        // The notification overlay paints text alone, so no role of it may own
        // a background color. A background would hide the buffer behind it.
        for role in [
            ThemeRole::NotificationRunning,
            ThemeRole::NotificationDone,
            ThemeRole::NotificationMessage,
            ThemeRole::NotificationGroup,
        ] {
            assert_eq!(theme.style(role).bg, None, "{role:?} paints no background");
        }
    }

    #[test]
    fn a_markup_role_decorates_the_surface_band_of_the_float() {
        // The float paints one row over its own surface band, so a markup style
        // that owned a background would cut a hole into that band.
        let theme = theme();
        for role in MARKUP_ROLES {
            let style = theme.style(ThemeRole::Markup(role));
            assert_eq!(style.bg, None, "{role:?} paints no background");
            assert!(style.fg.is_some(), "{role:?} names its own foreground");
        }
        let structure = theme.style(ThemeRole::MarkupStructure);
        assert_eq!(structure.bg, None, "a float glyph paints no background");
    }

    /// Every markup role, so a new role reaches the test above.
    const MARKUP_ROLES: [MarkupRole; 7] = [
        MarkupRole::Text,
        MarkupRole::Heading,
        MarkupRole::Emphasis,
        MarkupRole::Strong,
        MarkupRole::InlineCode,
        MarkupRole::Link,
        MarkupRole::Quote,
    ];

    #[test]
    fn the_end_of_buffer_marker_stays_readable_over_the_editor_background() {
        // The role owned the background color on both sides and hid the marker.
        let marker = theme().style(ThemeRole::EndOfBuffer);
        assert_ne!(
            marker.fg, marker.bg,
            "the marker must separate from the row behind it"
        );
    }

    #[test]
    fn a_decoration_role_carries_no_foreground_of_its_own() {
        // The cursor and the selection patch over the style below them, so a
        // later syntax color survives both.
        let cursor = theme().style(ThemeRole::Cursor);
        assert_eq!((cursor.fg, cursor.bg), (None, None));
        assert!(cursor.add_modifier.contains(Modifier::REVERSED));
        let selection = theme().style(ThemeRole::Selection);
        assert_eq!(selection.fg, None);
        assert!(selection.bg.is_some());
    }

    #[test]
    fn a_window_title_never_matches_a_syntax_style() {
        // `docs/windows.md` keeps a pane title readable as chrome instead of as
        // code. The palette gives the title and the function role one hue, so
        // the surface band and the bold modifier carry the distinction.
        let title = theme().style(ThemeRole::Title);
        for role in SYNTAX_ROLES {
            assert_ne!(
                theme().style(ThemeRole::Syntax(role)),
                title,
                "{role:?} must not render like a window title"
            );
        }
    }

    /// Every syntax role, so a new role reaches the tests above.
    const SYNTAX_ROLES: [SyntaxRole; 19] = [
        SyntaxRole::Attribute,
        SyntaxRole::Boolean,
        SyntaxRole::Bracket,
        SyntaxRole::Comment,
        SyntaxRole::Constant,
        SyntaxRole::Constructor,
        SyntaxRole::Delimiter,
        SyntaxRole::Function,
        SyntaxRole::Keyword,
        SyntaxRole::Macro,
        SyntaxRole::Number,
        SyntaxRole::Operator,
        SyntaxRole::Parameter,
        SyntaxRole::Preprocessor,
        SyntaxRole::Property,
        SyntaxRole::Statement,
        SyntaxRole::String,
        SyntaxRole::Type,
        SyntaxRole::Variable,
    ];

    /// The source files that may hold a color value.
    ///
    /// This module owns the palette. A test file names a color to assert one,
    /// which is the only other honest reason to write one.
    const COLOR_FILES: [&str; 3] = ["theme.rs", "render_tests.rs", "picker_tests.rs"];

    /// Returns every Rust source file of this crate.
    fn crate_sources(directory: &Path) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        let entries = fs::read_dir(directory).expect("the crate source directory is readable");
        for entry in entries {
            let path = entry.expect("one directory entry is readable").path();
            if path.is_dir() {
                found.extend(crate_sources(&path));
            } else if path.extension().is_some_and(|kind| kind == "rs") {
                found.push(path);
            }
        }
        found
    }

    #[test]
    fn only_the_theme_module_names_a_color() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let sources = crate_sources(&root);
        assert!(!sources.is_empty(), "the crate holds source files to read");
        for path in sources {
            let name = path
                .file_name()
                .expect("a source file has a name")
                .to_string_lossy()
                .into_owned();
            if COLOR_FILES.contains(&name.as_str()) {
                continue;
            }
            let text = fs::read_to_string(&path).expect("a source file is readable");
            for (number, line) in text.lines().enumerate() {
                // A doc line and a comment describe a color without painting
                // one, so the guard reads code alone.
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                assert!(
                    !code.contains("Color::"),
                    "{name}:{} names a color; move it to a role in theme.rs",
                    number + 1
                );
            }
        }
    }
}
