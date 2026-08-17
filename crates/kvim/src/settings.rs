//! The `EditorSettings` structure and its defaults.
//!
//! The defaults match the reference Neovim configuration. The first release does
//! not read a configuration file. A later release adds a loader that overrides
//! these fields. The module depends on no other module.

use std::num::NonZeroU8;
use std::time::Duration;

/// The largest file that Kvim loads into a buffer, in bytes.
///
/// The limit protects the editor against unbounded memory use. ReviewGraph uses
/// the same bound for analysis sources.
pub const FILE_BYTES_MAX: u64 = 4 * 1024 * 1024;

/// The largest count that the input resolver accepts before one command.
pub const COUNT_MAX: u32 = 9_999;

/// The default width of the file-tree sidebar, in cells.
///
/// The value holds a nested path of a Rust repository beside the editor
/// windows. See `docs/windows.md`.
pub const FILE_TREE_WIDTH_CELLS_DEFAULT: u16 = 40;

/// The largest number of keys that one pending input sequence holds.
pub const PENDING_KEYS_MAX: u8 = 4;

/// The default time between the first key of a sequence and the which-key
/// overlay.
///
/// The reference which-key configuration uses the same delay. See
/// `docs/input-actions.md`.
pub const WHICH_KEY_DELAY_DEFAULT: Duration = Duration::from_millis(500);

/// One 24-bit terminal color.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Rgb {
    /// Creates a color from its red, green, and blue components.
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

/// The rule that reserves the sign column beside the line numbers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SignColumn {
    /// Reserve the sign column only while the buffer shows a sign.
    Auto,
    /// Reserve the sign column at all times.
    #[default]
    Always,
    /// Never reserve the sign column.
    Never,
}

/// The number of cells that one shift operator moves.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ShiftWidth {
    /// Shift by the configured tab width.
    #[default]
    FollowTabWidth,
    /// Shift by an explicit number of cells.
    Cells(NonZeroU8),
}

impl ShiftWidth {
    /// Resolves the shift width against the configured tab width.
    #[must_use]
    pub const fn resolve(self, tab_width: NonZeroU8) -> NonZeroU8 {
        match self {
            Self::FollowTabWidth => tab_width,
            Self::Cells(cells) => cells,
        }
    }
}

/// The rule that compares a search query with the buffer text.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CaseSensitivity {
    /// Compare the case of every character.
    Sensitive,
    /// Ignore the case of every character.
    Insensitive,
    /// Ignore the case until the query contains an uppercase character.
    #[default]
    SmartCase,
}

/// Whether the file tree paints one icon before each name.
///
/// An icon needs a patched font. A terminal without one hides the icons, and the
/// tree still aligns, because a hidden icon reserves no cell in any row. See
/// `docs/files.md`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FileTreeIcons {
    /// Paint one icon before each name.
    #[default]
    Shown,
    /// Paint no icon.
    Hidden,
}

/// The side that receives a new horizontal split.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HorizontalSplitPlacement {
    /// Put the new window above the current window.
    Above,
    /// Put the new window below the current window.
    #[default]
    Below,
}

/// The side that receives a new vertical split.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VerticalSplitPlacement {
    /// Put the new window left of the current window.
    Left,
    /// Put the new window right of the current window.
    #[default]
    Right,
}

/// The depth of the diagnostic check that a language server runs.
///
/// The value stays language neutral. Each language adapter maps the mode onto
/// the option of its own server, so no setting names one server. See
/// `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CheckDepth {
    /// Run only the compile or type check of the language.
    Compile,
    /// Run the extended lint check of the language, when the server has one.
    #[default]
    Lints,
}

/// The width-to-height ratio that selects a vertical split.
///
/// The adaptive split command selects a vertical split when the window width
/// exceeds the window height multiplied by this ratio.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitRatio(f32);

