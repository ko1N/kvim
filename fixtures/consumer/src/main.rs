//! One external consumer of every public kvim package.
//!
//! The program is not an example of one feature. It exists to prove that an
//! outside repository can name the public facades through a revision-pinned Git
//! dependency, without a shared parent workspace and without a test seam.
//!
//! It compiles and runs under every combination of the public feature matrix,
//! including the default build, which bundles no grammar at all.

use std::fmt;
use std::num::NonZeroU16;
use std::sync::Arc;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use kvim_core::TextBuffer;
use kvim_editor::{
    EditContext, EditingState, RegisterValue, Registers, Viewport, WindowState,
};
use kvim_fuzzy::{rank, score_candidate};
use kvim_input::{
    Command, EditedLine, LineChange, PromptEdit, Registry as InputRegistry, Resolution,
    Resolver as InputResolver,
};
use kvim_keymap::{
    Binding, CommandMetadata, Dispatch, DispatchContext, Input, InputContextSnapshot, Key, KeyCode,
    Registry, Resolver, Scope,
};
use kvim_lsp::{DiagnosticsLimits, DocumentRevision, ManagerLimits, WaitPolicy};
use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_settings::{EditorSettings, FileSettings, InputSettings};
use kvim_syntax::{HighlightLimits, NeverCancelled, SyntaxHighlighter};
use kvim_tui::{
    COMPLETION_CANDIDATES_MAX, COMPLETION_COLUMNS_MAX, COMPLETION_ROWS_MAX, CompletionCycle,
    CompletionOutcome, EditorAccess, EditorCapacity, EditorEvent, FILE_SIDEBAR_MARK_CELLS,
    FILE_SIDEBAR_SELECTION_MARK, FileRowGit, LineCompletion, RegionFocus, Theme,
    draw_completion_menu,
};
use kvim_ui::{
    BAND_SEGMENTS_MAX, BandError, BandPlacement, BandRank, BandSegment, BandSide, ChildSide,
    ChromeBand, Orientation, SELECTOR_CANDIDATES_MAX, Selector, SelectorCandidate, WindowLimits,
    WindowTree,
};

/// The host area that this consumer paints.
const HOST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

/// The longest key sequence that the table below binds.
const KEYS_MAX: u8 = 2;

/// The wait before a which-key overlay would appear.
const WHICH_KEY_DELAY: Duration = Duration::from_millis(500);

/// The largest number of characters that the prompt line of this host accepts.
///
/// The host states this bound, because kvim publishes the line and never the
/// prompt that owns it.
const PROMPT_CHARS_MAX: usize = 256;

/// The commands that this host owns.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum HostCommand {
    Quit,
    SplitRight,
}

impl fmt::Display for HostCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for HostCommand {
    fn id(&self) -> &str {
        match self {
            Self::Quit => "quit",
            Self::SplitRight => "split-right",
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Quit => "Quit",
            Self::SplitRight => "Split right",
        }
    }
}

/// The one scope that this host owns.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Global;

impl fmt::Display for Global {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Global")
    }
}

impl Scope for Global {
    const COUNT: usize = 1;
}

/// The opaque surface identity that this host gives one window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SurfaceId(u16);

fn main() {
    check_path();
    check_syntax();
    check_keymap();
    check_fuzzy();
    check_selector();
    check_input();
    check_editor();
    check_ui();
    check_lsp();
    check_embedded_editor();
    check_prompt_line();
    check_chrome();
    println!("every public kvim facade compiles and answers.");
}

/// Names one worktree root and one safe relative path.
fn check_path() {
    let directory = std::env::temp_dir();
    let root = WorktreeRoot::open(&directory).expect("the temporary directory exists");
    let relative = WorktreeRelativePath::new("notes/todo.md").expect("the path stays inside");
    println!("root {} holds {}", root.as_path().display(), relative.as_path().display());
}

/// Highlights one fragment when the build bundles the Rust grammar.
///
/// The default build bundles no grammar, so the lookup answers `None` and the
/// consumer stays correct without a parser.
fn check_syntax() {
    let mut highlighter = SyntaxHighlighter::new();
    match kvim_syntax::language("rust") {
        Some(entry) => {
            let highlighted = highlighter
                .highlight(
                    entry,
                    "fn main() {}\n",
                    &HighlightLimits::default(),
                    &NeverCancelled,
                )
                .expect("the fragment stays inside every bound");
            println!("the Rust grammar returned {} spans", highlighted.spans().len());
        }
        None => println!("this build bundles no Rust grammar, so it highlights nothing"),
    }
}

