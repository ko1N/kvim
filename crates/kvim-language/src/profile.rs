//! Grammar-independent language service profiles and diagnostics selection.

use std::{ffi::OsStr, path::Path};

use thiserror::Error;

use kvim_lsp::{
    LSP_LANGUAGE_BYTES_MAX, LSP_MESSAGE_BYTES_MAX, LSP_SERVER_ARGUMENT_BYTES_MAX,
    LSP_SERVER_ARGUMENTS_MAX, LSP_SERVER_COMMAND_BYTES_MAX, LSP_SERVER_PROGRAM_BYTES_MAX,
};
use kvim_settings::{CheckDepth, LanguageSettings};

use crate::server::marker_is_valid;
use crate::{
    LANGUAGE_ROOT_MARKERS_MAX, LANGUAGE_SERVERS_MAX, LanguageServerDeclaration, ServerFormatting,
};

/// The number of profiles in the first release.
pub const FIRST_RELEASE_LANGUAGE_PROFILES: usize = 25;
/// The largest number of profiles accepted by one registry.
pub const LANGUAGE_PROFILES_MAX: usize = 64;
/// The largest stable profile or server identifier, in bytes.
///
/// A server declaration uses its identifier as the fallback diagnostic source.
/// This one bound therefore limits both values.
pub const LANGUAGE_SERVICE_ID_BYTES_MAX: usize = 64;
/// The largest number of names declared by one profile.
pub const LANGUAGE_NAMES_MAX: usize = 8;
/// The largest language name, extension, file name, or root marker, in bytes.
pub const LANGUAGE_SELECTOR_BYTES_MAX: usize = 128;
/// The largest serialized initialization-options value, in bytes.
pub const LANGUAGE_INITIALIZATION_OPTIONS_BYTES_MAX: usize = LSP_MESSAGE_BYTES_MAX;
/// The largest serialized workspace-settings value, in bytes.
pub const LANGUAGE_WORKSPACE_SETTINGS_BYTES_MAX: usize = LSP_MESSAGE_BYTES_MAX;

/// One grammar-independent language and its declared services.
///
/// The stable identifier is the identity reported to a headless host. The
/// profile is the only source for case-sensitive path selectors, folded
/// language aliases, and ordered server declarations. Grammar-backed adapters
/// delegate to it. A profile creates no runtime, process, or task.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use kvim_language::LanguageServiceProfile;
///
/// let profile = LanguageServiceProfile::new("demo", "1", &["demo"], &["demo"], &[], &[]);
/// assert!(profile.supports_path(Path::new("main.demo")));
/// assert!(profile.supports_language("Demo"));
/// ```
#[derive(Clone, Copy, Debug)]
pub struct LanguageServiceProfile {
    id: &'static str,
    version: &'static str,
    language_names: &'static [&'static str],
    extensions: &'static [&'static str],
    file_names: &'static [&'static str],
    language_servers: &'static [LanguageServerDeclaration],
}

impl LanguageServiceProfile {
    /// Creates profile data for validation by [`DiagnosticsRegistry::new`].
    #[must_use]
    pub const fn new(
        id: &'static str,
        version: &'static str,
        language_names: &'static [&'static str],
        extensions: &'static [&'static str],
        file_names: &'static [&'static str],
        language_servers: &'static [LanguageServerDeclaration],
    ) -> Self {
        Self {
            id,
            version,
            language_names,
            extensions,
            file_names,
            language_servers,
        }
    }

    /// Returns the stable profile identifier.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }
    /// Returns the profile data version.
    #[must_use]
    pub const fn version(&self) -> &'static str {
        self.version
    }
    /// Returns the language names and aliases in declaration order.
    #[must_use]
    pub const fn language_names(&self) -> &'static [&'static str] {
        self.language_names
    }
    /// Returns the case-sensitive file extensions.
    #[must_use]
    pub const fn extensions(&self) -> &'static [&'static str] {
        self.extensions
    }
    /// Returns the case-sensitive complete file names.
    #[must_use]
    pub const fn file_names(&self) -> &'static [&'static str] {
        self.file_names
    }
    /// Returns the ordered server declarations.
    ///
    /// Each declaration has an explicit exact-revision diagnostics completion policy.
    #[must_use]
    pub const fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        self.language_servers
    }
    /// Reports whether the profile owns a path by extension or complete file name.
    #[must_use]
    pub fn supports_path(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|value| owns(self.extensions, value))
            || path
                .file_name()
                .is_some_and(|value| owns(self.file_names, value))
    }
    /// Reports whether the profile answers to a language name, folding ASCII case.
    #[must_use]
    pub fn supports_language(&self, language: &str) -> bool {
        self.language_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(language))
    }
}