impl SplitRatio {
    /// Creates a ratio from a finite value that is greater than zero.
    ///
    /// Returns `None` for a value that is not finite or not greater than zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim::settings::SplitRatio;
    ///
    /// assert!(SplitRatio::new(2.5).is_some());
    /// assert!(SplitRatio::new(f32::NAN).is_none());
    /// ```
    #[must_use]
    pub fn new(value: f32) -> Option<Self> {
        if value.is_finite() && value > 0.0 {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Returns the ratio value.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// The visible layout of one editor window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplaySettings {
    /// Show the absolute number of the cursor line.
    pub number: bool,
    /// Show the distance of every other line from the cursor line.
    pub relative_number: bool,
    /// Wrap a long line onto the next terminal row.
    pub wrap: bool,
    /// The vertical scroll margin, in rows.
    pub scrolloff_rows: u16,
    /// The horizontal scroll margin, in cells.
    pub sidescrolloff_cells: u16,
    /// The sign column rule.
    pub signcolumn: SignColumn,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            number: true,
            relative_number: true,
            wrap: false,
            scrolloff_rows: 2,
            sidescrolloff_cells: 4,
            signcolumn: SignColumn::Always,
        }
    }
}

/// The indent policy that the buffer applies to tabs and shifts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndentSettings {
    /// Insert spaces instead of one tab character.
    pub expand_tab: bool,
    /// The number of cells that one tab character occupies.
    pub tab_width: NonZeroU8,
    /// The number of cells that one shift operator moves.
    pub shift_width: ShiftWidth,
}

impl Default for IndentSettings {
    fn default() -> Self {
        Self {
            expand_tab: true,
            tab_width: NonZeroU8::new(4).expect("the literal 4 is not zero"),
            shift_width: ShiftWidth::FollowTabWidth,
        }
    }
}

/// The search behavior of the editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchSettings {
    /// The rule that compares the query with the buffer text.
    pub case_sensitivity: CaseSensitivity,
    /// Highlight every match of the active query.
    pub highlight_matches: bool,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            case_sensitivity: CaseSensitivity::SmartCase,
            highlight_matches: true,
        }
    }
}

/// The split, focus, and resize behavior of the window tree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowSettings {
    /// The side that receives a new horizontal split.
    pub horizontal_split_placement: HorizontalSplitPlacement,
    /// The side that receives a new vertical split.
    pub vertical_split_placement: VerticalSplitPlacement,
    /// The width-to-height ratio that selects a vertical split.
    pub adaptive_split_ratio: SplitRatio,
    /// The number of cells that one directional resize command moves.
    pub resize_step_cells: u16,
    /// The smallest usable window width, in cells.
    ///
    /// The value keeps a line number column, a sign column, and readable text
    /// visible after a split or a terminal resize.
    pub min_window_width_cells: u16,
    /// The smallest usable window height, in rows.
    ///
    /// The value keeps a winbar row, a text row, and a statusline row visible.
    pub min_window_height_rows: u16,
    /// The width of the file-tree sidebar, in cells.
    ///
    /// The sidebar keeps a fixed width. A directional resize toward it changes
    /// the width of the open sidebar only, not this default.
    pub file_tree_width_cells: u16,
    /// Whether the file tree paints one icon before each name.
    pub file_tree_icons: FileTreeIcons,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            horizontal_split_placement: HorizontalSplitPlacement::Below,
            vertical_split_placement: VerticalSplitPlacement::Right,
            adaptive_split_ratio: SplitRatio(2.5),
            resize_step_cells: 6,
            min_window_width_cells: 20,
            min_window_height_rows: 3,
            file_tree_width_cells: FILE_TREE_WIDTH_CELLS_DEFAULT,
            file_tree_icons: FileTreeIcons::Shown,
        }
    }
}

/// The file load and save policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileSettings {
    /// Keep a persistent undo file beside the editor state directory.
    pub undo_file: bool,
    /// Format a buffer through the language server before every save.
    pub format_on_save: bool,
    /// Replace a file through a staged atomic write where the platform supports it.
    pub atomic_save: bool,
    /// The largest file that Kvim loads into a buffer, in bytes.
    pub max_file_bytes: u64,
}

