//! The one shared key resolver of a composed interface.
//!
//! The resolver owns exactly one pending key sequence. It reads the registry
//! for dispatch and for which-key hints, so no second binding table exists. It
//! reads no clock and holds no surface state: the caller supplies the context,
//! the input, and the elapsed time as values.
//!
//! Scope order is overlay, host-global, and focused surface. The first scope
//! that completes the sequence wins. When no scope completes it, the first
//! scope that can extend it owns the pending prefix, and the rest of the
//! sequence stays in that scope.

use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use crate::binding::CommandOwner;
use crate::context::{ContextGeneration, InputContextSnapshot};
use crate::hint::WhichKeyHint;
use crate::key::Key;
use crate::registry::Registry;
use crate::{BoundCommand, CommandMetadata, KeySequence, Scope};

/// The largest paste that one resolution accepts.
///
/// A terminal paste arrives as one bounded block. The limit keeps one input
/// event, one edit transaction, and one undo unit finite.
pub const PASTE_BYTES_MAX: usize = 65_536;

/// A rejected paste block.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PasteError {
    /// The block held no text, so it carries no input.
    #[error("a paste block holds no text")]
    Empty,
    /// The block held more bytes than one resolution accepts.
    #[error("a paste block holds {bytes} bytes, but the maximum is {PASTE_BYTES_MAX}")]
    TooLong {
        /// The length of the rejected block in bytes.
        bytes: usize,
    },
}

/// One bounded, non-empty paste block.
///
/// ```
/// use kvim_keymap::{PasteError, PasteText};
///
/// assert_eq!(PasteText::new("fn main")?.as_str(), "fn main");
/// assert_eq!(PasteText::new(""), Err(PasteError::Empty));
/// # Ok::<(), PasteError>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PasteText(String);

impl PasteText {
    /// Builds a paste block and checks both bounds.
    ///
    /// # Errors
    ///
    /// Returns [`PasteError::Empty`] for empty text and [`PasteError::TooLong`]
    /// for text above [`PASTE_BYTES_MAX`].
    pub fn new(text: &str) -> Result<Self, PasteError> {
        if text.is_empty() {
            return Err(PasteError::Empty);
        }
        if text.len() > PASTE_BYTES_MAX {
            return Err(PasteError::TooLong { bytes: text.len() });
        }
        Ok(Self(text.to_owned()))
    }

    /// Returns the pasted text.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Literal text that one owner takes.
///
/// A key types one character, and a paste types one bounded block. The two stay
/// distinct, because a paste is one edit transaction and one undo unit.
///
/// ```
/// use kvim_keymap::TypedText;
///
/// assert_eq!(TypedText::Typed('a').as_str(), None);
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TypedText {
    /// One key typed one character.
    Typed(char),
    /// One paste block carried the text.
    Pasted(PasteText),
}

impl TypedText {
    /// Returns the pasted text, or `None` for a typed character.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Typed(_) => None,
            Self::Pasted(text) => Some(text.as_str()),
        }
    }
}

/// One bounded input that the resolver accepts.
///
/// The terminal adapter rejects a key that carries an unsupported modifier. The
/// host reports that rejection as [`Input::Unsupported`], so an unsupported
/// chord never degrades into the binding of the unmodified key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Input {
    /// One normalized key press.
    Key(Key),
    /// One bounded paste block.
    Paste(PasteText),
    /// The terminal reported input that no binding accepts.
    Unsupported,
}

/// The scopes and the surface context of one resolution request.
///
/// The host builds the value for every request, so the resolver stores no
/// scope, no focus, and no surface state of its own.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DispatchContext<S> {
    /// The scope of the open overlay, which answers first.
    pub overlay: Option<S>,
    /// The host-global scope, which answers after the overlay.
    pub global: Option<S>,
    /// The published context of the focused surface, which answers last.
    pub focus: InputContextSnapshot<S>,
}

impl<S> DispatchContext<S> {
    /// Builds a context with no overlay and no host-global scope.
    #[inline]
    #[must_use]
    pub const fn focused(focus: InputContextSnapshot<S>) -> Self {
        Self {
            overlay: None,
            global: None,
            focus,
        }
    }
}

