use kvim_fuzzy::rank;

fn main() {
    let ranked = rank("sec", [("first", ""), ("second", "")]);
    assert_eq!(ranked.len(), 1);
}
