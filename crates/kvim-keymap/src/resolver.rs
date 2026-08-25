//! The one shared key resolver of a composed interface.
//!
//! The resolver owns exactly one pending key sequence. It reads the registry
//! for dispatch and for which-key hints, so no second binding table exists. It
//! reads no clock and holds no surface state: the caller supplies the context,
//! the input, and the elapsed time as values. A caller that draws no which-key
//! overlay supplies no time at all.
//!
//! Scope order is overlay, host-global, and focused surface. Every key of a
//! sequence walks this order. The first scope that completes it wins. When no
//! scope completes it, the first scope that can extend it owns that one key.
//! The owning scope can change from one key to the next, because the walk
//! runs again for every key. The which-key hints of the pending prefix walk
//! the same order. Every hinted key resolves to some scope's binding.
//! [`Resolver::idle_which_key`] walks the same order with no pending prefix.
//! A host uses it to list a complete one-key binding of another scope, such
//! as a host-global escape, which never extends the focused scope's prefix.
//!
//! `crates/kvim-keymap/examples/dispatch_keys.rs` is the dedicated example of
//! this feature. It composes one registry, dispatches a one-key binding and a
//! two-key sequence, and reads the hints of the pending prefix.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use crate::binding::CommandOwner;
use crate::context::{ContextGeneration, InputContextSnapshot};
use crate::hint::ScopedWhichKeyHint;
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
/// A terminal reports a carriage return as the line separator inside a
/// bracketed paste, so [`PasteText::new`] converts every `\r\n` and every
/// remaining lone `\r` of the supplied text to `\n` before storage. A stored
/// block therefore never holds a carriage return.
///
/// ```
/// use kvim_keymap::{PasteError, PasteText};
///
/// assert_eq!(PasteText::new("fn main")?.as_str(), "fn main");
/// assert_eq!(PasteText::new("one\r\ntwo\rthree")?.as_str(), "one\ntwo\nthree");
/// assert_eq!(PasteText::new(""), Err(PasteError::Empty));
/// # Ok::<(), PasteError>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PasteText(String);

impl PasteText {
    /// Builds a paste block and checks both bounds.
    ///
    /// Converts every `\r\n` and every remaining lone `\r` of `text` to `\n`
    /// before either bound applies, so the stored text is exactly the text
    /// that the editor inserts. A terminal reports a carriage return as the
    /// line separator inside a bracketed paste, and ropey treats a lone `\r`
    /// as a line break on its own, so an unconverted block would render as
    /// separate lines on screen while the buffer held carriage returns; a
    /// save would then write a file that every other tool reads as one long
    /// line.
    ///
    /// # Errors
    ///
    /// Returns [`PasteError::Empty`] for empty text and [`PasteError::TooLong`]
    /// for text above [`PASTE_BYTES_MAX`] after normalization.
    pub fn new(text: &str) -> Result<Self, PasteError> {
        if text.is_empty() {
            return Err(PasteError::Empty);
        }
        let normalized = Self::normalize_line_separators(text);
        if normalized.len() > PASTE_BYTES_MAX {
            return Err(PasteError::TooLong {
                bytes: normalized.len(),
            });
        }
        Ok(Self(normalized.into_owned()))
    }

    /// Returns the pasted text.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Converts every `\r\n` and every remaining lone `\r` of `text` to `\n`.
    ///
    /// Returns the borrowed text unchanged when it holds no carriage return,
    /// because that is the common case on a terminal that already sends line
    /// feeds, and this runs on the terminal event loop.
    fn normalize_line_separators(text: &str) -> Cow<'_, str> {
        if !text.contains('\r') {
            return Cow::Borrowed(text);
        }
        let mut normalized = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(current) = chars.next() {
            if current == '\r' {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                normalized.push('\n');
            } else {
                normalized.push(current);
            }
        }
        Cow::Owned(normalized)
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

impl<S: Scope> ContextIdentity<S> {
    /// Reads the identity of one request context.
    fn of(context: &DispatchContext<S>) -> Self {
        Self {
            overlay: context.overlay,
            global: context.global,
            focus: context.focus.scope,
            generation: context.focus.generation,
        }
    }

