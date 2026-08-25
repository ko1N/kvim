use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use super::{
    Dispatch, DispatchContext, Input, PasteError, PasteText, Resolver, TypedText, scope_order,
};
use crate::binding::{Binding, CommandMetadata, CommandOwner, Scope};
use crate::context::{ContextGeneration, InputContextSnapshot, TextFallback};
use crate::hint::ScopedWhichKeyHint;
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
        resolver.dispatch(&context, Input::Key(ch('j')), Some(NOW)),
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
        resolver.dispatch(&context, Input::Key(ch('j')), Some(NOW)),
        Dispatch::Host {
            command: Action::Close
        },
        "a host binding wins over the focused surface"
    );
    assert_eq!(
        resolver.dispatch(
            &context,
            Input::Key(Key::ctrl(KeyCode::Char('q'))),
            Some(NOW)
        ),
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
        resolver.dispatch(&with_overlay, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Surface {
            command: Action::Close
        }
    );

    let context = normal();
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(resolver.pending_keys(), [ch('g')]);
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
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
        resolver.dispatch(&context, Input::Key(ch('a')), Some(NOW)),
        Dispatch::Text {
            owner: CommandOwner::Surface,
            text: TypedText::Typed('a')
        }
    );

    let mut normal_focus = InputContextSnapshot::idle(Table::Normal);
    normal_focus.text_fallback = TextFallback::Typed(CommandOwner::Surface);
    let context = DispatchContext::focused(normal_focus);
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('a')), Some(NOW)),
        Dispatch::Unbound,
        "the second key of a started sequence types nothing"
    );
}

