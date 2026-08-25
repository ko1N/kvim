//! Behavior tests for operators, registers, paste, selection edits, and repeat.

use std::num::{NonZeroU8, NonZeroU16, NonZeroU32};

use crate::{
    AutoIndent, CommandOutcome, EditContext, EditingState, Operator, RegisterShape, RegisterValue,
    Registers, Selection, Viewport, WindowState,
};
use kvim_core::{LineEnding, TextBuffer};
use kvim_input::{Command, Mode};
use kvim_settings::{EditorSettings, FileSettings};

/// One buffer, one register file, one editing state, and one viewport.
struct Session {
    buffer: TextBuffer,
    settings: EditorSettings,
    /// The width that the language adapter of the buffer declares.
    ///
    /// The session of the terminal reads it from the adapter. A test buffer
    /// has no language, so the default is `None`.
    language_indent_width: Option<NonZeroU8>,
    registers: Registers,
    state: EditingState,
    view: WindowState,
}

impl Session {
    fn new(text: &str) -> Self {
        let buffer =
            TextBuffer::from_text(text, &FileSettings::default()).expect("the test text is small");
        Self {
            buffer,
            settings: EditorSettings::default(),
            language_indent_width: None,
            registers: Registers::default(),
            state: EditingState::new(),
            view: WindowState::new(Viewport::new(
                NonZeroU16::new(20).expect("the literal 20 is not zero"),
                NonZeroU16::new(80).expect("the literal 80 is not zero"),
            )),
        }
    }

    fn apply(&mut self, command: Command, count: Option<NonZeroU32>) -> CommandOutcome {
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: self.language_indent_width,
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state
            .apply(&mut context, &mut self.view, command, count)
    }

    fn apply_with_register(
        &mut self,
        command: Command,
        count: Option<NonZeroU32>,
        register: Option<char>,
    ) -> CommandOutcome {
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: self.language_indent_width,
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state
            .apply_with_register(&mut context, &mut self.view, command, count, register)
    }

    fn run(&mut self, commands: &[Command]) -> CommandOutcome {
        let mut outcome = CommandOutcome::Applied;
        for command in commands {
            outcome = self.apply(*command, None);
        }
        outcome
    }

    fn insert_text(&mut self, text: &str) -> CommandOutcome {
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: self.language_indent_width,
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state.insert_text(&mut context, &mut self.view, text)
    }

    fn insert_line_break(&mut self) -> CommandOutcome {
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: self.language_indent_width,
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state.insert_line_break(&mut context, &mut self.view)
    }

    fn delete_backward(&mut self) -> CommandOutcome {
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: self.language_indent_width,
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state.delete_backward(&mut context, &mut self.view)
    }

    fn delete_word_backward(&mut self) -> CommandOutcome {
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: self.language_indent_width,
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state
            .delete_word_backward(&mut context, &mut self.view)
    }

    /// Enters Insert mode, which `Enter` and `Backspace` both need.
    fn enter_insert(&mut self) {
        self.state
            .enter_mode(&self.buffer, &mut self.view, Mode::Insert);
    }

    /// Applies one command with the automatic indent of a language adapter.
    fn apply_indented(&mut self, command: Command, auto: AutoIndent) -> CommandOutcome {
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: self.language_indent_width,
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state
            .apply_indented(&mut context, &mut self.view, command, None, auto)
    }

    /// Inserts one line break with the automatic indent of a language adapter.
    fn insert_line_break_indented(&mut self, auto: AutoIndent) -> CommandOutcome {
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: self.language_indent_width,
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state
            .insert_line_break_indented(&mut context, &mut self.view, auto)
    }

