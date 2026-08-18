//! The one icon table of the editor.
//!
//! An icon is presentation data. The file-tree table keys on a file extension
//! and on a well-known file name, and the which-key table keys on a command
//! group. Neither ever selects a parser, an indent rule, a comment token, or a
//! language server. `docs/architecture.md` records this one narrow exception to
//! the language-adapter rule.
//!
//! Every glyph needs a patched font, which the reference configuration installs.
//! A terminal without one hides the icons through [`FileTreeIcons`]. That one
//! setting covers the file tree and the which-key overlay together. Each glyph
//! occupies one terminal cell, so the rows align with icons and without them.

use kvim_input::CommandGroup;
use kvim_settings::FileTreeIcons;
use kvim_workspace::{Expansion, RowContent, TreeRow};

use super::theme::IconRole;

/// The number of cells that one icon and its gap occupy.
///
/// Every row reserves the same width while the tree shows icons, so a row
/// without an icon keeps the names of its neighbors aligned.
pub(super) const ICON_CELLS: usize = 2;

/// One icon glyph and the theme role that colors it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Icon {
    /// The glyph of the icon, which occupies one terminal cell.
    pub(super) glyph: &'static str,
    /// The role that the theme maps to a color.
    pub(super) role: IconRole,
}

/// The icon of a file that no table entry names.
const DEFAULT_FILE: Icon = Icon {
    glyph: "\u{f15b}",
    role: IconRole::Unknown,
};

/// The icon of a closed directory.
const CLOSED_DIRECTORY: Icon = Icon {
    glyph: "\u{f07b}",
    role: IconRole::Directory,
};

/// The icon of an open directory.
const OPEN_DIRECTORY: Icon = Icon {
    glyph: "\u{f07c}",
    role: IconRole::Directory,
};

/// The icons of the well-known file names.
///
/// The table comes before the extension table, so `Cargo.lock` reads as a lock
/// file and `.gitignore` reads as a Git file. It also names a file that carries
/// no extension at all, such as `LICENSE`. The comparison ignores the case of
/// ASCII letters, because a filesystem may hold either spelling.
const NAMED_FILES: &[(&str, Icon)] = &[
    (
        ".gitignore",
        Icon {
            glyph: "\u{e702}",
            role: IconRole::VersionControl,
        },
    ),
    (
        ".gitattributes",
        Icon {
            glyph: "\u{e702}",
            role: IconRole::VersionControl,
        },
    ),
    (
        ".gitmodules",
        Icon {
            glyph: "\u{e702}",
            role: IconRole::VersionControl,
        },
    ),
    (
        ".gitconfig",
        Icon {
            glyph: "\u{e702}",
            role: IconRole::VersionControl,
        },
    ),
    (
        "LICENSE",
        Icon {
            glyph: "\u{f0e3}",
            role: IconRole::Document,
        },
    ),
    (
        "LICENCE",
        Icon {
            glyph: "\u{f0e3}",
            role: IconRole::Document,
        },
    ),
    (
        "Cargo.lock",
        Icon {
            glyph: "\u{f023}",
            role: IconRole::Generated,
        },
    ),
    (
        "flake.lock",
        Icon {
            glyph: "\u{f023}",
            role: IconRole::Generated,
        },
    ),
];

/// The icons of the known file extensions.
///
/// The comparison ignores the case of ASCII letters, so `README.MD` and
/// `readme.md` reach one icon.
const EXTENSIONS: &[(&str, Icon)] = &[
    (
        "rs",
        Icon {
            glyph: "\u{e7a8}",
            role: IconRole::Code,
        },
    ),
    (
        "lua",
        Icon {
            glyph: "\u{e620}",
            role: IconRole::Code,
        },
    ),
    (
        "toml",
        Icon {
            glyph: "\u{e615}",
            role: IconRole::Configuration,
        },
    ),
    (
        "yaml",
        Icon {
            glyph: "\u{e615}",
            role: IconRole::Configuration,
        },
    ),
    (
        "yml",
        Icon {
            glyph: "\u{e615}",
            role: IconRole::Configuration,
        },
    ),
    (
        "json",
        Icon {
            glyph: "\u{e60b}",
            role: IconRole::Configuration,
        },
    ),
    (
        "nix",
        Icon {
            glyph: "\u{f313}",
            role: IconRole::Configuration,
        },
    ),
    (
        "lock",
        Icon {
            glyph: "\u{f023}",
            role: IconRole::Generated,
        },
    ),
    (
        "md",
        Icon {
            glyph: "\u{f48a}",
            role: IconRole::Document,
        },
    ),
    (
        "sh",
        Icon {
            glyph: "\u{f489}",
            role: IconRole::Script,
        },
    ),
    (
        "bash",
        Icon {
            glyph: "\u{f489}",
            role: IconRole::Script,
        },
    ),
    (
        "zsh",
        Icon {
            glyph: "\u{f489}",
            role: IconRole::Script,
        },
    ),
    (
        "fish",
        Icon {
            glyph: "\u{f489}",
            role: IconRole::Script,
        },
    ),
    (
        "png",
        Icon {
            glyph: "\u{f1c5}",
            role: IconRole::Media,
        },
    ),
    (
        "jpg",
        Icon {
            glyph: "\u{f1c5}",
            role: IconRole::Media,
        },
    ),
    (
        "jpeg",
        Icon {
            glyph: "\u{f1c5}",
            role: IconRole::Media,
        },
    ),
    (
        "gif",
        Icon {
            glyph: "\u{f1c5}",
            role: IconRole::Media,
        },
    ),
    (
        "svg",
        Icon {
            glyph: "\u{f1c5}",
            role: IconRole::Media,
        },
    ),
    (
        "webp",
        Icon {
            glyph: "\u{f1c5}",
            role: IconRole::Media,
        },
    ),
    (
        "ico",
        Icon {
            glyph: "\u{f1c5}",
            role: IconRole::Media,
        },
    ),
];