#[test]
fn a_context_change_clears_the_pending_prefix() {
    let mut resolver = resolver();
    let context = normal();
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );

    let mut changed = normal();
    changed.focus.generation = ContextGeneration::FIRST.advanced();
    assert_eq!(
        resolver.dispatch(&changed, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending,
        "the cleared prefix starts the sequence again"
    );
    assert_eq!(resolver.pending_keys(), [ch('g')]);

    let mut focused_elsewhere = changed;
    focused_elsewhere.focus.scope = Table::Insert;
    assert_eq!(
        resolver.dispatch(&focused_elsewhere, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Unbound,
        "a focus change clears the prefix and Insert binds no `g`"
    );

    let mut overlay_opened = normal();
    overlay_opened.overlay = Some(Table::Overlay);
    assert_eq!(
        resolver.dispatch(&normal(), Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        resolver.dispatch(&overlay_opened, Input::Key(ch('g')), Some(NOW)),
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
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Unsupported, Some(NOW)),
        Dispatch::Unsupported
    );
    assert!(resolver.pending_keys().is_empty());
}

#[test]
fn a_paste_follows_the_text_fallback_of_the_focused_scope() {
    let mut resolver = resolver();
    let block = PasteText::new("two words").expect("the block is bounded");
    assert_eq!(
        resolver.dispatch(&insert(), Input::Paste(block.clone()), Some(NOW)),
        Dispatch::Text {
            owner: CommandOwner::Surface,
            text: TypedText::Pasted(block.clone())
        }
    );
    assert_eq!(
        resolver.dispatch(&normal(), Input::Paste(block), Some(NOW)),
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
fn a_paste_block_converts_every_crlf_pair_to_one_line_feed() {
    assert_eq!(
        PasteText::new("one\r\ntwo").map(|text| text.as_str().to_owned()),
        Ok("one\ntwo".to_owned())
    );
}

#[test]
fn a_paste_block_converts_a_lone_carriage_return_to_a_line_feed() {
    assert_eq!(
        PasteText::new("one\rtwo").map(|text| text.as_str().to_owned()),
        Ok("one\ntwo".to_owned())
    );
}

#[test]
fn a_paste_block_converts_mixed_carriage_returns_in_one_block() {
    assert_eq!(
        PasteText::new("one\r\ntwo\rthree\nfour").map(|text| text.as_str().to_owned()),
        Ok("one\ntwo\nthree\nfour".to_owned())
    );
}

#[test]
fn normalization_lets_a_crlf_block_fit_that_would_not_fit_unnormalized() {
    // Every `\r\n` pair collapses to one `\n`, so a block whose raw
    // length exceeds the bound can still fit once normalized.
    let pairs = super::PASTE_BYTES_MAX;
    let text = "\r\n".repeat(pairs);
    let pasted = PasteText::new(&text).expect("the normalized block fits the bound");
    assert_eq!(pasted.as_str().len(), pairs);
}

#[test]
fn a_paste_block_above_the_bound_after_normalization_still_reports_too_long() {
    let pairs = super::PASTE_BYTES_MAX + 1;
    let text = "\r\n".repeat(pairs);
    assert_eq!(
        PasteText::new(&text),
        Err(PasteError::TooLong { bytes: pairs })
    );
}

#[test]
fn the_which_key_view_reads_the_same_registry_and_prefix() {
    let mut resolver = resolver();
    let context = normal();
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
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
fn which_key_hints_from_one_scope_stay_unchanged() {
    let mut resolver = resolver();
    let context = normal();
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );

    let view = resolver.which_key(DELAY).expect("the delay passed");
    let hints = view.hints();
    assert_eq!(
        hints.len(),
        1,
        "only the Normal scope extends the prefix, so it yields one hint"
    );
    assert_eq!(hints[0].scope(), Table::Normal);
    assert_eq!(hints[0].hint().key(), ch('g'));
    assert_eq!(hints[0].hint().commands(), [Action::FirstLine]);
}

#[test]
fn which_key_hints_span_the_host_and_the_focused_scope() {
    // The host scope and the focused scope both extend the one-key prefix
    // `g`, so the host scope arms it, and the hints of the pending prefix
    // name both scopes, in evaluation order.
    let bindings = vec![
        Binding::host(Table::Global, &[ch('g'), ch('x')], Action::Close),
        Binding::surface(Table::Normal, &[ch('g'), ch('g')], Action::FirstLine),
    ];
    let registry = Registry::from_bindings(&bindings, 4).expect("the test table validates");
    let mut resolver = Resolver::new(Arc::new(registry), 4, DELAY);
    let context = DispatchContext {
        overlay: None,
        global: Some(Table::Global),
        focus: InputContextSnapshot::idle(Table::Normal),
    };

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    let view = resolver.which_key(DELAY).expect("the delay passed");
    assert_eq!(
        view.scope(),
        Table::Global,
        "the earlier scope in evaluation order armed the prefix"
    );

    let hints = view.hints();
    let scopes: Vec<_> = hints.iter().map(|hint| hint.scope()).collect();
    assert_eq!(
        scopes,
        vec![Table::Global, Table::Normal],
        "the host scope's hint precedes the focused scope's hint, without repetition"
    );
    assert_eq!(hints[0].hint().key(), ch('x'));
    assert_eq!(hints[1].hint().key(), ch('g'));
}

#[test]
fn idle_which_key_lists_one_entry_per_distinct_first_key_of_one_scope() {
    let resolver = resolver();
    let context = normal();

    let hints = resolver.idle_which_key(&context);
    let entries: Vec<_> = hints
        .iter()
        .map(|hint| (hint.scope(), hint.hint().key()))
        .collect();
    assert_eq!(
        entries,
        vec![(Table::Normal, ch('g')), (Table::Normal, ch('j'))],
        "the focused scope alone answers, one entry for each distinct first key"
    );
}

#[test]
fn idle_which_key_surfaces_a_host_global_escape_that_the_leader_never_reaches() {
    // A host binds Ctrl-E, Ctrl-N, and Ctrl-P as complete one-key bindings in
    // its global scope, the way a host reserves the keys that leave its
    // embedded editor. None of the three extends the focused scope's own
    // leader sequence, so a pending-prefix view can never surface them. This
    // is the reported gap: a reader inside the focused scope had no way to
    // learn that the three keys return to the host.
    let bindings = vec![
        Binding::host(
            Table::Global,
            &[Key::ctrl(KeyCode::Char('e'))],
            Action::Close,
        ),
        Binding::host(
            Table::Global,
            &[Key::ctrl(KeyCode::Char('n'))],
            Action::Down,
        ),
        Binding::host(
            Table::Global,
            &[Key::ctrl(KeyCode::Char('p'))],
            Action::Quit,
        ),
        Binding::surface(Table::Normal, &[ch(' '), ch('c')], Action::FirstLine),
    ];
    let registry = Registry::from_bindings(&bindings, 4).expect("the test table validates");
    let resolver = Resolver::new(Arc::new(registry), 4, DELAY);
    let context = DispatchContext {
        overlay: None,
        global: Some(Table::Global),
        focus: InputContextSnapshot::idle(Table::Normal),
    };

    let hints = resolver.idle_which_key(&context);
    let escapes: Vec<_> = hints
        .iter()
        .filter(|hint| hint.scope() == Table::Global)
        .map(|hint| hint.hint().key())
        .collect();
    assert_eq!(
        escapes,
        vec![
            Key::ctrl(KeyCode::Char('e')),
            Key::ctrl(KeyCode::Char('n')),
            Key::ctrl(KeyCode::Char('p')),
        ],
        "every host-global escape is a complete one-key binding, so the idle view lists it with no pending prefix"
    );
    assert!(
        hints
            .iter()
            .any(|hint| hint.scope() == Table::Normal && hint.hint().key() == ch(' ')),
        "the focused scope's own leader still appears beside the host-global escapes"
    );
}

#[test]
fn idle_which_key_does_not_fold_a_shared_key_across_two_scopes() {
    // Both scopes bind their own sequence under the same first key `j`. The
    // idle view keeps that key as two entries, one for each scope, because
    // pressing it resolves to only one of them: the earlier scope in
    // evaluation order.
    let resolver = resolver();
    let context = DispatchContext {
        overlay: None,
        global: Some(Table::Global),
        focus: InputContextSnapshot::idle(Table::Normal),
    };

    let hints = resolver.idle_which_key(&context);
    let j_entries: Vec<_> = hints
        .iter()
        .filter(|hint| hint.hint().key() == ch('j'))
        .map(ScopedWhichKeyHint::scope)
        .collect();
    assert_eq!(
        j_entries,
        vec![Table::Global, Table::Normal],
        "the host-global scope's entry precedes the focused scope's entry, and neither is dropped"
    );
}

#[test]
fn a_later_scopes_completion_still_resolves_after_an_earlier_scope_arms_the_prefix() {
    // The host scope arms the prefix, because it holds a longer sequence
    // under the same first key. Only the focused scope binds the second key,
    // so the walk that continues the prefix must reach it. Before this walk
    // spanned the scope order, the second key returned `Unbound`.
    let bindings = vec![
        Binding::host(Table::Global, &[ch('g'), ch('x')], Action::Close),
        Binding::surface(Table::Normal, &[ch('g'), ch('g')], Action::FirstLine),
    ];
    let registry = Registry::from_bindings(&bindings, 4).expect("the test table validates");
    let mut resolver = Resolver::new(Arc::new(registry), 4, DELAY);
    let context = DispatchContext {
        overlay: None,
        global: Some(Table::Global),
        focus: InputContextSnapshot::idle(Table::Normal),
    };

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending,
        "the host scope arms the prefix"
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Surface {
            command: Action::FirstLine
        },
        "the focused scope's own completion still resolves, even though the host scope armed the prefix"
    );
}

#[test]
fn the_earlier_scope_wins_a_completion_collision() {
    // Both scopes bind the exact same two-key sequence to different
    // commands. The host scope precedes the focused scope in evaluation
    // order, so its command answers, even though the focused scope also
    // completes the same keys.
    let bindings = vec![
        Binding::host(Table::Global, &[ch('g'), ch('g')], Action::Close),
        Binding::surface(Table::Normal, &[ch('g'), ch('g')], Action::FirstLine),
    ];
    let registry = Registry::from_bindings(&bindings, 4).expect("the test table validates");
    let mut resolver = Resolver::new(Arc::new(registry), 4, DELAY);
    let context = DispatchContext {
        overlay: None,
        global: Some(Table::Global),
        focus: InputContextSnapshot::idle(Table::Normal),
    };

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Host {
            command: Action::Close
        },
        "the host scope precedes the focused scope in evaluation order, so it wins the collision"
    );
}

#[test]
fn a_later_scopes_complete_binding_beats_an_earlier_scopes_longer_sequence() {
    // The host scope holds a three-key sequence under the same two-key
    // prefix that the focused scope completes. The complete-binding pass
    // must finish across every scope before the longer-sequence pass
    // considers re-arming the host scope, or the host scope would win by
    // holding a longer sequence at this depth. That would prove the two
    // passes had merged into one.
    let bindings = vec![
        Binding::host(Table::Global, &[ch('g'), ch('g'), ch('z')], Action::Close),
        Binding::surface(Table::Normal, &[ch('g'), ch('g')], Action::FirstLine),
    ];
    let registry = Registry::from_bindings(&bindings, 4).expect("the test table validates");
    let mut resolver = Resolver::new(Arc::new(registry), 4, DELAY);
    let context = DispatchContext {
        overlay: None,
        global: Some(Table::Global),
        focus: InputContextSnapshot::idle(Table::Normal),
    };

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending,
        "the host scope arms the prefix, because it is the earlier scope with a longer sequence"
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Surface {
            command: Action::FirstLine
        },
        "the focused scope's complete binding wins, even though the host scope also extends this prefix"
    );
}

