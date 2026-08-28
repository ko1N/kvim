//! The kvim executable entry point.
//!
//! The file keeps argument parsing pure and testable. It performs input and
//! output only after the parser returns one action.
//!
//! The executable is the composition root of the standalone editor. It builds
//! the asynchronous runtime, resolves the worktree root, and hands both to the
//! `editor` module, which owns the terminal for the lifetime of one session.
//! See `docs/architecture.md`.

mod editor;

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use kvim_embed::{WorktreeHostReportRequest, WorktreeHostWorkspace};
use kvim_path::WorktreeRoot;
use kvim_settings::EditorSettings;
use thiserror::Error as ErrorDerive;

use crate::editor::PanicProbe;

/// The environment variable that asks the editor to panic after its first
/// frame.
///
/// The variable proves that the panic hook of the terminal session restores the
/// terminal. Any value enables it, and the editor never prints the value. It is
/// a diagnostic, not an editor feature. See `docs/architecture.md`.
const PANIC_PROBE_VARIABLE: &str = "KVIM_PANIC_PROBE";

/// The result of parsing command-line arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
enum CliAction {
    /// Print the command help.
    Help,
    /// Print the executable version.
    Version,
    /// Print the host report that explains an unavailable feature.
    Diagnostics,
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
        CliAction::Diagnostics => {
            print!("{}", host_report());
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
        .block_on(editor::run(
            EditorSettings::default(),
            root,
            path,
            panic_probe(),
        ))
        .map_err(|error| describe(&error))
}

/// Returns the panic probe that the environment selects.
///
/// The function reads whether the variable exists. It never reads and never
/// reports the value.
fn panic_probe() -> PanicProbe {
    if std::env::var_os(PANIC_PROBE_VARIABLE).is_some() {
        PanicProbe::AfterFirstFrame
    } else {
        PanicProbe::Disabled
    }
}

/// Returns the workspace root that contains every document of a language server.
///
/// The root is the working directory of the editor, with every symlink
/// resolved. File operations reject every path outside this root. See
/// `docs/language-services.md`.
fn workspace_root() -> Result<WorktreeRoot, String> {
    let current = std::env::current_dir()
        .map_err(|error| format!("cannot read the working directory: {error}"))?;
    WorktreeRoot::open(current)
        .map_err(|error| format!("cannot open the working directory as a worktree: {error}"))
}

/// Returns the host report of this machine as plain text.
///
/// The report names every external program that kvim runs, and whether this
/// host holds it. The internal presentation layer owns the shared builder, and
/// `kvim-embed` publishes this facade-owned wrapper. The `:diagnostics` command
/// of the editor runs the same one, so the flag and the command can never
/// disagree about what the host holds.
///
/// The command line runs before the terminal event loop exists, so it probes
/// the executable search path here. The editor submits the same probe to its
/// bounded worker service instead. See `docs/architecture.md`.
fn host_report() -> String {
    let workspace = match workspace_root() {
        Ok(root) => WorktreeHostWorkspace::Resolved {
            root: root.as_path().to_path_buf(),
        },
        Err(reason) => WorktreeHostWorkspace::Unresolved { reason },
    };
    WorktreeHostReportRequest::built_in(workspace).run()
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
        "--diagnostics" => Ok(CliAction::Diagnostics),
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
        "      --diagnostics       Print the host report and exit\n",
    )
}

#[cfg(test)]
mod tests;
