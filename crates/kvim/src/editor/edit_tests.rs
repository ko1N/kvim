//! Behavior tests for operators, registers, paste, selection edits, and repeat.

use std::num::{NonZeroU16, NonZeroU32};

use super::{
    AutoIndent, CommandOutcome, EditContext, EditingState, Operator, RegisterShape, RegisterValue,
    Registers, Selection, Viewport, WindowState,
};
use crate::core::{LineEnding, TextBuffer};
use crate::input::{Command, Mode};
use crate::settings::{EditorSettings, FileSettings};

/// One buffer, one register file, one editing state, and one viewport.
struct Session {
    buffer: TextBuffer,
    settings: EditorSettings,
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
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state
            .apply(&mut context, &mut self.view, command, count)
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
            registers: &mut self.registers,
            applied: Vec::new(),
        };
        self.state.delete_backward(&mut context, &mut self.view)
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
        (Command::MoveDown, "", "alpha beta\nsecond\n"),
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
    let mut session = Session::new("one\ntwo");
    place(&mut session, 1, 0);
    session.run(&[Command::DeleteOverMotion, Command::DeleteOverMotion]);
    assert_eq!(session.text(), "one");
    assert_eq!(session.register(), Some(("two\n", RegisterShape::Linewise)));
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
fn a_backward_delete_at_column_zero_removes_a_complete_crlf_ending() {
    let mut session = Session::new("alpha\r\nbeta\r\n");
    assert_eq!(session.buffer.line_ending(), LineEnding::Crlf);
    place(&mut session, 1, 0);
    session.enter_insert();
    assert_eq!(session.delete_backward(), CommandOutcome::Changed);
    assert_eq!(session.text(), "alphabeta\r\n");
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
    let mut session = Session::new("one");
    set_register(&mut session, RegisterValue::linewise("new", LineEnding::Lf));
    session.apply(Command::PasteAfter, None);
    assert_eq!(session.text(), "one\nnew");
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
    assert_eq!(session.text(), "XXab\nYY");
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
