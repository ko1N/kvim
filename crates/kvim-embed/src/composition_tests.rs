use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use kvim_input::{BindingProfile, BindingScope, ContextGeneration, KeyCode, Mode};
use kvim_keymap::{CommandMetadata, Dispatch, Input, Resolver, SEQUENCE_KEYS_MAX};

use super::*;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum HostCommand {
    Leave,
    Sessions,
    Send,
    ReviewComment,
    EditorId,
}

impl fmt::Display for HostCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for HostCommand {
    fn id(&self) -> &str {
        match self {
            Self::Leave => "host-leave",
            Self::Sessions => "host-sessions",
            Self::Send => "chat-send",
            Self::ReviewComment => "review-comment",
            Self::EditorId => "move-down",
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Leave => "Leave the surface",
            Self::Sessions => "Open sessions",
            Self::Send => "Send message",
            Self::ReviewComment => "Add review comment",
            Self::EditorId => "Host command sharing an editor identifier",
        }
    }
}

impl WorktreeHostCommand for HostCommand {
    fn owner_label(&self) -> &str {
        "Keel"
    }

    fn group_label(&self) -> &str {
        match self {
            Self::Leave | Self::Sessions => "workspace",
            Self::Send => "chat",
            Self::ReviewComment => "review",
            Self::EditorId => "workspace",
        }
    }
}

fn key(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

fn host_bindings() -> Vec<WorktreeHostBinding<HostCommand>> {
    vec![
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Global,
            &[Key::ctrl(KeyCode::Char('e'))],
            HostCommand::Leave,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
            &[key(' '), key('s')],
            HostCommand::Sessions,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
            &[Key::plain(KeyCode::Enter)],
            HostCommand::Send,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Editor),
            &[key(' '), key('s')],
            HostCommand::Sessions,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Review),
            &[key(' '), key('s')],
            HostCommand::Sessions,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Review),
            &[key('c')],
            HostCommand::ReviewComment,
        )
        .unwrap(),
    ]
}

fn model() -> WorktreeBindingModel<HostCommand> {
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    WorktreeBindingModel::compose(&manifest, &host_bindings()).unwrap()
}

#[test]
fn focus_projection_selects_host_and_only_the_relevant_surface_groups() {
    let model = model();
    let registry = model.registry();
    let leader = key(' ');

    let chat = WorktreeBindingModel::<HostCommand>::chat_context(ContextGeneration::FIRST);
    let mut chat_resolver = Resolver::new(Arc::clone(&registry), 4, Duration::ZERO);
    assert_eq!(
        chat_resolver.dispatch(&chat, Input::Key(leader), Some(Duration::ZERO)),
        Dispatch::Pending
    );
    let chat_hints = chat_resolver.which_key(Duration::ZERO).unwrap().hints();
    assert!(chat_hints.iter().any(|hint| {
        hint.hint()
            .commands()
            .contains(&WorktreeMergedCommand::Host(HostCommand::Sessions))
    }));
    assert!(!chat_hints.iter().any(|hint| {
        hint.hint()
            .commands()
            .iter()
            .any(|command| matches!(command, WorktreeMergedCommand::Editor { .. }))
    }));

    let normal = InputContextSnapshot::idle(BindingScope::Mode(Mode::Normal));
    let editor = WorktreeBindingModel::<HostCommand>::editor_snapshot_context(normal, None);
    let mut editor_resolver = Resolver::new(Arc::clone(&registry), 4, Duration::ZERO);
    assert_eq!(
        editor_resolver.dispatch(&editor, Input::Key(leader), Some(Duration::ZERO)),
        Dispatch::Pending
    );
    let editor_hints = editor_resolver.which_key(Duration::ZERO).unwrap().hints();
    assert!(editor_hints.iter().any(|hint| {
        hint.hint()
            .commands()
            .contains(&WorktreeMergedCommand::Host(HostCommand::Sessions))
    }));
    assert!(editor_hints.iter().any(|hint| {
        hint.hint()
            .commands()
            .iter()
            .any(|command| matches!(command, WorktreeMergedCommand::Editor { .. }))
    }));

    let review = WorktreeBindingModel::<HostCommand>::review_context(ContextGeneration::FIRST);
    let idle = Resolver::new(Arc::clone(&registry), 4, Duration::ZERO).idle_which_key(&review);
    assert!(idle.iter().any(|hint| {
        hint.hint()
            .commands()
            .contains(&WorktreeMergedCommand::Host(HostCommand::ReviewComment))
    }));
    assert!(!idle.iter().any(|hint| {
        hint.hint().commands().iter().any(|command| {
            matches!(command, WorktreeMergedCommand::Editor { group, .. } if *group != CommandGroup::Review)
        })
    }));
}