    /// Returns the scopes that armed the pending prefix, in evaluation order
    /// and without repetition.
    ///
    /// The identity stores exactly the fields that [`scope_order`] reads from
    /// a live [`DispatchContext`]. This walks the same order that
    /// `start_sequence` walked when it armed the prefix.
    /// [`WhichKeyView::hints`] calls this instead of asking the caller for a
    /// fresh context. A hint can never span a different scope order than the
    /// one that armed its prefix.
    fn scope_order(&self) -> impl Iterator<Item = S> {
        scope_order_of(self.overlay, self.global, self.focus)
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
/// [`WhichKeyView::hints`] spans the scope order of the pending prefix. Every
/// hinted key resolves to some scope's binding, with an earlier scope winning
/// a collision.
#[derive(Clone, Copy, Debug)]
pub struct WhichKeyView<'a, C, S> {
    registry: &'a Registry<C, S>,
    identity: ContextIdentity<S>,
    scope: S,
    prefix: &'a [Key],
}

impl<C, S> WhichKeyView<'_, C, S>
where
    C: CommandMetadata,
    S: Scope,
{
    /// Returns the scope that currently owns the pending prefix.
    ///
    /// A further key can move ownership to a different scope of the
    /// evaluation order. This value can therefore differ from what it
    /// reported one key ago.
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
    /// order, from the scope that currently owns it.
    pub fn extensions(&self) -> impl Iterator<Item = (&KeySequence, BoundCommand<C>)> {
        self.registry.extensions_of_prefix(self.scope, self.prefix)
    }

    /// Returns one hint for each distinct next key of the pending prefix.
    ///
    /// The hints come from every scope of the context that extends the
    /// prefix, in scope order and without repetition.
    ///
    /// A presentation layer reads these hints alone. It needs no binding table
    /// of its own, because the hints come from the registry that dispatch
    /// reads. Every hint names its scope. A host can group or style a
    /// host-global hint apart from a focused-surface hint.
    /// `crates/kvim-ui/examples/which_key.rs` renders them.
    ///
    /// A key that this view hints resolves to some scope's binding when
    /// pressed. Two scopes can hint the same key with different commands.
    /// The earlier scope in the evaluation order wins that collision, exactly
    /// as [`WhichKeyView::scope`] would report after the press.
    #[must_use]
    pub fn hints(&self) -> Vec<ScopedWhichKeyHint<C, S>> {
        let mut hints = Vec::new();
        for scope in self.identity.scope_order() {
            for hint in self.registry.hints_for_prefix(scope, self.prefix) {
                hints.push(ScopedWhichKeyHint::new(scope, hint));
            }
        }
        hints
    }
}

/// The one shared key resolver of a composed interface.
///
/// The resolver holds one immutable registry snapshot, one pending key prefix,
/// and one which-key overlay state. It reads no clock: the caller measures the
/// elapsed time and supplies it. A caller that draws no which-key overlay
/// supplies no time at all.
///
/// A host can hold the resolver inside a value that derives [`PartialEq`] and
/// [`Eq`]. Equality reads through the shared registry, so one comparison walks
/// every binding of every scope. Keep the comparison out of a hot path.
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
/// let now = Some(Duration::ZERO);
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
    ///
    /// # Examples
    ///
    /// The registry below binds a two-key sequence `g g` in the host-global
    /// scope and a different two-key sequence `g e` in the focused editor
    /// scope. Both extend the one-key prefix `g`, so the host-global scope
    /// arms it first, and the hints of the pending prefix name both scopes.
    ///
    /// ```
    /// # use std::fmt;
    /// # use std::sync::Arc;
    /// # use std::time::Duration;
    /// # use kvim_keymap::{
    /// #     Binding, CommandMetadata, Dispatch, DispatchContext, Input, InputContextSnapshot, Key,
    /// #     KeyCode, Registry, Resolver, Scope,
    /// # };
    /// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// # enum Action { First, Second }
    /// # impl fmt::Display for Action {
    /// #     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.id()) }
    /// # }
    /// # impl CommandMetadata for Action {
    /// #     fn id(&self) -> &str {
    /// #         match self {
    /// #             Self::First => "first",
    /// #             Self::Second => "second",
    /// #         }
    /// #     }
    /// #     fn label(&self) -> &str {
    /// #         match self {
    /// #             Self::First => "Go to the first line",
    /// #             Self::Second => "Go to the second scope's target",
    /// #         }
    /// #     }
    /// # }
    /// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// # enum HostScope { Global, Editor }
    /// # impl fmt::Display for HostScope {
    /// #     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    /// #         f.write_str(match self { Self::Global => "Global", Self::Editor => "Editor" })
    /// #     }
    /// # }
    /// # impl Scope for HostScope { const COUNT: usize = 2; }
    /// let g = Key::plain(KeyCode::Char('g'));
    /// let registry = Registry::from_bindings(
    ///     &[
    ///         Binding::host(HostScope::Global, &[g, g], Action::First),
    ///         Binding::surface(HostScope::Editor, &[g, Key::plain(KeyCode::Char('e'))], Action::Second),
    ///     ],
    ///     4,
    /// )?;
    /// let delay = Duration::from_millis(500);
    /// let mut resolver = Resolver::new(Arc::new(registry), 4, delay);
    /// let context = DispatchContext {
    ///     overlay: None,
    ///     global: Some(HostScope::Global),
    ///     focus: InputContextSnapshot::idle(HostScope::Editor),
    /// };
    ///
    /// resolver.dispatch(&context, Input::Key(g), Some(Duration::ZERO));
    /// assert!(
    ///     resolver.which_key(Duration::ZERO).is_none(),
    ///     "the overlay waits out its delay before it first appears"
    /// );
    ///
    /// let view = resolver.which_key(delay).expect("the delay elapsed");
    /// assert_eq!(view.prefix(), [g]);
    /// assert_eq!(
    ///     view.scope(),
    ///     HostScope::Global,
    ///     "the earlier scope in evaluation order armed the prefix"
    /// );
    ///
    /// let hints = view.hints();
    /// assert_eq!(hints.len(), 2, "the global scope and the editor scope both extend the prefix");
    /// assert_eq!(hints[0].scope(), HostScope::Global);
    /// assert_eq!(hints[0].hint().key(), g);
    /// assert_eq!(
    ///     hints[1].scope(),
    ///     HostScope::Editor,
    ///     "the focused scope's hint follows the global scope's hint"
    /// );
    /// assert_eq!(hints[1].hint().key(), Key::plain(KeyCode::Char('e')));
    ///
    /// assert_eq!(
    ///     resolver.dispatch(&context, Input::Key(g), Some(delay)),
    ///     Dispatch::Host { command: Action::First },
    ///     "the host scope's complete binding wins, because it precedes the editor scope"
    /// );
    ///
    /// // The editor scope's own hint also resolves, even though the host
    /// // scope armed the prefix first.
    /// resolver.dispatch(&context, Input::Key(g), Some(delay));
    /// assert_eq!(
    ///     resolver.dispatch(&context, Input::Key(Key::plain(KeyCode::Char('e'))), Some(delay)),
    ///     Dispatch::Surface {
    ///         command: Action::Second
    ///     },
    ///     "every hinted key resolves to some scope's binding"
    /// );
    /// # Ok::<(), kvim_keymap::RegistryError<Action, HostScope>>(())
    /// ```
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
        let PendingPrefix::Active {
            identity,
            scope,
            keys,
        } = &self.pending
        else {
            debug_assert!(false, "an empty prefix leaves the function above");
            return None;
        };
        Some(WhichKeyView {
            registry: &self.registry,
            identity: *identity,
            scope: *scope,
            prefix: keys,
        })
    }

