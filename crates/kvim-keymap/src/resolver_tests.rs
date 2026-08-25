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
