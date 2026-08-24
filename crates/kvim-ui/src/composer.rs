//! The one domain-neutral composition model of a host-owned workspace.
//!
//! [`WorkspaceComposer`] combines the split geometry of [`WindowTree`], the
//! sidebar regions, the overlay ownership, the input focus, one shared
//! [`Resolver`], and the which-key state of that resolver. It owns no host
//! surface value and no host command: the host supplies opaque surface
//! identities, geometry, and one published [`InputContextSnapshot`] for each
//! identity.
//!
//! One reduction routes a key or a paste to one host command, one surface
//! command, one typed-text owner, one pending sequence, one unsupported input,
//! or one unbound result. One layout pass returns the clipped rectangle of
//! every visible surface and of the open overlay.
//!
//! A focus or overlay transition that needs surface state returns one bounded,
//! addressed [`CompositionEffect::CancelPending`]. Focus and overlay ownership
//! stay unchanged until the host applies that effect to the named surface and
//! resumes with the reset snapshot. See `docs/embedding.md`.
//!
//! The module is pure. It reads no clock, no filesystem, and no terminal, it
//! starts no task, and it accepts no host input or render callback.
//!
//! `crates/kvim-tui/examples/host_workspace.rs` is one complete host of one
//! such workspace: it composes a chat panel, one embedded editor, one review
//! surface, and one two-line sidebar through one shared registry.

use std::fmt;
use std::num::NonZeroU64;
use std::time::Duration;

use ratatui::layout::Rect;
use thiserror::Error;

use kvim_keymap::{
    CommandMetadata, CommandOwner, ContextGeneration, Dispatch, DispatchContext, Input,
    InputContextSnapshot, Resolver, Scope, SemanticPhases, TypedText, WhichKeyView,
};

use crate::layout::RegionKind;
use crate::window::{
    ChildSide, CloseOutcome, Direction, IdentityError, LayoutChange, LayoutFit, Orientation,
    RegionError, SidebarSide, SplitError, WINDOWS_MAX, WindowId, WindowLimits, WindowTree,
};

/// The largest number of surface identities that one composer addresses.
///
/// The window tree holds [`WINDOWS_MAX`] leaves, and one sidebar sits at each
/// edge, so this bound covers every region that can publish a context.
pub const COMPOSED_SURFACES_MAX: usize = WINDOWS_MAX + 2;

/// The identity of one proposed focus or overlay transition.
///
/// The composer issues a new identity for every proposal, so a resume that
/// names an earlier proposal is refused instead of committing a transition that
/// the host abandoned.
///
/// ```
/// use kvim_ui::TransitionId;
///
/// // The value is opaque. A host keeps it and hands it back to the composer.
/// fn keep(transition: TransitionId) -> TransitionId {
///     transition
/// }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransitionId(NonZeroU64);

impl TransitionId {
    /// Returns the proposal number.
    #[inline]
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for TransitionId {
    /// Writes the proposal number.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// What one proposed transition changes after the addressed surface resets.
///
/// The value stays inside the composer. The host holds the opaque
/// [`TransitionId`] alone, so it cannot commit a transition by itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transition<Sid, S> {
    /// Move the input focus to the named region.
    Focus { region: WindowId },
    /// Give the named surface and scope overlay ownership.
    OpenOverlay {
        surface: Sid,
        scope: S,
        area: Rect,
        context: InputContextSnapshot<S>,
    },
    /// Take overlay ownership back.
    CloseOverlay,
}

/// One open overlay and the rectangle that the host asked for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Overlay<Sid, S> {
    surface: Sid,
    scope: S,
    area: Rect,
}

/// One proposal that waits for the reset of its addressed surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingTransition<Sid, S> {
    id: TransitionId,
    surface: Sid,
    generation: ContextGeneration,
    transition: Transition<Sid, S>,
}