fn owns(keys: &[&str], value: &OsStr) -> bool {
    keys.iter().any(|key| value == OsStr::new(key))
}

/// A typed grammar-independent path selection failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum DiagnosticsSelectionError {
    /// No profile owns the path.
    #[error("no language service profile supports this path")]
    UnsupportedPath,
    /// Multiple profiles own the path.
    #[error("more than one language service profile supports this path")]
    AmbiguousPath,
}

/// A grammar-independent registry of language service profiles.
///
/// [`DiagnosticsRegistry::first_release`] is available with
/// `--no-default-features`; it always contains the 25 service profiles.
/// [`crate::LanguageRegistry::first_release`] is the editor/syntax registry and
/// remains empty without grammar features. Explicit registries may contain
/// cross-profile path duplicates. Lookup reports those paths as
/// [`DiagnosticsSelectionError::AmbiguousPath`]. Language names and stable
/// profile identifiers must remain unique. Construction validates the complete
/// table before publishing it and creates no runtime or task.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use kvim_language::DiagnosticsRegistry;
///
/// let registry = DiagnosticsRegistry::first_release();
/// assert_eq!(registry.profile(Path::new("src/lib.rs")).unwrap().id(), "rust");
/// assert_eq!(registry.profile_of_language("Rust").unwrap().id(), "rust");
/// ```
#[derive(Clone, Copy)]
pub struct DiagnosticsRegistry {
    profiles: &'static [LanguageServiceProfile],
}

impl DiagnosticsRegistry {
    /// Returns all first-release profiles in stable declaration order.
    #[must_use]
    pub fn first_release() -> Self {
        Self::new(&FIRST_RELEASE).expect("built-in profiles are release-validated")
    }

    /// Validates an explicit profile table without changing live state.
    ///
    /// # Errors
    ///
    /// Returns a typed error for each invalid identity, selector, declaration,
    /// or serialized payload bound.
    pub fn new(
        profiles: &'static [LanguageServiceProfile],
    ) -> Result<Self, DiagnosticsRegistryError> {
        validate_profiles(profiles)?;
        Ok(Self { profiles })
    }

    /// Returns the profiles in declaration order.
    #[must_use]
    pub const fn profiles(&self) -> &'static [LanguageServiceProfile] {
        self.profiles
    }

    /// Selects the one profile that owns a path.
    ///
    /// # Errors
    ///
    /// Returns [`DiagnosticsSelectionError::UnsupportedPath`] for no match and
    /// [`DiagnosticsSelectionError::AmbiguousPath`] for multiple matches.
    pub fn profile(
        &self,
        path: &Path,
    ) -> Result<&'static LanguageServiceProfile, DiagnosticsSelectionError> {
        let mut found = None;
        for profile in self.profiles {
            if profile.supports_path(path) {
                if found.is_some() {
                    return Err(DiagnosticsSelectionError::AmbiguousPath);
                }
                found = Some(profile);
            }
        }
        found.ok_or(DiagnosticsSelectionError::UnsupportedPath)
    }

    /// Selects a profile by a case-insensitive language name.
    #[must_use]
    pub fn profile_of_language(&self, language: &str) -> Option<&'static LanguageServiceProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.supports_language(language))
    }
}

