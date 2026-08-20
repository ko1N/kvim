//! The kvim executable entry point.
//!
//! The file keeps argument parsing pure and testable. It performs input and
//! output only after the parser returns one action.

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use kvim_clipboard::{CLIPBOARD_BYTES_MAX, ClipboardSelection, DisplaySession, program_on_path};
use kvim_language::{LanguageAdapter, LspError, RustAdapter, WorkspaceRoot};
use kvim_settings::{COUNT_MAX, EditorSettings, FILE_BYTES_MAX, PENDING_KEYS_MAX};
use kvim_tui::PanicProbe;
use kvim_workspace::{
    BUFFERS_MAX, PICKER_CANDIDATES_MAX, RIPGREP_MATCHES_MAX, RIPGREP_PROGRAM, UNDO_FILE_STEPS_MAX,
};
use thiserror::Error as ErrorDerive;

/// The environment variable that asks the editor to panic after its first
/// frame.
///
/// The variable proves that the panic hook of the terminal session restores the
/// terminal. Any value enables it, and the editor never prints the value. It is
/// a diagnostic, not an editor feature. See `docs/architecture.md`.
const PANIC_PROBE_VARIABLE: &str = "KVIM_PANIC_PROBE";

/// The width of the label column of one diagnostics row, in characters.
const REPORT_LABEL_WIDTH: usize = 22;

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
            print!("{}", diagnostics_report(&probe_host()));
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
        .block_on(kvim_tui::run(
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
/// resolved, so it matches the spelling that a loaded buffer holds. A document
/// outside the root receives no language service and stays fully editable. See
/// `docs/language-services.md`.
fn workspace_root() -> Result<PathBuf, String> {
    let current = std::env::current_dir()
        .map_err(|error| format!("cannot read the working directory: {error}"))?;
    Ok(std::fs::canonicalize(&current).unwrap_or(current))
}

/// Whether one external program exists on the executable search path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProgramState {
    /// The search path holds the program.
    Found,
    /// The search path holds no such program.
    Missing,
}

impl ProgramState {
    /// Returns the word that the report prints for this state.
    const fn label(self) -> &'static str {
        match self {
            Self::Found => "found",
            Self::Missing => "missing",
        }
    }
}

/// One external program of the report, and whether this host provides it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProgramReport {
    /// The program that kvim runs.
    program: &'static str,
    /// Whether the executable search path holds the program.
    state: ProgramState,
}

impl ProgramReport {
    /// Reports one program by reading the executable search path.
    fn probe(program: &'static str) -> Self {
        let state = if program_on_path(program) {
            ProgramState::Found
        } else {
            ProgramState::Missing
        };
        Self { program, state }
    }
}

/// The workspace root of the report, and whether language services accept it.
///
/// The rejection carries the typed reason, so the report never classifies a
/// failure by its message text. [`LspError`] holds an input-output error in
/// other variants, so the value is neither cloneable nor comparable.
#[derive(Debug)]
enum WorkspaceReport {
    /// The root resolved, and the language services attach to it.
    Attached {
        /// The resolved root.
        root: PathBuf,
    },
    /// The root resolved, and the language services reject it.
    Rejected {
        /// The resolved root.
        root: PathBuf,
        /// Why the containment boundary refused the root.
        reason: LspError,
    },
    /// The working directory is unreadable, so no root exists.
    Unresolved {
        /// Why the working directory is unreadable.
        reason: String,
    },
}

/// Every host fact that one diagnostics report describes.
#[derive(Debug)]
struct HostFacts {
    /// The workspace root and its language-service state.
    workspace: WorkspaceReport,
    /// The search command of the search picker.
    ripgrep: ProgramReport,
    /// The language server that the Rust adapter declares.
    language_server: ProgramReport,
    /// The clipboard commands of this host.
    clipboard: ClipboardSelection,
}

/// Reads every host fact that the diagnostics report needs.
///
/// The function is the boundary of the report. It reads the working directory
/// and the executable search path, and it asks `kvim-clipboard` which commands
/// this host selects. It never reads and never prints an environment value.
fn probe_host() -> HostFacts {
    let workspace = match workspace_root() {
        Ok(root) => match WorkspaceRoot::new(root.clone()) {
            Ok(_) => WorkspaceReport::Attached { root },
            Err(reason) => WorkspaceReport::Rejected { root, reason },
        },
        Err(reason) => WorkspaceReport::Unresolved { reason },
    };
    let server = RustAdapter::new()
        .language_servers()
        .first()
        .expect("the Rust adapter of the first release declares a language server");
    HostFacts {
        workspace,
        ripgrep: ProgramReport::probe(RIPGREP_PROGRAM),
        language_server: ProgramReport::probe(server.program),
        clipboard: ClipboardSelection::detect(),
    }
}

