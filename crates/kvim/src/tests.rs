use std::path::PathBuf;
use std::time::Duration;

use kvim_embed::{
    SurfaceOwnership, WorktreeBindingMode, WorktreeEditor, WorktreePresentation, WorktreeShutdown,
};
use ratatui::layout::Rect;

use crate::editor::standalone_binary_preset;

use super::{CliAction, CliError, host_report, parse_cli};

#[test]
fn no_argument_selects_an_empty_buffer() {
    let action = parse_cli(Vec::<String>::new()).expect("no argument is valid");
    assert_eq!(action, CliAction::Edit { path: None });
}

#[test]
fn help_flags_select_the_help_action() {
    assert_eq!(parse_cli(["-h"]), Ok(CliAction::Help));
    assert_eq!(parse_cli(["--help"]), Ok(CliAction::Help));
}

#[test]
fn version_flags_select_the_version_action() {
    assert_eq!(parse_cli(["-V"]), Ok(CliAction::Version));
    assert_eq!(parse_cli(["--version"]), Ok(CliAction::Version));
}

#[test]
fn one_path_selects_the_edit_action() {
    assert_eq!(
        parse_cli(["src/main.rs"]),
        Ok(CliAction::Edit {
            path: Some(PathBuf::from("src/main.rs")),
        })
    );
}

#[test]
fn an_empty_path_is_rejected() {
    assert_eq!(parse_cli([""]), Err(CliError::EmptyPath));
}

#[test]
fn an_unknown_flag_is_rejected() {
    assert_eq!(
        parse_cli(["--split"]),
        Err(CliError::UnknownOption {
            option: "--split".to_owned(),
        })
    );
}

#[test]
fn too_many_arguments_are_rejected() {
    assert_eq!(
        parse_cli(["src/main.rs", "src/lib.rs"]),
        Err(CliError::TooManyArguments)
    );
}

#[test]
fn the_diagnostics_flag_selects_the_diagnostics_action() {
    assert_eq!(parse_cli(["--diagnostics"]), Ok(CliAction::Diagnostics));
}

#[test]
fn the_flag_prints_the_report_of_the_shared_builder() {
    // The flag adds nothing of its own, so one builder answers here and
    // inside the editor. See `docs/architecture.md`.
    let report = host_report();
    assert!(
        report.starts_with("kvim "),
        "the report opens with the version: {report}"
    );
    assert!(report.contains("Language servers ("), "{report}");
    assert!(report.contains("Formatters ("), "{report}");
    assert!(
        !report.contains('\u{1b}'),
        "the report runs outside the alternate screen, so it carries no escape: {report}"
    );
}

#[test]
fn standalone_binary_selects_the_traditional_bindings_and_internal_presentation() {
    let (binding_mode, presentation) = standalone_binary_preset();
    assert_eq!(binding_mode, WorktreeBindingMode::FacadeResolved);
    assert_eq!(presentation, WorktreePresentation::standalone());
    assert_eq!(
        presentation.command_line_ownership(),
        SurfaceOwnership::Embedded
    );
    assert_eq!(
        presentation.statusline_ownership(),
        SurfaceOwnership::Embedded
    );
    assert_eq!(
        presentation.which_key_ownership(),
        SurfaceOwnership::Embedded
    );
    assert_eq!(
        presentation.file_sidebar_ownership(),
        SurfaceOwnership::Embedded
    );
}

#[tokio::test]
async fn facade_lifecycle_runs_inside_the_binary_runtime() {
    let root = std::env::current_dir().expect("the test process has a working directory");
    let mut editor = WorktreeEditor::builder(root, Rect::new(0, 0, 40, 8))
        .open()
        .expect("the worktree facade opens inside Tokio");
    let _ = editor.dispatch();
    let completed = tokio::time::timeout(Duration::from_secs(10), editor.ready())
        .await
        .expect("initial work completes inside the host runtime");
    editor
        .apply(completed, Duration::ZERO)
        .expect("the completion returns to its owner");
    let shutdown = editor.shutdown(Duration::from_secs(10)).await;
    assert!(matches!(shutdown, WorktreeShutdown::Finished { .. }));
}
