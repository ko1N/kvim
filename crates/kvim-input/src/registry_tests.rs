use kvim_keymap::KeySequence;

use super::{
    Binding, BindingScope, Command, CommandGroup, Key, KeyCode, Mode, PENDING_KEYS_MAX, Registry,
    RegistryError, WhichKeyTarget, ch, ctrl, ctrl_alt, leader,
};

/// Builds the sequence of a rejection case.
fn sequence(keys: &[Key]) -> KeySequence {
    KeySequence::new(keys, 4).expect("the test sequence is bounded")
}

/// The scope of Normal mode, which most rejection cases use.
const NORMAL_SCOPE: BindingScope = BindingScope::Mode(Mode::Normal);

fn binding(mode: Mode, keys: &[Key], command: Command) -> Binding {
    Binding::surface(BindingScope::Mode(mode), keys, command)
}

#[test]
fn the_first_release_table_validates() {
    let registry = Registry::first_release();
    assert!(
        registry.bindings(Mode::Normal).count() > 40,
        "the Normal table holds the first-release binding set"
    );
}

#[test]
fn the_registry_rejects_every_invalid_construction() {
    let cases: [(&str, Vec<Binding>, RegistryError); 4] = [
        (
            "an empty sequence",
            vec![binding(Mode::Normal, &[], Command::Undo)],
            RegistryError::EmptySequence {
                scope: NORMAL_SCOPE,
                command: Command::Undo,
            },
        ),
        (
            "a sequence above the pending-key maximum",
            vec![binding(
                Mode::Normal,
                &[ch('a'), ch('b'), ch('c'), ch('d'), ch('e')],
                Command::Undo,
            )],
            RegistryError::SequenceTooLong {
                scope: NORMAL_SCOPE,
                command: Command::Undo,
                keys: 5,
                keys_max: 4,
            },
        ),
        (
            "a duplicate sequence inside one mode",
            vec![
                binding(Mode::Normal, &[ch('u')], Command::Undo),
                binding(Mode::Normal, &[ch('u')], Command::Redo),
            ],
            RegistryError::DuplicateSequence {
                scope: NORMAL_SCOPE,
                keys: sequence(&[ch('u')]),
                first: Command::Undo,
                second: Command::Redo,
            },
        ),
        (
            "an ambiguous prefix pair",
            vec![
                binding(Mode::Normal, &[ch('g')], Command::Undo),
                binding(Mode::Normal, &[ch('g'), ch('g')], Command::MoveFirstLine),
            ],
            RegistryError::AmbiguousPrefix {
                scope: NORMAL_SCOPE,
                prefix: sequence(&[ch('g')]),
                prefix_command: Command::Undo,
                longer: sequence(&[ch('g'), ch('g')]),
                longer_command: Command::MoveFirstLine,
            },
        ),
    ];
    for (name, bindings, expected) in cases {
        let rejection = Registry::from_bindings(&bindings, 4)
            .err()
            .unwrap_or_else(|| panic!("{name} must be rejected"));
        assert_eq!(rejection, expected, "{name} must be rejected");
    }
}

#[test]
fn an_ambiguous_prefix_is_rejected_in_either_declaration_order() {
    let bindings = vec![
        binding(Mode::Normal, &[ch('g'), ch('g')], Command::MoveFirstLine),
        binding(Mode::Normal, &[ch('g')], Command::Undo),
    ];
    assert!(matches!(
        Registry::from_bindings(&bindings, 4),
        Err(RegistryError::AmbiguousPrefix { .. })
    ));
}

#[test]
fn one_sequence_reaches_different_commands_in_different_modes() {
    let registry = Registry::first_release();
    let keys = [ch('d')];
    assert_eq!(
        registry.command(Mode::Normal, &keys),
        Some(Command::DeleteOverMotion)
    );
    for mode in [Mode::Visual, Mode::VisualLine, Mode::VisualBlock] {
        assert_eq!(
            registry.command(mode, &keys),
            Some(Command::DeleteSelection),
            "{mode} deletes the selection"
        );
    }
}

#[test]
fn the_arrow_keys_and_the_word_chords_reach_the_motions_in_every_mode() {
    let registry = Registry::first_release();
    let cases = [
        (Key::plain(KeyCode::Left), Command::MoveLeft),
        (Key::plain(KeyCode::Down), Command::MoveDown),
        (Key::plain(KeyCode::Up), Command::MoveUp),
        (Key::plain(KeyCode::Right), Command::MoveRight),
        (Key::ctrl(KeyCode::Left), Command::MovePreviousWordStart),
        (Key::ctrl(KeyCode::Right), Command::MoveNextWordStart),
    ];
    for mode in Mode::ALL {
        for (key, expected) in cases {
            assert_eq!(
                registry.command(mode, &[key]),
                Some(expected),
                "{mode} `{}` must reach `{expected}`",
                key.label()
            );
        }
    }
    // The letter motions stay out of Insert mode, where a letter is buffer
    // text. Only the arrow keys and the word chords move the cursor there.
    for letter in ['h', 'j', 'k', 'l', 'w', 'b'] {
        assert_eq!(
            registry.command(Mode::Insert, &[ch(letter)]),
            None,
            "`{letter}` is buffer text in Insert mode"
        );
    }
}

