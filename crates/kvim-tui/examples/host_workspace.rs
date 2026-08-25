//! Composes one complete host-owned workspace around one embedded kvim editor.
//!
//! The example is one embedding host. It needs no terminal, no network, and no
//! checkout of its own. It creates one temporary repository, captures one
//! worktree diff, and composes four surfaces through one
//! [`WorkspaceComposer`]: a host-owned chat panel, one real [`EmbeddedEditor`],
//! one real review surface, and one two-line sidebar. One shared registry
//! answers every key, and the host paints every surface into one cell buffer
//! that it owns.
//!
//! The run proves six facts of `docs/embedding.md`:
//!
//! - the composer owns no surface value and no host command: it routes one key
//!   to one host command, one surface command, one text owner, one pending
//!   sequence, or one unbound result;
//! - one host-global binding answers from every surface, and each focused
//!   surface answers from its own table;
//! - focus crosses the chat, editor, review, and sidebar boundaries, and the
//!   host keeps the final focus policy;
//! - a focus move that meets pending editor state returns one addressed
//!   `CompositionEffect::CancelPending`, and focus changes only after the host
//!   reset that editor and resumed the proposal;
//! - the review publishes one typed `ReviewEvent`, and kvim gives that comment
//!   no host meaning;
//! - one layout pass returns the clipped rectangle of every visible surface and
//!   of the which-key overlay, and the host renders each one itself.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p kvim-tui --example host_workspace
//! ```

use std::error::Error;
use std::fmt;
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use tokio::time::sleep;

use kvim_input::{BindingScope, Command, Mode};
use kvim_keymap::{
    Binding, CommandMetadata, CommandOwner, Input, InputContextSnapshot, Key, KeyCode, Registry,
    Resolver, Scope, ScopedWhichKeyHint, TextFallback, TypedText,
};
use kvim_path::{WorktreeRelativePath, WorktreeRoot};
use kvim_runtime::{
    ProcessOutput, ProcessRequest, PublicationGate, RequestSlot, Runtime, RuntimeLimits,
};
use kvim_settings::EditorSettings;
use kvim_tui::{EditorEvent, EditorShutdown, EmbeddedEditor};
use kvim_ui::{
    ChildSide, CloseOutcome, Composition, CompositionEffect, Direction, Orientation, RegionKind,
    RowKind, SidebarInput, SidebarMotion, SidebarRow, SidebarSide, SidebarState, SurfacePlacement,
    WhichKeyHint, WhichKeyOverlay, WhichKeyStyles, WindowLimits, WorkspaceComposer,
};
use kvim_workspace::temp::TempRepository;
use kvim_workspace::{
    BaseRevision, CommentBody, DiffComparison, DiffSide, DiffTarget, HunkStep, ReviewEvent,
    ReviewRow, ReviewState, TargetAuthority, WorktreeDiff, WorktreeDiffRead, WorktreeDiffRequest,
};

/// The file that the workspace shows and reviews.
const DOCUMENT: &str = "src/main.rs";

/// The exact text that the base commit holds.
const BASE_TEXT: &str = "fn main() {\n    let timeout = 30;\n}\n";

/// The exact text that the working tree holds.
const REVIEWED_TEXT: &str = "fn main() {\n    let timeout = 90;\n}\n";

/// The comment that the reader submits on the review surface.
const COMMENT: &str = "name the unit: timeout_seconds";

/// The rectangle of the cells that the host owns.
const HOST_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 100,
    height: 24,
};

/// The width of the sidebar of this host, in cells.
const SIDEBAR_CELLS: u16 = 22;

/// The longest key sequence that the shared registry binds.
const KEYS_MAX: u8 = 2;

/// The wait before the which-key overlay first appears.
const WHICH_KEY_DELAY: Duration = Duration::from_millis(500);

/// The elapsed time that the host stamps on every input of this run.
///
/// Every composed part reads no clock, so the host owns this value.
const NOW: Duration = Duration::ZERO;

