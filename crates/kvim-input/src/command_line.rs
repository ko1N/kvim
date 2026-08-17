//! The command line parser for the fixed first-release command set.
//!
//! Kvim implements no Ex grammar. The parser accepts `:w`, `:q`, `:q!`, `:wq`,
//! `:e <path>`, and `:<number>` only, and rejects every other line. It never
//! guesses a command from a prefix.

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
    /// `:e` arrived without a file path.
    #[error("`:e` needs one file path")]
    EditWithoutPath,
    /// The line number was zero, or it did not fit an unsigned 32-bit value.
    #[error("the line number must be between 1 and {max}")]
    LineNumberOutOfRange {
        /// The largest accepted line number.
        max: u32,
    },
    /// The line matched no accepted command.
    #[error("the command line accepts :w, :q, :q!, :wq, :e <path>, and :<number> only")]
    Unknown,
}

impl CommandLineCommand {
    /// Parses one command line.
    ///
    /// The `line` holds the text after the `:` prompt character, because `:`
    /// opens the prompt and is not part of the command.
    ///
    /// # Errors
    ///
    /// Returns [`CommandLineError`] for an empty line, a line above
    /// [`COMMAND_LINE_CHARS_MAX`], `:e` without a path, a line number outside
    /// its range, and every line that matches no accepted command.
    ///
    /// ```
    /// use std::num::NonZeroU32;
    /// use std::path::PathBuf;
    ///
    /// use kvim_input::{CommandLineCommand, CommandLineError};
    ///
    /// assert_eq!(CommandLineCommand::parse("wq"), Ok(CommandLineCommand::WriteQuit));
    /// assert_eq!(
    ///     CommandLineCommand::parse("e src/main.rs"),
    ///     Ok(CommandLineCommand::Edit(PathBuf::from("src/main.rs")))
    /// );
    /// assert_eq!(
    ///     CommandLineCommand::parse("42"),
    ///     Ok(CommandLineCommand::GoToLine(NonZeroU32::new(42).unwrap()))
    /// );
    /// assert_eq!(
    ///     CommandLineCommand::parse("wqa"),
    ///     Err(CommandLineError::Unknown),
    ///     "the parser never guesses a command from a prefix"
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
        match trimmed {
            "w" => return Ok(Self::Write),
            "q" => return Ok(Self::Quit),
            "q!" => return Ok(Self::QuitDiscard),
            "wq" => return Ok(Self::WriteQuit),
            _ => {}
        }
        if let Some(rest) = trimmed.strip_prefix('e') {
            // `:e` needs a separator, so `:edit` stays an unknown command.
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                let path = rest.trim();
                if path.is_empty() {
                    return Err(CommandLineError::EditWithoutPath);
                }
                return Ok(Self::Edit(PathBuf::from(path)));
            }
            return Err(CommandLineError::Unknown);
        }
        if trimmed.bytes().all(|value| value.is_ascii_digit()) {
            return trimmed
                .parse::<u32>()
                .ok()
                .and_then(NonZeroU32::new)
                .map(Self::GoToLine)
                .ok_or(CommandLineError::LineNumberOutOfRange { max: u32::MAX });
        }
        Err(CommandLineError::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::path::PathBuf;

    use super::{COMMAND_LINE_CHARS_MAX, CommandLineCommand, CommandLineError};

    fn line(value: u32) -> CommandLineCommand {
        CommandLineCommand::GoToLine(NonZeroU32::new(value).expect("the test line is not zero"))
    }

    #[test]
    fn the_fixed_command_set_parses() {
        let cases = [
            ("w", CommandLineCommand::Write),
            ("q", CommandLineCommand::Quit),
            ("q!", CommandLineCommand::QuitDiscard),
            ("wq", CommandLineCommand::WriteQuit),
            ("  w  ", CommandLineCommand::Write),
            (
                "e src/main.rs",
                CommandLineCommand::Edit(PathBuf::from("src/main.rs")),
            ),
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
            ("e", CommandLineError::EditWithoutPath),
            ("e   ", CommandLineError::EditWithoutPath),
            (
                "0",
                CommandLineError::LineNumberOutOfRange { max: u32::MAX },
            ),
            (
                "4294967296",
                CommandLineError::LineNumberOutOfRange { max: u32::MAX },
            ),
            ("edit foo", CommandLineError::Unknown),
            ("wqa", CommandLineError::Unknown),
            ("q!!", CommandLineError::Unknown),
            ("W", CommandLineError::Unknown),
            (":w", CommandLineError::Unknown),
            ("s/a/b/", CommandLineError::Unknown),
            ("12a", CommandLineError::Unknown),
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
