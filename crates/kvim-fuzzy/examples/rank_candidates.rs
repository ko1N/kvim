//! Ranks one list of candidates against one query.
//!
//! Run it with `cargo run -p kvim-fuzzy --example rank_candidates`.
//!
//! The example holds no path and no buffer. It shows the whole workflow that a
//! caller performs: score every candidate, drop the candidates that the query
//! does not answer, and order the rest by score.

use kvim_fuzzy::score_candidate;

/// One candidate of the list below: the name and the directory that holds it.
struct Candidate {
    name: &'static str,
    directory: &'static str,
}

const CANDIDATES: &[Candidate] = &[
    Candidate {
        name: "main.rs",
        directory: "src",
    },
    Candidate {
        name: "manifest.toml",
        directory: "",
    },
    Candidate {
        name: "notes.md",
        directory: "docs",
    },
    Candidate {
        name: "mod.rs",
        directory: "src/parser",
    },
];

fn main() {
    let query = "mn";

    let mut ranked: Vec<(i32, &Candidate)> = CANDIDATES
        .iter()
        .filter_map(|candidate| {
            let score = score_candidate(query, candidate.name, candidate.directory)?;
            Some((score, candidate))
        })
        .collect();

    // A higher score ranks first. The scan is deterministic, so an equal score
    // keeps the order of the input list.
    ranked.sort_by(|left, right| right.0.cmp(&left.0));

    println!(
        "the query {query:?} ranks {} of {}",
        ranked.len(),
        CANDIDATES.len()
    );
    for (score, candidate) in &ranked {
        if candidate.directory.is_empty() {
            println!("  {score:>6}  {}", candidate.name);
        } else {
            println!("  {score:>6}  {}/{}", candidate.directory, candidate.name);
        }
    }
}
