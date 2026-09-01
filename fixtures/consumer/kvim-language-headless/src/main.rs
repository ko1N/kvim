use kvim_language::{
    CompletionPolicy, DiagnosticsMarkerGate, DiagnosticsRegistry, HeadlessDiagnosticsProject,
};
use kvim_lsp::ProjectId;
use kvim_path::WorktreeRelativePath;
use kvim_settings::LanguageSettings;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = DiagnosticsRegistry::first_release();
    let profile = registry.profile(std::path::Path::new("src/lib.rs"))?;
    assert_eq!(profile.id(), "rust");

    let root = std::env::current_dir()?.canonicalize()?;
    let project = HeadlessDiagnosticsProject::new(
        registry,
        root,
        LanguageSettings::default(),
        ProjectId::FIRST,
    )?;
    let path = WorktreeRelativePath::new("src/lib.rs")?;
    let selection = project.select(&path)?;
    let server = selection.declarations()[0];
    assert_eq!(server.id().adapter(), "rust");
    assert_eq!(server.program(), "rust-analyzer");
    assert_eq!(server.completion(), CompletionPolicy::Unsupported);
    assert!(matches!(server.gate(), DiagnosticsMarkerGate::NoMarkersRequired));
    assert!(server.neutral_id().is_none());
    Ok(())
}
