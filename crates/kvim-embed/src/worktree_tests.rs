use std::fs;
use std::time::Duration;

use kvim_input::KeyCode;
use kvim_keymap::{CellPosition, PointerAction, PointerButton, PointerModifiers};

use crate::{DialogChoice, DialogChoiceId, DialogStyles};

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

fn dialog_request(area: Rect) -> DialogRequest {
    let cancel = DialogChoiceId::new(1);
    DialogRequest::new(
        "Continue?",
        std::iter::empty::<&str>(),
        [
            DialogChoice::new(cancel, "Cancel"),
            DialogChoice::new(DialogChoiceId::new(2), "Continue"),
        ],
        cancel,
        cancel,
        area,
        DialogStyles::default(),
    )
    .unwrap()
}

#[test]
fn host_dialog_lifecycle_owns_context_render_semantics_and_event() {
    let root = TestRoot::new("host-dialog-lifecycle");
    let area = Rect::new(2, 1, 40, 10);
    let mut editor = WorktreeEditor::builder(&root.0, area)
        .binding_mode(WorktreeBindingMode::HostResolved {
            reserved_escape: Key::ctrl(KeyCode::Char(']')),
        })
        .presentation(WorktreePresentation::standalone().which_key(SurfaceOwnership::HostOwned))
        .open()
        .unwrap();
    let before = editor.input_context();
    editor.open_dialog(dialog_request(area)).unwrap();
    let opened = editor.input_context();
    assert_eq!(opened.scope, BindingScope::Confirmation);
    assert_ne!(opened.generation, before.generation);
    assert_eq!(editor.binding_context().unwrap().context(), opened);
    assert_eq!(editor.binding_context().unwrap().overlay_scope(), None);

    let mut cells = Buffer::empty(Rect::new(0, 0, 48, 14));
    editor.render(&mut cells).unwrap();
    assert!(editor.dialog_snapshot().unwrap().placement().is_some());
    assert_eq!(
        editor.literal("leak", Duration::ZERO),
        WorktreeInputOutcome::Applied
    );
    assert_eq!(
        editor.paste(&PasteText::new("leak").unwrap(), Duration::ZERO),
        WorktreeInputOutcome::Applied
    );
    assert_eq!(
        editor.command(Command::InsertBeforeCursor, None, None, Duration::ZERO,),
        Ok(WorktreeInputOutcome::Applied)
    );
    let addressed = WorktreeSemanticDispatch::new(
        editor.instance(),
        opened.generation,
        WorktreeDispatchDecision::Unbound,
    );
    assert_eq!(
        editor.semantic_dispatch(addressed, Duration::ZERO),
        Ok(WorktreeDispatchOutcome::Consumed)
    );
    let stale = WorktreeSemanticDispatch::new(
        editor.instance(),
        before.generation,
        WorktreeDispatchDecision::Unbound,
    );
    assert_eq!(
        editor.semantic_dispatch(stale, Duration::ZERO),
        Err(WorktreeDispatchError::StaleGeneration)
    );

    assert_eq!(
        editor.dialog_input(DialogInput::Key(Key::plain(KeyCode::Down))),
        DialogInputOutcome::Redraw
    );
    let focused = editor.input_context();
    assert_eq!(focused.scope, BindingScope::Confirmation);
    assert_ne!(focused.generation, opened.generation);
    editor.render(&mut cells).unwrap();
    assert_eq!(
        editor.dialog_input(DialogInput::Key(Key::plain(KeyCode::Enter))),
        DialogInputOutcome::Answered
    );
    let closed = editor.input_context();
    assert_ne!(closed.scope, BindingScope::Confirmation);
    assert_ne!(closed.generation, focused.generation);
    assert_eq!(
        editor.take_event(),
        Some(WorktreeEvent::DialogAnswered(DialogAnswer {
            choice: DialogChoiceId::new(2),
        }))
    );
    assert!(!matches!(
        editor.take_event(),
        Some(WorktreeEvent::DialogAnswered(_))
    ));
}