#[test]
fn editor_projection_preserves_normal_insert_prompt_and_sidebar_contexts() {
    let model = model();
    let registry = model.registry();
    for scope in [
        BindingScope::Mode(Mode::Normal),
        BindingScope::Mode(Mode::Insert),
        BindingScope::Prompt,
        BindingScope::Sidebar,
    ] {
        let source = InputContextSnapshot {
            scope,
            phases: SemanticPhases::IDLE,
            text_fallback: scope.text_fallback(),
            unbound_input: scope.unbound_input(),
            generation: ContextGeneration::FIRST,
        };
        let context = WorktreeBindingModel::<HostCommand>::editor_snapshot_context(source, None);
        assert_eq!(context.global, Some(WorktreeMergedScope::HostGlobal));
        assert_eq!(context.focus.scope, WorktreeMergedScope::Editor(scope));
        assert_eq!(context.focus.text_fallback, source.text_fallback);
        let idle = Resolver::new(Arc::clone(&registry), 4, Duration::ZERO).idle_which_key(&context);
        assert!(
            idle.len() <= BINDINGS_MAX,
            "the validated registry bounds every hint list"
        );
    }
}

#[test]
fn which_key_publishes_owner_groups_continuations_and_interruptions() {
    let model = model();
    let registry = model.registry();
    let context = WorktreeBindingModel::<HostCommand>::editor_snapshot_context(
        InputContextSnapshot::idle(BindingScope::Mode(Mode::Normal)),
        None,
    );
    let mut resolver = Resolver::new(registry, 4, Duration::ZERO);
    assert_eq!(
        resolver.dispatch(&context, Input::Key(key(' ')), Some(Duration::ZERO)),
        Dispatch::Pending
    );
    let view = resolver.which_key(Duration::ZERO).unwrap();
    let hints = view.hints();
    assert_eq!(
        hints
            .iter()
            .filter(|hint| hint.hint().key() == key('s'))
            .count(),
        1,
        "one effective editor table produces one menu entry for the host leader"
    );
    let sessions = hints
        .iter()
        .flat_map(|hint| hint.hint().commands())
        .find(|command| **command == WorktreeMergedCommand::Host(HostCommand::Sessions))
        .unwrap();
    assert_eq!(sessions.owner_label(), "Keel");
    assert_eq!(sessions.group_label(), "workspace");
    assert!(hints.len() <= BINDINGS_MAX);

    let interruptions = view.interruptions();
    assert_eq!(interruptions.len(), 1);
    assert_eq!(interruptions[0].hint().key(), Key::ctrl(KeyCode::Char('e')));
    assert_eq!(
        resolver.dispatch(
            &context,
            Input::Key(Key::ctrl(KeyCode::Char('e'))),
            Some(Duration::ZERO),
        ),
        Dispatch::Interrupted {
            owner: CommandOwner::Host,
            command: WorktreeMergedCommand::Host(HostCommand::Leave),
        }
    );
}