    /// Toggles the line comment with the token of a language adapter.
    fn toggle_comment_with(&mut self, comment: Option<&str>) -> CommandOutcome {
        let mut context = EditContext {
            buffer: &mut self.buffer,
            settings: &self.settings,
            search: None,
            language_indent_width: self.language_indent_width,
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state
            .toggle_comment(&mut context, &mut self.view, comment)
    }

    /// Toggles the line comment of a buffer that an adapter serves.
    fn toggle_comment(&mut self) -> CommandOutcome {
        self.toggle_comment_with(Some("//"))
    }

    fn text(&self) -> String {
        self.buffer.to_string()
    }

    fn position(&self) -> (usize, usize) {
        (
            self.view.cursor().line().get(),
            self.view.cursor().column().get(),
        )
    }

    fn register(&self) -> Option<(&str, RegisterShape)> {
        self.registers
            .unnamed()
            .map(|value| (value.text(), value.shape()))
    }

    fn selection(&self) -> Option<Selection> {
        self.state.selection(&self.buffer, &self.view)
    }
}

fn count(value: u32) -> Option<NonZeroU32> {
    Some(NonZeroU32::new(value).expect("the test count is not zero"))
}

/// Moves the cursor to one line and one column with plain motions.
fn place(session: &mut Session, line: usize, column: usize) {
    session.apply(Command::MoveFirstLine, None);
    if line > 0 {
        session.apply(Command::MoveDown, count(line as u32));
    }
    session.apply(Command::MoveFirstColumn, None);
    if column > 0 {
        session.apply(Command::MoveRight, count(column as u32));
    }
}

const OPERATOR_KEYS: &[(Command, Operator)] = &[
    (Command::DeleteOverMotion, Operator::Delete),
    (Command::ChangeOverMotion, Operator::Change),
    (Command::YankOverMotion, Operator::Yank),
];

#[test]
fn an_operator_waits_for_a_motion_and_cannot_apply_alone() {
    for (key, operator) in OPERATOR_KEYS {
        let mut session = Session::new("alpha beta\n");
        assert_eq!(session.apply(*key, None), CommandOutcome::OperatorPending);
        assert_eq!(session.state.pending_operator(), Some(*operator));
        assert_eq!(session.text(), "alpha beta\n", "{key}");
        assert_eq!(session.register(), None, "{key}");
    }
}

#[test]
fn an_operator_over_a_command_that_is_no_motion_changes_nothing() {
    let mut session = Session::new("alpha beta\n");
    session.apply(Command::DeleteOverMotion, None);
    assert_eq!(
        session.apply(Command::SaveBuffer, None),
        CommandOutcome::OperatorAborted
    );
    assert_eq!(session.text(), "alpha beta\n");
    assert_eq!(session.state.pending_operator(), None);
}

#[test]
fn every_operator_over_every_motion_produces_its_range() {
    // The expected text, the expected register, and the cursor after a delete.
    let expected: &[(Command, &str, &str)] = &[
        (Command::MoveNextWordStart, "beta\nsecond\n", "alpha "),
        (Command::MoveNextWordEnd, " beta\nsecond\n", "alpha"),
        (Command::MoveRight, "lpha beta\nsecond\n", "a"),
        (Command::MoveLineEnd, "\nsecond\n", "alpha beta"),
        // A delete of every line keeps the one empty line that a buffer holds.
        (Command::MoveDown, "\n", "alpha beta\nsecond\n"),
    ];

    for (motion, remaining, yanked) in expected {
        let mut delete = Session::new("alpha beta\nsecond\n");
        delete.apply(Command::DeleteOverMotion, None);
        assert_eq!(delete.apply(*motion, None), CommandOutcome::Changed);
        assert_eq!(delete.text(), *remaining, "delete over {motion}");
        assert_eq!(delete.register().map(|value| value.0), Some(*yanked));
        assert_eq!(delete.state.mode(), Mode::Normal);

        let mut change = Session::new("alpha beta\nsecond\n");
        change.apply(Command::ChangeOverMotion, None);
        assert_eq!(change.apply(*motion, None), CommandOutcome::Changed);
        assert_eq!(change.state.mode(), Mode::Insert, "change over {motion}");

        let mut yank = Session::new("alpha beta\nsecond\n");
        yank.apply(Command::YankOverMotion, None);
        yank.apply(*motion, None);
        assert_eq!(
            yank.text(),
            "alpha beta\nsecond\n",
            "a yank changes no text over {motion}"
        );
        assert_eq!(yank.register().map(|value| value.0), Some(*yanked));
    }
}

#[test]
fn a_backward_motion_deletes_before_the_cursor() {
    let mut session = Session::new("alpha beta\n");
    place(&mut session, 0, 6);
    session.apply(Command::DeleteOverMotion, None);
    assert_eq!(
        session.apply(Command::MovePreviousWordStart, None),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "beta\n");
    assert_eq!(session.position(), (0, 0));

    let mut session = Session::new("alpha beta\n");
    place(&mut session, 0, 4);
    session.apply(Command::DeleteOverMotion, None);
    session.apply(Command::MoveFirstColumn, None);
    assert_eq!(session.text(), "a beta\n");
}

#[test]
fn an_operator_over_the_bracket_motion_takes_both_brackets() {
    // The start column and the text that remains after `d%`. The motion is
    // characterwise and inclusive, so the matched bracket belongs to the range
    // in both directions.
    let expected: &[(usize, &str, &str)] = &[
        (4, "call\n", "(alpha)"),
        (10, "call\n", "(alpha)"),
        // From a position before the pair the range starts at the cursor.
        (0, "\n", "call(alpha)"),
    ];
    for &(column, remaining, yanked) in expected {
        let mut session = Session::new("call(alpha)\n");
        place(&mut session, 0, column);
        session.apply(Command::DeleteOverMotion, None);
        assert_eq!(
            session.apply(Command::MoveMatchingBracket, None),
            CommandOutcome::Changed,
            "d% from column {column}"
        );
        assert_eq!(session.text(), remaining, "d% from column {column}");
        assert_eq!(session.register().map(|value| value.0), Some(yanked));
    }
}

#[test]
fn an_operator_without_a_bracket_match_changes_nothing() {
    let mut session = Session::new("call(alpha\n");
    place(&mut session, 0, 4);
    session.apply(Command::DeleteOverMotion, None);
    assert_eq!(
        session.apply(Command::MoveMatchingBracket, None),
        CommandOutcome::OperatorAborted
    );
    assert_eq!(session.text(), "call(alpha\n");
    assert_eq!(session.state.pending_operator(), None);
}

#[test]
fn the_two_line_end_operators_differ_on_a_line_of_trailing_blanks() {
    // `$` reaches the last column, and `g_` the last visible character, so the
    // two operators keep a different remainder.
    let expected: &[(Command, &str)] = &[
        (Command::MoveLineEnd, "  \n"),
        (Command::MoveLastNonBlank, "    \n"),
    ];
    for &(motion, remaining) in expected {
        let mut session = Session::new("  alpha  \n");
        place(&mut session, 0, 2);
        session.apply(Command::DeleteOverMotion, None);
        assert_eq!(session.apply(motion, None), CommandOutcome::Changed);
        assert_eq!(session.text(), remaining, "delete over {motion}");
    }
}

#[test]
fn a_word_delete_at_the_line_end_keeps_the_line_break() {
    let mut session = Session::new("one two\nthree\n");
    place(&mut session, 0, 4);
    session.apply(Command::DeleteOverMotion, None);
    session.apply(Command::MoveNextWordStart, None);
    assert_eq!(session.text(), "one \nthree\n");
}

#[test]
fn the_operator_count_multiplies_the_motion_count() {
    let mut session = Session::new("one two three four five six seven\n");
    session.apply(Command::DeleteOverMotion, count(2));
    assert_eq!(
        session.apply(Command::MoveNextWordStart, count(3)),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "seven\n", "2d3w deletes six words");

    // The same product arrives through one count only.
    let mut single = Session::new("one two three four five six seven\n");
    single.apply(Command::DeleteOverMotion, None);
    single.apply(Command::MoveNextWordStart, count(6));
    assert_eq!(single.text(), "seven\n");
}

#[test]
fn a_count_before_a_line_motion_keeps_naming_a_line() {
    let mut session = Session::new("one\ntwo\nthree\nfour\n");
    session.apply(Command::DeleteOverMotion, None);
    assert_eq!(
        session.apply(Command::MoveLastLine, count(3)),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "four\n", "d3G deletes to line three");
}

#[test]
fn a_repeated_operator_key_deletes_changes_and_yanks_lines() {
    let mut delete = Session::new("one\ntwo\nthree\n");
    assert_eq!(
        delete.run(&[Command::DeleteOverMotion, Command::DeleteOverMotion]),
        CommandOutcome::Changed
    );
    assert_eq!(delete.text(), "two\nthree\n");
    assert_eq!(delete.register(), Some(("one\n", RegisterShape::Linewise)));

    let mut yank = Session::new("one\ntwo\nthree\n");
    yank.run(&[Command::YankOverMotion, Command::YankOverMotion]);
    assert_eq!(yank.text(), "one\ntwo\nthree\n");
    assert_eq!(yank.register(), Some(("one\n", RegisterShape::Linewise)));

    let mut change = Session::new("    indented\nnext\n");
    change.run(&[Command::ChangeOverMotion, Command::ChangeOverMotion]);
    assert_eq!(
        change.text(),
        "    \nnext\n",
        "a linewise change keeps the line and its indent"
    );
    assert_eq!(change.state.mode(), Mode::Insert);
    assert_eq!(change.position(), (0, 4));
}

#[test]
fn a_count_before_the_repeated_operator_key_covers_several_lines() {
    let mut session = Session::new("one\ntwo\nthree\nfour\n");
    session.apply(Command::DeleteOverMotion, count(2));
    session.apply(Command::DeleteOverMotion, None);
    assert_eq!(session.text(), "three\nfour\n");
    assert_eq!(
        session.register(),
        Some(("one\ntwo\n", RegisterShape::Linewise))
    );

    // The counts multiply for the linewise form as well.
    let mut both = Session::new("one\ntwo\nthree\nfour\nfive\n");
    both.apply(Command::DeleteOverMotion, count(2));
    both.apply(Command::DeleteOverMotion, count(2));
    assert_eq!(both.text(), "five\n");
}

#[test]
fn a_line_delete_on_the_last_line_keeps_the_lines_above_terminated() {
    // The delete removes the line ending of the removed line, whatever the file
    // held at its end, so the line above it keeps its own terminator.
    for text in ["one\ntwo", "one\ntwo\n"] {
        let mut session = Session::new(text);
        place(&mut session, 1, 0);
        session.run(&[Command::DeleteOverMotion, Command::DeleteOverMotion]);
        assert_eq!(session.text(), "one\n", "{text:?}");
        assert_eq!(session.register(), Some(("two\n", RegisterShape::Linewise)));
    }
}

#[test]
fn the_end_of_line_and_line_forms_reach_their_own_ranges() {
    let mut delete = Session::new("alpha\nbeta\n");
    place(&mut delete, 0, 2);
    assert_eq!(
        delete.apply(Command::DeleteToLineEnd, None),
        CommandOutcome::Changed
    );
    assert_eq!(delete.text(), "al\nbeta\n");
    assert_eq!(
        delete.register(),
        Some(("pha", RegisterShape::Characterwise))
    );

    let mut change = Session::new("alpha\nbeta\n");
    place(&mut change, 0, 2);
    change.apply(Command::ChangeToLineEnd, None);
    assert_eq!(change.text(), "al\nbeta\n");
    assert_eq!(change.state.mode(), Mode::Insert);

    let mut yank = Session::new("alpha\nbeta\n");
    place(&mut yank, 0, 2);
    assert_eq!(yank.apply(Command::YankLine, None), CommandOutcome::Applied);
    assert_eq!(yank.text(), "alpha\nbeta\n");
    assert_eq!(yank.register(), Some(("alpha\n", RegisterShape::Linewise)));
}

#[test]
fn every_insert_entry_command_places_the_cursor_for_the_change() {
    let expected: &[(Command, (usize, usize))] = &[
        (Command::InsertBeforeCursor, (0, 2)),
        (Command::InsertAtFirstNonBlank, (0, 4)),
        (Command::InsertAfterCursor, (0, 3)),
        (Command::InsertAtLineEnd, (0, 9)),
    ];
    for (command, place_at) in expected {
        let mut session = Session::new("    alpha\nnext\n");
        place(&mut session, 0, 2);
        assert_eq!(session.apply(*command, None), CommandOutcome::Applied);
        assert_eq!(session.state.mode(), Mode::Insert, "{command}");
        assert_eq!(session.position(), *place_at, "{command}");
        assert_eq!(session.text(), "    alpha\nnext\n", "{command}");
    }
}

#[test]
fn open_line_below_copies_the_indent_of_the_previous_non_empty_line() {
    let mut session = Session::new("    alpha\n");
    assert_eq!(
        session.apply(Command::OpenLineBelow, None),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "    alpha\n    \n");
    assert_eq!(session.position(), (1, 4));
    assert_eq!(session.state.mode(), Mode::Insert);

    // The new line and its indent are one transaction.
    session.apply(Command::Undo, None);
    assert_eq!(session.text(), "    alpha\n");
}

#[test]
fn open_line_above_copies_the_indent_of_the_previous_non_empty_line() {
    let mut session = Session::new("    alpha\n\n");
    place(&mut session, 1, 0);
    assert_eq!(
        session.apply(Command::OpenLineAbove, None),
        CommandOutcome::Changed
    );
    assert_eq!(
        session.text(),
        "    alpha\n    \n\n",
        "the empty cursor line falls back to the line above it"
    );
    assert_eq!(session.position(), (1, 4));

    session.apply(Command::Undo, None);
    assert_eq!(session.text(), "    alpha\n\n");
}

#[test]
fn a_line_break_copies_the_indent_of_the_previous_non_empty_line() {
    // The cursor line carries the indent.
    let mut indented = Session::new("    alpha\n");
    place(&mut indented, 0, 7);
    indented.enter_insert();
    assert_eq!(indented.insert_line_break(), CommandOutcome::Changed);
    assert_eq!(indented.text(), "    alp\n    ha\n");
    assert_eq!(indented.position(), (1, 4));
    assert_eq!(indented.state.mode(), Mode::Insert);

    // The line break and its indent are one transaction.
    indented.apply(Command::Undo, None);
    assert_eq!(indented.text(), "    alpha\n");

    // An empty cursor line falls back to the line above it.
    let mut empty = Session::new("    alpha\n\n");
    place(&mut empty, 1, 0);
    empty.enter_insert();
    assert_eq!(empty.insert_line_break(), CommandOutcome::Changed);
    assert_eq!(empty.text(), "    alpha\n\n    \n");
    assert_eq!(empty.position(), (2, 4));

    // Inside the leading whitespace the remaining indent moves down behind the
    // automatic indent.
    let mut leading = Session::new("    alpha\n");
    place(&mut leading, 0, 2);
    leading.enter_insert();
    assert_eq!(leading.insert_line_break(), CommandOutcome::Changed);
    assert_eq!(leading.text(), "  \n      alpha\n");
    assert_eq!(leading.position(), (1, 4));
}

#[test]
fn a_backward_delete_removes_one_character_and_joins_lines_at_column_zero() {
    let mut session = Session::new("alpha\nbeta\n");
    place(&mut session, 0, 3);
    session.enter_insert();
    assert_eq!(session.delete_backward(), CommandOutcome::Changed);
    assert_eq!(session.text(), "alha\nbeta\n");
    assert_eq!(session.position(), (0, 2));

    // One undo reverses the delete.
    session.apply(Command::Undo, None);
    assert_eq!(session.text(), "alpha\nbeta\n");

    // Column zero joins the cursor line with the line above it.
    let mut join = Session::new("alpha\nbeta\n");
    place(&mut join, 1, 0);
    join.enter_insert();
    assert_eq!(join.delete_backward(), CommandOutcome::Changed);
    assert_eq!(join.text(), "alphabeta\n");
    assert_eq!(join.position(), (0, 5));

    // The start of the buffer holds no character before the cursor.
    let mut start = Session::new("alpha\n");
    start.enter_insert();
    assert_eq!(start.delete_backward(), CommandOutcome::Applied);
    assert_eq!(start.text(), "alpha\n");
    assert_eq!(start.position(), (0, 0));
}

#[test]
fn a_backward_delete_removes_one_whole_grapheme_cluster() {
    // `e` and a combining acute are two characters and one cluster, so one
    // backward delete removes both and never leaves the mark alone.
    let mut session = Session::new("ae\u{301}b\n");
    place(&mut session, 0, 3);
    session.enter_insert();
    assert_eq!(session.delete_backward(), CommandOutcome::Changed);
    assert_eq!(session.text(), "ab\n");
    assert_eq!(session.position(), (0, 1));

    // One undo reverses the whole cluster.
    session.apply(Command::Undo, None);
    assert_eq!(session.text(), "ae\u{301}b\n");
}

#[test]
fn a_backward_delete_at_column_zero_removes_a_complete_crlf_ending() {
    let mut session = Session::new("alpha\r\nbeta\r\n");
    assert_eq!(session.buffer.line_ending(), LineEnding::Crlf);
    place(&mut session, 1, 0);
    session.enter_insert();
    assert_eq!(session.delete_backward(), CommandOutcome::Changed);
    assert_eq!(session.text(), "alphabeta\r\n");
}

#[test]
fn a_backward_word_delete_removes_the_word_and_the_blanks_before_the_cursor() {
    // `A` enters Insert mode after the last character, where `Ctrl-W` acts.
    let mut session = Session::new("alpha beta\n");
    session.apply(Command::InsertAtLineEnd, None);
    assert_eq!(session.position(), (0, 10));
    assert_eq!(session.delete_word_backward(), CommandOutcome::Changed);
    assert_eq!(session.text(), "alpha \n");
    assert_eq!(session.position(), (0, 6));

    // One undo reverses the complete delete.
    session.apply(Command::Undo, None);
    assert_eq!(session.text(), "alpha beta\n");

    // The blanks between the cursor and the word go with the word, because `b`
    // walks over them.
    let mut blanks = Session::new("alpha beta   \n");
    blanks.apply(Command::InsertAtLineEnd, None);
    assert_eq!(blanks.position(), (0, 13));
    assert_eq!(blanks.delete_word_backward(), CommandOutcome::Changed);
    assert_eq!(blanks.text(), "alpha \n");
    assert_eq!(blanks.position(), (0, 6));

    // The delete crosses a line boundary, exactly as `b` does.
    let mut joined = Session::new("alpha\nbeta\n");
    place(&mut joined, 1, 0);
    joined.enter_insert();
    assert_eq!(joined.delete_word_backward(), CommandOutcome::Changed);
    assert_eq!(joined.text(), "beta\n");
    assert_eq!(joined.position(), (0, 0));

    // The start of the buffer holds no word before the cursor.
    let mut start = Session::new("alpha\n");
    start.enter_insert();
    assert_eq!(start.delete_word_backward(), CommandOutcome::Applied);
    assert_eq!(start.text(), "alpha\n");
    assert_eq!(start.position(), (0, 0));
}

#[test]
fn an_inserted_line_feed_follows_the_line_ending_of_the_buffer() {
    let mut crlf = Session::new("alpha\r\n");
    assert_eq!(crlf.buffer.line_ending(), LineEnding::Crlf);
    place(&mut crlf, 0, 0);
    crlf.enter_insert();
    assert_eq!(crlf.insert_text("one\ntwo"), CommandOutcome::Changed);
    assert_eq!(crlf.text(), "one\r\ntwoalpha\r\n");
    assert_eq!(
        crlf.position(),
        (1, 3),
        "the cursor follows the last inserted character"
    );

    let mut lf = Session::new("alpha\n");
    assert_eq!(lf.buffer.line_ending(), LineEnding::Lf);
    place(&mut lf, 0, 0);
    lf.enter_insert();
    assert_eq!(lf.insert_text("one\ntwo"), CommandOutcome::Changed);
    assert_eq!(lf.text(), "one\ntwoalpha\n");
    assert_eq!(lf.position(), (1, 3));
}

/// Selects the rectangle from line 0, column 2, to line 2, column 4.
fn select_block(session: &mut Session) {
    place(session, 0, 2);
    session.apply(Command::EnterVisualBlock, None);
    session.apply(Command::MoveRight, count(2));
    session.apply(Command::MoveDown, count(2));
}

#[test]
fn a_block_operator_skips_a_line_shorter_than_the_left_edge() {
    let mut delete = Session::new("abcdef\na\nabcdef\n");
    select_block(&mut delete);
    assert_eq!(
        delete.apply(Command::DeleteSelection, None),
        CommandOutcome::Changed
    );
    assert_eq!(delete.text(), "abf\na\nabf\n");
    assert_eq!(
        delete.register(),
        Some(("cde\n\ncde", RegisterShape::Blockwise)),
        "a skipped line keeps one empty register line"
    );
    assert_eq!(delete.state.mode(), Mode::Normal);
    assert_eq!(delete.position(), (0, 2));

    let mut yank = Session::new("abcdef\na\nabcdef\n");
    select_block(&mut yank);
    yank.apply(Command::YankSelection, None);
    assert_eq!(yank.text(), "abcdef\na\nabcdef\n");
    assert_eq!(
        yank.register(),
        Some(("cde\n\ncde", RegisterShape::Blockwise))
    );

    let mut change = Session::new("abcdef\na\nabcdef\n");
    select_block(&mut change);
    change.apply(Command::ChangeSelection, None);
    assert_eq!(change.text(), "abf\na\nabf\n");
    assert_eq!(change.state.mode(), Mode::Insert);
}

#[test]
fn a_line_that_ends_inside_the_block_gives_its_remaining_characters() {
    let mut session = Session::new("abcdef\nabc\nabcdef\n");
    select_block(&mut session);
    session.apply(Command::DeleteSelection, None);
    assert_eq!(session.text(), "abf\nab\nabf\n");
    assert_eq!(
        session.register(),
        Some(("cde\nc\ncde", RegisterShape::Blockwise))
    );
}

#[test]
fn a_block_insert_writes_every_reached_line_as_one_transaction() {
    for (command, expected) in [
        (Command::BlockInsertBefore, "ab>>cdef\na\nab>>cdef\n"),
        (Command::BlockInsertAfter, "abcde>>f\na\nabcde>>f\n"),
    ] {
        let mut session = Session::new("abcdef\na\nabcdef\n");
        select_block(&mut session);
        assert_eq!(session.apply(command, None), CommandOutcome::Applied);
        assert_eq!(session.state.mode(), Mode::Insert, "{command}");
        assert_eq!(session.insert_text(">>"), CommandOutcome::Changed);
        assert_eq!(session.text(), expected, "{command}");

        // One undo reverses the whole block.
        session.apply(Command::Undo, None);
        assert_eq!(session.text(), "abcdef\na\nabcdef\n", "{command}");
    }
}

#[test]
fn a_selection_operator_covers_the_characterwise_and_linewise_shapes() {
    let mut characters = Session::new("alpha beta\n");
    characters.apply(Command::EnterVisual, None);
    characters.apply(Command::MoveRight, count(4));
    assert_eq!(
        characters.apply(Command::DeleteSelection, None),
        CommandOutcome::Changed
    );
    assert_eq!(characters.text(), " beta\n");
    assert_eq!(
        characters.register(),
        Some(("alpha", RegisterShape::Characterwise))
    );

    let mut lines = Session::new("one\ntwo\nthree\n");
    lines.apply(Command::EnterVisualLine, None);
    lines.apply(Command::MoveDown, None);
    assert_eq!(
        lines.apply(Command::DeleteSelection, None),
        CommandOutcome::Changed
    );
    assert_eq!(lines.text(), "three\n");
    assert_eq!(
        lines.register(),
        Some(("one\ntwo\n", RegisterShape::Linewise))
    );
}

#[test]
fn moving_the_selection_reindents_the_moved_lines_and_keeps_the_selection() {
    let mut down = Session::new("alpha\n    beta\ngamma\n");
    down.apply(Command::EnterVisualLine, None);
    assert_eq!(
        down.apply(Command::MoveSelectionDown, None),
        CommandOutcome::Changed
    );
    assert_eq!(
        down.text(),
        "    beta\n    alpha\ngamma\n",
        "the moved line takes the indent of the previous non-empty line"
    );
    assert_eq!(down.state.mode(), Mode::VisualLine);
    assert_eq!(
        down.selection(),
        Some(Selection::Linewise {
            first: down.buffer.line_index(1).expect("the line exists"),
            last: down.buffer.line_index(1).expect("the line exists"),
        })
    );

    let mut up = Session::new("one\ntwo\nthree\n");
    place(&mut up, 1, 0);
    up.apply(Command::EnterVisualLine, None);
    assert_eq!(
        up.apply(Command::MoveSelectionUp, None),
        CommandOutcome::Changed
    );
    assert_eq!(up.text(), "two\none\nthree\n");
    assert_eq!(up.position(), (0, 0));
    assert_eq!(up.state.mode(), Mode::VisualLine);
}

/// The buffer of the reported selection-move bug.
const SCOPE_TEXT: &str = "struct Foo {\n    text: String,\n    shape: Shape,\n}\n";

/// Selects the two field lines of [`SCOPE_TEXT`] in Visual Line mode.
fn select_fields(session: &mut Session) {
    place(session, 1, 0);
    session.apply(Command::EnterVisualLine, None);
    session.apply(Command::MoveDown, None);
}

#[test]
fn a_selection_move_out_of_a_scope_and_back_follows_the_syntax_indent() {
    let mut session = Session::new(SCOPE_TEXT);
    select_fields(&mut session);

    // The block leaves the struct body, where the adapter reports no level.
    assert_eq!(
        session.apply_indented(Command::MoveSelectionDown, AutoIndent::Levels(0)),
        CommandOutcome::Changed
    );
    assert_eq!(
        session.text(),
        "struct Foo {\n}\ntext: String,\nshape: Shape,\n",
        "a block that leaves a scope loses one level"
    );
    assert_eq!(session.state.mode(), Mode::VisualLine);

    // The block returns into the struct body, where the adapter reports one
    // level. This direction kept the block at column zero before the fix.
    assert_eq!(
        session.apply_indented(Command::MoveSelectionUp, AutoIndent::Levels(1)),
        CommandOutcome::Changed
    );
    assert_eq!(
        session.text(),
        SCOPE_TEXT,
        "a block that enters a scope gains one level"
    );
    assert_eq!(session.state.mode(), Mode::VisualLine);
    assert_eq!(
        session.selection(),
        Some(Selection::Linewise {
            first: session.buffer.line_index(1).expect("the line exists"),
            last: session.buffer.line_index(2).expect("the line exists"),
        }),
        "the selection follows the moved lines"
    );
}

#[test]
fn a_selection_move_without_a_parse_result_copies_the_previous_line() {
    let mut session = Session::new("struct Foo {\n}\ntext: String,\nshape: Shape,\n");
    place(&mut session, 2, 0);
    session.apply(Command::EnterVisualLine, None);
    session.apply(Command::MoveDown, None);

    // Without a parse result the block copies the indent of the line above its
    // new position, which is `struct Foo {` at column zero.
    assert_eq!(
        session.apply_indented(Command::MoveSelectionUp, AutoIndent::PreviousLine),
        CommandOutcome::Changed
    );
    assert_eq!(
        session.text(),
        "struct Foo {\ntext: String,\nshape: Shape,\n}\n"
    );
}

#[test]
fn a_selection_move_keeps_an_empty_line_inside_the_block() {
    let mut session = Session::new("struct Foo {\n    text: String,\n\n    shape: Shape,\n}\n");
    place(&mut session, 1, 0);
    session.apply(Command::EnterVisualLine, None);
    session.apply(Command::MoveDown, count(2));

    assert_eq!(
        session.apply_indented(Command::MoveSelectionDown, AutoIndent::Levels(0)),
        CommandOutcome::Changed
    );
    assert_eq!(
        session.text(),
        "struct Foo {\n}\ntext: String,\n\nshape: Shape,\n",
        "an empty line inside the block keeps its shape"
    );

    assert_eq!(
        session.apply_indented(Command::MoveSelectionUp, AutoIndent::Levels(1)),
        CommandOutcome::Changed
    );
    assert_eq!(
        session.text(),
        "struct Foo {\n    text: String,\n\n    shape: Shape,\n}\n"
    );
}

#[test]
fn a_selection_move_at_column_zero_keeps_the_column() {
    let mut session = Session::new("fn one() {}\nfn two() {}\nfn three() {}\n");
    session.apply(Command::EnterVisualLine, None);

    // The block stays at the top level, so the level count stays zero.
    assert_eq!(
        session.apply_indented(Command::MoveSelectionDown, AutoIndent::Levels(0)),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "fn two() {}\nfn one() {}\nfn three() {}\n");
}

#[test]
fn one_undo_reverses_a_selection_move_and_its_reindent() {
    let mut session = Session::new(SCOPE_TEXT);
    select_fields(&mut session);
    session.apply_indented(Command::MoveSelectionDown, AutoIndent::Levels(0));
    session.apply(Command::ReturnToNormal, None);

    assert_eq!(session.apply(Command::Undo, None), CommandOutcome::Changed);
    assert_eq!(
        session.text(),
        SCOPE_TEXT,
        "the move and the reindent are one transaction"
    );
}

#[test]
fn a_selection_at_a_buffer_limit_moves_nowhere() {
    let mut session = Session::new("one\ntwo\n");
    session.apply(Command::EnterVisualLine, None);
    assert_eq!(
        session.apply(Command::MoveSelectionUp, None),
        CommandOutcome::Applied
    );
    assert_eq!(session.text(), "one\ntwo\n");
}

#[test]
fn shifting_the_selection_moves_one_shift_width_and_keeps_the_selection() {
    let mut session = Session::new("one\ntwo\n\n");
    session.apply(Command::EnterVisualLine, None);
    session.apply(Command::MoveDown, count(2));
    assert_eq!(
        session.apply(Command::ShiftSelectionRight, None),
        CommandOutcome::Changed
    );
    assert_eq!(
        session.text(),
        "    one\n    two\n\n",
        "an empty line keeps its shape"
    );
    assert_eq!(session.state.mode(), Mode::VisualLine);

    assert_eq!(
        session.apply(Command::ShiftSelectionLeft, None),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "one\ntwo\n\n");
    assert_eq!(session.state.mode(), Mode::VisualLine);
    assert!(
        matches!(session.selection(), Some(Selection::Linewise { first, last }) if first.get() == 0 && last.get() == 2)
    );
}

#[test]
fn a_left_shift_stops_at_the_first_column() {
    let mut session = Session::new("  one\n");
    session.apply(Command::EnterVisualLine, None);
    session.apply(Command::ShiftSelectionLeft, None);
    assert_eq!(session.text(), "one\n");
}

fn set_register(session: &mut Session, value: RegisterValue) {
    session.registers.set_unnamed(value);
}

#[test]
fn paste_before_and_after_follows_the_characterwise_shape() {
    let mut after = Session::new("abc\n");
    set_register(&mut after, RegisterValue::characterwise("XY"));
    place(&mut after, 0, 1);
    assert_eq!(
        after.apply(Command::PasteAfter, None),
        CommandOutcome::Changed
    );
    assert_eq!(after.text(), "abXYc\n");
    assert_eq!(
        after.position(),
        (0, 3),
        "the cursor stops on the last character"
    );

    let mut before = Session::new("abc\n");
    set_register(&mut before, RegisterValue::characterwise("XY"));
    place(&mut before, 0, 1);
    before.apply(Command::PasteBefore, None);
    assert_eq!(before.text(), "aXYbc\n");
}

#[test]
fn paste_before_and_after_follows_the_linewise_shape() {
    let mut after = Session::new("one\ntwo\n");
    set_register(&mut after, RegisterValue::linewise("new", LineEnding::Lf));
    after.apply(Command::PasteAfter, None);
    assert_eq!(after.text(), "one\nnew\ntwo\n");
    assert_eq!(after.position(), (1, 0));

    let mut before = Session::new("one\ntwo\n");
    set_register(&mut before, RegisterValue::linewise("new", LineEnding::Lf));
    before.apply(Command::PasteBefore, None);
    assert_eq!(before.text(), "new\none\ntwo\n");
    assert_eq!(before.position(), (0, 0));
}

#[test]
fn a_linewise_paste_after_the_last_line_opens_a_new_line() {
    for text in ["one", "one\n"] {
        let mut session = Session::new(text);
        set_register(&mut session, RegisterValue::linewise("new", LineEnding::Lf));
        session.apply(Command::PasteAfter, None);
        assert_eq!(session.text(), "one\nnew\n", "{text:?}");
        assert_eq!(session.position(), (1, 0), "{text:?}");
    }
}

#[test]
fn paste_before_and_after_follows_the_blockwise_shape() {
    let block = || RegisterValue::blockwise(&["XX".to_owned(), "YY".to_owned()], LineEnding::Lf);

    let mut before = Session::new("ab\ncd\n");
    set_register(&mut before, block());
    before.apply(Command::PasteBefore, None);
    assert_eq!(before.text(), "XXab\nYYcd\n");

    let mut after = Session::new("ab\ncd\n");
    set_register(&mut after, block());
    after.apply(Command::PasteAfter, None);
    assert_eq!(after.text(), "aXXb\ncYYd\n");
}

#[test]
fn a_blockwise_paste_past_the_last_line_opens_the_missing_lines() {
    let mut session = Session::new("ab");
    set_register(
        &mut session,
        RegisterValue::blockwise(&["XX".to_owned(), "YY".to_owned()], LineEnding::Lf),
    );
    session.apply(Command::PasteBefore, None);
    // The buffer terminates its last line, and the save writes the file end
    // that the loaded text held.
    assert_eq!(session.text(), "XXab\nYY\n");
}

#[test]
fn a_count_before_a_paste_repeats_the_value() {
    let mut session = Session::new("abc\n");
    set_register(&mut session, RegisterValue::characterwise("X"));
    session.apply(Command::PasteAfter, count(3));
    assert_eq!(session.text(), "aXXXbc\n");
}

#[test]
fn a_paste_without_a_register_value_changes_nothing() {
    let mut session = Session::new("abc\n");
    assert_eq!(
        session.apply(Command::PasteAfter, None),
        CommandOutcome::RegisterEmpty
    );
    assert_eq!(session.text(), "abc\n");
}

#[test]
fn a_visual_paste_replaces_the_selection_and_preserves_the_source_register() {
    let mut characters = Session::new("abcd\n");
    set_register(&mut characters, RegisterValue::characterwise("XY"));
    characters.apply(Command::EnterVisual, None);
    characters.apply(Command::MoveRight, None);
    assert_eq!(
        characters.apply(Command::PasteAfter, None),
        CommandOutcome::Changed
    );
    assert_eq!(characters.text(), "XYcd\n");
    assert_eq!(
        characters.register(),
        Some(("XY", RegisterShape::Characterwise)),
        "the replaced text never reaches the register"
    );
    assert_eq!(characters.state.mode(), Mode::Normal);

    // The same paste repeats, because the register still holds the source.
    let mut lines = Session::new("one\ntwo\n");
    lines.run(&[Command::YankOverMotion, Command::YankOverMotion]);
    place(&mut lines, 1, 0);
    lines.apply(Command::EnterVisualLine, None);
    lines.apply(Command::PasteAfter, None);
    assert_eq!(lines.text(), "one\none\n");
    assert_eq!(lines.register(), Some(("one\n", RegisterShape::Linewise)));
}

#[test]
fn a_visual_block_paste_replaces_the_rectangle_and_keeps_the_register() {
    let mut session = Session::new("abcdef\na\nabcdef\n");
    set_register(&mut session, RegisterValue::characterwise("Z"));
    select_block(&mut session);
    assert_eq!(
        session.apply(Command::PasteBefore, None),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "abZf\na\nabf\n");
    assert_eq!(
        session.register(),
        Some(("Z", RegisterShape::Characterwise))
    );
}

#[test]
fn undo_and_redo_reverse_and_reapply_every_operator() {
    let sequences: &[&[Command]] = &[
        &[Command::DeleteOverMotion, Command::MoveNextWordStart],
        &[Command::ChangeOverMotion, Command::MoveNextWordStart],
        &[Command::DeleteOverMotion, Command::DeleteOverMotion],
        &[Command::ChangeOverMotion, Command::ChangeOverMotion],
        &[Command::DeleteToLineEnd],
        &[Command::ChangeToLineEnd],
        &[Command::OpenLineBelow],
        &[Command::OpenLineAbove],
    ];

    for sequence in sequences {
        let mut session = Session::new("alpha beta\nsecond\n");
        session.run(sequence);
        let changed = session.text();
        assert_ne!(changed, "alpha beta\nsecond\n", "{sequence:?}");

        assert_eq!(
            session.apply(Command::Undo, None),
            CommandOutcome::Changed,
            "{sequence:?}"
        );
        assert_eq!(session.text(), "alpha beta\nsecond\n", "{sequence:?}");
        assert_eq!(session.state.mode(), Mode::Normal, "{sequence:?}");

        assert_eq!(
            session.apply(Command::Redo, None),
            CommandOutcome::Changed,
            "{sequence:?}"
        );
        assert_eq!(session.text(), changed, "{sequence:?}");
    }
}

#[test]
fn undo_and_redo_report_an_exhausted_history() {
    let mut session = Session::new("alpha\n");
    assert_eq!(
        session.apply(Command::Undo, None),
        CommandOutcome::HistoryExhausted
    );
    assert_eq!(
        session.apply(Command::Redo, None),
        CommandOutcome::HistoryExhausted
    );
    assert_eq!(session.text(), "alpha\n");
}

#[test]
fn a_visual_selection_edit_reverses_with_one_undo() {
    let mut session = Session::new("one\ntwo\n");
    session.apply(Command::EnterVisualLine, None);
    session.apply(Command::MoveDown, None);
    session.apply(Command::ShiftSelectionRight, None);
    assert_eq!(session.text(), "    one\n    two\n");
    session.apply(Command::Undo, None);
    assert_eq!(session.text(), "one\ntwo\n");
}

#[test]
fn dot_repeat_replays_the_description_of_the_last_change() {
    let mut words = Session::new("one two three\n");
    words.apply(Command::DeleteOverMotion, None);
    words.apply(Command::MoveNextWordStart, None);
    assert_eq!(words.text(), "two three\n");
    assert_eq!(
        words.apply(Command::RepeatChange, None),
        CommandOutcome::Changed
    );
    assert_eq!(words.text(), "three\n");

    let mut lines = Session::new("one\ntwo\nthree\n");
    lines.run(&[Command::DeleteOverMotion, Command::DeleteOverMotion]);
    lines.apply(Command::RepeatChange, None);
    assert_eq!(lines.text(), "three\n");

    let mut paste = Session::new("abc\n");
    set_register(&mut paste, RegisterValue::characterwise("X"));
    paste.apply(Command::PasteAfter, None);
    paste.apply(Command::RepeatChange, None);
    assert_eq!(paste.text(), "aXXbc\n");
}

#[test]
fn dot_repeat_without_a_recorded_change_does_nothing() {
    let mut session = Session::new("one two\n");
    assert_eq!(
        session.apply(Command::RepeatChange, None),
        CommandOutcome::Unhandled
    );
    assert_eq!(session.text(), "one two\n");

    // A yank changes no text, so it never becomes the repeated change.
    session.run(&[Command::YankOverMotion, Command::YankOverMotion]);
    assert_eq!(
        session.apply(Command::RepeatChange, None),
        CommandOutcome::Unhandled
    );
}

#[test]
fn the_comment_toggle_writes_the_token_behind_the_indent() {
    let mut session = Session::new("fn main() {\n    let value = 1;\n}\n");
    place(&mut session, 1, 4);

    assert_eq!(session.toggle_comment(), CommandOutcome::Changed);
    assert_eq!(
        session.text(),
        "fn main() {\n    // let value = 1;\n}\n",
        "the toggle preserves the existing indent"
    );

    // The second toggle removes the token and its separating space again.
    assert_eq!(session.toggle_comment(), CommandOutcome::Changed);
    assert_eq!(session.text(), "fn main() {\n    let value = 1;\n}\n");
}

#[test]
fn the_comment_toggle_of_a_mixed_selection_comments_every_line() {
    let mut session = Session::new("// one\ntwo\n\nthree\n");
    session.apply(Command::EnterVisualLine, None);
    session.apply(Command::MoveDown, count(3));

    assert_eq!(session.toggle_comment(), CommandOutcome::Changed);
    // One line already carries the token, so the mixed selection comments the
    // rest instead of removing anything. A blank line keeps its shape.
    assert_eq!(session.text(), "// // one\n// two\n\n// three\n");
    assert_eq!(session.state.mode(), Mode::Normal);

    // Every non-blank line now carries the token, so one toggle removes it.
    session.apply(Command::EnterVisualLine, None);
    session.apply(Command::MoveUp, count(3));
    session.toggle_comment();
    assert_eq!(session.text(), "// one\ntwo\n\nthree\n");
}

#[test]
fn one_undo_reverses_a_complete_comment_toggle() {
    let mut session = Session::new("one\ntwo\nthree\n");
    session.apply(Command::EnterVisualLine, None);
    session.apply(Command::MoveDown, count(2));
    session.toggle_comment();
    assert_eq!(session.text(), "// one\n// two\n// three\n");

    assert_eq!(session.apply(Command::Undo, None), CommandOutcome::Changed);
    assert_eq!(
        session.text(),
        "one\ntwo\nthree\n",
        "the toggle applies as one transaction"
    );
}

#[test]
fn a_buffer_without_a_comment_token_stays_unchanged() {
    // A file that no adapter serves stays fully editable. The toggle changes
    // nothing and reports that it did nothing, so the caller can name the
    // reason.
    let mut session = Session::new("value\n");

    assert_eq!(session.toggle_comment_with(None), CommandOutcome::Unhandled);
    assert_eq!(session.text(), "value\n");
}

#[test]
fn a_blank_selection_keeps_the_buffer_unchanged() {
    let mut session = Session::new("\n\n");
    session.apply(Command::EnterVisualLine, None);
    session.apply(Command::MoveDown, None);

    assert_eq!(session.toggle_comment(), CommandOutcome::Applied);
    assert_eq!(session.text(), "\n\n");
}

#[test]
fn the_syntax_indent_replaces_the_previous_line_rule() {
    // The previous-line rule would copy the indent of `fn main() {`, which is
    // zero. The language adapter reports one level inside the block instead.
    let mut session = Session::new("fn main() {\n}\n");
    place(&mut session, 0, 10);

    session.apply_indented(Command::OpenLineBelow, AutoIndent::Levels(1));
    assert_eq!(session.text(), "fn main() {\n    \n}\n");
    assert_eq!(session.position(), (1, 4));

    // A closing delimiter loses the level again.
    let mut closing = Session::new("fn main() {\n    let value = 1;\n}\n");
    place(&mut closing, 1, 0);
    closing.apply_indented(Command::OpenLineBelow, AutoIndent::Levels(0));
    assert_eq!(closing.text(), "fn main() {\n    let value = 1;\n\n}\n");
}

#[test]
fn the_language_width_sizes_one_indent_level_and_one_shift_step() {
    let two = NonZeroU8::new(2).expect("the literal 2 is not zero");

    // The settings tab width is four columns, but a two-column language
    // renders one level as two columns and two levels as four.
    let mut session = Session::new("{\n}\n");
    session.language_indent_width = Some(two);
    place(&mut session, 0, 0);
    session.apply_indented(Command::OpenLineBelow, AutoIndent::Levels(1));
    assert_eq!(session.text(), "{\n  \n}\n");

    let mut nested = Session::new("{\n}\n");
    nested.language_indent_width = Some(two);
    place(&mut nested, 0, 0);
    nested.apply_indented(Command::OpenLineBelow, AutoIndent::Levels(2));
    assert_eq!(nested.text(), "{\n    \n}\n");

    // One level is also one shift step, as it is in Vim.
    let mut shifted = Session::new("one\n");
    shifted.language_indent_width = Some(two);
    shifted.apply(Command::EnterVisualLine, None);
    shifted.apply(Command::ShiftSelectionRight, None);
    assert_eq!(shifted.text(), "  one\n");

    // A buffer that no adapter serves keeps the settings width.
    let mut plain = Session::new("{\n}\n");
    place(&mut plain, 0, 0);
    plain.apply_indented(Command::OpenLineBelow, AutoIndent::Levels(1));
    assert_eq!(plain.text(), "{\n    \n}\n");
}

#[test]
fn a_line_break_uses_the_syntax_indent_and_keeps_the_fallback() {
    let mut syntax = Session::new("fn main() {}\n");
    syntax.enter_insert();
    place(&mut syntax, 0, 11);
    syntax.enter_insert();
    syntax.insert_line_break_indented(AutoIndent::Levels(2));
    assert_eq!(syntax.text(), "fn main() {\n        }\n");

    // Without a parse result the editor copies the previous non-empty line.
    let mut fallback = Session::new("    value\n");
    place(&mut fallback, 0, 8);
    fallback.enter_insert();
    fallback.insert_line_break_indented(AutoIndent::PreviousLine);
    assert_eq!(fallback.text(), "    valu\n    e\n");
    assert_eq!(fallback.position(), (1, 4));
}

/// Puts the cursor inside the body of a delimiter pair and starts a delete.
///
/// Every delimiter case below shares one text layout, so the column is the same
/// for each pair.
fn delete_object(text: &str, column: usize, object: Command) -> Session {
    let mut session = Session::new(text);
    place(&mut session, 0, column);
    session.apply(Command::DeleteOverMotion, None);
    session.apply(object, None);
    session
}

#[test]
fn every_delimiter_object_takes_its_inner_and_its_around_text() {
    // The text, the inner command, the around command, and the two results.
    let cases: &[(&str, Command, Command, &str, &str)] = &[
        (
            "call(alpha)\n",
            Command::SelectInnerParen,
            Command::SelectAroundParen,
            "call()\n",
            "call\n",
        ),
        (
            "call[alpha]\n",
            Command::SelectInnerBracket,
            Command::SelectAroundBracket,
            "call[]\n",
            "call\n",
        ),
        (
            "call{alpha}\n",
            Command::SelectInnerBrace,
            Command::SelectAroundBrace,
            "call{}\n",
            "call\n",
        ),
        (
            "call<alpha>\n",
            Command::SelectInnerAngle,
            Command::SelectAroundAngle,
            "call<>\n",
            "call\n",
        ),
        (
            "call\"alpha\"\n",
            Command::SelectInnerDoubleQuote,
            Command::SelectAroundDoubleQuote,
            "call\"\"\n",
            "call\n",
        ),
        (
            "call'alpha'\n",
            Command::SelectInnerSingleQuote,
            Command::SelectAroundSingleQuote,
            "call''\n",
            "call\n",
        ),
        (
            "call`alpha`\n",
            Command::SelectInnerBacktick,
            Command::SelectAroundBacktick,
            "call``\n",
            "call\n",
        ),
    ];
    for (text, inner, around, after_inner, after_around) in cases {
        let session = delete_object(text, 6, *inner);
        assert_eq!(session.text(), *after_inner, "{inner}");
        assert_eq!(
            session.register(),
            Some(("alpha", RegisterShape::Characterwise)),
            "{inner}"
        );

        let session = delete_object(text, 6, *around);
        assert_eq!(session.text(), *after_around, "{around}");
    }
}

#[test]
fn every_operator_takes_one_text_object() {
    // The operator, the outcome, the text after it, and the mode after it.
    let cases: &[(Command, CommandOutcome, &str, Mode)] = &[
        (
            Command::DeleteOverMotion,
            CommandOutcome::Changed,
            "call()\n",
            Mode::Normal,
        ),
        (
            Command::ChangeOverMotion,
            CommandOutcome::Changed,
            "call()\n",
            Mode::Insert,
        ),
        // A yank writes the register and keeps the text.
        (
            Command::YankOverMotion,
            CommandOutcome::Applied,
            "call(alpha)\n",
            Mode::Normal,
        ),
    ];
    for (operator, outcome, expected, mode) in cases {
        let mut session = Session::new("call(alpha)\n");
        place(&mut session, 0, 6);
        session.apply(*operator, None);
        assert_eq!(
            session.apply(Command::SelectInnerParen, None),
            *outcome,
            "{operator}"
        );
        assert_eq!(session.text(), *expected, "{operator}");
        assert_eq!(session.state.mode(), *mode, "{operator}");
        assert_eq!(
            session.register(),
            Some(("alpha", RegisterShape::Characterwise)),
            "{operator}"
        );
    }
}

#[test]
fn a_cursor_on_either_delimiter_names_the_same_pair() {
    for column in [4, 10] {
        let session = delete_object("call(alpha)\n", column, Command::SelectInnerParen);
        assert_eq!(session.text(), "call()\n", "column {column}");
    }
}

#[test]
fn an_unmatched_delimiter_leaves_the_buffer_unchanged() {
    let mut session = Session::new("call(alpha\n");
    place(&mut session, 0, 6);
    session.apply(Command::DeleteOverMotion, None);
    assert_eq!(
        session.apply(Command::SelectInnerParen, None),
        CommandOutcome::OperatorAborted
    );
    assert_eq!(session.text(), "call(alpha\n");
    assert_eq!(session.state.pending_operator(), None);
    assert_eq!(session.register(), None);
}

#[test]
fn a_count_names_the_pair_that_holds_the_inner_pair() {
    let mut nested = Session::new("((alpha))\n");
    place(&mut nested, 0, 3);
    nested.apply(Command::DeleteOverMotion, None);
    nested.apply(Command::SelectInnerParen, count(2));
    assert_eq!(nested.text(), "()\n");

    // A pair without an outer pair changes nothing.
    let mut single = Session::new("(alpha)\n");
    place(&mut single, 0, 3);
    single.apply(Command::DeleteOverMotion, None);
    assert_eq!(
        single.apply(Command::SelectInnerParen, count(2)),
        CommandOutcome::OperatorAborted
    );
    assert_eq!(single.text(), "(alpha)\n");

    // A quote pair never nests, so a count above one names nothing.
    let mut quoted = Session::new("\"alpha\"\n");
    place(&mut quoted, 0, 3);
    quoted.apply(Command::DeleteOverMotion, None);
    assert_eq!(
        quoted.apply(Command::SelectInnerDoubleQuote, count(2)),
        CommandOutcome::OperatorAborted
    );
    assert_eq!(quoted.text(), "\"alpha\"\n");
}

#[test]
fn a_pair_matches_across_lines_as_one_transaction() {
    let mut session = Session::new("fn main() {\n    let value = 1;\n}\n");
    place(&mut session, 1, 4);
    session.apply(Command::DeleteOverMotion, None);
    assert_eq!(
        session.apply(Command::SelectInnerBrace, None),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "fn main() {}\n");

    // One undo reverses the whole multi-line change.
    session.apply(Command::Undo, None);
    assert_eq!(session.text(), "fn main() {\n    let value = 1;\n}\n");
}

#[test]
fn an_empty_pair_holds_no_inner_text() {
    let mut inner = Session::new("call()\n");
    place(&mut inner, 0, 4);
    inner.apply(Command::DeleteOverMotion, None);
    assert_eq!(
        inner.apply(Command::SelectInnerParen, None),
        CommandOutcome::Applied
    );
    assert_eq!(inner.text(), "call()\n");

    let around = delete_object("call()\n", 4, Command::SelectAroundParen);
    assert_eq!(around.text(), "call\n");
}

#[test]
fn the_word_objects_take_one_run_and_its_blanks() {
    // The text, the cursor column, the command, and the text after a delete.
    let cases: &[(&str, usize, Command, &str)] = &[
        ("alpha beta\n", 0, Command::SelectInnerWord, " beta\n"),
        ("alpha beta\n", 0, Command::SelectAroundWord, "beta\n"),
        // `w` stops at a class change, so the punctuation stays.
        ("alpha.beta\n", 0, Command::SelectInnerWord, ".beta\n"),
        ("alpha.beta\n", 0, Command::SelectAroundWord, ".beta\n"),
        // `W` joins the punctuation into one non-blank run.
        (
            "alpha.beta gamma\n",
            0,
            Command::SelectInnerLongWord,
            " gamma\n",
        ),
        (
            "alpha.beta gamma\n",
            0,
            Command::SelectAroundLongWord,
            "gamma\n",
        ),
    ];
    for (text, column, object, expected) in cases {
        let session = delete_object(text, *column, *object);
        assert_eq!(session.text(), *expected, "{object} in `{text}`");
    }

    // A count takes one further run for each repetition.
    let mut inner = Session::new("alpha beta gamma\n");
    place(&mut inner, 0, 0);
    inner.apply(Command::DeleteOverMotion, None);
    inner.apply(Command::SelectInnerWord, count(2));
    assert_eq!(inner.text(), "beta gamma\n");

    // A count takes one further word and its blanks for each repetition.
    let mut around = Session::new("alpha beta gamma\n");
    place(&mut around, 0, 0);
    around.apply(Command::DeleteOverMotion, None);
    around.apply(Command::SelectAroundWord, count(2));
    assert_eq!(around.text(), "gamma\n");
}

#[test]
fn a_word_object_stays_inside_its_line() {
    // The last word of a line takes its leading blank, because none follows it.
    let mut last = Session::new("alpha beta\ngamma\n");
    place(&mut last, 0, 8);
    last.apply(Command::DeleteOverMotion, None);
    last.apply(Command::SelectAroundWord, None);
    assert_eq!(last.text(), "alpha\ngamma\n");

    // An empty line holds no run, so the object changes nothing.
    let mut empty = Session::new("alpha\n\nbeta\n");
    place(&mut empty, 1, 0);
    empty.apply(Command::DeleteOverMotion, None);
    assert_eq!(
        empty.apply(Command::SelectInnerWord, None),
        CommandOutcome::Applied
    );
    assert_eq!(empty.text(), "alpha\n\nbeta\n");
}

#[test]
fn a_text_object_counts_characters_in_a_unicode_body() {
    let session = delete_object("wert(äöü…)\n", 6, Command::SelectInnerParen);
    assert_eq!(session.text(), "wert()\n");
    assert_eq!(
        session.register(),
        Some(("äöü…", RegisterShape::Characterwise))
    );

    let word = delete_object("straße größe\n", 2, Command::SelectAroundWord);
    assert_eq!(word.text(), "größe\n");
}

#[test]
fn a_text_object_selects_the_range_in_visual_mode() {
    let mut session = Session::new("call(alpha)\n");
    place(&mut session, 0, 6);
    session.apply(Command::EnterVisual, None);
    assert_eq!(
        session.apply(Command::SelectInnerParen, None),
        CommandOutcome::Applied
    );
    let Some(Selection::Characterwise(range)) = session.selection() else {
        panic!("Visual mode selects a run of characters");
    };
    assert_eq!((range.start().get(), range.end().get()), (5, 10));
    assert_eq!(
        session.apply(Command::DeleteSelection, None),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "call()\n");
}

#[test]
fn dot_repeat_replays_a_text_object_change() {
    let mut session = Session::new("one(alpha)\ntwo(beta)\n");
    place(&mut session, 0, 5);
    session.apply(Command::DeleteOverMotion, None);
    session.apply(Command::SelectInnerParen, None);
    assert_eq!(session.text(), "one()\ntwo(beta)\n");

    place(&mut session, 1, 5);
    assert_eq!(
        session.apply(Command::RepeatChange, None),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "one()\ntwo()\n");
}

#[test]
fn a_named_yank_and_a_named_paste_use_the_same_register() {
    // `"ayy` yanks the first line, and `"ap` pastes it below the second one.
    let mut session = Session::new("alpha\nbeta\n");
    session.apply_with_register(Command::YankOverMotion, None, Some('a'));
    session.apply(Command::YankOverMotion, None);
    assert_eq!(
        session.registers.value(Some('a')).map(RegisterValue::text),
        Some("alpha\n"),
    );
    // The unnamed register stayed empty, so the clipboard sees no yank.
    assert!(session.registers.unnamed().is_none());

    place(&mut session, 1, 0);
    session.apply_with_register(Command::PasteAfter, None, Some('a'));
    assert_eq!(session.text(), "alpha\nbeta\nalpha\n");
}

#[test]
fn a_named_operator_keeps_its_register_until_its_motion_arrives() {
    // `"adw` names the register with `d`, and `w` completes the operator.
    let mut session = Session::new("alpha beta gamma\n");
    assert_eq!(
        session.apply_with_register(Command::DeleteOverMotion, None, Some('a')),
        CommandOutcome::OperatorPending,
    );
    assert_eq!(
        session.apply(Command::MoveNextWordStart, None),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "beta gamma\n");
    assert_eq!(
        session.registers.value(Some('a')).map(RegisterValue::text),
        Some("alpha "),
    );
    assert!(session.registers.unnamed().is_none());

    // The name qualified that operation alone, so the next delete is unnamed.
    session.run(&[Command::DeleteOverMotion, Command::MoveNextWordStart]);
    assert_eq!(
        session.registers.value(Some('a')).map(RegisterValue::text),
        Some("alpha "),
    );
    assert_eq!(
        session.registers.unnamed().map(RegisterValue::text),
        Some("beta "),
    );
}

#[test]
fn a_black_hole_delete_leaves_the_unnamed_register_unchanged() {
    let mut session = Session::new("alpha\nbeta\n");
    // `yy` fills the unnamed register.
    session.run(&[Command::YankOverMotion, Command::YankOverMotion]);
    assert_eq!(
        session.registers.unnamed().map(RegisterValue::text),
        Some("alpha\n"),
    );

    // `"_dd` removes the line and keeps the yanked value.
    session.apply_with_register(Command::DeleteOverMotion, None, Some('_'));
    assert_eq!(
        session.apply(Command::DeleteOverMotion, None),
        CommandOutcome::Changed
    );
    assert_eq!(session.text(), "beta\n");
    assert_eq!(
        session.registers.unnamed().map(RegisterValue::text),
        Some("alpha\n"),
    );
}

#[test]
fn a_paste_from_an_empty_named_register_changes_nothing() {
    let mut session = Session::new("alpha\n");
    assert_eq!(
        session.apply_with_register(Command::PasteAfter, None, Some('q')),
        CommandOutcome::RegisterEmpty,
    );
    assert_eq!(session.text(), "alpha\n");
}

#[test]
fn a_delete_over_the_word_motion_stops_at_the_line_end() {
    // `dw` on the last word of a line removes that word and keeps the line, so
    // the delete never joins the next line.
    let mut session = Session::new("alpha beta gamma\ndelta\n");
    place(&mut session, 0, 11);
    session.run(&[Command::DeleteOverMotion, Command::MoveNextWordStart]);
    assert_eq!(session.text(), "alpha beta \ndelta\n");

    // The last line of the buffer follows the same rule.
    let mut last = Session::new("alpha beta gamma\n");
    place(&mut last, 0, 11);
    last.run(&[Command::DeleteOverMotion, Command::MoveNextWordStart]);
    assert_eq!(last.text(), "alpha beta \n");

    // A count stops at the end of the last word that the motion moved over.
    let mut counted = Session::new("alpha beta\ngamma\n");
    counted.apply(Command::DeleteOverMotion, None);
    counted.apply(Command::MoveNextWordStart, count(2));
    assert_eq!(counted.text(), "\ngamma\n");
}

#[test]
fn a_word_motion_without_an_operator_still_stops_on_the_last_character() {
    // The rule above belongs to the operator, so `w` alone is unchanged.
    let mut session = Session::new("alpha beta gamma\n");
    place(&mut session, 0, 11);
    session.apply(Command::MoveNextWordStart, None);
    assert_eq!(session.position(), (0, 15));
}

#[test]
fn a_change_over_the_word_motion_keeps_the_blanks_after_the_word() {
    // `cw` on a non-blank changes to the end of the word, exactly as `ce` does.
    let mut session = Session::new("alpha  beta\n");
    session.run(&[Command::ChangeOverMotion, Command::MoveNextWordStart]);
    assert_eq!(session.text(), "  beta\n");
    assert_eq!(session.state.mode(), Mode::Insert);

    // On a blank the plain rule applies, so the change removes the blanks.
    let mut blank = Session::new("alpha  beta\n");
    place(&mut blank, 0, 5);
    blank.run(&[Command::ChangeOverMotion, Command::MoveNextWordStart]);
    assert_eq!(blank.text(), "alphabeta\n");
}
