use std::fs;
use std::time::Duration;

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
        instance: kvim_tui::EditorInstanceId::allocate(),
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
fn watcher_open_error_keeps_the_facade_kind_and_source() {
    let source = std::io::Error::other("watcher start refused");
    let error = WorktreeOpenError::new(WorktreeOpenErrorKind::Watcher, None, source);

    assert_eq!(error.kind(), WorktreeOpenErrorKind::Watcher);
    assert!(StdError::source(&error).is_some());
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
        let _ = editor.apply(completion, Duration::ZERO);
    }
    panic!("bounded lifecycle did not produce the expected event");
}

#[cfg(feature = "grammar-rust")]
#[test]
fn rust_feature_populates_the_registry_used_by_the_facade() {
    assert!(
        LanguageRegistry::first_release()
            .adapter(Path::new("src/lib.rs"))
            .is_ok()
    );
}
