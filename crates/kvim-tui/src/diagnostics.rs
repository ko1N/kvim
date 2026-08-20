//! The host report that names every external program that kvim runs.
//!
//! kvim runs programs that the host provides: the search command of the picker,
//! the Git command of the file tree, the language servers of the registry, and
//! the external formatters of the registry. A user whose feature is missing
//! needs one answer: does this host hold the program?
//!
//! One builder answers that question in both places. `kvim --diagnostics`
//! prints the report and exits, before the editor starts. `:diagnostics` opens
//! the same report in a buffer. The two therefore never disagree about what the
//! host holds.
//!
//! [`HostReportRequest::run`] reads the executable search path, which is
//! filesystem work. The command line runs it before the terminal event loop
//! exists. The editor submits it to the bounded worker service instead, so the
//! event loop reads no path. See `docs/architecture.md` and
//! `docs/responsiveness.md`.

use std::path::PathBuf;

use kvim_clipboard::{CLIPBOARD_BYTES_MAX, ClipboardSelection, DisplaySession, program_on_path};
use kvim_language::{
    LANGUAGE_SERVERS_MAX, LanguageAdapter, LanguageRegistry, LspError, WorkspaceRoot,
};
use kvim_settings::{COUNT_MAX, FILE_BYTES_MAX, PENDING_KEYS_MAX};
use kvim_workspace::{
    BUFFERS_MAX, GIT_PROGRAM, PICKER_CANDIDATES_MAX, RIPGREP_MATCHES_MAX, RIPGREP_PROGRAM,
    UNDO_FILE_STEPS_MAX,
};

/// The name of the buffer that holds one host report.
pub(super) const HOST_BUFFER_NAME: &str = "[Diagnostics]";

/// The largest number of external programs that one host report probes.
///
/// The report probes the search command of the picker, the Git command of the
/// file tree, every distinct language-server program of the registry, and every
/// distinct formatter program of it. One adapter declares at most
/// [`LANGUAGE_SERVERS_MAX`] servers and at most one formatter, so a registry of
/// 25 adapters names at most 125 programs. The picker and the file tree add one
/// program each, so one report probes at most 127 programs and this bound holds
/// every one of them. This build declares 22 server programs and 12 formatter
/// programs. One probe is one search-path lookup.
pub const HOST_PROGRAMS_MAX: usize = 128;

/// The width of the label column of one report row, in characters.
const REPORT_LABEL_WIDTH: usize = 22;

/// The width of the program column of one program row, in characters.
///
/// `vscode-eslint-language-server` is the longest declared program of the
/// registry, at 29 characters, so this width keeps one blank after it.
const REPORT_PROGRAM_WIDTH: usize = 30;

/// The width of the state column of one program row, in characters.
const REPORT_STATE_WIDTH: usize = 8;

/// Whether one external program exists on the executable search path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProgramState {
    /// The search path holds the program.
    Found,
    /// The search path holds no such program.
    Missing,
}

impl ProgramState {
    /// Reports one program by reading the executable search path.
    fn probe(program: &str) -> Self {
        if program_on_path(program) {
            Self::Found
        } else {
            Self::Missing
        }
    }

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
        Self {
            program,
            state: ProgramState::probe(program),
        }
    }
}

/// Which declaration of one language adapter names an external program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProgramRole {
    /// The program runs one language server of the adapter.
    Server,
    /// The program formats the documents of the adapter.
    Formatter,
}

impl ProgramRole {
    /// Returns the section heading that the report prints for this role.
    const fn heading(self) -> &'static str {
        match self {
            Self::Server => "Language servers",
            Self::Formatter => "Formatters",
        }
    }

    /// Returns the programs that one adapter declares for this role, in
    /// declaration order.
    ///
    /// One adapter declares at most [`LANGUAGE_SERVERS_MAX`] servers and at
    /// most one formatter, so the answer is bounded.
    fn programs(self, adapter: &'static dyn LanguageAdapter) -> Vec<&'static str> {
        match self {
            Self::Server => adapter
                .language_servers()
                .iter()
                .map(|declaration| declaration.program)
                .collect(),
            Self::Formatter => adapter
                .external_formatter()
                .map(|declaration| declaration.program)
                .into_iter()
                .collect(),
        }
    }
}