    /// Returns one hint for each distinct first key of every scope of
    /// `context`, with no pending prefix.
    ///
    /// [`Resolver::which_key`] hints an extension of the pending prefix. This
    /// function hints the idle state instead, before any key of a sequence
    /// arrives. A complete one-key binding, such as a host-global escape, is
    /// never an extension of a pending prefix, so only this function surfaces
    /// it. A reader asks "what can I press", so this function answers at
    /// once. It reads no clock, waits out no which-key delay, and changes no
    /// overlay state.
    ///
    /// The hints come from every scope of `context`, in scope order, without
    /// repetition. This function does not fold hints across scopes. A key
    /// bound in two scopes yields two entries, one for each scope. The
    /// earlier scope in the order is the one that answers when the reader
    /// presses it.
    ///
    /// A key that only starts a longer sequence is still one entry, because
    /// the reader can still press it. Each hint's `target` method reports
    /// whether the key completes one command or opens a group of several.
    ///
    /// The list is longer than the list of one pending prefix, because it
    /// holds every first key of up to three scopes. The preset of the
    /// standalone editor holds 81 distinct first keys in Normal mode, 56 in
    /// Visual mode, and 48 in the sidebar. A host that draws the result
    /// through `kvim_ui::WhichKeyOverlay` must therefore respect
    /// `WHICH_KEY_HINTS_MAX`, which refuses a longer list instead of cutting
    /// it. Bound or page the list before the overlay takes it.
    ///
    /// # Examples
    ///
    /// The registry below binds `Ctrl-E` as a complete one-key binding in the
    /// host-global scope, the way a host binds the key that returns focus to
    /// its own surface. The focused editor scope binds its own leader
    /// sequence. `Ctrl-E` never extends that leader, so only the idle view
    /// surfaces it, marked with the host-global scope.
    ///
    /// ```
    /// # use std::fmt;
    /// # use std::sync::Arc;
    /// # use std::time::Duration;
    /// # use kvim_keymap::{
    /// #     Binding, CommandMetadata, DispatchContext, InputContextSnapshot, Key,
    /// #     KeyCode, Registry, Resolver, Scope,
    /// # };
    /// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// # enum Action { LeaveToChat, OpenFiles }
    /// # impl fmt::Display for Action {
    /// #     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.id()) }
    /// # }
    /// # impl CommandMetadata for Action {
    /// #     fn id(&self) -> &str {
    /// #         match self {
    /// #             Self::LeaveToChat => "leave-to-chat",
    /// #             Self::OpenFiles => "open-files",
    /// #         }
    /// #     }
    /// #     fn label(&self) -> &str {
    /// #         match self {
    /// #             Self::LeaveToChat => "Leave to chat",
    /// #             Self::OpenFiles => "Open the file picker",
    /// #         }
    /// #     }
    /// # }
    /// # #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    /// # enum HostScope { Global, Editor }
    /// # impl fmt::Display for HostScope {
    /// #     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    /// #         f.write_str(match self { Self::Global => "Global", Self::Editor => "Editor" })
    /// #     }
    /// # }
    /// # impl Scope for HostScope { const COUNT: usize = 2; }
    /// let escape = Key::ctrl(KeyCode::Char('e'));
    /// let leader = Key::plain(KeyCode::Char(' '));
    /// let registry = Registry::from_bindings(
    ///     &[
    ///         Binding::host(HostScope::Global, &[escape], Action::LeaveToChat),
    ///         Binding::surface(
    ///             HostScope::Editor,
    ///             &[leader, Key::plain(KeyCode::Char('f'))],
    ///             Action::OpenFiles,
    ///         ),
    ///     ],
    ///     4,
    /// )?;
    /// let resolver = Resolver::new(Arc::new(registry), 4, Duration::from_millis(500));
    /// let context = DispatchContext {
    ///     overlay: None,
    ///     global: Some(HostScope::Global),
    ///     focus: InputContextSnapshot::idle(HostScope::Editor),
    /// };
    ///
    /// let hints = resolver.idle_which_key(&context);
    /// assert_eq!(hints.len(), 2, "the escape and the leader are each one entry");
    /// assert_eq!(hints[0].scope(), HostScope::Global, "the host-global scope answers first");
    /// assert_eq!(hints[0].hint().key(), escape);
    /// assert_eq!(hints[1].scope(), HostScope::Editor);
    /// assert_eq!(hints[1].hint().key(), leader);
    /// # Ok::<(), kvim_keymap::RegistryError<Action, HostScope>>(())
    /// ```
    #[must_use]
    pub fn idle_which_key(&self, context: &DispatchContext<S>) -> Vec<ScopedWhichKeyHint<C, S>> {
        let mut hints = Vec::new();
        for scope in scope_order(context) {
            for hint in self.registry.hints_for_prefix(scope, &[]) {
                hints.push(ScopedWhichKeyHint::new(scope, hint));
            }
        }
        hints
    }