#[test]
fn composition_rejects_collisions_deterministically() {
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let first = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
        &[key('x')],
        HostCommand::Send,
    )
    .unwrap();
    let second = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
        &[key('x')],
        HostCommand::Sessions,
    )
    .unwrap();
    let left = WorktreeBindingModel::compose(&manifest, &[first.clone(), second.clone()])
        .expect_err("the collision is unapproved");
    let right = WorktreeBindingModel::compose(&manifest, &[second, first])
        .expect_err("registration order cannot select a winner");
    assert_eq!(left, right);
    assert!(matches!(
        left,
        WorktreeBindingCompositionError::UnapprovedConflict {
            kind: WorktreeBindingConflictKind::DuplicateSequence,
            ..
        }
    ));
}

#[test]
fn host_collection_and_input_bounds_fail_before_composition() {
    assert_eq!(
        WorktreeHostBinding::new(WorktreeHostBindingLayer::Global, &[], HostCommand::Leave,),
        Err(WorktreeHostBindingError::EmptySequence)
    );
    let long = vec![key('x'); usize::from(PENDING_KEYS_MAX) + 1];
    assert!(matches!(
        WorktreeHostBinding::new(WorktreeHostBindingLayer::Global, &long, HostCommand::Leave,),
        Err(WorktreeHostBindingError::SequenceTooLong { .. })
    ));
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let binding = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Global,
        &[key('x')],
        HostCommand::Leave,
    )
    .unwrap();
    let oversized = vec![binding; WORKTREE_HOST_BINDINGS_MAX + 1];
    assert!(matches!(
        WorktreeBindingModel::compose(&manifest, &oversized),
        Err(WorktreeBindingCompositionError::TooManyHostBindings { .. })
    ));
    assert!(usize::from(PENDING_KEYS_MAX) <= usize::from(SEQUENCE_KEYS_MAX));
}

#[test]
fn picker_overlay_precedes_prompt_and_ordinary_contexts_have_no_overlay() {
    let prompt = InputContextSnapshot {
        scope: BindingScope::Prompt,
        phases: SemanticPhases::IDLE,
        text_fallback: BindingScope::Prompt.text_fallback(),
        unbound_input: BindingScope::Prompt.unbound_input(),
        generation: ContextGeneration::FIRST,
    };
    let picker = WorktreeBindingModel::<HostCommand>::editor_snapshot_context(
        prompt,
        Some(BindingScope::Picker),
    );
    assert_eq!(
        picker.overlay,
        Some(WorktreeMergedScope::Editor(BindingScope::Picker))
    );
    assert_eq!(
        picker.focus.scope,
        WorktreeMergedScope::Editor(BindingScope::Prompt)
    );

    let normal = WorktreeBindingModel::<HostCommand>::editor_snapshot_context(
        InputContextSnapshot::idle(BindingScope::Mode(Mode::Normal)),
        None,
    );
    assert_eq!(normal.overlay, None);
}

#[test]
fn editor_host_leader_does_not_enter_literal_or_internal_pending_contexts() {
    let model = model();
    let registry = model.registry();
    let leader = [key(' '), key('s')];
    for scope in [
        BindingScope::Mode(Mode::Insert),
        BindingScope::Picker,
        BindingScope::OperatorPending,
        BindingScope::Prompt,
        BindingScope::Confirmation,
        BindingScope::RegisterSelection,
    ] {
        assert_eq!(
            registry.command(WorktreeMergedScope::Editor(scope), &leader),
            None,
            "host editor leader must not create a prefix in {scope:?}"
        );
    }

    assert_eq!(
        registry.command(
            WorktreeMergedScope::Editor(BindingScope::Mode(Mode::Insert)),
            &[Key::plain(KeyCode::Tab)],
        ),
        Some(WorktreeMergedCommand::Editor {
            command: Command::InsertIndent,
            group: Command::InsertIndent.group(),
        })
    );
    assert_eq!(
        registry.command(
            WorktreeMergedScope::Editor(BindingScope::Prompt),
            &[Key::plain(KeyCode::Tab)],
        ),
        Some(WorktreeMergedCommand::Editor {
            command: Command::PromptCompleteNext,
            group: Command::PromptCompleteNext.group(),
        })
    );
}

