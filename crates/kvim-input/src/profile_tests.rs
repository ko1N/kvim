use super::*;

fn ch(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

fn replacement(scope: BindingScope, sequence: &[Key], command: Command) -> BindingReplacement {
    BindingReplacement::new(scope, sequence, command).expect("the test sequence fits")
}

#[test]
fn standalone_profile_matches_the_first_release_registry() {
    let standalone = BindingProfile::Standalone
        .registry()
        .expect("the standalone profile is valid");
    let first_release = Registry::first_release();
    for scope in BindingScope::ALL {
        assert_eq!(
            standalone.bindings(scope).collect::<Vec<_>>(),
            first_release.bindings(scope).collect::<Vec<_>>(),
            "the standalone profile preserves {scope}",
        );
    }
}

#[test]
fn embedded_profile_leaves_host_navigation_and_review_entry_unbound() {
    let registry = BindingProfile::Embedded
        .registry()
        .expect("the embedded profile is valid");
    for scope in [
        BindingScope::Mode(Mode::Normal),
        BindingScope::Mode(Mode::Visual),
        BindingScope::Mode(Mode::VisualLine),
        BindingScope::Mode(Mode::VisualBlock),
        BindingScope::Sidebar,
    ] {
        assert_eq!(registry.command(scope, &[Key::plain(KeyCode::Tab)]), None);
        assert_eq!(
            registry.command(scope, &[Key::plain(KeyCode::BackTab)]),
            None
        );
    }
    for scope in BindingScope::ALL {
        assert!(
            registry
                .bindings(scope)
                .all(|(_, command)| command != Command::OpenReview),
            "OpenReview remains bound in {scope}",
        );
    }
}

#[test]
fn embedded_profile_keeps_insert_and_prompt_tab_bindings() {
    let registry = BindingProfile::Embedded
        .registry()
        .expect("the embedded profile is valid");
    assert_eq!(
        registry.command(Mode::Insert, &[Key::plain(KeyCode::Tab)]),
        Some(Command::InsertIndent)
    );
    assert_eq!(
        registry.command(BindingScope::Prompt, &[Key::plain(KeyCode::Tab)]),
        Some(Command::PromptCompleteNext)
    );
    assert_eq!(
        registry.command(BindingScope::Prompt, &[Key::plain(KeyCode::BackTab)]),
        Some(Command::PromptCompletePrevious)
    );
}

#[test]
fn embedded_profile_adds_conflict_free_secondary_bindings() {
    let registry = BindingProfile::Embedded
        .registry()
        .expect("the embedded profile is valid");
    assert_eq!(
        registry.command(Mode::Normal, &[ch(']'), ch('j')]),
        Some(Command::JumpForward)
    );
    assert_eq!(
        registry.command(Mode::Normal, &[ch('['), ch('j')]),
        Some(Command::JumpBack)
    );
    assert_eq!(
        registry.command(BindingScope::Review, &[ch(']'), ch('s')]),
        Some(Command::NextReviewSection)
    );
    assert_eq!(
        registry.command(BindingScope::Review, &[ch('['), ch('s')]),
        Some(Command::PreviousReviewSection)
    );
}

#[test]
fn enable_restores_embedded_defaults_by_command_identity() {
    let registry = BindingProfile::Embedded
        .with_overrides(&[BindingOverride::Enable(Command::OpenReview)])
        .expect("the override is valid");
    assert_eq!(
        registry.command(Mode::Normal, &[ch(' '), ch('g'), ch('g')]),
        Some(Command::OpenReview)
    );
    assert!(Command::ALL.contains(&Command::OpenReview));
}

#[test]
fn replacements_replace_all_command_bindings_and_accept_multiple_sequences() {
    let registry = BindingProfile::Standalone
        .with_overrides(&[
            BindingOverride::Replace(replacement(
                BindingScope::Mode(Mode::Normal),
                &[ch('x')],
                Command::Undo,
            )),
            BindingOverride::Replace(replacement(
                BindingScope::Mode(Mode::Visual),
                &[ch('X')],
                Command::Undo,
            )),
        ])
        .expect("the replacements are valid");
    assert_eq!(registry.command(Mode::Normal, &[ch('u')]), None);
    assert_eq!(
        registry.command(Mode::Normal, &[ch('x')]),
        Some(Command::Undo)
    );
    assert_eq!(
        registry.command(Mode::Visual, &[ch('X')]),
        Some(Command::Undo)
    );
}

#[test]
fn overrides_reject_conflicts_and_registry_conflicts() {
    let contradictory = BindingProfile::Standalone.with_overrides(&[
        BindingOverride::Disable(Command::Undo),
        BindingOverride::Enable(Command::Undo),
    ]);
    assert!(matches!(
        contradictory,
        Err(BindingProfileError::ContradictoryOverride {
            command: Command::Undo
        })
    ));

    let conflict = BindingProfile::Embedded.with_overrides(&[BindingOverride::Replace(
        replacement(BindingScope::Mode(Mode::Normal), &[ch('i')], Command::Undo),
    )]);
    assert!(matches!(
        conflict,
        Err(BindingProfileError::Registry(
            RegistryError::DuplicateSequence { .. }
        ))
    ));

    let prefix = BindingProfile::Embedded.with_overrides(&[BindingOverride::Replace(replacement(
        BindingScope::Mode(Mode::Normal),
        &[ch('g')],
        Command::Undo,
    ))]);
    assert!(matches!(
        prefix,
        Err(BindingProfileError::Registry(
            RegistryError::AmbiguousPrefix { .. }
        ))
    ));
}

#[test]
fn replacement_construction_rejects_unbounded_sequences_before_copying() {
    let keys = vec![ch('a'); usize::from(PENDING_KEYS_MAX) + 1];
    assert!(matches!(
        BindingReplacement::new(BindingScope::Mode(Mode::Normal), &keys, Command::Undo),
        Err(BindingReplacementError::Sequence(
            SequenceError::TooLong { .. }
        ))
    ));
}

#[test]
fn overrides_reject_lists_above_the_published_bounds() {
    let overrides = vec![BindingOverride::Disable(Command::OpenReview); BINDING_OVERRIDES_MAX + 1];
    assert!(matches!(
        BindingProfile::Standalone.with_overrides(&overrides),
        Err(BindingProfileError::TooManyOverrides { overrides }) if overrides == BINDING_OVERRIDES_MAX + 1
    ));

    let replacements = vec![
        BindingOverride::Replace(replacement(
            BindingScope::Mode(Mode::Normal),
            &[ch('z'), ch('x')],
            Command::Undo,
        ));
        BINDING_REPLACEMENTS_MAX + 1
    ];
    assert!(matches!(
        BindingProfile::Standalone.with_overrides(&replacements),
        Err(BindingProfileError::TooManyReplacements { replacements }) if replacements == BINDING_REPLACEMENTS_MAX + 1
    ));
}

#[test]
fn manifest_publishes_scope_semantics_and_command_metadata() {
    let manifest = BindingProfile::Embedded
        .manifest()
        .expect("the embedded manifest is valid");
    let indent = manifest
        .entries()
        .iter()
        .find(|entry| entry.command() == Command::InsertIndent)
        .expect("Insert indent is published");
    assert_eq!(indent.scope(), BindingScope::Mode(Mode::Insert));
    assert_eq!(indent.group(), CommandGroup::Other);
    assert_eq!(
        indent.interruption_policy(),
        BindingInterruptionPolicy::PrecedingScopeMayInterruptPrefix
    );
    assert!(
        !manifest
            .entries()
            .iter()
            .any(|entry| entry.command() == Command::OpenReview)
    );
}

#[test]
fn manifest_applies_semantic_overrides() {
    let manifest = BindingProfile::Standalone
        .manifest_with_overrides(&[BindingOverride::Disable(Command::OpenReview)])
        .expect("the override is valid");
    assert!(
        manifest
            .entries()
            .iter()
            .all(|entry| entry.command() != Command::OpenReview)
    );
}
