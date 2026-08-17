//! Behavior tests for modes, cursors, motions, selections, search, and the viewport.

use std::num::{NonZeroU16, NonZeroU32};

use super::{
    ColumnLimit, CommandContext, CommandOutcome, Cursor, EditContext, EditingState, ModeState,
    Registers, SearchDirection, SearchError, SearchQuery, Selection, Viewport, ViewportAlignment,
    WindowState,
};
use crate::core::TextBuffer;
use crate::input::{Command, Mode};
use crate::settings::{
    CaseSensitivity, DisplaySettings, EditorSettings, FileSettings, SearchSettings,
};

/// Five lines that cover a long line, an indented line, an empty line, a line of
/// blanks, and a last line without a terminator.
const SAMPLE: &str = "alpha beta\n    gamma\n\n   \nlast";

const MOTION_COMMANDS: &[Command] = &[
    Command::MoveLeft,
    Command::MoveRight,
    Command::MoveDown,
    Command::MoveUp,
    Command::MoveNextWordStart,
    Command::MovePreviousWordStart,
    Command::MoveNextWordEnd,
    Command::MoveFirstColumn,
    Command::MoveFirstNonBlank,
    Command::MoveLineEnd,
    Command::MoveFirstLine,
    Command::MoveLastLine,
    Command::MoveHalfPageDown,
    Command::MoveHalfPageUp,
    Command::MoveFullPageDown,
    Command::MoveFullPageUp,
    Command::CenterCursorLine,
    Command::AlignCursorLineTop,
    Command::AlignCursorLineBottom,
];

fn buffer(text: &str) -> TextBuffer {
    TextBuffer::from_text(text, &FileSettings::default()).expect("the test text is small")
}

fn viewport(rows: u16, cells: u16) -> Viewport {
    Viewport::new(
        NonZeroU16::new(rows).expect("the test row count is not zero"),
        NonZeroU16::new(cells).expect("the test cell count is not zero"),
    )
}

fn window(rows: u16, cells: u16) -> WindowState {
    WindowState::new(viewport(rows, cells))
}

fn count(value: u32) -> Option<NonZeroU32> {
    Some(NonZeroU32::new(value).expect("the test count is not zero"))
}

fn apply(
    text: &mut TextBuffer,
    state: &mut EditingState,
    view: &mut WindowState,
    command: Command,
    repeat: Option<NonZeroU32>,
) -> CommandOutcome {
    let settings = EditorSettings::default();
    let mut registers = Registers::default();
    let mut context = EditContext {
        buffer: text,
        settings: &settings,
        search: None,
        registers: &mut registers,
        applied: Vec::new(),
    };
    state.apply(&mut context, view, command, repeat)
}

fn search(
    text: &mut TextBuffer,
    query: &SearchQuery,
    settings: &SearchSettings,
    state: &mut EditingState,
    view: &mut WindowState,
    command: Command,
) -> CommandOutcome {
    let settings = EditorSettings {
        search: *settings,
        ..EditorSettings::default()
    };
    let mut registers = Registers::default();
    let mut context = EditContext {
        buffer: text,
        settings: &settings,
        search: Some(query),
        registers: &mut registers,
        applied: Vec::new(),
    };
    state.apply(&mut context, view, command, None)
}

fn position(view: &WindowState) -> (usize, usize) {
    (view.cursor().line().get(), view.cursor().column().get())
}

#[test]
fn every_motion_stays_inside_an_empty_buffer() {
    let mut text = buffer("");
    for command in MOTION_COMMANDS {
        let mut state = EditingState::new();
        let mut view = window(10, 80);
        let outcome = apply(&mut text, &mut state, &mut view, *command, count(7));
        assert_eq!(outcome, CommandOutcome::Applied, "{command}");
        assert_eq!(position(&view), (0, 0), "{command}");
        assert_eq!(view.first_line(), 0, "{command}");
    }
}

