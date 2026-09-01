use std::fs;
use std::num::NonZeroU16;
use std::path::Path;

use kvim_language::{MarkupRole, SyntaxRole};
use kvim_ui::fade_foreground;
use ratatui::style::{Color, Modifier, Style};

use super::{BASE, SURFACE, Theme, ThemeRole};

fn theme() -> Theme {
    Theme::new()
}

#[test]
fn theme_fade_delegates_to_the_supported_color_rule() {
    let steps = NonZeroU16::new(4).expect("the literal four is not zero");
    let style = Style::new().fg(Color::Rgb(200, 100, 0));
    let background = Some(Color::Rgb(0, 0, 200));

    assert_eq!(
        theme().fade_foreground(style, background, 1, steps),
        fade_foreground(style.fg, background, 1, steps)
            .map_or_else(Style::new, |foreground| Style::new().fg(foreground))
    );
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

/// The production source files that may hold a color value.
///
/// This module owns the palette. A test file names a color to assert one,
/// which is the only other honest reason to write one, and the scan reads no
/// test file.
const COLOR_FILES: [&str; 1] = ["theme.rs"];

/// Returns every Rust source file of this crate.
fn holds_tests(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name == "tests.rs" || name.ends_with("_tests.rs")
    })
}

fn crate_sources(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let entries = fs::read_dir(directory).expect("the crate source directory is readable");
    for entry in entries {
        let path = entry.expect("one directory entry is readable").path();
        if path.is_dir() {
            found.extend(crate_sources(&path));
        } else if path.extension().is_some_and(|kind| kind == "rs") && !holds_tests(&path) {
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

#[test]
fn the_active_tab_takes_the_body_background_and_the_bar_stays_above_it() {
    // A band lighter than the bar reads washed out, because the bar and the
    // text then sit at the same weight. The active tab drops onto the color of
    // the body instead, so it connects to its own content.
    let theme = Theme::default();
    let active = theme.style(ThemeRole::TabActive);
    let inactive = theme.style(ThemeRole::TabInactive);
    let body = theme.style(ThemeRole::Text);

    assert_eq!(active.bg, body.bg, "the tab connects to the body below it");
    assert_ne!(active.bg, inactive.bg, "the bar stays above the active tab");
    assert_ne!(
        active.fg, inactive.fg,
        "the active tab reads brighter than the rest"
    );
}

#[test]
fn the_dialog_roles_supply_only_the_background_their_layer_needs() {
    // `Dialog::render` patches `DialogRail`, `DialogIcon`, `DialogBody`, and
    // `DialogQuestion` over `Surface`, and patches `DialogChoice`,
    // `DialogDefaultChoice`, and `DialogFocusedChoice` over `DialogFooter`. A
    // role that needs no different background stays `None`, so the layer
    // below it shows through instead of a role silently owning a background
    // that another role already supplies.
    let theme = theme();
    let footer = theme.style(ThemeRole::DialogFooter);
    let surface = theme.style(ThemeRole::Surface);
    assert_ne!(
        footer.bg, surface.bg,
        "the footer band separates from the popup surface"
    );

    for role in [ThemeRole::DialogChoice, ThemeRole::DialogDefaultChoice] {
        assert_eq!(
            theme.style(role).bg,
            None,
            "{role:?} inherits the footer band background instead of owning one"
        );
    }

    let focused = theme.style(ThemeRole::DialogFocusedChoice);
    assert!(
        focused.bg.is_some() && focused.bg != footer.bg,
        "the focused chip fills with its own accent, distinct from the footer band"
    );
    assert_ne!(
        focused.fg,
        theme.style(ThemeRole::DialogChoice).fg,
        "the filled chip's dark foreground differs from an unfocused chip"
    );
}

#[test]
fn the_default_dialog_choice_stays_distinguishable_from_a_plain_choice_and_the_focused_chip() {
    let theme = theme();
    let choice = theme.style(ThemeRole::DialogChoice);
    let default_choice = theme.style(ThemeRole::DialogDefaultChoice);
    let focused = theme.style(ThemeRole::DialogFocusedChoice);
    assert_ne!(
        default_choice.fg, choice.fg,
        "the safe default reads brighter than a plain unfocused choice"
    );
    assert_ne!(
        default_choice, focused,
        "the unfocused default never paints like the focused chip"
    );
}