/// The dimension that failed registry validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsRegistryMeasure {
    /// Number of profiles.
    Profiles,
    /// Profile identifier bytes.
    ProfileId,
    /// Profile version bytes.
    ProfileVersion,
    /// Number of language names.
    LanguageNames,
    /// Language-name bytes.
    LanguageName,
    /// Extension bytes.
    Extension,
    /// Complete file-name bytes.
    FileName,
    /// Number of servers.
    Servers,
    /// Server identifier and fallback-source bytes.
    ServerIdAndSource,
    /// Server program bytes.
    Program,
    /// Number of server arguments.
    Arguments,
    /// Bytes in one server argument.
    Argument,
    /// Aggregate program and argument bytes.
    Command,
    /// Protocol language identifier bytes.
    LanguageId,
    /// Number of root markers.
    RootMarkers,
    /// Root-marker bytes.
    RootMarker,
    /// Serialized initialization-options bytes.
    InitializationOptions,
    /// Serialized workspace-settings bytes.
    WorkspaceSettings,
}

/// Why a diagnostics profile registry is invalid.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum DiagnosticsRegistryError {
    /// A count or byte length is outside its accepted bound.
    #[error("profile {profile} server {server:?} has invalid {measure:?}: {actual}, limit {limit}")]
    Bound {
        /// Profile identifier, or an empty string for the registry itself.
        profile: &'static str,
        /// Server index when the dimension belongs to a server.
        server: Option<usize>,
        /// Failed dimension.
        measure: DiagnosticsRegistryMeasure,
        /// Supplied count or byte length.
        actual: usize,
        /// Largest accepted count or byte length.
        limit: usize,
    },
    /// A required string is empty or a stable identity is not ASCII.
    #[error("profile {profile} server {server:?} has invalid {measure:?}")]
    InvalidValue {
        /// Profile identifier.
        profile: &'static str,
        /// Server index when applicable.
        server: Option<usize>,
        /// Failed dimension.
        measure: DiagnosticsRegistryMeasure,
    },
    /// Two profiles use the same stable identifier.
    #[error("duplicate profile identifier {profile}")]
    DuplicateProfileId {
        /// Duplicated identifier.
        profile: &'static str,
    },
    /// A language name is duplicated with ASCII case folded.
    #[error("duplicate language name {name}")]
    DuplicateLanguageName {
        /// Duplicated name.
        name: &'static str,
    },
    /// An extension or file name is duplicated inside one profile.
    #[error("profile {profile} has duplicate {measure:?} {selector}")]
    DuplicatePathSelector {
        /// Profile identifier.
        profile: &'static str,
        /// Selector kind.
        measure: DiagnosticsRegistryMeasure,
        /// Duplicated selector.
        selector: &'static str,
    },
    /// One profile repeats a server identifier.
    #[error("profile {profile} has duplicate server identifier {server_id}")]
    DuplicateServer {
        /// Profile identifier.
        profile: &'static str,
        /// Duplicated server identifier.
        server_id: &'static str,
    },
    /// One profile declares more than one formatting server.
    #[error("profile {profile} declares more than one formatting server")]
    DuplicateFormatter {
        /// Profile identifier.
        profile: &'static str,
    },
    /// A root marker contains a directory component or another invalid value.
    #[error("profile {profile} server {server} has invalid root marker")]
    InvalidRootMarker {
        /// Profile identifier.
        profile: &'static str,
        /// Server index.
        server: usize,
    },
    /// JSON serialization failed before its size could be validated.
    #[error("profile {profile} server {server} could not serialize {measure:?}")]
    Serialization {
        /// Profile identifier.
        profile: &'static str,
        /// Server index.
        server: usize,
        /// Payload kind.
        measure: DiagnosticsRegistryMeasure,
    },
}

fn bound(
    profile: &'static str,
    server: Option<usize>,
    measure: DiagnosticsRegistryMeasure,
    actual: usize,
    limit: usize,
) -> Result<(), DiagnosticsRegistryError> {
    if actual > limit {
        return Err(DiagnosticsRegistryError::Bound {
            profile,
            server,
            measure,
            actual,
            limit,
        });
    }
    Ok(())
}

fn required_ascii(
    profile: &'static str,
    server: Option<usize>,
    measure: DiagnosticsRegistryMeasure,
    value: &str,
    limit: usize,
) -> Result<(), DiagnosticsRegistryError> {
    if value.is_empty() || !value.is_ascii() {
        return Err(DiagnosticsRegistryError::InvalidValue {
            profile,
            server,
            measure,
        });
    }
    bound(profile, server, measure, value.len(), limit)
}