/// The answer to one proposed focus or overlay transition.
///
/// ```
/// use kvim_ui::CompositionEffect;
///
/// let effect: CompositionEffect<&str> = CompositionEffect::Applied;
/// assert_eq!(effect, CompositionEffect::Applied);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionEffect<Sid> {
    /// The proposal named the state that the composer already holds.
    Unchanged,
    /// No surface held semantic state, so the composer committed at once.
    Applied,
    /// The surface that owns input holds semantic state. The composer keeps
    /// focus and overlay ownership unchanged and asks the host to reset that
    /// surface and to resume the named proposal.
    CancelPending {
        /// The surface that must reset its count, operator, register, text
        /// object, and prompt phases.
        surface: Sid,
        /// The proposal that [`WorkspaceComposer::resume_transition`] takes.
        transition: TransitionId,
    },
}

/// Why the composer refused one resume.
///
/// Every refusal leaves focus, overlay ownership, and the waiting proposal
/// unchanged, so a wrong answer can never commit a transition.
///
/// ```
/// use kvim_ui::ResumeError;
///
/// assert_eq!(ResumeError::Idle.to_string(), "no proposed transition waits for a reset");
/// ```
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ResumeError {
    /// No proposal waits for a reset.
    #[error("no proposed transition waits for a reset")]
    Idle,
    /// Another proposal waits, so the named one is stale.
    #[error("the composer waits for the transition {waiting}, not for the named one")]
    Stale {
        /// The proposal that the composer waits for.
        waiting: TransitionId,
    },
    /// The snapshot came from a surface that the proposal does not address.
    #[error("the reset snapshot came from a surface that the proposal does not address")]
    WrongSurface,
    /// The snapshot carries the generation of the proposal, so the surface
    /// published no new context and performed no reset.
    #[error("the reset snapshot still carries the generation {generation} of the proposal")]
    UnchangedGeneration {
        /// The generation that the composer recorded when it proposed.
        generation: ContextGeneration,
    },
    /// The snapshot is newer, but one grammar phase still waits for input.
    #[error("the reset snapshot still holds a pending phase: {phases:?}")]
    StillPending {
        /// The phases that the surface published.
        phases: SemanticPhases,
    },
}

/// The composer addresses no surface with the supplied identity.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("the composer shows no surface with this identity")]
pub struct UnknownSurface;

/// The typed outcome of one composed input.
///
/// Exactly one variant answers one input, so no caller reads two owners for one
/// key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Composition<C, Sid> {
    /// The host executes the command itself.
    Host {
        /// The command that the sequence reached.
        command: C,
    },
    /// The named surface executes the command.
    Surface {
        /// The surface that owns input.
        surface: Sid,
        /// The command that the sequence reached.
        command: C,
    },
    /// One owner takes the input as literal text.
    Text {
        /// The surface that owns input.
        surface: Sid,
        /// The side that takes the text.
        owner: CommandOwner,
        /// The text that the input carried.
        text: TypedText,
    },
    /// The sequence is a valid prefix of at least one longer binding.
    Pending,
    /// The terminal reported input that no binding accepts.
    Unsupported {
        /// The surface that owns input.
        surface: Sid,
    },
    /// No binding and no text fallback took the input.
    Unbound {
        /// The surface that owns input.
        surface: Sid,
    },
}

/// One visible region, the surface that it shows, and its rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfacePlacement<Sid> {
    /// The host surface that the region shows.
    pub surface: Sid,
    /// The stable identity of the window or the sidebar.
    pub region: WindowId,
    /// The purpose of the region.
    pub kind: RegionKind,
    /// The rectangle that the region occupies.
    pub area: Rect,
}

/// The open overlay and its clipped rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlayPlacement<Sid, S> {
    /// The host surface that draws the overlay.
    pub surface: Sid,
    /// The scope that answers input first while the overlay is open.
    pub scope: S,
    /// The requested rectangle, clipped to the composed area.
    pub area: Rect,
}

/// The clipped placement of every visible surface and of the open overlay.
///
/// The composer draws nothing. The host renders each placement itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositionLayout<Sid, S> {
    surfaces: Vec<SurfacePlacement<Sid>>,
    overlay: Option<OverlayPlacement<Sid, S>>,
    fit: LayoutFit,
}

