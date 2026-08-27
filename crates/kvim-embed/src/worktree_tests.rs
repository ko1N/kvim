use std::fs;
use std::time::Duration;

use kvim_input::KeyCode;

use super::*;

const TEST_STEPS_MAX: usize = 64;

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "kvim-embed-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn event_conversion_owns_bounded_workspace_vocabulary() {
    let operation = FileOperation::Delete {
        paths: vec![WorktreeRelativePath::new("src/lib.rs").unwrap()],
    };
    let event = convert_published(TuiPublishedEvent {
        instance: kvim_tui::__private::EditorInstanceId::allocate(),
        event: TuiEditorEvent::WorkspaceReconciliationRequired { operation },
    });
    let WorktreeEvent::WorkspaceReconciliationRequired { operation } = event else {
        panic!("conversion must retain the reconciliation event");
    };
    assert_eq!(operation.kind(), WorkspaceOperationKind::Delete);
    assert_eq!(operation.deleted_paths().unwrap().len(), 1);
    assert!(operation.deleted_paths().unwrap().len() <= WORKSPACE_OPERATION_PATHS_MAX);
}

#[test]
fn capacities_reject_zero_and_excessive_values() {
    assert_eq!(
        WorktreeCapacity::new(0, 1, 1),
        Err(CapacityError::Completions)
    );
    assert_eq!(
        WorktreeCapacity::new(1, WORKER_CAPACITY_MAX + 1, 1),
        Err(CapacityError::Workers)
    );
    assert_eq!(
        WorktreeCapacity::new(1, 1, PROCESS_CAPACITY_MAX + 1),
        Err(CapacityError::Processes)
    );
}

#[test]
fn default_construction_disables_every_optional_service() {
    let root = TestRoot::new("disabled-services");
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .open()
        .unwrap();

    assert_eq!(editor.capabilities(), WorktreeCapabilities::default());
    let _ = editor.dispatch();
    assert_eq!(editor.capabilities().git, ServicePolicy::Disabled);
    assert_eq!(editor.capabilities().watcher, ServicePolicy::Disabled);
    assert_eq!(editor.capabilities().language, ServicePolicy::Disabled);
    assert_eq!(editor.capabilities().clipboard, ServicePolicy::Disabled);
    assert!(!editor.git_status_enabled());
    assert!(!editor.git_request_queued());
}

#[test]
fn service_policy_controls_construction_failure() {
    let mut disabled_calls = 0;
    let disabled = construct_service(ServicePolicy::Disabled, || {
        disabled_calls += 1;
        Err::<(), _>(std::io::Error::other("must not run"))
    })
    .unwrap();
    assert!(disabled.is_none());
    assert_eq!(disabled_calls, 0);

    let required = construct_service(ServicePolicy::BuiltIn, || {
        Err::<(), _>(std::io::Error::other("startup refused"))
    });
    assert!(required.is_err());

    let best_effort = construct_service(ServicePolicy::BestEffortBuiltIn, || {
        Err::<(), _>(std::io::Error::other("startup refused"))
    })
    .unwrap();
    assert!(best_effort.is_none());
}

#[test]
fn successful_best_effort_service_is_retained() {
    let service = construct_service(ServicePolicy::BestEffortBuiltIn, || {
        Ok::<_, std::io::Error>(17)
    })
    .unwrap();

    assert_eq!(service, Some(17));
}

#[test]
fn watcher_open_error_keeps_the_facade_kind_and_source() {
    let source = std::io::Error::other("watcher start refused");
    let error = WorktreeOpenError::new(WorktreeOpenErrorKind::Watcher, None, source);

    assert_eq!(error.kind(), WorktreeOpenErrorKind::Watcher);
    assert!(StdError::source(&error).is_some());
}