#[test]
fn accepted_resize_closes_dialog_when_fixed_body_no_longer_fits() {
    let root = TestRoot::new("dialog-resize-close");
    let area = Rect::new(0, 0, 40, 10);
    let mut editor = WorktreeEditor::builder(&root.0, area).open().unwrap();
    editor.open_dialog(dialog_request(area)).unwrap();
    editor.resize(Rect::new(0, 0, 20, 6)).unwrap();
    assert!(!editor.dialog_is_open());
    assert!(editor.dialog_snapshot().is_none());
    assert!(!matches!(
        editor.take_event(),
        Some(WorktreeEvent::DialogAnswered(_))
    ));
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
fn every_presentation_combination_opens_and_writes_only_the_accepted_rectangle() {
    let root = TestRoot::new("presentation-combinations");
    let area = Rect::new(3, 2, 9, 4);
    let buffer_area = Rect::new(0, 0, 16, 9);

    for command_line in [SurfaceOwnership::Embedded, SurfaceOwnership::HostOwned] {
        for statusline in [SurfaceOwnership::Embedded, SurfaceOwnership::HostOwned] {
            for which_key in [SurfaceOwnership::Embedded, SurfaceOwnership::HostOwned] {
                for file_sidebar in [SurfaceOwnership::Embedded, SurfaceOwnership::HostOwned] {
                    let presentation = WorktreePresentation::standalone()
                        .command_line(command_line)
                        .statusline(statusline)
                        .which_key(which_key)
                        .file_sidebar(file_sidebar);
                    let mut builder =
                        WorktreeEditor::builder(&root.0, area).presentation(presentation);
                    if command_line == SurfaceOwnership::HostOwned {
                        builder = builder.command_surface(WorktreeCommandSurface::new());
                    }
                    if which_key == SurfaceOwnership::HostOwned {
                        builder = builder.binding_mode(WorktreeBindingMode::HostResolved {
                            reserved_escape: Key::ctrl(KeyCode::Char(']')),
                        });
                    }
                    let editor = builder.open().unwrap();
                    let mut cells = Buffer::filled(buffer_area, ratatui::buffer::Cell::new("#"));
                    let cursor = editor.render(&mut cells).unwrap();
                    assert!(
                        cursor
                            .position
                            .is_none_or(|position| area.contains(position))
                    );
                    for y in buffer_area.y..buffer_area.bottom() {
                        for x in buffer_area.x..buffer_area.right() {
                            if !area.contains(Position::new(x, y)) {
                                assert_eq!(cells[(x, y)].symbol(), "#");
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn host_owned_sidebar_publishes_stable_bounded_rows_and_semantic_commands() {
    let root = TestRoot::new("host-sidebar-state");
    fs::create_dir_all(root.0.join("src")).unwrap();
    fs::create_dir_all(root.0.join("target")).unwrap();
    fs::write(root.0.join(".hidden"), "hidden\n").unwrap();
    fs::write(root.0.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.0.join("README.md"), "read me\n").unwrap();
    let presentation = WorktreePresentation::standalone().file_sidebar(SurfaceOwnership::HostOwned);
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 80, 8))
        .presentation(presentation)
        .open()
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for _ in 0..TEST_STEPS_MAX {
            editor.dispatch();
            if editor
                .file_sidebar_snapshot()
                .is_some_and(|snapshot| !snapshot.rows().is_empty())
            {
                break;
            }
            let completion = editor.ready().await;
            editor.apply(completion, Duration::ZERO).unwrap();
        }
    });

    let first = editor.file_sidebar_snapshot().unwrap();
    assert_eq!(first.instance(), editor.instance());
    assert!(first.rows().len() <= FILE_SIDEBAR_ROWS_MAX);
    let readme = first
        .rows()
        .iter()
        .find(|row| row.label() == "README.md")
        .unwrap();
    assert_eq!(readme.path().unwrap().as_path(), Path::new("README.md"));
    assert_eq!(readme.kind(), FileSidebarRowKind::File);
    assert_eq!(readme.icon(), Some(FileSidebarIconRole::Document));
    assert_eq!(readme.icon_glyph(), Some("\u{f48a}"));
    assert_eq!(readme.dimming(), None);
    assert_eq!(readme.notice_kind(), None);
    assert_eq!(readme.matched_characters(), None);
    let generated = first
        .rows()
        .iter()
        .find(|row| row.label() == "target")
        .unwrap();
    assert_eq!(generated.dimming(), Some(FileSidebarDimming::Generated));
    assert_ne!(generated.git(), Some(FileSidebarGitState::Ignored));
    let hidden_notice = first
        .rows()
        .iter()
        .find(|row| matches!(row.kind(), FileSidebarRowKind::Notice(_)))
        .unwrap();
    assert_eq!(
        hidden_notice.notice_kind(),
        Some(FileSidebarNoticeKind::Hidden)
    );
    assert_eq!(hidden_notice.icon(), None);
    assert_eq!(hidden_notice.icon_glyph(), None);
    let ids = first
        .rows()
        .iter()
        .map(|row| row.id())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), first.rows().len());
    let readme_id = readme.id().clone();
    assert!(matches!(
        editor.file_sidebar_command(FileSidebarCommand::Select(readme_id.clone())),
        FileSidebarOutcome::Applied(_)
    ));
    assert!(
        editor
            .file_sidebar_snapshot()
            .unwrap()
            .rows()
            .iter()
            .any(|row| row.id() == &readme_id && row.is_selected())
    );

    let selected_before_notice = editor
        .file_sidebar_snapshot()
        .unwrap()
        .rows()
        .iter()
        .find(|row| row.is_selected())
        .map(|row| row.id().clone());
    let hidden_notice_id = hidden_notice.id().clone();
    assert_eq!(
        editor.file_sidebar_command(FileSidebarCommand::Select(hidden_notice_id.clone())),
        FileSidebarOutcome::NotSelected(hidden_notice_id)
    );
    assert_eq!(
        editor
            .file_sidebar_snapshot()
            .unwrap()
            .rows()
            .iter()
            .find(|row| row.is_selected())
            .map(|row| row.id().clone()),
        selected_before_notice
    );

    let _ = editor.file_sidebar_command(FileSidebarCommand::MoveFirst);
    let selected_before_motion = editor
        .file_sidebar_snapshot()
        .unwrap()
        .rows()
        .iter()
        .find(|row| row.is_selected())
        .map(|row| row.id().clone());
    let _ = editor.file_sidebar_command(FileSidebarCommand::MoveDown);
    let selected_after_motion = editor
        .file_sidebar_snapshot()
        .unwrap()
        .rows()
        .iter()
        .find(|row| row.is_selected())
        .map(|row| row.id().clone());
    assert_ne!(selected_after_motion, selected_before_motion);

    fs::remove_file(root.0.join("README.md")).unwrap();
    fs::write(
        root.0.join("0-new-sibling.rs"),
        "pub const NEW: bool = true;\n",
    )
    .unwrap();

    assert_eq!(
        editor.file_sidebar_command(FileSidebarCommand::Refresh),
        FileSidebarOutcome::Applied(WorktreeUpdate::Unchanged)
    );
    runtime.block_on(async {
        for _ in 0..TEST_STEPS_MAX {
            editor.dispatch();
            let completion = editor.ready().await;
            editor.apply(completion, Duration::ZERO).unwrap();
            if editor
                .file_sidebar_snapshot()
                .unwrap()
                .rows()
                .iter()
                .any(|row| row.label() == "0-new-sibling.rs")
            {
                break;
            }
        }
    });
    let refreshed = editor.file_sidebar_snapshot().unwrap();
    assert!(!refreshed.rows().iter().any(|row| row.id() == &readme_id));
    let selection_before_stale = refreshed
        .rows()
        .iter()
        .find(|row| row.is_selected())
        .map(|row| row.id().clone());
    assert_eq!(
        editor.file_sidebar_command(FileSidebarCommand::Select(readme_id.clone())),
        FileSidebarOutcome::NotSelected(readme_id)
    );
    assert_eq!(
        editor
            .file_sidebar_snapshot()
            .unwrap()
            .rows()
            .iter()
            .find(|row| row.is_selected())
            .map(|row| row.id().clone()),
        selection_before_stale
    );

    assert_eq!(
        editor.file_sidebar_command(FileSidebarCommand::FocusBoundary(Direction::Left)),
        FileSidebarOutcome::HostFocusBoundary(Direction::Left)
    );
    let embedded_root = TestRoot::new("embedded-sidebar-input");
    let mut embedded = WorktreeEditor::builder(&embedded_root.0, Rect::new(0, 0, 20, 4))
        .open()
        .unwrap();
    assert!(embedded.file_sidebar_snapshot().is_none());
    assert_eq!(
        embedded.file_sidebar_command(FileSidebarCommand::MoveDown),
        FileSidebarOutcome::Embedded
    );
}

#[test]
fn host_sidebar_search_lifecycle_rejects_stale_wrong_instance_and_oversized_queries() {
    let root = TestRoot::new("host-sidebar-search-lifecycle");
    let presentation = WorktreePresentation::standalone().file_sidebar(SurfaceOwnership::HostOwned);
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 40, 6))
        .presentation(presentation)
        .open()
        .unwrap();
    let other_root = TestRoot::new("other-host-sidebar-search");
    let other_presentation =
        WorktreePresentation::standalone().file_sidebar(SurfaceOwnership::HostOwned);
    let mut other = WorktreeEditor::builder(&other_root.0, Rect::new(0, 0, 40, 6))
        .presentation(other_presentation)
        .open()
        .unwrap();

    let stale_prompt = editor.begin_file_sidebar_search().unwrap();
    let prompt = editor.begin_file_sidebar_search().unwrap();
    assert_eq!(
        editor.accept_file_sidebar_search(stale_prompt, "src"),
        Err(FileSidebarOperationError::StaleSearch)
    );
    assert_eq!(
        other.accept_file_sidebar_search(prompt, "src"),
        Err(FileSidebarOperationError::WrongInstance)
    );
    let oversized = "x".repeat(FILE_SIDEBAR_SEARCH_CHARS_MAX + 1);
    assert_eq!(
        editor.accept_file_sidebar_search(prompt, &oversized),
        Err(FileSidebarOperationError::QueryTooLong)
    );
    editor.accept_file_sidebar_search(prompt, "src").unwrap();
    assert_eq!(
        editor.update_file_sidebar_search(prompt, &oversized),
        Err(FileSidebarOperationError::QueryTooLong)
    );
    editor.update_file_sidebar_search(prompt, "main").unwrap();

    let replacement_prompt = editor.begin_file_sidebar_search().unwrap();
    editor
        .cancel_file_sidebar_search_prompt(replacement_prompt)
        .unwrap();
    assert_eq!(
        editor.next_file_sidebar_match(prompt),
        Ok(FileSidebarSearchOutcome::SearchMissed)
    );
    editor.end_file_sidebar_search(prompt).unwrap();
    assert_eq!(
        editor.previous_file_sidebar_match(prompt),
        Err(FileSidebarOperationError::StaleSearch)
    );

    let empty = editor.begin_file_sidebar_search().unwrap();
    editor.accept_file_sidebar_search(empty, "").unwrap();
    assert_eq!(
        editor.end_file_sidebar_search(empty),
        Err(FileSidebarOperationError::StaleSearch)
    );
}

#[test]
fn host_sidebar_page_commands_accept_geometry_and_embedded_sidebar_rejects_operations() {
    let root = TestRoot::new("host-sidebar-page-api");
    let presentation = WorktreePresentation::standalone().file_sidebar(SurfaceOwnership::HostOwned);
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 40, 6))
        .presentation(presentation)
        .open()
        .unwrap();
    editor
        .record_file_sidebar_viewport(NonZeroU16::new(4).unwrap(), NonZeroU16::new(20).unwrap())
        .unwrap();
    editor.move_file_sidebar_half_page_down().unwrap();
    editor.move_file_sidebar_half_page_up().unwrap();
    editor.move_file_sidebar_full_page_down().unwrap();
    editor.move_file_sidebar_full_page_up().unwrap();

    let embedded_root = TestRoot::new("embedded-sidebar-operation");
    let mut embedded = WorktreeEditor::builder(&embedded_root.0, Rect::new(0, 0, 20, 4))
        .open()
        .unwrap();
    assert_eq!(
        embedded.begin_file_sidebar_search(),
        Err(FileSidebarOperationError::Embedded)
    );
    assert_eq!(
        embedded.move_file_sidebar_half_page_down(),
        Err(FileSidebarOperationError::Embedded)
    );

    let host_command_root = TestRoot::new("host-command-host-sidebar-search");
    let host_command_presentation = WorktreePresentation::standalone()
        .command_line(SurfaceOwnership::HostOwned)
        .file_sidebar(SurfaceOwnership::HostOwned);
    let mut host_command_editor =
        WorktreeEditor::builder(&host_command_root.0, Rect::new(0, 0, 20, 4))
            .presentation(host_command_presentation)
            .command_surface(WorktreeCommandSurface::new())
            .open()
            .unwrap();
    let WorktreeInputOutcome::Request(WorktreeInputRequest::OpenCommandLine(command_session)) =
        host_command_editor
            .command(Command::OpenCommandLine, None, None, Duration::ZERO)
            .unwrap()
    else {
        panic!("host command line must open independently");
    };
    let search = host_command_editor.begin_file_sidebar_search().unwrap();
    host_command_editor
        .cancel_file_sidebar_search_prompt(search)
        .unwrap();
    host_command_editor
        .close_command_session(command_session)
        .unwrap();
}

