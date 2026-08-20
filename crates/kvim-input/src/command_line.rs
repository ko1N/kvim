//! The command line parser for the fixed first-release command set.
//!
//! Kvim implements no Ex grammar. The parser accepts `write`, `quit`, `wq`,
//! `edit`, `edit <path>`, `log`, the `!` variant of `quit` and `edit`, and a
//! line number only. It rejects every other line.
//!
//! Each command declares one full name and the shortest abbreviation that names
//! it, as Vim does. `quit` declares one character, so `q`, `qu`, `qui`, and
//! `quit` all reach the same command. The declared minimum names the command,
//! and the shortest unique prefix does not: `w` starts both `write` and `wq`,
//! and the minimum of `write` keeps `:w` unambiguous. See
//! `docs/input-actions.md`.

use std::num::NonZeroU32;
use std::path::PathBuf;

use thiserror::Error;

/// The largest command line that Kvim accepts, in characters.
///
/// The bound keeps one rejected paste from growing the prompt without limit. A
/// path far below this length still fits every supported filesystem.
pub const COMMAND_LINE_CHARS_MAX: usize = 1024;

/// One accepted command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandLineCommand {
    /// `:w` saves the active buffer.
    Write,
    /// `:q` closes the focused window.
    Quit,
    /// `:q!` closes the focused window and discards unsaved changes.
    QuitDiscard,
    /// `:wq` saves the active buffer, then closes the focused window.
    WriteQuit,
    /// `:e <path>` opens one file in the focused window.
    Edit(PathBuf),
    /// `:e` reads the file of the focused window again.
    Reload,
    /// `:e!` discards the unsaved changes of that buffer and reads its file.
    ReloadDiscard,
    /// `:log` opens one snapshot of the editor log in a new buffer.
    Log,
    /// `:<number>` moves the cursor to that line.
    GoToLine(NonZeroU32),
}

/// A rejected command line.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CommandLineError {
    /// The line held no command.
    #[error("the command line is empty")]
    Empty,
    /// The line was longer than [`COMMAND_LINE_CHARS_MAX`].
    #[error("the command line holds {chars} characters, but the maximum is {chars_max}")]
    TooLong {
        /// The number of characters in the rejected line.
        chars: usize,
        /// The accepted maximum.
        chars_max: usize,
    },
    /// The line number was zero, or it did not fit an unsigned 32-bit value.
    #[error("the line number must be between 1 and {max}")]
    LineNumberOutOfRange {
        /// The largest accepted line number.
        max: u32,
    },
    /// The line matched no accepted command.
    #[error(
        "the command line accepts :w[rite], :q[uit], :q[uit]!, :wq, :e[dit], :e[dit]!, :e[dit] <path>, :l[og], and :<number> only"
    )]
    Unknown,
}

/// The command that one declared name reaches, without its argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NamedCommand {
    /// The name of [`CommandLineCommand::Edit`], [`CommandLineCommand::Reload`],
    /// and [`CommandLineCommand::ReloadDiscard`].
    Edit,
    /// The name of [`CommandLineCommand::Log`].
    Log,
    /// The name of [`CommandLineCommand::Quit`] and
    /// [`CommandLineCommand::QuitDiscard`].
    Quit,
    /// The name of [`CommandLineCommand::Write`].
    Write,
    /// The name of [`CommandLineCommand::WriteQuit`].
    WriteQuit,
}

/// The command name of one command line and the path after it.
///
/// The value keeps the name exactly as the user typed it, so a completed line
/// holds the abbreviation that the user chose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandPathArgument<'line> {
    /// The command name, without its separator.
    pub name: &'line str,
    /// The path text after the name, without a surrounding blank.
    pub typed: &'line str,
}

/// Whether a `!` follows the typed name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Bang {
    /// The name carries no `!`.
    Absent,
    /// The name carries a `!`.
    Present,
}

/// One command name and the shortest abbreviation that names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CommandName {
    /// The command that the name reaches.
    command: NamedCommand,
    /// The full name of the command.
    full: &'static str,
    /// The smallest number of leading characters that names the command.
    ///
    /// The value is a promise. A later release may declare a smaller minimum,
    /// because that breaks no command line that already works. A larger one
    /// breaks a command line that a user already types.
    minimum: usize,
}

/// Every declared command name, in ascending order of the full name.
///
/// The table is the sole source of the command names, so one new command needs
/// one new row here and no new completion code. The table holds no `:<number>`
/// row, because a line number is no name. It holds no `:e <path>` row either,
/// because the path is an argument of `edit`. See `docs/input-actions.md`.
const NAMES: [CommandName; 5] = [
    CommandName {
        command: NamedCommand::Edit,
        full: "edit",
        minimum: 1,
    },
    CommandName {
        command: NamedCommand::Log,
        full: "log",
        minimum: 1,
    },
    CommandName {
        command: NamedCommand::Quit,
        full: "quit",
        minimum: 1,
    },
    CommandName {
        command: NamedCommand::WriteQuit,
        full: "wq",
        minimum: 2,
    },
    CommandName {
        command: NamedCommand::Write,
        full: "write",
        minimum: 1,
    },
];

