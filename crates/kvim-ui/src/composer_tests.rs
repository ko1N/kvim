//! Simulations of the workspace composer.
//!
//! The fixture below is one host. It names its own surfaces, its own commands,
//! and its own binding scopes, so every test drives the composer exactly as an
//! external host does.

use std::cell::Cell;
use std::fmt;
use std::num::NonZeroU16;
use std::sync::Arc;
use std::time::Duration;

use ratatui::layout::Rect;

use kvim_keymap::{
    Binding, CommandMetadata, CommandOwner, ContextGeneration, Input, InputContextSnapshot, Key,
    KeyCode, PasteText, Phase, Registry, Resolver, Scope, SemanticPhases, TextFallback, TypedText,
};

use crate::composer::{Composition, CompositionEffect, ResumeError, UnknownSurface};
use crate::layout::RegionKind;
use crate::window::{
    ChildSide, CloseOutcome, Direction, LayoutChange, LayoutFit, Orientation, RegionError,
    SidebarSide, WindowLimits,
};
use crate::{RowKind, SidebarRow, SidebarState, WorkspaceComposer};

/// The surfaces that this host owns.
const CHAT: &str = "chat";
const EDITOR: &str = "editor";
const REVIEW: &str = "review";
const TREE: &str = "tree";
const PALETTE: &str = "palette";

/// The area of the composed workspace.
const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 120,
    height: 40,
};

/// The width of the sidebar of this host, in cells.
const SIDEBAR_CELLS: u16 = 24;

/// The longest sequence that the table binds.
const KEYS_MAX: u8 = 2;

/// The wait before the which-key overlay first appears.
const WHICH_KEY_DELAY: Duration = Duration::from_millis(500);

/// The elapsed time that every test stamps on its input.
const NOW: Duration = Duration::ZERO;

/// The commands that this host binds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Action {
    Quit,
    OpenPalette,
    AcceptPalette,
    FocusLeft,
    FirstLine,
    EditorDown,
    ReviewNext,
    SidebarDown,
    ChatSend,
}

impl fmt::Display for Action {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for Action {
    fn id(&self) -> &str {
        match self {
            Self::Quit => "quit",
            Self::OpenPalette => "open-palette",
            Self::AcceptPalette => "accept-palette",
            Self::FocusLeft => "focus-left",
            Self::FirstLine => "first-line",
            Self::EditorDown => "editor-down",
            Self::ReviewNext => "review-next",
            Self::SidebarDown => "sidebar-down",
            Self::ChatSend => "chat-send",
        }
    }

    fn label(&self) -> &str {
        "one bounded host command"
    }
}

/// The binding tables that this host declares.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Table {
    Global,
    Palette,
    Chat,
    EditorNormal,
    EditorInsert,
    Review,
    Sidebar,
}

impl fmt::Display for Table {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Global => "Global",
            Self::Palette => "Palette",
            Self::Chat => "Chat",
            Self::EditorNormal => "Editor Normal",
            Self::EditorInsert => "Editor Insert",
            Self::Review => "Review",
            Self::Sidebar => "Sidebar",
        })
    }
}

impl Scope for Table {
    const COUNT: usize = 7;
}

type Composer = WorkspaceComposer<&'static str, Action, Table>;

