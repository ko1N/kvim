//! Host and kvim binding composition for one host-owned resolver.

use std::fmt;
use std::sync::Arc;

use kvim_input::{
    BINDING_MANIFEST_ENTRIES_MAX, BindingManifest, BindingScope, Command, CommandGroup,
    ContextGeneration, DispatchContext, InputContextSnapshot, Key, Mode, SemanticPhases,
    TextFallback,
};
use kvim_keymap::{
    BINDINGS_MAX, Binding, BoundCommand, CommandMetadata, CommandOwner, Registry, RegistryError,
    Scope, UnboundInput,
};
use kvim_settings::PENDING_KEYS_MAX;
use thiserror::Error;

use crate::worktree::WorktreeBindingContext;

/// The maximum number of host bindings accepted by one merged model.
pub const WORKTREE_HOST_BINDINGS_MAX: usize = 128;
/// The maximum number of explicit conflict overrides accepted by one model.
pub const WORKTREE_BINDING_OVERRIDES_MAX: usize = 128;
/// The maximum length of a published which-key owner label.
pub const WORKTREE_OWNER_LABEL_BYTES_MAX: usize = 32;
/// The maximum length of a published which-key group label.
pub const WORKTREE_GROUP_LABEL_BYTES_MAX: usize = 32;

const EDITOR_HOST_SCOPES: [BindingScope; 5] = [
    BindingScope::Mode(Mode::Normal),
    BindingScope::Mode(Mode::Visual),
    BindingScope::Mode(Mode::VisualLine),
    BindingScope::Mode(Mode::VisualBlock),
    BindingScope::Sidebar,
];

/// Metadata required from an opaque host command.
pub trait WorktreeHostCommand: CommandMetadata {
    /// Returns the bounded owner label shown by which-key.
    fn owner_label(&self) -> &str;
    /// Returns the bounded semantic group shown by which-key.
    fn group_label(&self) -> &str;
}

/// The focused host surface category used for binding projection.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WorktreeBindingFocus {
    /// A host chat surface.
    Chat,
    /// A kvim editor surface.
    Editor,
    /// A review surface.
    Review,
}

/// Where one host binding participates in resolution.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WorktreeHostBindingLayer {
    /// The binding receives first refusal in every focus.
    Global,
    /// The binding is part of the host leader for one focus category.
    Leader(WorktreeBindingFocus),
    /// The binding applies directly to one focused context category.
    Focused(WorktreeBindingFocus),
}

/// One bounded host binding contribution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeHostBinding<H: WorktreeHostCommand> {
    layer: WorktreeHostBindingLayer,
    sequence: Vec<Key>,
    command: H,
}

impl<H: WorktreeHostCommand> WorktreeHostBinding<H> {
    /// Creates one bounded host contribution.
    ///
    /// Editor leader and focused contributions participate only in Normal,
    /// Visual, and sidebar contexts. Insert, picker, prompt, confirmation,
    /// register-selection, and operator-pending contexts retain literal and
    /// semantic input ownership. Use [`WorktreeHostBindingLayer::Global`] only
    /// for a binding that must receive first refusal in every context.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeHostBindingError`] before sequence allocation when the
    /// sequence is invalid or a presentation label exceeds its byte bound.
    pub fn new(
        layer: WorktreeHostBindingLayer,
        sequence: &[Key],
        command: H,
    ) -> Result<Self, WorktreeHostBindingError> {
        if sequence.is_empty() {
            return Err(WorktreeHostBindingError::EmptySequence);
        }
        if sequence.len() > usize::from(PENDING_KEYS_MAX) {
            return Err(WorktreeHostBindingError::SequenceTooLong {
                keys: sequence.len(),
            });
        }
        if command.owner_label().len() > WORKTREE_OWNER_LABEL_BYTES_MAX {
            return Err(WorktreeHostBindingError::OwnerLabelTooLong {
                bytes: command.owner_label().len(),
            });
        }
        if command.group_label().len() > WORKTREE_GROUP_LABEL_BYTES_MAX {
            return Err(WorktreeHostBindingError::GroupLabelTooLong {
                bytes: command.group_label().len(),
            });
        }
        Ok(Self {
            layer,
            sequence: sequence.to_vec(),
            command,
        })
    }
}