impl CommandName {
    /// Returns the declared name that `stem` abbreviates.
    ///
    /// The `stem` holds the typed name without its `!`. Every full name holds
    /// ASCII characters only, so a `stem` that starts one holds as many
    /// characters as bytes.
    fn resolve(stem: &str) -> Option<Self> {
        let mut matches = NAMES
            .into_iter()
            .filter(|name| name.full.starts_with(stem) && stem.len() >= name.minimum);
        let resolved = matches.next();
        debug_assert!(
            matches.next().is_none(),
            "the declared minimum of each name resolves one command; see docs/input-actions.md"
        );
        resolved
    }
}

impl CommandLineCommand {
    /// Returns every command name that the text `line` abbreviates, in
    /// ascending order.
    ///
    /// The command-line completion reads this function, so one new row of the
    /// name table serves the parser and the completion together.
    ///
    /// The function offers the full name of a command and never an intermediate
    /// abbreviation, so the list stays short and one cycle shows the whole name.
    /// It offers a name that [`CommandLineCommand::parse`] accepts only, so a
    /// command without a `!` variant never reaches the list with one.
    ///
    /// The function offers a `!` variant only while the text already holds the
    /// `!`. A `!` variant discards unsaved changes and asks nothing, so no cycle
    /// of a text without a `!` writes one.
    ///
    /// ```
    /// use kvim_input::CommandLineCommand;
    ///
    /// // The text holds no `!`, so the `!` variant of `quit` stays off the list.
    /// assert_eq!(CommandLineCommand::names_matching("q"), ["quit"]);
    /// // A `!` at the end of the text keeps the `!` variants alone.
    /// assert_eq!(CommandLineCommand::names_matching("q!"), ["quit!"]);
    /// // `w` starts both names, and neither one is a `!` variant.
    /// assert_eq!(CommandLineCommand::names_matching("w"), ["wq", "write"]);
    /// // `write` has no `!` variant, and a line number is no name.
    /// assert!(CommandLineCommand::names_matching("w!").is_empty());
    /// assert!(CommandLineCommand::names_matching("42").is_empty());
    /// ```
    pub fn names_matching(line: &str) -> Vec<String> {
        // A separator opens the argument of a command, and an argument is no
        // name. A leading blank names nothing either.
        if line.contains(char::is_whitespace) {
            return Vec::new();
        }
        let (stem, bang) = match line.strip_suffix('!') {
            // A bare `!` names no command.
            Some("") => return Vec::new(),
            Some(stem) => (stem, Bang::Present),
            None => (line, Bang::Absent),
        };
        let mut names = Vec::with_capacity(NAMES.len());
        for name in NAMES {
            if !name.full.starts_with(stem) {
                continue;
            }
            match bang {
                // The typed text holds no `!`, so the plain name is the whole
                // offer. A `!` variant discards unsaved changes and asks
                // nothing, and the user must type that `!` to reach it.
                Bang::Absent => {
                    debug_assert!(
                        Self::parse(name.full).is_ok(),
                        "the parser reads the same name table, so it accepts every full name"
                    );
                    names.push(name.full.to_owned());
                }
                // The typed `!` is a deliberate choice, so the completion
                // serves it. A command without a `!` variant still offers
                // nothing, because the parser rejects that name.
                Bang::Present => {
                    let discarding = format!("{}!", name.full);
                    if Self::parse(&discarding).is_ok() {
                        names.push(discarding);
                    }
                }
            }
        }
        names
    }

