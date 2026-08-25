use super::{JUMPS_MAX, JumpDirection, JumpEntry, JumpList, JumpStep};

use kvim_workspace::BufferId;

/// Returns one entry that names a buffer and a line.
fn entry(buffer: u32, line: usize) -> JumpEntry {
    JumpEntry::new(BufferId::new(buffer), None, line, 0)
}

#[test]
fn a_push_discards_the_forward_history() {
    let mut jumps = JumpList::default();
    jumps.push(entry(1, 10));
    jumps.push(entry(1, 20));
    jumps.push(entry(1, 30));

    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 40)),
        JumpStep::Moved(entry(1, 30))
    );
    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 30)),
        JumpStep::Moved(entry(1, 20))
    );

    // The jump taken from inside the history drops lines 30 and 40.
    jumps.push(entry(1, 25));
    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 25)),
        JumpStep::Moved(entry(1, 20))
    );
    assert_eq!(
        jumps.step(JumpDirection::Forward, entry(1, 20)),
        JumpStep::Moved(entry(1, 25))
    );
    assert_eq!(
        jumps.step(JumpDirection::Forward, entry(1, 25)),
        JumpStep::AtNewest
    );
}

#[test]
fn a_push_that_repeats_the_current_line_replaces_the_entry() {
    let buffer = BufferId::new(1);
    let mut jumps = JumpList::default();
    jumps.push(JumpEntry::new(buffer, None, 10, 0));
    jumps.push(JumpEntry::new(buffer, None, 10, 7));

    // The list holds the second column and one entry, not two.
    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 50)),
        JumpStep::Moved(JumpEntry::new(buffer, None, 10, 7))
    );
    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 10)),
        JumpStep::AtOldest
    );
}

#[test]
fn a_push_drops_an_older_entry_that_names_the_same_line() {
    let mut jumps = JumpList::default();
    jumps.push(entry(1, 10));
    jumps.push(entry(1, 20));
    // Line 10 already sits in the list behind line 20, so the list keeps
    // the new entry alone and a walk stops on line 10 exactly once.
    jumps.push(entry(1, 10));

    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 99)),
        JumpStep::Moved(entry(1, 10))
    );
    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 10)),
        JumpStep::Moved(entry(1, 20))
    );
    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 20)),
        JumpStep::AtOldest
    );
}

#[test]
fn the_bound_drops_the_oldest_entry() {
    let mut jumps = JumpList::default();
    for line in 1..=JUMPS_MAX {
        jumps.push(entry(1, line));
    }
    jumps.push(entry(1, JUMPS_MAX + 1));

    // The first step repeats the newest line, so it records nothing new and
    // the walk sees the list exactly as the pushes left it.
    let mut current = entry(1, JUMPS_MAX + 1);
    let mut steps = 0_usize;
    let mut oldest = None;
    loop {
        match jumps.step(JumpDirection::Backward, current) {
            JumpStep::Moved(reached) => {
                steps += 1;
                assert!(steps <= JUMPS_MAX, "the bound must stop the walk");
                current = reached.clone();
                oldest = Some(reached);
            }
            JumpStep::AtOldest => break,
            JumpStep::AtNewest => panic!("a backward step never reaches the newest end"),
        }
    }

    assert_eq!(steps, JUMPS_MAX - 1);
    assert_eq!(oldest.map(|reached| reached.line()), Some(2));
}

#[test]
fn a_backward_step_and_a_forward_step_return_to_the_start() {
    let mut jumps = JumpList::default();
    jumps.push(entry(1, 10));

    let start = entry(2, 50);
    assert_eq!(
        jumps.step(JumpDirection::Backward, start.clone()),
        JumpStep::Moved(entry(1, 10))
    );
    assert_eq!(
        jumps.step(JumpDirection::Forward, entry(1, 10)),
        JumpStep::Moved(start)
    );
}

#[test]
fn a_backward_step_at_the_oldest_entry_reports_that_end() {
    let mut jumps = JumpList::default();
    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 5)),
        JumpStep::AtOldest
    );

    jumps.push(entry(1, 10));
    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 50)),
        JumpStep::Moved(entry(1, 10))
    );
    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 10)),
        JumpStep::AtOldest
    );
}

#[test]
fn a_forward_step_at_the_newest_entry_reports_that_end() {
    let mut jumps = JumpList::default();
    jumps.push(entry(1, 10));
    assert_eq!(
        jumps.step(JumpDirection::Forward, entry(1, 50)),
        JumpStep::AtNewest
    );

    assert_eq!(
        jumps.step(JumpDirection::Backward, entry(1, 50)),
        JumpStep::Moved(entry(1, 10))
    );
    assert_eq!(
        jumps.step(JumpDirection::Forward, entry(1, 10)),
        JumpStep::Moved(entry(1, 50))
    );
    assert_eq!(
        jumps.step(JumpDirection::Forward, entry(1, 50)),
        JumpStep::AtNewest
    );
}