#[test]
fn host_resolved_dispatch_and_cancellation_are_addressed_and_atomic() {
    let root = TestRoot::new("host-resolved-cancel");
    let escape = Key::ctrl(KeyCode::Char(']'));
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .binding_mode(WorktreeBindingMode::HostResolved {
            reserved_escape: escape,
        })
        .open()
        .unwrap();

    let metadata = editor
        .binding_context()
        .expect("host resolution publishes metadata");
    assert_eq!(metadata.instance(), editor.instance());
    assert_eq!(metadata.reserved_escape(), escape);
    assert!(!editor.binding_manifest().unwrap().entries().is_empty());

    let generation = metadata.context().generation;
    let pending = WorktreeSemanticDispatch::new(
        editor.instance(),
        generation,
        WorktreeDispatchDecision::Complete {
            command: Command::CountDigitThree,
        },
    );
    assert!(matches!(
        editor.semantic_dispatch(pending, Duration::ZERO),
        Ok(WorktreeDispatchOutcome::Complete(
            WorktreeInputOutcome::Applied
        ))
    ));
    assert!(!editor.input_context().phases.is_idle());

    let stale_dispatch = WorktreeSemanticDispatch::new(
        editor.instance(),
        generation,
        WorktreeDispatchDecision::Unbound,
    );
    let before = editor.input_context();
    assert_eq!(
        editor.semantic_dispatch(stale_dispatch, Duration::ZERO),
        Err(WorktreeDispatchError::StaleGeneration)
    );
    assert_eq!(editor.input_context(), before);

    let wrong_dispatch = WorktreeSemanticDispatch::new(
        WorktreeInstanceId(editor.instance().0 + 1),
        before.generation,
        WorktreeDispatchDecision::Unbound,
    );
    assert_eq!(
        editor.semantic_dispatch(wrong_dispatch, Duration::ZERO),
        Err(WorktreeDispatchError::WrongInstance)
    );
    assert_eq!(editor.input_context(), before);

    let current = editor.input_context();
    let interruption = WorktreeSemanticDispatch::new(
        editor.instance(),
        current.generation,
        WorktreeDispatchDecision::Interrupted,
    );
    let WorktreeDispatchOutcome::Interrupted(proposal) = editor
        .semantic_dispatch(interruption, Duration::ZERO)
        .unwrap()
    else {
        panic!("an interruption must propose cancellation");
    };
    assert!(
        !editor.input_context().phases.is_idle(),
        "proposal changes no state"
    );

    let wrong = CancelPendingProposal {
        instance: WorktreeInstanceId(editor.instance().0 + 1),
        generation: proposal.generation,
    };
    let before = editor.input_context();
    assert_eq!(
        editor.cancel_pending(wrong, Duration::ZERO),
        Err(WorktreeDispatchError::WrongInstance)
    );
    assert_eq!(editor.input_context(), before);

    let resume = editor.cancel_pending(proposal, Duration::ZERO).unwrap();
    assert_eq!(resume.instance(), editor.instance());
    assert!(resume.context().phases.is_idle());
    assert_ne!(resume.context().generation, proposal.generation);
    assert_eq!(
        editor.cancel_pending(proposal, Duration::ZERO),
        Err(WorktreeDispatchError::StaleGeneration)
    );
}

#[test]
fn host_resolved_raw_input_and_idle_interruption_are_atomic_refusals() {
    let root = TestRoot::new("host-raw-refusal");
    let mut editor = host_resolved_editor(&root);
    let before = editor.input_context();

    assert_eq!(
        editor.input(
            WorktreeInput::Key(Key::plain(KeyCode::Char('3'))),
            Duration::ZERO,
        ),
        Err(WorktreeInputError::HostResolved)
    );
    assert_eq!(editor.input_context(), before);

    let paste = PasteText::new("text").unwrap();
    assert_eq!(
        editor.input(WorktreeInput::Paste(paste), Duration::ZERO),
        Err(WorktreeInputError::HostResolved)
    );
    assert_eq!(editor.input_context(), before);

    assert_eq!(
        editor.semantic_dispatch(
            WorktreeSemanticDispatch::new(
                editor.instance(),
                before.generation,
                WorktreeDispatchDecision::Interrupted,
            ),
            Duration::ZERO,
        ),
        Err(WorktreeDispatchError::NoPending)
    );
    assert_eq!(editor.input_context(), before);
}