#[test]
fn addressed_overrides_select_host_and_editor_winners() {
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let host_x = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
        &[key('x')],
        HostCommand::Send,
    )
    .unwrap();
    let other_x = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
        &[key('x')],
        HostCommand::Sessions,
    )
    .unwrap();
    let host_override = WorktreeBindingOverride::new(
        WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
        &[key('x')],
        WorktreeAddressedCommand::Host(HostCommand::Sessions),
    )
    .unwrap();
    let model = WorktreeBindingModel::compose_with_overrides(
        &manifest,
        &[host_x, other_x],
        &[host_override],
    )
    .unwrap();
    assert_eq!(
        model.registry().command(
            WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
            &[key('x')]
        ),
        Some(WorktreeMergedCommand::Host(HostCommand::Sessions))
    );

    let editor_host = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Editor),
        &[key('j')],
        HostCommand::Sessions,
    )
    .unwrap();
    let editor_overrides: Vec<_> = [
        BindingScope::Mode(Mode::Normal),
        BindingScope::Mode(Mode::Visual),
        BindingScope::Mode(Mode::VisualLine),
        BindingScope::Mode(Mode::VisualBlock),
        BindingScope::Sidebar,
    ]
    .into_iter()
    .map(|scope| {
        WorktreeBindingOverride::new(
            WorktreeMergedScope::Editor(scope),
            &[key('j')],
            WorktreeAddressedCommand::Editor(Command::MoveDown),
        )
        .unwrap()
    })
    .collect();
    let model =
        WorktreeBindingModel::compose_with_overrides(&manifest, &[editor_host], &editor_overrides)
            .unwrap();
    assert!(matches!(
        model.registry().command(
            WorktreeMergedScope::Editor(BindingScope::Mode(Mode::Normal)),
            &[key('j')],
        ),
        Some(WorktreeMergedCommand::Editor {
            command: Command::MoveDown,
            ..
        })
    ));

    let colliding_id = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Editor),
        &[key('j')],
        HostCommand::EditorId,
    )
    .unwrap();
    let host_overrides: Vec<_> = [
        BindingScope::Mode(Mode::Normal),
        BindingScope::Mode(Mode::Visual),
        BindingScope::Mode(Mode::VisualLine),
        BindingScope::Mode(Mode::VisualBlock),
        BindingScope::Sidebar,
    ]
    .into_iter()
    .map(|scope| {
        WorktreeBindingOverride::new(
            WorktreeMergedScope::Editor(scope),
            &[key('j')],
            WorktreeAddressedCommand::Host(HostCommand::EditorId),
        )
        .unwrap()
    })
    .collect();
    let model =
        WorktreeBindingModel::compose_with_overrides(&manifest, &[colliding_id], &host_overrides)
            .unwrap();
    assert_eq!(
        model.registry().command(
            WorktreeMergedScope::Editor(BindingScope::Mode(Mode::Normal)),
            &[key('j')],
        ),
        Some(WorktreeMergedCommand::Host(HostCommand::EditorId)),
        "typed ownership distinguishes equal host and editor string identifiers"
    );
}

