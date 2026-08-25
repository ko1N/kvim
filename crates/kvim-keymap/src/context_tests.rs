use super::{ContextGeneration, Phase, SemanticPhases, TextFallback};
use crate::binding::CommandOwner;

#[test]
fn every_phase_alone_leaves_the_context_busy() {
    let cases = [
        SemanticPhases {
            count: Phase::Pending,
            ..SemanticPhases::IDLE
        },
        SemanticPhases {
            operator: Phase::Pending,
            ..SemanticPhases::IDLE
        },
        SemanticPhases {
            register: Phase::Pending,
            ..SemanticPhases::IDLE
        },
        SemanticPhases {
            text_object: Phase::Pending,
            ..SemanticPhases::IDLE
        },
        SemanticPhases {
            prompt: Phase::Pending,
            ..SemanticPhases::IDLE
        },
    ];
    assert!(SemanticPhases::IDLE.is_idle());
    for phases in cases {
        assert!(
            !phases.is_idle(),
            "{phases:?} holds one pending phase, so a transition must wait"
        );
    }
}

#[test]
fn each_generation_differs_from_the_one_before_it() {
    let mut generation = ContextGeneration::FIRST;
    let mut seen = vec![generation];
    for _ in 0..64 {
        generation = generation.advanced();
        assert!(
            !seen.contains(&generation),
            "a repeated generation would keep a stale pending prefix"
        );
        seen.push(generation);
    }
}

#[test]
fn a_text_fallback_names_at_most_one_owner() {
    assert_eq!(TextFallback::None.owner(), None);
    assert_eq!(
        TextFallback::Typed(CommandOwner::Host).owner(),
        Some(CommandOwner::Host)
    );
}