#[test]
fn fabricated_complete_command_is_rejected_in_each_current_scope() {
    let root = TestRoot::new("invalid-resolved-command");
    let mut editor = host_resolved_editor(&root);
    let before = editor.input_context();
    assert_eq!(
        dispatch_result(
            &mut editor,
            WorktreeDispatchDecision::Complete {
                command: Command::PromptAccept,
            },
        ),
        Err(WorktreeDispatchError::InvalidResolvedCommand)
    );
    assert_eq!(editor.input_context(), before);

    assert_eq!(
        dispatch_result(
            &mut editor,
            WorktreeDispatchDecision::TextFallback(TypedText::Typed('x')),
        ),
        Err(WorktreeDispatchError::InvalidResolvedCommand)
    );
    assert_eq!(editor.input_context(), before);

    dispatch_complete(&mut editor, Command::OpenCommandLine);
    let prompt = editor.input_context();
    assert_eq!(prompt.scope, BindingScope::Prompt);
    assert_eq!(
        dispatch_result(
            &mut editor,
            WorktreeDispatchDecision::Complete {
                command: Command::MoveDown,
            },
        ),
        Err(WorktreeDispatchError::InvalidResolvedCommand)
    );
    assert_eq!(editor.input_context(), prompt);

    dispatch_complete(&mut editor, Command::PromptCancel);
    assert_eq!(
        dispatch_result(
            &mut editor,
            WorktreeDispatchDecision::Complete {
                command: Command::NextReviewSection,
            },
        ),
        Err(WorktreeDispatchError::InvalidResolvedCommand)
    );
}

#[test]
fn complete_validation_accepts_only_active_focus_or_overlay_scopes() {
    let manifest = BindingProfile::Embedded.manifest().unwrap();

    for scope in BindingScope::ALL {
        let active = manifest
            .entries()
            .iter()
            .find(|entry| entry.scope() == scope)
            .map(|entry| entry.command());
        if let Some(command) = active {
            assert!(
                resolved_command_is_valid(&manifest, scope, None, command),
                "{scope:?} must accept its own bound command"
            );
        }

        let inactive = manifest
            .entries()
            .iter()
            .find(|candidate| {
                !manifest
                    .entries()
                    .iter()
                    .any(|entry| entry.scope() == scope && entry.command() == candidate.command())
            })
            .expect("every scope excludes at least one command")
            .command();
        assert!(
            !resolved_command_is_valid(&manifest, scope, None, inactive),
            "{scope:?} must reject a command bound only in an inactive scope"
        );
    }

    assert!(resolved_command_is_valid(
        &manifest,
        BindingScope::Prompt,
        Some(BindingScope::Picker),
        Command::PickerSelectNext,
    ));
    assert!(!resolved_command_is_valid(
        &manifest,
        BindingScope::Prompt,
        Some(BindingScope::Picker),
        Command::NextReviewSection,
    ));
}

#[test]
fn every_stale_dispatch_decision_is_rejected_before_mutation() {
    let root = TestRoot::new("stale-decisions");
    let mut editor = host_resolved_editor(&root);
    let stale = editor.input_context().generation;
    dispatch_complete(&mut editor, Command::CountDigitThree);
    let before = editor.input_context();
    let decisions = [
        WorktreeDispatchDecision::Complete {
            command: Command::MoveDown,
        },
        WorktreeDispatchDecision::Pending,
        WorktreeDispatchDecision::TextObjectPending,
        WorktreeDispatchDecision::TextFallback(TypedText::Typed('x')),
        WorktreeDispatchDecision::Unbound,
        WorktreeDispatchDecision::Interrupted,
    ];

    for decision in decisions {
        let dispatch = WorktreeSemanticDispatch::new(editor.instance(), stale, decision);
        assert_eq!(
            editor.semantic_dispatch(dispatch, Duration::ZERO),
            Err(WorktreeDispatchError::StaleGeneration)
        );
        assert_eq!(editor.input_context(), before);
    }
}

#[test]
fn static_host_prefix_does_not_invent_semantic_pending_state() {
    let root = TestRoot::new("static-prefix");
    let mut editor = host_resolved_editor(&root);
    let before = editor.input_context();

    assert_eq!(
        dispatch_result(&mut editor, WorktreeDispatchDecision::Pending),
        Ok(WorktreeDispatchOutcome::Pending)
    );
    assert_eq!(editor.input_context(), before);
}