/// Resolves one pending key sequence through one shared registry.
fn check_keymap() {
    let leader = Key::plain(KeyCode::Char(' '));
    let bindings = [
        Binding::host(Global, &[leader, Key::plain(KeyCode::Char('q'))], HostCommand::Quit),
        Binding::host(
            Global,
            &[leader, Key::plain(KeyCode::Char('v'))],
            HostCommand::SplitRight,
        ),
    ];
    let registry = Registry::from_bindings(&bindings, KEYS_MAX).expect("the table validates");
    let mut resolver = Resolver::new(Arc::new(registry), KEYS_MAX, WHICH_KEY_DELAY);
    let context = DispatchContext::focused(InputContextSnapshot::idle(Global));
    // This consumer draws no which-key overlay and holds no clock, so it
    // supplies no elapsed time and arms no timer.
    let pending = resolver.dispatch(&context, Input::Key(leader), None);
    assert_eq!(pending, Dispatch::Pending, "the leader opens a sequence");
    println!("the leader key answers {pending:?}");
}

/// Ranks one list of host values against one query.
///
/// The scorer and the ranking rule name no path and no buffer, so this
/// consumer orders its own values with them and writes no sort of its own.
fn check_fuzzy() {
    let rows = ["first session", "second session", "notes"];
    let scored = score_candidate("ses", rows[0], "").expect("the first row holds the query");
    let ranked = rank("ses", rows.iter().map(|row| (*row, "")));
    println!(
        "the first row scores {scored} and the query ses ranks {} of {} rows",
        ranked.len(),
        rows.len()
    );
}

/// Selects one host value through the domain-neutral selector.
///
/// The selector names no path, no buffer, and no file, so this consumer drives
/// it over its own identity and takes the bounded query, the ranked match
/// list, and the selection without a second implementation.
fn check_selector() {
    let mut selector: Selector<SurfaceId> = Selector::default();
    selector.set_candidates(
        vec![
            SelectorCandidate::new(SurfaceId(1), "first session", "worktree"),
            SelectorCandidate::new(SurfaceId(2), "second session", "worktree"),
        ],
        false,
    );
    selector.set_query("second");
    selector.select_next();
    let selected = selector
        .selected()
        .expect("the query keeps the second session");
    println!(
        "the selector keeps {} candidate of at most {SELECTOR_CANDIDATES_MAX} and selects {:?}",
        selector.matches().len(),
        selected.id()
    );
}

/// Reads one resolved command, count, and register name from the preset.
///
/// A host that resolves keys itself takes all three from `Resolution::Command`
/// and hands them to the editor. A dropped register name would send every
/// operation to the unnamed register.
fn check_input() {
    let mut resolver = InputResolver::new(InputRegistry::first_release(), InputSettings::default());
    let quote = Key::plain(KeyCode::Char('"'));
    let name = Key::plain(KeyCode::Char('a'));
    let yank = Key::plain(KeyCode::Char('y'));
    // `"` opens the selection and `a` names the register. The operator key that
    // follows carries that name into the completed operation.
    let _ = resolver.resolve(quote, Duration::ZERO);
    let _ = resolver.resolve(name, Duration::ZERO);
    match resolver.resolve(yank, Duration::ZERO) {
        Resolution::Command {
            command,
            count,
            register,
        } => {
            assert_eq!(command, Command::YankOverMotion, "`y` starts the operator");
            assert_eq!(register, Some('a'), "the resolver keeps the register name");
            println!("the keys answer {command} with count {count:?} and register {register:?}");
        }
        other => panic!("`\"ay` completes one command, not {other:?}"),
    }
}