/// The largest number of commands that one complete diff capture runs.
const CAPTURE_COMMANDS_MAX: usize = 64;

/// The steps that the editor loop runs before it reports a defect.
const DRIVE_STEPS_MAX: usize = 64;

/// The time that one step of the editor loop waits for a result.
const STEP_DEADLINE: Duration = Duration::from_secs(10);

/// The time that the host gives the background work of the editor at exit.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

/// The rows of the composed frame that the run prints.
const PRINTED_ROWS: u16 = 24;

/// The surfaces that this host owns.
///
/// The composer copies this identity and nothing else. Every transcript,
/// buffer, diff, and row list stays with the host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Surface {
    Chat,
    Editor,
    Review,
    Tree,
}

/// The commands that this host binds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Action {
    CloseFocused,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    ChatSend,
    EditorInsert,
    EditorReturnToNormal,
    EditorOpenCommandLine,
    ReviewNextHunk,
    ReviewComment,
    SidebarDown,
    SidebarUp,
}

impl fmt::Display for Action {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for Action {
    fn id(&self) -> &str {
        match self {
            Self::CloseFocused => "close-focused",
            Self::FocusLeft => "focus-left",
            Self::FocusRight => "focus-right",
            Self::FocusUp => "focus-up",
            Self::FocusDown => "focus-down",
            Self::ChatSend => "chat-send",
            Self::EditorInsert => "editor-insert",
            Self::EditorReturnToNormal => "editor-normal",
            Self::EditorOpenCommandLine => "editor-command-line",
            Self::ReviewNextHunk => "review-next-hunk",
            Self::ReviewComment => "review-comment",
            Self::SidebarDown => "sidebar-down",
            Self::SidebarUp => "sidebar-up",
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::CloseFocused => "Close the focused region",
            Self::FocusLeft => "Focus the surface on the left",
            Self::FocusRight => "Focus the surface on the right",
            Self::FocusUp => "Focus the surface above",
            Self::FocusDown => "Focus the surface below",
            Self::ChatSend => "Send the chat message",
            Self::EditorInsert => "Insert before the cursor",
            Self::EditorReturnToNormal => "Return to Normal mode",
            Self::EditorOpenCommandLine => "Open the command line",
            Self::ReviewNextHunk => "Go to the next hunk",
            Self::ReviewComment => "Comment on the selected lines",
            Self::SidebarDown => "Select the row below",
            Self::SidebarUp => "Select the row above",
        }
    }
}

/// The binding tables that this host declares.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Table {
    Global,
    Chat,
    EditorNormal,
    EditorInsert,
    EditorPrompt,
    Review,
    Tree,
}

impl fmt::Display for Table {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Global => "Global",
            Self::Chat => "Chat",
            Self::EditorNormal => "Editor Normal",
            Self::EditorInsert => "Editor Insert",
            Self::EditorPrompt => "Editor Prompt",
            Self::Review => "Review",
            Self::Tree => "File Tree",
        })
    }
}

impl Scope for Table {
    const COUNT: usize = 7;
}

impl Table {
    /// Returns the host table that one editor scope maps to.
    ///
    /// The embedded editor publishes its own scope. The host owns the shared
    /// registry, so it names the table that answers for that scope.
    const fn of_editor(scope: BindingScope) -> Self {
        match scope {
            BindingScope::Mode(Mode::Insert) => Self::EditorInsert,
            BindingScope::Prompt | BindingScope::Confirmation => Self::EditorPrompt,
            _ => Self::EditorNormal,
        }
    }
}

/// The rows that the sidebar of this host shows.
///
/// Each row occupies two terminal rows: one name line and one note line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TreeRow {
    name: &'static str,
    note: &'static str,
}

/// The rows of the sidebar, in display order.
const TREE_ROWS: [TreeRow; 3] = [
    TreeRow {
        name: "src/main.rs",
        note: "modified against the base",
    },
    TreeRow {
        name: "Cargo.toml",
        note: "unchanged",
    },
    TreeRow {
        name: "README.md",
        note: "unchanged",
    },
];