/// Why one host contribution was rejected before composition.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeHostBindingError {
    /// The sequence held no key.
    #[error("a host binding sequence holds no key")]
    EmptySequence,
    /// The sequence exceeded kvim's pending-key limit.
    #[error("a host binding holds {keys} keys, but the maximum is {PENDING_KEYS_MAX}")]
    SequenceTooLong {
        /// Rejected key count.
        keys: usize,
    },
    /// The owner label exceeded its public bound.
    #[error(
        "a host owner label holds {bytes} bytes, but the maximum is {WORKTREE_OWNER_LABEL_BYTES_MAX}"
    )]
    OwnerLabelTooLong {
        /// Rejected byte count.
        bytes: usize,
    },
    /// The group label exceeded its public bound.
    #[error(
        "a host group label holds {bytes} bytes, but the maximum is {WORKTREE_GROUP_LABEL_BYTES_MAX}"
    )]
    GroupLabelTooLong {
        /// Rejected byte count.
        bytes: usize,
    },
}

/// The addressed identity and owner of one merged command.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WorktreeAddressedCommand<H> {
    /// A host-owned command identity.
    Host(H),
    /// A kvim-owned semantic command identity.
    Editor(Command),
}

/// One command in the merged host and kvim registry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WorktreeMergedCommand<H> {
    /// An opaque host command.
    Host(H),
    /// A semantic kvim command with its effective which-key group.
    Editor {
        /// The semantic command identity.
        command: Command,
        /// The group published for the effective focused context.
        group: CommandGroup,
    },
}

impl<H: WorktreeHostCommand> fmt::Display for WorktreeMergedCommand<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl<H: WorktreeHostCommand> CommandMetadata for WorktreeMergedCommand<H> {
    fn id(&self) -> &str {
        match self {
            Self::Host(command) => command.id(),
            Self::Editor { command, .. } => command.id(),
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Host(command) => command.label(),
            Self::Editor { command, .. } => command.label(),
        }
    }
}

impl<H: WorktreeHostCommand> WorktreeMergedCommand<H> {
    /// Returns the addressed identity without presentation metadata.
    #[must_use]
    pub const fn addressed(self) -> WorktreeAddressedCommand<H> {
        match self {
            Self::Host(command) => WorktreeAddressedCommand::Host(command),
            Self::Editor { command, .. } => WorktreeAddressedCommand::Editor(command),
        }
    }

    /// Returns the bounded owner label for a which-key row.
    #[must_use]
    pub fn owner_label(&self) -> &str {
        match self {
            Self::Host(command) => command.owner_label(),
            Self::Editor { .. } => "kvim",
        }
    }

    /// Returns the bounded semantic group for a which-key row.
    #[must_use]
    pub fn group_label(&self) -> &str {
        match self {
            Self::Host(command) => command.group_label(),
            Self::Editor { group, .. } => editor_group_label(*group),
        }
    }
}

/// One effective table in the merged registry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WorktreeMergedScope {
    /// Host-global first refusal.
    HostGlobal,
    /// Host chat bindings.
    Chat,
    /// One kvim editor context with valid host contributions projected into it.
    Editor(BindingScope),
    /// Review bindings from the host and kvim.
    Review,
}

impl fmt::Display for WorktreeMergedScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostGlobal => formatter.write_str("Host Global"),
            Self::Chat => formatter.write_str("Chat"),
            Self::Editor(scope) => write!(formatter, "Editor {scope}"),
            Self::Review => formatter.write_str("Review"),
        }
    }
}

impl Scope for WorktreeMergedScope {
    const COUNT: usize = BindingScope::COUNT + 3;
}

/// An explicit winner for one effective binding conflict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeBindingOverride<H> {
    scope: WorktreeMergedScope,
    sequence: Vec<Key>,
    winner: WorktreeAddressedCommand<H>,
}