/// The outcome of one resolution request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Dispatch<C> {
    /// The host executes the command.
    Host {
        /// The command that the sequence reached.
        command: C,
    },
    /// The focused surface executes the command.
    Surface {
        /// The command that the sequence reached.
        command: C,
    },
    /// One owner takes the input as literal text.
    Text {
        /// The side that takes the text.
        owner: CommandOwner,
        /// The text that the input carried.
        text: TypedText,
    },
    /// The sequence is a valid prefix of at least one longer binding.
    Pending,
    /// The terminal reported input that no binding accepts.
    Unsupported,
    /// No binding and no text fallback took the input.
    Unbound,
}

/// The which-key overlay state of the resolver.
///
/// The delay governs the first appearance only. The overlay then stays visible
/// while the pending input continues, so a deeper level updates its rows
/// without hiding them again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlayState {
    /// No pending input has armed the overlay.
    Hidden,
    /// The overlay appears at this elapsed time.
    Delayed { at: Duration },
    /// The overlay is visible and stays visible while input is pending.
    Visible,
}

/// The identity of one published context.
///
/// The resolver arms its pending prefix under one identity. A different
/// identity means a focus change, an overlay change, or a context-state change,
/// and every one of them clears the prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ContextIdentity<S> {
    overlay: Option<S>,
    global: Option<S>,
    focus: S,
    generation: ContextGeneration,
}

impl<S: Copy> ContextIdentity<S> {
    /// Reads the identity of one request context.
    fn of(context: &DispatchContext<S>) -> Self {
        Self {
            overlay: context.overlay,
            global: context.global,
            focus: context.focus.scope,
            generation: context.focus.generation,
        }
    }
}

/// The pending key prefix of the resolver.
///
/// The active variant ties the prefix, its owning scope, and the identity that
/// armed it together, so a prefix without an owner cannot exist.
#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingPrefix<S> {
    /// No key waits for completion.
    Idle,
    /// A prefix waits for the rest of its sequence.
    Active {
        identity: ContextIdentity<S>,
        scope: S,
        keys: Vec<Key>,
    },
}

/// The which-key hints of the active pending prefix.
///
/// The view borrows the same registry and the same prefix that dispatch uses,
/// so a hint can never disagree with the command that the next key reaches.
#[derive(Clone, Copy, Debug)]
pub struct WhichKeyView<'a, C, S> {
    registry: &'a Registry<C, S>,
    scope: S,
    prefix: &'a [Key],
}

impl<C, S> WhichKeyView<'_, C, S>
where
    C: CommandMetadata,
    S: Scope,
{
    /// Returns the scope that owns the pending prefix.
    #[inline]
    #[must_use]
    pub const fn scope(&self) -> S {
        self.scope
    }

    /// Returns the pending prefix.
    #[inline]
    #[must_use]
    pub const fn prefix(&self) -> &[Key] {
        self.prefix
    }

    /// Returns every binding that extends the pending prefix, in registry
    /// order.
    pub fn extensions(&self) -> impl Iterator<Item = (&KeySequence, BoundCommand<C>)> {
        self.registry.extensions_of_prefix(self.scope, self.prefix)
    }

    /// Returns one hint for each distinct next key of the pending prefix.
    ///
    /// A presentation layer reads these hints alone. It needs no binding table
    /// of its own, because the hints come from the registry that dispatch
    /// reads. `crates/kvim-ui/examples/which_key.rs` renders them.
    #[must_use]
    pub fn hints(&self) -> Vec<WhichKeyHint<C>> {
        self.registry.hints_for_prefix(self.scope, self.prefix)
    }
}