impl<Sid, S> CompositionLayout<Sid, S> {
    /// Returns every visible surface placement in layout order.
    ///
    /// The order is the left sidebar, then the windows in tree order, then the
    /// right sidebar.
    #[must_use]
    pub fn surfaces(&self) -> &[SurfacePlacement<Sid>] {
        &self.surfaces
    }

    /// Returns the placement of the open overlay.
    #[must_use]
    pub const fn overlay(&self) -> Option<&OverlayPlacement<Sid, S>> {
        self.overlay.as_ref()
    }

    /// Reports how much of the workspace this layout shows.
    #[must_use]
    pub const fn fit(&self) -> LayoutFit {
        self.fit
    }
}

/// One domain-neutral composition model of a complete host-owned workspace.
///
/// The composer holds the split tree, the sidebar regions, the overlay
/// ownership, one shared resolver, and the published context of every surface
/// that it addresses. It holds no surface value, no transcript, no session, no
/// worktree, and no host command.
///
/// # Examples
///
/// ```
/// use std::fmt;
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// use ratatui::layout::Rect;
///
/// use kvim_keymap::{
///     Binding, CommandMetadata, Input, InputContextSnapshot, Key, KeyCode, Registry, Resolver,
///     Scope,
/// };
/// use kvim_ui::{Composition, WindowLimits, WorkspaceComposer};
///
/// #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// enum Command {
///     Send,
/// }
///
/// impl fmt::Display for Command {
///     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
///         formatter.write_str(self.id())
///     }
/// }
///
/// impl CommandMetadata for Command {
///     fn id(&self) -> &str {
///         "send"
///     }
///     fn label(&self) -> &str {
///         "Send the message"
///     }
/// }
///
/// #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// struct Chat;
///
/// impl fmt::Display for Chat {
///     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
///         formatter.write_str("Chat")
///     }
/// }
///
/// impl Scope for Chat {
///     const COUNT: usize = 1;
/// }
///
/// // The host names its own surfaces. The composer copies the identity only.
/// let keys = [Key::plain(KeyCode::Enter)];
/// let registry = Registry::from_bindings(&[Binding::surface(Chat, &keys, Command::Send)], 2)?;
/// let mut composer = WorkspaceComposer::new(
///     "chat",
///     InputContextSnapshot::idle(Chat),
///     Rect::new(0, 0, 80, 24),
///     WindowLimits::default(),
///     Resolver::new(Arc::new(registry), 2, Duration::from_millis(500)),
/// );
///
/// assert_eq!(
///     composer.reduce(Input::Key(keys[0]), Duration::ZERO),
///     Composition::Surface {
///         surface: "chat",
///         command: Command::Send
///     }
/// );
/// assert_eq!(composer.layout().surfaces().len(), 1);
/// # Ok::<(), kvim_keymap::RegistryError<Command, Chat>>(())
/// ```
#[derive(Clone, Debug)]
pub struct WorkspaceComposer<Sid, C, S> {
    tree: WindowTree<Sid>,
    resolver: Resolver<C, S>,
    global: Option<S>,
    /// The surface of the left sidebar and of the right sidebar, in the order
    /// of [`SidebarSide`].
    sidebars: [Option<Sid>; 2],
    contexts: Vec<(Sid, InputContextSnapshot<S>)>,
    overlay: Option<Overlay<Sid, S>>,
    pending: Option<PendingTransition<Sid, S>>,
    next_transition: u64,
}

