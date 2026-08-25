use std::num::NonZeroU32;

use kvim_keymap::{CommandOwner, Dispatch, PasteText, Phase, TextFallback, TypedText};
use kvim_settings::InputSettings;

use super::{Reduction, SemanticOperation, SemanticReducer};
use crate::{BindingScope, Command, InputContext, Mode, PromptKind};

fn reducer() -> SemanticReducer {
    SemanticReducer::new(InputSettings::default())
}

fn surface(command: Command) -> Dispatch<Command> {
    Dispatch::Surface { command }
}

fn typed(value: char) -> Dispatch<Command> {
    Dispatch::Text {
        owner: CommandOwner::Surface,
        text: TypedText::Typed(value),
    }
}

fn operation(command: Command, count: Option<u32>, register: Option<char>) -> Reduction {
    Reduction::Operation(SemanticOperation {
        command,
        count: count.and_then(NonZeroU32::new),
        register,
    })
}

#[test]
fn a_count_composes_from_digits_and_clears_after_the_operation() {
    let mut reducer = reducer();
    assert_eq!(
        reducer.reduce(surface(Command::CountDigitThree)).reduction,
        Reduction::Prefix
    );
    assert_eq!(reducer.phases().count, Phase::Pending);
    assert_eq!(
        reducer.reduce(surface(Command::MoveDown)).reduction,
        operation(Command::MoveDown, Some(3), None)
    );
    assert_eq!(reducer.phases().count, Phase::Empty);
}

#[test]
fn the_zero_motion_becomes_the_zero_digit_while_a_count_is_open() {
    let mut reducer = reducer();
    assert_eq!(
        reducer.reduce(surface(Command::MoveFirstColumn)).reduction,
        operation(Command::MoveFirstColumn, None, None),
        "`0` alone is the first-column motion"
    );
    reducer.reduce(surface(Command::CountDigitOne));
    assert_eq!(
        reducer.reduce(surface(Command::MoveFirstColumn)).reduction,
        Reduction::Prefix,
        "`0` after `1` extends the count"
    );
    assert_eq!(
        reducer.reduce(surface(Command::MoveDown)).reduction,
        operation(Command::MoveDown, Some(10), None)
    );
}

#[test]
fn a_count_above_the_maximum_resets_every_phase() {
    let mut reducer = reducer();
    for _ in 0..4 {
        assert_eq!(
            reducer.reduce(surface(Command::CountDigitNine)).reduction,
            Reduction::Prefix
        );
    }
    assert_eq!(
        reducer.reduce(surface(Command::CountDigitNine)).reduction,
        Reduction::Unbound,
        "the count maximum is 9,999, so the fifth digit is invalid input"
    );
    assert_eq!(reducer.phases().count, Phase::Empty);
    assert_eq!(
        reducer.reduce(surface(Command::MoveDown)).reduction,
        operation(Command::MoveDown, None, None)
    );
}

#[test]
fn an_operator_opens_its_scope_and_one_command_closes_it() {
    let mut reducer = reducer();
    assert_eq!(
        reducer.reduce(surface(Command::DeleteOverMotion)).reduction,
        operation(Command::DeleteOverMotion, None, None)
    );
    assert_eq!(reducer.active_scope(), BindingScope::OperatorPending);
    assert_eq!(reducer.phases().operator, Phase::Pending);
    // `d2w` keeps the operator through the count and the motion.
    reducer.reduce(surface(Command::CountDigitTwo));
    assert_eq!(reducer.active_scope(), BindingScope::OperatorPending);
    assert_eq!(
        reducer
            .reduce(surface(Command::MoveNextWordStart))
            .reduction,
        operation(Command::MoveNextWordStart, Some(2), None)
    );
    assert_eq!(reducer.phases().operator, Phase::Empty);
    assert_eq!(reducer.active_scope(), BindingScope::Mode(Mode::Normal));
}

#[test]
fn a_register_qualifies_exactly_one_operation() {
    let mut reducer = reducer();
    assert_eq!(
        reducer.reduce(surface(Command::SelectRegister)).reduction,
        Reduction::Prefix
    );
    assert_eq!(reducer.active_scope(), BindingScope::RegisterSelection);
    assert_eq!(
        reducer.active_scope().text_fallback(),
        TextFallback::Typed(CommandOwner::Surface)
    );
    assert_eq!(reducer.reduce(typed('a')).reduction, Reduction::Prefix);
    assert_eq!(reducer.phases().register, Phase::Pending);
    assert_eq!(
        reducer.reduce(surface(Command::YankLine)).reduction,
        operation(Command::YankLine, None, Some('a'))
    );
    assert_eq!(reducer.phases().register, Phase::Empty);
    assert_eq!(
        reducer.reduce(surface(Command::YankLine)).reduction,
        operation(Command::YankLine, None, None),
        "the register applies to one operation only"
    );
}

#[test]
fn a_register_selection_takes_a_count_and_a_name_together() {
    let mut reducer = reducer();
    reducer.reduce(surface(Command::CountDigitTwo));
    reducer.reduce(surface(Command::SelectRegister));
    reducer.reduce(typed('z'));
    assert_eq!(
        reducer.reduce(surface(Command::DeleteOverMotion)).reduction,
        operation(Command::DeleteOverMotion, Some(2), Some('z'))
    );
}