#[test]
fn every_hinted_key_resolves_to_something_other_than_unbound() {
    // The host scope binds two extensions of the prefix, one of which
    // collides with the focused scope's own key. The focused scope binds a
    // third extension that only it holds. Every key that `hints()` reports
    // must still resolve, because the walk that continues a prefix now spans
    // the same scope order that armed it.
    let bindings = vec![
        Binding::host(Table::Global, &[ch('g'), ch('x')], Action::Close),
        Binding::host(Table::Global, &[ch('g'), ch('g')], Action::Quit),
        Binding::surface(Table::Normal, &[ch('g'), ch('g')], Action::FirstLine),
        Binding::surface(Table::Normal, &[ch('g'), ch('e')], Action::Down),
    ];
    let registry =
        Arc::new(Registry::from_bindings(&bindings, 4).expect("the test table validates"));
    let context = DispatchContext {
        overlay: None,
        global: Some(Table::Global),
        focus: InputContextSnapshot::idle(Table::Normal),
    };

    let mut resolver = Resolver::new(Arc::clone(&registry), 4, DELAY);
    resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW));
    let view = resolver.which_key(DELAY).expect("the delay passed");
    let hinted_keys: Vec<Key> = view.hints().iter().map(|hint| hint.hint().key()).collect();
    assert_eq!(
        hinted_keys.len(),
        4,
        "the host scope hints two keys and the focused scope hints two keys"
    );

    for key in hinted_keys {
        let mut fresh = Resolver::new(Arc::clone(&registry), 4, DELAY);
        fresh.dispatch(&context, Input::Key(ch('g')), Some(NOW));
        let outcome = fresh.dispatch(&context, Input::Key(key), Some(NOW));
        assert_ne!(
            outcome,
            Dispatch::Unbound,
            "every key that the which-key view hints must resolve to something"
        );
    }
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
        resolver.dispatch(&normal(), Input::Key(ch('g')), Some(DELAY)),
        Dispatch::Pending
    );
    assert!(
        resolver.which_key(DELAY).is_some(),
        "the armed delay already passed, so the hints appear at once"
    );
}

