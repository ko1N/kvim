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
