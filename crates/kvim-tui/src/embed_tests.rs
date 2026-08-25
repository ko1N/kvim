//! Tests for the embedding contract of one editor instance.
//!
//! Every test drives one host-owned editor. No test opens a terminal: the
//! editor receives resolved commands, literal text, an explicit rectangle, and
//! an elapsed time, and it renders into a cell buffer that the test owns. See
//! `docs/embedding.md`.

use std::path::Path;
use std::time::Duration;

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;

use tokio::time::timeout;

use kvim_input::{Command, CommandAuthority, PasteText};
use kvim_path::WorktreeRelativePath;
use kvim_runtime::{RuntimeLimits, WORKER_CONCURRENCY_LIMIT_MAX};
use kvim_settings::EditorSettings;
use kvim_workspace::temp::TempDir;
use kvim_workspace::{EntryKind, FileOperation, TransferMode, WorkspaceRequest};

use crate::embed::{
    CursorShape, EDITOR_EVENTS_MAX, EditorAccess, EditorCapacity, EditorEvent, EditorShutdown,
    EmbeddedEditor, GeometryError, InputRequest, PublishedEvent, Refusal,
};
use crate::session::{Redraw, RunState, Session, test_root};

const NOW: Duration = Duration::ZERO;

/// The rectangle that most tests give the editor.
const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

/// The largest number of queued host operations that one test drains.
const DRAIN_STEPS_MAX: usize = 64;

/// Creates one editor over one temporary workspace.
fn editor(root: &Path) -> Session {
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    Session::new(AREA, settings, test_root(root.to_path_buf()))
}

/// Runs every queued language, file, and workspace request of one editor.
///
/// The host performs the identical step, so the editor reaches the same state
/// that a running host would show.
fn settle(session: &mut Session) {
    for _ in 0..DRAIN_STEPS_MAX {
        let mut worked = false;
        while let Some(request) = session.take_language_request() {
            let _ = session
                .apply_language_dispatch(&request, Err(kvim_language::LspError::NoServerDeclared));
            worked = true;
        }
        if let Some(request) = session.take_file_request() {
            let _ = session.apply_file_result(request.run());
            worked = true;
        }
        if let Some(request) = session.take_workspace_request() {
            let _ = session.apply_workspace_result(request.run());
            worked = true;
        }
        if !worked {
            return;
        }
    }
    panic!("one transition queues fewer operations than the drain bound");
}

/// Takes every published event of one editor.
fn drain_events(session: &mut Session) -> Vec<PublishedEvent> {
    let mut events = Vec::new();
    while let Some(event) = session.take_event() {
        assert!(
            events.len() < EDITOR_EVENTS_MAX + 2,
            "the outbox is bounded, so a drain always ends"
        );
        events.push(event);
    }
    events
}

/// Opens the file tree and gives it the focus, as `Ctrl-E` does.
fn reveal_tree(session: &mut Session) {
    let _ = session.apply_command(Command::RevealInFileTree, None, None, NOW);
    settle(session);
}

/// Runs one file-tree prompt: the command, the typed line, and `Enter`.
fn run_tree_prompt(session: &mut Session, command: Command, text: &str) {
    let _ = session.apply_command(command, None, None, NOW);
    for value in text.chars() {
        let _ = session.insert_literal(&value.to_string(), NOW);
    }
    let _ = session.apply_command(Command::PromptAccept, None, None, NOW);
    settle(session);
}

#[test]
fn a_non_zero_origin_paints_only_inside_the_editor_rectangle() {
    let directory = TempDir::new("embed-origin");
    let area = Rect::new(3, 2, 40, 12);
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    let session = Session::new(area, settings, test_root(directory.path.clone()));

    let empty = CellBuffer::empty(Rect::new(0, 0, 60, 20));
    let mut cells = empty.clone();
    let cursor = session
        .draw(&mut cells, area)
        .expect("the rectangle fits the cell buffer");

    assert_eq!(cursor.shape, CursorShape::Block);
    let position = cursor.position.expect("the focused window shows a cursor");
    assert!(
        position.x >= area.x && position.y >= area.y,
        "the cursor sits inside the editor rectangle"
    );
    for y in empty.area.y..empty.area.bottom() {
        for x in empty.area.x..empty.area.right() {
            let inside = x >= area.x && x < area.right() && y >= area.y && y < area.bottom();
            if inside {
                continue;
            }
            assert_eq!(
                cells[(x, y)],
                empty[(x, y)],
                "the editor changed the cell at {x},{y} outside its rectangle"
            );
        }
    }
    assert_ne!(cells, empty, "the editor painted its own rectangle");
}