/// One declared program of the registry, and the languages that declare it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LanguageProgram {
    /// The program, and whether this host provides it.
    report: ProgramReport,
    /// The identifiers of the adapters that declare the program, in registry
    /// order.
    ///
    /// Several languages share one program. `prettier` formats eight of them,
    /// so the row names every language that the program serves.
    languages: Vec<&'static str>,
}

/// The workspace root that the caller resolved.
///
/// The report performs no working-directory read of its own. The command line
/// resolves the root before it probes, and the editor already holds the root
/// that the composition root passed. See `docs/language-services.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostWorkspace {
    /// The caller resolved the root.
    Resolved {
        /// The resolved root.
        root: PathBuf,
    },
    /// The caller could not read the working directory.
    Unresolved {
        /// Why the working directory is unreadable.
        reason: String,
    },
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

impl WorkspaceReport {
    /// Classifies one resolved workspace root.
    ///
    /// The containment boundary of the language services performs no input and
    /// no output, so this classification reads nothing.
    fn of(workspace: HostWorkspace) -> Self {
        match workspace {
            HostWorkspace::Resolved { root } => match WorkspaceRoot::new(root.clone()) {
                Ok(_) => Self::Attached { root },
                Err(reason) => Self::Rejected { root, reason },
            },
            HostWorkspace::Unresolved { reason } => Self::Unresolved { reason },
        }
    }
}

/// Every host fact that one report describes.
#[derive(Debug)]
struct HostFacts {
    /// The workspace root and its language-service state.
    workspace: WorkspaceReport,
    /// The search command of the search picker.
    ripgrep: ProgramReport,
    /// The command that reads the repository state of the file tree.
    git: ProgramReport,
    /// Every distinct language-server program of the registry.
    servers: Vec<LanguageProgram>,
    /// Every distinct formatter program of the registry.
    formatters: Vec<LanguageProgram>,
    /// The clipboard commands of this host.
    clipboard: ClipboardSelection,
}

/// One bounded probe of the host, and the plain-text report that it produces.
///
/// # Examples
///
/// ```
/// use kvim_language::LanguageRegistry;
/// use kvim_tui::{HostReportRequest, HostWorkspace};
///
/// let root = std::env::current_dir().expect("the test process holds a working directory");
/// let report = HostReportRequest::new(
///     LanguageRegistry::first_release(),
///     HostWorkspace::Resolved { root },
/// )
/// .run();
///
/// // The report names every declared program, whether the host holds it or not.
/// assert!(report.contains("rust-analyzer"), "{report}");
/// assert!(report.contains("prettier"), "{report}");
/// ```
#[derive(Clone)]
pub struct HostReportRequest {
    /// The language adapters whose programs the report names.
    registry: LanguageRegistry,
    /// The workspace root that the caller resolved.
    workspace: HostWorkspace,
}

impl HostReportRequest {
    /// Creates the request of one host report.
    #[must_use]
    pub fn new(registry: LanguageRegistry, workspace: HostWorkspace) -> Self {
        Self {
            registry,
            workspace,
        }
    }

    /// Reads every host fact and returns the report as plain text.
    ///
    /// The call reads the executable search path once for each distinct
    /// program, and it asks `kvim-clipboard` which commands this host selects.
    /// It never reads and never prints an environment value.
    ///
    /// # Panics
    ///
    /// Panics when the registry declares more than [`HOST_PROGRAMS_MAX`]
    /// distinct programs. The bound follows from the adapter table, so a
    /// violation means that the table and the bound drifted apart. The report
    /// runs once for each request, so this cold-path check must fail loudly.
    #[must_use]
    pub fn run(self) -> String {
        let servers = probe_declared(self.registry, ProgramRole::Server);
        let formatters = probe_declared(self.registry, ProgramRole::Formatter);
        let probed = servers.len() + formatters.len() + 2;
        assert!(
            probed <= HOST_PROGRAMS_MAX,
            "one adapter declares at most {LANGUAGE_SERVERS_MAX} servers and one formatter, \
             so {} adapters name {probed} programs, above the {HOST_PROGRAMS_MAX} of this bound; \
             the adapter table and the bound drifted apart, see docs/architecture.md",
            self.registry.adapters().len()
        );
        let facts = HostFacts {
            workspace: WorkspaceReport::of(self.workspace),
            ripgrep: ProgramReport::probe(RIPGREP_PROGRAM),
            git: ProgramReport::probe(GIT_PROGRAM),
            servers,
            formatters,
            clipboard: ClipboardSelection::detect(),
        };
        report(&facts)
    }
}

