use std::path::PathBuf;

use kvim_clipboard::ClipboardSelection;
use kvim_language::LanguageRegistry;

use super::{
    HOST_PROGRAMS_MAX, HostFacts, HostReportRequest, HostWorkspace, LanguageProgram, ProgramReport,
    ProgramRole, ProgramState, WorkspaceReport, probe_declared, report,
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