#[test]
fn an_out_of_buffer_rectangle_returns_a_typed_error_and_changes_no_cell() {
    let directory = TempDir::new("embed-outside");
    let area = Rect::new(40, 1, 40, 12);
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    let session = Session::new(area, settings, test_root(directory.path.clone()));

    let empty = CellBuffer::empty(Rect::new(0, 0, 60, 20));
    let mut cells = empty.clone();
    let error = session
        .draw(&mut cells, area)
        .expect_err("the rectangle leaves the cell buffer");

    assert_eq!(
        error,
        GeometryError::OutsideBuffer {
            area,
            buffer: empty.area,
        }
    );
    assert_eq!(cells, empty, "an invalid rectangle changes no cell");
}

#[test]
fn an_empty_rectangle_returns_a_typed_error_and_keeps_the_accepted_area() {
    let directory = TempDir::new("embed-empty");
    let mut session = editor(&directory.path);
    let empty = CellBuffer::empty(Rect::new(0, 0, 80, 24));
    let mut cells = empty.clone();

    let flat = Rect::new(0, 0, 80, 0);
    assert_eq!(
        session.set_area(flat).expect_err("the rectangle is empty"),
        GeometryError::Empty { area: flat }
    );
    assert_eq!(session.area(), AREA, "the editor keeps its accepted area");
    assert_eq!(
        session
            .draw(&mut cells, flat)
            .expect_err("the rectangle is empty"),
        GeometryError::Empty { area: flat }
    );
    assert_eq!(cells, empty, "an invalid rectangle changes no cell");
}

#[test]
fn a_rectangle_that_the_editor_never_accepted_returns_a_typed_error() {
    let directory = TempDir::new("embed-unreconciled");
    let mut session = editor(&directory.path);
    let empty = CellBuffer::empty(Rect::new(0, 0, 80, 24));
    let mut cells = empty.clone();

    let other = Rect::new(0, 0, 40, 12);
    assert_eq!(
        session
            .draw(&mut cells, other)
            .expect_err("the editor accepted another rectangle"),
        GeometryError::Unreconciled {
            area: other,
            accepted: AREA,
        }
    );
    assert_eq!(cells, empty, "an invalid rectangle changes no cell");

    session.set_area(other).expect("the rectangle holds cells");
    session
        .draw(&mut cells, other)
        .expect("the editor accepted the rectangle");
}

#[test]
fn view_only_access_refuses_every_mutating_command() {
    let directory = TempDir::new("embed-view-only-commands");
    let path = directory.write("main.rs", "one\ntwo\n");
    let mut session = editor(&directory.path).with_access(EditorAccess::ViewOnly);
    session.open_path(path);
    settle(&mut session);
    let before = session.buffer().to_string();

    let mutating: Vec<Command> = Command::ALL
        .iter()
        .copied()
        .filter(|command| command.authority() != CommandAuthority::Read)
        .collect();
    assert!(
        mutating.len() >= 30,
        "the command table holds every mutating command"
    );
    for command in mutating {
        let reduction = session.apply_command(command, None, None, NOW);
        assert_eq!(
            reduction.refusal(),
            Some(Refusal::ViewOnly),
            "{} reached a durable change under view-only access",
            command.id()
        );
        assert!(
            session.take_file_request().is_none(),
            "{} started a file operation under view-only access",
            command.id()
        );
        assert!(
            session.take_workspace_request().is_none(),
            "{} started a workspace operation under view-only access",
            command.id()
        );
    }
    assert_eq!(
        session.buffer().to_string(),
        before,
        "view-only access changed no text"
    );
}

#[test]
fn view_only_access_refuses_literal_text_and_paste() {
    let directory = TempDir::new("embed-view-only-text");
    let path = directory.write("main.rs", "one\n");
    let mut session = editor(&directory.path).with_access(EditorAccess::ViewOnly);
    session.open_path(path);
    settle(&mut session);

    assert_eq!(
        session.insert_literal("x", NOW).refusal(),
        Some(Refusal::ViewOnly)
    );
    let block = PasteText::new("pasted").expect("the block is bounded");
    assert_eq!(
        session.paste(&block, NOW).refusal(),
        Some(Refusal::ViewOnly)
    );
    assert_eq!(session.buffer().to_string(), "one\n");
}