impl<H: Copy> WorktreeBindingOverride<H> {
    /// Addresses the binding that must win one or more direct conflicts.
    ///
    /// The scope, sequence, and typed owner identity must identify exactly one
    /// projected binding. A host and editor command remain distinct when their
    /// string identifiers are equal.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeBindingOverrideError`] before allocation when the
    /// winner sequence is empty or too long.
    pub fn new(
        scope: WorktreeMergedScope,
        sequence: &[Key],
        winner: WorktreeAddressedCommand<H>,
    ) -> Result<Self, WorktreeBindingOverrideError> {
        if sequence.is_empty() {
            return Err(WorktreeBindingOverrideError::EmptySequence);
        }
        if sequence.len() > usize::from(PENDING_KEYS_MAX) {
            return Err(WorktreeBindingOverrideError::SequenceTooLong {
                keys: sequence.len(),
            });
        }
        Ok(Self {
            scope,
            sequence: sequence.to_vec(),
            winner,
        })
    }
}

/// Why one conflict override was rejected before composition.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeBindingOverrideError {
    /// The winner sequence held no key.
    #[error("a binding override sequence holds no key")]
    EmptySequence,
    /// The winner sequence exceeded kvim's pending-key limit.
    #[error("a binding override holds {keys} keys, but the maximum is {PENDING_KEYS_MAX}")]
    SequenceTooLong {
        /// Rejected key count.
        keys: usize,
    },
}

/// The shape of one rejected binding conflict.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeBindingConflictKind {
    /// Two commands use the same sequence.
    DuplicateSequence,
    /// One command sequence is a strict prefix of another.
    StrictPrefix,
}

/// A validated merged registry and its context projection helpers.
#[derive(Clone, Debug)]
pub struct WorktreeBindingModel<H> {
    registry: Arc<Registry<WorktreeMergedCommand<H>, WorktreeMergedScope>>,
}