/// The one shared key resolver of a composed interface.
///
/// The resolver holds one immutable registry snapshot, one pending key prefix,
/// and one which-key overlay state. It reads no clock: the caller measures the
/// elapsed time and supplies it.
///
/// ```
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// use std::fmt;
///
/// use kvim_keymap::{
///     Binding, CommandMetadata, Dispatch, DispatchContext, Input, InputContextSnapshot, Key,
///     KeyCode, Registry, Resolver, Scope,
/// };
///
/// #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// enum Action {
///     First,
/// }
///
/// impl fmt::Display for Action {
///     fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
///         formatter.write_str(self.id())
///     }
/// }
///
/// impl CommandMetadata for Action {
///     fn id(&self) -> &str {
///         "first"
///     }
///     fn label(&self) -> &str {
///         "Go to the first line"
///     }
/// }
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
/// let keys = [Key::plain(KeyCode::Char('g')); 2];
/// let registry = Registry::from_bindings(&[Binding::surface(Editor, &keys, Action::First)], 4)?;
/// let mut resolver = Resolver::new(Arc::new(registry), 4, Duration::from_millis(500));
/// let context = DispatchContext::focused(InputContextSnapshot::idle(Editor));
/// let now = Duration::ZERO;
///
/// assert_eq!(
///     resolver.dispatch(&context, Input::Key(keys[0]), now),
///     Dispatch::Pending
/// );
/// assert_eq!(
///     resolver.dispatch(&context, Input::Key(keys[1]), now),
///     Dispatch::Surface {
///         command: Action::First
///     }
/// );
/// # Ok::<(), kvim_keymap::RegistryError<Action, Editor>>(())
/// ```
#[derive(Clone, Debug)]
pub struct Resolver<C, S> {
    registry: Arc<Registry<C, S>>,
    keys_max: u8,
    which_key_delay: Duration,
    pending: PendingPrefix<S>,
    overlay: OverlayState,
}