#[test]
fn view_only_access_refuses_a_workspace_mutation() {
    let directory = TempDir::new("embed-view-only-workspace");
    directory.file("README.md", "kvim\n");
    let mut session = editor(&directory.path).with_access(EditorAccess::ViewOnly);
    reveal_tree(&mut session);

    let reduction = session.apply_command(Command::TreeAddFile, None, None, NOW);
    assert_eq!(reduction.refusal(), Some(Refusal::ViewOnly));
    settle(&mut session);
    assert!(
        !directory.path.join("added.rs").exists(),
        "view-only access created no entry"
    );
}

#[test]
fn read_write_access_keeps_the_editing_and_saving_behavior() {
    let directory = TempDir::new("embed-read-write");
    let path = directory.write("main.rs", "one\n");
    let mut session = editor(&directory.path);
    session.open_path(path.clone());
    settle(&mut session);

    assert_eq!(session.access(), EditorAccess::ReadWrite);
    let _ = session.apply_command(Command::InsertBeforeCursor, None, None, NOW);
    let _ = session.insert_literal("new ", NOW);
    let _ = session.apply_command(Command::ReturnToNormal, None, None, NOW);
    assert_eq!(session.buffer().to_string(), "new one\n");

    let _ = session.apply_command(Command::SaveBuffer, None, None, NOW);
    settle(&mut session);
    assert_eq!(
        std::fs::read_to_string(&path).expect("the save wrote the file"),
        "new one\n"
    );
}

/// Fills the bounded outbox with the facts of completed writes.
///
/// The host reads no event, so every completed write keeps its slot. The
/// editor then holds no free slot for the next durable operation.
fn saturate(session: &mut Session) {
    for _ in 0..EDITOR_EVENTS_MAX {
        let reduction = session.apply_command(Command::SaveBuffer, None, None, NOW);
        assert_eq!(reduction.refusal(), None, "the outbox still holds one slot");
        settle(session);
    }
}

#[test]
fn a_saturated_outbox_refuses_a_save_before_the_write_starts() {
    let directory = TempDir::new("embed-saturated-save");
    let path = directory.write("main.rs", "one\n");
    let mut session = editor(&directory.path);
    session.open_path(path);
    settle(&mut session);
    saturate(&mut session);

    let reduction = session.apply_command(Command::SaveBuffer, None, None, NOW);
    assert_eq!(reduction.refusal(), Some(Refusal::Saturated));
    assert!(
        session.take_file_request().is_none(),
        "a refused save starts no write"
    );

    // One read frees one slot, so the next save runs and publishes its fact.
    let first = session.take_event().expect("the outbox holds every fact");
    assert!(matches!(
        first.event,
        EditorEvent::FileWritten { .. } | EditorEvent::RedrawRequested
    ));
    assert_eq!(
        session
            .apply_command(Command::SaveBuffer, None, None, NOW)
            .refusal(),
        None
    );
}

#[test]
fn a_saturated_outbox_refuses_a_workspace_mutation_before_it_starts() {
    let directory = TempDir::new("embed-saturated-mutation");
    directory.file("README.md", "kvim\n");
    let path = directory.write("main.rs", "one\n");
    let mut session = editor(&directory.path);
    session.open_path(path);
    settle(&mut session);
    reveal_tree(&mut session);
    saturate(&mut session);

    let _ = session.apply_command(Command::TreeAddFile, None, None, NOW);
    for value in "added.rs".chars() {
        let _ = session.insert_literal(&value.to_string(), NOW);
    }
    let reduction = session.apply_command(Command::PromptAccept, None, None, NOW);
    assert_eq!(reduction.refusal(), Some(Refusal::Saturated));
    while let Some(request) = session.take_workspace_request() {
        assert!(
            !matches!(request, WorkspaceRequest::Mutate(_)),
            "a refused mutation reaches no worker"
        );
        let _ = session.apply_workspace_result(request.run());
    }
    assert!(
        !directory.path.join("added.rs").exists(),
        "a refused mutation creates no entry"
    );
}

#[test]
fn a_completed_write_publishes_its_mandatory_fact() {
    let directory = TempDir::new("embed-write-fact");
    let path = directory.write("main.rs", "one\n");
    let mut session = editor(&directory.path);
    session.open_path(path);
    settle(&mut session);
    let _ = drain_events(&mut session);

    let _ = session.apply_command(Command::SaveBuffer, None, None, NOW);
    settle(&mut session);

    let written = WorktreeRelativePath::new("main.rs").expect("the path is contained");
    let events = drain_events(&mut session);
    assert!(
        events.iter().any(|published| published.event
            == EditorEvent::FileWritten {
                path: written.clone()
            }),
        "a completed write publishes its fact: {events:?}"
    );
}