    /// Resolves one input against the supplied context at the elapsed time.
    ///
    /// The function evaluates the overlay scope, the host-global scope, and the
    /// focused scope in that order. A pending prefix keeps the scope that armed
    /// it, so a sequence never changes owner in the middle.
    ///
    /// `now` is the elapsed time that the caller measured. It reaches the
    /// which-key overlay alone. `None` states that the caller draws no
    /// which-key overlay, so pending input arms no timer and the overlay stays
    /// hidden.
    pub fn dispatch(
        &mut self,
        context: &DispatchContext<S>,
        input: Input,
        now: Option<Duration>,
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
        now: Option<Duration>,
    ) -> Dispatch<C> {
        match std::mem::replace(&mut self.pending, PendingPrefix::Idle) {
            PendingPrefix::Active { mut keys, .. } => {
                debug_assert!(
                    keys.len() < usize::from(self.keys_max),
                    "the registry rejects a binding above the pending-key maximum, so a prefix keeps room for one key"
                );
                keys.push(key);
                self.continue_prefix(identity, keys, now)
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
        now: Option<Duration>,
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
                if let Some(now) = now {
                    self.arm_overlay(now);
                }
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
    /// The walk mirrors `start_sequence`. A complete binding in any scope of
    /// the order beats a longer sequence in any other scope. Both passes run
    /// in full before the next begins, so the scope that resolves this key
    /// can differ from the scope that armed the prefix one key ago.
    fn continue_prefix(
        &mut self,
        identity: ContextIdentity<S>,
        keys: Vec<Key>,
        now: Option<Duration>,
    ) -> Dispatch<C> {
        for scope in identity.scope_order() {
            if let Some(bound) = self.registry.bound_command(scope, &keys) {
                self.overlay = OverlayState::Hidden;
                return dispatch_of(bound);
            }
        }
        for scope in identity.scope_order() {
            if self.registry.has_longer_sequence(scope, &keys) {
                if let Some(now) = now {
                    self.arm_overlay(now);
                }
                self.pending = PendingPrefix::Active {
                    identity,
                    scope,
                    keys,
                };
                return Dispatch::Pending;
            }
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
    scope_order_of(context.overlay, context.global, context.focus.scope)
}

/// Returns the overlay scope, the host-global scope, and the focused scope, in
/// that evaluation order, without repetition.
///
/// [`scope_order`] reads the three scopes from a live [`DispatchContext`].
/// [`ContextIdentity::scope_order`] reads the same three scopes from the
/// identity that armed a pending prefix. Both call this one function, so
/// `start_sequence` and [`WhichKeyView::hints`] never walk two different
/// orders.
fn scope_order_of<S: Scope>(
    overlay: Option<S>,
    global: Option<S>,
    focus: S,
) -> impl Iterator<Item = S> {
    let ordered = [overlay, global, Some(focus)];
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
#[path = "resolver_tests.rs"]
mod tests;