#[test]
fn a_caller_that_supplies_no_time_arms_no_overlay() {
    let context = normal();

    let mut without_clock = resolver();
    assert_eq!(
        without_clock.dispatch(&context, Input::Key(ch('g')), None),
        Dispatch::Pending,
        "the sequence opens without a clock"
    );
    assert_eq!(
        without_clock.overlay_deadline(),
        None,
        "no timer armed, so the host needs no wake"
    );
    assert!(
        without_clock.which_key(DELAY).is_none(),
        "no elapsed time reveals an overlay that never armed"
    );

    let mut with_clock = resolver();
    assert_eq!(
        with_clock.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        with_clock.overlay_deadline(),
        Some(DELAY),
        "a supplied time still arms the overlay"
    );
}

#[test]
fn a_completed_command_and_a_cleared_prefix_both_hide_the_overlay() {
    let mut resolver = resolver();
    let context = normal();
    resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW));
    assert!(resolver.which_key(DELAY).is_some());
    resolver.dispatch(&context, Input::Key(ch('g')), Some(DELAY));
    assert!(resolver.which_key(DELAY).is_none());

    resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW));
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

/// Builds one resolver over the supplied bindings.
fn resolver_over(bindings: Vec<Binding<Action, Table>>) -> Resolver<Action, Table> {
    let registry = Registry::from_bindings(&bindings, 4).expect("the test table validates");
    Resolver::new(Arc::new(registry), 4, DELAY)
}