#[test]
fn cancellation_clears_each_semantic_prefix_but_preserves_insert_mode() {
    let scenarios: &[(&str, &[Command])] = &[
        ("count", &[Command::CountDigitThree]),
        ("operator", &[Command::DeleteOverMotion]),
        ("register", &[Command::SelectRegister]),
        ("text-object-selection", &[Command::DeleteOverMotion]),
        ("command-prompt", &[Command::OpenCommandLine]),
    ];
    for (name, commands) in scenarios {
        let root = TestRoot::new(&format!("semantic-cancel-{name}"));
        let mut editor = host_resolved_editor(&root);
        for command in *commands {
            dispatch_complete(&mut editor, *command);
        }
        assert!(!editor.input_context().phases.is_idle());
        interrupt_and_cancel(&mut editor);
        assert!(editor.input_context().phases.is_idle());
    }

    let root = TestRoot::new("partial-static-prefix-cancel");
    let mut editor = host_resolved_editor(&root);
    let before = editor.input_context();
    dispatch_decision(&mut editor, WorktreeDispatchDecision::Pending);
    assert_eq!(editor.input_context(), before);
    assert_eq!(
        dispatch_result(&mut editor, WorktreeDispatchDecision::Interrupted),
        Err(WorktreeDispatchError::NoPending)
    );

    let root = TestRoot::new("text-object-selection-cancel");
    let mut editor = host_resolved_editor(&root);
    dispatch_complete(&mut editor, Command::DeleteOverMotion);
    dispatch_decision(&mut editor, WorktreeDispatchDecision::TextObjectPending);
    assert!(editor.input_context().phases.text_object.is_pending());
    interrupt_and_cancel(&mut editor);
    assert!(editor.input_context().phases.is_idle());

    let root = TestRoot::new("insert-cancel");
    let mut editor = host_resolved_editor(&root);
    dispatch_complete(&mut editor, Command::InsertBeforeCursor);
    assert_eq!(editor.mode(), Mode::Insert);
    dispatch_decision(
        &mut editor,
        WorktreeDispatchDecision::TextFallback(TypedText::Typed('x')),
    );
    let before = editor.input_context();
    assert!(before.phases.is_idle());
    assert_eq!(
        dispatch_result(&mut editor, WorktreeDispatchDecision::Interrupted),
        Err(WorktreeDispatchError::NoPending)
    );
    assert_eq!(editor.mode(), Mode::Insert);
    assert_eq!(editor.input_context(), before);
}

fn host_resolved_editor(root: &TestRoot) -> WorktreeEditor {
    WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .binding_mode(WorktreeBindingMode::HostResolved {
            reserved_escape: Key::ctrl(KeyCode::Char(']')),
        })
        .open()
        .unwrap()
}

fn dispatch_complete(editor: &mut WorktreeEditor, command: Command) {
    dispatch_decision(editor, WorktreeDispatchDecision::Complete { command });
}

fn dispatch_decision(editor: &mut WorktreeEditor, decision: WorktreeDispatchDecision) {
    dispatch_result(editor, decision).unwrap();
}

fn dispatch_result(
    editor: &mut WorktreeEditor,
    decision: WorktreeDispatchDecision,
) -> Result<WorktreeDispatchOutcome, WorktreeDispatchError> {
    let context = editor.input_context();
    editor.semantic_dispatch(
        WorktreeSemanticDispatch::new(editor.instance(), context.generation, decision),
        Duration::ZERO,
    )
}

fn interrupt_and_cancel(editor: &mut WorktreeEditor) {
    let context = editor.input_context();
    let outcome = editor
        .semantic_dispatch(
            WorktreeSemanticDispatch::new(
                editor.instance(),
                context.generation,
                WorktreeDispatchDecision::Interrupted,
            ),
            Duration::ZERO,
        )
        .unwrap();
    let WorktreeDispatchOutcome::Interrupted(proposal) = outcome else {
        panic!("interruption must return a proposal");
    };
    editor.cancel_pending(proposal, Duration::ZERO).unwrap();
}

#[test]
fn invalid_register_is_rejected_before_state_changes() {
    let root = TestRoot::new("invalid-register");
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .open()
        .unwrap();
    let before = editor.input_context();

    assert_eq!(
        editor.command(Command::DeleteOverMotion, None, Some('!'), Duration::ZERO),
        Err(WorktreeCommandError::InvalidRegisterName { name: '!' })
    );
    assert_eq!(editor.input_context(), before);
}

