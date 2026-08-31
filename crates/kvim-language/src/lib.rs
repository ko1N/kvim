//! Grammar-independent Kvim language service declarations.

#![deny(missing_docs)]

#[cfg(not(feature = "editor-services"))]
use std::path::Path;

#[cfg(not(feature = "editor-services"))]
use thiserror::Error;

mod headless;
mod profile;
mod server;

pub use headless::{
    DiagnosticsMarkerGate, DiagnosticsMarkerKind, HeadlessDiagnosticsError,
    HeadlessDiagnosticsProject, HeadlessDiagnosticsSelection, OpenedHeadlessDiagnosticsProject,
    RealizedDiagnosticsServer,
};

pub use kvim_lsp::{CompletionPolicy, LSP_LANGUAGE_BYTES_MAX};
pub use kvim_path::WorktreeRelativePath;
pub use profile::{
    DiagnosticsRegistry, DiagnosticsRegistryError, DiagnosticsRegistryMeasure,
    DiagnosticsSelectionError, FIRST_RELEASE_LANGUAGE_PROFILES,
    LANGUAGE_INITIALIZATION_OPTIONS_BYTES_MAX, LANGUAGE_NAMES_MAX, LANGUAGE_PROFILES_MAX,
    LANGUAGE_SELECTOR_BYTES_MAX, LANGUAGE_SERVICE_ID_BYTES_MAX,
    LANGUAGE_WORKSPACE_SETTINGS_BYTES_MAX, LanguageServiceProfile,
};
pub use server::{
    LANGUAGE_ROOT_MARKERS_MAX, LANGUAGE_SERVERS_MAX, LanguageServerDeclaration, LanguageServerId,
    ServerFormatting,
};

/// A path-selection error in a grammar-free editor registry.
#[cfg(not(feature = "editor-services"))]
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AnalysisError {
    /// No enabled grammar adapter owns the path.
    #[error("no language adapter supports this path")]
    UnsupportedPath,
    /// More than one enabled grammar adapter owns the path.
    #[error("more than one language adapter supports this path")]
    AmbiguousPath,
}

/// The empty grammar-backed registry of a build without editor services.
#[cfg(not(feature = "editor-services"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct LanguageRegistry;

#[cfg(not(feature = "editor-services"))]
impl LanguageRegistry {
    /// Returns the valid empty registry of this grammar-free build.
    #[must_use]
    pub const fn first_release() -> Self {
        Self
    }

    /// Returns unsupported because this build enables no grammar adapters.
    pub fn adapter(&self, _path: &Path) -> Result<(), AnalysisError> {
        Err(AnalysisError::UnsupportedPath)
    }

    /// Returns no adapter because this build enables no grammar adapters.
    #[must_use]
    pub fn adapter_of_language(&self, _language: &str) -> Option<()> {
        None
    }
}

#[cfg(feature = "editor-services")]
mod editor;
#[cfg(feature = "editor-services")]
pub use editor::*;
