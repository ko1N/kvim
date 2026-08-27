//! Bounded semantic binding profiles for embedded hosts.
//!
//! A host consumes this manifest instead of parsing the standalone table. The
//! profile changes physical bindings only. It never changes which semantic
//! commands the editor can execute.

use std::collections::BTreeMap;

use kvim_keymap::{
    BINDINGS_MAX, Key, KeyCode, KeySequence, SequenceError, TextFallback, UnboundInput,
};
use kvim_settings::PENDING_KEYS_MAX;
use thiserror::Error;

use super::command::{Command, CommandGroup};
use super::mode::{BindingScope, Mode};
use super::registry::{Binding, Registry, RegistryError};

/// The maximum number of binding entries in one published manifest.
///
/// This equals the generic registry capacity. A realized registry validates this
/// limit before this crate publishes its entries.
pub const BINDING_MANIFEST_ENTRIES_MAX: usize = BINDINGS_MAX;

/// The maximum number of binding overrides in one profile realization.
pub const BINDING_OVERRIDES_MAX: usize = 128;

/// The maximum number of replacement bindings in one override list.
pub const BINDING_REPLACEMENTS_MAX: usize = 64;

/// The policy for a static key prefix while a host resolver has higher scopes.
///
/// This reports only static-prefix arbitration. The facade cancellation protocol
/// separately validates surface state before a host changes focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingInterruptionPolicy {
    /// A complete binding in a preceding scope can interrupt this prefix.
    PrecedingScopeMayInterruptPrefix,
}

/// One bounded semantic binding publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingManifestEntry {
    command: Command,
    scope: BindingScope,
    sequence: KeySequence,
    group: CommandGroup,
    description: &'static str,
    text_fallback: TextFallback,
    unbound_input: UnboundInput,
    interruption_policy: BindingInterruptionPolicy,
}

impl BindingManifestEntry {
    /// Returns the semantic command identity.
    #[must_use]
    pub const fn command(&self) -> Command {
        self.command
    }

    /// Returns the scope that owns this sequence.
    #[must_use]
    pub const fn scope(&self) -> BindingScope {
        self.scope
    }

    /// Returns the validated non-empty key sequence.
    #[must_use]
    pub fn sequence(&self) -> &KeySequence {
        &self.sequence
    }

    /// Returns the semantic command group.
    #[must_use]
    pub const fn group(&self) -> CommandGroup {
        self.group
    }

    /// Returns the command description.
    #[must_use]
    pub const fn description(&self) -> &'static str {
        self.description
    }

    /// Returns the scope text fallback.
    #[must_use]
    pub const fn text_fallback(&self) -> TextFallback {
        self.text_fallback
    }

    /// Returns the scope unbound-input behavior.
    #[must_use]
    pub const fn unbound_input(&self) -> UnboundInput {
        self.unbound_input
    }

    /// Returns the static-prefix interruption policy.
    #[must_use]
    pub const fn interruption_policy(&self) -> BindingInterruptionPolicy {
        self.interruption_policy
    }
}

/// A bounded semantic publication of one profile's bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingManifest(Vec<BindingManifestEntry>);

impl BindingManifest {
    /// Returns every binding in deterministic scope and sequence order.
    #[must_use]
    pub fn entries(&self) -> &[BindingManifestEntry] {
        &self.0
    }
}

/// The binding preset that realizes the editor key table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingProfile {
    /// The first-release standalone key table.
    Standalone,
    /// A host-friendly table that leaves host navigation keys unclaimed.
    Embedded,
}

/// A semantic command binding override.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingOverride {
    /// Restore every first-release binding of this semantic command.
    ///
    /// This explicitly restores bindings that the embedded profile removes.
    Enable(Command),
    /// Remove every binding that reaches this semantic command.
    Disable(Command),
    /// Replace every profile binding of this command with supplied mappings.
    Replace(BindingReplacement),
}

/// A replacement mapping for one semantic command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingReplacement {
    scope: BindingScope,
    sequence: KeySequence,
    command: Command,
}