#[test]
fn a_failed_write_releases_its_reserved_slot() {
    let directory = TempDir::new("embed-write-failure");
    let path = directory.write("main.rs", "one\n");
    let mut session = editor(&directory.path);
    session.open_path(path);
    settle(&mut session);
    let _ = drain_events(&mut session);

    let _ = session.apply_command(Command::SaveBuffer, None, None, NOW);
    while let Some(request) = session.take_language_request() {
        let _ = session
            .apply_language_dispatch(&request, Err(kvim_language::LspError::NoServerDeclared));
    }
    let _ = session
        .take_file_request()
        .expect("the save queued one write");
    let _ = session.abandon_file_request(crate::session::FileRequestFailure::Saturated);

    let events = drain_events(&mut session);
    assert!(
        !events
            .iter()
            .any(|published| matches!(published.event, EditorEvent::FileWritten { .. })),
        "a refused write publishes no fact"
    );
    // The released slot is usable again, so the next save runs.
    assert_eq!(
        session
            .apply_command(Command::SaveBuffer, None, None, NOW)
            .refusal(),
        None
    );
}

#[test]
fn every_workspace_mutation_publishes_its_fact() {
    let directory = TempDir::new("embed-workspace-facts");
    directory.file("README.md", "kvim\n");
    directory.dir("docs");
    let mut session = editor(&directory.path);
    reveal_tree(&mut session);
    let _ = drain_events(&mut session);

    // Create beside the selected file of the workspace root.
    select_named(&mut session, "README.md");
    run_tree_prompt(&mut session, Command::TreeAddFile, "added.rs");
    assert_eq!(
        one_mutation(&mut session),
        FileOperation::Create {
            path: relative("added.rs"),
            kind: EntryKind::File,
        }
    );

    // Rename.
    run_tree_prompt(&mut session, Command::TreeRename, "moved.rs");
    assert_eq!(
        one_mutation(&mut session),
        FileOperation::Rename {
            from: relative("added.rs"),
            to: relative("moved.rs"),
        }
    );

    // Copy into the `docs` directory.
    let _ = session.apply_command(Command::TreeCopyEntry, None, None, NOW);
    select_named(&mut session, "docs");
    let _ = session.apply_command(Command::TreePasteEntries, None, None, NOW);
    settle(&mut session);
    assert_eq!(
        one_mutation(&mut session),
        FileOperation::Transfer {
            mode: TransferMode::Copy,
            sources: vec![relative("moved.rs")],
            destination: kvim_path::WorktreeDirectoryPath::Relative(relative("docs")),
        }
    );

    // Delete the copy, so the `docs` directory can take the source instead.
    select_copy_in_docs(&mut session);
    let _ = session.apply_command(Command::TreeDelete, None, None, NOW);
    let _ = session.insert_literal("y", NOW);
    let _ = session.apply_command(Command::PromptAccept, None, None, NOW);
    settle(&mut session);
    assert!(
        matches!(one_mutation(&mut session), FileOperation::Delete { .. }),
        "the confirmed delete publishes its fact"
    );

    // Move the source into the `docs` directory that the delete emptied.
    select_named(&mut session, "moved.rs");
    let _ = session.apply_command(Command::TreeCutEntry, None, None, NOW);
    select_named(&mut session, "docs");
    let _ = session.apply_command(Command::TreePasteEntries, None, None, NOW);
    settle(&mut session);
    assert!(
        matches!(
            one_mutation(&mut session),
            FileOperation::Transfer {
                mode: TransferMode::Move,
                ..
            }
        ),
        "the paste of a cut entry moves it"
    );
}

/// Returns the one workspace fact that the editor published.
fn one_mutation(session: &mut Session) -> FileOperation {
    let events = drain_events(session);
    let mut facts = events.into_iter().filter_map(|published| {
        assert_eq!(published.instance, session.instance());
        match published.event {
            EditorEvent::WorkspaceChanged { operation } => Some(operation),
            _ => None,
        }
    });
    let operation = facts.next().expect("the mutation published one fact");
    assert!(facts.next().is_none(), "one mutation publishes one fact");
    operation
}

/// Returns one contained path of the test workspace.
fn relative(path: &str) -> WorktreeRelativePath {
    WorktreeRelativePath::new(path).expect("the test path is contained")
}