impl<C, S> Resolver<C, S>
where
    C: CommandMetadata,
    S: Scope,
{
    /// Creates a resolver over one shared registry snapshot.
    ///
    /// `keys_max` is the pending-sequence limit that the registry validated.
    /// `which_key_delay` is the wait before the overlay first appears.
    #[must_use]
    pub fn new(registry: Arc<Registry<C, S>>, keys_max: u8, which_key_delay: Duration) -> Self {
        debug_assert!(keys_max > 0, "a registry rejects a zero pending-key limit");
        Self {
            registry,
            keys_max,
            which_key_delay,
            pending: PendingPrefix::Idle,
            overlay: OverlayState::Hidden,
        }
    }

    /// Returns the shared registry snapshot.
    ///
    /// Dispatch, conflict reports, help, and which-key read this one table.
    #[inline]
    #[must_use]
    pub fn registry(&self) -> &Registry<C, S> {
        &self.registry
    }

    /// Returns the keys of the pending prefix.
    #[must_use]
    pub fn pending_keys(&self) -> &[Key] {
        match &self.pending {
            PendingPrefix::Idle => &[],
            PendingPrefix::Active { keys, .. } => keys,
        }
    }

    /// Clears the pending prefix and hides the which-key overlay.
    ///
    /// A focus change, an overlay change, and an applied cancellation effect
    /// all reach this function. It changes no surface state.
    pub fn clear_pending(&mut self) {
        self.pending = PendingPrefix::Idle;
        self.overlay = OverlayState::Hidden;
    }

    /// Arms the which-key overlay for pending input that the resolver does not
    /// own.
    ///
    /// A surface can hold its own grammar prefix, such as a decimal count. The
    /// host arms the overlay when that prefix opens, so the delay counts from
    /// the first pending input and not from the first key of a sequence.
    pub fn arm_overlay(&mut self, now: Duration) {
        if matches!(self.overlay, OverlayState::Hidden) {
            self.overlay = OverlayState::Delayed {
                at: now
                    .checked_add(self.which_key_delay)
                    .unwrap_or(Duration::MAX),
            };
        }
    }

    /// Returns the elapsed time at which the which-key overlay appears.
    ///
    /// The host wakes exactly at that time. A visible overlay and an empty
    /// prefix both report no time, because no transition could consume it.
    #[must_use]
    pub fn overlay_deadline(&self) -> Option<Duration> {
        if self.pending_keys().is_empty() {
            return None;
        }
        match self.overlay {
            OverlayState::Delayed { at } => Some(at),
            OverlayState::Hidden | OverlayState::Visible => None,
        }
    }

    /// Returns the which-key hints of the pending prefix, or `None` while the
    /// overlay stays hidden.
    ///
    /// The call records the first appearance, so every further key of the same
    /// sequence updates the hints at once, without a second wait.
    pub fn which_key(&mut self, now: Duration) -> Option<WhichKeyView<'_, C, S>> {
        if self.pending_keys().is_empty() {
            return None;
        }
        match self.overlay {
            OverlayState::Hidden => return None,
            OverlayState::Delayed { at } if now < at => return None,
            OverlayState::Delayed { .. } | OverlayState::Visible => {
                self.overlay = OverlayState::Visible;
            }
        }
        let PendingPrefix::Active { scope, keys, .. } = &self.pending else {
            debug_assert!(false, "an empty prefix leaves the function above");
            return None;
        };
        Some(WhichKeyView {
            registry: &self.registry,
            scope: *scope,
            prefix: keys,
        })
    }

    /// Resolves one input against the supplied context at the elapsed time.
    ///
    /// The function evaluates the overlay scope, the host-global scope, and the
    /// focused scope in that order. A pending prefix keeps the scope that armed
    /// it, so a sequence never changes owner in the middle.
    pub fn dispatch(
        &mut self,
        context: &DispatchContext<S>,
        input: Input,
        now: Duration,
    ) -> Dispatch<C> {
        let identity = ContextIdentity::of(context);
        if let PendingPrefix::Active {
            identity: armed, ..
        } = &self.pending
            && *armed != identity
        {
            self.clear_pending();
        }
        match input {
            Input::Unsupported => {
                self.clear_pending();
                Dispatch::Unsupported
            }
            Input::Paste(text) => {
                self.clear_pending();
                self.typed_text(context, TypedText::Pasted(text))
            }
            Input::Key(key) => self.dispatch_key(context, identity, key, now),
        }
    }

    /// Resolves one key against the supplied context.
    fn dispatch_key(
        &mut self,
        context: &DispatchContext<S>,
        identity: ContextIdentity<S>,
        key: Key,
        now: Duration,
    ) -> Dispatch<C> {
        match std::mem::replace(&mut self.pending, PendingPrefix::Idle) {
            PendingPrefix::Active {
                scope, mut keys, ..
            } => {
                debug_assert!(
                    keys.len() < usize::from(self.keys_max),
                    "the registry rejects a binding above the pending-key maximum, so a prefix keeps room for one key"
                );
                keys.push(key);
                self.continue_prefix(identity, scope, keys, now)
            }
            PendingPrefix::Idle => self.start_sequence(context, identity, key, now),
        }
    }

    /// Resolves one key that starts a sequence.
    fn start_sequence(
        &mut self,
        context: &DispatchContext<S>,
        identity: ContextIdentity<S>,
        key: Key,
        now: Duration,
    ) -> Dispatch<C> {
        let keys = [key];
        for scope in scope_order(context) {
            if let Some(bound) = self.registry.bound_command(scope, &keys) {
                self.overlay = OverlayState::Hidden;
                return dispatch_of(bound);
            }
        }
        for scope in scope_order(context) {
            if self.registry.has_longer_sequence(scope, &keys) {
                self.arm_overlay(now);
                self.pending = PendingPrefix::Active {
                    identity,
                    scope,
                    keys: keys.to_vec(),
                };
                return Dispatch::Pending;
            }
        }
        self.overlay = OverlayState::Hidden;
        // A text fallback takes the first key of a sequence only. A key that
        // breaks a started sequence types nothing.
        match key.typed_char() {
            Some(value) => self.typed_text(context, TypedText::Typed(value)),
            None => Dispatch::Unbound,
        }
    }

    /// Resolves one key that continues the pending prefix.
    ///
    /// The scope that armed the prefix owns every further key, so the which-key
    /// hints and the reached command always come from one table.
    fn continue_prefix(
        &mut self,
        identity: ContextIdentity<S>,
        scope: S,
        keys: Vec<Key>,
        now: Duration,
    ) -> Dispatch<C> {
        if let Some(bound) = self.registry.bound_command(scope, &keys) {
            self.overlay = OverlayState::Hidden;
            return dispatch_of(bound);
        }
        if self.registry.has_longer_sequence(scope, &keys) {
            self.arm_overlay(now);
            self.pending = PendingPrefix::Active {
                identity,
                scope,
                keys,
            };
            return Dispatch::Pending;
        }
        self.overlay = OverlayState::Hidden;
        Dispatch::Unbound
    }

    /// Routes literal text to the text-fallback owner of the focused scope.
    fn typed_text(&self, context: &DispatchContext<S>, text: TypedText) -> Dispatch<C> {
        match context.focus.text_fallback.owner() {
            Some(owner) => Dispatch::Text { owner, text },
            None => Dispatch::Unbound,
        }
    }
}