#[test]
fn every_motion_from_the_buffer_start_reaches_its_position() {
    let expected: &[(Command, (usize, usize))] = &[
        (Command::MoveLeft, (0, 0)),
        (Command::MoveRight, (0, 1)),
        (Command::MoveDown, (1, 0)),
        (Command::MoveUp, (0, 0)),
        (Command::MoveNextWordStart, (0, 6)),
        (Command::MovePreviousWordStart, (0, 0)),
        (Command::MoveNextWordEnd, (0, 4)),
        (Command::MoveFirstColumn, (0, 0)),
        (Command::MoveFirstNonBlank, (0, 0)),
        (Command::MoveLineEnd, (0, 9)),
        (Command::MoveFirstLine, (0, 0)),
        (Command::MoveLastLine, (4, 0)),
        (Command::MoveHalfPageDown, (4, 0)),
        (Command::MoveHalfPageUp, (0, 0)),
        (Command::MoveFullPageDown, (4, 0)),
        (Command::MoveFullPageUp, (0, 0)),
        (Command::CenterCursorLine, (0, 0)),
        (Command::AlignCursorLineTop, (0, 0)),
        (Command::AlignCursorLineBottom, (0, 0)),
    ];

    let mut text = buffer(SAMPLE);
    for (command, place) in expected {
        let mut state = EditingState::new();
        let mut view = window(10, 80);
        apply(&mut text, &mut state, &mut view, *command, None);
        assert_eq!(position(&view), *place, "{command}");
    }
}

#[test]
fn every_motion_from_the_buffer_end_stays_inside_the_buffer() {
    let expected: &[(Command, (usize, usize))] = &[
        (Command::MoveLeft, (4, 2)),
        (Command::MoveRight, (4, 3)),
        (Command::MoveDown, (4, 3)),
        (Command::MoveUp, (3, 2)),
        (Command::MoveNextWordStart, (4, 3)),
        (Command::MovePreviousWordStart, (4, 0)),
        (Command::MoveNextWordEnd, (4, 3)),
        (Command::MoveFirstColumn, (4, 0)),
        (Command::MoveFirstNonBlank, (4, 0)),
        (Command::MoveLineEnd, (4, 3)),
        (Command::MoveFirstLine, (0, 0)),
        (Command::MoveLastLine, (4, 0)),
        (Command::MoveHalfPageDown, (4, 3)),
        (Command::MoveHalfPageUp, (0, 3)),
        (Command::MoveFullPageDown, (4, 3)),
        (Command::MoveFullPageUp, (0, 3)),
        (Command::CenterCursorLine, (4, 3)),
        (Command::AlignCursorLineTop, (4, 3)),
        (Command::AlignCursorLineBottom, (4, 3)),
    ];

    let mut text = buffer(SAMPLE);
    for (command, place) in expected {
        let mut state = EditingState::new();
        let mut view = window(10, 80);
        apply(
            &mut text,
            &mut state,
            &mut view,
            Command::MoveLastLine,
            None,
        );
        apply(
            &mut text,
            &mut state,
            &mut view,
            Command::MoveRight,
            count(3),
        );
        assert_eq!(
            position(&view),
            (4, 3),
            "the fixture starts at the last character"
        );

        apply(&mut text, &mut state, &mut view, *command, None);
        assert_eq!(position(&view), *place, "{command}");
    }
}