/// Applies one qualified operation to one buffer.
///
/// The editor needs no terminal and no clock, so a host can put a real Vim
/// buffer behind its own text field.
fn check_editor() {
    let settings = EditorSettings::default();
    let mut buffer = TextBuffer::from_text("alpha\nbeta\n", kvim_core::BufferBytesMax::default())
        .expect("the text is small");
    let mut registers = Registers::default();
    let mut context = EditContext {
        buffer: &mut buffer,
        settings: &settings,
        search: None,
        language_indent_width: None,
        registers: &mut registers,
        applied: Vec::new(),
    };

    let rows = NonZeroU16::new(HOST_AREA.height).expect("the host area holds rows");
    let cells = NonZeroU16::new(HOST_AREA.width).expect("the host area holds cells");
    let mut window = WindowState::new(Viewport::new(rows, cells));
    let mut state = EditingState::new();

    // `"ayy` yanks the first line into the register `a`.
    state.apply_with_register(&mut context, &mut window, Command::YankOverMotion, None, Some('a'));
    state.apply(&mut context, &mut window, Command::YankOverMotion, None);
    let stored = context
        .registers
        .value(Some('a'))
        .map(RegisterValue::text)
        .expect("the yank wrote the register");
    println!("the register a holds {stored:?}");
}

/// Splits one host area between two caller-owned surfaces.
fn check_ui() {
    let mut tree = WindowTree::new(SurfaceId(1), HOST_AREA, WindowLimits::default());
    let right = tree
        .split(Orientation::Vertical, ChildSide::Second)
        .expect("the host area is wide enough for two windows");
    tree.replace_surface(right, SurfaceId(2))
        .expect("the split returned this window");

    let mut cells = Buffer::empty(HOST_AREA);
    let area = tree.layout().area(right).expect("the window is visible");
    cells.set_string(area.x, area.y, "right", ratatui::style::Style::default());
    println!("the right window sits at {area:?}");
}

/// Names the bounded values that one changed-file request carries.
fn check_lsp() {
    let limits = DiagnosticsLimits::default();
    let manager = ManagerLimits::default();
    let revision = DocumentRevision::new(1);
    let wait = WaitPolicy::Immediate;
    println!("diagnostics {limits:?} manager {manager:?} revision {revision:?} wait {wait:?}");
}

/// Names the embedded editor facade without starting a runtime.
///
/// The consumer of this facade supplies its own asynchronous runtime and its
/// own bounded spawner. This check proves that the values compile and that the
/// event vocabulary stays reachable.
fn check_embedded_editor() {
    let access = EditorAccess::ViewOnly;
    let capacity = EditorCapacity::default();
    println!("an embedded editor accepts {access:?} with {capacity:?}");
    println!("a redraw request names {}", event_name(&EditorEvent::RedrawRequested));
    println!("a staged file reports {}", git_state_name(FileRowGit::Staged));
    println!(
        "a prompt line answers {}",
        prompt_edit_name(PromptEdit::CursorWordBackward)
    );
}

/// Edits one host-owned prompt line through the published vocabulary.
///
/// The line holds the text and the cursor, and the host holds what the prompt
/// is for. Every edit reports one `LineChange`, and the match below names every
/// variant of it.
fn check_prompt_line() {
    let mut line = EditedLine::opened(String::from("write"), PROMPT_CHARS_MAX)
        .expect("the seed meets the limit");
    assert_eq!(line.apply(PromptEdit::CursorLineStart), LineChange::CursorMoved);
    assert_eq!(line.apply(PromptEdit::Insert('q')), LineChange::TextChanged);
    let accepted = line_change_name(line.apply(PromptEdit::Accept));
    println!(
        "a host line holds {:?} with the cursor at {} and answers {accepted}",
        line.text(),
        line.cursor()
    );
}

/// Returns the stable name of one line change.
///
/// The match names every variant, so a new one stops this build until the host
/// decides what it means for its own line.
fn line_change_name(change: LineChange) -> &'static str {
    match change {
        LineChange::TextChanged => "text-changed",
        LineChange::CursorMoved => "cursor-moved",
        LineChange::Unchanged => "unchanged",
        LineChange::Deferred => "deferred",
    }
}

