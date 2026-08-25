use std::num::NonZeroU32;
use std::path::PathBuf;

use super::{COMMAND_LINE_CHARS_MAX, CommandLineCommand, CommandLineError, NAMES};

fn line(value: u32) -> CommandLineCommand {
    CommandLineCommand::GoToLine(NonZeroU32::new(value).expect("the test line is not zero"))
}

#[test]
fn the_fixed_command_set_parses() {
    let cases = [
        ("w", CommandLineCommand::Write),
        ("write", CommandLineCommand::Write),
        ("q", CommandLineCommand::Quit),
        ("quit", CommandLineCommand::Quit),
        ("q!", CommandLineCommand::QuitDiscard),
        ("quit!", CommandLineCommand::QuitDiscard),
        ("wq", CommandLineCommand::WriteQuit),
        ("  w  ", CommandLineCommand::Write),
        ("  quit  ", CommandLineCommand::Quit),
        (
            "e src/main.rs",
            CommandLineCommand::Edit(PathBuf::from("src/main.rs")),
        ),
        (
            "edit src/main.rs",
            CommandLineCommand::Edit(PathBuf::from("src/main.rs")),
        ),
        ("e", CommandLineCommand::Reload),
        ("edit", CommandLineCommand::Reload),
        ("  e  ", CommandLineCommand::Reload),
        ("e!", CommandLineCommand::ReloadDiscard),
        ("edit!", CommandLineCommand::ReloadDiscard),
        ("l", CommandLineCommand::Log),
        ("logs", CommandLineCommand::Log),
        ("d", CommandLineCommand::Diagnostics),
        ("diagnostics", CommandLineCommand::Diagnostics),
        (
            "e  a path/with space.rs ",
            CommandLineCommand::Edit(PathBuf::from("a path/with space.rs")),
        ),
        ("1", line(1)),
        ("0042", line(42)),
        ("4294967295", line(u32::MAX)),
    ];
    for (input, expected) in cases {
        assert_eq!(
            CommandLineCommand::parse(input),
            Ok(expected),
            "`:{input}` must parse"
        );
    }
}

#[test]
fn every_length_between_the_declared_minimum_and_the_full_name_parses() {
    for name in NAMES {
        let full = CommandLineCommand::parse(name.full);
        assert!(full.is_ok(), "the full name `:{}` must parse", name.full);
        for length in name.minimum..=name.full.len() {
            let abbreviation = &name.full[..length];
            assert_eq!(
                CommandLineCommand::parse(abbreviation),
                full,
                "`:{abbreviation}` must reach the same command as `:{}`",
                name.full
            );
        }
        for length in 0..name.minimum {
            let shorter = &name.full[..length];
            assert_ne!(
                CommandLineCommand::parse(shorter),
                full,
                "`:{shorter}` is below the declared minimum of `:{}`",
                name.full
            );
        }
    }
}

#[test]
fn each_declared_minimum_names_one_command() {
    let declared: Vec<&str> = NAMES.iter().map(|name| name.full).collect();
    let mut ordered = declared.clone();
    ordered.sort_unstable();
    ordered.dedup();
    assert_eq!(
        ordered, declared,
        "the completion offers the names in this order, and each one once"
    );
    for name in NAMES {
        let minimum = &name.full[..name.minimum];
        let shadowed: Vec<&str> = NAMES
            .iter()
            .filter(|other| {
                other.full != name.full
                    && other.full.starts_with(minimum)
                    && minimum.len() >= other.minimum
            })
            .map(|other| other.full)
            .collect();
        assert!(
            shadowed.is_empty(),
            "`:{minimum}` names `:{}` and also {shadowed:?}",
            name.full
        );
    }
}

#[test]
fn the_declared_minimum_of_write_resolves_the_prefix_that_wq_shares() {
    // A plain unique-prefix rule would reject `:w`, because `w` starts both
    // `write` and `wq`. The declared minimum of `write` names the winner.
    let cases = [
        ("w", CommandLineCommand::Write),
        ("wr", CommandLineCommand::Write),
        ("wri", CommandLineCommand::Write),
        ("writ", CommandLineCommand::Write),
        ("write", CommandLineCommand::Write),
        ("wq", CommandLineCommand::WriteQuit),
    ];
    for (input, expected) in cases {
        assert_eq!(
            CommandLineCommand::parse(input),
            Ok(expected),
            "`:{input}` must parse"
        );
    }
}