#[test]
fn a_count_past_a_buffer_limit_stops_at_the_limit() {
    let mut text = buffer(SAMPLE);
    let expected: &[(Command, (usize, usize))] = &[
        (Command::MoveDown, (4, 0)),
        (Command::MoveRight, (0, 9)),
        (Command::MoveNextWordStart, (4, 3)),
        (Command::MoveNextWordEnd, (4, 3)),
        (Command::MoveHalfPageDown, (4, 0)),
        (Command::MoveFullPageDown, (4, 0)),
        (Command::MoveLastLine, (4, 0)),
        (Command::MoveLineEnd, (4, 3)),
    ];
    for (command, place) in expected {
        let mut state = EditingState::new();
        let mut view = window(10, 80);
        apply(&mut text, &mut state, &mut view, *command, count(9_999));
        assert_eq!(position(&view), *place, "{command}");
    }

    // The same rule holds at the low limit.
    let mut state = EditingState::new();
    let mut view = window(10, 80);
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveLastLine,
        None,
    );
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveUp,
        count(9_999),
    );
    assert_eq!(position(&view), (0, 0));
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveLeft,
        count(9_999),
    );
    assert_eq!(position(&view), (0, 0));
}

#[test]
fn a_count_before_a_line_motion_names_a_line_number() {
    let mut text = buffer(SAMPLE);
    let mut state = EditingState::new();
    let mut view = window(10, 80);

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveLastLine,
        count(2),
    );
    assert_eq!(
        position(&view),
        (1, 4),
        "the second line indents by four spaces"
    );

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveFirstLine,
        count(4),
    );
    assert_eq!(
        position(&view),
        (3, 2),
        "a line of blanks holds its last character"
    );

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveFirstLine,
        count(999),
    );
    assert_eq!(
        position(&view),
        (4, 0),
        "a line past the buffer stops at the last line"
    );
}

#[test]
fn motions_over_an_empty_line_and_a_line_of_blanks_stay_valid() {
    let mut text = buffer(SAMPLE);
    let mut state = EditingState::new();
    let mut view = window(10, 80);

    // The empty line holds column zero only.
    apply(&mut text, &mut state, &mut view, Command::MoveLineEnd, None);
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveDown,
        count(2),
    );
    assert_eq!(position(&view), (2, 0));

    // The line of blanks keeps the end-of-line preference.
    apply(&mut text, &mut state, &mut view, Command::MoveDown, None);
    assert_eq!(position(&view), (3, 2));

    // The first non-blank motion stays inside a line without a non-blank character.
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveFirstNonBlank,
        None,
    );
    assert_eq!(position(&view), (3, 2));

    // A word motion crosses the blank line and stops on the empty line. The
    // window owns the cursor, so the run starts from a fresh window.
    let mut state = EditingState::new();
    let mut view = window(10, 80);
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveNextWordStart,
        count(3),
    );
    assert_eq!(position(&view), (2, 0), "an empty line is one word");
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveNextWordStart,
        None,
    );
    assert_eq!(position(&view), (4, 0), "the blank line holds no word");
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MovePreviousWordStart,
        None,
    );
    assert_eq!(position(&view), (2, 0));
}

#[test]
fn word_motions_separate_words_from_punctuation() {
    let mut text = buffer("foo.bar  baz");
    let starts: &[(u32, usize)] = &[(1, 3), (2, 4), (3, 9), (4, 11)];
    for (repeat, column) in starts {
        let mut state = EditingState::new();
        let mut view = window(10, 80);
        apply(
            &mut text,
            &mut state,
            &mut view,
            Command::MoveNextWordStart,
            count(*repeat),
        );
        assert_eq!(position(&view), (0, *column), "next word start {repeat}");
    }

    let ends: &[(u32, usize)] = &[(1, 2), (2, 3), (3, 6), (4, 11)];
    for (repeat, column) in ends {
        let mut state = EditingState::new();
        let mut view = window(10, 80);
        apply(
            &mut text,
            &mut state,
            &mut view,
            Command::MoveNextWordEnd,
            count(*repeat),
        );
        assert_eq!(position(&view), (0, *column), "next word end {repeat}");
    }

    let backward: &[(u32, usize)] = &[(1, 9), (2, 4), (3, 3), (4, 0)];
    for (repeat, column) in backward {
        let mut state = EditingState::new();
        let mut view = window(10, 80);
        apply(&mut text, &mut state, &mut view, Command::MoveLineEnd, None);
        apply(
            &mut text,
            &mut state,
            &mut view,
            Command::MovePreviousWordStart,
            count(*repeat),
        );
        assert_eq!(
            position(&view),
            (0, *column),
            "previous word start {repeat}"
        );
    }
}

