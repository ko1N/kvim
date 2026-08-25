use std::path::PathBuf;

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
