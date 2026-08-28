//! The `EditorSettings` structure and its defaults.
//!
//! The defaults match the reference Neovim configuration. The first release does
//! not read a configuration file. A later release adds a loader that overrides
//! these fields. The crate depends on no other crate.

use std::fmt;
use std::num::NonZeroU8;
use std::time::Duration;

/// The largest file that kvim loads into a buffer, in bytes.
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

/// The largest number of rows that the notification overlay paints.
///
/// The reference `fidget.nvim` configuration uses the same render limit. See
/// `docs/language-services.md`.
pub const NOTIFICATION_ROWS_MAX: usize = 16;

/// The default time of one complete spinner cycle of the notification overlay.
///
/// The reference `fidget.nvim` configuration names the same period for its
/// `dots` pattern.
pub const NOTIFICATION_SPINNER_PERIOD_DEFAULT: Duration = Duration::from_secs(1);

/// The default time that a finished notification stays visible.
///
/// The reference `fidget.nvim` configuration names the same value.
pub const NOTIFICATION_DONE_TTL_DEFAULT: Duration = Duration::from_secs(2);

/// A field of [`EditorSettings`] did not establish its published resource bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsError {
    /// One numeric setting is zero.
    ZeroBound {
        /// The stable field name.
        field: &'static str,
    },
    /// One numeric setting exceeds its published maximum.
    InvalidBound {
        /// The stable field name.
        field: &'static str,
        /// The inclusive maximum.
        max: u64,
        /// The supplied value.
        actual: u64,
    },
    /// One duration is zero.
    ZeroDuration {
        /// The stable field name.
        field: &'static str,
    },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroBound { field } => {
                write!(
                    formatter,
                    "the editor setting {field} must be greater than zero"
                )
            }
            Self::InvalidBound { field, max, actual } => write!(
                formatter,
                "the editor setting {field} must not exceed {max}, got {actual}"
            ),
            Self::ZeroDuration { field } => {
                write!(
                    formatter,
                    "the editor setting {field} must be greater than zero"
                )
            }
        }
    }
}

impl std::error::Error for SettingsError {}

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

/// The number of cells that one indent level occupies.
///
/// A language adapter declares the width of one indent level for its language,
/// exactly as it declares its comment token, so the default follows the
/// language. An explicit width overrides every language.
/// [`IndentSettings::indent_columns`] owns the complete resolution order. See
/// `docs/settings.md`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IndentWidth {
    /// Indent by the width that the language adapter of the buffer declares.
    #[default]
    FollowLanguage,
    /// Indent by an explicit number of cells, in every language.
    Cells(NonZeroU8),
}

impl IndentWidth {
    /// Resolves the indent width against the language of the buffer.
    ///
    /// `language` is the width that the language adapter of the buffer
    /// declares, or `None` for a buffer that no adapter serves. `fallback`
    /// answers that buffer.
    #[must_use]
    pub const fn resolve(self, language: Option<NonZeroU8>, fallback: NonZeroU8) -> NonZeroU8 {
        match self {
            Self::FollowLanguage => match language {
                Some(cells) => cells,
                None => fallback,
            },
            Self::Cells(cells) => cells,
        }
    }
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
    /// use kvim_settings::SplitRatio;
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
            scrolloff_rows: 2,
            sidescrolloff_cells: 4,
            signcolumn: SignColumn::Always,
        }
    }
}

/// The view that the diff of one review draws.
///
/// Both views read the same aligned rows, so they cannot disagree about what
/// one hunk holds. See `docs/diff-view.md`.
///
/// # Examples
///
/// ```
/// use kvim_settings::{DiffSettings, DiffView};
///
/// // Two columns are the default, because a replacement reads best beside the
/// // text that it replaced.
/// assert_eq!(DiffSettings::default().view, DiffView::SideBySide);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiffView {
    /// Draw the old side and the new side in two columns.
    #[default]
    SideBySide,
    /// Draw one column and mark the origin of every line.
    Inline,
}

/// The diff view policy of the editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiffSettings {
    /// The view that a review opens with.
    pub view: DiffView,
    /// The smallest width that one column of the two-column view needs.
    ///
    /// A window narrower than two such columns draws inline instead, because
    /// two columns of a handful of cells each show nothing useful.
    pub side_column_cells_min: u16,
}