#[test]
fn word_motions_count_characters_and_not_bytes() {
    let mut text = buffer("héllo wörld\nλ+β");
    let mut state = EditingState::new();
    let mut view = window(10, 80);

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveNextWordEnd,
        None,
    );
    assert_eq!(
        position(&view),
        (0, 4),
        "the multi-byte word ends at column four"
    );

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveNextWordStart,
        None,
    );
    assert_eq!(position(&view), (0, 6));

    // A Greek letter is a word character, and the plus sign is punctuation.
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveNextWordStart,
        None,
    );
    assert_eq!(position(&view), (1, 0));
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveNextWordStart,
        None,
    );
    assert_eq!(position(&view), (1, 1));
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveNextWordStart,
        None,
    );
    assert_eq!(position(&view), (1, 2));
}

#[test]
fn vertical_movement_keeps_one_preferred_column() {
    let mut text = buffer("abcdefgh\nx\nabcdefgh");
    let mut state = EditingState::new();
    let mut view = window(10, 80);

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveRight,
        count(4),
    );
    assert_eq!(position(&view), (0, 4));
    apply(&mut text, &mut state, &mut view, Command::MoveDown, None);
    assert_eq!(
        position(&view),
        (1, 0),
        "the short line shortens the cursor column"
    );
    apply(&mut text, &mut state, &mut view, Command::MoveDown, None);
    assert_eq!(
        position(&view),
        (2, 4),
        "the long line restores the preferred column"
    );
    apply(&mut text, &mut state, &mut view, Command::MoveUp, count(2));
    assert_eq!(position(&view), (0, 4));

    // A horizontal motion replaces the preferred column.
    apply(&mut text, &mut state, &mut view, Command::MoveLeft, None);
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveDown,
        count(2),
    );
    assert_eq!(position(&view), (2, 3));
}

#[test]
fn the_end_of_line_motion_keeps_the_cursor_at_every_line_end() {
    let mut text = buffer("abcdefgh\nx\nabcd");
    let mut state = EditingState::new();
    let mut view = window(10, 80);

    apply(&mut text, &mut state, &mut view, Command::MoveLineEnd, None);
    assert_eq!(position(&view), (0, 7));
    apply(&mut text, &mut state, &mut view, Command::MoveDown, None);
    assert_eq!(position(&view), (1, 0));
    apply(&mut text, &mut state, &mut view, Command::MoveDown, None);
    assert_eq!(
        position(&view),
        (2, 3),
        "the end-of-line preference survives a short line"
    );
}

#[test]
fn each_visual_mode_produces_its_own_selection_shape() {
    let mut text = buffer("alpha\nbeta\ngamma\n");

    let mut state = EditingState::new();
    let mut view = window(10, 80);
    assert!(
        state.selection(&text, &view).is_none(),
        "Normal mode holds no selection"
    );

    apply(&mut text, &mut state, &mut view, Command::EnterVisual, None);
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveRight,
        count(2),
    );
    assert_eq!(state.mode(), Mode::Visual);
    match state
        .selection(&text, &view)
        .expect("Visual mode holds a selection")
    {
        Selection::Characterwise(range) => {
            assert_eq!(range.start().get(), 0);
            assert_eq!(range.end().get(), 3);
        }
        other => panic!("Visual mode produced {other:?}"),
    }

    // Each mode starts from a fresh window, because the window owns the cursor.
    let mut state = EditingState::new();
    let mut view = window(10, 80);
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::EnterVisualLine,
        None,
    );
    apply(&mut text, &mut state, &mut view, Command::MoveDown, None);
    match state
        .selection(&text, &view)
        .expect("Visual Line mode holds a selection")
    {
        Selection::Linewise { first, last } => {
            assert_eq!((first.get(), last.get()), (0, 1));
        }
        other => panic!("Visual Line mode produced {other:?}"),
    }

    let mut state = EditingState::new();
    let mut view = window(10, 80);
    apply(&mut text, &mut state, &mut view, Command::MoveRight, None);
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::EnterVisualBlock,
        None,
    );
    apply(&mut text, &mut state, &mut view, Command::MoveDown, None);
    apply(&mut text, &mut state, &mut view, Command::MoveRight, None);
    match state
        .selection(&text, &view)
        .expect("Visual Block mode holds a selection")
    {
        Selection::Block {
            first_line,
            last_line,
            left,
            right,
        } => {
            assert_eq!((first_line.get(), last_line.get()), (0, 1));
            assert_eq!((left.get(), right.get()), (1, 2));
        }
        other => panic!("Visual Block mode produced {other:?}"),
    }
}

