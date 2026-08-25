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
