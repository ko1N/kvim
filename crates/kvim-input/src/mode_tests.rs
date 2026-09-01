use kvim_keymap::UnboundInput;

use super::{BindingScope, InputContext, Mode, PromptKind, TreePrompt};

#[test]
fn scope_indexes_are_unique_and_bounded() {
    let mut seen = [false; BindingScope::COUNT];
    for scope in BindingScope::ALL {
        let index = scope.index();
        assert!(
            index < BindingScope::COUNT,
            "{scope} indexes outside the table"
        );
        assert!(!seen[index], "{scope} repeats a table index");
        seen[index] = true;
    }
}

#[test]
fn a_second_prompt_keeps_the_original_return_scope() {
    let visual = InputContext::Mode(Mode::Visual);
    let search = visual.open_prompt(PromptKind::Search);
    let command = search.open_prompt(PromptKind::CommandLine);
    assert_eq!(command.scope(), BindingScope::Mode(Mode::Visual));
    assert_eq!(command.close_prompt(), visual);
}

#[test]
fn a_confirmation_returns_input_to_the_scope_below_it() {
    let sidebar = InputContext::Sidebar;
    let confirmation = sidebar.open_confirmation();
    assert_eq!(confirmation.prompt(), None);
    assert_eq!(confirmation.scope(), BindingScope::Sidebar);
    assert_eq!(confirmation.close_prompt(), sidebar);
}

#[test]
fn a_tree_prompt_returns_input_to_the_sidebar() {
    let sidebar = InputContext::Sidebar;
    let prompt = sidebar.open_prompt(PromptKind::Tree(TreePrompt::Rename));
    assert_eq!(prompt.scope(), BindingScope::Sidebar);
    assert_eq!(prompt.close_prompt(), sidebar);
}

#[test]
fn prompt_kinds_declare_empty_backspace_cancellation() {
    let cancelling = [
        PromptKind::CommandLine,
        PromptKind::Search,
        PromptKind::Tree(TreePrompt::AddFile),
        PromptKind::Tree(TreePrompt::AddDirectory),
        PromptKind::Tree(TreePrompt::Search),
        PromptKind::Picker,
    ];
    for kind in cancelling {
        assert!(
            kind.cancels_on_empty_backspace(),
            "{kind:?} cancels when Backspace reaches an empty line"
        );
    }
    assert!(
        !PromptKind::Tree(TreePrompt::Rename).cancels_on_empty_backspace(),
        "rename stays open so the seed can be replaced"
    );
}

#[test]
fn the_register_selection_is_the_only_scope_that_unbound_input_cancels() {
    // The scope waits for one register name that it binds nowhere, so any
    // other input ends it. Every other scope binds its own cancel keys or
    // keeps its state, so unbound input leaves it open.
    for scope in BindingScope::ALL {
        let expected = if scope == BindingScope::RegisterSelection {
            UnboundInput::Cancels
        } else {
            UnboundInput::Ignored
        };
        assert_eq!(
            scope.unbound_input(),
            expected,
            "the {scope} scope declares the wrong rule for unbound input"
        );
    }
}
