use kvim_ui::{Selector, SelectorCandidate};

fn main() {
    let mut selector = Selector::default();
    selector.set_candidates(vec![SelectorCandidate::new(1_u8, "one", "")], false);
    assert_eq!(selector.matches().len(), 1);
}
