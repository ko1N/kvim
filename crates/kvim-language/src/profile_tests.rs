use super::*;
use kvim_lsp::CompletionPolicy;
use serde_json::{Value, json};

const EMPTY: &[LanguageServerDeclaration] = &[];

fn profile(
    id: &'static str,
    names: &'static [&'static str],
    extensions: &'static [&'static str],
    files: &'static [&'static str],
    servers: &'static [LanguageServerDeclaration],
) -> LanguageServiceProfile {
    LanguageServiceProfile::new(id, "1", names, extensions, files, servers)
}

fn registry(
    profile: LanguageServiceProfile,
) -> Result<DiagnosticsRegistry, DiagnosticsRegistryError> {
    DiagnosticsRegistry::new(Box::leak(Box::new([profile])))
}

fn server() -> LanguageServerDeclaration {
    LanguageServerDeclaration {
        id: "server",
        program: "server",
        args: &[],
        language_id: "demo",
        formatting: ServerFormatting::Disabled,
        diagnostics_completion: CompletionPolicy::Unsupported,
        root_markers: &[],
        initialization_options: |_| json!({}),
        workspace_settings: None,
    }
}

fn leaked(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}
fn strings(values: Vec<&'static str>) -> &'static [&'static str] {
    Box::leak(values.into_boxed_slice())
}
fn servers(values: Vec<LanguageServerDeclaration>) -> &'static [LanguageServerDeclaration] {
    Box::leak(values.into_boxed_slice())
}

#[test]
fn rejects_profile_count_bounds() {
    assert!(matches!(
        DiagnosticsRegistry::new(&[]),
        Err(DiagnosticsRegistryError::Bound {
            measure: DiagnosticsRegistryMeasure::Profiles,
            actual: 0,
            ..
        })
    ));
    let values = vec![profile("p", &["p"], &[], &[], EMPTY); LANGUAGE_PROFILES_MAX + 1];
    assert!(matches!(
        DiagnosticsRegistry::new(Box::leak(values.into_boxed_slice())),
        Err(DiagnosticsRegistryError::Bound {
            measure: DiagnosticsRegistryMeasure::Profiles,
            ..
        })
    ));
}

#[test]
fn rejects_profile_identity_and_version_dimensions() {
    for (id, version, measure) in [
        ("", "1", DiagnosticsRegistryMeasure::ProfileId),
        (
            leaked("x".repeat(LANGUAGE_SERVICE_ID_BYTES_MAX + 1)),
            "1",
            DiagnosticsRegistryMeasure::ProfileId,
        ),
        ("démo", "1", DiagnosticsRegistryMeasure::ProfileId),
        ("demo", "", DiagnosticsRegistryMeasure::ProfileVersion),
        (
            "demo",
            leaked("x".repeat(LANGUAGE_SERVICE_ID_BYTES_MAX + 1)),
            DiagnosticsRegistryMeasure::ProfileVersion,
        ),
    ] {
        let names = strings(vec![id]);
        assert!(
            matches!(registry(LanguageServiceProfile::new(id, version, names, &[], &[], EMPTY)), Err(DiagnosticsRegistryError::InvalidValue { measure: found, .. }) | Err(DiagnosticsRegistryError::Bound { measure: found, .. }) if found == measure)
        );
    }
    let duplicate =
        Box::leak(vec![profile("demo", &["demo"], &[], &[], EMPTY); 2].into_boxed_slice());
    assert!(matches!(
        DiagnosticsRegistry::new(duplicate),
        Err(DiagnosticsRegistryError::DuplicateProfileId { .. })
    ));
}

#[test]
fn rejects_language_name_dimensions() {
    for names in [
        &[][..],
        strings(vec!["demo"; LANGUAGE_NAMES_MAX + 1]),
        &[""][..],
        strings(vec![leaked("x".repeat(LANGUAGE_SELECTOR_BYTES_MAX + 1))]),
    ] {
        assert!(registry(profile("demo", names, &[], &[], EMPTY)).is_err());
    }
    assert!(matches!(
        registry(profile("demo", &["demo", "DEMO"], &[], &[], EMPTY)),
        Err(DiagnosticsRegistryError::DuplicateLanguageName { .. })
    ));
}