#[test]
fn host_owned_sidebar_activation_opens_the_selected_file() {
    let root = TestRoot::new("host-sidebar-activation");
    fs::write(root.0.join("only.rs"), "pub fn only() {}\n").unwrap();
    let presentation = WorktreePresentation::standalone().file_sidebar(SurfaceOwnership::HostOwned);
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 40, 6))
        .presentation(presentation)
        .open()
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for _ in 0..TEST_STEPS_MAX {
            editor.dispatch();
            if editor
                .file_sidebar_snapshot()
                .is_some_and(|snapshot| !snapshot.rows().is_empty())
            {
                break;
            }
            let completion = editor.ready().await;
            editor.apply(completion, Duration::ZERO).unwrap();
        }
    });
    assert_eq!(
        editor.file_sidebar_command(FileSidebarCommand::Activate),
        FileSidebarOutcome::Activated {
            path: WorktreeRelativePath::new("only.rs").unwrap(),
            update: WorktreeUpdate::Redraw,
        }
    );
}

#[test]
fn host_owned_sidebar_allocates_no_columns_and_embedded_sidebar_keeps_its_geometry() {
    let root = TestRoot::new("presentation-sidebar");
    let area = Rect::new(0, 0, 80, 8);
    let host_presentation =
        WorktreePresentation::standalone().file_sidebar(SurfaceOwnership::HostOwned);
    let mut host = WorktreeEditor::builder(&root.0, area)
        .presentation(host_presentation)
        .open()
        .unwrap();
    let _ = host.command(Command::RevealInFileTree, None, None, Duration::ZERO);
    let _ = host.command(Command::FocusWindowRight, None, None, Duration::ZERO);
    let _ = host.command(Command::RevealInFileTree, None, None, Duration::ZERO);
    let host_regions = host.region_areas();

    assert_ne!(host.input_context().scope, BindingScope::Sidebar);
    let mut embedded = WorktreeEditor::builder(&root.0, area).open().unwrap();
    let _ = embedded.command(Command::RevealInFileTree, None, None, Duration::ZERO);
    let embedded_regions = embedded.region_areas();

    assert_eq!(host_regions.len(), 1);
    assert_eq!(host_regions[0].0, kvim_ui::RegionKind::Surface);
    assert_eq!(host_regions[0].1.width, area.width);
    assert!(embedded_regions.iter().any(|(kind, region)| {
        matches!(kind, kvim_ui::RegionKind::Sidebar(_)) && region.width == 40
    }));
}