#[test]
fn overrides_reject_stale_ambiguous_uncovered_and_prefix_conflicts() {
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let binding = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
        &[key('x')],
        HostCommand::Send,
    )
    .unwrap();
    let stale = WorktreeBindingOverride::new(
        WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
        &[key('z')],
        WorktreeAddressedCommand::Host(HostCommand::Send),
    )
    .unwrap();
    assert_eq!(
        WorktreeBindingModel::compose_with_overrides(
            &manifest,
            std::slice::from_ref(&binding),
            &[stale],
        )
        .unwrap_err(),
        WorktreeBindingCompositionError::OverrideTargetNotFound
    );

    let no_conflict = WorktreeBindingOverride::new(
        WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
        &[key('x')],
        WorktreeAddressedCommand::Host(HostCommand::Send),
    )
    .unwrap();
    assert_eq!(
        WorktreeBindingModel::compose_with_overrides(
            &manifest,
            std::slice::from_ref(&binding),
            &[no_conflict],
        )
        .unwrap_err(),
        WorktreeBindingCompositionError::OverrideHasNoConflict
    );

    let duplicate = [binding.clone(), binding.clone()];
    let ambiguous = WorktreeBindingOverride::new(
        WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
        &[key('x')],
        WorktreeAddressedCommand::Host(HostCommand::Send),
    )
    .unwrap();
    assert_eq!(
        WorktreeBindingModel::compose_with_overrides(&manifest, &duplicate, &[ambiguous])
            .unwrap_err(),
        WorktreeBindingCompositionError::OverrideTargetAmbiguous
    );

    let second = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
        &[key('x')],
        HostCommand::Sessions,
    )
    .unwrap();
    assert!(matches!(
        WorktreeBindingModel::compose_with_overrides(&manifest, &[binding.clone(), second], &[]),
        Err(WorktreeBindingCompositionError::UnapprovedConflict {
            kind: WorktreeBindingConflictKind::DuplicateSequence,
            ..
        })
    ));

    let longer = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
        &[key('x'), key('y')],
        HostCommand::Sessions,
    )
    .unwrap();
    assert!(matches!(
        WorktreeBindingModel::compose_with_overrides(
            &manifest,
            &[binding.clone(), longer.clone()],
            &[]
        ),
        Err(WorktreeBindingCompositionError::UnapprovedConflict {
            kind: WorktreeBindingConflictKind::StrictPrefix,
            ..
        })
    ));
    let longer_wins = WorktreeBindingOverride::new(
        WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
        &[key('x'), key('y')],
        WorktreeAddressedCommand::Host(HostCommand::Sessions),
    )
    .unwrap();
    let model =
        WorktreeBindingModel::compose_with_overrides(&manifest, &[binding, longer], &[longer_wins])
            .unwrap();
    assert_eq!(
        model.registry().command(
            WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
            &[key('x'), key('y')]
        ),
        Some(WorktreeMergedCommand::Host(HostCommand::Sessions))
    );
}

#[test]
fn one_winner_can_remove_multiple_conflicting_losers() {
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let winner = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
        &[key('x')],
        HostCommand::Send,
    )
    .unwrap();
    let duplicate = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
        &[key('x')],
        HostCommand::Sessions,
    )
    .unwrap();
    let extension = WorktreeHostBinding::new(
        WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(WorktreeHostScope::CHAT)),
        &[key('x'), key('y')],
        HostCommand::ReviewComment,
    )
    .unwrap();
    let selected = WorktreeBindingOverride::new(
        WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
        &[key('x')],
        WorktreeAddressedCommand::Host(HostCommand::Send),
    )
    .unwrap();

    let model = WorktreeBindingModel::compose_with_overrides(
        &manifest,
        &[winner, duplicate, extension],
        &[selected],
    )
    .unwrap();
    assert_eq!(
        model.registry().command(
            WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
            &[key('x')]
        ),
        Some(WorktreeMergedCommand::Host(HostCommand::Send))
    );
}