fn validate_profiles(profiles: &[LanguageServiceProfile]) -> Result<(), DiagnosticsRegistryError> {
    if profiles.is_empty() {
        return Err(DiagnosticsRegistryError::Bound {
            profile: "",
            server: None,
            measure: DiagnosticsRegistryMeasure::Profiles,
            actual: 0,
            limit: LANGUAGE_PROFILES_MAX,
        });
    }
    bound(
        "",
        None,
        DiagnosticsRegistryMeasure::Profiles,
        profiles.len(),
        LANGUAGE_PROFILES_MAX,
    )?;
    for (index, profile) in profiles.iter().enumerate() {
        required_ascii(
            profile.id,
            None,
            DiagnosticsRegistryMeasure::ProfileId,
            profile.id,
            LANGUAGE_SERVICE_ID_BYTES_MAX,
        )?;
        required_ascii(
            profile.id,
            None,
            DiagnosticsRegistryMeasure::ProfileVersion,
            profile.version,
            LANGUAGE_SERVICE_ID_BYTES_MAX,
        )?;
        if profiles[..index]
            .iter()
            .any(|earlier| earlier.id == profile.id)
        {
            return Err(DiagnosticsRegistryError::DuplicateProfileId {
                profile: profile.id,
            });
        }
        if profile.language_names.is_empty() {
            return Err(DiagnosticsRegistryError::Bound {
                profile: profile.id,
                server: None,
                measure: DiagnosticsRegistryMeasure::LanguageNames,
                actual: 0,
                limit: LANGUAGE_NAMES_MAX,
            });
        }
        bound(
            profile.id,
            None,
            DiagnosticsRegistryMeasure::LanguageNames,
            profile.language_names.len(),
            LANGUAGE_NAMES_MAX,
        )?;
        for (name_index, name) in profile.language_names.iter().enumerate() {
            required_ascii(
                profile.id,
                None,
                DiagnosticsRegistryMeasure::LanguageName,
                name,
                LANGUAGE_SELECTOR_BYTES_MAX,
            )?;
            if profile.language_names[..name_index]
                .iter()
                .any(|earlier| earlier.eq_ignore_ascii_case(name))
                || profiles[..index]
                    .iter()
                    .any(|earlier| earlier.supports_language(name))
            {
                return Err(DiagnosticsRegistryError::DuplicateLanguageName { name });
            }
        }
        if !profile.language_names.contains(&profile.id) {
            return Err(DiagnosticsRegistryError::InvalidValue {
                profile: profile.id,
                server: None,
                measure: DiagnosticsRegistryMeasure::LanguageName,
            });
        }
        for (selector_index, selector) in profile.extensions.iter().enumerate() {
            required_ascii(
                profile.id,
                None,
                DiagnosticsRegistryMeasure::Extension,
                selector,
                LANGUAGE_SELECTOR_BYTES_MAX,
            )?;
            if profile.extensions[..selector_index].contains(selector) {
                return Err(DiagnosticsRegistryError::DuplicatePathSelector {
                    profile: profile.id,
                    measure: DiagnosticsRegistryMeasure::Extension,
                    selector,
                });
            }
        }
        for (selector_index, selector) in profile.file_names.iter().enumerate() {
            required_ascii(
                profile.id,
                None,
                DiagnosticsRegistryMeasure::FileName,
                selector,
                LANGUAGE_SELECTOR_BYTES_MAX,
            )?;
            if profile.file_names[..selector_index].contains(selector) {
                return Err(DiagnosticsRegistryError::DuplicatePathSelector {
                    profile: profile.id,
                    measure: DiagnosticsRegistryMeasure::FileName,
                    selector,
                });
            }
        }
        validate_servers(profile)?;
    }
    Ok(())
}