/// Returns a context whose scopes are the host-global table and the focused
/// table.
fn host_and_focus() -> DispatchContext<Table> {
    DispatchContext {
        overlay: None,
        global: Some(Table::Global),
        focus: InputContextSnapshot::idle(Table::Normal),
    }
}

#[test]
fn a_host_global_binding_interrupts_a_pending_focused_prefix() {
    // The host binds `Ctrl-Q` alone, the way a host binds the key that returns
    // focus to its own surface. The focused scope holds the two-key sequence
    // `g g`. No scope binds `g Ctrl-Q`, and no scope extends it, so the third
    // pass reads `Ctrl-Q` alone against the host scope, which precedes the
    // focused scope.
    let escape = Key::ctrl(KeyCode::Char('q'));
    let mut resolver = resolver_over(vec![
        Binding::host(Table::Global, &[escape], Action::Quit),
        Binding::surface(Table::Normal, &[ch('g'), ch('g')], Action::FirstLine),
    ]);
    let context = host_and_focus();

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending,
        "the focused scope arms the prefix"
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(escape), Some(NOW)),
        Dispatch::Interrupted {
            owner: CommandOwner::Host,
            command: Action::Quit
        },
        "the host scope precedes the focused scope, so its escape cancels the prefix and runs"
    );
    assert!(
        resolver.pending_keys().is_empty(),
        "the interruption leaves no prefix behind"
    );
}

#[test]
fn a_key_of_the_scope_that_owns_the_prefix_does_not_interrupt_it() {
    // `d` opens the operator sequence of the focused scope, and the same scope
    // binds `x` alone. Vim aborts the operator there, so `x` must abort the
    // sequence instead of running. The host scope precedes the focused scope
    // but binds no `x`, so it takes nothing either.
    let mut resolver = resolver_over(vec![
        Binding::host(
            Table::Global,
            &[Key::ctrl(KeyCode::Char('q'))],
            Action::Quit,
        ),
        Binding::surface(Table::Normal, &[ch('d'), ch('w')], Action::Down),
        Binding::surface(Table::Normal, &[ch('x')], Action::Close),
    ]);
    let context = host_and_focus();

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('d')), Some(NOW)),
        Dispatch::Pending,
        "the focused scope owns the operator prefix"
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('x')), Some(NOW)),
        Dispatch::Unbound,
        "a key of the scope that owns the prefix aborts the sequence, exactly as Vim aborts it"
    );
    assert!(resolver.pending_keys().is_empty());
}