/// Selects the sidebar row of one entry name.
fn select_named(session: &mut Session, name: &str) {
    let _ = session.apply_command(Command::MoveFirstLine, None, None, NOW);
    for _ in 0..DRAIN_STEPS_MAX {
        if selected_name(session) == name {
            return;
        }
        let _ = session.apply_command(Command::MoveDown, None, None, NOW);
    }
    panic!("the sidebar shows one row of {name}");
}

/// Expands `docs` and selects the copied entry inside it.
fn select_copy_in_docs(session: &mut Session) {
    select_named(session, "docs");
    let _ = session.apply_command(Command::TreeExpandEntry, None, None, NOW);
    settle(session);
    select_named(session, "moved.rs");
    assert!(
        session.file_tree().selected().is_some_and(|path| path
            .parent()
            .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "docs"))),
        "the sidebar selects the copy inside the docs directory"
    );
}

/// Returns the file name of the selected sidebar entry.
fn selected_name(session: &Session) -> String {
    session
        .file_tree()
        .selected()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

#[test]
fn an_active_file_change_publishes_its_path() {
    let directory = TempDir::new("embed-active-file");
    let path = directory.write("main.rs", "one\n");
    let mut session = editor(&directory.path);

    let _ = session.open(relative("main.rs"));
    settle(&mut session);

    let events = drain_events(&mut session);
    assert!(
        events.iter().any(|published| published.event
            == EditorEvent::ActiveFileChanged {
                path: Some(relative("main.rs")),
            }),
        "the open published the new active file: {events:?}"
    );
    assert!(path.exists());
}

#[test]
fn a_visible_change_publishes_one_coalesced_redraw_request() {
    let directory = TempDir::new("embed-redraw");
    let mut session = editor(&directory.path);
    let _ = drain_events(&mut session);

    let _ = session.apply_command(Command::SplitAdaptive, None, None, NOW);
    let _ = session.apply_command(Command::SplitAdaptive, None, None, NOW);

    let events = drain_events(&mut session);
    let redraws = events
        .iter()
        .filter(|published| published.event == EditorEvent::RedrawRequested)
        .count();
    assert_eq!(redraws, 1, "the redraw latch coalesces every change");
}

#[test]
fn a_focus_move_at_the_edge_reports_the_boundary() {
    let directory = TempDir::new("embed-focus-boundary");
    let mut session = editor(&directory.path);

    let reduction = session.apply_command(Command::FocusWindowLeft, None, None, NOW);
    assert_eq!(
        reduction.request(),
        Some(InputRequest::FocusBoundary(crate::Direction::Left))
    );
    assert_eq!(
        reduction.request().map(InputRequest::event),
        Some(EditorEvent::FocusBoundary(crate::Direction::Left))
    );

    // A move that stays inside this editor reports no boundary.
    let _ = session.apply_command(Command::SplitAdaptive, None, None, NOW);
    assert_eq!(
        session
            .apply_command(Command::FocusWindowLeft, None, None, NOW)
            .request(),
        None
    );
}

#[test]
fn the_last_window_close_reports_the_close_request() {
    let directory = TempDir::new("embed-close");
    let mut session = editor(&directory.path);

    let _ = session.apply_command(Command::SplitAdaptive, None, None, NOW);
    assert_eq!(
        session
            .apply_command(Command::CloseWindow, None, None, NOW)
            .request(),
        None,
        "one window remains"
    );

    let reduction = session.apply_command(Command::CloseWindow, None, None, NOW);
    assert_eq!(reduction.request(), Some(InputRequest::CloseRequested));
    assert_eq!(session.run_state(), RunState::Finished);
}

#[test]
fn the_published_context_names_the_scope_of_the_editor() {
    let directory = TempDir::new("embed-context");
    let mut session = editor(&directory.path);
    let normal = session.input_context();
    assert!(normal.phases.is_idle());

    let _ = session.apply_command(Command::InsertBeforeCursor, None, None, NOW);
    let insert = session.input_context();
    assert_ne!(
        insert.scope, normal.scope,
        "the mode change publishes a scope"
    );
    assert_ne!(
        insert.generation, normal.generation,
        "every context change publishes a new generation"
    );
    assert_ne!(
        insert.text_fallback, normal.text_fallback,
        "Insert mode names the editor as the owner of printable input"
    );
}

#[test]
fn a_cancel_resets_the_prompt_phase_and_publishes_a_new_generation() {
    let directory = TempDir::new("embed-cancel");
    let mut session = editor(&directory.path);

    let _ = session.apply_command(Command::OpenCommandLine, None, None, NOW);
    let open = session.input_context();
    assert!(
        open.phases.prompt.is_pending(),
        "the open command line holds the prompt phase"
    );

    // The composer addresses this effect to the editor before it moves focus.
    let reduction = session.cancel_pending(NOW);
    assert_eq!(reduction.refusal(), None);
    let reset = session.input_context();
    assert!(
        reset.phases.is_idle(),
        "every named phase resets, so the composer can commit its transition"
    );
    assert_ne!(
        reset.generation, open.generation,
        "the reset publishes a new generation, which the composer validates"
    );
    assert_eq!(
        session.input_context().phases.prompt,
        kvim_input::Phase::Empty
    );
}

#[test]
fn a_cancel_of_an_idle_editor_still_publishes_a_new_generation() {
    let directory = TempDir::new("embed-cancel-idle");
    let mut session = editor(&directory.path);
    let before = session.input_context();
    let _reduction = session.cancel_pending(NOW);
    let after = session.input_context();
    assert!(after.phases.is_idle());
    assert_ne!(after.generation, before.generation);
}

#[test]
fn the_editor_events_hold_no_review_fact() {
    // The match is exhaustive, so a review fact inside `EditorEvent` fails to
    // compile here. A review surface publishes its own typed `ReviewEvent`
    // values instead. See `docs/embedding.md`.
    let names: Vec<&str> = vec![
        EditorEvent::ActiveFileChanged { path: None },
        EditorEvent::FileWritten {
            path: relative("main.rs"),
        },
        EditorEvent::WorkspaceChanged {
            operation: FileOperation::Delete {
                paths: vec![relative("main.rs")],
            },
        },
        EditorEvent::RedrawRequested,
        EditorEvent::FocusBoundary(crate::Direction::Left),
        EditorEvent::CloseRequested,
    ]
    .into_iter()
    .map(|event| match event {
        EditorEvent::ActiveFileChanged { .. } => "active-file-changed",
        EditorEvent::FileWritten { .. } => "file-written",
        EditorEvent::WorkspaceChanged { .. } => "workspace-changed",
        EditorEvent::RedrawRequested => "redraw-requested",
        EditorEvent::FocusBoundary(_) => "focus-boundary",
        EditorEvent::CloseRequested => "close-requested",
    })
    .collect();
    assert_eq!(
        names,
        vec![
            "active-file-changed",
            "file-written",
            "workspace-changed",
            "redraw-requested",
            "focus-boundary",
            "close-requested",
        ]
    );
}

#[test]
fn every_published_event_carries_its_instance_identity() {
    let directory = TempDir::new("embed-identity");
    directory.file("main.rs", "one\n");
    let mut first = editor(&directory.path);
    let second = editor(&directory.path);
    assert_ne!(
        first.instance(),
        second.instance(),
        "two editors never share an identity"
    );

    let _ = first.open(relative("main.rs"));
    settle(&mut first);
    let _ = first.apply_command(Command::SaveBuffer, None, None, NOW);
    settle(&mut first);

    let events = drain_events(&mut first);
    assert!(
        events.len() >= 3,
        "the run published every fact: {events:?}"
    );
    for published in &events {
        assert_eq!(published.instance, first.instance());
        assert_ne!(published.instance, second.instance());
    }
    assert_eq!(
        first
            .apply_command(Command::MoveDown, None, None, NOW)
            .instance,
        first.instance()
    );
}

#[test]
fn an_accepted_area_change_reports_one_frame() {
    let directory = TempDir::new("embed-area");
    let mut session = editor(&directory.path);
    let _ = drain_events(&mut session);

    let area = Rect::new(2, 1, 40, 12);
    assert_eq!(
        session.set_area(area).expect("the rectangle holds cells"),
        Redraw::Needed
    );
    assert_eq!(session.area(), area);
    let events = drain_events(&mut session);
    assert_eq!(
        events
            .iter()
            .filter(|published| published.event == EditorEvent::RedrawRequested)
            .count(),
        1
    );
}

#[test]
fn a_saturated_editor_leaves_the_outbox_of_its_neighbour_free() {
    let directory = TempDir::new("embed-saturated-neighbour");
    let first_path = directory.write("first.rs", "one\n");
    let second_path = directory.write("second.rs", "two\n");
    let mut first = editor(&directory.path);
    let mut second = editor(&directory.path);
    assert_ne!(
        first.instance(),
        second.instance(),
        "two editors of one process never share an identity"
    );
    first.open_path(first_path);
    settle(&mut first);
    second.open_path(second_path);
    settle(&mut second);

    saturate(&mut first);
    assert_eq!(
        first
            .apply_command(Command::SaveBuffer, None, None, NOW)
            .refusal(),
        Some(Refusal::Saturated)
    );

    // The queue of the second editor holds every slot of its own bound, so the
    // saturated neighbour refuses nothing here.
    let reduction = second.apply_command(Command::SaveBuffer, None, None, NOW);
    assert_eq!(reduction.refusal(), None);
    settle(&mut second);
    let written = relative("second.rs");
    assert!(
        drain_events(&mut second).iter().any(|published| {
            published.instance == second.instance()
                && published.event
                    == EditorEvent::FileWritten {
                        path: written.clone(),
                    }
        }),
        "the neighbour publishes the fact of its own completed write"
    );
}

/// The steps that one embedded test loop runs before it reports a defect.
///
/// One step hands every queued request to the spawner and applies one result,
/// so this bound covers every chain that one command of these tests starts.
const DRIVE_STEPS_MAX: usize = 64;

/// The time that one step of an embedded test loop waits for a result.
const STEP_DEADLINE: Duration = Duration::from_secs(10);

/// The time that an embedded test gives the background work of one editor.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

/// The results that the spawner of one embedded test editor holds.
const RESULT_QUEUE: usize = 64;

/// The external processes of one embedded test editor that run together.
const PROCESSES: usize = 4;

/// Builds one embedded editor that owns every permit of its own.
///
/// A shared pool would let the work of one editor wait for the work of
/// another, so every test below names its own capacity.
fn embedded(root: &Path, area: Rect) -> EmbeddedEditor {
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    let limits = RuntimeLimits::new(RESULT_QUEUE, WORKER_CONCURRENCY_LIMIT_MAX, PROCESSES)
        .expect("every capacity is nonzero");
    EmbeddedEditor::builder(test_root(root.to_path_buf()), area)
        .settings(settings)
        .capacity(EditorCapacity::Isolated(limits))
        .open()
        .expect("the rectangle holds cells")
}

/// Drives one embedded editor until it publishes the wanted event.
///
/// The loop is the host: it hands every queued request to the spawner, reads
/// every published event, and applies every result.
async fn drive_until(
    editor: &mut EmbeddedEditor,
    wanted: fn(&EditorEvent) -> bool,
) -> Vec<PublishedEvent> {
    let mut seen = Vec::new();
    for _ in 0..DRIVE_STEPS_MAX {
        let _redraw = editor.dispatch();
        while let Some(published) = editor.take_event() {
            let found = wanted(&published.event);
            seen.push(published);
            if found {
                return seen;
            }
        }
        let completed = timeout(STEP_DEADLINE, editor.recv())
            .await
            .expect("one step of one editor answers inside its deadline");
        let _redraw = editor.apply(completed, NOW);
    }
    panic!("one editor publishes its event inside the step bound: {seen:?}");
}

/// Reports whether one event names the file that an editor now shows.
fn is_active_file(event: &EditorEvent) -> bool {
    matches!(event, EditorEvent::ActiveFileChanged { .. })
}

/// Reports whether one event names one completed write.
fn is_file_written(event: &EditorEvent) -> bool {
    matches!(event, EditorEvent::FileWritten { .. })
}

/// Returns the text of every row inside one rectangle of the host cells.
fn rendered_text(cells: &CellBuffer, area: Rect) -> String {
    let mut text = String::new();
    for row in 0..area.height {
        for column in 0..area.width {
            text.push_str(cells[(area.x + column, area.y + row)].symbol());
        }
        text.push('\n');
    }
    text
}

/// Types one run of text into one embedded editor and returns to Normal mode.
fn type_text(editor: &mut EmbeddedEditor, text: &str) {
    let _ = editor.command(Command::InsertBeforeCursor, None, None, NOW);
    let _ = editor.insert_literal(text, NOW);
    let _ = editor.command(Command::ReturnToNormal, None, None, NOW);
}

#[test]
fn an_editor_rectangle_without_cells_returns_a_typed_error() {
    let directory = TempDir::new("embedded-empty-area");
    let area = Rect::new(0, 0, 0, 24);
    let refused = EmbeddedEditor::builder(test_root(directory.path.clone()), area)
        .open()
        .expect_err("a rectangle without cells builds no editor");
    assert_eq!(refused, GeometryError::Empty { area });
}

#[tokio::test]
async fn two_editors_on_one_root_publish_only_their_own_facts() {
    let directory = TempDir::new("embedded-one-root");
    directory.file("first.rs", "one\n");
    directory.file("second.rs", "two\n");
    let mut first = embedded(&directory.path, AREA);
    let mut second = embedded(&directory.path, AREA);
    assert_ne!(
        first.instance(),
        second.instance(),
        "two editors of one root never share an identity"
    );

    let _redraw = first.open_file(relative("first.rs"));
    let _redraw = second.open_file(relative("second.rs"));
    let opened_first = drive_until(&mut first, is_active_file).await;
    let opened_second = drive_until(&mut second, is_active_file).await;

    assert!(
        opened_first
            .iter()
            .all(|published| published.instance == first.instance()),
        "every fact of one editor carries its identity: {opened_first:?}"
    );
    assert!(
        opened_second
            .iter()
            .all(|published| published.instance == second.instance()),
        "every fact of one editor carries its identity: {opened_second:?}"
    );
    assert!(opened_first.iter().any(|published| published.event
        == EditorEvent::ActiveFileChanged {
            path: Some(relative("first.rs"))
        }));
    assert!(opened_second.iter().any(|published| published.event
        == EditorEvent::ActiveFileChanged {
            path: Some(relative("second.rs"))
        }));

    assert!(matches!(
        first.shutdown(SHUTDOWN_DEADLINE).await,
        EditorShutdown::Finished { .. }
    ));
    assert!(matches!(
        second.shutdown(SHUTDOWN_DEADLINE).await,
        EditorShutdown::Finished { .. }
    ));
}

#[tokio::test]
async fn two_editors_on_different_roots_edit_render_and_shut_down_independently() {
    const LEFT_AREA: Rect = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 24,
    };
    const RIGHT_AREA: Rect = Rect {
        x: 40,
        y: 0,
        width: 40,
        height: 24,
    };
    const LEFT_TEXT: &str = "leftward";
    const RIGHT_TEXT: &str = "rightward";

    let left_root = TempDir::new("embedded-left-root");
    left_root.file("note.rs", "one\n");
    let right_root = TempDir::new("embedded-right-root");
    right_root.file("note.rs", "two\n");
    let mut left = embedded(&left_root.path, LEFT_AREA);
    let mut right = embedded(&right_root.path, RIGHT_AREA);

    let _redraw = left.open_file(relative("note.rs"));
    let _redraw = right.open_file(relative("note.rs"));
    let _opened = drive_until(&mut left, is_active_file).await;
    let _opened = drive_until(&mut right, is_active_file).await;

    type_text(&mut left, LEFT_TEXT);
    type_text(&mut right, RIGHT_TEXT);

    // One host buffer holds both editors. Each one writes only inside the
    // rectangle that it accepted.
    let mut cells = CellBuffer::empty(AREA);
    let _cursor = left
        .draw(&mut cells, LEFT_AREA)
        .expect("the left half fits");
    let _cursor = right
        .draw(&mut cells, RIGHT_AREA)
        .expect("the right half fits");
    let left_text = rendered_text(&cells, LEFT_AREA);
    let right_text = rendered_text(&cells, RIGHT_AREA);
    assert!(left_text.contains(LEFT_TEXT), "{left_text}");
    assert!(!left_text.contains(RIGHT_TEXT), "{left_text}");
    assert!(right_text.contains(RIGHT_TEXT), "{right_text}");
    assert!(!right_text.contains(LEFT_TEXT), "{right_text}");

    // The shutdown of one editor cancels the pre-commit work of that editor
    // alone, so the other editor still writes its file and publishes its fact.
    assert!(matches!(
        left.shutdown(SHUTDOWN_DEADLINE).await,
        EditorShutdown::Finished { .. }
    ));

    let _reduction = right.command(Command::SaveBuffer, None, None, NOW);
    let written = drive_until(&mut right, is_file_written).await;
    assert!(
        written
            .iter()
            .all(|published| published.instance == right.instance()),
        "the surviving editor publishes its own facts: {written:?}"
    );
    assert!(
        std::fs::read_to_string(right_root.join("note.rs"))
            .expect("the save wrote the file")
            .contains(RIGHT_TEXT)
    );
    assert!(
        std::fs::read_to_string(left_root.join("note.rs")).expect("the left file stays readable")
            == "one\n",
        "the closed editor wrote no file of its own root"
    );

    assert!(matches!(
        right.shutdown(SHUTDOWN_DEADLINE).await,
        EditorShutdown::Finished { .. }
    ));
}