impl<H: WorktreeHostCommand> WorktreeBindingModel<H> {
    /// Composes host contributions with one kvim binding manifest.
    ///
    /// This strict entry point rejects every conflict. Use
    /// [`Self::compose_with_overrides`] when the host explicitly selects a
    /// winner.
    ///
    /// ```
    /// use std::{fmt, time::Duration};
    /// use kvim_embed::{
    ///     WorktreeBindingFocus, WorktreeBindingModel, WorktreeHostBinding,
    ///     WorktreeHostBindingLayer, WorktreeHostCommand,
    /// };
    /// use kvim_input::{BindingProfile, BindingScope, InputContextSnapshot, Key, KeyCode, Mode};
    /// use kvim_keymap::{CommandMetadata, Dispatch, Input, Resolver};
    ///
    /// #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// enum HostCommand { OpenSessions }
    /// impl fmt::Display for HostCommand {
    ///     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.id()) }
    /// }
    /// impl CommandMetadata for HostCommand {
    ///     fn id(&self) -> &str { "host-open-sessions" }
    ///     fn label(&self) -> &str { "Open sessions" }
    /// }
    /// impl WorktreeHostCommand for HostCommand {
    ///     fn owner_label(&self) -> &str { "Host" }
    ///     fn group_label(&self) -> &str { "workspace" }
    /// }
    ///
    /// let leader = Key::plain(KeyCode::Char(' '));
    /// let binding = WorktreeHostBinding::new(
    ///     WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Editor),
    ///     &[leader, Key::plain(KeyCode::Char('s'))],
    ///     HostCommand::OpenSessions,
    /// )?;
    /// let manifest = BindingProfile::Embedded.manifest()?;
    /// let model = WorktreeBindingModel::compose(&manifest, &[binding])?;
    /// let context = WorktreeBindingModel::<HostCommand>::editor_snapshot_context(
    ///     InputContextSnapshot::idle(BindingScope::Mode(Mode::Normal)),
    ///     None,
    /// );
    /// let mut resolver = Resolver::new(model.registry(), 4, Duration::ZERO);
    /// assert_eq!(
    ///     resolver.dispatch(&context, Input::Key(leader), Some(Duration::ZERO)),
    ///     Dispatch::Pending,
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeBindingCompositionError`] for collection bounds or
    /// any unapproved conflict.
    pub fn compose(
        manifest: &BindingManifest,
        host: &[WorktreeHostBinding<H>],
    ) -> Result<Self, WorktreeBindingCompositionError<H>> {
        Self::compose_with_overrides(manifest, host, &[])
    }

    /// Composes host and kvim bindings with explicit addressed winners.
    ///
    /// Contributions and overrides are sorted and matched by effective scope,
    /// physical sequence, and typed owner identity. Registration order never
    /// selects a winner. Every duplicate or strict-prefix pair requires one
    /// explicit winner. Stale, nonexistent, ambiguous, or losing winners are
    /// rejected.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeBindingCompositionError`] for bounds, invalid
    /// overrides, uncovered conflicts, or final registry validation.
    pub fn compose_with_overrides(
        manifest: &BindingManifest,
        host: &[WorktreeHostBinding<H>],
        overrides: &[WorktreeBindingOverride<H>],
    ) -> Result<Self, WorktreeBindingCompositionError<H>> {
        if host.len() > WORKTREE_HOST_BINDINGS_MAX {
            return Err(WorktreeBindingCompositionError::TooManyHostBindings {
                bindings: host.len(),
            });
        }
        if overrides.len() > WORKTREE_BINDING_OVERRIDES_MAX {
            return Err(WorktreeBindingCompositionError::TooManyOverrides {
                overrides: overrides.len(),
            });
        }
        debug_assert!(
            manifest.entries().len() <= BINDING_MANIFEST_ENTRIES_MAX,
            "BindingManifest validates its publication bound"
        );
        for binding in host {
            if binding.command.owner_label().len() > WORKTREE_OWNER_LABEL_BYTES_MAX {
                return Err(WorktreeBindingCompositionError::OwnerLabelTooLong {
                    command: binding.command,
                });
            }
            if binding.command.group_label().len() > WORKTREE_GROUP_LABEL_BYTES_MAX {
                return Err(WorktreeBindingCompositionError::GroupLabelTooLong {
                    command: binding.command,
                });
            }
        }

        let projected_host = host.iter().try_fold(0usize, |total, binding| {
            total
                .checked_add(projected_scope_count(binding.layer))
                .filter(|count| *count <= BINDINGS_MAX)
                .ok_or(WorktreeBindingCompositionError::TooManyProjectedBindings)
        })?;
        let capacity = manifest
            .entries()
            .len()
            .checked_add(projected_host)
            .filter(|count| *count <= BINDINGS_MAX)
            .ok_or(WorktreeBindingCompositionError::TooManyProjectedBindings)?;
        let mut bindings = Vec::with_capacity(capacity);

        for entry in manifest.entries() {
            let scope = if entry.scope() == BindingScope::Review {
                WorktreeMergedScope::Review
            } else {
                WorktreeMergedScope::Editor(entry.scope())
            };
            bindings.push(Binding::surface(
                scope,
                entry.sequence().keys(),
                WorktreeMergedCommand::Editor {
                    command: entry.command(),
                    group: if entry.scope() == BindingScope::Review {
                        CommandGroup::Review
                    } else {
                        entry.group()
                    },
                },
            ));
        }
        for binding in host {
            for_each_projected_scope(binding.layer, |scope| {
                bindings.push(Binding::host(
                    scope,
                    &binding.sequence,
                    WorktreeMergedCommand::Host(binding.command),
                ));
            });
        }
        bindings.sort();
        let bindings = apply_overrides(bindings, overrides)?;
        let registry = Registry::from_bindings(&bindings, PENDING_KEYS_MAX)?;
        Ok(Self {
            registry: Arc::new(registry),
        })
    }

    /// Returns the immutable registry used by dispatch and which-key.
    #[must_use]
    pub fn registry(&self) -> Arc<Registry<WorktreeMergedCommand<H>, WorktreeMergedScope>> {
        Arc::clone(&self.registry)
    }

    /// Projects a chat focus into the merged resolver context.
    #[must_use]
    pub const fn chat_context(
        generation: ContextGeneration,
    ) -> DispatchContext<WorktreeMergedScope> {
        DispatchContext {
            overlay: None,
            global: Some(WorktreeMergedScope::HostGlobal),
            focus: InputContextSnapshot {
                scope: WorktreeMergedScope::Chat,
                phases: SemanticPhases::IDLE,
                text_fallback: TextFallback::Typed(CommandOwner::Host),
                unbound_input: UnboundInput::Ignored,
                generation,
            },
        }
    }

    /// Projects the current facade binding context, including picker overlay.
    ///
    /// This entry point also proves that the facade's reserved escape key is a
    /// complete host-global command. The host must validate every context it
    /// receives before dispatch through that context.
    ///
    /// # Errors
    ///
    /// Returns [`WorktreeBindingContextError`] when the reserved key is absent,
    /// pending, editor-owned, or ambiguous in the effective host-global table.
    pub fn editor_context(
        &self,
        context: &WorktreeBindingContext,
    ) -> Result<DispatchContext<WorktreeMergedScope>, WorktreeBindingContextError> {
        let sequence = [context.reserved_escape()];
        validate_reserved_escape(
            self.registry
                .bound_command(WorktreeMergedScope::HostGlobal, &sequence),
            self.registry
                .has_longer_sequence(WorktreeMergedScope::HostGlobal, &sequence),
        )?;
        Ok(Self::editor_snapshot_context(
            context.context(),
            context.overlay_scope(),
        ))
    }

    /// Projects explicit editor focus and overlay snapshots.
    ///
    /// This form is useful to test or adapt a facade context without storing
    /// the facade-owned instance and reserved escape key.
    #[must_use]
    pub const fn editor_snapshot_context(
        focus: InputContextSnapshot<BindingScope>,
        overlay: Option<BindingScope>,
    ) -> DispatchContext<WorktreeMergedScope> {
        DispatchContext {
            overlay: match overlay {
                Some(scope) => Some(WorktreeMergedScope::Editor(scope)),
                None => None,
            },
            global: Some(WorktreeMergedScope::HostGlobal),
            focus: InputContextSnapshot {
                scope: WorktreeMergedScope::Editor(focus.scope),
                phases: focus.phases,
                text_fallback: focus.text_fallback,
                unbound_input: focus.unbound_input,
                generation: focus.generation,
            },
        }
    }

    /// Projects review focus into the merged resolver context.
    #[must_use]
    pub const fn review_context(
        generation: ContextGeneration,
    ) -> DispatchContext<WorktreeMergedScope> {
        DispatchContext {
            overlay: None,
            global: Some(WorktreeMergedScope::HostGlobal),
            focus: InputContextSnapshot {
                scope: WorktreeMergedScope::Review,
                phases: SemanticPhases::IDLE,
                text_fallback: TextFallback::None,
                unbound_input: UnboundInput::Ignored,
                generation,
            },
        }
    }
}

