//! The semantic reducer that composes the kvim grammar after shared dispatch.
//!
//! The shared resolver of `kvim-keymap` owns the only key table. It returns one
//! command, one text block, or one typed miss. This reducer composes those
//! outcomes into the Vim grammar: a decimal count, a waiting operator, a
//! selected register, a text object, and an open prompt. It reads no key, so no
//! second sequence table exists.
//!
//! Every reduction publishes one [`InputContextSnapshot`]. The host supplies
//! that snapshot with the next resolution request, and the shared resolver
//! clears its pending prefix whenever the generation changes.
//!
//! `docs/input-actions.md` owns the reset rules that this module implements.

use std::num::NonZeroU32;

use kvim_keymap::{
    ContextGeneration, Dispatch, DispatchContext, InputContextSnapshot, Phase, SemanticPhases,
    TypedText,
};
use kvim_settings::InputSettings;

use super::command::Command;
use super::mode::{BindingScope, InputContext};

/// The decimal count that precedes one operation.
///
/// The active variant always holds a value between one and the count maximum,
/// because `0` opens no count.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CountPhase {
    /// No digit opened a count.
    #[default]
    Empty,
    /// The digits that the user typed so far.
    Digits(u32),
}

/// Whether an operator waits for the target that the next commands name.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum OperatorPhase {
    /// No operator waits, so the active context owns the keys.
    #[default]
    Idle,
    /// One operator waits, so [`BindingScope::OperatorPending`] owns the keys.
    Pending,
}

/// The register that qualifies the next completed operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RegisterPhase {
    /// No register qualifies the next operation.
    #[default]
    Empty,
    /// The selection waits for the name of the register.
    Selecting,
    /// The named register qualifies exactly one completed operation.
    Selected(char),
}

/// One completed semantic operation.
///
/// The count and the register belong to this operation alone. The reducer
/// clears both as soon as it publishes the operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticOperation {
    /// The command that the operation performs.
    pub command: Command,
    /// The decimal count before the operation.
    pub count: Option<NonZeroU32>,
    /// The register that qualifies the operation.
    pub register: Option<char>,
}

/// The semantic outcome of one dispatched input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reduction {
    /// A grammar-prefix transition. No operation completes.
    ///
    /// A count digit, a register selection, and a pending key sequence all
    /// reach this outcome.
    Prefix,
    /// One complete semantic operation.
    Operation(SemanticOperation),
    /// Literal text for the focused surface.
    Text(TypedText),
    /// The pending semantic state was cancelled and reset.
    Cancelled,
    /// No binding and no text fallback took the input.
    Unbound,
    /// The terminal reported input that no binding accepts.
    Unsupported,
}

/// One reduction with the context that it published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reduced {
    /// What the input reached.
    pub reduction: Reduction,
    /// The context that the next resolution request carries.
    pub context: InputContextSnapshot<BindingScope>,
}

/// The semantic reducer of the kvim editor grammar.
///
/// The reducer owns the count, the operator, the register, the text object, and
/// the prompt ownership of one editor instance. It owns no key sequence.
///
/// ```
/// use kvim_input::{Command, Reduction, SemanticReducer};
/// use kvim_keymap::Dispatch;
/// use kvim_settings::InputSettings;
///
/// let mut reducer = SemanticReducer::new(InputSettings::default());
/// assert_eq!(
///     reducer
///         .reduce(Dispatch::Surface {
///             command: Command::CountDigitThree
///         })
///         .reduction,
///     Reduction::Prefix
/// );
/// let reduced = reducer.reduce(Dispatch::Surface {
///     command: Command::MoveDown,
/// });
/// assert!(matches!(
///     reduced.reduction,
///     Reduction::Operation(operation) if operation.count == std::num::NonZeroU32::new(3)
/// ));
/// ```
#[derive(Clone, Debug)]
pub struct SemanticReducer {
    settings: InputSettings,
    context: InputContext,
    count: CountPhase,
    operator: OperatorPhase,
    register: RegisterPhase,
    text_object: Phase,
    generation: ContextGeneration,
}

impl SemanticReducer {
    /// Creates a reducer in Normal mode with every phase empty.
    #[must_use]
    pub fn new(settings: InputSettings) -> Self {
        debug_assert!(
            settings.count_max > 0,
            "EditorSettings holds a positive count maximum"
        );
        Self {
            settings,
            context: InputContext::NORMAL,
            count: CountPhase::Empty,
            operator: OperatorPhase::Idle,
            register: RegisterPhase::Empty,
            text_object: Phase::Empty,
            generation: ContextGeneration::FIRST,
        }
    }