/// Returns every distinct program that `registry` declares for `role`.
///
/// The answer is ascending by program name, and one entry names the adapters
/// that declare it in registry order. The function reads the executable search
/// path once for each distinct program, and it performs no other input.
fn probe_declared(registry: LanguageRegistry, role: ProgramRole) -> Vec<LanguageProgram> {
    let mut collected: Vec<LanguageProgram> = Vec::new();
    for adapter in registry.adapters() {
        for program in role.programs(*adapter) {
            let found = collected
                .iter_mut()
                .find(|entry| entry.report.program == program);
            match found {
                // One adapter may name one program in two declarations, so the
                // language reaches the row once.
                Some(entry) => {
                    if !entry.languages.contains(&adapter.id()) {
                        entry.languages.push(adapter.id());
                    }
                }
                None => collected.push(LanguageProgram {
                    report: ProgramReport::probe(program),
                    languages: vec![adapter.id()],
                }),
            }
        }
    }
    collected.sort_unstable_by_key(|entry| entry.report.program);
    collected
}

/// Formats one host report as plain text.
///
/// The report carries no escape sequence, because a redirected output and an
/// editor buffer must both stay readable. The function is pure, so the same
/// facts always produce the same text.
fn report(facts: &HostFacts) -> String {
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
        facts.git.program,
        &format!(
            "{}, for the repository marks of the file tree",
            facts.git.state.label()
        ),
    ));

    push_programs(&mut text, ProgramRole::Server, &facts.servers);
    push_programs(&mut text, ProgramRole::Formatter, &facts.formatters);

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

/// Writes one section that names every declared program of one role.
///
/// The heading counts the declared, the found, and the missing programs, so a
/// reader finds an incomplete host in one line. One row then names the program,
/// its state, and the languages that declare it.
fn push_programs(text: &mut String, role: ProgramRole, programs: &[LanguageProgram]) {
    let declared = programs.len();
    let missing = programs
        .iter()
        .filter(|entry| entry.report.state == ProgramState::Missing)
        .count();
    let found = declared - missing;
    text.push_str(&format!(
        "\n{} ({declared} declared, {found} found, {missing} missing)\n",
        role.heading()
    ));
    for entry in programs {
        text.push_str(&format!(
            "  {:<REPORT_PROGRAM_WIDTH$}{:<REPORT_STATE_WIDTH$}{}\n",
            entry.report.program,
            entry.report.state.label(),
            entry.languages.join(", ")
        ));
    }
}