#[test]
fn reserved_escape_requires_one_complete_host_global_command() {
    let host = BoundCommand {
        command: WorktreeMergedCommand::Host(HostCommand::Leave),
        owner: CommandOwner::Host,
    };
    assert_eq!(validate_reserved_escape(Some(host), false), Ok(()));
    assert_eq!(
        validate_reserved_escape::<HostCommand>(None, false),
        Err(WorktreeBindingContextError::ReservedEscapeAbsent)
    );
    assert_eq!(
        validate_reserved_escape::<HostCommand>(None, true),
        Err(WorktreeBindingContextError::ReservedEscapePending)
    );

    let editor: BoundCommand<WorktreeMergedCommand<HostCommand>> = BoundCommand {
        command: WorktreeMergedCommand::Editor {
            command: Command::MoveDown,
            group: CommandGroup::Other,
        },
        owner: CommandOwner::Surface,
    };
    assert_eq!(
        validate_reserved_escape(Some(editor), false),
        Err(WorktreeBindingContextError::ReservedEscapeEditorOwned)
    );
    assert_eq!(
        validate_reserved_escape(Some(host), true),
        Err(WorktreeBindingContextError::ReservedEscapeAmbiguous)
    );

    let mismatched_owner = BoundCommand {
        command: WorktreeMergedCommand::Host(HostCommand::Leave),
        owner: CommandOwner::Surface,
    };
    assert_eq!(
        validate_reserved_escape(Some(mismatched_owner), false),
        Err(WorktreeBindingContextError::ReservedEscapeAmbiguous)
    );
}

#[test]
fn override_bounds_fail_before_composition() {
    assert_eq!(
        WorktreeBindingOverride::<HostCommand>::new(
            WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
            &[],
            WorktreeAddressedCommand::Host(HostCommand::Send),
        ),
        Err(WorktreeBindingOverrideError::EmptySequence)
    );
    let override_value = WorktreeBindingOverride::new(
        WorktreeMergedScope::Host(WorktreeHostScope::CHAT),
        &[key('x')],
        WorktreeAddressedCommand::Host(HostCommand::Send),
    )
    .unwrap();
    let oversized = vec![override_value; WORKTREE_BINDING_OVERRIDES_MAX + 1];
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    assert!(matches!(
        WorktreeBindingModel::<HostCommand>::compose_with_overrides(&manifest, &[], &oversized),
        Err(WorktreeBindingCompositionError::TooManyOverrides { .. })
    ));
}

/// The host scopes of one modal host surface.
///
/// The names belong to the host. Kvim reads the index alone, so the mapping is
/// the only place that carries the meaning.
fn modal_scope(index: usize) -> WorktreeHostScope {
    WorktreeHostScope::new(index).expect("the test stays inside the published bound")
}

#[test]
fn a_modal_host_binds_one_sequence_in_each_of_its_scopes() {
    // `j` moves in the host Normal mode and types a letter in its Insert mode.
    // Two host tables therefore hold the same sequence, exactly as two editor
    // contexts do, and neither is a conflict.
    let normal = modal_scope(0);
    let insert = modal_scope(1);
    let visual = modal_scope(2);
    let host = vec![
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(normal)),
            &[key('j')],
            HostCommand::Sessions,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(insert)),
            &[key('j')],
            HostCommand::Send,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(visual)),
            &[key('j')],
            HostCommand::Leave,
        )
        .unwrap(),
    ];
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let model = WorktreeBindingModel::compose(&manifest, &host)
        .expect("one sequence in three host scopes is no conflict");
    let registry = model.registry();

    assert_eq!(
        registry.command(WorktreeMergedScope::Host(normal), &[key('j')]),
        Some(WorktreeMergedCommand::Host(HostCommand::Sessions))
    );
    assert_eq!(
        registry.command(WorktreeMergedScope::Host(insert), &[key('j')]),
        Some(WorktreeMergedCommand::Host(HostCommand::Send))
    );
    assert_eq!(
        registry.command(WorktreeMergedScope::Host(visual), &[key('j')]),
        Some(WorktreeMergedCommand::Host(HostCommand::Leave))
    );

    // The resolver reads the table of the focused host mode alone.
    let context =
        WorktreeBindingModel::<HostCommand>::host_context(insert, ContextGeneration::FIRST);
    let mut resolver = Resolver::new(Arc::clone(&registry), 4, Duration::ZERO);
    assert_eq!(
        resolver.dispatch(&context, Input::Key(key('j')), Some(Duration::ZERO)),
        Dispatch::Host {
            command: WorktreeMergedCommand::Host(HostCommand::Send),
        }
    );
}