impl Default for FileSettings {
    fn default() -> Self {
        Self {
            undo_file: true,
            format_on_save: true,
            atomic_save: true,
            max_file_bytes: FILE_BYTES_MAX,
        }
    }
}

/// The bounds of the modal input resolver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputSettings {
    /// The time between the first key of a sequence and the which-key overlay.
    ///
    /// The delay keeps a fast key combination from flashing the overlay. A
    /// pending sequence itself never expires, so the resolver holds no other
    /// time value. See `docs/input-actions.md`.
    pub which_key_delay: Duration,
    /// The largest count that the resolver accepts before one command.
    pub count_max: u32,
    /// The largest number of keys that one pending sequence holds.
    pub pending_keys_max: u8,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            which_key_delay: WHICH_KEY_DELAY_DEFAULT,
            count_max: COUNT_MAX,
            pending_keys_max: PENDING_KEYS_MAX,
        }
    }
}

/// The language service policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LanguageSettings {
    /// The depth of the diagnostic check that a language server runs.
    pub check_depth: CheckDepth,
    /// Request and render language server diagnostics.
    pub diagnostics_enabled: bool,
}

impl Default for LanguageSettings {
    fn default() -> Self {
        Self {
            check_depth: CheckDepth::Lints,
            diagnostics_enabled: true,
        }
    }
}

/// The two background colors that Kvim overrides on top of tokyonight night.
///
/// Slice 8 owns the complete palette and the semantic theme roles. Do not add
/// further palette values here before that slice defines the roles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThemeSettings {
    /// The darkened editor background.
    pub base: Rgb,
    /// The darkened surface background of panes and overlays.
    pub surface: Rgb,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            base: Rgb::new(0x11, 0x13, 0x17),
            surface: Rgb::new(0x16, 0x1a, 0x20),
        }
    }
}

/// Every adjustable editor behavior in one structure.
///
/// The defaults match the reference Neovim configuration. A caller reads a
/// default and overrides a field in code. The first release does not read a
/// configuration file.
///
/// # Examples
///
/// ```
/// use kvim::settings::{EditorSettings, ShiftWidth, SignColumn};
///
/// let mut settings = EditorSettings::default();
/// assert_eq!(settings.display.signcolumn, SignColumn::Always);
/// assert_eq!(settings.indent.shift_width, ShiftWidth::FollowTabWidth);
///
/// settings.display.wrap = true;
/// assert!(settings.display.wrap);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EditorSettings {
    /// The visible layout of one editor window.
    pub display: DisplaySettings,
    /// The file load and save policy.
    pub files: FileSettings,
    /// The indent policy of the buffer.
    pub indent: IndentSettings,
    /// The bounds of the modal input resolver.
    pub input: InputSettings,
    /// The language service policy.
    pub language: LanguageSettings,
    /// The search behavior of the editor.
    pub search: SearchSettings,
    /// The two background colors that Kvim overrides.
    pub theme: ThemeSettings,
    /// The split, focus, and resize behavior of the window tree.
    pub windows: WindowSettings,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU8;

    use super::{ShiftWidth, SplitRatio};

    fn cells(value: u8) -> NonZeroU8 {
        NonZeroU8::new(value).expect("the test value is not zero")
    }

    #[test]
    fn follow_tab_width_resolves_to_the_tab_width() {
        assert_eq!(ShiftWidth::FollowTabWidth.resolve(cells(4)), cells(4));
        assert_eq!(ShiftWidth::FollowTabWidth.resolve(cells(8)), cells(8));
    }

    #[test]
    fn explicit_cells_resolve_to_themselves() {
        assert_eq!(ShiftWidth::Cells(cells(2)).resolve(cells(8)), cells(2));
    }

    #[test]
    fn split_ratio_rejects_values_outside_its_domain() {
        assert!(SplitRatio::new(f32::NAN).is_none());
        assert!(SplitRatio::new(f32::INFINITY).is_none());
        assert!(SplitRatio::new(0.0).is_none());
        assert!(SplitRatio::new(-1.0).is_none());
    }
}