#[test]
fn a_selection_grows_in_both_directions_from_its_anchor() {
    let mut text = buffer("alpha beta\n");
    let mut state = EditingState::new();
    let mut view = window(10, 80);

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveRight,
        count(4),
    );
    apply(&mut text, &mut state, &mut view, Command::EnterVisual, None);
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveLeft,
        count(2),
    );
    match state
        .selection(&text, &view)
        .expect("Visual mode holds a selection")
    {
        Selection::Characterwise(range) => {
            assert_eq!((range.start().get(), range.end().get()), (2, 5));
        }
        other => panic!("Visual mode produced {other:?}"),
    }
}

#[test]
fn a_change_between_visual_modes_keeps_the_anchor() {
    let mut text = buffer("alpha\nbeta\ngamma\n");
    let mut state = EditingState::new();
    let mut view = window(10, 80);

    apply(&mut text, &mut state, &mut view, Command::MoveDown, None);
    apply(&mut text, &mut state, &mut view, Command::EnterVisual, None);
    apply(&mut text, &mut state, &mut view, Command::MoveDown, None);
    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::EnterVisualLine,
        None,
    );
    match state.mode_state(&text, &view) {
        ModeState::VisualLine { anchor } => assert_eq!(anchor.get(), 1),
        other => panic!("the mode state is {other:?}"),
    }

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::ReturnToNormal,
        None,
    );
    assert_eq!(state.mode(), Mode::Normal);
    assert!(state.selection(&text, &view).is_none());
}

#[test]
fn insert_mode_allows_one_more_column_than_the_other_modes() {
    let mut text = buffer("ab\n");
    let mut state = EditingState::new();
    let mut view = window(10, 80);

    apply(&mut text, &mut state, &mut view, Command::MoveLineEnd, None);
    assert_eq!(
        position(&view),
        (0, 1),
        "Normal mode stays on the last character"
    );

    state.enter_mode(&text, &mut view, Mode::Insert);
    state.move_to(&text, &mut view, 0, 2);
    assert_eq!(
        position(&view),
        (0, 2),
        "Insert mode stands after the last character"
    );

    state.enter_mode(&text, &mut view, Mode::Normal);
    assert_eq!(
        position(&view),
        (0, 1),
        "the mode change clamps the cursor again"
    );
}

#[test]
fn a_query_stays_bounded_and_holds_one_line() {
    assert_eq!(
        SearchQuery::new("", SearchDirection::Forward).unwrap_err(),
        SearchError::Empty
    );
    assert_eq!(
        SearchQuery::new("a\nb", SearchDirection::Forward).unwrap_err(),
        SearchError::MultipleLines
    );
    let long = "a".repeat(super::SEARCH_QUERY_CHARS_MAX + 1);
    assert_eq!(
        SearchQuery::new(&long, SearchDirection::Forward).unwrap_err(),
        SearchError::TooLong {
            chars: super::SEARCH_QUERY_CHARS_MAX + 1,
            max: super::SEARCH_QUERY_CHARS_MAX,
        }
    );
    assert!(SearchQuery::new(&long[..1], SearchDirection::Forward).is_ok());
}

