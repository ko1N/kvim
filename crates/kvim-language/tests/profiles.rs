use std::path::Path;

use kvim_settings::{CheckDepth, LanguageSettings};

use kvim_language::{
    CompletionPolicy, DiagnosticsRegistry, DiagnosticsRegistryError, DiagnosticsSelectionError,
    FIRST_RELEASE_LANGUAGE_PROFILES, LanguageServerDeclaration, LanguageServiceProfile,
    ServerFormatting,
};

#[test]
fn first_release_is_grammar_independent_and_deterministic() {
    let registry = DiagnosticsRegistry::first_release();
    assert_eq!(registry.profiles().len(), FIRST_RELEASE_LANGUAGE_PROFILES);
    #[cfg(not(feature = "grammar-rust"))]
    assert!(
        kvim_language::LanguageRegistry::first_release()
            .adapter(Path::new("src/lib.rs"))
            .is_err()
    );
    #[cfg(feature = "grammar-rust")]
    assert_eq!(
        kvim_language::LanguageRegistry::first_release()
            .adapter(Path::new("src/lib.rs"))
            .unwrap()
            .id(),
        "rust",
    );
    assert_eq!(
        registry.profiles().first().map(LanguageServiceProfile::id),
        Some("asm")
    );
    assert_eq!(
        registry.profiles().last().map(LanguageServiceProfile::id),
        Some("zig")
    );

    let rust = registry.profile(Path::new("src/lib.rs")).unwrap();
    assert_eq!(rust.id(), "rust");
    assert_eq!(rust.language_servers()[0].program, "rust-analyzer");
    assert!(matches!(
        registry.profile(Path::new("src/lib.RS")),
        Err(DiagnosticsSelectionError::UnsupportedPath)
    ));
    assert_eq!(
        registry.profile(Path::new("flake.lock")).unwrap().id(),
        "json"
    );
    assert_eq!(registry.profile_of_language("Rust").unwrap().id(), "rust");
}

#[test]
fn javascript_family_keeps_declaration_order_and_explicit_policies() {
    for id in ["javascript", "tsx", "typescript"] {
        let profile = DiagnosticsRegistry::first_release()
            .profile_of_language(id)
            .unwrap();
        let declarations = profile.language_servers();
        assert_eq!(declarations[0].id, "eslint");
        assert_eq!(
            declarations[0].diagnostics_completion,
            CompletionPolicy::Pull
        );
        assert_eq!(declarations[1].id, "ts_ls");
        assert_eq!(
            declarations[1].diagnostics_completion,
            CompletionPolicy::Unsupported
        );
    }
}

#[test]
fn completion_inventory_is_evidence_based() {
    let registry = DiagnosticsRegistry::first_release();
    let mut pull = Vec::new();
    let mut unsupported = 0;
    for profile in registry.profiles() {
        for declaration in profile.language_servers() {
            match declaration.diagnostics_completion {
                CompletionPolicy::Pull => pull.push((profile.id(), declaration.id)),
                CompletionPolicy::Unsupported => unsupported += 1,
                CompletionPolicy::VersionedPush => {
                    panic!("no first-release declaration has verified versioned-push evidence")
                }
            }
        }
    }
    assert_eq!(
        pull,
        [
            ("javascript", "eslint"),
            ("tsx", "eslint"),
            ("typescript", "eslint")
        ]
    );
    assert_eq!(unsupported, 25);
}

#[test]
fn settings_realization_stays_in_the_profile_declaration() {
    let registry = DiagnosticsRegistry::first_release();
    let rust = registry
        .profile_of_language("rust")
        .unwrap()
        .language_servers()[0];
    let settings = LanguageSettings {
        check_depth: CheckDepth::Lints,
        ..LanguageSettings::default()
    };
    assert_eq!(
        rust.options(settings),
        serde_json::json!({ "check": { "command": "clippy" } })
    );

    let eslint = registry
        .profile_of_language("typescript")
        .unwrap()
        .language_servers()[0];
    assert_eq!(
        eslint.settings(LanguageSettings::default()),
        Some(serde_json::json!({
            "validate": "on",
            "nodePath": serde_json::Value::Null,
            "problems": { "shortenToSingleLine": false },
            "rulesCustomizations": [],
        }))
    );
}

#[test]
fn explicit_registry_reports_ambiguous_paths() {
    static ONE: LanguageServiceProfile =
        LanguageServiceProfile::new("one", "1", &["one"], &["x"], &[], &[]);
    static TWO: LanguageServiceProfile =
        LanguageServiceProfile::new("two", "1", &["two"], &["x"], &[], &[]);
    static PROFILES: [LanguageServiceProfile; 2] = [ONE, TWO];
    let registry = DiagnosticsRegistry::new(&PROFILES).unwrap();
    assert!(matches!(
        registry.profile(Path::new("file.x")),
        Err(DiagnosticsSelectionError::AmbiguousPath)
    ));
}

#[test]
fn validation_rejects_invalid_identity_selectors_and_servers() {
    static BAD_ID: LanguageServiceProfile =
        LanguageServiceProfile::new("", "1", &[""], &[], &[], &[]);
    static BAD_ID_SET: [LanguageServiceProfile; 1] = [BAD_ID];
    assert!(matches!(
        DiagnosticsRegistry::new(&BAD_ID_SET),
        Err(DiagnosticsRegistryError::InvalidValue {
            profile: "",
            server: None,
            measure: kvim_language::DiagnosticsRegistryMeasure::ProfileId,
        })
    ));

    static BAD_SERVER: LanguageServerDeclaration = LanguageServerDeclaration {
        id: "server",
        program: "",
        args: &[],
        language_id: "x",
        formatting: ServerFormatting::Disabled,
        root_markers: &[],
        initialization_options: |_| serde_json::json!({}),
        workspace_settings: None,
        diagnostics_completion: CompletionPolicy::Unsupported,
    };
    static BAD_PROFILE: LanguageServiceProfile =
        LanguageServiceProfile::new("bad", "1", &["bad"], &["bad"], &[], &[BAD_SERVER]);
    static BAD_SERVER_SET: [LanguageServiceProfile; 1] = [BAD_PROFILE];
    assert!(matches!(
        DiagnosticsRegistry::new(&BAD_SERVER_SET),
        Err(DiagnosticsRegistryError::InvalidValue {
            profile: "bad",
            server: Some(0),
            measure: kvim_language::DiagnosticsRegistryMeasure::Program,
        })
    ));
}

#[cfg(feature = "grammar-rust")]
#[test]
fn rust_adapter_delegates_to_the_service_profile() {
    use kvim_language::{LanguageAdapter, RustAdapter};

    let adapter = RustAdapter::new();
    assert_eq!(adapter.service_profile().id(), "rust");
    assert_eq!(adapter.language_servers()[0].program, "rust-analyzer");
    assert!(adapter.supports_path(Path::new("src/lib.rs")));
}