#[test]
fn host_global_refusal_reaches_every_host_scope() {
    // The host-global table precedes the focused table in every host mode, so
    // one host key answers everywhere without a copy in each table.
    let leave = Key::ctrl(KeyCode::Char('e'));
    let mut host = host_bindings();
    for index in 1..4 {
        host.push(
            WorktreeHostBinding::new(
                WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(modal_scope(index))),
                &[key('j')],
                HostCommand::Send,
            )
            .unwrap(),
        );
    }
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let model = WorktreeBindingModel::compose(&manifest, &host).unwrap();
    let registry = model.registry();

    for index in 0..4 {
        let context = WorktreeBindingModel::<HostCommand>::host_context(
            modal_scope(index),
            ContextGeneration::FIRST,
        );
        let mut resolver = Resolver::new(Arc::clone(&registry), 4, Duration::ZERO);
        assert_eq!(
            resolver.dispatch(&context, Input::Key(leave), Some(Duration::ZERO)),
            Dispatch::Host {
                command: WorktreeMergedCommand::Host(HostCommand::Leave),
            },
            "host-global refusal must reach host scope {index}"
        );
    }
}

#[test]
fn a_duplicate_inside_one_host_scope_still_fails() {
    let normal = modal_scope(0);
    let host = vec![
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(normal)),
            &[key('j')],
            HostCommand::Sessions,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(normal)),
            &[key('j')],
            HostCommand::Send,
        )
        .unwrap(),
    ];
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    assert!(matches!(
        WorktreeBindingModel::compose(&manifest, &host),
        Err(WorktreeBindingCompositionError::UnapprovedConflict {
            kind: WorktreeBindingConflictKind::DuplicateSequence,
            ..
        })
    ));
}

#[test]
fn a_strict_prefix_separates_by_host_scope() {
    // `g` alone in one host scope and `g d` in another is no conflict. The two
    // inside one scope still is.
    let first = modal_scope(0);
    let second = modal_scope(1);
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let separated = vec![
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(first)),
            &[key('g')],
            HostCommand::Sessions,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(second)),
            &[key('g'), key('d')],
            HostCommand::Send,
        )
        .unwrap(),
    ];
    let model = WorktreeBindingModel::compose(&manifest, &separated)
        .expect("a prefix in another host scope is no conflict");
    assert!(
        !model
            .registry()
            .has_longer_sequence(WorktreeMergedScope::Host(first), &[key('g')])
    );
    assert!(
        model
            .registry()
            .has_longer_sequence(WorktreeMergedScope::Host(second), &[key('g')])
    );

    let together = vec![
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(first)),
            &[key('g')],
            HostCommand::Sessions,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(first)),
            &[key('g'), key('d')],
            HostCommand::Send,
        )
        .unwrap(),
    ];
    assert!(matches!(
        WorktreeBindingModel::compose(&manifest, &together),
        Err(WorktreeBindingCompositionError::UnapprovedConflict {
            kind: WorktreeBindingConflictKind::StrictPrefix,
            ..
        })
    ));
}