#[test]
fn the_case_rule_selects_the_matches() {
    let text = buffer("foo bar\nFoo baz\nfoo qux\n");
    let expected: &[(CaseSensitivity, &str, &[usize])] = &[
        (CaseSensitivity::SmartCase, "foo", &[0, 8, 16]),
        (CaseSensitivity::SmartCase, "Foo", &[8]),
        (CaseSensitivity::Sensitive, "foo", &[0, 16]),
        (CaseSensitivity::Sensitive, "Foo", &[8]),
        (CaseSensitivity::Insensitive, "foo", &[0, 8, 16]),
        (CaseSensitivity::Insensitive, "Foo", &[0, 8, 16]),
        (CaseSensitivity::SmartCase, "zzz", &[]),
    ];

    for (case_sensitivity, query, positions) in expected {
        let settings = SearchSettings {
            case_sensitivity: *case_sensitivity,
            ..SearchSettings::default()
        };
        let query = SearchQuery::new(query, SearchDirection::Forward)
            .expect("the test query holds one short line");
        let found: Vec<usize> = query
            .matches(&text, &settings)
            .iter()
            .map(|position| position.get())
            .collect();
        assert_eq!(found, *positions, "{case_sensitivity:?} {}", query.text());
    }
}

#[test]
fn search_moves_forward_and_backward_and_wraps() {
    let mut text = buffer("foo bar\nFoo baz\nfoo qux\n");
    let settings = SearchSettings::default();
    let query = SearchQuery::new("foo", SearchDirection::Forward)
        .expect("the test query holds one short line");

    let mut state = EditingState::new();
    let mut view = window(10, 80);

    let outcome = search(
        &mut text,
        &query,
        &settings,
        &mut state,
        &mut view,
        Command::SearchNext,
    );
    assert_eq!(outcome, CommandOutcome::Applied);
    assert_eq!(position(&view), (1, 0));

    search(
        &mut text,
        &query,
        &settings,
        &mut state,
        &mut view,
        Command::SearchNext,
    );
    assert_eq!(position(&view), (2, 0));

    search(
        &mut text,
        &query,
        &settings,
        &mut state,
        &mut view,
        Command::SearchNext,
    );
    assert_eq!(
        position(&view),
        (0, 0),
        "the forward search wraps at the buffer end"
    );

    search(
        &mut text,
        &query,
        &settings,
        &mut state,
        &mut view,
        Command::SearchPrevious,
    );
    assert_eq!(
        position(&view),
        (2, 0),
        "the backward search wraps at the buffer start"
    );

    search(
        &mut text,
        &query,
        &settings,
        &mut state,
        &mut view,
        Command::SearchPrevious,
    );
    assert_eq!(position(&view), (1, 0));
}

#[test]
fn a_backward_query_reverses_both_search_commands() {
    let mut text = buffer("foo bar\nFoo baz\nfoo qux\n");
    let settings = SearchSettings::default();
    let query = SearchQuery::new("foo", SearchDirection::Backward)
        .expect("the test query holds one short line");

    let mut state = EditingState::new();
    let mut view = window(10, 80);

    search(
        &mut text,
        &query,
        &settings,
        &mut state,
        &mut view,
        Command::SearchNext,
    );
    assert_eq!(
        position(&view),
        (2, 0),
        "the backward query wraps to the last match"
    );

    search(
        &mut text,
        &query,
        &settings,
        &mut state,
        &mut view,
        Command::SearchPrevious,
    );
    assert_eq!(position(&view), (0, 0));
}