    /// Returns the current input context.
    #[inline]
    #[must_use]
    pub const fn context(&self) -> InputContext {
        self.context
    }

    /// Moves input to another context and resets every grammar phase.
    ///
    /// A mode change, a focus change, and a prompt change all reset the count,
    /// the operator, the register, and the text object. An unchanged context
    /// changes nothing, so it keeps a pending sequence alive.
    pub fn set_context(&mut self, context: InputContext) {
        if context == self.context {
            return;
        }
        self.context = context;
        self.reset();
    }

    /// Clears every grammar phase and publishes a new generation.
    pub fn reset(&mut self) {
        self.count = CountPhase::Empty;
        self.operator = OperatorPhase::Idle;
        self.register = RegisterPhase::Empty;
        self.text_object = Phase::Empty;
        self.generation = self.generation.advanced();
    }

    /// Returns the scope that owns the keys of the next request.
    ///
    /// An open prompt and an open confirmation own the keys first. A waiting
    /// register selection owns them next, then a waiting operator, because `i`
    /// and `a` start a text object there instead of Insert mode.
    #[must_use]
    pub fn active_scope(&self) -> BindingScope {
        match self.context.owning_scope() {
            scope @ (BindingScope::Prompt | BindingScope::Confirmation) => scope,
            scope => {
                if matches!(self.register, RegisterPhase::Selecting) {
                    BindingScope::RegisterSelection
                } else if self.operator == OperatorPhase::Pending {
                    BindingScope::OperatorPending
                } else {
                    scope
                }
            }
        }
    }

    /// Returns the grammar phases of the surface.
    #[must_use]
    pub fn phases(&self) -> SemanticPhases {
        SemanticPhases {
            count: phase_of(!matches!(self.count, CountPhase::Empty)),
            operator: phase_of(self.operator == OperatorPhase::Pending),
            register: phase_of(!matches!(self.register, RegisterPhase::Empty)),
            text_object: self.text_object,
            prompt: phase_of(matches!(
                self.context.owning_scope(),
                BindingScope::Prompt | BindingScope::Confirmation
            )),
        }
    }

    /// Returns the context that the next resolution request carries.
    #[must_use]
    pub fn snapshot(&self) -> InputContextSnapshot<BindingScope> {
        let scope = self.active_scope();
        InputContextSnapshot {
            scope,
            phases: self.phases(),
            text_fallback: scope.text_fallback(),
            generation: self.generation,
        }
    }

    /// Returns the scopes that the next resolution request evaluates.
    ///
    /// The standalone editor is its own host, so it declares no host-global
    /// scope. An open picker reads a query through the prompt line and owns its
    /// own chords, so its table answers above that prompt.
    #[must_use]
    pub fn dispatch_context(&self) -> DispatchContext<BindingScope> {
        let focus = self.snapshot();
        let overlay = (focus.scope == BindingScope::Prompt
            && self.context.scope() == BindingScope::Picker)
            .then_some(BindingScope::Picker);
        DispatchContext {
            overlay,
            global: None,
            focus,
        }
    }

    /// Reports whether a count or a register selection waits for more input.
    ///
    /// A cancel reaches this state directly. A waiting operator is absent here,
    /// because its own scope binds the cancel keys to a command.
    #[must_use]
    pub fn holds_grammar_prefix(&self) -> bool {
        !matches!(self.count, CountPhase::Empty) || !matches!(self.register, RegisterPhase::Empty)
    }

    /// Cancels every pending grammar phase.
    ///
    /// A waiting operator lives in the editor too, so the cancel must reach it.
    /// [`Command::ReturnToNormal`] names no motion and no text object, which
    /// aborts the operator and changes nothing.
    pub fn cancel(&mut self) -> Reduced {
        let operator_waited = self.operator == OperatorPhase::Pending;
        self.reset();
        let reduction = if operator_waited {
            Reduction::Operation(SemanticOperation {
                command: Command::ReturnToNormal,
                count: None,
                register: None,
            })
        } else {
            Reduction::Cancelled
        };
        self.published(reduction)
    }