impl Icon {
    /// Returns the icon of one which-key command group.
    ///
    /// The mapping lives beside the file-tree table, because the interface
    /// layer owns every glyph and every color. `kvim-input` names the group
    /// alone. A group that the table does not name receives the default
    /// keyboard icon, which every ordinary mapping carries.
    pub(super) const fn of_group(group: CommandGroup) -> Self {
        match group {
            CommandGroup::Search => Self {
                glyph: "\u{f002}",
                role: IconRole::CommandSearch,
            },
            CommandGroup::Code => Self {
                glyph: "\u{f121}",
                role: IconRole::CommandCode,
            },
            CommandGroup::Window => Self {
                glyph: "\u{f2d0}",
                role: IconRole::CommandWindow,
            },
            CommandGroup::Buffer => Self {
                glyph: "\u{f0f6}",
                role: IconRole::CommandBuffer,
            },
            CommandGroup::Tree => Self {
                glyph: "\u{f07b}",
                role: IconRole::CommandTree,
            },
            CommandGroup::Other => Self {
                glyph: "\u{f11c}",
                role: IconRole::CommandOther,
            },
        }
    }
}

/// Returns the icon of one file name.
///
/// A well-known name wins over the extension, and an unknown name receives the
/// default icon. Both tables hold a fixed number of entries, so one lookup costs
/// a bounded number of comparisons.
pub(super) fn file_icon(name: &str) -> Icon {
    if let Some((_, icon)) = NAMED_FILES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
    {
        return *icon;
    }
    // A name that starts with a full stop and holds no other one, such as
    // `.envrc`, carries no extension.
    let extension = name
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map_or("", |(_, extension)| extension);
    EXTENSIONS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(extension))
        .map_or(DEFAULT_FILE, |(_, icon)| *icon)
}

/// Returns the icon of one directory.
pub(super) const fn directory_icon(expansion: Expansion) -> Icon {
    match expansion {
        Expansion::Collapsed => CLOSED_DIRECTORY,
        Expansion::Expanded | Expansion::Pending => OPEN_DIRECTORY,
    }
}

/// Returns the icon of one visible tree row, while the tree shows icons.
///
/// A notice row reports a bounded or a failed directory read instead of an
/// entry, so it carries no icon and keeps the reserved cells blank.
pub(super) fn row_icon(row: &TreeRow, icons: FileTreeIcons) -> Option<Icon> {
    if icons == FileTreeIcons::Hidden {
        return None;
    }
    match &row.content {
        RowContent::File { name, .. } => Some(file_icon(name)),
        RowContent::Directory { expansion, .. } => Some(directory_icon(*expansion)),
        RowContent::Notice(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use kvim_input::CommandGroup;

    use crate::cells::text_cells;

    use super::Icon;

    /// Every group that the icon table names.
    const NAMED_GROUPS: &[CommandGroup] = &[
        CommandGroup::Search,
        CommandGroup::Code,
        CommandGroup::Window,
        CommandGroup::Buffer,
        CommandGroup::Tree,
    ];

    #[test]
    fn every_named_command_group_reaches_its_own_icon() {
        let glyphs = NAMED_GROUPS
            .iter()
            .map(|group| Icon::of_group(*group).glyph)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            glyphs.len(),
            NAMED_GROUPS.len(),
            "a reader tells the groups apart by the glyph"
        );
        let roles = NAMED_GROUPS
            .iter()
            .map(|group| Icon::of_group(*group).role)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            roles.len(),
            NAMED_GROUPS.len(),
            "a reader tells the groups apart by the color"
        );
    }

    #[test]
    fn a_command_without_a_named_group_reaches_the_default_icon() {
        let default = Icon::of_group(CommandGroup::Other);
        assert_eq!(
            default,
            Icon::of_group(CommandGroup::default()),
            "the default group and the default icon name the same rows"
        );
        assert!(
            NAMED_GROUPS
                .iter()
                .all(|group| Icon::of_group(*group) != default),
            "no named group shares the default icon"
        );
    }

    #[test]
    fn every_group_icon_occupies_one_terminal_cell() {
        // The columns of the overlay reserve the same width for every icon, so
        // a wider glyph would move one label out of its column.
        for group in NAMED_GROUPS.iter().copied().chain([CommandGroup::Other]) {
            let glyph = Icon::of_group(group).glyph;
            assert_eq!(
                text_cells(glyph),
                1,
                "{group:?} carries a glyph of one cell"
            );
        }
    }
}