impl Default for DiffSettings {
    fn default() -> Self {
        Self {
            view: DiffView::SideBySide,
            side_column_cells_min: 24,
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
    /// The number of cells that one indent level occupies.
    ///
    /// The default follows the language of the buffer. See
    /// [`IndentSettings::indent_columns`] for the resolution order.
    pub indent_width: IndentWidth,
}

impl IndentSettings {
    /// Resolves the number of cells that one indent level takes in one buffer.
    ///
    /// `language` is the width that the language adapter of the buffer
    /// declares, or `None` for a buffer that no adapter serves. The resolution
    /// order is:
    ///
    /// 1. An explicit [`IndentWidth::Cells`] override wins for every language.
    /// 2. Otherwise, the width that the language adapter declares wins.
    /// 3. Otherwise, the shift width applies, which follows the tab width by
    ///    default.
    ///
    /// See `docs/settings.md`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::num::NonZeroU8;
    ///
    /// use kvim_settings::IndentSettings;
    ///
    /// let cells = NonZeroU8::new(2).expect("the literal 2 is not zero");
    /// let settings = IndentSettings::default();
    /// assert_eq!(settings.indent_columns(Some(cells)), cells);
    /// assert_eq!(settings.indent_columns(None), settings.tab_width);
    /// ```
    #[must_use]
    pub const fn indent_columns(&self, language: Option<NonZeroU8>) -> NonZeroU8 {
        self.indent_width
            .resolve(language, self.shift_width.resolve(self.tab_width))
    }
}

impl Default for IndentSettings {
    fn default() -> Self {
        Self {
            expand_tab: true,
            tab_width: NonZeroU8::new(4).expect("the literal 4 is not zero"),
            shift_width: ShiftWidth::FollowTabWidth,
            indent_width: IndentWidth::FollowLanguage,
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
    /// Format a buffer through the formatter of its language before every save.
    ///
    /// The setting names no formatter. The language adapter decides whether an
    /// external program or a language server formats the buffer. See
    /// `docs/language-services.md`.
    pub format_on_save: bool,
    /// Replace a file through a staged atomic write where the platform supports it.
    pub atomic_save: bool,
    /// The largest file that kvim loads into a buffer, in bytes.
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

/// The bounds and the timing of the notification overlay.
///
/// The overlay shows the progress of every language server and every editor
/// message on one surface. The event loop drives the spinner and the removal of
/// a finished item from these two times, so no frame loop exists. See
/// `docs/language-services.md` and `docs/responsiveness.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotificationSettings {
    /// The largest number of rows that the overlay paints.
    ///
    /// The overlay drops its oldest item above this bound.
    pub rows_max: usize,
    /// The time of one complete spinner cycle.
    ///
    /// The overlay divides the period by the number of spinner frames, so the
    /// pattern completes exactly once in this time.
    pub spinner_period: Duration,
    /// The time that a finished item stays visible.
    pub done_ttl: Duration,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            rows_max: NOTIFICATION_ROWS_MAX,
            spinner_period: NOTIFICATION_SPINNER_PERIOD_DEFAULT,
            done_ttl: NOTIFICATION_DONE_TTL_DEFAULT,
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

/// Every adjustable editor behavior in one structure.
///
/// The defaults match the reference Neovim configuration. A caller reads a
/// default and overrides a field in code. The first release does not read a
/// configuration file.
///
/// # Examples
///
/// ```
/// use kvim_settings::{EditorSettings, ShiftWidth, SignColumn};
///
/// let mut settings = EditorSettings::default();
/// assert_eq!(settings.display.signcolumn, SignColumn::Always);
/// assert_eq!(settings.indent.shift_width, ShiftWidth::FollowTabWidth);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EditorSettings {
    /// The view and the bounds of the diff of one review.
    pub diff: DiffSettings,
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
    /// The bounds and the timing of the notification overlay.
    pub notifications: NotificationSettings,
    /// The search behavior of the editor.
    pub search: SearchSettings,
    /// The split, focus, and resize behavior of the window tree.
    pub windows: WindowSettings,
}

impl EditorSettings {
    /// Realizes raw public settings as one validated editor configuration.
    ///
    /// The public fields remain available for source-compatible overrides. A
    /// host calls this boundary once before it creates editor state.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsError`] when a resolver, window, file, notification,
    /// or related numeric bound is zero or exceeds its published maximum.
    pub fn realize(self) -> Result<Self, SettingsError> {
        validate_bound(
            "files.max_file_bytes",
            self.files.max_file_bytes,
            FILE_BYTES_MAX,
        )?;
        validate_bound(
            "input.count_max",
            u64::from(self.input.count_max),
            u64::from(COUNT_MAX),
        )?;
        validate_bound(
            "input.pending_keys_max",
            u64::from(self.input.pending_keys_max),
            u64::from(PENDING_KEYS_MAX),
        )?;
        validate_bound(
            "notifications.rows_max",
            self.notifications.rows_max as u64,
            NOTIFICATION_ROWS_MAX as u64,
        )?;
        for (field, value) in [
            (
                "diff.side_column_cells_min",
                self.diff.side_column_cells_min,
            ),
            ("windows.resize_step_cells", self.windows.resize_step_cells),
            (
                "windows.min_window_width_cells",
                self.windows.min_window_width_cells,
            ),
            (
                "windows.min_window_height_rows",
                self.windows.min_window_height_rows,
            ),
            (
                "windows.file_tree_width_cells",
                self.windows.file_tree_width_cells,
            ),
        ] {
            validate_nonzero(field, u64::from(value))?;
        }
        for (field, value) in [
            ("input.which_key_delay", self.input.which_key_delay),
            (
                "notifications.spinner_period",
                self.notifications.spinner_period,
            ),
            ("notifications.done_ttl", self.notifications.done_ttl),
        ] {
            if value.is_zero() {
                return Err(SettingsError::ZeroDuration { field });
            }
        }
        Ok(self)
    }
}

fn validate_bound(field: &'static str, actual: u64, max: u64) -> Result<(), SettingsError> {
    validate_nonzero(field, actual)?;
    if actual > max {
        return Err(SettingsError::InvalidBound { field, max, actual });
    }
    Ok(())
}

fn validate_nonzero(field: &'static str, actual: u64) -> Result<(), SettingsError> {
    if actual == 0 {
        return Err(SettingsError::ZeroBound { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