    /// Composes one dispatched input into the editor grammar.
    pub fn reduce(&mut self, dispatch: Dispatch<Command>) -> Reduced {
        let reduction = match dispatch {
            Dispatch::Pending => {
                // The phase mirrors the pending prefix of the shared resolver,
                // so it publishes no new generation. A new generation would
                // clear the very prefix that produced this phase.
                self.text_object = phase_of(self.active_scope().binds_text_objects());
                return Reduced {
                    reduction: Reduction::Prefix,
                    context: self.snapshot(),
                };
            }
            Dispatch::Host { command } | Dispatch::Surface { command } => self.command(command),
            Dispatch::Text { text, .. } => self.text(text),
            Dispatch::Unsupported => {
                self.reset();
                Reduction::Unsupported
            }
            Dispatch::Unbound => {
                self.reset();
                Reduction::Unbound
            }
        };
        self.published(reduction)
    }

    /// Composes one resolved command.
    fn command(&mut self, command: Command) -> Reduction {
        self.text_object = Phase::Empty;
        if let Some(digit) = command.count_digit() {
            return self.grow_count(digit);
        }
        // `0` is the first-column motion until a count is already open, so the
        // registry binds it to that motion and the reducer reads it here.
        if command == Command::MoveFirstColumn && !matches!(self.count, CountPhase::Empty) {
            return self.grow_count(0);
        }
        if command == Command::SelectRegister {
            self.register = RegisterPhase::Selecting;
            self.generation = self.generation.advanced();
            return Reduction::Prefix;
        }
        let count = match self.count {
            CountPhase::Empty => None,
            CountPhase::Digits(value) => {
                debug_assert!(
                    value > 0,
                    "`0` opens no count, so an open count is positive"
                );
                NonZeroU32::new(value)
            }
        };
        let register = match self.register {
            RegisterPhase::Empty => None,
            RegisterPhase::Selected(name) => Some(name),
            RegisterPhase::Selecting => {
                debug_assert!(
                    false,
                    "the register-selection scope binds no key, so no command completes there"
                );
                None
            }
        };
        self.count = CountPhase::Empty;
        self.register = RegisterPhase::Empty;
        // The editor consumes exactly one command after an operator, so one
        // completed command always closes the operator-pending scope, even when
        // it names another operator: `dd` is one linewise delete.
        self.operator = match self.operator {
            OperatorPhase::Idle if command.starts_operator_pending() => OperatorPhase::Pending,
            OperatorPhase::Idle | OperatorPhase::Pending => OperatorPhase::Idle,
        };
        self.generation = self.generation.advanced();
        Reduction::Operation(SemanticOperation {
            command,
            count,
            register,
        })
    }

    /// Extends the decimal count with one digit.
    ///
    /// The composition is checked and bounded. A count above the maximum is
    /// invalid input, so it resets every phase.
    fn grow_count(&mut self, digit: u8) -> Reduction {
        let current = match self.count {
            CountPhase::Empty => 0,
            CountPhase::Digits(value) => value,
        };
        let next = current
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(digit)))
            .filter(|value| *value <= self.settings.count_max);
        let Some(next) = next else {
            self.reset();
            return Reduction::Unbound;
        };
        self.count = CountPhase::Digits(next);
        self.generation = self.generation.advanced();
        Reduction::Prefix
    }

    /// Composes one text block.
    ///
    /// A waiting register selection reads the name of its register here. Every
    /// other scope passes the text to the focused surface.
    fn text(&mut self, text: TypedText) -> Reduction {
        if !matches!(self.register, RegisterPhase::Selecting) {
            self.text_object = Phase::Empty;
            return Reduction::Text(text);
        }
        let TypedText::Typed(name) = text else {
            // A paste names no register, so the selection is invalid.
            self.reset();
            return Reduction::Unbound;
        };
        if !is_register_name(name) {
            self.reset();
            return Reduction::Unbound;
        }
        self.register = RegisterPhase::Selected(name);
        self.generation = self.generation.advanced();
        Reduction::Prefix
    }

    /// Publishes one reduction with the context that follows it.
    fn published(&self, reduction: Reduction) -> Reduced {
        Reduced {
            reduction,
            context: self.snapshot(),
        }
    }
}

/// Returns the phase that one pending flag names.
#[inline]
const fn phase_of(pending: bool) -> Phase {
    if pending {
        Phase::Pending
    } else {
        Phase::Empty
    }
}

/// Reports whether one character names a register.
///
/// The accepted set is the ASCII alphanumeric characters, the unnamed register
/// `"`, and the black-hole register `_`. Every other character is an invalid
/// selection, which resets the register phase.
#[inline]
const fn is_register_name(value: char) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, '"' | '_')
}

#[cfg(test)]
mod tests {
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
}