#[test]
fn a_search_without_a_match_or_without_a_query_moves_nothing() {
    let mut text = buffer("foo bar\n");
    let settings = SearchSettings::default();
    let missing = SearchQuery::new("zzz", SearchDirection::Forward)
        .expect("the test query holds one short line");

    let mut state = EditingState::new();
    let mut view = window(10, 80);
    let outcome = search(
        &mut text,
        &missing,
        &settings,
        &mut state,
        &mut view,
        Command::SearchNext,
    );
    assert_eq!(outcome, CommandOutcome::SearchMissed);
    assert_eq!(position(&view), (0, 0));

    let outcome = apply(&mut text, &mut state, &mut view, Command::SearchNext, None);
    assert_eq!(outcome, CommandOutcome::SearchMissed);
    assert_eq!(position(&view), (0, 0));
}

#[test]
fn an_accepted_query_moves_the_cursor_and_the_viewport() {
    let text = buffer("alpha\nbeta\ngamma\n");
    let settings = EditorSettings::default();
    let query = SearchQuery::new("gam", SearchDirection::Forward)
        .expect("the test query holds one short line");
    let context = CommandContext {
        buffer: &text,
        settings: &settings,
        search: Some(&query),
    };

    let mut state = EditingState::new();
    let mut view = window(2, 80);
    assert_eq!(
        state.search(&context, &mut view, &query),
        CommandOutcome::Applied
    );
    assert_eq!(position(&view), (2, 0));
    assert_eq!(view.first_line(), 1, "the viewport follows the match");
}

#[test]
fn search_matches_characters_and_not_bytes() {
    let text = buffer("héllo wörld\n");
    let settings = SearchSettings::default();
    let query = SearchQuery::new("wörld", SearchDirection::Forward)
        .expect("the test query holds one short line");

    let matches = query.matches(&text, &settings);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].get(), 6);

    // The smart-case rule folds a multi-byte character too.
    let folded = SearchQuery::new("wörld", SearchDirection::Forward)
        .expect("the test query holds one short line");
    assert_eq!(folded.matches(&buffer("WÖRLD"), &settings).len(), 1);
}

#[test]
fn the_viewport_keeps_the_scroll_margin_at_both_edges() {
    let text = buffer(&"line\n".repeat(100));
    let display = DisplaySettings::default();
    let limit = ColumnLimit::LastCharacter;

    // The viewport moves as little as possible, so a jump down stops with the
    // cursor two rows above the last window row.
    let base = viewport(10, 80).reconciled(&text, Cursor::clamped(&text, 20, 0, limit), &display);
    assert_eq!(base.first_line(), 13);

    // A cursor inside both margins leaves the viewport unchanged.
    let steady = base.reconciled(&text, Cursor::clamped(&text, 16, 0, limit), &display);
    assert_eq!(steady.first_line(), 13);

    // The top edge scrolls up two rows before the cursor reaches the first row.
    let top = base.reconciled(&text, Cursor::clamped(&text, 14, 0, limit), &display);
    assert_eq!(top.first_line(), 12);

    // The bottom edge scrolls down two rows before the cursor reaches the last row.
    let bottom = viewport(10, 80).reconciled(&text, Cursor::clamped(&text, 8, 0, limit), &display);
    assert_eq!(bottom.first_line(), 1);

    // The margin stops at the last line, so the viewport shows the buffer end.
    let end = viewport(10, 80).reconciled(&text, Cursor::clamped(&text, 100, 0, limit), &display);
    assert_eq!(end.first_line(), 91);
}

#[test]
fn a_viewport_smaller_than_twice_the_margin_reduces_the_margin() {
    let text = buffer(&"line\n".repeat(100));
    let display = DisplaySettings::default();
    let limit = ColumnLimit::LastCharacter;

    // Three rows leave one row of margin.
    let small = viewport(3, 80).reconciled(&text, Cursor::clamped(&text, 5, 0, limit), &display);
    assert_eq!(small.first_line(), 4);

    // One row leaves no margin, and the cursor line stays visible.
    let single = viewport(1, 80).reconciled(&text, Cursor::clamped(&text, 5, 0, limit), &display);
    assert_eq!(single.first_line(), 5);
}

