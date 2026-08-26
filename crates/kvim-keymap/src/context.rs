//! The input context that one focused surface publishes.
//!
//! A surface owns its own semantic grammar: a count, an operator, a register, a
//! text object, and a prompt. The shared resolver owns none of that state. The
//! surface publishes one [`InputContextSnapshot`] after every input, and the
//! host supplies that snapshot with the next resolution request.

use std::fmt;

use crate::binding::CommandOwner;

/// The version of one published input context.
///
/// Every context-state change produces a new generation. The shared resolver
/// compares the generation of two requests, and it clears its pending key
/// prefix when the value changes.
///
/// ```
/// use kvim_keymap::ContextGeneration;
///
/// assert_ne!(ContextGeneration::FIRST, ContextGeneration::FIRST.advanced());
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContextGeneration(u64);

impl ContextGeneration {
    /// The generation of a surface that has published no change yet.
    pub const FIRST: Self = Self(0);

    /// Returns the generation that follows this one.
    ///
    /// The counter holds 64 bits and counts the context changes of one
    /// session, so the wrap is unreachable on any real terminal.
    ///
    /// ```
    /// use kvim_keymap::ContextGeneration;
    ///
    /// let first = ContextGeneration::FIRST;
    /// assert_ne!(first.advanced(), first);
    /// assert_ne!(first.advanced().advanced(), first.advanced());
    /// ```
    #[inline]
    #[must_use]
    pub const fn advanced(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

impl fmt::Display for ContextGeneration {
    /// Writes the generation counter.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Whether one grammar phase of a surface holds state.
///
/// A pending phase means that the surface waits for more input before an
/// operation completes. A composer refuses a focus or overlay transition while
/// any phase is pending.
///
/// ```
/// use kvim_keymap::Phase;
///
/// assert!(Phase::Pending.is_pending());
/// assert!(!Phase::Empty.is_pending());
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Phase {
    /// The phase holds no state.
    #[default]
    Empty,
    /// The phase waits for more input.
    Pending,
}

impl Phase {
    /// Reports whether the phase waits for more input.
    #[inline]
    #[must_use]
    pub const fn is_pending(self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// The grammar phases that one surface reports.
///
/// The composer reads these phases before it moves focus or overlay ownership.
/// It commits the transition only when every phase is empty.
///
/// ```
/// use kvim_keymap::{Phase, SemanticPhases};
///
/// assert!(SemanticPhases::IDLE.is_idle());
/// let counting = SemanticPhases {
///     count: Phase::Pending,
///     ..SemanticPhases::IDLE
/// };
/// assert!(!counting.is_idle());
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticPhases {
    /// The decimal count before an operation.
    pub count: Phase,
    /// The operator that waits for its target.
    pub operator: Phase,
    /// The register that qualifies the next operation.
    pub register: Phase,
    /// The text object that names a range.
    pub text_object: Phase,
    /// The line prompt that reads text.
    pub prompt: Phase,
}

impl SemanticPhases {
    /// Every phase empty.
    pub const IDLE: Self = Self {
        count: Phase::Empty,
        operator: Phase::Empty,
        register: Phase::Empty,
        text_object: Phase::Empty,
        prompt: Phase::Empty,
    };

    /// Reports whether every phase is empty.
    #[inline]
    #[must_use]
    pub const fn is_idle(self) -> bool {
        !self.count.is_pending()
            && !self.operator.is_pending()
            && !self.register.is_pending()
            && !self.text_object.is_pending()
            && !self.prompt.is_pending()
    }
}

/// The owner that takes printable input as literal text.
///
/// An insert scope, a prompt scope, and a register-selection scope each convert
/// printable input into typed text for exactly one owner. Every other scope
/// leaves such input unbound.
///
/// ```
/// use kvim_keymap::{CommandOwner, TextFallback};
///
/// assert_eq!(
///     TextFallback::Typed(CommandOwner::Surface).owner(),
///     Some(CommandOwner::Surface)
/// );
/// assert_eq!(TextFallback::None.owner(), None);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TextFallback {
    /// No owner takes printable input, so the input stays unbound.
    #[default]
    None,
    /// One side takes printable input as literal text.
    Typed(CommandOwner),
}

impl TextFallback {
    /// Returns the owner of the typed text, or `None` when no owner takes it.
    #[inline]
    #[must_use]
    pub const fn owner(self) -> Option<CommandOwner> {
        match self {
            Self::None => None,
            Self::Typed(owner) => Some(owner),
        }
    }
}

/// What one focused scope does with input that nothing takes.
///
/// A scope can hold no binding for the input and no text fallback for it. The
/// scope then declares the outcome. A modal scope that waits for one answer,
/// such as a register selection, ends there. Every other scope keeps its state
/// and leaves the input unbound.
///
/// The declaration sits beside [`TextFallback`] in [`InputContextSnapshot`], so
/// a host states both facts of one scope in one place. The resolver reads the
/// declaration after the whole scope order and after the text fallback, so it
/// changes no precedence: a binding of any scope and a text fallback both still
/// win.
///
/// # Examples
///
/// The overlay scope below waits for one answer and binds no key. It declares
/// that unbound input cancels it, so the resolver names the cancellation and
/// the host closes the overlay.
///
/// ```
/// # use std::fmt;
/// # use std::sync::Arc;
/// # use std::time::Duration;
/// # use kvim_keymap::{
/// #     Binding, CommandMetadata, Dispatch, DispatchContext, Input, InputContextSnapshot, Key,
/// #     KeyCode, Registry, Resolver, Scope, UnboundInput,
/// # };
/// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// # enum Action { Save }
/// # impl fmt::Display for Action {
/// #     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.id()) }
/// # }
/// # impl CommandMetadata for Action {
/// #     fn id(&self) -> &str { "save" }
/// #     fn label(&self) -> &str { "Save the file" }
/// # }
/// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// # enum HostScope { Answer, Editor }
/// # impl fmt::Display for HostScope {
/// #     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
/// #         f.write_str(match self { Self::Answer => "Answer", Self::Editor => "Editor" })
/// #     }
/// # }
/// # impl Scope for HostScope { const COUNT: usize = 2; }
/// let registry = Registry::from_bindings(
///     &[Binding::surface(
///         HostScope::Editor,
///         &[Key::plain(KeyCode::Char('s'))],
///         Action::Save,
///     )],
///     4,
/// )?;
/// let mut resolver = Resolver::new(Arc::new(registry), 4, Duration::from_millis(500));
/// let answer = InputContextSnapshot {
///     unbound_input: UnboundInput::Cancels,
///     ..InputContextSnapshot::idle(HostScope::Answer)
/// };
/// let context = DispatchContext::focused(answer);
///
/// assert_eq!(
///     resolver.dispatch(&context, Input::Key(Key::plain(KeyCode::PageDown)), None),
///     Dispatch::Cancelled,
///     "the answer scope binds no key, so the input ends it"
/// );
///
/// // A scope that keeps the default declaration reports the same input as
/// // unbound and stays open.
/// let editor = DispatchContext::focused(InputContextSnapshot::idle(HostScope::Editor));
/// assert_eq!(
///     resolver.dispatch(&editor, Input::Key(Key::plain(KeyCode::PageDown)), None),
///     Dispatch::Unbound
/// );
/// # Ok::<(), kvim_keymap::RegistryError<Action, HostScope>>(())
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UnboundInput {
    /// The scope keeps its state, and the input stays unbound.
    #[default]
    Ignored,
    /// The input cancels the scope.
    Cancels,
}

/// The input context that one focused surface publishes.
///
/// The surface returns this value after every command, text, paste, unbound,
/// unsupported, or cancellation input. The host supplies it with the next
/// resolution request, so the shared resolver reads context as a value and
/// keeps no surface state.
///
/// ```
/// use std::fmt;
///
/// use kvim_keymap::{ContextGeneration, InputContextSnapshot, Scope, SemanticPhases};
///
/// #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// struct Editor;
///
/// impl fmt::Display for Editor {
///     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
///         formatter.write_str("Editor")
///     }
/// }
///
/// impl Scope for Editor {
///     const COUNT: usize = 1;
/// }
///
/// let snapshot = InputContextSnapshot::idle(Editor);
/// assert_eq!(snapshot.phases, SemanticPhases::IDLE);
/// assert_eq!(snapshot.generation, ContextGeneration::FIRST);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InputContextSnapshot<S> {
    /// The scope that owns the keys of the focused surface.
    pub scope: S,
    /// The grammar phases of the surface.
    pub phases: SemanticPhases,
    /// The owner that takes printable input as literal text.
    pub text_fallback: TextFallback,
    /// What the scope does with input that nothing takes.
    pub unbound_input: UnboundInput,
    /// The version of the published context.
    pub generation: ContextGeneration,
}

impl<S> InputContextSnapshot<S> {
    /// Builds a snapshot of a surface that holds no grammar state.
    #[inline]
    #[must_use]
    pub const fn idle(scope: S) -> Self {
        Self {
            scope,
            phases: SemanticPhases::IDLE,
            text_fallback: TextFallback::None,
            unbound_input: UnboundInput::Ignored,
            generation: ContextGeneration::FIRST,
        }
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