#[test]
fn rejects_path_selector_dimensions_and_duplicates() {
    for (extensions, files, measure) in [
        (&[""][..], &[][..], DiagnosticsRegistryMeasure::Extension),
        (
            strings(vec![leaked("x".repeat(LANGUAGE_SELECTOR_BYTES_MAX + 1))]),
            &[][..],
            DiagnosticsRegistryMeasure::Extension,
        ),
        (&[][..], &[""][..], DiagnosticsRegistryMeasure::FileName),
        (
            &[][..],
            strings(vec![leaked("x".repeat(LANGUAGE_SELECTOR_BYTES_MAX + 1))]),
            DiagnosticsRegistryMeasure::FileName,
        ),
    ] {
        assert!(
            matches!(registry(profile("demo", &["demo"], extensions, files, EMPTY)), Err(DiagnosticsRegistryError::InvalidValue { measure: found, .. }) | Err(DiagnosticsRegistryError::Bound { measure: found, .. }) if found == measure)
        );
    }
    assert!(matches!(
        registry(profile("demo", &["demo"], &["x", "x"], &[], EMPTY)),
        Err(DiagnosticsRegistryError::DuplicatePathSelector { .. })
    ));
    assert!(matches!(
        registry(profile("demo", &["demo"], &[], &["x", "x"], EMPTY)),
        Err(DiagnosticsRegistryError::DuplicatePathSelector { .. })
    ));
}

#[test]
fn explicit_cross_profile_path_duplicates_are_typed_ambiguity() {
    let values = Box::leak(Box::new([
        profile("one", &["one"], &["x"], &[], EMPTY),
        profile("two", &["two"], &["x"], &[], EMPTY),
    ]));
    let registry = DiagnosticsRegistry::new(values).unwrap();
    assert_eq!(
        registry.profile(Path::new("file.x")).unwrap_err(),
        DiagnosticsSelectionError::AmbiguousPath
    );
}

#[test]
fn rejects_server_count_identity_and_formatter_dimensions() {
    let many = servers(vec![server(); LANGUAGE_SERVERS_MAX + 1]);
    assert!(matches!(
        registry(profile("demo", &["demo"], &[], &[], many)),
        Err(DiagnosticsRegistryError::Bound {
            measure: DiagnosticsRegistryMeasure::Servers,
            ..
        })
    ));
    let duplicate = servers(vec![server(), server()]);
    assert!(matches!(
        registry(profile("demo", &["demo"], &[], &[], duplicate)),
        Err(DiagnosticsRegistryError::DuplicateServer { .. })
    ));
    let mut one = server();
    one.formatting = ServerFormatting::Enabled;
    let mut two = server();
    two.id = "other";
    two.formatting = ServerFormatting::Enabled;
    assert!(matches!(
        registry(profile(
            "demo",
            &["demo"],
            &[],
            &[],
            servers(vec![one, two])
        )),
        Err(DiagnosticsRegistryError::DuplicateFormatter { .. })
    ));
    for id in ["", leaked("x".repeat(LANGUAGE_SERVICE_ID_BYTES_MAX + 1))] {
        let mut value = server();
        value.id = id;
        assert!(registry(profile("demo", &["demo"], &[], &[], servers(vec![value]))).is_err());
    }
}