/// Why a facade context cannot use a composed host resolver.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorktreeBindingContextError {
    /// The reserved escape key has no host-global binding.
    #[error("the reserved escape key has no host-global binding")]
    ReservedEscapeAbsent,
    /// The reserved escape key starts a host-global prefix but completes no command.
    #[error("the reserved escape key is a pending host-global prefix")]
    ReservedEscapePending,
    /// The reserved escape key resolves to an editor-owned command.
    #[error("the reserved escape key resolves to an editor-owned command")]
    ReservedEscapeEditorOwned,
    /// The reserved escape key has more than one effective interpretation.
    #[error("the reserved escape key is ambiguous in the host-global scope")]
    ReservedEscapeAmbiguous,
}

/// Why host and kvim binding composition failed.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum WorktreeBindingCompositionError<H> {
    /// The host contribution slice exceeded its public bound.
    #[error(
        "the host supplied {bindings} bindings, but the maximum is {WORKTREE_HOST_BINDINGS_MAX}"
    )]
    TooManyHostBindings {
        /// Rejected binding count.
        bindings: usize,
    },
    /// The override slice exceeded its public bound.
    #[error(
        "the host supplied {overrides} overrides, but the maximum is {WORKTREE_BINDING_OVERRIDES_MAX}"
    )]
    TooManyOverrides {
        /// Rejected override count.
        overrides: usize,
    },
    /// A host command changed its owner label after binding construction.
    #[error("the owner label of `{command}` exceeds {WORKTREE_OWNER_LABEL_BYTES_MAX} bytes")]
    OwnerLabelTooLong {
        /// Command carrying the label.
        command: H,
    },
    /// A host command changed its group label after binding construction.
    #[error("the group label of `{command}` exceeds {WORKTREE_GROUP_LABEL_BYTES_MAX} bytes")]
    GroupLabelTooLong {
        /// Command carrying the label.
        command: H,
    },
    /// Focus projection would exceed the generic registry bound.
    #[error("the projected merged registry exceeds its binding bound")]
    TooManyProjectedBindings,
    /// An override does not identify a projected binding.
    #[error("a binding override does not identify an existing projected binding")]
    OverrideTargetNotFound,
    /// An override identifies multiple indistinguishable projected bindings.
    #[error("a binding override identifies multiple projected bindings")]
    OverrideTargetAmbiguous,
    /// An override identifies a binding with no conflict.
    #[error("a binding override is stale because its target has no conflict")]
    OverrideHasNoConflict,
    /// Both sides of one conflict were selected as winners.
    #[error("one binding conflict has two explicit winners")]
    ConflictingOverrides,
    /// A selected winner loses another conflict.
    #[error("an override winner is removed by another conflict override")]
    OverrideWinnerRemoved,
    /// One conflict had no explicit addressed winner.
    #[error("an effective {kind:?} binding conflict is not explicitly covered")]
    UnapprovedConflict {
        /// Effective scope containing the conflict.
        scope: WorktreeMergedScope,
        /// Conflict shape.
        kind: WorktreeBindingConflictKind,
        /// First addressed command in deterministic order.
        first: WorktreeAddressedCommand<H>,
        /// Second addressed command in deterministic order.
        second: WorktreeAddressedCommand<H>,
    },
    /// The final generic registry rejected a non-conflict invariant.
    #[error(transparent)]
    Registry(#[from] RegistryError<WorktreeMergedCommand<H>, WorktreeMergedScope>),
}