fn validate_servers(profile: &LanguageServiceProfile) -> Result<(), DiagnosticsRegistryError> {
    bound(
        profile.id,
        None,
        DiagnosticsRegistryMeasure::Servers,
        profile.language_servers.len(),
        LANGUAGE_SERVERS_MAX,
    )?;
    let mut formatters = 0;
    for (index, server) in profile.language_servers.iter().enumerate() {
        if profile.language_servers[..index]
            .iter()
            .any(|earlier| earlier.id == server.id)
        {
            return Err(DiagnosticsRegistryError::DuplicateServer {
                profile: profile.id,
                server_id: server.id,
            });
        }
        formatters += usize::from(server.formatting == ServerFormatting::Enabled);
        required_ascii(
            profile.id,
            Some(index),
            DiagnosticsRegistryMeasure::ServerIdAndSource,
            server.id,
            LANGUAGE_SERVICE_ID_BYTES_MAX,
        )?;
        required_ascii(
            profile.id,
            Some(index),
            DiagnosticsRegistryMeasure::Program,
            server.program,
            LSP_SERVER_PROGRAM_BYTES_MAX,
        )?;
        bound(
            profile.id,
            Some(index),
            DiagnosticsRegistryMeasure::Arguments,
            server.args.len(),
            LSP_SERVER_ARGUMENTS_MAX,
        )?;
        for arg in server.args {
            bound(
                profile.id,
                Some(index),
                DiagnosticsRegistryMeasure::Argument,
                arg.len(),
                LSP_SERVER_ARGUMENT_BYTES_MAX,
            )?;
        }
        let command_bytes =
            server.program.len() + server.args.iter().map(|arg| arg.len()).sum::<usize>();
        bound(
            profile.id,
            Some(index),
            DiagnosticsRegistryMeasure::Command,
            command_bytes,
            LSP_SERVER_COMMAND_BYTES_MAX,
        )?;
        required_ascii(
            profile.id,
            Some(index),
            DiagnosticsRegistryMeasure::LanguageId,
            server.language_id,
            LSP_LANGUAGE_BYTES_MAX,
        )?;
        bound(
            profile.id,
            Some(index),
            DiagnosticsRegistryMeasure::RootMarkers,
            server.root_markers.len(),
            LANGUAGE_ROOT_MARKERS_MAX,
        )?;
        for marker in server.root_markers {
            if !marker_is_valid(marker) {
                return Err(DiagnosticsRegistryError::InvalidRootMarker {
                    profile: profile.id,
                    server: index,
                });
            }
            bound(
                profile.id,
                Some(index),
                DiagnosticsRegistryMeasure::RootMarker,
                marker.len(),
                LANGUAGE_SELECTOR_BYTES_MAX,
            )?;
        }
        for check_depth in [CheckDepth::Compile, CheckDepth::Lints] {
            for diagnostics_enabled in [false, true] {
                let settings = LanguageSettings {
                    check_depth,
                    diagnostics_enabled,
                };
                let options = serde_json::to_vec(&server.options(settings)).map_err(|_| {
                    DiagnosticsRegistryError::Serialization {
                        profile: profile.id,
                        server: index,
                        measure: DiagnosticsRegistryMeasure::InitializationOptions,
                    }
                })?;
                bound(
                    profile.id,
                    Some(index),
                    DiagnosticsRegistryMeasure::InitializationOptions,
                    options.len(),
                    LANGUAGE_INITIALIZATION_OPTIONS_BYTES_MAX,
                )?;
                if let Some(value) = server.settings(settings) {
                    let bytes = serde_json::to_vec(&value).map_err(|_| {
                        DiagnosticsRegistryError::Serialization {
                            profile: profile.id,
                            server: index,
                            measure: DiagnosticsRegistryMeasure::WorkspaceSettings,
                        }
                    })?;
                    bound(
                        profile.id,
                        Some(index),
                        DiagnosticsRegistryMeasure::WorkspaceSettings,
                        bytes.len(),
                        LANGUAGE_WORKSPACE_SETTINGS_BYTES_MAX,
                    )?;
                }
            }
        }
    }
    if formatters > 1 {
        return Err(DiagnosticsRegistryError::DuplicateFormatter {
            profile: profile.id,
        });
    }
    Ok(())
}

mod data;
pub(crate) use data::*;

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