#[test]
fn rejects_program_argument_command_and_language_id_dimensions() {
    let cases = [
        (DiagnosticsRegistryMeasure::Program, "", &[][..], "demo"),
        (
            DiagnosticsRegistryMeasure::Program,
            leaked("x".repeat(LSP_SERVER_PROGRAM_BYTES_MAX + 1)),
            &[][..],
            "demo",
        ),
        (
            DiagnosticsRegistryMeasure::Arguments,
            "s",
            strings(vec!["x"; LSP_SERVER_ARGUMENTS_MAX + 1]),
            "demo",
        ),
        (
            DiagnosticsRegistryMeasure::Argument,
            "s",
            strings(vec![leaked("x".repeat(LSP_SERVER_ARGUMENT_BYTES_MAX + 1))]),
            "demo",
        ),
        (
            DiagnosticsRegistryMeasure::Command,
            leaked("x".repeat(LSP_SERVER_PROGRAM_BYTES_MAX)),
            strings(vec![leaked("x".repeat(LSP_SERVER_ARGUMENT_BYTES_MAX)); 4]),
            "demo",
        ),
        (DiagnosticsRegistryMeasure::LanguageId, "s", &[][..], ""),
        (
            DiagnosticsRegistryMeasure::LanguageId,
            "s",
            &[][..],
            leaked("x".repeat(LSP_LANGUAGE_BYTES_MAX + 1)),
        ),
    ];
    for (measure, program, args, language_id) in cases {
        let mut value = server();
        value.program = program;
        value.args = args;
        value.language_id = language_id;
        assert!(
            matches!(registry(profile("demo", &["demo"], &[], &[], servers(vec![value]))), Err(DiagnosticsRegistryError::InvalidValue { measure: found, .. }) | Err(DiagnosticsRegistryError::Bound { measure: found, .. }) if found == measure)
        );
    }
}

#[test]
fn rejects_marker_dimensions() {
    let cases = [
        (
            strings(vec!["x"; LANGUAGE_ROOT_MARKERS_MAX + 1]),
            DiagnosticsRegistryMeasure::RootMarkers,
        ),
        (
            strings(vec![leaked("x".repeat(LANGUAGE_SELECTOR_BYTES_MAX + 1))]),
            DiagnosticsRegistryMeasure::RootMarker,
        ),
    ];
    for (markers, measure) in cases {
        let mut value = server();
        value.root_markers = markers;
        assert!(
            matches!(registry(profile("demo", &["demo"], &[], &[], servers(vec![value]))), Err(DiagnosticsRegistryError::Bound { measure: found, .. }) if found == measure)
        );
    }
    let mut value = server();
    value.root_markers = &["dir/file"];
    assert!(matches!(
        registry(profile("demo", &["demo"], &[], &[], servers(vec![value]))),
        Err(DiagnosticsRegistryError::InvalidRootMarker { .. })
    ));
}

fn oversized(_: LanguageSettings) -> Value {
    Value::String("x".repeat(LSP_MESSAGE_BYTES_MAX + 1))
}

#[test]
fn rejects_serialized_options_and_settings_bounds() {
    let mut options = server();
    options.initialization_options = oversized;
    assert!(matches!(
        registry(profile("demo", &["demo"], &[], &[], servers(vec![options]))),
        Err(DiagnosticsRegistryError::Bound {
            measure: DiagnosticsRegistryMeasure::InitializationOptions,
            ..
        })
    ));
    let mut settings = server();
    settings.workspace_settings = Some(oversized);
    assert!(matches!(
        registry(profile(
            "demo",
            &["demo"],
            &[],
            &[],
            servers(vec![settings])
        )),
        Err(DiagnosticsRegistryError::Bound {
            measure: DiagnosticsRegistryMeasure::WorkspaceSettings,
            ..
        })
    ));
}

#[test]
fn validates_every_current_language_settings_realization() {
    let rust = DiagnosticsRegistry::first_release()
        .profile_of_language("rust")
        .unwrap()
        .language_servers()[0];
    for (depth, command) in [
        (CheckDepth::Compile, "check"),
        (CheckDepth::Lints, "clippy"),
    ] {
        let settings = LanguageSettings {
            check_depth: depth,
            diagnostics_enabled: true,
        };
        assert_eq!(rust.options(settings)["check"]["command"], command);
    }
}
