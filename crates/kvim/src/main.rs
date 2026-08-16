//! The Kvim executable entry point.
//!
//! The file keeps argument parsing pure and testable. It performs input and
//! output only after the parser returns one action.

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use kvim::settings::EditorSettings;
use thiserror::Error as ErrorDerive;

/// The result of parsing command-line arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
enum CliAction {
    /// Print the command help.
    Help,
    /// Print the executable version.
    Version,
    /// Edit one file, or start with an empty buffer.
    Edit { path: Option<PathBuf> },
}

/// One invalid command line.
#[derive(Clone, Debug, Eq, ErrorDerive, PartialEq)]
enum CliError {
    #[error("the file path is empty")]
    EmptyPath,
    #[error("too many arguments; run kvim --help")]
    TooManyArguments,
    #[error("unknown option `{option}`; run kvim --help")]
    UnknownOption { option: String },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("kvim: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let action = parse_cli(std::env::args().skip(1)).map_err(|error| error.to_string())?;
    match action {
        CliAction::Help => {
            print!("{}", help_text());
            Ok(())
        }
        CliAction::Version => {
            println!("kvim {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        CliAction::Edit { path } => start_editor(path),
    }
}

/// Starts the editor over one file, or over one empty scratch buffer.
///
/// The function builds the asynchronous runtime that the bounded background
/// services need, because the executable is the composition root. The editor
/// reads the named file through that runtime, never on the event loop. The
/// composition root also resolves the workspace root once, because the language
/// services perform no filesystem lookup.
fn start_editor(path: Option<PathBuf>) -> Result<(), String> {
    let root = workspace_root()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the editor runtime: {error}"))?;
    runtime
        .block_on(kvim::tui::run(EditorSettings::default(), root, path))
        .map_err(|error| describe(&error))
}

/// Returns the workspace root that contains every document of a language server.
///
/// The root is the working directory of the editor, with every symlink
/// resolved, so it matches the spelling that a loaded buffer holds. A document
/// outside the root receives no language service and stays fully editable. See
/// `docs/language-services.md`.
fn workspace_root() -> Result<PathBuf, String> {
    let current = std::env::current_dir()
        .map_err(|error| format!("cannot read the working directory: {error}"))?;
    Ok(std::fs::canonicalize(&current).unwrap_or(current))
}

/// Writes one error and every cause below it.
///
/// The command line is the last boundary, so it shows the complete chain
/// instead of the top-level summary alone.
fn describe(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(source) = cause {
        text.push_str(": ");
        text.push_str(&source.to_string());
        cause = source.source();
    }
    text
}

/// Parses command-line arguments without performing input or output.
fn parse_cli<I, S>(arguments: I) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    let [argument] = arguments.as_slice() else {
        return if arguments.is_empty() {
            Ok(CliAction::Edit { path: None })
        } else {
            Err(CliError::TooManyArguments)
        };
    };
    match argument.as_str() {
        "-h" | "--help" => Ok(CliAction::Help),
        "-V" | "--version" => Ok(CliAction::Version),
        "" => Err(CliError::EmptyPath),
        option if option.starts_with('-') => Err(CliError::UnknownOption {
            option: option.to_owned(),
        }),
        path => Ok(CliAction::Edit {
            path: Some(PathBuf::from(path)),
        }),
    }
}

/// Returns the stable command help.
const fn help_text() -> &'static str {
    concat!(
        "Edit Rust sources in a modal terminal editor.\n\n",
        "Usage: kvim [PATH]\n\n",
        "Arguments:\n",
        "  [PATH]                  Open one file\n\n",
        "Options:\n",
        "  -h, --help              Print help\n",
        "  -V, --version           Print the version\n",
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{CliAction, CliError, parse_cli};

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
}