fn validate_reserved_escape<H: WorktreeHostCommand>(
    command: Option<BoundCommand<WorktreeMergedCommand<H>>>,
    has_extension: bool,
) -> Result<(), WorktreeBindingContextError> {
    match (command, has_extension) {
        (None, false) => Err(WorktreeBindingContextError::ReservedEscapeAbsent),
        (None, true) => Err(WorktreeBindingContextError::ReservedEscapePending),
        (Some(_), true) => Err(WorktreeBindingContextError::ReservedEscapeAmbiguous),
        (Some(bound), false) => match (bound.owner, bound.command) {
            (CommandOwner::Host, WorktreeMergedCommand::Host(_)) => Ok(()),
            (CommandOwner::Surface, WorktreeMergedCommand::Editor { .. }) => {
                Err(WorktreeBindingContextError::ReservedEscapeEditorOwned)
            }
            _ => Err(WorktreeBindingContextError::ReservedEscapeAmbiguous),
        },
    }
}

fn apply_overrides<H: WorktreeHostCommand>(
    bindings: Vec<Binding<WorktreeMergedCommand<H>, WorktreeMergedScope>>,
    overrides: &[WorktreeBindingOverride<H>],
) -> Result<
    Vec<Binding<WorktreeMergedCommand<H>, WorktreeMergedScope>>,
    WorktreeBindingCompositionError<H>,