#[test]
fn the_name_source_offers_a_full_name_that_the_parser_accepts() {
    let cases: [(&str, &[&str]); 25] = [
        // A text without a `!` offers no `!` variant, so no cycle of that
        // text writes a command that discards unsaved changes.
        ("", &["diagnostics", "edit", "logs", "quit", "wq", "write"]),
        ("d", &["diagnostics"]),
        // `diagnostics` has no `!` variant, so a typed `!` offers nothing.
        ("d!", &[]),
        ("e", &["edit"]),
        ("edit", &["edit"]),
        ("l", &["logs"]),
        ("logs", &["logs"]),
        // `log` has no `!` variant, so a typed `!` offers nothing.
        ("l!", &[]),
        ("q", &["quit"]),
        ("qu", &["quit"]),
        // The typed `!` is a deliberate choice, so the completion serves it.
        ("e!", &["edit!"]),
        ("edit!", &["edit!"]),
        ("q!", &["quit!"]),
        ("qu!", &["quit!"]),
        ("quit!", &["quit!"]),
        // Neither `wq` nor `write` is a `!` variant, so `w` offers both.
        ("w", &["wq", "write"]),
        ("wr", &["write"]),
        ("wq", &["wq"]),
        // `write` and `wq` have no `!` variant, and `!` is no name.
        ("w!", &[]),
        ("wq!", &[]),
        ("!", &[]),
        ("x", &[]),
        ("42", &[]),
        (" q", &[]),
        ("e src/ma", &[]),
    ];
    for (line, expected) in cases {
        let names = CommandLineCommand::names_matching(line);
        assert_eq!(names, expected, "`:{line}` offers {names:?}");
        for name in &names {
            assert!(
                CommandLineCommand::parse(name).is_ok(),
                "the completion offers `:{name}`, so the parser must accept it"
            );
            assert!(
                line.ends_with('!') || !name.ends_with('!'),
                "`:{line}` holds no `!`, so the completion must not offer `:{name}`"
            );
        }
    }
}

#[test]
fn every_other_line_is_a_typed_rejection() {
    let long = "w".repeat(COMMAND_LINE_CHARS_MAX + 1);
    let cases = [
        ("", CommandLineError::Empty),
        ("   ", CommandLineError::Empty),
        (
            long.as_str(),
            CommandLineError::TooLong {
                chars: COMMAND_LINE_CHARS_MAX + 1,
                chars_max: COMMAND_LINE_CHARS_MAX,
            },
        ),
        (
            "0",
            CommandLineError::LineNumberOutOfRange { max: u32::MAX },
        ),
        (
            "4294967296",
            CommandLineError::LineNumberOutOfRange { max: u32::MAX },
        ),
        // A text longer than the full name names no command.
        ("quitt", CommandLineError::Unknown),
        ("edits", CommandLineError::Unknown),
        // Only `edit` carries an argument, and only `quit` and `edit`
        // carry a `!`.
        ("write foo", CommandLineError::Unknown),
        ("quit foo", CommandLineError::Unknown),
        ("log foo", CommandLineError::Unknown),
        ("log!", CommandLineError::Unknown),
        ("diagnostics foo", CommandLineError::Unknown),
        ("diagnostics!", CommandLineError::Unknown),
        ("e! src/main.rs", CommandLineError::Unknown),
        ("edit! src/main.rs", CommandLineError::Unknown),
        ("w!", CommandLineError::Unknown),
        ("write!", CommandLineError::Unknown),
        ("wq!", CommandLineError::Unknown),
        ("e!!", CommandLineError::Unknown),
        ("wqa", CommandLineError::Unknown),
        ("q!!", CommandLineError::Unknown),
        ("!", CommandLineError::Unknown),
        ("W", CommandLineError::Unknown),
        (":w", CommandLineError::Unknown),
        ("s/a/b/", CommandLineError::Unknown),
        ("12a", CommandLineError::Unknown),
        ("12 a", CommandLineError::Unknown),
    ];
    for (input, expected) in cases {
        assert_eq!(
            CommandLineCommand::parse(input),
            Err(expected),
            "`:{input}` must be rejected"
        );
    }
}