#[test]
fn a_pointer_border_drag_moves_the_published_split_edge_of_the_facade() {
    let root = TestRoot::new("pointer-border-drag");
    let area = Rect::new(0, 0, 80, 8);
    let mut editor = WorktreeEditor::builder(&root.0, area).open().unwrap();
    let _ = editor.command(Command::SplitAdaptive, None, None, Duration::ZERO);
    let surfaces = |editor: &WorktreeEditor| -> Vec<Rect> {
        let mut areas: Vec<Rect> = editor
            .region_areas()
            .into_iter()
            .filter(|(kind, _)| *kind == kvim_ui::RegionKind::Surface)
            .map(|(_, region)| region)
            .collect();
        areas.sort_by_key(|region| region.x);
        areas
    };
    let before = surfaces(&editor);
    assert_eq!(before.len(), 2, "the adaptive rule splits the one window");
    // A vertical border is the last column of the pane left of it.
    let column = before[0].right() - 1;
    let row = before[0].y + 2;

    let press = PointerEvent::new(
        CellPosition::new(column, row),
        PointerModifiers::default(),
        PointerAction::Press(PointerButton::Left),
    );
    let drag = PointerEvent::new(
        CellPosition::new(column + 7, row),
        PointerModifiers::default(),
        PointerAction::Drag(PointerButton::Left),
    );
    let _ = editor.pointer(press, Duration::ZERO);
    assert_eq!(
        editor.pointer(drag, Duration::ZERO),
        WorktreeUpdate::Redraw,
        "the facade forwards the drag to the border under it"
    );

    let after = surfaces(&editor);
    assert_eq!(after[0].width, before[0].width + 7);
    assert_eq!(after[1].width, before[1].width - 7);
    assert_eq!(
        after[0].width + after[1].width,
        area.width,
        "the panes still cover the host area"
    );
}