impl BindingReplacement {
    /// Builds a bounded replacement mapping.
    ///
    /// ```
    /// use kvim_input::{
    ///     BindingReplacement, BindingScope, Command, Key, KeyCode, Mode,
    /// };
    ///
    /// let sequence = [Key::plain(KeyCode::Char('u'))];
    /// let replacement = BindingReplacement::new(
    ///     BindingScope::Mode(Mode::Normal),
    ///     &sequence,
    ///     Command::Undo,
    /// )?;
    /// # Ok::<(), kvim_input::BindingReplacementError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`BindingReplacementError::Sequence`] when the sequence is empty
    /// or exceeds the pending-key capacity.
    pub fn new(
        scope: BindingScope,
        sequence: &[Key],
        command: Command,
    ) -> Result<Self, BindingReplacementError> {
        Ok(Self {
            scope,
            sequence: KeySequence::new(sequence, PENDING_KEYS_MAX)?,
            command,
        })
    }
}

/// A rejected replacement mapping.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum BindingReplacementError {
    /// The replacement sequence violates the pending-key bound.
    #[error(transparent)]
    Sequence(#[from] SequenceError),
}

/// A rejected binding-profile realization.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BindingProfileError {
    /// The override list exceeds its published bound.
    #[error("the profile holds {overrides} overrides, but the maximum is {BINDING_OVERRIDES_MAX}")]
    TooManyOverrides { overrides: usize },
    /// The replacement list exceeds its published bound.
    #[error(
        "the profile holds {replacements} replacements, but the maximum is {BINDING_REPLACEMENTS_MAX}"
    )]
    TooManyReplacements { replacements: usize },
    /// One command has contradictory override declarations.
    #[error("the profile has contradictory overrides for {command}")]
    ContradictoryOverride { command: Command },
    /// The realized key registry violates a sequence or scope invariant.
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum OverrideKind {
    Enable,
    Disable,
    Replace,
}

impl BindingProfile {
    /// Builds the profile registry with no caller overrides.
    ///
    /// ```
    /// use kvim_input::{BindingProfile, Command, Key, KeyCode, Mode};
    ///
    /// let profile = BindingProfile::Embedded.registry().expect("the built-in profile is valid");
    /// assert_eq!(
    ///     profile.command(Mode::Normal, &[Key::plain(KeyCode::Char(' ')), Key::plain(KeyCode::Char('g')), Key::plain(KeyCode::Char('g'))]),
    ///     None,
    /// );
    /// assert_eq!(Command::OpenReview.id(), "open-review");
    /// ```
    pub fn registry(self) -> Result<Registry, BindingProfileError> {
        self.with_overrides(&[])
    }

    /// Realizes this profile with bounded semantic binding overrides.
    ///
    /// Disabling a command removes only its physical mappings. Replacing a
    /// command removes its profile mappings before it adds all replacements.
    /// Neither operation removes semantic editor execution.
    ///
    /// # Errors
    ///
    /// Returns [`BindingProfileError::ContradictoryOverride`] when enable,
    /// disable, and replacement declarations name the same command.
    pub fn with_overrides(
        self,
        overrides: &[BindingOverride],
    ) -> Result<Registry, BindingProfileError> {
        if overrides.len() > BINDING_OVERRIDES_MAX {
            return Err(BindingProfileError::TooManyOverrides {
                overrides: overrides.len(),
            });
        }
        let replacements = overrides
            .iter()
            .filter(|item| matches!(item, BindingOverride::Replace(_)))
            .count();
        if replacements > BINDING_REPLACEMENTS_MAX {
            return Err(BindingProfileError::TooManyReplacements { replacements });
        }

        let kinds = override_kinds(overrides)?;
        let first_release = Registry::first_release_bindings();
        let mut bindings = profile_bindings(self, &first_release);
        for (command, kind) in kinds {
            match kind {
                OverrideKind::Enable => {
                    bindings.retain(|binding| binding.command != command);
                    bindings.extend(
                        first_release
                            .iter()
                            .filter(|binding| binding.command == command)
                            .cloned(),
                    );
                }
                OverrideKind::Disable => bindings.retain(|binding| binding.command != command),
                OverrideKind::Replace => {
                    bindings.retain(|binding| binding.command != command);
                    bindings.extend(overrides.iter().filter_map(|override_| match override_ {
                        BindingOverride::Replace(replacement) if replacement.command == command => {
                            Some(Binding::surface(
                                replacement.scope,
                                replacement.sequence.keys(),
                                replacement.command,
                            ))
                        }
                        BindingOverride::Enable(_)
                        | BindingOverride::Disable(_)
                        | BindingOverride::Replace(_) => None,
                    }));
                }
            }
        }
        Registry::from_bindings(&bindings, PENDING_KEYS_MAX).map_err(Into::into)
    }

