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