#[test]
fn which_key_keeps_its_owner_and_group_labels_in_a_host_scope() {
    let scope = modal_scope(3);
    let host = vec![
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Host(scope)),
            &[key(' '), key('s')],
            HostCommand::Sessions,
        )
        .unwrap(),
    ];
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let model = WorktreeBindingModel::compose(&manifest, &host).unwrap();
    let registry = model.registry();
    let context =
        WorktreeBindingModel::<HostCommand>::host_context(scope, ContextGeneration::FIRST);
    let mut resolver = Resolver::new(Arc::clone(&registry), 4, Duration::ZERO);

    assert_eq!(
        resolver.dispatch(&context, Input::Key(key(' ')), Some(Duration::ZERO)),
        Dispatch::Pending
    );
    let hints = resolver.which_key(Duration::ZERO).unwrap();
    let command = WorktreeMergedCommand::Host(HostCommand::Sessions);
    assert!(
        hints
            .hints()
            .iter()
            .any(|hint| hint.hint().commands().contains(&command))
    );
    assert_eq!(command.owner_label(), HostCommand::Sessions.owner_label());
    assert_eq!(command.group_label(), HostCommand::Sessions.group_label());
}

#[test]
fn the_host_binding_ceiling_admits_a_complete_modal_surface() {
    // One host table for each published host scope, filled to the total bound.
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let mut host = Vec::with_capacity(WORKTREE_HOST_BINDINGS_MAX);
    for index in 0..WORKTREE_HOST_BINDINGS_MAX {
        let scope = modal_scope(index % WORKTREE_HOST_SCOPES_MAX);
        let letter =
            char::from(b'a' + u8::try_from(index / WORKTREE_HOST_SCOPES_MAX).unwrap() % 26);
        let digit =
            char::from(b'0' + u8::try_from(index / (WORKTREE_HOST_SCOPES_MAX * 26)).unwrap());
        host.push(
            WorktreeHostBinding::new(
                WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(scope)),
                &[key(digit), key(letter)],
                HostCommand::Sessions,
            )
            .unwrap(),
        );
    }
    assert_eq!(host.len(), WORKTREE_HOST_BINDINGS_MAX);
    assert!(WorktreeBindingModel::compose(&manifest, &host).is_ok());

    host.push(
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(modal_scope(0))),
            &[key('z'), key('z')],
            HostCommand::Send,
        )
        .unwrap(),
    );
    assert!(matches!(
        WorktreeBindingModel::compose(&manifest, &host),
        Err(WorktreeBindingCompositionError::TooManyHostBindings { .. })
    ));
}

#[test]
fn a_host_scope_index_stays_inside_its_published_bound() {
    assert_eq!(WorktreeHostScope::CHAT.index(), 0);
    assert!(WorktreeHostScope::new(WORKTREE_HOST_SCOPES_MAX - 1).is_ok());
    assert_eq!(
        WorktreeHostScope::new(WORKTREE_HOST_SCOPES_MAX),
        Err(WorktreeHostScopeError::IndexOutOfRange {
            index: WORKTREE_HOST_SCOPES_MAX,
        })
    );
}

#[test]
fn a_host_binds_shift_enter_beside_enter() {
    // `Shift-Enter` is one key code of its own, so a host binds it without
    // touching the binding of `Enter`. See `docs/input-actions.md`.
    let scope = WorktreeHostScope::CHAT;
    let host = vec![
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(scope)),
            &[Key::plain(KeyCode::Enter)],
            HostCommand::Send,
        )
        .unwrap(),
        WorktreeHostBinding::new(
            WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Host(scope)),
            &[Key::plain(KeyCode::ShiftEnter)],
            HostCommand::Leave,
        )
        .unwrap(),
    ];
    let manifest = BindingProfile::Embedded.manifest().unwrap();
    let model =
        WorktreeBindingModel::compose(&manifest, &host).expect("the two codes are two sequences");
    let registry = model.registry();

    assert_eq!(
        registry.command(
            WorktreeMergedScope::Host(scope),
            &[Key::plain(KeyCode::Enter)]
        ),
        Some(WorktreeMergedCommand::Host(HostCommand::Send))
    );
    assert_eq!(
        registry.command(
            WorktreeMergedScope::Host(scope),
            &[Key::plain(KeyCode::ShiftEnter)]
        ),
        Some(WorktreeMergedCommand::Host(HostCommand::Leave))
    );
}