fn ch(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

fn ctrl(value: char) -> Key {
    Key::ctrl(KeyCode::Char(value))
}

/// Builds the one shared registry of this workspace.
///
/// Every surface contributes its own table, and the host-global table holds the
/// focus keys, so no surface owns a second binding table.
fn shared_resolver() -> Resolver<Action, Table> {
    let leader = ch(' ');
    let bindings = [
        Binding::host(Table::Global, &[ctrl('h')], Action::FocusLeft),
        Binding::host(Table::Global, &[ctrl('l')], Action::FocusRight),
        Binding::host(Table::Global, &[ctrl('k')], Action::FocusUp),
        Binding::host(Table::Global, &[ctrl('j')], Action::FocusDown),
        Binding::host(Table::Global, &[leader, ch('h')], Action::FocusLeft),
        Binding::host(Table::Global, &[leader, ch('l')], Action::FocusRight),
        Binding::host(Table::Global, &[leader, ch('q')], Action::CloseFocused),
        Binding::surface(Table::Chat, &[Key::plain(KeyCode::Enter)], Action::ChatSend),
        Binding::surface(Table::EditorNormal, &[ch('i')], Action::EditorInsert),
        Binding::surface(
            Table::EditorNormal,
            &[ch(':')],
            Action::EditorOpenCommandLine,
        ),
        Binding::surface(
            Table::EditorInsert,
            &[Key::plain(KeyCode::Esc)],
            Action::EditorReturnToNormal,
        ),
        Binding::surface(Table::Review, &[ch('n')], Action::ReviewNextHunk),
        Binding::surface(Table::Review, &[ch('c')], Action::ReviewComment),
        Binding::surface(Table::Tree, &[ch('j')], Action::SidebarDown),
        Binding::surface(Table::Tree, &[ch('k')], Action::SidebarUp),
    ];
    let registry = Registry::from_bindings(&bindings, KEYS_MAX).expect("the table validates");
    Resolver::new(Arc::new(registry), KEYS_MAX, WHICH_KEY_DELAY)
}

/// The host-owned state of every surface except the editor.
struct Host {
    chat: Vec<String>,
    draft: String,
    review: ReviewState,
    tree: SidebarState<&'static str>,
}

impl Host {
    /// Returns the context that the chat surface publishes.
    ///
    /// The chat reads text, so it names itself as the owner of printable input.
    fn chat_context(&self) -> InputContextSnapshot<Table> {
        InputContextSnapshot {
            text_fallback: TextFallback::Typed(CommandOwner::Surface),
            ..InputContextSnapshot::idle(Table::Chat)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // One temporary repository holds the base commit and the working change.
    let repository = TempRepository::new("host-workspace");
    repository.file(DOCUMENT, BASE_TEXT);
    repository.commit("the review base");
    let base = BaseRevision::new(&repository.head())?;
    repository.file(DOCUMENT, REVIEWED_TEXT);

    let candidate = capture(repository.path(), base, DiffTarget::Worktree).await?;
    println!("captured revision {}", candidate.revision());

    let root = Arc::new(WorktreeRoot::open(repository.path())?);
    let mut settings = EditorSettings::default();
    settings.files.undo_file = false;
    let mut editor = EmbeddedEditor::builder(Arc::clone(&root), HOST_AREA)
        .settings(settings)
        .open()?;
    println!("editor instance {}", editor.instance().get());
    let _redraw = editor.open_file(WorktreeRelativePath::new(DOCUMENT)?);
    drive(&mut editor, |event| {
        matches!(event, EditorEvent::ActiveFileChanged { .. })
    })
    .await?;

    let mut tree = SidebarState::new(HOST_AREA.height);
    let two_lines = NonZeroU16::new(2).expect("the literal 2 is not zero");
    tree.set_rows(
        TREE_ROWS
            .iter()
            .map(|row| SidebarRow::new(row.name, two_lines, RowKind::Selectable))
            .collect(),
    )?;
    let mut host = Host {
        chat: vec!["reviewer: the timeout changed".to_owned()],
        draft: String::new(),
        review: ReviewState::new(candidate),
        tree,
    };

    // The composer joins the four surfaces. It receives identities, geometry,
    // and contexts, and it never sees a transcript, a buffer, or a diff.
    let mut composer = WorkspaceComposer::new(
        Surface::Chat,
        host.chat_context(),
        HOST_AREA,
        WindowLimits::default(),
        shared_resolver(),
    );
    composer.set_global_scope(Some(Table::Global));
    let editor_window = composer.split(Orientation::Vertical, ChildSide::Second)?;
    composer.replace_surface(editor_window, Surface::Editor, editor_context(&editor))?;
    let review_window = composer.split(Orientation::Horizontal, ChildSide::Second)?;
    composer.replace_surface(
        review_window,
        Surface::Review,
        InputContextSnapshot::idle(Table::Review),
    )?;
    composer.open_sidebar(
        SidebarSide::Left,
        SIDEBAR_CELLS,
        Surface::Tree,
        InputContextSnapshot::idle(Table::Tree),
    )?;
    let chat_window = composer.tree().window_ids()[0];
    let _effect = composer.focus_region(chat_window)?;
    println!(
        "the workspace shows {} regions",
        composer.layout().surfaces().len()
    );

    // The chat owns printable input, so its own table and the text fallback of
    // its context both reach the same surface. The leader key stays out of the
    // message, because the host-global table answers before every fallback.
    for key in "looks-good".chars().map(ch) {
        step(&mut composer, &mut host, &mut editor, Input::Key(key)).await?;
    }
    step(
        &mut composer,
        &mut host,
        &mut editor,
        Input::Key(Key::plain(KeyCode::Enter)),
    )
    .await?;

    // One host-global binding moves the focus across a surface boundary.
    step(&mut composer, &mut host, &mut editor, Input::Key(ctrl('l'))).await?;
    println!("focus: {:?}", composer.focused_surface());

    // The editor answers its own table, and Insert mode takes printable input
    // as literal buffer text.
    for key in [ch('i'), ch('/'), ch('/'), Key::plain(KeyCode::Esc)] {
        step(&mut composer, &mut host, &mut editor, Input::Key(key)).await?;
    }

    // The editor opens its command line, which leaves its prompt phase pending.
    step(&mut composer, &mut host, &mut editor, Input::Key(ch(':'))).await?;
    println!(
        "the editor publishes the phases {:?}",
        editor.input_context().phases
    );

    // The focus move now needs a semantic reset, so the composer proposes one
    // addressed effect and keeps the focus where it is.
    step(&mut composer, &mut host, &mut editor, Input::Key(ctrl('j'))).await?;
    println!("focus: {:?}", composer.focused_surface());

    // The review surface answers its own table and publishes one typed event.
    step(&mut composer, &mut host, &mut editor, Input::Key(ch('n'))).await?;
    step(&mut composer, &mut host, &mut editor, Input::Key(ch('c'))).await?;
    while let Some(ReviewEvent::CommentSubmitted { anchor, body }) = host.review.take_event() {
        // kvim publishes the comment as one domain-neutral fact. The host
        // decides what it means.
        println!(
            "review event: comment on line {} of {}: {}",
            anchor.location().first(),
            anchor.path().as_path().display(),
            body.as_str()
        );
    }

    // Focus crosses back to the sidebar, which owns its own keys.
    step(&mut composer, &mut host, &mut editor, Input::Key(ctrl('h'))).await?;
    step(&mut composer, &mut host, &mut editor, Input::Key(ctrl('h'))).await?;
    step(&mut composer, &mut host, &mut editor, Input::Key(ch('j'))).await?;
    println!(
        "focus: {:?}, selected row: {:?}",
        composer.focused_surface(),
        host.tree.selected()
    );

    // One leader key opens a pending sequence, so the which-key overlay reads
    // the same registry that dispatch reads.
    step(&mut composer, &mut host, &mut editor, Input::Key(ch(' '))).await?;
    let hints: Vec<(String, String)> = composer
        .which_key(WHICH_KEY_DELAY)
        .map(|view| {
            view.hints()
                .iter()
                .map(|hint: &ScopedWhichKeyHint<Action, Table>| {
                    (
                        hint.hint().key_label().to_string(),
                        hint.hint().target().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    println!("which-key rows: {hints:?}");

    // The leader key above still waits, so one more key completes that
    // sequence and closes the focused region. The focused sidebar hides first
    // and keeps its surface, so the host can show it again unchanged.
    step(&mut composer, &mut host, &mut editor, Input::Key(ch('q'))).await?;
    println!(
        "the workspace shows {} regions, focus: {:?}",
        composer.layout().surfaces().len(),
        composer.focused_surface()
    );

    // A close from a window removes that window. It goes away with its own
    // semantic state, so the composer asks for no reset and the focus lands on
    // a surviving region at once.
    for key in [ch(' '), ch('q')] {
        step(&mut composer, &mut host, &mut editor, Input::Key(key)).await?;
    }
    println!(
        "the workspace shows {} regions, focus: {:?}",
        composer.layout().surfaces().len(),
        composer.focused_surface()
    );

    // One layout pass, one host cell buffer, one frame.
    let mut cells = CellBuffer::empty(HOST_AREA);
    render(&composer, &host, &mut editor, &hints, &mut cells)?;
    print_frame(&cells);

    match editor.shutdown(SHUTDOWN_DEADLINE).await {
        EditorShutdown::Finished { events } => {
            println!(
                "the shutdown finished with {} remaining events",
                events.len()
            );
        }
        EditorShutdown::Draining(drain) => {
            println!(
                "the drain delivered {} remaining events",
                drain.complete().await.len()
            );
        }
    }
    Ok(())
}

/// Returns the context that the embedded editor publishes.
///
/// The editor names its own scope and its own phases. The host maps that scope
/// onto the table of the shared registry and keeps every other fact.
fn editor_context(editor: &EmbeddedEditor) -> InputContextSnapshot<Table> {
    let published = editor.input_context();
    InputContextSnapshot {
        scope: Table::of_editor(published.scope),
        phases: published.phases,
        text_fallback: published.text_fallback,
        generation: published.generation,
    }
}

/// Feeds one input to the composer and applies what it named.
///
/// The composer routes the input. This function is the complete host side: it
/// runs host commands itself, hands surface commands and text to the surface
/// that owns input, and republishes the context of that surface.
async fn step(
    composer: &mut WorkspaceComposer<Surface, Action, Table>,
    host: &mut Host,
    editor: &mut EmbeddedEditor,
    input: Input,
) -> Result<(), Box<dyn Error>> {
    match composer.reduce(input, Some(NOW)) {
        Composition::Host { command } => {
            apply_host(composer, host, editor, command).await?;
        }
        Composition::Surface { surface, command } => {
            apply_surface(composer, host, editor, surface, command).await?;
        }
        Composition::Text {
            surface,
            owner: _,
            text,
        } => {
            apply_text(composer, host, editor, surface, &text)?;
        }
        // A pending sequence changes only the which-key overlay, and an
        // unbound or unsupported input changes nothing at all.
        Composition::Pending | Composition::Unsupported { .. } | Composition::Unbound { .. } => {}
    }
    Ok(())
}

/// Runs one host-global command.
///
/// Focus policy stays here. The composer proposes, and the host decides how to
/// answer an addressed reset.
async fn apply_host(
    composer: &mut WorkspaceComposer<Surface, Action, Table>,
    host: &mut Host,
    editor: &mut EmbeddedEditor,
    command: Action,
) -> Result<(), Box<dyn Error>> {
    if command == Action::CloseFocused {
        // A close needs no reset handshake. The surface that would have to
        // reset is the surface that goes away, so the composer commits at
        // once. One remaining window returns the decision to this host.
        match composer.close_focused() {
            CloseOutcome::Closed(region) => println!("the host closed the region {region:?}"),
            CloseOutcome::LastWindow => println!("one window remains, so the host would exit"),
        }
        return Ok(());
    }
    let direction = match command {
        Action::FocusLeft => Direction::Left,
        Action::FocusRight => Direction::Right,
        Action::FocusUp => Direction::Up,
        Action::FocusDown => Direction::Down,
        other => return Err(format!("the host-global table binds no {other}").into()),
    };
    match composer.focus_direction(direction) {
        CompositionEffect::Applied | CompositionEffect::Unchanged => Ok(()),
        CompositionEffect::CancelPending {
            surface,
            transition,
        } => {
            println!("the composer asks {surface:?} to reset its pending input");
            // The host applies the addressed effect to its own surface and
            // resumes with the context that the surface published afterwards.
            let context = reset_surface(host, editor, surface).await;
            composer.resume_transition(transition, &surface, context)?;
            Ok(())
        }
    }
}

/// Resets the pending semantic state of one surface.
async fn reset_surface(
    host: &mut Host,
    editor: &mut EmbeddedEditor,
    surface: Surface,
) -> InputContextSnapshot<Table> {
    match surface {
        Surface::Editor => {
            let _reduction = editor.cancel_pending(NOW);
            let _redraw = editor.dispatch();
            editor_context(editor)
        }
        Surface::Chat => {
            host.draft.clear();
            host.chat_context()
        }
        Surface::Review => InputContextSnapshot::idle(Table::Review),
        Surface::Tree => InputContextSnapshot::idle(Table::Tree),
    }
}

/// Runs one command on the surface that owns input.
async fn apply_surface(
    composer: &mut WorkspaceComposer<Surface, Action, Table>,
    host: &mut Host,
    editor: &mut EmbeddedEditor,
    surface: Surface,
    command: Action,
) -> Result<(), Box<dyn Error>> {
    let context = match (surface, command) {
        (Surface::Chat, Action::ChatSend) => {
            let message = std::mem::take(&mut host.draft);
            host.chat.push(format!("you: {message}"));
            host.chat_context()
        }
        (Surface::Editor, _) => {
            let editor_command = match command {
                Action::EditorInsert => Command::InsertBeforeCursor,
                Action::EditorReturnToNormal => Command::ReturnToNormal,
                Action::EditorOpenCommandLine => Command::OpenCommandLine,
                other => return Err(format!("the editor table binds no {other}").into()),
            };
            let _reduction = editor.command(editor_command, None, None, NOW);
            let _redraw = editor.dispatch();
            editor_context(editor)
        }
        (Surface::Review, Action::ReviewNextHunk) => {
            if host.review.next_hunk() == HunkStep::AtBorder {
                println!("the review cursor stays on the last hunk");
            }
            InputContextSnapshot::idle(Table::Review)
        }
        (Surface::Review, Action::ReviewComment) => {
            let line = changed_line(&host.review)
                .ok_or("the candidate publishes one changed new-side line")?;
            host.review.select(DiffSide::New, line, 1)?;
            let authority = TargetAuthority::of(host.review.candidate());
            host.review
                .submit_comment(CommentBody::new(COMMENT)?, &authority)?;
            InputContextSnapshot::idle(Table::Review)
        }
        (Surface::Tree, Action::SidebarDown | Action::SidebarUp) => {
            let motion = if command == Action::SidebarDown {
                SidebarMotion::Down(1)
            } else {
                SidebarMotion::Up(1)
            };
            let _event = host.tree.reduce(&SidebarInput::Move(motion));
            InputContextSnapshot::idle(Table::Tree)
        }
        (surface, command) => {
            return Err(format!("{surface:?} binds no {command}").into());
        }
    };
    composer.set_context(&surface, context)?;
    Ok(())
}

/// Hands literal text to the surface that owns input.
fn apply_text(
    composer: &mut WorkspaceComposer<Surface, Action, Table>,
    host: &mut Host,
    editor: &mut EmbeddedEditor,
    surface: Surface,
    text: &TypedText,
) -> Result<(), Box<dyn Error>> {
    let context = match surface {
        Surface::Chat => {
            match text {
                TypedText::Typed(value) => host.draft.push(*value),
                TypedText::Pasted(block) => host.draft.push_str(block.as_str()),
            }
            host.chat_context()
        }
        Surface::Editor => {
            match text {
                TypedText::Typed(value) => {
                    let _reduction = editor.insert_literal(&value.to_string(), NOW);
                }
                TypedText::Pasted(block) => {
                    let _reduction = editor.paste(block, NOW);
                }
            }
            let _redraw = editor.dispatch();
            editor_context(editor)
        }
        other => return Err(format!("{other:?} takes no literal text").into()),
    };
    composer.set_context(&surface, context)?;
    Ok(())
}

/// Returns the first added new-side line of the review candidate.
///
/// An added line holds a new-side number and no old-side number, so the anchor
/// names the change itself and not a context line around it.
fn changed_line(review: &ReviewState) -> Option<u32> {
    review.rows().find_map(|row| match row {
        ReviewRow::Line { line, .. } if line.number(DiffSide::Old).is_none() => {
            line.number(DiffSide::New)
        }
        _ => None,
    })
}

/// Paints every published placement into the cell buffer of the host.
///
/// The composer draws nothing. It publishes rectangles, and this function is
/// the only code of the run that writes a cell.
fn render(
    composer: &WorkspaceComposer<Surface, Action, Table>,
    host: &Host,
    editor: &mut EmbeddedEditor,
    hints: &[(String, String)],
    cells: &mut CellBuffer,
) -> Result<(), Box<dyn Error>> {
    let layout = composer.layout();
    println!("layout fit: {:?}", layout.fit());
    for placement in layout.surfaces() {
        match placement.kind {
            RegionKind::Sidebar(_) => render_tree(host, placement, cells)?,
            RegionKind::Surface => match placement.surface {
                Surface::Chat => render_lines(cells, placement.area, &host.chat, "chat"),
                Surface::Review => render_review(host, placement.area, cells),
                Surface::Tree => {}
                Surface::Editor => {
                    // The editor accepts one rectangle first, because its
                    // layout, its viewports, and its cursor all follow it.
                    let _redraw = editor.set_area(placement.area)?;
                    let cursor = editor.draw(cells, placement.area)?;
                    println!("the editor frame asks for {:?}", cursor.shape);
                }
            },
        }
    }
    if !hints.is_empty() {
        let rows: Vec<WhichKeyHint<'_>> = hints
            .iter()
            .map(|(key, label)| WhichKeyHint::new(key, label))
            .collect();
        let accent = Style::default().fg(Color::Yellow);
        WhichKeyOverlay::new(
            " Which Key ",
            &rows,
            WhichKeyStyles {
                surface: Style::default().bg(Color::Black).fg(Color::Gray),
                title: accent,
                key: accent,
            },
        )?
        .render(cells, HOST_AREA)?;
    }
    Ok(())
}

/// Paints the two-line sidebar rows inside the published rectangle.
fn render_tree(
    host: &Host,
    placement: &SurfacePlacement<Surface>,
    cells: &mut CellBuffer,
) -> Result<(), Box<dyn Error>> {
    let selected = host.tree.selected().copied();
    host.tree.render(cells, placement.area, |canvas, row| {
        let Some(entry) = TREE_ROWS.iter().find(|entry| entry.name == *row.row()) else {
            return;
        };
        let style = if selected == Some(entry.name) {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else {
            Style::default().fg(Color::Gray)
        };
        canvas.draw(0, 0, entry.name, style);
        if canvas.lines() > 1 {
            canvas.draw(1, 2, entry.note, Style::default().fg(Color::DarkGray));
        }
    })?;
    Ok(())
}

/// Paints the published review rows inside one rectangle.
fn render_review(host: &Host, area: Rect, cells: &mut CellBuffer) {
    let mut lines = Vec::new();
    for row in host.review.rows() {
        match row {
            ReviewRow::File { file } => {
                lines.push(format!("file {}", file.path().as_path().display()))
            }
            ReviewRow::Hunk { hunk, .. } => lines.push(format!("  hunk {}", hunk.id().get())),
            ReviewRow::Line { line, .. } => {
                let text = line.text().as_str().unwrap_or("<not text>");
                lines.push(format!("   {text}"));
            }
            ReviewRow::Truncated { limit } => lines.push(format!("  omitted: {limit:?}")),
        }
    }
    render_lines(cells, area, &lines, "review");
}

/// Paints one list of lines inside one rectangle.
fn render_lines(cells: &mut CellBuffer, area: Rect, lines: &[String], title: &str) {
    if area.is_empty() {
        return;
    }
    cells.set_stringn(
        area.x,
        area.y,
        title,
        usize::from(area.width),
        Style::default().fg(Color::Cyan),
    );
    for (index, line) in lines.iter().enumerate() {
        let Ok(offset) = u16::try_from(index + 1) else {
            return;
        };
        if offset >= area.height {
            return;
        }
        cells.set_stringn(
            area.x,
            area.y + offset,
            line,
            usize::from(area.width),
            Style::default(),
        );
    }
}

/// Prints the first rows of the composed frame.
fn print_frame(cells: &CellBuffer) {
    for row in 0..PRINTED_ROWS.min(cells.area.height) {
        let mut line = String::new();
        for column in 0..cells.area.width {
            line.push_str(cells[(column, row)].symbol());
        }
        println!("|{}|", line.trim_end());
    }
}

/// Drives the editor until it publishes one event that the host waits for.
async fn drive(
    editor: &mut EmbeddedEditor,
    wanted: fn(&EditorEvent) -> bool,
) -> Result<(), Box<dyn Error>> {
    for _ in 0..DRIVE_STEPS_MAX {
        let _redraw = editor.dispatch();
        while let Some(published) = editor.take_event() {
            if wanted(&published.event) {
                return Ok(());
            }
        }
        tokio::select! {
            completed = editor.recv() => {
                let _redraw = editor.apply(completed, NOW);
            }
            () = sleep(STEP_DEADLINE) => return Err("one editor step passed its deadline".into()),
        }
    }
    Err("the editor loop stays inside its step bound".into())
}

/// Captures one worktree diff through the bounded process service.
async fn capture(
    root: &Path,
    base: BaseRevision,
    target: DiffTarget,
) -> Result<WorktreeDiff, Box<dyn Error>> {
    let root = Arc::new(WorktreeRoot::open(root)?);
    let mut request =
        WorktreeDiffRequest::new(root, DiffComparison::CommitToWorktree(base), target);
    for _ in 0..CAPTURE_COMMANDS_MAX {
        let output = run(request.command()).await;
        match request.publish(&output) {
            Ok(WorktreeDiffRead::Pending(next)) => request = *next,
            Ok(WorktreeDiffRead::Published(candidate)) => return Ok(*candidate),
            Err(failure) => return Err(format!("the capture returned {failure:?}").into()),
        }
    }
    Err(format!("one capture stays inside {CAPTURE_COMMANDS_MAX} commands").into())
}

/// Runs one bounded command through the process service of the editor.
async fn run(command: ProcessRequest) -> ProcessOutput {
    let limits = RuntimeLimits::new(1, 1, 1).expect("every capacity is nonzero");
    let (runtime, mut events) = Runtime::<ProcessOutput>::with_limits(limits);
    let handle =
        PublicationGate::default().begin(RequestSlot::new(1), &runtime.cancellation_root());
    runtime
        .submit_process(handle, command, |output| output)
        .expect("the isolated runtime holds one free permit");
    let event = events
        .recv()
        .await
        .expect("every accepted request produces one result");
    let output = event.result.expect("the host provides the git command");
    runtime.shutdown().await;
    output
}