/// Returns the scopes of one context in evaluation order, without repetition.
fn scope_order<S: Scope>(context: &DispatchContext<S>) -> impl Iterator<Item = S> {
    let ordered = [context.overlay, context.global, Some(context.focus.scope)];
    ordered
        .into_iter()
        .enumerate()
        .filter_map(move |(index, scope)| {
            let scope = scope?;
            // An earlier scope already answered for this table, so a repeated
            // scope would search it twice.
            ordered[..index]
                .iter()
                .all(|earlier| *earlier != Some(scope))
                .then_some(scope)
        })
}

/// Returns the dispatch outcome of one bound command.
fn dispatch_of<C>(bound: BoundCommand<C>) -> Dispatch<C> {
    match bound.owner {
        CommandOwner::Host => Dispatch::Host {
            command: bound.command,
        },
        CommandOwner::Surface => Dispatch::Surface {
            command: bound.command,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{
        Dispatch, DispatchContext, Input, PasteError, PasteText, Resolver, TypedText, scope_order,
    };
    use crate::binding::{Binding, CommandMetadata, CommandOwner, Scope};
    use crate::context::{ContextGeneration, InputContextSnapshot, TextFallback};
    use crate::key::{Key, KeyCode};
    use crate::registry::Registry;

    const NOW: Duration = Duration::ZERO;
    const DELAY: Duration = Duration::from_millis(500);

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    enum Action {
        Quit,
        FirstLine,
        Down,
        Close,
        PickNext,
    }

    impl fmt::Display for Action {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.id())
        }
    }

    impl CommandMetadata for Action {
        fn id(&self) -> &str {
            match self {
                Self::Quit => "quit",
                Self::FirstLine => "first-line",
                Self::Down => "down",
                Self::Close => "close",
                Self::PickNext => "pick-next",
            }
        }

        fn label(&self) -> &str {
            match self {
                Self::Quit => "Quit",
                Self::FirstLine => "First line",
                Self::Down => "Down",
                Self::Close => "Close",
                Self::PickNext => "Next result",
            }
        }
    }

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    enum Table {
        Normal,
        Insert,
        Global,
        Overlay,
    }

    impl fmt::Display for Table {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(match self {
                Self::Normal => "Normal",
                Self::Insert => "Insert",
                Self::Global => "Global",
                Self::Overlay => "Overlay",
            })
        }
    }

    impl Scope for Table {
        const COUNT: usize = 4;
    }

    fn ch(value: char) -> Key {
        Key::plain(KeyCode::Char(value))
    }

    fn resolver() -> Resolver<Action, Table> {
        let bindings = vec![
            Binding::surface(Table::Normal, &[ch('g'), ch('g')], Action::FirstLine),
            Binding::surface(Table::Normal, &[ch('j')], Action::Down),
            Binding::host(
                Table::Global,
                &[Key::ctrl(KeyCode::Char('q'))],
                Action::Quit,
            ),
            Binding::host(Table::Global, &[ch('j')], Action::Close),
            Binding::surface(Table::Overlay, &[ch('j')], Action::PickNext),
            Binding::surface(Table::Overlay, &[ch('g')], Action::Close),
        ];
        let registry = Registry::from_bindings(&bindings, 4).expect("the test table validates");
        Resolver::new(Arc::new(registry), 4, DELAY)
    }

    fn normal() -> DispatchContext<Table> {
        DispatchContext::focused(InputContextSnapshot::idle(Table::Normal))
    }

    fn insert() -> DispatchContext<Table> {
        let mut focus = InputContextSnapshot::idle(Table::Insert);
        focus.text_fallback = TextFallback::Typed(CommandOwner::Surface);
        DispatchContext::focused(focus)
    }

    #[test]
    fn the_scope_order_puts_the_overlay_first_and_drops_a_repeated_table() {
        let context = DispatchContext {
            overlay: Some(Table::Overlay),
            global: Some(Table::Normal),
            focus: InputContextSnapshot::idle(Table::Normal),
        };
        assert_eq!(
            scope_order(&context).collect::<Vec<_>>(),
            vec![Table::Overlay, Table::Normal]
        );
    }

    #[test]
    fn an_overlay_answers_before_the_host_and_the_focused_surface() {
        let mut resolver = resolver();
        let context = DispatchContext {
            overlay: Some(Table::Overlay),
            global: Some(Table::Global),
            focus: InputContextSnapshot::idle(Table::Normal),
        };
        assert_eq!(
            resolver.dispatch(&context, Input::Key(ch('j')), NOW),
            Dispatch::Surface {
                command: Action::PickNext
            }
        );
    }

    #[test]
    fn the_host_scope_answers_before_the_focused_surface() {
        let mut resolver = resolver();
        let context = DispatchContext {
            overlay: None,
            global: Some(Table::Global),
            focus: InputContextSnapshot::idle(Table::Normal),
        };
        assert_eq!(
            resolver.dispatch(&context, Input::Key(ch('j')), NOW),
            Dispatch::Host {
                command: Action::Close
            },
            "a host binding wins over the focused surface"
        );
        assert_eq!(
            resolver.dispatch(&context, Input::Key(Key::ctrl(KeyCode::Char('q'))), NOW),
            Dispatch::Host {
                command: Action::Quit
            }
        );
    }

    #[test]
    fn the_scope_that_armed_a_prefix_owns_the_rest_of_the_sequence() {
        let mut resolver = resolver();
        // The overlay binds `g` alone, so it answers at once and the focused
        // `g g` sequence never opens.
        let with_overlay = DispatchContext {
            overlay: Some(Table::Overlay),
            global: None,
            focus: InputContextSnapshot::idle(Table::Normal),
        };
        assert_eq!(
            resolver.dispatch(&with_overlay, Input::Key(ch('g')), NOW),
            Dispatch::Surface {
                command: Action::Close
            }
        );

        let context = normal();
        assert_eq!(
            resolver.dispatch(&context, Input::Key(ch('g')), NOW),
            Dispatch::Pending
        );
        assert_eq!(resolver.pending_keys(), [ch('g')]);
        assert_eq!(
            resolver.dispatch(&context, Input::Key(ch('g')), NOW),
            Dispatch::Surface {
                command: Action::FirstLine
            }
        );
        assert!(resolver.pending_keys().is_empty());
    }

    #[test]
    fn a_broken_sequence_types_no_text() {
        let mut resolver = resolver();
        let context = insert();
        // Insert holds no binding, so a printable key types text at once.
        assert_eq!(
            resolver.dispatch(&context, Input::Key(ch('a')), NOW),
            Dispatch::Text {
                owner: CommandOwner::Surface,
                text: TypedText::Typed('a')
            }
        );

        let mut normal_focus = InputContextSnapshot::idle(Table::Normal);
        normal_focus.text_fallback = TextFallback::Typed(CommandOwner::Surface);
        let context = DispatchContext::focused(normal_focus);
        assert_eq!(
            resolver.dispatch(&context, Input::Key(ch('g')), NOW),
            Dispatch::Pending
        );
        assert_eq!(
            resolver.dispatch(&context, Input::Key(ch('a')), NOW),
            Dispatch::Unbound,
            "the second key of a started sequence types nothing"
        );
    }

    #[test]
    fn a_context_change_clears_the_pending_prefix() {
        let mut resolver = resolver();
        let context = normal();
        assert_eq!(
            resolver.dispatch(&context, Input::Key(ch('g')), NOW),
            Dispatch::Pending
        );

        let mut changed = normal();
        changed.focus.generation = ContextGeneration::FIRST.advanced();
        assert_eq!(
            resolver.dispatch(&changed, Input::Key(ch('g')), NOW),
            Dispatch::Pending,
            "the cleared prefix starts the sequence again"
        );
        assert_eq!(resolver.pending_keys(), [ch('g')]);

        let mut focused_elsewhere = changed;
        focused_elsewhere.focus.scope = Table::Insert;
        assert_eq!(
            resolver.dispatch(&focused_elsewhere, Input::Key(ch('g')), NOW),
            Dispatch::Unbound,
            "a focus change clears the prefix and Insert binds no `g`"
        );

        let mut overlay_opened = normal();
        overlay_opened.overlay = Some(Table::Overlay);
        assert_eq!(
            resolver.dispatch(&normal(), Input::Key(ch('g')), NOW),
            Dispatch::Pending
        );
        assert_eq!(
            resolver.dispatch(&overlay_opened, Input::Key(ch('g')), NOW),
            Dispatch::Surface {
                command: Action::Close
            },
            "an opened overlay clears the prefix and answers itself"
        );
    }

    #[test]
    fn unsupported_input_reaches_no_binding_and_clears_the_prefix() {
        let mut resolver = resolver();
        let context = normal();
        assert_eq!(
            resolver.dispatch(&context, Input::Key(ch('g')), NOW),
            Dispatch::Pending
        );
        assert_eq!(
            resolver.dispatch(&context, Input::Unsupported, NOW),
            Dispatch::Unsupported
        );
        assert!(resolver.pending_keys().is_empty());
    }

    #[test]
    fn a_paste_follows_the_text_fallback_of_the_focused_scope() {
        let mut resolver = resolver();
        let block = PasteText::new("two words").expect("the block is bounded");
        assert_eq!(
            resolver.dispatch(&insert(), Input::Paste(block.clone()), NOW),
            Dispatch::Text {
                owner: CommandOwner::Surface,
                text: TypedText::Pasted(block.clone())
            }
        );
        assert_eq!(
            resolver.dispatch(&normal(), Input::Paste(block), NOW),
            Dispatch::Unbound,
            "a scope without a text fallback takes no paste"
        );
    }

    #[test]
    fn a_paste_block_states_both_of_its_bounds() {
        assert_eq!(PasteText::new(""), Err(PasteError::Empty));
        let long = "a".repeat(super::PASTE_BYTES_MAX + 1);
        assert!(matches!(
            PasteText::new(&long),
            Err(PasteError::TooLong { .. })
        ));
        assert_eq!(
            PasteText::new("x").map(|text| text.as_str().len()),
            Ok(1_usize)
        );
    }

    #[test]
    fn the_which_key_view_reads_the_same_registry_and_prefix() {
        let mut resolver = resolver();
        let context = normal();
        assert_eq!(
            resolver.dispatch(&context, Input::Key(ch('g')), NOW),
            Dispatch::Pending
        );
        assert_eq!(resolver.overlay_deadline(), Some(DELAY));
        assert!(
            resolver
                .which_key(DELAY - Duration::from_millis(1))
                .is_none()
        );

        let view = resolver.which_key(DELAY).expect("the delay passed");
        assert_eq!(view.scope(), Table::Normal);
        assert_eq!(view.prefix(), [ch('g')]);
        let reached: Vec<_> = view
            .extensions()
            .map(|(keys, bound)| (keys.to_string(), bound.command))
            .collect();
        assert_eq!(reached, vec![("g g".to_owned(), Action::FirstLine)]);
        assert_eq!(
            resolver.overlay_deadline(),
            None,
            "a visible overlay needs no further wake"
        );
    }

    #[test]
    fn a_surface_prefix_arms_the_overlay_before_the_first_key() {
        let mut resolver = resolver();
        // The surface opened its own count, so the delay starts here.
        resolver.arm_overlay(NOW);
        assert_eq!(
            resolver.overlay_deadline(),
            None,
            "the hints list the keys that follow a sequence, and none is pending"
        );
        assert_eq!(
            resolver.dispatch(&normal(), Input::Key(ch('g')), DELAY),
            Dispatch::Pending
        );
        assert!(
            resolver.which_key(DELAY).is_some(),
            "the armed delay already passed, so the hints appear at once"
        );
    }

    #[test]
    fn a_completed_command_and_a_cleared_prefix_both_hide_the_overlay() {
        let mut resolver = resolver();
        let context = normal();
        resolver.dispatch(&context, Input::Key(ch('g')), NOW);
        assert!(resolver.which_key(DELAY).is_some());
        resolver.dispatch(&context, Input::Key(ch('g')), DELAY);
        assert!(resolver.which_key(DELAY).is_none());

        resolver.dispatch(&context, Input::Key(ch('g')), NOW);
        assert!(resolver.which_key(DELAY).is_some());
        resolver.clear_pending();
        assert!(resolver.which_key(DELAY).is_none());
        assert_eq!(resolver.overlay_deadline(), None);
    }

    #[test]
    fn the_registry_of_the_resolver_is_the_dispatch_table() {
        let resolver = resolver();
        assert_eq!(
            resolver.registry().command(Table::Normal, &[ch('j')]),
            Some(Action::Down)
        );
    }
}