#[test]
fn host_owned_command_line_requires_its_surface_before_root_or_live_state() {
    let missing_root = std::env::temp_dir().join("kvim-missing-presentation-root");
    let error = WorktreeEditor::builder(&missing_root, Rect::new(0, 0, 20, 3))
        .presentation(WorktreePresentation::standalone().command_line(SurfaceOwnership::HostOwned))
        .open()
        .unwrap_err();
    assert_eq!(error.kind(), WorktreeOpenErrorKind::CommandSurface);
}

#[test]
fn integrated_host_opens_with_its_required_input_and_command_capabilities() {
    let root = TestRoot::new("integrated-host-presentation");
    let escape = Key::ctrl(KeyCode::Char(']'));
    let editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .binding_mode(WorktreeBindingMode::HostResolved {
            reserved_escape: escape,
        })
        .presentation(WorktreePresentation::integrated_host())
        .command_surface(WorktreeCommandSurface::new())
        .open()
        .unwrap();

    let context = editor
        .binding_context()
        .expect("the integrated host owns physical resolution");
    assert_eq!(context.reserved_escape(), escape);
    assert_eq!(editor.region_areas().len(), 1);
}

#[test]
fn presentation_ownership_must_match_the_effective_resolver() {
    let root = TestRoot::new("presentation-ownership");
    let area = Rect::new(0, 0, 30, 6);

    let default_editor = WorktreeEditor::builder(&root.0, area).open().unwrap();
    drop(default_editor);

    let host_editor = WorktreeEditor::builder(&root.0, area)
        .binding_mode(WorktreeBindingMode::HostResolved {
            reserved_escape: Key::ctrl(KeyCode::Char(']')),
        })
        .presentation(WorktreePresentation::standalone().which_key(SurfaceOwnership::HostOwned))
        .open()
        .unwrap();
    drop(host_editor);

    let host_with_embedded = WorktreeEditor::builder(&root.0, area)
        .binding_mode(WorktreeBindingMode::HostResolved {
            reserved_escape: Key::ctrl(KeyCode::Char(']')),
        })
        .open()
        .unwrap_err();
    assert_eq!(
        host_with_embedded.kind(),
        WorktreeOpenErrorKind::Presentation
    );

    let facade_with_host = WorktreeEditor::builder(&root.0, area)
        .presentation(WorktreePresentation::standalone().which_key(SurfaceOwnership::HostOwned))
        .open()
        .unwrap_err();
    assert_eq!(facade_with_host.kind(), WorktreeOpenErrorKind::Presentation);
}

#[test]
fn embedded_profile_keeps_semantic_review_entry_executable() {
    let root = TestRoot::new("semantic-review-entry");
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .binding_mode(WorktreeBindingMode::HostResolved {
            reserved_escape: Key::ctrl(KeyCode::Char(']')),
        })
        .presentation(WorktreePresentation::standalone().which_key(SurfaceOwnership::HostOwned))
        .open()
        .unwrap();
    assert!(editor.binding_context().is_some());
    assert!(
        !editor
            .binding_manifest()
            .expect("host-resolved mode publishes a manifest")
            .entries()
            .iter()
            .any(|entry| { entry.command() == Command::OpenReview })
    );

    editor
        .command(Command::OpenReview, None, None, Duration::ZERO)
        .expect("direct semantic review entry remains executable");
    assert_eq!(editor.input_context().scope, BindingScope::Review);
}