    /// Returns the command name of `line` and the path argument after it.
    ///
    /// The first blank ends the name and opens its argument, so a line without
    /// a blank names a command and carries no path. Only `:e[dit]` takes a
    /// path, so every other name returns `None`. The rule lives beside the name
    /// table, so the parser and the command-line completion can never disagree
    /// about which command takes a path.
    ///
    /// ```
    /// use kvim_input::CommandLineCommand;
    ///
    /// let argument = CommandLineCommand::path_argument("e src/ma").expect("`:e` takes a path");
    /// assert_eq!(argument.name, "e");
    /// assert_eq!(argument.typed, "src/ma");
    /// // The name keeps the abbreviation that the user typed.
    /// let full = CommandLineCommand::path_argument("edit ").expect("`:edit` takes a path");
    /// assert_eq!(full.name, "edit");
    /// assert_eq!(full.typed, "");
    /// // A line without a blank is a name, not a path.
    /// assert_eq!(CommandLineCommand::path_argument("edit"), None);
    /// // `:e!` reads the file of the focused window again and takes no path.
    /// assert_eq!(CommandLineCommand::path_argument("e! src"), None);
    /// // No other command takes a path.
    /// assert_eq!(CommandLineCommand::path_argument("w src"), None);
    /// ```
    #[must_use]
    pub fn path_argument(line: &str) -> Option<CommandPathArgument<'_>> {
        let separator = line.find(char::is_whitespace)?;
        let (name, argument) = line.split_at(separator);
        // A `!` variant of `edit` reloads the buffer, so it carries no path.
        if name.ends_with('!') {
            return None;
        }
        let resolved = CommandName::resolve(name)?;
        if resolved.command != NamedCommand::Edit {
            return None;
        }
        Some(CommandPathArgument {
            name,
            typed: argument.trim(),
        })
    }

    /// Parses one command line.
    ///
    /// The `line` holds the text after the `:` prompt character, because `:`
    /// opens the prompt and is not part of the command.
    ///
    /// # Errors
    ///
    /// Returns [`CommandLineError`] for an empty line, a line above
    /// [`COMMAND_LINE_CHARS_MAX`], a line number outside its range, and every
    /// line that matches no accepted command.
    ///
    /// ```
    /// use std::num::NonZeroU32;
    /// use std::path::PathBuf;
    ///
    /// use kvim_input::{CommandLineCommand, CommandLineError};
    ///
    /// assert_eq!(CommandLineCommand::parse("wq"), Ok(CommandLineCommand::WriteQuit));
    /// assert_eq!(
    ///     CommandLineCommand::parse("edit src/main.rs"),
    ///     Ok(CommandLineCommand::Edit(PathBuf::from("src/main.rs")))
    /// );
    /// // Every abbreviation down to the declared minimum names the command.
    /// assert_eq!(CommandLineCommand::parse("quit"), Ok(CommandLineCommand::Quit));
    /// assert_eq!(CommandLineCommand::parse("q"), Ok(CommandLineCommand::Quit));
    /// // `:e` without a path reads the file of the focused window again.
    /// assert_eq!(CommandLineCommand::parse("e"), Ok(CommandLineCommand::Reload));
    /// assert_eq!(CommandLineCommand::parse("edit!"), Ok(CommandLineCommand::ReloadDiscard));
    /// // `:l[og]` opens one snapshot of the editor log.
    /// assert_eq!(CommandLineCommand::parse("l"), Ok(CommandLineCommand::Log));
    /// assert_eq!(
    ///     CommandLineCommand::parse("42"),
    ///     Ok(CommandLineCommand::GoToLine(NonZeroU32::new(42).unwrap()))
    /// );
    /// assert_eq!(
    ///     CommandLineCommand::parse("wqa"),
    ///     Err(CommandLineError::Unknown),
    ///     "the parser accepts a declared abbreviation only"
    /// );
    /// ```
    pub fn parse(line: &str) -> Result<Self, CommandLineError> {
        let chars = line.chars().count();
        if chars > COMMAND_LINE_CHARS_MAX {
            return Err(CommandLineError::TooLong {
                chars,
                chars_max: COMMAND_LINE_CHARS_MAX,
            });
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Err(CommandLineError::Empty);
        }
        // The first separator ends the name and opens the argument.
        let (word, argument) = match trimmed.find(char::is_whitespace) {
            Some(index) => (&trimmed[..index], trimmed[index..].trim()),
            None => (trimmed, ""),
        };
        let (stem, bang) = match word.strip_suffix('!') {
            Some(stem) => (stem, Bang::Present),
            None => (word, Bang::Absent),
        };
        let Some(name) = CommandName::resolve(stem) else {
            if argument.is_empty() && word.bytes().all(|value| value.is_ascii_digit()) {
                return word
                    .parse::<u32>()
                    .ok()
                    .and_then(NonZeroU32::new)
                    .map(Self::GoToLine)
                    .ok_or(CommandLineError::LineNumberOutOfRange { max: u32::MAX });
            }
            return Err(CommandLineError::Unknown);
        };
        let argument = (!argument.is_empty()).then_some(argument);
        // Every combination that no arm names is a rejected line, so `:w!` and
        // `:wq!` stay unknown and only `edit` carries a path.
        let command = match (name.command, bang, argument) {
            (NamedCommand::Write, Bang::Absent, None) => Self::Write,
            (NamedCommand::Quit, Bang::Absent, None) => Self::Quit,
            (NamedCommand::Quit, Bang::Present, None) => Self::QuitDiscard,
            (NamedCommand::WriteQuit, Bang::Absent, None) => Self::WriteQuit,
            // `:e` reads the file of the focused window again, and `:e!` does
            // the same after it discards the unsaved changes of that buffer.
            (NamedCommand::Edit, Bang::Absent, None) => Self::Reload,
            (NamedCommand::Edit, Bang::Present, None) => Self::ReloadDiscard,
            (NamedCommand::Edit, Bang::Absent, Some(path)) => Self::Edit(PathBuf::from(path)),
            (NamedCommand::Log, Bang::Absent, None) => Self::Log,
            _ => return Err(CommandLineError::Unknown),
        };
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
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
            ("log", CommandLineCommand::Log),
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
        let cases: [(&str, &[&str]); 23] = [
            // A text without a `!` offers no `!` variant, so no cycle of that
            // text writes a command that discards unsaved changes.
            ("", &["edit", "log", "quit", "wq", "write"]),
            ("e", &["edit"]),
            ("edit", &["edit"]),
            ("l", &["log"]),
            ("log", &["log"]),
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
}