#[test]
fn an_overlay_interrupts_a_host_prefix_and_the_focused_scope_does_not() {
    // The host scope arms the prefix, so only the overlay scope precedes it.
    // The overlay key therefore interrupts, and the focused key does not.
    let bindings = vec![
        Binding::host(Table::Global, &[ch('g'), ch('g')], Action::Quit),
        Binding::surface(Table::Overlay, &[ch('p')], Action::PickNext),
        Binding::surface(Table::Normal, &[ch('j')], Action::Down),
    ];
    let context = DispatchContext {
        overlay: Some(Table::Overlay),
        global: Some(Table::Global),
        focus: InputContextSnapshot::idle(Table::Normal),
    };

    let mut resolver = resolver_over(bindings.clone());
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending,
        "the host scope arms the prefix, because the overlay scope holds no `g`"
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('p')), Some(NOW)),
        Dispatch::Interrupted {
            owner: CommandOwner::Surface,
            command: Action::PickNext
        },
        "the overlay scope precedes the host scope"
    );

    let mut resolver = resolver_over(bindings);
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('j')), Some(NOW)),
        Dispatch::Unbound,
        "the focused scope follows the host scope, so its own key interrupts nothing"
    );
}

#[test]
fn a_context_with_one_scope_holds_no_preceding_scope() {
    // The one scope of this context binds the two-key sequence and the one-key
    // command together. A preceding scope with that one-key command would
    // interrupt, so this test states that no scope precedes the owner here.
    let mut resolver = resolver_over(vec![
        Binding::surface(Table::Normal, &[ch('g'), ch('g')], Action::FirstLine),
        Binding::surface(Table::Normal, &[ch('j')], Action::Down),
    ]);
    let context = normal();
    assert_eq!(
        scope_order(&context).collect::<Vec<_>>(),
        vec![Table::Normal],
        "one scope answers, so the interruption pass reads nothing"
    );

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('j')), Some(NOW)),
        Dispatch::Unbound,
        "the one scope owns its own prefix, so its own key breaks the sequence"
    );
    assert!(resolver.pending_keys().is_empty());

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Surface {
            command: Action::FirstLine
        },
        "the complete sequence still resolves, key for key"
    );
}

#[test]
fn a_key_that_only_opens_a_group_of_a_preceding_scope_does_not_interrupt() {
    // The host scope binds `z z` and binds `z` alone to nothing. An
    // interruption on `z` would cancel the sequence of the reader and run no
    // command, so the pass reads the pressed key alone and takes nothing.
    let mut resolver = resolver_over(vec![
        Binding::host(Table::Global, &[ch('z'), ch('z')], Action::Quit),
        Binding::surface(Table::Normal, &[ch('g'), ch('g')], Action::FirstLine),
    ]);
    let context = host_and_focus();

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('z')), Some(NOW)),
        Dispatch::Unbound,
        "a key that only opens a group of a preceding scope interrupts nothing"
    );
}

#[test]
fn a_completion_of_the_owning_scope_beats_an_interruption() {
    // The host scope binds `x` alone, and the focused scope completes `g x`.
    // The first pass runs before the interruption pass, so the completion wins.
    let mut resolver = resolver_over(vec![
        Binding::host(Table::Global, &[ch('x')], Action::Quit),
        Binding::surface(Table::Normal, &[ch('g'), ch('x')], Action::FirstLine),
    ]);
    let context = host_and_focus();

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('x')), Some(NOW)),
        Dispatch::Surface {
            command: Action::FirstLine
        },
        "a complete binding of any scope beats the interruption of a preceding scope"
    );
}

#[test]
fn a_longer_sequence_of_the_owning_scope_beats_an_interruption() {
    // The host scope binds `x` alone, and the focused scope extends `g x` to a
    // three-key sequence. The second pass runs before the interruption pass,
    // so the sequence stays pending.
    let mut resolver = resolver_over(vec![
        Binding::host(Table::Global, &[ch('x')], Action::Quit),
        Binding::surface(
            Table::Normal,
            &[ch('g'), ch('x'), ch('y')],
            Action::FirstLine,
        ),
    ]);
    let context = host_and_focus();

    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('g')), Some(NOW)),
        Dispatch::Pending
    );
    assert_eq!(
        resolver.dispatch(&context, Input::Key(ch('x')), Some(NOW)),
        Dispatch::Pending,
        "a longer sequence of any scope beats the interruption of a preceding scope"
    );
    assert_eq!(resolver.pending_keys(), [ch('g'), ch('x')]);
}
