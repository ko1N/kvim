use super::{COMPLETION_CANDIDATES_MAX, CompletionCycle, CompletionOutcome, LineCompletion};

/// The character bound of a prompt that accepts every test candidate.
const CHARS_MAX: usize = 16;

#[test]
fn the_completion_bounds_its_candidates_and_rejects_an_empty_list() {
    let none = LineCompletion::open("q", Vec::new(), CHARS_MAX, CompletionCycle::Next);
    assert!(none.is_none(), "an empty completion is unrepresentable");

    // The prompt rejects a longer text, so the completion never writes one.
    let long = "a".repeat(CHARS_MAX + 1);
    let dropped = LineCompletion::open(
        "a",
        vec![long, "ab".to_owned()],
        CHARS_MAX,
        CompletionCycle::Next,
    )
    .expect("one candidate fits the bound");
    assert_eq!(dropped.selected(), "ab");
    assert_eq!(dropped.outcome(), CompletionOutcome::Completed);

    // The bound drops the tail of a longer source, so the cycle returns to
    // the first candidate after exactly the bounded number of steps.
    let many: Vec<String> = (0..COMPLETION_CANDIDATES_MAX + 8)
        .map(|index| format!("c{index}"))
        .collect();
    let mut bounded = LineCompletion::open("c", many, CHARS_MAX, CompletionCycle::Next)
        .expect("the source holds candidates");
    assert_eq!(bounded.outcome(), CompletionOutcome::Listed);
    assert_eq!(bounded.selected(), "c0");
    for _ in 0..COMPLETION_CANDIDATES_MAX - 1 {
        bounded.cycle(CompletionCycle::Next);
    }
    let last = format!("c{}", COMPLETION_CANDIDATES_MAX - 1);
    assert_eq!(bounded.selected(), last, "the bound drops every later row");
    bounded.cycle(CompletionCycle::Next);
    assert_eq!(bounded.selected(), "c0", "the cycle wraps at the bound");
}