/// Builds one band of host-owned parts and one menu of host-owned candidates.
///
/// The band answers where every kept part sits, and the menu paints itself, so
/// this host writes no shedding rule and no second menu.
fn check_chrome() {
    let segments = vec![
        BandSegment::left(" ONLINE ", BandRank::new(2)),
        BandSegment::right("3 unread ", BandRank::new(0)),
    ];
    assert!(segments.len() <= BAND_SEGMENTS_MAX);
    let band = match ChromeBand::new(segments) {
        Ok(band) => band,
        Err(BandError::Limit { actual, max }) => panic!("{actual} segments pass the bound {max}"),
    };
    let row = Rect::new(0, 0, 40, 1);
    let kept: Vec<BandPlacement<'_>> = band.placements(row);
    let sides: Vec<BandSide> = kept.iter().map(|placement| placement.segment.side).collect();
    println!("a host band keeps {} parts on the sides {sides:?}", kept.len());

    let candidates = vec![String::from("write"), String::from("wq")];
    assert!(candidates.len() <= COMPLETION_CANDIDATES_MAX);
    let completion = LineCompletion::open("w", candidates, PROMPT_CHARS_MAX, CompletionCycle::Next)
        .expect("two candidates stay inside the bound");
    let outcome = match completion.outcome() {
        CompletionOutcome::Missed => "missed",
        CompletionOutcome::Completed => "completed",
        CompletionOutcome::Listed => "listed",
    };
    let mut cells = Buffer::empty(HOST_AREA);
    draw_completion_menu(&mut cells, HOST_AREA, Theme::new(), &completion);
    println!(
        "a host menu reports {outcome} over at most {COMPLETION_ROWS_MAX} rows of \
         {COMPLETION_COLUMNS_MAX} cells and selects {:?}",
        completion.selected()
    );

    println!(
        "a host tree reserves {FILE_SIDEBAR_MARK_CELLS} cell for {FILE_SIDEBAR_SELECTION_MARK:?} \
         while its region reports {:?}",
        RegionFocus::Focused
    );
}

/// Returns the stable name of one editor event.
///
/// The match names every variant, so this build fails until the consumer
/// names a new one, which proves the exhaustive-enum contract of the facade.
fn event_name(event: &EditorEvent) -> &'static str {
    match event {
        EditorEvent::ActiveFileChanged { .. } => "active-file-changed",
        EditorEvent::FileWritten { .. } => "file-written",
        EditorEvent::WorkspaceChanged { .. } => "workspace-changed",
        EditorEvent::FileActivated { .. } => "file-activated",
        EditorEvent::RedrawRequested => "redraw-requested",
        EditorEvent::FocusBoundary(_) => "focus-boundary",
        EditorEvent::CloseRequested => "close-requested",
    }
}

/// Names one edit of a prompt line.
///
/// The match is exhaustive on purpose. `PromptEdit` names an edit that a host
/// answers, so a new variant stops this build until the host decides what the
/// edit means for its own line. `docs/architecture.md` records that rule.
fn prompt_edit_name(edit: PromptEdit) -> &'static str {
    match edit {
        PromptEdit::Insert(_) => "insert",
        PromptEdit::DeleteBackward => "delete-backward",
        PromptEdit::DeleteWordBackward => "delete-word-backward",
        PromptEdit::CursorLeft => "cursor-left",
        PromptEdit::CursorRight => "cursor-right",
        PromptEdit::CursorWordBackward => "cursor-word-backward",
        PromptEdit::CursorWordForward => "cursor-word-forward",
        PromptEdit::CursorLineStart => "cursor-line-start",
        PromptEdit::CursorLineEnd => "cursor-line-end",
        PromptEdit::CompleteNext => "complete-next",
        PromptEdit::CompletePrevious => "complete-previous",
        PromptEdit::Accept => "accept",
        PromptEdit::Cancel => "cancel",
    }
}

/// Returns the stable name of one file-sidebar Git state.
///
/// The match names every variant of a second facade enum, so the same
/// exhaustive-enum contract as `event_name` exercises `FileRowGit` too.
fn git_state_name(git: FileRowGit) -> &'static str {
    match git {
        FileRowGit::Ignored => "ignored",
        FileRowGit::Untracked => "untracked",
        FileRowGit::Staged => "staged",
        FileRowGit::Modified => "modified",
        FileRowGit::StagedAndModified => "staged-and-modified",
        FileRowGit::Conflicted => "conflicted",
    }
}