fn ch(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

/// Builds the one shared registry of this host.
fn resolver() -> Resolver<Action, Table> {
    let first_line = [ch('g'); 2];
    let bindings = [
        Binding::host(Table::Global, &[ch('q')], Action::Quit),
        Binding::host(Table::Global, &[ch('p')], Action::OpenPalette),
        Binding::host(Table::Global, &[ch('h')], Action::FocusLeft),
        Binding::host(Table::Global, &first_line, Action::FirstLine),
        // The palette answers before the host-global table, so it takes `q`
        // back from the quit command while it stays open.
        Binding::surface(Table::Palette, &[ch('q')], Action::AcceptPalette),
        Binding::surface(Table::Chat, &[Key::plain(KeyCode::Enter)], Action::ChatSend),
        Binding::surface(Table::EditorNormal, &[ch('j')], Action::EditorDown),
        Binding::surface(Table::Review, &[ch('n')], Action::ReviewNext),
        Binding::surface(Table::Sidebar, &[ch('j')], Action::SidebarDown),
    ];
    let registry = Registry::from_bindings(&bindings, KEYS_MAX).expect("the table validates");
    Resolver::new(Arc::new(registry), KEYS_MAX, WHICH_KEY_DELAY)
}

/// Returns the idle context of one scope.
fn idle(scope: Table) -> InputContextSnapshot<Table> {
    InputContextSnapshot::idle(scope)
}

/// Returns one context that holds exactly one pending phase.
fn pending(scope: Table, phases: SemanticPhases) -> InputContextSnapshot<Table> {
    InputContextSnapshot {
        scope,
        phases,
        text_fallback: TextFallback::None,
        generation: ContextGeneration::FIRST.advanced(),
    }
}

/// Returns every phase set alone, with the name that the plan uses.
fn each_phase() -> [(&'static str, SemanticPhases); 5] {
    [
        (
            "count",
            SemanticPhases {
                count: Phase::Pending,
                ..SemanticPhases::IDLE
            },
        ),
        (
            "operator",
            SemanticPhases {
                operator: Phase::Pending,
                ..SemanticPhases::IDLE
            },
        ),
        (
            "register",
            SemanticPhases {
                register: Phase::Pending,
                ..SemanticPhases::IDLE
            },
        ),
        (
            "text object",
            SemanticPhases {
                text_object: Phase::Pending,
                ..SemanticPhases::IDLE
            },
        ),
        (
            "prompt",
            SemanticPhases {
                prompt: Phase::Pending,
                ..SemanticPhases::IDLE
            },
        ),
    ]
}

/// Builds one workspace: a left sidebar, one editor window, and one review
/// window beside it. The editor holds the focus.
fn workspace() -> Composer {
    let mut composer = WorkspaceComposer::new(
        EDITOR,
        idle(Table::EditorNormal),
        AREA,
        WindowLimits::default(),
        resolver(),
    );
    composer.set_global_scope(Some(Table::Global));
    let review = composer
        .split(Orientation::Vertical, ChildSide::Second)
        .expect("the area is wide");
    composer
        .replace_surface(review, REVIEW, idle(Table::Review))
        .expect("the split created the window");
    composer
        .open_sidebar(SidebarSide::Left, SIDEBAR_CELLS, TREE, idle(Table::Sidebar))
        .expect("the tree issues one identity");
    let editor = composer.tree().window_ids()[0];
    let effect = composer
        .focus_region(editor)
        .expect("the editor window is visible");
    assert_eq!(effect, CompositionEffect::Applied);
    composer
}

/// Returns the region of the named surface.
fn region_of(composer: &Composer, surface: &str) -> crate::WindowId {
    composer
        .layout()
        .surfaces()
        .iter()
        .find(|placement| placement.surface == surface)
        .map(|placement| placement.region)
        .unwrap_or_else(|| panic!("the layout shows {surface}"))
}

#[test]
fn the_overlay_scope_answers_before_the_host_and_the_focused_surface() {
    let mut composer = workspace();
    assert_eq!(
        composer.reduce(Input::Key(ch('q')), Some(NOW)),
        Composition::Host {
            command: Action::Quit
        },
        "no overlay is open, so the host-global table answers"
    );

    let effect = composer.open_overlay(PALETTE, Table::Palette, AREA, idle(Table::Palette));
    assert_eq!(effect, CompositionEffect::Applied);
    assert_eq!(
        composer.reduce(Input::Key(ch('q')), Some(NOW)),
        Composition::Surface {
            surface: PALETTE,
            command: Action::AcceptPalette
        },
        "the open overlay owns input above the host-global table"
    );
    assert_eq!(
        composer.focused_surface(),
        &EDITOR,
        "overlay ownership leaves the focused surface unchanged"
    );

    assert_eq!(composer.close_overlay(), CompositionEffect::Applied);
    assert_eq!(
        composer.reduce(Input::Key(ch('q')), Some(NOW)),
        Composition::Host {
            command: Action::Quit
        }
    );
}

#[test]
fn a_host_global_binding_answers_from_every_surface() {
    let mut composer = workspace();
    for surface in [EDITOR, REVIEW, TREE] {
        let region = region_of(&composer, surface);
        composer
            .focus_region(region)
            .expect("the region is visible");
        assert_eq!(
            composer.reduce(Input::Key(ch('p')), Some(NOW)),
            Composition::Host {
                command: Action::OpenPalette
            },
            "the host-global table must answer while {surface} holds the focus"
        );
    }
}

#[test]
fn each_focused_surface_answers_from_its_own_table() {
    let mut composer = workspace();
    assert_eq!(
        composer.reduce(Input::Key(ch('j')), Some(NOW)),
        Composition::Surface {
            surface: EDITOR,
            command: Action::EditorDown
        }
    );

    let review = region_of(&composer, REVIEW);
    composer
        .focus_region(review)
        .expect("the review is visible");
    assert_eq!(
        composer.reduce(Input::Key(ch('n')), Some(NOW)),
        Composition::Surface {
            surface: REVIEW,
            command: Action::ReviewNext
        }
    );

    let tree = region_of(&composer, TREE);
    composer.focus_region(tree).expect("the sidebar is visible");
    assert_eq!(
        composer.reduce(Input::Key(ch('j')), Some(NOW)),
        Composition::Surface {
            surface: TREE,
            command: Action::SidebarDown
        },
        "the focused sidebar owns its own keys"
    );
}

#[test]
fn printable_input_and_one_paste_reach_the_text_owner_of_the_focused_scope() {
    let mut composer = workspace();
    assert_eq!(
        composer.reduce(Input::Key(ch('z')), Some(NOW)),
        Composition::Unbound { surface: EDITOR },
        "the normal scope names no text owner"
    );

    composer
        .set_context(
            &EDITOR,
            InputContextSnapshot {
                scope: Table::EditorInsert,
                phases: SemanticPhases::IDLE,
                text_fallback: TextFallback::Typed(CommandOwner::Surface),
                generation: ContextGeneration::FIRST.advanced(),
            },
        )
        .expect("the composer shows the editor");
    assert_eq!(
        composer.reduce(Input::Key(ch('z')), Some(NOW)),
        Composition::Text {
            surface: EDITOR,
            owner: CommandOwner::Surface,
            text: TypedText::Typed('z')
        }
    );

    let block = PasteText::new("two lines").expect("the block is bounded");
    assert_eq!(
        composer.reduce(Input::Paste(block.clone()), Some(NOW)),
        Composition::Text {
            surface: EDITOR,
            owner: CommandOwner::Surface,
            text: TypedText::Pasted(block)
        }
    );
}

#[test]
fn unsupported_terminal_input_reaches_the_surface_that_owns_input() {
    let mut composer = workspace();
    assert_eq!(
        composer.reduce(Input::Unsupported, Some(NOW)),
        Composition::Unsupported { surface: EDITOR }
    );
}

#[test]
fn a_pending_prefix_reports_pending_and_then_reaches_its_command() {
    let mut composer = workspace();
    assert_eq!(
        composer.reduce(Input::Key(ch('g')), Some(NOW)),
        Composition::Pending
    );
    assert_eq!(composer.resolver().pending_keys().len(), 1);
    assert_eq!(
        composer.reduce(Input::Key(ch('g')), Some(NOW)),
        Composition::Host {
            command: Action::FirstLine
        }
    );
    assert!(composer.resolver().pending_keys().is_empty());
}

#[test]
fn a_committed_transition_clears_the_pending_key_prefix() {
    let mut composer = workspace();
    assert_eq!(
        composer.reduce(Input::Key(ch('g')), Some(NOW)),
        Composition::Pending
    );
    let review = region_of(&composer, REVIEW);
    let effect = composer
        .focus_region(review)
        .expect("the review is visible");
    assert_eq!(effect, CompositionEffect::Applied);
    assert!(
        composer.resolver().pending_keys().is_empty(),
        "a focus change must never leave a prefix of the previous surface"
    );
}

#[test]
fn directional_focus_crosses_the_editor_and_review_boundary() {
    let mut composer = workspace();
    assert_eq!(composer.focused_surface(), &EDITOR);
    assert_eq!(
        composer.focus_direction(Direction::Right),
        CompositionEffect::Applied
    );
    assert_eq!(composer.focused_surface(), &REVIEW);
    assert_eq!(
        composer.reduce(Input::Key(ch('n')), Some(NOW)),
        Composition::Surface {
            surface: REVIEW,
            command: Action::ReviewNext
        }
    );
    assert_eq!(
        composer.focus_direction(Direction::Left),
        CompositionEffect::Applied
    );
    assert_eq!(composer.focused_surface(), &EDITOR);
    assert_eq!(
        composer.focus_direction(Direction::Left),
        CompositionEffect::Applied,
        "the left neighbor of the editor is the sidebar"
    );
    assert_eq!(composer.focused_surface(), &TREE);
    assert_eq!(
        composer.focus_direction(Direction::Left),
        CompositionEffect::Unchanged,
        "the sidebar sits at the edge, so the host decides what lies beyond it"
    );
}

#[test]
fn a_split_keeps_the_surface_and_the_host_points_the_new_window_at_another_one() {
    let mut composer = workspace();
    let chat = composer
        .split(Orientation::Horizontal, ChildSide::Second)
        .expect("the area is tall");
    assert_eq!(
        composer.focused_surface(),
        &EDITOR,
        "the new window shows the surface of its source, so input keeps its owner"
    );
    composer
        .replace_surface(chat, CHAT, idle(Table::Chat))
        .expect("the split created the window");
    assert_eq!(composer.focused_surface(), &CHAT);
    assert_eq!(
        composer.reduce(Input::Key(Key::plain(KeyCode::Enter)), Some(NOW)),
        Composition::Surface {
            surface: CHAT,
            command: Action::ChatSend
        }
    );
    assert_eq!(
        composer.focus_direction(Direction::Up),
        CompositionEffect::Applied
    );
    assert_eq!(composer.focused_surface(), &EDITOR);
}

#[test]
fn a_replaced_surface_leaves_the_composer_with_its_context() {
    let mut composer = workspace();
    let review = region_of(&composer, REVIEW);
    composer
        .replace_surface(review, CHAT, idle(Table::Chat))
        .expect("the window exists");
    assert_eq!(composer.context(&REVIEW), None);
    assert_eq!(
        composer.set_context(&REVIEW, idle(Table::Review)),
        Err(UnknownSurface)
    );
}

#[test]
fn a_hidden_and_an_unknown_region_both_report_their_own_error() {
    let mut composer = workspace();
    composer.set_sidebar_visible(SidebarSide::Left, false);
    let sidebar = composer
        .tree()
        .sidebar(SidebarSide::Left)
        .expect("the sidebar keeps its identity")
        .id();
    assert_eq!(
        composer.focus_region(sidebar),
        Err(RegionError::Hidden(sidebar))
    );
}

#[test]
fn a_proposal_that_meets_a_pending_phase_returns_one_addressed_effect() {
    for (name, phases) in each_phase() {
        let mut composer = workspace();
        composer
            .set_context(&EDITOR, pending(Table::EditorNormal, phases))
            .expect("the composer shows the editor");
        let before = composer.focused_region();
        let effect = composer.focus_direction(Direction::Right);
        let CompositionEffect::CancelPending {
            surface,
            transition,
        } = effect
        else {
            panic!("a pending {name} phase must propose one addressed reset, not {effect:?}");
        };
        assert_eq!(surface, EDITOR);
        assert_eq!(composer.pending_transition(), Some(transition));
        assert_eq!(
            composer.focused_region(),
            before,
            "focus must not change before the reset is confirmed"
        );
        assert_eq!(composer.focused_surface(), &EDITOR);
    }
}

#[test]
fn a_resume_commits_the_focus_only_after_every_phase_reset() {
    for (name, phases) in each_phase() {
        let mut composer = workspace();
        let context = pending(Table::EditorNormal, phases);
        composer
            .set_context(&EDITOR, context)
            .expect("the composer shows the editor");
        let CompositionEffect::CancelPending {
            surface,
            transition,
        } = composer.focus_direction(Direction::Right)
        else {
            panic!("a pending {name} phase proposes one reset");
        };

        // A newer generation that still holds the phase changes nothing.
        let still = InputContextSnapshot {
            generation: context.generation.advanced(),
            ..context
        };
        assert_eq!(
            composer.resume_transition(transition, &surface, still),
            Err(ResumeError::StillPending { phases })
        );
        assert_eq!(composer.focused_surface(), &EDITOR);
        assert_eq!(composer.pending_transition(), Some(transition));

        let reset = InputContextSnapshot {
            scope: Table::EditorNormal,
            phases: SemanticPhases::IDLE,
            text_fallback: TextFallback::None,
            generation: context.generation.advanced(),
        };
        assert_eq!(
            composer.resume_transition(transition, &surface, reset),
            Ok(LayoutChange::Changed)
        );
        assert_eq!(composer.focused_surface(), &REVIEW);
        assert_eq!(composer.pending_transition(), None);
        assert_eq!(composer.context(&EDITOR), Some(reset));
    }
}

#[test]
fn a_stale_transition_never_commits() {
    let mut composer = workspace();
    let context = pending(
        Table::EditorNormal,
        SemanticPhases {
            count: Phase::Pending,
            ..SemanticPhases::IDLE
        },
    );
    composer
        .set_context(&EDITOR, context)
        .expect("the composer shows the editor");
    let CompositionEffect::CancelPending {
        transition: first, ..
    } = composer.focus_direction(Direction::Right)
    else {
        panic!("the pending count proposes one reset");
    };
    // The host abandons the first proposal and asks for another transition.
    let CompositionEffect::CancelPending {
        transition: second, ..
    } = composer.focus_direction(Direction::Left)
    else {
        panic!("the pending count proposes one reset");
    };
    assert_ne!(first, second);

    let reset = idle(Table::EditorNormal);
    assert_eq!(
        composer.resume_transition(first, &EDITOR, reset),
        Err(ResumeError::Stale { waiting: second })
    );
    assert_eq!(composer.focused_surface(), &EDITOR);
    assert_eq!(
        composer.resume_transition(second, &EDITOR, reset),
        Ok(LayoutChange::Changed)
    );
    assert_eq!(composer.focused_surface(), &TREE);
}

#[test]
fn a_resume_from_another_surface_never_commits() {
    let mut composer = workspace();
    composer
        .set_context(
            &EDITOR,
            pending(
                Table::EditorNormal,
                SemanticPhases {
                    operator: Phase::Pending,
                    ..SemanticPhases::IDLE
                },
            ),
        )
        .expect("the composer shows the editor");
    let CompositionEffect::CancelPending { transition, .. } =
        composer.focus_direction(Direction::Right)
    else {
        panic!("the pending operator proposes one reset");
    };
    assert_eq!(
        composer.resume_transition(transition, &REVIEW, idle(Table::Review)),
        Err(ResumeError::WrongSurface)
    );
    assert_eq!(composer.focused_surface(), &EDITOR);
    assert_eq!(composer.pending_transition(), Some(transition));
}

#[test]
fn a_resume_that_carries_the_generation_of_the_proposal_never_commits() {
    let mut composer = workspace();
    let context = pending(
        Table::EditorNormal,
        SemanticPhases {
            register: Phase::Pending,
            ..SemanticPhases::IDLE
        },
    );
    composer
        .set_context(&EDITOR, context)
        .expect("the composer shows the editor");
    let CompositionEffect::CancelPending { transition, .. } =
        composer.focus_direction(Direction::Right)
    else {
        panic!("the pending register proposes one reset");
    };

    // The phases read idle, but the generation proves that the surface
    // published no new context, so the answer describes a state that the
    // surface never reached.
    let unchanged = InputContextSnapshot {
        phases: SemanticPhases::IDLE,
        ..context
    };
    assert_eq!(
        composer.resume_transition(transition, &EDITOR, unchanged),
        Err(ResumeError::UnchangedGeneration {
            generation: context.generation
        })
    );
    assert_eq!(composer.focused_surface(), &EDITOR);
    assert_eq!(composer.pending_transition(), Some(transition));
}

#[test]
fn a_committed_proposal_cannot_commit_a_second_time() {
    let mut composer = workspace();
    let context = pending(
        Table::EditorNormal,
        SemanticPhases {
            text_object: Phase::Pending,
            ..SemanticPhases::IDLE
        },
    );
    composer
        .set_context(&EDITOR, context)
        .expect("the composer shows the editor");
    let CompositionEffect::CancelPending { transition, .. } =
        composer.focus_direction(Direction::Right)
    else {
        panic!("the pending text object proposes one reset");
    };
    let reset = InputContextSnapshot {
        generation: context.generation.advanced(),
        ..idle(Table::EditorNormal)
    };
    assert_eq!(
        composer.resume_transition(transition, &EDITOR, reset),
        Ok(LayoutChange::Changed)
    );
    assert_eq!(
        composer.resume_transition(transition, &EDITOR, reset),
        Err(ResumeError::Idle),
        "one proposal commits one transition"
    );
    assert_eq!(composer.focused_surface(), &REVIEW);
}

#[test]
fn an_overlay_transition_follows_the_same_reset_protocol() {
    let mut composer = workspace();
    let context = pending(
        Table::EditorNormal,
        SemanticPhases {
            prompt: Phase::Pending,
            ..SemanticPhases::IDLE
        },
    );
    composer
        .set_context(&EDITOR, context)
        .expect("the composer shows the editor");
    let CompositionEffect::CancelPending {
        surface,
        transition,
    } = composer.open_overlay(PALETTE, Table::Palette, AREA, idle(Table::Palette))
    else {
        panic!("the pending prompt proposes one reset");
    };
    assert_eq!(surface, EDITOR);
    assert!(
        composer.overlay_owner().is_none(),
        "overlay ownership must not change before the reset is confirmed"
    );
    let reset = InputContextSnapshot {
        generation: context.generation.advanced(),
        ..idle(Table::EditorNormal)
    };
    assert_eq!(
        composer.resume_transition(transition, &surface, reset),
        Ok(LayoutChange::Changed)
    );
    assert_eq!(composer.overlay_owner(), Some((&PALETTE, Table::Palette)));
    assert_eq!(
        composer.reduce(Input::Key(ch('q')), Some(NOW)),
        Composition::Surface {
            surface: PALETTE,
            command: Action::AcceptPalette
        }
    );
}

#[test]
fn a_proposal_that_names_the_current_state_changes_nothing() {
    let mut composer = workspace();
    let editor = composer.focused_region();
    assert_eq!(
        composer.focus_region(editor),
        Ok(CompositionEffect::Unchanged)
    );
    assert_eq!(composer.close_overlay(), CompositionEffect::Unchanged);
    assert_eq!(composer.pending_transition(), None);
}

#[test]
fn a_resize_moves_one_shared_edge_and_keeps_every_surface() {
    let mut composer = workspace();
    let before = composer
        .layout()
        .surfaces()
        .iter()
        .find(|placement| placement.surface == EDITOR)
        .map(|placement| placement.area)
        .expect("the editor is visible");
    assert_eq!(composer.resize(Direction::Right, 6), LayoutChange::Changed);
    let after = composer
        .layout()
        .surfaces()
        .iter()
        .find(|placement| placement.surface == EDITOR)
        .map(|placement| placement.area)
        .expect("the editor is visible");
    assert_eq!(after.width, before.width + 6);
    assert_eq!(composer.layout().surfaces().len(), 3);
}

#[test]
fn a_narrow_area_reports_a_constrained_layout_and_keeps_the_focused_surface() {
    let mut composer = workspace();
    let fit = composer.set_area(Rect::new(0, 0, 30, 10));
    assert!(
        matches!(fit, LayoutFit::Constrained { .. }),
        "a narrow area names every constraint instead of hiding a surface"
    );
    let layout = composer.layout();
    assert_eq!(layout.fit(), fit);
    assert!(
        layout
            .surfaces()
            .iter()
            .any(|placement| placement.surface == EDITOR),
        "the focused surface stays visible"
    );
    assert!(
        layout
            .surfaces()
            .iter()
            .all(|placement| placement.area.width <= 30),
        "no placement leaves the composed area"
    );
}

#[test]
fn the_layout_names_every_region_kind_and_clips_the_overlay() {
    let mut composer = workspace();
    composer.open_overlay(
        PALETTE,
        Table::Palette,
        Rect::new(100, 30, 60, 40),
        idle(Table::Palette),
    );
    let layout = composer.layout();
    let sidebar = layout
        .surfaces()
        .iter()
        .find(|placement| placement.surface == TREE)
        .expect("the sidebar is visible");
    assert_eq!(sidebar.kind, RegionKind::Sidebar(SidebarSide::Left));
    assert_eq!(sidebar.area.width, SIDEBAR_CELLS);
    assert!(
        layout
            .surfaces()
            .iter()
            .filter(|placement| placement.kind == RegionKind::Surface)
            .count()
            == 2
    );
    let overlay = layout.overlay().expect("the overlay is open");
    assert_eq!(overlay.surface, PALETTE);
    assert_eq!(overlay.area, Rect::new(100, 30, 20, 10));
}

#[test]
fn reduction_and_layout_invoke_no_host_input_or_render_callback() {
    /// One host surface with its own input and render counters.
    ///
    /// The composer stores the identity alone, so neither counter can move
    /// while the host reduces input or reads placements.
    struct HostSurface {
        inputs: Cell<usize>,
        renders: Cell<usize>,
    }

    let chat = HostSurface {
        inputs: Cell::new(0),
        renders: Cell::new(0),
    };
    let mut composer = workspace();
    let window = composer
        .split(Orientation::Horizontal, ChildSide::Second)
        .expect("the area is tall");
    composer
        .replace_surface(window, CHAT, idle(Table::Chat))
        .expect("the split created the window");

    for _ in 0..8 {
        let _outcome = composer.reduce(Input::Key(Key::plain(KeyCode::Enter)), Some(NOW));
        let _layout = composer.layout();
    }
    assert_eq!(chat.inputs.get(), 0);
    assert_eq!(chat.renders.get(), 0);

    // The host, not the composer, drives its surface.
    for placement in composer.layout().surfaces() {
        if placement.surface == CHAT {
            chat.renders.set(chat.renders.get() + 1);
        }
    }
    chat.inputs.set(chat.inputs.get() + 1);
    assert_eq!(chat.renders.get(), 1);
    assert_eq!(chat.inputs.get(), 1);
}

#[test]
fn the_which_key_view_reads_the_one_shared_registry() {
    let mut composer = workspace();
    assert_eq!(composer.which_key_deadline(), None);
    assert_eq!(
        composer.reduce(Input::Key(ch('g')), Some(NOW)),
        Composition::Pending
    );
    assert_eq!(composer.which_key_deadline(), Some(WHICH_KEY_DELAY));
    let view = composer
        .which_key(WHICH_KEY_DELAY)
        .expect("the delay passed, so the overlay is visible");
    assert_eq!(view.scope(), Table::Global);
    assert_eq!(view.hints().len(), 1);
}

#[test]
fn a_host_that_supplies_no_time_arms_no_overlay() {
    // A host that draws no which-key overlay reads no clock, so it holds the
    // composer inside pure state and stamps no elapsed time on its input.
    let mut composer = workspace();
    assert_eq!(
        composer.reduce(Input::Key(ch('g')), None),
        Composition::Pending,
        "the sequence opens without a clock"
    );
    assert_eq!(
        composer.which_key_deadline(),
        None,
        "no timer armed, so the host needs no wake"
    );
    assert!(
        composer.which_key(WHICH_KEY_DELAY).is_none(),
        "no elapsed time reveals an overlay that never armed"
    );
}

#[test]
fn the_host_arms_the_overlay_for_a_grammar_prefix_of_its_own_surface() {
    let mut composer = workspace();
    // A surface can hold its own grammar prefix, such as a decimal count. The
    // shared resolver does not own that prefix, so the host arms the overlay.
    assert_eq!(composer.which_key_deadline(), None);
    composer.arm_which_key(NOW);
    assert_eq!(
        composer.which_key_deadline(),
        None,
        "an armed overlay without pending input reports no time"
    );
    assert_eq!(
        composer.reduce(Input::Key(ch('g')), Some(NOW)),
        Composition::Pending
    );
    assert_eq!(composer.which_key_deadline(), Some(WHICH_KEY_DELAY));
}

#[test]
fn the_host_clips_its_two_line_sidebar_rows_inside_the_published_rectangle() {
    let composer = workspace();
    let sidebar = composer
        .layout()
        .surfaces()
        .iter()
        .find(|placement| placement.surface == TREE)
        .copied()
        .expect("the sidebar is visible");

    // The host owns the rows, their heights, and their meaning. It reads the
    // rectangle of the composer and clips its own placements inside it.
    let two_lines = NonZeroU16::new(2).expect("the literal 2 is not zero");
    let mut rows = SidebarState::new(sidebar.area.height);
    rows.set_rows(vec![
        SidebarRow::new(1_u32, two_lines, RowKind::Selectable),
        SidebarRow::new(2_u32, two_lines, RowKind::Selectable),
    ])
    .expect("two rows stay inside every bound");
    for placement in rows.placements() {
        let area = placement.area(sidebar.area);
        assert_eq!(area.width, SIDEBAR_CELLS);
        assert_eq!(sidebar.area.union(area), sidebar.area);
    }
}

#[test]
fn a_close_commits_at_once_even_while_the_focused_surface_holds_state() {
    // The surface that would have to reset is the surface that goes away, so
    // a close never returns `CancelPending`. See `docs/embedding.md`.
    let mut composer = workspace();
    let review = region_of(&composer, REVIEW);
    let effect = composer
        .focus_region(review)
        .expect("the review window is visible");
    assert_eq!(effect, CompositionEffect::Applied);
    composer
        .set_context(
            &REVIEW,
            pending(
                Table::Review,
                SemanticPhases {
                    count: Phase::Pending,
                    ..SemanticPhases::IDLE
                },
            ),
        )
        .expect("the composer shows the review surface");

    assert_eq!(composer.close_focused(), CloseOutcome::Closed(review));

    assert_eq!(
        composer.pending_transition(),
        None,
        "a close asks the host for no reset"
    );
    assert_eq!(
        composer.context(&REVIEW),
        None,
        "the state of the closed surface left with its region"
    );
    assert_eq!(composer.focused_surface(), &EDITOR);
}

#[test]
fn a_close_ends_every_waiting_proposal() {
    let mut composer = workspace();
    composer
        .set_context(
            &EDITOR,
            pending(
                Table::EditorNormal,
                SemanticPhases {
                    operator: Phase::Pending,
                    ..SemanticPhases::IDLE
                },
            ),
        )
        .expect("the composer shows the editor surface");
    let review = region_of(&composer, REVIEW);
    let effect = composer
        .focus_region(review)
        .expect("the review window is visible");
    let transition = match effect {
        CompositionEffect::CancelPending { transition, .. } => transition,
        other => panic!("the focused surface holds one phase: {other:?}"),
    };
    assert_eq!(composer.pending_transition(), Some(transition));

    let editor = region_of(&composer, EDITOR);
    assert_eq!(composer.close_focused(), CloseOutcome::Closed(editor));

    assert_eq!(composer.pending_transition(), None);
    assert_eq!(
        composer.resume_transition(transition, &EDITOR, idle(Table::EditorNormal)),
        Err(ResumeError::Idle),
        "the topology that the proposal addressed is gone"
    );
}

#[test]
fn a_close_that_reaches_the_last_window_changes_nothing() {
    let mut composer = workspace();
    let review = region_of(&composer, REVIEW);
    let effect = composer
        .focus_region(review)
        .expect("the review window is visible");
    assert_eq!(effect, CompositionEffect::Applied);
    assert_eq!(composer.close_focused(), CloseOutcome::Closed(review));

    // The sidebar is no window of the split tree, so one window remains.
    assert_eq!(composer.close_focused(), CloseOutcome::LastWindow);

    assert_eq!(composer.focused_surface(), &EDITOR);
    assert_eq!(composer.context(&EDITOR), Some(idle(Table::EditorNormal)));
}

#[test]
fn a_closed_sidebar_keeps_its_surface_and_its_context() {
    let mut composer = workspace();
    let tree = region_of(&composer, TREE);
    let effect = composer.focus_region(tree).expect("the sidebar is visible");
    assert_eq!(effect, CompositionEffect::Applied);

    assert_eq!(composer.close_focused(), CloseOutcome::Closed(tree));

    assert_eq!(
        composer.context(&TREE),
        Some(idle(Table::Sidebar)),
        "a hidden sidebar keeps the surface that the host opened"
    );
    assert_eq!(
        composer.set_sidebar_visible(SidebarSide::Left, true),
        LayoutChange::Changed
    );
    assert_eq!(region_of(&composer, TREE), tree);
}

#[test]
fn an_open_overlay_keeps_input_and_its_state_across_one_close() {
    // No close removes an overlay, so the overlay surface never resets and it
    // still owns every key below it.
    let mut composer = workspace();
    let effect = composer.open_overlay(PALETTE, Table::Palette, AREA, idle(Table::Palette));
    assert_eq!(effect, CompositionEffect::Applied);
    let editor = region_of(&composer, EDITOR);

    assert_eq!(composer.close_focused(), CloseOutcome::Closed(editor));

    assert_eq!(composer.input_surface(), &PALETTE);
    assert_eq!(composer.context(&PALETTE), Some(idle(Table::Palette)));
    assert_eq!(
        composer.reduce(Input::Key(ch('q')), Some(NOW)),
        Composition::Surface {
            surface: PALETTE,
            command: Action::AcceptPalette
        },
        "the overlay still answers before the host-global table"
    );
}