#[test]
fn the_picker_arrow_keys_and_control_chords_select_results() {
    let registry = Registry::first_release();
    for (key, expected) in [
        (Key::plain(KeyCode::Down), Command::PickerSelectNext),
        (Key::plain(KeyCode::Up), Command::PickerSelectPrevious),
        (ctrl('j'), Command::PickerSelectNext),
        (ctrl('k'), Command::PickerSelectPrevious),
        (ctrl('d'), Command::PickerSelectPageNext),
        (ctrl('u'), Command::PickerSelectPagePrevious),
    ] {
        assert_eq!(
            registry.command(BindingScope::Picker, &[key]),
            Some(expected),
            "picker `{}` must reach `{expected}`",
            key.label()
        );
    }
}

#[test]
fn the_operator_grammar_stays_out_of_the_registry() {
    let registry = Registry::first_release();
    for (operator, command) in [
        ('d', Command::DeleteOverMotion),
        ('c', Command::ChangeOverMotion),
        ('y', Command::YankOverMotion),
    ] {
        assert_eq!(
            registry.command(Mode::Normal, &[ch(operator)]),
            Some(command)
        );
        assert!(
            !registry.has_longer_sequence(Mode::Normal, &[ch(operator)]),
            "the editor operator-pending state consumes the motion after `{operator}`"
        );
    }
}

#[test]
fn the_open_and_the_close_delimiter_name_one_text_object() {
    let registry = Registry::first_release();
    let pairs = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];
    for scope in [
        BindingScope::OperatorPending,
        BindingScope::Mode(Mode::Visual),
    ] {
        for (open, close) in pairs {
            for prefix in ['i', 'a'] {
                let opened = registry.command(scope, &[ch(prefix), ch(open)]);
                assert!(
                    opened.is_some(),
                    "{scope} `{prefix}{open}` must reach a text object"
                );
                assert_eq!(
                    opened,
                    registry.command(scope, &[ch(prefix), ch(close)]),
                    "{scope} `{prefix}{open}` and `{prefix}{close}` name one object"
                );
            }
        }
    }
}

#[test]
fn an_insert_key_starts_a_text_object_while_an_operator_waits() {
    let registry = Registry::first_release();
    assert_eq!(
        registry.command(Mode::Normal, &[ch('i')]),
        Some(Command::InsertBeforeCursor)
    );
    assert_eq!(
        registry.command(BindingScope::OperatorPending, &[ch('i')]),
        None,
        "`i` alone names no command while an operator waits"
    );
    assert!(registry.has_longer_sequence(BindingScope::OperatorPending, &[ch('i')]));
    assert_eq!(
        registry.command(BindingScope::OperatorPending, &[ch('i'), ch('w')]),
        Some(Command::SelectInnerWord)
    );
    // A repeated operator key still means linewise.
    assert_eq!(
        registry.command(BindingScope::OperatorPending, &[ch('d')]),
        Some(Command::DeleteOverMotion)
    );
}

#[test]
fn which_key_rows_list_one_level_of_next_keys() {
    let registry = Registry::first_release();
    let listed = |prefix: &[Key]| {
        registry
            .rows_for_prefix(Mode::Normal, prefix)
            .iter()
            .map(|row| (row.key_label().to_string(), row.target))
            .collect::<Vec<_>>()
    };

    // The leader prefix reaches `Space f b`, `Space f f`, and `Space f /`
    // through one next key, so that key carries a group marker.
    assert!(listed(&[leader()]).contains(&("f".to_owned(), WhichKeyTarget::Group { commands: 3 })));
    // `Space c` reaches exactly one command, so the row names it.
    assert!(listed(&[leader()]).contains(&(
        "c".to_owned(),
        WhichKeyTarget::Command(Command::ToggleFormatOnSave)
    )));
    assert!(
        listed(&[leader()])
            .iter()
            .all(|(key, _)| !key.contains(' ')),
        "a row names one key, never a sequence"
    );

    // One key further, the group opens into its own single keys.
    assert_eq!(
        listed(&[leader(), ch('f')]),
        vec![
            (
                "/".to_owned(),
                WhichKeyTarget::Command(Command::OpenRipgrepPicker)
            ),
            (
                "b".to_owned(),
                WhichKeyTarget::Command(Command::OpenBufferPicker)
            ),
            (
                "f".to_owned(),
                WhichKeyTarget::Command(Command::OpenFilePicker)
            ),
        ]
    );
}