#[test]
fn an_invalid_register_name_resets_the_selection() {
    let mut reducer = reducer();
    reducer.reduce(surface(Command::SelectRegister));
    assert_eq!(reducer.reduce(typed(' ')).reduction, Reduction::Unbound);
    assert_eq!(reducer.phases().register, Phase::Empty);

    let mut pasted = reducer;
    pasted.reduce(surface(Command::SelectRegister));
    let block = PasteText::new("ab").expect("the block is bounded");
    assert_eq!(
        pasted
            .reduce(Dispatch::Text {
                owner: CommandOwner::Surface,
                text: TypedText::Pasted(block),
            })
            .reduction,
        Reduction::Unbound,
        "a paste names no register"
    );
}

#[test]
fn every_reset_path_clears_the_count_the_operator_and_the_register() {
    let open = |reducer: &mut SemanticReducer| {
        reducer.reduce(surface(Command::CountDigitTwo));
        reducer.reduce(surface(Command::SelectRegister));
        reducer.reduce(typed('b'));
        reducer.reduce(surface(Command::DeleteOverMotion));
        reducer.reduce(surface(Command::CountDigitThree));
    };

    let mut cancelled = reducer();
    open(&mut cancelled);
    assert_eq!(
        cancelled.cancel().reduction,
        operation(Command::ReturnToNormal, None, None),
        "the editor aborts its operator over a command that names no target"
    );
    assert!(cancelled.phases().is_idle());

    let mut unbound = reducer();
    open(&mut unbound);
    assert_eq!(
        unbound.reduce(Dispatch::Unbound).reduction,
        Reduction::Unbound
    );
    assert!(unbound.phases().is_idle());

    let mut unsupported = reducer();
    open(&mut unsupported);
    assert_eq!(
        unsupported.reduce(Dispatch::Unsupported).reduction,
        Reduction::Unsupported
    );
    assert!(unsupported.phases().is_idle());

    let mut changed = reducer();
    open(&mut changed);
    changed.set_context(InputContext::Mode(Mode::Visual));
    assert!(changed.phases().is_idle());

    let mut prompted = reducer();
    open(&mut prompted);
    prompted.set_context(InputContext::NORMAL.open_prompt(PromptKind::Search));
    let phases = prompted.phases();
    assert!(!phases.count.is_pending() && !phases.operator.is_pending());
    assert!(!phases.register.is_pending() && phases.prompt.is_pending());
}

#[test]
fn a_cancel_without_an_operator_reports_a_cancellation() {
    let mut reducer = reducer();
    reducer.reduce(surface(Command::CountDigitFive));
    assert_eq!(reducer.cancel().reduction, Reduction::Cancelled);
}

#[test]
fn every_state_change_publishes_a_new_generation() {
    let mut reducer = reducer();
    let start = reducer.snapshot().generation;
    let counted = reducer.reduce(surface(Command::CountDigitFour)).context;
    assert_ne!(counted.generation, start);
    let moved = reducer.reduce(surface(Command::MoveDown)).context;
    assert_ne!(moved.generation, counted.generation);

    // A typed character changes no phase, so it keeps the generation and
    // the pending prefix of the shared resolver.
    reducer.set_context(InputContext::Mode(Mode::Insert));
    let before = reducer.snapshot().generation;
    let published = reducer.reduce(typed('a')).context;
    assert_eq!(published.generation, before);
    assert_eq!(
        published.text_fallback,
        TextFallback::Typed(CommandOwner::Surface)
    );
}

#[test]
fn a_pending_sequence_publishes_the_text_object_phase_in_its_own_scopes() {
    let mut reducer = reducer();
    reducer.reduce(surface(Command::DeleteOverMotion));
    let armed = reducer.reduce(Dispatch::Pending);
    assert_eq!(armed.reduction, Reduction::Prefix);
    assert_eq!(armed.context.phases.text_object, Phase::Pending);
    assert_eq!(
        armed.context.generation,
        reducer.snapshot().generation,
        "a pending prefix publishes no new generation"
    );
    assert_eq!(
        reducer
            .reduce(surface(Command::SelectInnerWord))
            .context
            .phases
            .text_object,
        Phase::Empty
    );

    let mut normal = reducer;
    normal.set_context(InputContext::NORMAL);
    assert_eq!(
        normal.reduce(Dispatch::Pending).context.phases.text_object,
        Phase::Empty,
        "Normal mode binds no text object, so `g` opens no object phase"
    );
}

#[test]
fn a_prompt_context_publishes_its_scope_and_its_text_fallback() {
    let mut reducer = reducer();
    reducer.set_context(InputContext::NORMAL.open_prompt(PromptKind::CommandLine));
    let snapshot = reducer.snapshot();
    assert_eq!(snapshot.scope, BindingScope::Prompt);
    assert_eq!(
        snapshot.text_fallback,
        TextFallback::Typed(CommandOwner::Surface)
    );
    assert!(snapshot.phases.prompt.is_pending());

    reducer.set_context(InputContext::NORMAL.open_confirmation());
    assert_eq!(reducer.snapshot().scope, BindingScope::Confirmation);
}

#[test]
fn a_picker_prompt_puts_the_picker_table_above_the_query() {
    let mut reducer = reducer();
    reducer.set_context(InputContext::Picker.open_prompt(PromptKind::Picker));
    let context = reducer.dispatch_context();
    assert_eq!(context.overlay, Some(BindingScope::Picker));
    assert_eq!(context.global, None);
    assert_eq!(context.focus.scope, BindingScope::Prompt);
}