#[test]
fn the_horizontal_offset_follows_the_cursor_column() {
    let text = buffer(&"x".repeat(100));
    let display = DisplaySettings::default();
    let limit = ColumnLimit::LastCharacter;

    let start = viewport(10, 20).reconciled(&text, Cursor::clamped(&text, 0, 0, limit), &display);
    assert_eq!(start.left_column(), 0);

    let scrolled = start.reconciled(&text, Cursor::clamped(&text, 0, 30, limit), &display);
    assert_eq!(scrolled.left_column(), 15);

    let back = scrolled.reconciled(&text, Cursor::clamped(&text, 0, 16, limit), &display);
    assert_eq!(back.left_column(), 12);
}

#[test]
fn an_alignment_command_overrides_the_scroll_margin() {
    let mut text = buffer(&"line\n".repeat(100));
    let mut state = EditingState::new();
    let mut view = window(10, 80);

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::MoveLastLine,
        count(51),
    );
    assert_eq!(view.cursor().line().get(), 50);
    assert_eq!(
        view.first_line(),
        43,
        "the margin rule places the cursor near the bottom"
    );

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::AlignCursorLineTop,
        None,
    );
    assert_eq!(view.first_line(), 50);

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::CenterCursorLine,
        None,
    );
    assert_eq!(view.first_line(), 45);

    apply(
        &mut text,
        &mut state,
        &mut view,
        Command::AlignCursorLineBottom,
        None,
    );
    assert_eq!(view.first_line(), 41);
}

#[test]
fn an_alignment_near_the_buffer_start_stops_at_the_first_line() {
    let text = buffer(&"line\n".repeat(100));
    let cursor = Cursor::clamped(&text, 2, 0, ColumnLimit::LastCharacter);
    let view = viewport(10, 80);

    assert_eq!(view.aligned(cursor, ViewportAlignment::Top).first_line(), 2);
    assert_eq!(
        view.aligned(cursor, ViewportAlignment::Center).first_line(),
        0
    );
    assert_eq!(
        view.aligned(cursor, ViewportAlignment::Bottom).first_line(),
        0
    );
}

#[test]
fn no_command_of_this_slice_changes_the_buffer() {
    let mut text = buffer(SAMPLE);
    let mut state = EditingState::new();
    let mut view = window(6, 20);
    for command in MOTION_COMMANDS {
        apply(&mut text, &mut state, &mut view, *command, count(3));
    }
    assert_eq!(text.to_string(), SAMPLE);
    assert_eq!(text.version().get(), 0);
    assert!(!text.is_modified());
}

#[test]
fn a_command_of_a_later_slice_stays_unhandled() {
    let mut text = buffer(SAMPLE);
    let mut state = EditingState::new();
    let mut view = window(10, 80);
    for command in [
        Command::SaveBuffer,
        Command::SplitAdaptive,
        Command::ToggleComment,
        Command::GoToDefinition,
    ] {
        assert_eq!(
            apply(&mut text, &mut state, &mut view, command, None),
            CommandOutcome::Unhandled,
            "{command}"
        );
    }
    assert_eq!(position(&view), (0, 0));
}

#[test]
fn a_full_page_move_keeps_two_lines_of_overlap() {
    // A page move that skipped the overlap would leave the reader without a
    // visible anchor from the previous view.
    assert_eq!(viewport(20, 80).full_page_rows(), 18);
    assert_eq!(viewport(10, 80).full_page_rows(), 8);
    // A window too small to hold the overlap still moves.
    assert_eq!(viewport(3, 80).full_page_rows(), 1);
    assert_eq!(viewport(1, 80).full_page_rows(), 1);
}