#[test]
fn a_group_row_carries_the_group_that_every_command_behind_it_shares() {
    let registry = Registry::first_release();
    let rows = registry.rows_for_prefix(Mode::Normal, &[leader()]);
    let group_of = |label: &str| {
        rows.iter()
            .find(|row| row.key_label().to_string() == label)
            .map(|row| row.group)
    };
    // Every picker behind `Space f` opens a file or a buffer, so the row
    // keeps that one group.
    assert_eq!(group_of("f"), Some(CommandGroup::Buffer));
    assert_eq!(group_of("c"), Some(CommandGroup::Code));
    assert_eq!(group_of("q"), Some(CommandGroup::Buffer));
}

#[test]
fn the_leader_write_group_reaches_the_three_write_commands() {
    let registry = Registry::first_release();
    for (key, expected) in [
        (ch('a'), Command::SaveAllBuffers),
        (ch('w'), Command::SaveBuffer),
        (ch('q'), Command::SaveBufferAndClose),
    ] {
        for scope in [BindingScope::Mode(Mode::Normal), BindingScope::Sidebar] {
            assert_eq!(
                registry.command(scope, &[leader(), ch('w'), key]),
                Some(expected),
                "{scope} `Space w {}` must reach `{expected}`",
                key.label()
            );
        }
    }
    let rows = registry.rows_for_prefix(Mode::Normal, &[leader()]);
    let write_row = rows
        .iter()
        .find(|row| row.key_label().to_string() == "w")
        .expect("the leader reaches the write group");
    assert_eq!(write_row.target, WhichKeyTarget::Group { commands: 3 });
}

#[test]
fn the_close_keys_close_the_buffer_and_the_unload_key_stays() {
    let registry = Registry::first_release();
    assert_eq!(
        registry.command(Mode::Normal, &[leader(), ch('q')]),
        Some(Command::CloseBuffer)
    );
    // `<C-q>` stays reachable while the user types, so every mode carries it.
    for mode in Mode::ALL {
        assert_eq!(
            registry.command(mode, &[ctrl('q')]),
            Some(Command::CloseBuffer),
            "{mode} `<C-q>` must close the buffer"
        );
    }
    assert_eq!(
        registry.command(Mode::Normal, &[leader(), ch('x')]),
        Some(Command::UnloadBuffer),
        "`Space x` keeps the unload command that leaves the editor open"
    );
}

#[test]
fn a_group_row_names_the_number_of_commands_behind_it() {
    assert_eq!(
        WhichKeyTarget::Group { commands: 3 }.to_string(),
        "+3 commands"
    );
}

#[test]
fn the_visual_modes_switch_between_each_other_and_back_to_normal() {
    let registry = Registry::first_release();
    let control_v = ctrl('v');
    let cases = [
        (Mode::Visual, ch('V'), Command::EnterVisualLine),
        (Mode::Visual, control_v, Command::EnterVisualBlock),
        (Mode::Visual, ch('v'), Command::ReturnToNormal),
        (Mode::VisualLine, ch('v'), Command::EnterVisual),
        (Mode::VisualLine, control_v, Command::EnterVisualBlock),
        (Mode::VisualLine, ch('V'), Command::ReturnToNormal),
        (Mode::VisualBlock, ch('v'), Command::EnterVisual),
        (Mode::VisualBlock, ch('V'), Command::EnterVisualLine),
        (Mode::VisualBlock, control_v, Command::ReturnToNormal),
    ];
    for (mode, key, expected) in cases {
        assert_eq!(
            registry.command(mode, &[key]),
            Some(expected),
            "{mode} `{key:?}` must reach `{expected}`"
        );
    }
}

#[test]
fn ctrl_c_returns_every_non_normal_mode_to_normal_mode() {
    let registry = Registry::first_release();
    for mode in [
        Mode::Insert,
        Mode::Visual,
        Mode::VisualLine,
        Mode::VisualBlock,
    ] {
        assert_eq!(
            registry.command(mode, &[ctrl('c')]),
            Some(Command::ReturnToNormal),
            "the reference configuration maps `<C-c>` to `<Esc>` in {mode}"
        );
    }
}

#[test]
fn a_prefix_without_a_binding_produces_no_row() {
    let registry = Registry::first_release();
    assert!(
        registry
            .rows_for_prefix(Mode::VisualBlock, &[leader()])
            .is_empty(),
        "docs/input-actions.md keeps the leader out of Visual Block mode"
    );
}