    /// Publishes the semantic binding manifest for this profile.
    pub fn manifest(self) -> Result<BindingManifest, BindingProfileError> {
        self.manifest_with_overrides(&[])
    }

    /// Publishes the semantic binding manifest after bounded overrides apply.
    pub fn manifest_with_overrides(
        self,
        overrides: &[BindingOverride],
    ) -> Result<BindingManifest, BindingProfileError> {
        let registry = self.with_overrides(overrides)?;
        let mut entries = Vec::with_capacity(BINDING_MANIFEST_ENTRIES_MAX);
        for scope in BindingScope::ALL {
            for (sequence, command) in registry.bindings(scope) {
                entries.push(BindingManifestEntry {
                    command,
                    scope,
                    sequence: sequence.clone(),
                    group: command.group(),
                    description: command.label(),
                    text_fallback: scope.text_fallback(),
                    unbound_input: scope.unbound_input(),
                    interruption_policy:
                        BindingInterruptionPolicy::PrecedingScopeMayInterruptPrefix,
                });
            }
        }
        debug_assert!(
            entries.len() <= BINDING_MANIFEST_ENTRIES_MAX,
            "the validated generic registry limits the manifest entry count"
        );
        Ok(BindingManifest(entries))
    }
}

fn override_kinds(
    overrides: &[BindingOverride],
) -> Result<BTreeMap<Command, OverrideKind>, BindingProfileError> {
    let mut kinds = BTreeMap::new();
    for override_ in overrides {
        let (command, kind) = match override_ {
            BindingOverride::Enable(command) => (*command, OverrideKind::Enable),
            BindingOverride::Disable(command) => (*command, OverrideKind::Disable),
            BindingOverride::Replace(replacement) => (replacement.command, OverrideKind::Replace),
        };
        if let Some(existing) = kinds.insert(command, kind) {
            if existing != kind {
                return Err(BindingProfileError::ContradictoryOverride { command });
            }
        }
    }
    Ok(kinds)
}

fn profile_bindings(profile: BindingProfile, first_release: &[Binding]) -> Vec<Binding> {
    let mut bindings = first_release.to_vec();
    if matches!(profile, BindingProfile::Embedded) {
        bindings.retain(|binding| {
            binding.command != Command::OpenReview
                && !is_embedded_host_navigation(binding.scope, &binding.keys)
        });
        add_embedded_secondary_bindings(&mut bindings);
    }
    bindings
}

fn is_embedded_host_navigation(scope: BindingScope, sequence: &[Key]) -> bool {
    matches!(
        scope,
        BindingScope::Mode(Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
            | BindingScope::Sidebar
    ) && (sequence == [Key::plain(KeyCode::Tab)] || sequence == [Key::plain(KeyCode::BackTab)])
}

fn add_embedded_secondary_bindings(bindings: &mut Vec<Binding>) {
    let normal = BindingScope::Mode(Mode::Normal);
    bindings.push(Binding::surface(
        normal,
        &[
            Key::plain(KeyCode::Char(']')),
            Key::plain(KeyCode::Char('j')),
        ],
        Command::JumpForward,
    ));
    bindings.push(Binding::surface(
        normal,
        &[
            Key::plain(KeyCode::Char('[')),
            Key::plain(KeyCode::Char('j')),
        ],
        Command::JumpBack,
    ));
    bindings.push(Binding::surface(
        BindingScope::Review,
        &[
            Key::plain(KeyCode::Char(']')),
            Key::plain(KeyCode::Char('s')),
        ],
        Command::NextReviewSection,
    ));
    bindings.push(Binding::surface(
        BindingScope::Review,
        &[
            Key::plain(KeyCode::Char('[')),
            Key::plain(KeyCode::Char('s')),
        ],
        Command::PreviousReviewSection,
    ));
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