impl<Sid, C, S> WorkspaceComposer<Sid, C, S>
where
    Sid: Clone + Eq,
    C: CommandMetadata,
    S: Scope,
{
    /// Creates a workspace with one window that shows the named surface.
    ///
    /// The surface enters with the context that it publishes, so every
    /// addressed surface always has one.
    #[must_use]
    pub fn new(
        surface: Sid,
        context: InputContextSnapshot<S>,
        area: Rect,
        limits: WindowLimits,
        resolver: Resolver<C, S>,
    ) -> Self {
        Self {
            tree: WindowTree::new(surface.clone(), area, limits),
            resolver,
            global: None,
            sidebars: [None, None],
            contexts: vec![(surface, context)],
            overlay: None,
            pending: None,
            next_transition: 1,
        }
    }

    /// Returns the split tree, the sidebars, and the cached window layout.
    ///
    /// The reference is read-only. Focus and overlay ownership change through
    /// the transition protocol of this composer alone.
    #[inline]
    #[must_use]
    pub const fn tree(&self) -> &WindowTree<Sid> {
        &self.tree
    }

    /// Returns the shared resolver that answers every input.
    #[inline]
    #[must_use]
    pub const fn resolver(&self) -> &Resolver<C, S> {
        &self.resolver
    }

    /// Returns the composed area.
    #[inline]
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.tree.area()
    }

    /// Recomputes the layout for a new composed area.
    ///
    /// The topology, every surface identity, the focus, and the overlay stay
    /// unchanged. The overlay rectangle follows the new area, because the
    /// layout clips it.
    pub fn set_area(&mut self, area: Rect) -> LayoutFit {
        self.tree.set_area(area)
    }

    /// Returns the host-global scope that answers after the overlay.
    #[inline]
    #[must_use]
    pub const fn global_scope(&self) -> Option<S> {
        self.global
    }

    /// Names the host-global scope, or removes it.
    ///
    /// The scope answers after the overlay and before the focused surface, so
    /// a host-global binding reaches the host from every surface.
    pub const fn set_global_scope(&mut self, scope: Option<S>) {
        self.global = scope;
    }

    /// Returns the region that holds the input focus.
    #[inline]
    #[must_use]
    pub fn focused_region(&self) -> WindowId {
        self.tree.focused_region()
    }

    /// Returns the surface that the focused region shows.
    #[must_use]
    pub fn focused_surface(&self) -> &Sid {
        let region = self.tree.focused_region();
        if let Some(surface) = self.region_surface(region) {
            return surface;
        }
        debug_assert!(
            false,
            "the focused region is one window or one open sidebar"
        );
        self.tree
            .surface(self.tree.focused_window())
            .expect("the focused window is always a leaf of the tree")
    }

    /// Returns the surface that owns input.
    ///
    /// An open overlay owns input while it stays open, so an overlay key never
    /// reaches the focused surface below it. The focused region and the focused
    /// surface stay unchanged.
    #[must_use]
    pub fn input_surface(&self) -> &Sid {
        match &self.overlay {
            Some(overlay) => &overlay.surface,
            None => self.focused_surface(),
        }
    }

    /// Returns the surface that draws the open overlay and its scope.
    #[must_use]
    pub fn overlay_owner(&self) -> Option<(&Sid, S)> {
        self.overlay
            .as_ref()
            .map(|overlay| (&overlay.surface, overlay.scope))
    }

    /// Returns the context that the named surface published.
    #[must_use]
    pub fn context(&self, surface: &Sid) -> Option<InputContextSnapshot<S>> {
        self.contexts
            .iter()
            .find(|(id, _)| id == surface)
            .map(|(_, context)| *context)
    }

    /// Records the context that the named surface published.
    ///
    /// The host calls this after every input that the surface reduced, so the
    /// next resolution reads the current scope, phases, text fallback, and
    /// generation.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownSurface`] when the composer shows no such surface.
    pub fn set_context(
        &mut self,
        surface: &Sid,
        context: InputContextSnapshot<S>,
    ) -> Result<(), UnknownSurface> {
        let slot = self
            .contexts
            .iter_mut()
            .find(|(id, _)| id == surface)
            .ok_or(UnknownSurface)?;
        slot.1 = context;
        Ok(())
    }

    /// Splits the focused window and focuses the new window.
    ///
    /// The new window shows the surface of the source window, so the surface
    /// that owns input does not change and no reset is needed. The host points
    /// the new window at another surface with
    /// [`WorkspaceComposer::replace_surface`].
    ///
    /// # Errors
    ///
    /// Returns every error of [`WindowTree::split`].
    pub fn split(
        &mut self,
        orientation: Orientation,
        new_side: ChildSide,
    ) -> Result<WindowId, SplitError> {
        let window = self.tree.split(orientation, new_side)?;
        self.resolver.clear_pending();
        Ok(window)
    }

    /// Closes the focused region.
    ///
    /// A close needs no reset handshake, so it commits at once and never
    /// returns [`CompositionEffect::CancelPending`]. The surface that would
    /// have to reset is the surface that goes away, so its count, operator,
    /// register, text object, and prompt phases die with the region. An open
    /// overlay keeps input ownership and its own state, because no close
    /// removes an overlay.
    ///
    /// The focused sidebar closes first, exactly as it does in
    /// [`WindowTree::close_focused`]. A hidden sidebar keeps its surface, so
    /// [`WorkspaceComposer::set_sidebar_visible`] shows it again unchanged.
    /// A surface that no region shows any longer leaves the composer with its
    /// context.
    ///
    /// A tree that holds one window reports [`CloseOutcome::LastWindow`] and
    /// changes nothing. The host then decides whether its workspace ends.
    ///
    /// A close ends every waiting proposal, because the topology that the
    /// proposal addressed changed under it. The host proposes again after the
    /// close.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::fmt;
    /// # use std::sync::Arc;
    /// # use std::time::Duration;
    /// # use ratatui::layout::Rect;
    /// # use kvim_keymap::{
    /// #     CommandMetadata, InputContextSnapshot, Registry, Resolver, Scope,
    /// # };
    /// # use kvim_ui::{ChildSide, CloseOutcome, Orientation, WindowLimits, WorkspaceComposer};
    /// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// # struct Send;
    /// # impl fmt::Display for Send {
    /// #     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    /// #         formatter.write_str("send")
    /// #     }
    /// # }
    /// # impl CommandMetadata for Send {
    /// #     fn id(&self) -> &str { "send" }
    /// #     fn label(&self) -> &str { "Send the message" }
    /// # }
    /// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// # struct Chat;
    /// # impl fmt::Display for Chat {
    /// #     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    /// #         formatter.write_str("Chat")
    /// #     }
    /// # }
    /// # impl Scope for Chat {
    /// #     const COUNT: usize = 1;
    /// # }
    /// # let registry: Registry<Send, Chat> = Registry::from_bindings(&[], 2)?;
    /// let mut composer = WorkspaceComposer::new(
    ///     "left",
    ///     InputContextSnapshot::idle(Chat),
    ///     Rect::new(0, 0, 80, 24),
    ///     WindowLimits::default(),
    ///     Resolver::new(Arc::new(registry), 2, Duration::from_millis(500)),
    /// );
    /// let right = composer.split(Orientation::Vertical, ChildSide::Second)?;
    ///
    /// // The focus follows the new window, so the close removes that window.
    /// assert_eq!(composer.close_focused(), CloseOutcome::Closed(right));
    /// assert_eq!(composer.layout().surfaces().len(), 1);
    ///
    /// // One window remains, so the host owns the decision.
    /// assert_eq!(composer.close_focused(), CloseOutcome::LastWindow);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn close_focused(&mut self) -> CloseOutcome {
        let outcome = self.tree.close_focused();
        if outcome == CloseOutcome::LastWindow {
            return outcome;
        }
        // The input owner changed, so the pending key prefix of the shared
        // resolver ends here, exactly as it does for a committed transition.
        self.resolver.clear_pending();
        self.pending = None;
        self.forget_unaddressed();
        outcome
    }

    /// Points the named window at another surface.
    ///
    /// The new surface enters with the context that it publishes. A surface
    /// that no region shows any longer leaves the composer with its context.
    ///
    /// # Errors
    ///
    /// Returns [`RegionError::Unknown`] when the tree holds no such window.
    pub fn replace_surface(
        &mut self,
        window: WindowId,
        surface: Sid,
        context: InputContextSnapshot<S>,
    ) -> Result<Sid, RegionError> {
        let previous = self.tree.replace_surface(window, surface.clone())?;
        self.remember(surface, context);
        self.forget_unaddressed();
        Ok(previous)
    }

    /// Creates or replaces the sidebar at the named edge.
    ///
    /// The sidebar is one more host-owned surface. It holds no place in the
    /// split tree, keeps a fixed width, and publishes its own context.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError`] when the tree cannot issue another region
    /// identity.
    pub fn open_sidebar(
        &mut self,
        side: SidebarSide,
        width_cells: u16,
        surface: Sid,
        context: InputContextSnapshot<S>,
    ) -> Result<WindowId, IdentityError> {
        let region = self.tree.open_sidebar(side, width_cells)?;
        self.sidebars[side_index(side)] = Some(surface.clone());
        self.remember(surface, context);
        self.forget_unaddressed();
        Ok(region)
    }

    /// Shows or hides the sidebar at the named edge.
    ///
    /// Hiding a sidebar that holds the focus returns the focus to the
    /// previously focused window, so no hidden region keeps input.
    pub fn set_sidebar_visible(&mut self, side: SidebarSide, visible: bool) -> LayoutChange {
        let change = self.tree.set_sidebar_visible(side, visible);
        if change == LayoutChange::Changed {
            self.resolver.clear_pending();
        }
        change
    }

    /// Moves one shared edge of the layout by the named number of cells.
    pub fn resize(&mut self, direction: Direction, step_cells: u16) -> LayoutChange {
        self.tree.resize(direction, step_cells)
    }

    /// Proposes the focus of the named region.
    ///
    /// # Errors
    ///
    /// Returns [`RegionError::Hidden`] for a region without a rectangle and
    /// [`RegionError::Unknown`] for an identity that the composer does not hold.
    pub fn focus_region(
        &mut self,
        region: WindowId,
    ) -> Result<CompositionEffect<Sid>, RegionError> {
        if self.tree.layout().region(region).is_none() {
            return Err(if self.region_surface(region).is_some() {
                RegionError::Hidden(region)
            } else {
                RegionError::Unknown(region)
            });
        }
        Ok(self.propose(Transition::Focus { region }))
    }

    /// Proposes the focus of the nearest region on the named side.
    ///
    /// The move compares layout rectangles, not tree order, so the focus
    /// crosses every surface boundary of the workspace. A side without a
    /// neighbor reports [`CompositionEffect::Unchanged`], and the host then
    /// decides what lies beyond the workspace.
    pub fn focus_direction(&mut self, direction: Direction) -> CompositionEffect<Sid> {
        let Some(region) = self
            .tree
            .layout()
            .neighbor(self.focused_region(), direction)
        else {
            return CompositionEffect::Unchanged;
        };
        self.propose(Transition::Focus { region })
    }

    /// Proposes overlay ownership for the named surface and scope.
    ///
    /// The overlay surface enters with the context that it publishes, because
    /// it owns input while it stays open. The rectangle is the one that the
    /// host asks for, and the layout clips it to the composed area.
    pub fn open_overlay(
        &mut self,
        surface: Sid,
        scope: S,
        area: Rect,
        context: InputContextSnapshot<S>,
    ) -> CompositionEffect<Sid> {
        self.propose(Transition::OpenOverlay {
            surface,
            scope,
            area,
            context,
        })
    }

    /// Proposes the end of overlay ownership.
    ///
    /// The surface that drew the overlay leaves the composer with its context
    /// when no region shows it.
    pub fn close_overlay(&mut self) -> CompositionEffect<Sid> {
        self.propose(Transition::CloseOverlay)
    }

    /// Returns the proposal that waits for the reset of its surface.
    #[inline]
    #[must_use]
    pub fn pending_transition(&self) -> Option<TransitionId> {
        self.pending.as_ref().map(|pending| pending.id)
    }

    /// Commits one proposed transition after the addressed surface reset.
    ///
    /// The call validates the proposal identity, the surface identity, and the
    /// snapshot generation, and it requires empty count, operator, register,
    /// text-object, and prompt phases. Only then does focus or overlay
    /// ownership change.
    ///
    /// A commit reports [`LayoutChange::Unchanged`] when the host area shrank
    /// between the proposal and the commit and the target region lost its
    /// rectangle. The focus then stays where it is.
    ///
    /// # Errors
    ///
    /// Returns [`ResumeError`] for every failed check. A refusal leaves focus,
    /// overlay ownership, and the waiting proposal unchanged.
    pub fn resume_transition(
        &mut self,
        transition: TransitionId,
        surface: &Sid,
        context: InputContextSnapshot<S>,
    ) -> Result<LayoutChange, ResumeError> {
        let waiting = self.pending.as_ref().ok_or(ResumeError::Idle)?;
        if waiting.id != transition {
            return Err(ResumeError::Stale {
                waiting: waiting.id,
            });
        }
        if waiting.surface != *surface {
            return Err(ResumeError::WrongSurface);
        }
        if context.generation == waiting.generation {
            return Err(ResumeError::UnchangedGeneration {
                generation: waiting.generation,
            });
        }
        if !context.phases.is_idle() {
            return Err(ResumeError::StillPending {
                phases: context.phases,
            });
        }
        let committed = self
            .pending
            .take()
            .expect("the borrow above proved that one proposal waits");
        self.remember(surface.clone(), context);
        Ok(self.commit(committed.transition))
    }

    /// Arms the which-key overlay for pending input that the resolver does not
    /// own.
    ///
    /// A surface can hold its own grammar prefix, such as a decimal count. The
    /// host arms the overlay when that prefix opens, so the delay counts from
    /// the first pending input.
    pub fn arm_which_key(&mut self, now: Duration) {
        self.resolver.arm_overlay(now);
    }

    /// Returns the elapsed time at which the which-key overlay appears.
    #[must_use]
    pub fn which_key_deadline(&self) -> Option<Duration> {
        self.resolver.overlay_deadline()
    }

    /// Returns the which-key hints of the pending prefix, or `None` while the
    /// overlay stays hidden.
    pub fn which_key(&mut self, now: Duration) -> Option<WhichKeyView<'_, C, S>> {
        self.resolver.which_key(now)
    }

    /// Resolves one input against the surface that owns it.
    ///
    /// The overlay scope answers first, the host-global scope answers next, and
    /// the scope of the input-owning surface answers last.
    pub fn reduce(&mut self, input: Input, now: Duration) -> Composition<C, Sid> {
        let surface = self.input_surface().clone();
        let focus = self.published_context(&surface);
        let context = DispatchContext {
            overlay: self.overlay.as_ref().map(|overlay| overlay.scope),
            global: self.global,
            focus,
        };
        match self.resolver.dispatch(&context, input, Some(now)) {
            Dispatch::Host { command } => Composition::Host { command },
            Dispatch::Surface { command } => Composition::Surface { surface, command },
            Dispatch::Text { owner, text } => Composition::Text {
                surface,
                owner,
                text,
            },
            Dispatch::Pending => Composition::Pending,
            Dispatch::Unsupported => Composition::Unsupported { surface },
            Dispatch::Unbound => Composition::Unbound { surface },
        }
    }

    /// Returns the clipped placement of every visible surface and overlay.
    ///
    /// The composer draws nothing and invokes no host callback. The host
    /// renders each placement inside its own rectangle.
    #[must_use]
    pub fn layout(&self) -> CompositionLayout<Sid, S> {
        let layout = self.tree.layout();
        let mut surfaces = Vec::with_capacity(layout.regions().len());
        for region in layout.regions() {
            let Some(surface) = self.region_surface(region.id) else {
                debug_assert!(false, "every visible region shows one host surface");
                continue;
            };
            surfaces.push(SurfacePlacement {
                surface: surface.clone(),
                region: region.id,
                kind: region.kind,
                area: region.area,
            });
        }
        let overlay = self.overlay.as_ref().map(|overlay| OverlayPlacement {
            surface: overlay.surface.clone(),
            scope: overlay.scope,
            area: overlay.area.intersection(self.tree.area()),
        });
        CompositionLayout {
            surfaces,
            overlay,
            fit: layout.fit(),
        }
    }

    /// Proposes one transition and reports the effect that the host applies.
    fn propose(&mut self, transition: Transition<Sid, S>) -> CompositionEffect<Sid> {
        if self.is_current(&transition) {
            return CompositionEffect::Unchanged;
        }
        let surface = self.input_surface().clone();
        let context = self.published_context(&surface);
        if context.phases.is_idle() {
            self.pending = None;
            let _change = self.commit(transition);
            return CompositionEffect::Applied;
        }
        let id = self.issue_transition_id();
        self.pending = Some(PendingTransition {
            id,
            surface: surface.clone(),
            generation: context.generation,
            transition,
        });
        CompositionEffect::CancelPending {
            surface,
            transition: id,
        }
    }

    /// Reports whether the workspace already holds the proposed state.
    fn is_current(&self, transition: &Transition<Sid, S>) -> bool {
        match transition {
            Transition::Focus { region } => self.tree.focused_region() == *region,
            Transition::OpenOverlay {
                surface,
                scope,
                area,
                ..
            } => self.overlay.as_ref().is_some_and(|overlay| {
                overlay.surface == *surface && overlay.scope == *scope && overlay.area == *area
            }),
            Transition::CloseOverlay => self.overlay.is_none(),
        }
    }

    /// Applies one validated transition.
    ///
    /// Every transition changes the owner of input, so the pending key prefix
    /// of the shared resolver ends here as well.
    fn commit(&mut self, transition: Transition<Sid, S>) -> LayoutChange {
        self.resolver.clear_pending();
        match transition {
            // A host area that shrank between the proposal and the commit can
            // hide the target region. The focus then stays where it is, which
            // is the same answer that the layout gives every other caller.
            Transition::Focus { region } => self
                .tree
                .focus_region(region)
                .unwrap_or(LayoutChange::Unchanged),
            Transition::OpenOverlay {
                surface,
                scope,
                area,
                context,
            } => {
                self.remember(surface.clone(), context);
                self.overlay = Some(Overlay {
                    surface,
                    scope,
                    area,
                });
                LayoutChange::Changed
            }
            Transition::CloseOverlay => {
                self.overlay = None;
                self.forget_unaddressed();
                LayoutChange::Changed
            }
        }
    }

    /// Returns the next proposal identity.
    ///
    /// The counter holds 64 bits and counts the proposals of one workspace, so
    /// the wrap is unreachable on any real terminal.
    fn issue_transition_id(&mut self) -> TransitionId {
        let value = NonZeroU64::new(self.next_transition)
            .expect("the proposal counter starts at one and only grows");
        self.next_transition = self.next_transition.wrapping_add(1).max(1);
        TransitionId(value)
    }

    /// Returns the context that one addressed surface published.
    fn published_context(&self, surface: &Sid) -> InputContextSnapshot<S> {
        self.context(surface)
            .expect("every composed surface enters with its published context")
    }

    /// Records the context of one surface, without duplicating an entry.
    fn remember(&mut self, surface: Sid, context: InputContextSnapshot<S>) {
        if let Some(slot) = self.contexts.iter_mut().find(|(id, _)| *id == surface) {
            slot.1 = context;
            return;
        }
        debug_assert!(
            self.contexts.len() < COMPOSED_SURFACES_MAX,
            "the window tree and the two sidebars bound the addressed surfaces"
        );
        self.contexts.push((surface, context));
    }

    /// Drops the context of every surface that no region shows.
    fn forget_unaddressed(&mut self) {
        let mut addressed: Vec<Sid> = self
            .tree
            .window_ids()
            .into_iter()
            .filter_map(|window| self.tree.surface(window).cloned())
            .collect();
        addressed.extend(self.sidebars.iter().flatten().cloned());
        if let Some(overlay) = self.overlay.as_ref() {
            addressed.push(overlay.surface.clone());
        }
        self.contexts
            .retain(|(surface, _)| addressed.contains(surface));
    }

    /// Returns the surface that one region shows.
    fn region_surface(&self, region: WindowId) -> Option<&Sid> {
        if let Some(surface) = self.tree.surface(region) {
            return Some(surface);
        }
        for side in [SidebarSide::Left, SidebarSide::Right] {
            if self
                .tree
                .sidebar(side)
                .is_some_and(|sidebar| sidebar.id() == region)
            {
                return self.sidebars[side_index(side)].as_ref();
            }
        }
        None
    }
}

/// Returns the slot of one sidebar edge.
const fn side_index(side: SidebarSide) -> usize {
    match side {
        SidebarSide::Left => 0,
        SidebarSide::Right => 1,
    }
}