#[tokio::test]
async fn worktree_lifecycle_opens_edits_renders_saves_and_shuts_down() {
    let root = TestRoot::new("lifecycle");
    fs::write(root.0.join("note.txt"), "hello\n").unwrap();
    let area = Rect::new(0, 0, 40, 8);
    let mut editor = WorktreeEditor::builder(&root.0, area).open().unwrap();

    let mut cells = Buffer::empty(area);
    let cursor = editor.render(&mut cells).unwrap();
    assert!(cursor.position.is_some());

    editor.open_file(WorktreeRelativePath::new("note.txt").unwrap());
    drive_until(&mut editor, |event| {
        matches!(event, WorktreeEvent::ActiveFileChanged { path: Some(path) } if path.as_path() == Path::new("note.txt"))
    })
    .await;

    assert_eq!(
        editor
            .command(Command::InsertBeforeCursor, None, None, Duration::ZERO)
            .unwrap(),
        WorktreeInputOutcome::Applied
    );
    assert_eq!(
        editor.literal("saved ", Duration::ZERO),
        WorktreeInputOutcome::Applied
    );
    editor
        .command(Command::ReturnToNormal, None, None, Duration::ZERO)
        .unwrap();
    editor
        .command(Command::SaveBuffer, None, None, Duration::ZERO)
        .unwrap();
    drive_until(&mut editor, |event| {
        matches!(event, WorktreeEvent::FileWritten { path } if path.as_path() == Path::new("note.txt"))
    })
    .await;
    assert!(
        !editor.git_request_queued(),
        "disabled Git status must not queue a process after a save"
    );
    assert!(
        !editor.git_status_enabled(),
        "disabled Git status must remain disabled for the editor lifetime"
    );
    assert_eq!(
        fs::read_to_string(root.0.join("note.txt")).unwrap(),
        "saved hello\n"
    );

    match editor.shutdown(Duration::from_secs(2)).await {
        WorktreeShutdown::Finished { .. } => {}
        WorktreeShutdown::Draining(drain) => {
            let _events = drain.complete().await;
        }
    }
}

async fn drive_until(editor: &mut WorktreeEditor, wanted: impl Fn(&WorktreeEvent) -> bool) {
    for _ in 0..TEST_STEPS_MAX {
        let _ = editor.dispatch();
        while let Some(event) = editor.take_event() {
            if wanted(&event) {
                return;
            }
        }
        let completion = tokio::time::timeout(Duration::from_secs(2), editor.ready())
            .await
            .expect("bounded work must complete");
        let _ = editor
            .apply(completion, Duration::ZERO)
            .expect("ready returns this editor's completion");
    }
    panic!("bounded lifecycle did not produce the expected event");
}

#[tokio::test]
async fn wrong_instance_completion_is_rejected_without_mutating_the_receiver() {
    let first_root = TestRoot::new("completion-owner-first");
    let second_root = TestRoot::new("completion-owner-second");
    let area = Rect::new(0, 0, 30, 6);
    let mut first = WorktreeEditor::builder(&first_root.0, area).open().unwrap();
    let mut second = WorktreeEditor::builder(&second_root.0, area)
        .open()
        .unwrap();

    assert_ne!(first.instance(), second.instance());
    let _ = first.dispatch();
    let _ = second.dispatch();
    let completion = tokio::time::timeout(Duration::from_secs(2), first.ready())
        .await
        .expect("the first editor's local request completes");
    let second_completion = tokio::time::timeout(Duration::from_secs(2), second.ready())
        .await
        .expect("the second editor's matching local request completes");
    while second.take_event().is_some() {}
    let mode_before = second.mode();
    let context_before = second.input_context();

    let error = second
        .apply(completion, Duration::from_secs(60))
        .expect_err("another editor must reject the completion in release builds");
    assert_eq!(
        error.kind(),
        WorktreeApplyErrorKind::WrongInstance {
            editor: second.instance(),
            completion: first.instance(),
        }
    );
    assert_eq!(second.mode(), mode_before);
    assert_eq!(second.input_context(), context_before);
    assert!(second.take_event().is_none());
    second
        .apply(second_completion, Duration::ZERO)
        .expect("rejection does not consume the receiver's matching request state");

    first
        .apply(error.into_completion(), Duration::ZERO)
        .expect("the producing editor accepts its recovered completion");
}

#[cfg(feature = "grammar-rust")]
#[test]
fn rust_feature_populates_the_registry_used_by_the_facade() {
    let request = WorktreeHostReportRequest::built_in(WorktreeHostWorkspace::Unresolved {
        reason: "test root unavailable".to_owned(),
    });

    assert!(request.run().contains("rust-analyzer"));
}