#[test]
fn host_resolved_dispatch_and_cancellation_are_addressed_and_atomic() {
    let root = TestRoot::new("host-resolved-cancel");
    let escape = Key::ctrl(KeyCode::Char(']'));
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .binding_mode(WorktreeBindingMode::HostResolved {
            reserved_escape: escape,
        })
        .presentation(WorktreePresentation::standalone().which_key(SurfaceOwnership::HostOwned))
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

    let pointer = PointerEvent::new(
        CellPosition::new(10, 2),
        PointerModifiers::default(),
        PointerAction::Press(PointerButton::Left),
    );
    assert!(
        editor
            .input(WorktreeInput::Pointer(pointer), Duration::ZERO)
            .is_ok()
    );
    assert_eq!(
        editor.pointer(pointer, Duration::ZERO),
        WorktreeUpdate::Redraw
    );

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
        .presentation(WorktreePresentation::standalone().which_key(SurfaceOwnership::HostOwned))
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
fn host_owned_command_line_opens_without_internal_prompt_and_completes_names() {
    let root = TestRoot::new("host-command-open");
    let presentation = WorktreePresentation::standalone().command_line(SurfaceOwnership::HostOwned);
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .presentation(presentation)
        .command_surface(WorktreeCommandSurface::new())
        .open()
        .unwrap();
    let before = editor.input_context();
    let outcome = editor
        .command(Command::OpenCommandLine, None, None, Duration::ZERO)
        .unwrap();
    let WorktreeInputOutcome::Request(WorktreeInputRequest::OpenCommandLine(session)) = outcome
    else {
        panic!("host-owned command line must publish its session");
    };
    assert_eq!(editor.input_context(), before, "no hidden prompt opens");
    assert_ne!(session.get(), 0);

    let completion = editor.command_catalog().complete_names("wr");
    assert_eq!(completion.candidates(), &["write"]);
}

#[test]
fn host_command_execution_failure_keeps_session_open_and_close_returns_context() {
    let root = TestRoot::new("host-command-failure-atomicity");
    let presentation = WorktreePresentation::standalone().command_line(SurfaceOwnership::HostOwned);
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .presentation(presentation)
        .command_surface(WorktreeCommandSurface::new())
        .open()
        .unwrap();
    let WorktreeInputOutcome::Request(WorktreeInputRequest::OpenCommandLine(session)) = editor
        .command(Command::OpenCommandLine, None, None, Duration::ZERO)
        .unwrap()
    else {
        panic!("host-owned command line must open");
    };
    let addressed = editor.command_catalog().address(EditorCommandId::Write);

    assert!(
        editor
            .execute_session_command(session, addressed, "write!")
            .is_err()
    );
    editor
        .request_command_completion(
            session,
            EditorCommandRequestId::new(1).unwrap(),
            "edit src/m",
        )
        .expect("failed execution keeps the command session open");

    let context = editor.close_command_session(session).unwrap();
    assert!(context.phases.is_idle());
    assert_eq!(
        editor.close_command_session(session),
        Err(EditorCommandSessionError::StaleSession)
    );
}

#[test]
fn host_completion_errors_distinguish_session_identity_from_invalid_lines() {
    let root = TestRoot::new("host-command-errors");
    let presentation = WorktreePresentation::standalone().command_line(SurfaceOwnership::HostOwned);
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .presentation(presentation)
        .command_surface(WorktreeCommandSurface::new())
        .open()
        .unwrap();
    let WorktreeInputOutcome::Request(WorktreeInputRequest::OpenCommandLine(session)) = editor
        .command(Command::OpenCommandLine, None, None, Duration::ZERO)
        .unwrap()
    else {
        panic!("host-owned command line must open");
    };
    let request = EditorCommandRequestId::new(1).unwrap();

    assert_eq!(
        editor.request_command_completion(session, request, "write"),
        Err(EditorCommandSessionError::InvalidCompletion)
    );
    let oversized = format!("edit {}", "x".repeat(kvim_input::COMMAND_LINE_CHARS_MAX));
    assert_eq!(
        editor.request_command_completion(session, request, &oversized),
        Err(EditorCommandSessionError::InvalidCompletion)
    );
    editor.close_command_session(session).unwrap();
    assert_eq!(
        editor.request_command_completion(session, request, "write"),
        Err(EditorCommandSessionError::StaleSession)
    );
}

#[tokio::test]
async fn host_path_completion_routes_and_rejects_obsolete_or_closed_sessions() {
    let root = TestRoot::new("host-command-path-completion");
    fs::create_dir_all(root.0.join("src")).unwrap();
    fs::write(root.0.join("src/main.rs"), "fn main() {}\n").unwrap();
    let presentation = WorktreePresentation::standalone().command_line(SurfaceOwnership::HostOwned);
    let capacity = WorktreeCapacity::new(8, WORKER_CAPACITY_MAX, 1).unwrap();
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .capacity(capacity)
        .presentation(presentation)
        .command_surface(WorktreeCommandSurface::new())
        .open()
        .unwrap();
    let WorktreeInputOutcome::Request(WorktreeInputRequest::OpenCommandLine(session)) = editor
        .command(Command::OpenCommandLine, None, None, Duration::ZERO)
        .unwrap()
    else {
        panic!("host-owned command line must open");
    };
    let first = EditorCommandRequestId::new(1).unwrap();
    let newest = EditorCommandRequestId::new(2).unwrap();
    editor
        .request_command_completion(session, first, "edit src/m")
        .unwrap();
    editor.dispatch();
    editor
        .request_command_completion(session, newest, "edit src/m")
        .unwrap();
    editor.dispatch();
    let mut completion = None;
    for _ in 0..TEST_STEPS_MAX {
        let Ok(ready) = tokio::time::timeout(Duration::from_millis(250), editor.ready()).await
        else {
            break;
        };
        editor.apply(ready, Duration::ZERO).unwrap();
        completion = editor.take_command_completion();
        if completion.is_some() {
            break;
        }
    }
    let completion = completion.expect("the newest path completion must publish");
    assert_eq!(completion.session(), session);
    assert_eq!(completion.request(), newest);
    assert_eq!(completion.candidates(), &["edit src/main.rs"]);
    assert!(editor.take_command_completion().is_none());

    editor.close_command_session(session).unwrap();
    assert_eq!(
        editor.request_command_completion(session, first, "edit src/m"),
        Err(EditorCommandSessionError::StaleSession)
    );
}

#[test]
fn addressed_command_catalog_tracks_state_and_rejects_wrong_or_stale_routes() {
    let root = TestRoot::new("addressed-command-catalog");
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .open()
        .unwrap();
    let generated = editor.command_catalog();
    assert!(generated.descriptors().len() <= EDITOR_COMMAND_DESCRIPTORS_MAX);
    let write = generated
        .descriptors()
        .iter()
        .find(|entry| entry.id() == EditorCommandId::Write)
        .copied()
        .unwrap();
    assert_eq!(
        write.availability(),
        EditorCommandAvailability::RequiresFile
    );
    assert_eq!(write.qualified_name(), "editor.write");

    let quit = generated.address(EditorCommandId::Quit);
    let wrong = AddressedEditorCommand {
        instance: WorktreeInstanceId(editor.instance().0 + 1),
        generation: quit.generation(),
        id: quit.id(),
    };
    let before = editor.input_context();
    assert_eq!(
        editor.execute_addressed_command(wrong, "quit"),
        Err(EditorCommandExecutionError::WrongInstance),
    );
    assert_eq!(editor.input_context(), before);

    editor
        .command(Command::OpenCommandLine, None, None, Duration::ZERO)
        .unwrap();
    assert_eq!(
        editor.execute_addressed_command(quit, "quit"),
        Err(EditorCommandExecutionError::StaleGeneration),
    );
    assert_eq!(
        editor.execute_addressed_command(
            editor
                .command_catalog()
                .address(EditorCommandId::Diagnostics),
            "quit",
        ),
        Err(EditorCommandExecutionError::IdentityMismatch),
    );
}

#[test]
fn command_catalog_reflects_file_and_access_availability() {
    let root = TestRoot::new("command-catalog-availability");
    fs::write(root.0.join("note.txt"), "hello\n").unwrap();
    let mut editor = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .open()
        .unwrap();
    editor.open_file(WorktreeRelativePath::new("note.txt").unwrap());
    // File completion is asynchronous; availability changes after apply in the
    // lifecycle test. The generated-buffer state above owns the negative case.
    let edit = editor
        .command_catalog()
        .descriptors()
        .iter()
        .find(|entry| entry.id() == EditorCommandId::Edit)
        .copied()
        .unwrap();
    assert_eq!(edit.completion(), EditorCommandCompletion::ContainedPath);
    assert_eq!(edit.arguments(), EditorCommandArguments::ContainedPath);

    let view_only = WorktreeEditor::builder(&root.0, Rect::new(0, 0, 30, 6))
        .access(WorktreeAccess::ViewOnly)
        .open()
        .unwrap();
    let write = view_only
        .command_catalog()
        .descriptors()
        .iter()
        .find(|entry| entry.id() == EditorCommandId::Write)
        .copied()
        .unwrap();
    assert_eq!(
        write.availability(),
        EditorCommandAvailability::RequiresWriteAccess,
    );
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
    let opened_status = editor.status();
    assert_eq!(opened_status.instance(), editor.instance());
    assert_eq!(opened_status.mode(), Mode::Normal);
    assert_eq!(opened_status.path(), None);
    assert!(!opened_status.is_modified());
    assert_eq!(opened_status.cursor().line(), 1);
    assert_eq!(opened_status.cursor().column(), 1);
    assert_eq!(opened_status.access(), WorktreeAccess::ReadWrite);
    assert_eq!(opened_status.diagnostics().total(), 0);
    assert_eq!(opened_status.formatter(), EditorFormatterState::Unavailable);

    let mut cells = Buffer::empty(area);
    let cursor = editor.render(&mut cells).unwrap();
    assert!(cursor.position.is_some());

    editor.open_file(WorktreeRelativePath::new("note.txt").unwrap());
    drive_until(&mut editor, |event| {
        matches!(event, WorktreeEvent::ActiveFileChanged { path: Some(path) } if path.as_path() == Path::new("note.txt"))
    })
    .await;
    assert_eq!(
        editor.status().path().map(WorktreeRelativePath::as_path),
        Some(Path::new("note.txt"))
    );
    assert_eq!(
        editor.status().formatter(),
        EditorFormatterState::Unavailable
    );

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
    assert_eq!(editor.status().mode(), Mode::Insert);
    assert_eq!(editor.status().cursor().column(), 7);
    assert!(editor.status().is_modified());
    editor
        .command(Command::ReturnToNormal, None, None, Duration::ZERO)
        .unwrap();
    assert_eq!(editor.status().mode(), Mode::Normal);
    editor
        .command(Command::SaveBuffer, None, None, Duration::ZERO)
        .unwrap();
    drive_until(&mut editor, |event| {
        matches!(event, WorktreeEvent::FileWritten { path } if path.as_path() == Path::new("note.txt"))
    })
    .await;
    assert!(!editor.status().is_modified());
    assert_eq!(editor.status().cursor().column(), 7);
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

#[test]
fn status_reports_view_only_access_without_reserving_a_host_status_row() {
    let root = TestRoot::new("status-view-only");
    let area = Rect::new(0, 0, 30, 4);
    let editor = WorktreeEditor::builder(&root.0, area)
        .access(WorktreeAccess::ViewOnly)
        .presentation(WorktreePresentation::standalone().statusline(SurfaceOwnership::HostOwned))
        .open()
        .unwrap();

    assert_eq!(editor.status().access(), WorktreeAccess::ViewOnly);
    assert_eq!(editor.region_areas()[0].1.height, 3);
}

async fn take_until(
    editor: &mut WorktreeEditor,
    wanted: impl Fn(&WorktreeEvent) -> bool,
) -> WorktreeEvent {
    for _ in 0..TEST_STEPS_MAX {
        let _ = editor.dispatch();
        while let Some(event) = editor.take_event() {
            if wanted(&event) {
                return event;
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

async fn drive_until(editor: &mut WorktreeEditor, wanted: impl Fn(&WorktreeEvent) -> bool) {
    drop(take_until(editor, wanted).await);
}

#[tokio::test]
async fn recovery_event_is_bounded_addressed_and_rejects_wrong_or_stale_decisions() {
    let root = TestRoot::new("recovery-facade");
    let state = root.0.join("state");
    fs::write(root.0.join("note.txt"), "disk\n").unwrap();
    let path = WorktreeRelativePath::new("note.txt").unwrap();
    let area = Rect::new(0, 0, 30, 6);

    let mut writer = WorktreeEditor::builder(&root.0, area)
        .recovery_state_directory(&state)
        .open()
        .unwrap();
    writer.open_file(path.clone());
    drive_until(&mut writer, |event| {
        matches!(event, WorktreeEvent::ActiveFileChanged { path: Some(_) })
    })
    .await;
    writer
        .command(Command::InsertBeforeCursor, None, None, Duration::ZERO)
        .unwrap();
    writer.literal("recovered ", Duration::ZERO);
    match writer.shutdown(Duration::from_secs(5)).await {
        WorktreeShutdown::Finished { .. } => {}
        WorktreeShutdown::Draining(drain) => drop(drain.complete().await),
    }

    let mut owner = WorktreeEditor::builder(&root.0, area)
        .recovery_state_directory(&state)
        .open()
        .unwrap();
    owner.open_file(path.clone());
    let event = take_until(&mut owner, |event| {
        matches!(event, WorktreeEvent::RecoveryCandidate { .. })
    })
    .await;
    assert!(
        size_of_val(&event) <= 256,
        "the event carries no recovered text"
    );
    let WorktreeEvent::RecoveryCandidate {
        id,
        path: event_path,
        status,
    } = event
    else {
        unreachable!("the predicate selected a recovery event")
    };
    assert_eq!(event_path, path);
    assert_eq!(status, WorktreeRecoveryStatus::Current);
    assert_eq!(id.instance(), owner.instance());

    let other_root = TestRoot::new("recovery-facade-other");
    let mut other = WorktreeEditor::builder(&other_root.0, area).open().unwrap();
    assert_eq!(
        other.decide_recovery(&id, WorktreeRecoveryDecision::Discard),
        Err(WorktreeRecoveryError::WrongInstance)
    );
    assert_eq!(
        owner.decide_recovery(&id, WorktreeRecoveryDecision::Defer),
        Ok(WorktreeRecoveryOutcome::Deferred)
    );
    assert_eq!(
        owner.decide_recovery(&id, WorktreeRecoveryDecision::Restore),
        Err(WorktreeRecoveryError::Stale)
    );
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