/// Formats one diagnostics report as plain text.
///
/// The report carries no escape sequence, because it runs outside the alternate
/// screen and a redirected output must stay readable.
fn diagnostics_report(facts: &HostFacts) -> String {
    let mut text = format!("kvim {}\n\nWorkspace\n", env!("CARGO_PKG_VERSION"));
    match &facts.workspace {
        WorkspaceReport::Attached { root } => {
            text.push_str(&report_row("root", &root.display().to_string()));
            text.push_str(&report_row("language services", "attach to this root"));
        }
        WorkspaceReport::Rejected { root, reason } => {
            text.push_str(&report_row("root", &root.display().to_string()));
            text.push_str(&report_row(
                "language services",
                &format!("unavailable: {reason}"),
            ));
        }
        WorkspaceReport::Unresolved { reason } => {
            text.push_str(&report_row("root", &format!("unresolved: {reason}")));
            text.push_str(&report_row("language services", "unavailable"));
        }
    }

    text.push_str("\nExternal commands\n");
    text.push_str(&report_row(
        facts.ripgrep.program,
        &format!("{}, for the search picker", facts.ripgrep.state.label()),
    ));
    text.push_str(&report_row(
        facts.language_server.program,
        &format!(
            "{}, for diagnostics, definition, hover, and formatting",
            facts.language_server.state.label()
        ),
    ));

    text.push_str("\nSystem clipboard\n");
    let implementation = match facts.clipboard {
        ClipboardSelection::MacOs => "macOS commands",
        ClipboardSelection::Linux {
            session: DisplaySession::Wayland,
            ..
        } => "Linux, Wayland session",
        ClipboardSelection::Linux {
            session: DisplaySession::X11,
            ..
        } => "Linux, X11 session",
        ClipboardSelection::Absent => "none",
    };
    text.push_str(&report_row("implementation", implementation));
    match facts.clipboard.commands() {
        Some(commands) => {
            text.push_str(&report_row("write", &commands.write.to_string()));
            text.push_str(&report_row("read", &commands.read.to_string()));
        }
        None => text.push_str(&report_row("effect", "the editor registers stay usable")),
    }

    text.push_str("\nLimits\n");
    text.push_str(&report_row("file size", &format!("{FILE_BYTES_MAX} bytes")));
    text.push_str(&report_row(
        "clipboard transfer",
        &format!("{CLIPBOARD_BYTES_MAX} bytes"),
    ));
    text.push_str(&report_row("loaded buffers", &BUFFERS_MAX.to_string()));
    text.push_str(&report_row(
        "picker candidates",
        &PICKER_CANDIDATES_MAX.to_string(),
    ));
    text.push_str(&report_row(
        "search matches",
        &RIPGREP_MATCHES_MAX.to_string(),
    ));
    text.push_str(&report_row(
        "undo file steps",
        &UNDO_FILE_STEPS_MAX.to_string(),
    ));
    text.push_str(&report_row("command count", &COUNT_MAX.to_string()));
    text.push_str(&report_row("pending keys", &PENDING_KEYS_MAX.to_string()));
    text
}

/// Formats one report row with its aligned label column.
fn report_row(label: &str, value: &str) -> String {
    format!("  {label:<REPORT_LABEL_WIDTH$}{value}\n")
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
mod tests {
    use std::path::PathBuf;

    use kvim_clipboard::ClipboardSelection;

    use super::{
        CliAction, CliError, HostFacts, ProgramReport, ProgramState, WorkspaceReport,
        diagnostics_report, parse_cli,
    };

    /// Returns facts of a host that provides every external feature.
    fn complete_facts() -> HostFacts {
        HostFacts {
            workspace: WorkspaceReport::Attached {
                root: PathBuf::from("/work/project"),
            },
            ripgrep: ProgramReport {
                program: "rg",
                state: ProgramState::Found,
            },
            language_server: ProgramReport {
                program: "rust-analyzer",
                state: ProgramState::Found,
            },
            clipboard: ClipboardSelection::MacOs,
        }
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
    fn the_report_names_every_command_and_limit() {
        let report = diagnostics_report(&complete_facts());
        assert!(
            report.starts_with("kvim "),
            "the report opens with the version: {report}"
        );
        assert!(report.contains("/work/project"), "{report}");
        assert!(report.contains("attach to this root"), "{report}");
        assert!(report.contains("rg"), "{report}");
        assert!(report.contains("rust-analyzer"), "{report}");
        assert!(report.contains("pbcopy"), "{report}");
        assert!(report.contains("pbpaste"), "{report}");
        assert!(report.contains("4194304 bytes"), "{report}");
        assert!(report.contains("1048576 bytes"), "{report}");
        assert!(
            !report.contains('\u{1b}'),
            "the report runs outside the alternate screen, so it carries no escape: {report}"
        );
    }

    #[test]
    fn a_host_without_a_clipboard_reports_that_the_registers_stay_usable() {
        let facts = HostFacts {
            clipboard: ClipboardSelection::Absent,
            ..complete_facts()
        };
        let report = diagnostics_report(&facts);
        assert!(
            report.contains("the editor registers stay usable"),
            "{report}"
        );
    }

    #[test]
    fn a_missing_command_is_named_in_the_report() {
        let facts = HostFacts {
            ripgrep: ProgramReport {
                program: "rg",
                state: ProgramState::Missing,
            },
            ..complete_facts()
        };
        let report = diagnostics_report(&facts);
        assert!(
            report.contains("missing, for the search picker"),
            "{report}"
        );
    }
}