/// Formats one report row with its aligned label column.
fn report_row(label: &str, value: &str) -> String {
    format!("  {label:<REPORT_LABEL_WIDTH$}{value}\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kvim_clipboard::ClipboardSelection;
    use kvim_language::LanguageRegistry;

    use super::{
        HOST_PROGRAMS_MAX, HostFacts, HostReportRequest, HostWorkspace, LanguageProgram,
        ProgramReport, ProgramRole, ProgramState, WorkspaceReport, probe_declared, report,
    };

    /// Returns one declared program with a fixed state and one language.
    fn program(program: &'static str, state: ProgramState) -> LanguageProgram {
        LanguageProgram {
            report: ProgramReport { program, state },
            languages: vec!["rust"],
        }
    }

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
            git: ProgramReport {
                program: "git",
                state: ProgramState::Found,
            },
            servers: vec![program("rust-analyzer", ProgramState::Found)],
            formatters: vec![program("rustfmt", ProgramState::Found)],
            clipboard: ClipboardSelection::MacOs,
        }
    }

    #[test]
    fn the_report_names_every_command_and_limit() {
        let report = report(&complete_facts());
        assert!(
            report.starts_with("kvim "),
            "the report opens with the version: {report}"
        );
        assert!(report.contains("/work/project"), "{report}");
        assert!(report.contains("attach to this root"), "{report}");
        assert!(report.contains("rg"), "{report}");
        assert!(report.contains("git"), "{report}");
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
        let report = report(&facts);
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
        let report = report(&facts);
        assert!(
            report.contains("missing, for the search picker"),
            "{report}"
        );
    }

    #[test]
    fn one_section_heading_counts_the_declared_the_found_and_the_missing_programs() {
        let facts = HostFacts {
            servers: vec![
                program("rust-analyzer", ProgramState::Found),
                program("zls", ProgramState::Missing),
            ],
            formatters: vec![program("xmlformat", ProgramState::Missing)],
            ..complete_facts()
        };
        let report = report(&facts);
        assert!(
            report.contains("Language servers (2 declared, 1 found, 1 missing)"),
            "{report}"
        );
        assert!(
            report.contains("Formatters (1 declared, 0 found, 1 missing)"),
            "{report}"
        );
        // A present program and a missing one read differently in the same
        // column, so one glance finds the incomplete row.
        assert!(
            report.contains("rust-analyzer                 found"),
            "{report}"
        );
        assert!(
            report.contains("zls                           missing"),
            "{report}"
        );
    }

    #[test]
    fn the_report_names_every_declared_program_of_the_registry() {
        let registry = LanguageRegistry::first_release();
        let servers = probe_declared(registry, ProgramRole::Server);
        let formatters = probe_declared(registry, ProgramRole::Formatter);
        assert_eq!(
            servers.len(),
            22,
            "the registry declares 22 server programs"
        );
        assert_eq!(
            formatters.len(),
            12,
            "the registry declares 12 formatter programs"
        );
        assert!(
            servers.len() + formatters.len() + 2 <= HOST_PROGRAMS_MAX,
            "the probe stays inside its bound"
        );

        let text = HostReportRequest::new(
            registry,
            HostWorkspace::Resolved {
                root: PathBuf::from("/work/project"),
            },
        )
        .run();
        for adapter in registry.adapters() {
            for declaration in adapter.language_servers() {
                assert!(
                    text.contains(declaration.program),
                    "the report names the server `{}` of `{}`",
                    declaration.program,
                    adapter.id()
                );
            }
            if let Some(declaration) = adapter.external_formatter() {
                assert!(
                    text.contains(declaration.program),
                    "the report names the formatter `{}` of `{}`",
                    declaration.program,
                    adapter.id()
                );
            }
            assert!(
                text.contains(adapter.id()),
                "the report names the language `{}`",
                adapter.id()
            );
        }
    }

    #[test]
    fn one_program_of_several_languages_holds_one_row_that_names_them_all() {
        let formatters = probe_declared(LanguageRegistry::first_release(), ProgramRole::Formatter);
        let prettier = formatters
            .iter()
            .find(|entry| entry.report.program == "prettier")
            .expect("the registry declares prettier");
        assert!(
            prettier.languages.len() > 1,
            "prettier formats several languages, not {:?}",
            prettier.languages
        );
        let mut sorted = prettier.languages.clone();
        sorted.dedup();
        assert_eq!(
            sorted, prettier.languages,
            "one language reaches the row once"
        );
    }

    #[test]
    fn an_unresolved_working_directory_reports_that_no_root_exists() {
        let text = HostReportRequest::new(
            LanguageRegistry::first_release(),
            HostWorkspace::Unresolved {
                reason: "permission denied".to_owned(),
            },
        )
        .run();
        assert!(text.contains("unresolved: permission denied"), "{text}");
        assert!(text.contains("language services"), "{text}");
    }
}
