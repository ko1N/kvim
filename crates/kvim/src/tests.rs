use std::path::{Path, PathBuf};
use std::time::Duration;

use kvim_embed::{
    SurfaceOwnership, WorktreeBindingMode, WorktreeEditor, WorktreePresentation, WorktreeShutdown,
};
use ratatui::layout::Rect;

use crate::editor::standalone_binary_preset;

use super::{CliAction, CliError, editor_start, host_report, parse_cli};

/// One temporary directory that the tests own and remove.
///
/// The path is canonical, so a comparison against a selected root never fails
/// on a symbolic link of the ambient temporary directory.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "kvim-root-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the test creates its own directory");
        Self(path.canonicalize().expect("the new directory resolves"))
    }

    fn directory(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(&path).expect("the test creates its own directory");
        path
    }

    fn file(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the test creates its own directory");
        }
        std::fs::write(&path, "text\n").expect("the test writes its own file");
        path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_path_outside_the_working_directory_brings_its_own_root() {
    let outside = TestDirectory::new("outside");
    let file = outside.file("notes.txt");

    let (root, relative) = editor_start(Some(&file)).expect("the argument names one directory");
    assert_eq!(root.as_path(), outside.0);
    assert_eq!(
        relative.expect("the argument names one file").as_path(),
        Path::new("notes.txt")
    );
}

#[test]
fn a_path_outside_the_working_directory_takes_its_enclosing_project() {
    let outside = TestDirectory::new("project");
    outside.directory(".git");
    let file = outside.file("crates/inner/src/main.rs");

    // The walk finds the `.git` entry above the file, so the whole project
    // opens instead of the one directory that holds the file.
    let (root, relative) = editor_start(Some(&file)).expect("the argument names one directory");
    assert_eq!(root.as_path(), outside.0);
    assert_eq!(
        relative.expect("the argument names one file").as_path(),
        Path::new("crates/inner/src/main.rs")
    );
}

#[test]
fn a_missing_directory_inside_the_argument_still_opens_one_buffer() {
    let outside = TestDirectory::new("missing");

    let target = outside.0.join("absent/notes.txt");
    let (root, relative) = editor_start(Some(&target)).expect("the deepest ancestor exists");
    assert_eq!(root.as_path(), outside.0);
    assert_eq!(
        relative.expect("the argument names one file").as_path(),
        Path::new("absent/notes.txt")
    );
}

#[test]
fn a_path_inside_the_working_directory_keeps_that_directory_as_the_root() {
    let current = std::env::current_dir()
        .and_then(std::fs::canonicalize)
        .expect("the test process holds a working directory");

    let (root, relative) = editor_start(Some(Path::new("src/main.rs")))
        .expect("the working directory is one worktree");
    assert_eq!(root.as_path(), current);
    assert_eq!(
        relative.expect("the argument names one file").as_path(),
        Path::new("src/main.rs")
    );
}

#[test]
fn parent_components_resolve_before_the_root_selection() {
    let outside = TestDirectory::new("parents");
    outside.file("notes.txt");
    let noisy = outside.0.join("./inner/../notes.txt");

    let (root, relative) = editor_start(Some(&noisy)).expect("the argument names one directory");
    assert_eq!(root.as_path(), outside.0);
    assert_eq!(
        relative.expect("the argument names one file").as_path(),
        Path::new("notes.txt")
    );
}

#[test]
fn an_argument_that_names_the_root_itself_opens_no_file() {
    let outside = TestDirectory::new("itself");

    let (root, relative) =
        editor_start(Some(&outside.0)).expect("the argument names one directory");
    assert_eq!(root.as_path(), outside.0);
    assert!(relative.is_none());
}

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