> {
    let mut targets = Vec::with_capacity(overrides.len());
    for binding_override in overrides {
        let mut matches = bindings.iter().enumerate().filter(|(_, binding)| {
            binding.scope == binding_override.scope
                && binding.keys == binding_override.sequence
                && binding.command.addressed() == binding_override.winner
        });
        let Some((target, _)) = matches.next() else {
            return Err(WorktreeBindingCompositionError::OverrideTargetNotFound);
        };
        if matches.next().is_some() {
            return Err(WorktreeBindingCompositionError::OverrideTargetAmbiguous);
        }
        targets.push(target);
    }

    let mut conflicts = Vec::new();
    let mut losers = vec![false; bindings.len()];
    let mut used = vec![false; overrides.len()];
    for first in 0..bindings.len() {
        for second in first + 1..bindings.len() {
            let Some(kind) = conflict_kind(&bindings[first], &bindings[second]) else {
                continue;
            };
            conflicts.push((first, second, kind));
            let first_override = targets.iter().position(|target| *target == first);
            let second_override = targets.iter().position(|target| *target == second);
            match (first_override, second_override) {
                (None, None) => {}
                (Some(_), Some(_)) => {
                    return Err(WorktreeBindingCompositionError::ConflictingOverrides);
                }
                (Some(index), None) => {
                    used[index] = true;
                    losers[second] = true;
                }
                (None, Some(index)) => {
                    used[index] = true;
                    losers[first] = true;
                }
            }
        }
    }
    if targets.iter().any(|target| losers[*target]) {
        return Err(WorktreeBindingCompositionError::OverrideWinnerRemoved);
    }
    for (first, second, kind) in conflicts {
        if !losers[first] && !losers[second] {
            return Err(WorktreeBindingCompositionError::UnapprovedConflict {
                scope: bindings[first].scope,
                kind,
                first: bindings[first].command.addressed(),
                second: bindings[second].command.addressed(),
            });
        }
    }
    if used.iter().any(|used| !used) {
        return Err(WorktreeBindingCompositionError::OverrideHasNoConflict);
    }

    Ok(bindings
        .into_iter()
        .zip(losers)
        .filter_map(|(binding, loser)| (!loser).then_some(binding))
        .collect())
}

fn conflict_kind<C, S>(
    first: &Binding<C, S>,
    second: &Binding<C, S>,
) -> Option<WorktreeBindingConflictKind>
where
    S: Eq,
{
    if first.scope != second.scope {
        return None;
    }
    if first.keys == second.keys {
        return Some(WorktreeBindingConflictKind::DuplicateSequence);
    }
    if first.keys.starts_with(&second.keys) || second.keys.starts_with(&first.keys) {
        return Some(WorktreeBindingConflictKind::StrictPrefix);
    }
    None
}

const fn projected_scope_count(layer: WorktreeHostBindingLayer) -> usize {
    match layer {
        WorktreeHostBindingLayer::Global => 1,
        WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Editor)
        | WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Editor) => {
            EDITOR_HOST_SCOPES.len()
        }
        WorktreeHostBindingLayer::Leader(_) | WorktreeHostBindingLayer::Focused(_) => 1,
    }
}

fn for_each_projected_scope(
    layer: WorktreeHostBindingLayer,
    mut apply: impl FnMut(WorktreeMergedScope),
) {
    match layer {
        WorktreeHostBindingLayer::Global => apply(WorktreeMergedScope::HostGlobal),
        WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Chat)
        | WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Chat) => {
            apply(WorktreeMergedScope::Chat);
        }
        WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Review)
        | WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Review) => {
            apply(WorktreeMergedScope::Review);
        }
        WorktreeHostBindingLayer::Leader(WorktreeBindingFocus::Editor)
        | WorktreeHostBindingLayer::Focused(WorktreeBindingFocus::Editor) => {
            for scope in EDITOR_HOST_SCOPES {
                apply(WorktreeMergedScope::Editor(scope));
            }
        }
    }
}

const fn editor_group_label(group: CommandGroup) -> &'static str {
    match group {
        CommandGroup::Search => "search",
        CommandGroup::Code => "code",
        CommandGroup::Window => "window",
        CommandGroup::Buffer => "buffer",
        CommandGroup::Tree => "tree",
        CommandGroup::Review => "review",
        CommandGroup::Other => "editor",
    }
}

#[cfg(test)]
#[path = "composition_tests.rs"]
mod tests;