#[test]
fn the_file_tree_arrow_keys_name_the_same_four_keys_as_hjkl() {
    let registry = Registry::first_release();
    let cases = [
        (ch('j'), Key::plain(KeyCode::Down), Command::MoveDown),
        (ch('k'), Key::plain(KeyCode::Up), Command::MoveUp),
        (
            ch('l'),
            Key::plain(KeyCode::Right),
            Command::TreeExpandEntry,
        ),
        (
            ch('h'),
            Key::plain(KeyCode::Left),
            Command::TreeCollapseEntry,
        ),
    ];
    for (letter, arrow, expected) in cases {
        assert_eq!(
            registry.command(BindingScope::Sidebar, &[letter]),
            Some(expected)
        );
        assert_eq!(
            registry.command(BindingScope::Sidebar, &[arrow]),
            Some(expected),
            "the file tree answers `{arrow:?}` exactly as it answers `{letter:?}`"
        );
    }
}

#[test]
fn the_file_tree_toggles_one_entry_with_tab() {
    let registry = Registry::first_release();
    assert_eq!(
        registry.command(BindingScope::Sidebar, &[Key::plain(KeyCode::Tab)]),
        Some(Command::TreeToggleEntry)
    );
    // The one-directional keys keep their own meanings beside the toggle.
    for (key, expected) in [
        (ch('l'), Command::TreeExpandEntry),
        (ch('h'), Command::TreeCollapseEntry),
        (Key::plain(KeyCode::Enter), Command::TreeOpenEntry),
    ] {
        assert_eq!(
            registry.command(BindingScope::Sidebar, &[key]),
            Some(expected)
        );
    }
    assert_eq!(
        registry.command(BindingScope::Sidebar, &[Key::plain(KeyCode::BackTab)]),
        None,
        "`S-Tab` stays free in the sidebar"
    );
    // An embedding host owns `Tab` in the sidebar, so the embedded profile
    // strips the toggle instead of taking the key from the host.
    let embedded = crate::BindingProfile::Embedded
        .registry()
        .expect("the embedded profile is valid");
    assert_eq!(
        embedded.command(BindingScope::Sidebar, &[Key::plain(KeyCode::Tab)]),
        None
    );
}

#[test]
fn the_file_tree_answers_the_resize_keys_itself() {
    // The sidebar owns its own scope, so a resize key that only the Normal
    // scope holds never reaches the focused file tree.
    let registry = Registry::first_release();
    let cases = [
        (ctrl_alt('h'), Command::ResizeWindowLeft),
        (ctrl_alt('j'), Command::ResizeWindowDown),
        (ctrl_alt('k'), Command::ResizeWindowUp),
        (ctrl_alt('l'), Command::ResizeWindowRight),
    ];
    for (key, expected) in cases {
        assert_eq!(
            registry.command(BindingScope::Sidebar, &[key]),
            Some(expected),
            "the file tree resizes with `{key:?}`"
        );
    }
}

#[test]
fn key_sequences_render_with_their_chord_and_name() {
    let cases = [
        (vec![ch('g'), ch('g')], "g g"),
        (vec![leader(), ch('f'), ch('/')], "Space f /"),
        (vec![ctrl('d')], "C-d"),
        (vec![ctrl_alt('h')], "C-A-h"),
        (vec![Key::ctrl(KeyCode::Enter)], "C-Enter"),
        (vec![Key::plain(KeyCode::Esc)], "Esc"),
    ];
    for (keys, expected) in cases {
        assert_eq!(sequence(&keys).to_string(), expected);
    }
}

/// One host scope for one kvim scope, the shape that `docs/embedding.md`
/// tells a host to build.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum HostScope {
    Editor(BindingScope),
}

impl std::fmt::Display for HostScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Editor(scope) => write!(f, "Editor({scope})"),
        }
    }
}

impl kvim_keymap::Scope for HostScope {
    const COUNT: usize = <BindingScope as kvim_keymap::Scope>::COUNT;
}

#[test]
fn a_host_builds_one_table_of_the_whole_preset_without_a_duplicate_sequence() {
    // This is the recipe that `docs/embedding.md` publishes, run against the
    // real preset rather than a small one. A host that collapses two scopes
    // meets `DuplicateSequence` here, because the preset reaches a different
    // command with one key in more than one table.
    let preset = Registry::first_release();
    let shared = preset.shared();
    let bindings: Vec<kvim_keymap::Binding<Command, HostScope>> = shared
        .all_bindings()
        .map(|(scope, keys, bound)| kvim_keymap::Binding {
            scope: HostScope::Editor(scope),
            keys: keys.keys().to_vec(),
            command: bound.command,
            owner: bound.owner,
        })
        .collect();
    assert!(
        !bindings.is_empty(),
        "the preset holds the bindings of the standalone editor"
    );

    let host = kvim_keymap::Registry::from_bindings(&bindings, PENDING_KEYS_MAX)
        .expect("one host scope for one kvim scope holds every binding of the preset");
    assert_eq!(
        host.len(),
        bindings.len(),
        "the host table loses no binding of the preset"
    );
}
