# Embedding

## Ownership

This document owns host, driver, embedded editor, event lifecycle, workspace
composition, external use, and public example rules.

Kvim supplies bounded library capabilities. A host composes them. Kvim knows no
host session, agent, tool, task, plan, or other host-domain concept.

## Target Facade Contract

`kvim-embed` is the only supported high-level editor facade. It publishes two
rendered editor types. `MemoryEditor` edits supplied bounded text and renders to
a caller-supplied ratatui buffer. It requires no worktree, filesystem, Git,
watcher, process, or language service. `WorktreeEditor` is a separate type
behind the `worktree` Cargo feature. It adds explicit worktree capabilities.
Do not add a common editor trait until shared behavior requires one.

The default `kvim-embed` feature set is in-memory only. It must not compile
`kvim-tui`, `kvim-runtime`, `kvim-language`, `kvim-lsp`, `kvim-workspace`,
`kvim-path`, `kvim-terminal`, Tokio, crossterm, notify, or cap-std. The
`worktree` feature enables the worktree path and may forward grammar features
only through that path.

Each editor owns bounded execution capacity internally. A host drives readiness,
result application, and shutdown through facade methods. The public facade
exposes no Tokio type, channel, generic work payload, or runtime handle. It may
name stable lower-crate values when those values own their meaning, including
commands, settings, paths, and ratatui geometry. It must not expose
`kvim-runtime`, `kvim-language`, or `kvim-workspace` types.

The host owns terminal lifecycle, terminal input, signals, raw mode, alternate
screen, panic restoration, cursor application, and final redraw scheduling.
The facade owns no such terminal operation.

`kvim-tui` remains a temporary compatibility facade during additive migration.
Migrate the executable, examples, and external consumers to `kvim-embed`.
Then remove the old facade and its unsupported public type leakage. Do not add
permanent aliases for that surface.

## Audit Invariant Ownership

Each audit item has one primary document owner. The listed owner records the
required invariant before implementation changes it.

| Finding | Owner | Required invariant |
|---|---|---|
| KV-A01 | `responsiveness.md` | A committing operation reports its actual durable outcome after commit begins. |
| KV-A02 | `text-model.md` | Text-derived requests carry stable buffer identity, generation, and version. |
| KV-A03 | `text-model.md` | A validated byte limit survives every buffer transition. |
| KV-A04 | `files.md` | Filesystem operations report `Unchanged`, `Committed`, or `Indeterminate` and reconcile uncertainty. |
| KV-A05 | `embedding.md` | Instance identity is validated in release builds before result application. |
| KV-A06 | `embedding.md` | High-level editors own bounded execution behind facade-owned values. |
| KV-A07 | `embedding.md` | Memory and worktree editors remain separate capability contracts. |
| KV-A08 | `language-services.md` | Public language construction validates every declared bound, identity, and service root. |
| KV-A09 | `settings.md` | Public settings and bounded value construction establish their invariants in release builds. |
| KV-A10 | `architecture.md` | Consumer and feature gates prove supported package contracts. |
| KV-A11 | `language-services.md` | No-grammar and grammar feature behavior remains documented and valid. |
| KV-A12 | `text-model.md` | Lower crates publish values that own their meaning and preserve text invariants. |
| KV-A13 | `architecture.md` | A supported setting must have production behavior; `DisplaySettings::wrap` is no-op and is scheduled for removal. |
| KV-A14 | `architecture.md` | A supported public path must have production behavior; stale paths are removed or implemented. |

## Legacy Embedding Contract

The sections below describe the current `kvim-tui` compatibility facade. They
do not define new supported high-level integrations. The target `kvim-embed`
contract above takes precedence where the documents differ.

## Host Responsibilities

The host owns:

- the set of worktrees, sessions, and visible surfaces,
- workspace state, focus policy, and commands,
- terminal lifecycle and terminal events,
- asynchronous runtime startup and task supervision,
- surface composition and final event effects,
- cursor application and redraw scheduling.

The host constructs the asynchronous runtime and supervises every returned
driver future. It names one `EditorCapacity` for each editor. `Isolated` builds
the bounded worker and process spawner of that editor alone. `SharedProcessPool`
builds the worker permits and the result queue of that editor and shares the one
process pool of the program. `Supplied` accepts the spawner that the host built
itself. Capacity is isolated for one instance unless the named choice shares a
pool.

The host can supply watcher and LSP handles, and it grants one
`ClipboardAccess` policy. These services are optional. The standalone `kvim`
binary constructs its implementations and grants `ClipboardAccess::System`. See
[`clipboard.md`](clipboard.md).

The standalone binary is one such host. It is the only layer that owns raw
mode, the alternate screen, standard input, standard output, the terminal event
stream, the termination signals, the panic restoration, the cursor shape, the
asynchronous runtime, the redraw schedule, and the shutdown order. `kvim-tui`
owns none of these, and a structural test in each of the two crates proves it.

The host keeps every event loop free from filesystem, process, Git, LSP,
formatter, and Tree-sitter work. It submits synchronous syntax work through its
bounded worker spawner.

## Driver Responsibilities

The legacy `EditorDriver` owns the external services of one `kvim-tui`
compatibility editor instance. The target `kvim-embed` facade owns the matching
orchestration values and internal execution capacity. Neither contract exposes
runtime handles to its host.

The driver creates no runtime and starts no detached task. It submits work
through the supplied spawner. It tracks every returned task until completion or
cancellation.

Closing one driver cannot cancel or drain another driver's services. Every
event and result carries its instance identity. Several editors can use one root
or different roots without sharing request or cancellation namespaces.

Shutdown consumes the driver. It rejects new work, cancels pre-commit work,
closes optional services, and waits for tracked work until the supplied
deadline. If the deadline expires while mandatory delivery remains, shutdown
returns a bounded, must-use `ShutdownDrain`. The drain owns the remaining tasks,
reservations, and event delivery. The host keeps its runtime alive until the
drain completes.

## Embedded Editor

`EmbeddedEditor` owns visible editor state for one explicit `WorktreeRoot`.
`EditorAccess::ViewOnly` rejects text and filesystem mutation.
`EditorAccess::ReadWrite` permits normal editing and bounded workspace writes.

Every command carries one `CommandAuthority`: `Read`, `Text`, or `Workspace`.
`kvim-input` owns that classification, and its match is exhaustive, so a new
command cannot reach an editor without an authority decision. A view-only
editor refuses every command above `Read` before any owner sees it. It refuses
literal text, paste, save, format, and workspace mutation at their own entry
points as well, so no second path reaches the buffer or the filesystem. An open
question and an open prompt line still accept literal text, because neither one
changes a file.

The editor accepts resolved surface commands, literal text, bounded paste, and
time. It does not run another key-sequence resolver. Input reduction returns an
`InputContextSnapshot` that the shared resolver uses for the next input.

`EmbeddedEditor` is the public facade of one instance. `EmbeddedEditor::builder`
takes the validated root and the first rectangle, because both bound what the
editor can reach. Every other setting has a default. `open` returns a typed
geometry error for a rectangle without cells, and it builds the model and the
driver of one instance together. `EmbeddedEditor::shutdown` consumes the editor
and returns `EditorShutdown`. `Finished` holds every remaining event.
`Draining` holds one `EditorDrain`, which owns the mandatory events of the
committed work, and the host keeps its runtime alive until that drain completes.
`crates/kvim-tui/examples/embedded_editor.rs` is one complete host of one such
editor.

The host owns the key-sequence resolver, so `EmbeddedEditor::command` takes the
command, its count, and the register that the operation names. `None` names the
unnamed register, and `Resolution::Command` carries the name that the `input`
charter resolved from a `"` prefix. A host that drops that name silently sends
every operation to the unnamed register. See [`clipboard.md`](clipboard.md).

The editor accepts only a resolved command. The host must therefore own the
table that resolves one, but it does not have to invent that table.
`kvim_input::Registry::first_release` builds kvim's own hardcoded preset.
`kvim_keymap::Registry::all_bindings` yields every `(scope, KeySequence,
BoundCommand)` triple of a registry, across every scope of the preset at
once. A host walks that one list, maps each `BindingScope` to its own scope
value, and builds its shared registry with `Registry::from_bindings`.

The host scope type must distinguish every `BindingScope` that the preset
uses. Kvim reuses one key across several tables. `j` is the clearest case: it
reaches `MoveDown` in Normal mode, a different motion in the sidebar, and a
motion under a waiting operator in the operator-pending table. A host scope
type that collapses two of those tables onto one hands `Registry::from_bindings`
two commands for one sequence in one scope. Construction then fails with
`RegistryError::DuplicateSequence`, at startup and never at dispatch. The safe
shape is one host scope variant for one `BindingScope`, for example a host
enum variant `Editor(BindingScope)`.

`Resolver::idle_which_key` lists the top-level bindings of every scope of a
context, with no pending prefix. A host that binds its own escape in the
host-global scope should read this method too: it is what keeps that escape
discoverable beside the editor's own bindings. See
[`input-actions.md`](input-actions.md).

`WorkspaceComposer::idle_which_key` is the same list for a host that composes
its workspace through the composer. It builds the context itself, from the
overlay ownership, the host-global scope, and the published context of the
surface that owns input, so the host rebuilds no context of its own. The call
takes no elapsed time and changes no state, because the which-key delay and the
overlay state govern `WorkspaceComposer::which_key` alone.

A pending prefix answers two lists, and the host draws them apart.
`WhichKeyView::hints` names the keys that continue the sequence.
`WhichKeyView::interruptions` names the keys of the preceding scopes that
abandon it. `WorkspaceComposer::which_key` returns that same view, so a host
reaches both lists from the one resolver that resolves its keys. Never draw an
idle hint among the extensions of a pending prefix. Read the interruption list
there instead, because every key of that list runs at that moment.
See [`input-actions.md`](input-actions.md).

Bound the idle list before you draw it. It can approach
`kvim_ui::WHICH_KEY_HINTS_MAX`, which is 256 and refuses a longer hint list
instead of cutting it. Kvim's own preset holds 81 distinct first keys in Normal
mode, 56 in Visual mode, and 48 in the sidebar, and the list spans up to three
scopes. A host bounds or filters the list before it hands it to
`WhichKeyOverlay`.

One frame of columns holds far fewer rows than that bound. The overlay pages an
accepted list: `WhichKeyOverlay::at_page` names the page, and the render
returns one `kvim_ui::WhichKeyPlacement` that names the drawn rows, the size of
the list, the drawn page, and the number of pages. Bind one key that steps the
page, and paint the reported position beside the overlay.
See [`input-actions.md`](input-actions.md).

`BindingScope::RegisterSelection` binds no key. It waits for one register name,
so any input that neither a binding nor its text fallback takes ends it. The
scope states that rule itself, through
`kvim_keymap::UnboundInput::Cancels` in the published
`kvim_keymap::InputContextSnapshot`. A host that owns the resolver states the
same rule for a scope of its own, so it spends no host-global chord on the
cancel. `BindingScope::unbound_input` returns the declaration of every kvim
scope, and a host that maps kvim's scopes onto its own tables must carry that
value into the snapshot it publishes.

The resolver answers such input with `Dispatch::Cancelled`, and the composer
answers it with `Composition::Cancelled`. The host closes the named scope and
runs no command. The declaration changes no precedence, because the resolver
reads it only after every scope of the order and after the text fallback. A
host-global binding, an extension of a pending prefix, and an interruption all
still win. See [`input-actions.md`](input-actions.md).

### The File Sidebar

`EmbeddedEditor` owns one lazy file tree over its worktree root. The facade
publishes that tree so a host draws a file sidebar beside the editor. The
surface names no type of `kvim-workspace`, because
[`architecture.md`](architecture.md) keeps that package out of the supported
set. It names its own vocabulary, the paths of `kvim-path`, and the geometry of
`kvim-ui`.

`EmbeddedEditor::file_rows` returns the drawable rows. One `FileRow` carries the
label, the indent guides, the depth, one `FileRowKind`, the selection, the
recorded Git state, the symbolic-link fact, and the icon role of one line. Every
accessor answers a fact and no cell, so a host that wants a look of its own
paints every cell itself. A host that wants the look of kvim hands the row to
`draw_file_row` instead. `FileRowKind` names the five states of one line: `File`,
`ClosedDirectory`, `OpenDirectory`, `LoadingDirectory`, and `Note`. A `Note` row
reports a bounded read, a failed read, or the number of entries that the
hidden-entry policy keeps out of the rows; it names no entry and takes no
selection.

`FileRow::guides` is the complete indent of the row. It already holds the one
leading blank that the file tree of kvim draws, because the workspace-root
header of that tree is no sibling of the first entries. A host that draws the
guides as they are published reproduces the look of kvim.
[`windows.md`](windows.md) owns the guide rule itself.

`FileRow::git` returns the recorded Git state of the row as `FileRowGit`, or
`None` while the row carries no state. `ThemeRole::TreeGit` names the same
state, so a host colors a row of its own from the published palette. A `Note`
row and a row of a workspace that no read has covered yet both report `None`.
The variant order rises in the same severity order as
`kvim_workspace::GitStatus`, so a host ranks two
states the way kvim ranks them. `FileRowGit::glyph` returns the mark that
kvim's own file tree draws for a state. A host that reproduces the look of
kvim draws that glyph; a host that draws its own marks matches on the state
instead.

`FileRow::is_symlink` reports whether the row names a symbolic link. The label
carries no suffix for it. `FILE_SIDEBAR_LINK_SUFFIX` is the suffix that kvim's
own file tree draws behind such a row, so a host that reproduces the look of
kvim appends that constant itself.

`FileRow::icon_role` returns the icon role of the row as `kvim_tui::IconRole`,
or `None` for a `Note` row. The role reaches the host regardless of
`FileTreeIcons`, the icon-visibility setting of kvim's own file tree, because a
host may want the role even while kvim would draw no icon of its own. kvim
publishes no icon glyph as a fact, because every glyph needs a patched font that
a host may not hold. A host that wants kvim's own icon color reads
`Theme::style(ThemeRole::Icon(role))`; the glyph stays the host's own choice,
unless the host paints through `draw_file_row`, which draws kvim's own glyph.

`FILE_SIDEBAR_ICON_CELLS` is the width that the icon column of one row takes.
A host that draws a tree of its own beside the file tree of the editor reserves
the same width, so the two icon columns line up in one window. A host needs no
icon table from kvim to do this. It chooses the glyph of its own row, because
its rows name its own domain, and it keeps kvim's gutter so the two trees read
as one surface. `kvim_settings::FileTreeIcons` hides every icon of the editor,
so one setting answers for both trees and a host adds no second switch.

`FILE_SIDEBAR_MARK_CELLS` is the width that the selection mark reserves at the
left edge of one row. A host that draws a tree of its own beside the file tree
of the editor reserves the same width as its own left column, so the two trees
line up in one window. `FILE_SIDEBAR_SELECTION_MARK` is the glyph that kvim's
own file tree draws in that column on the selected row. A host that reproduces
the look of kvim draws this glyph; a host that draws its own mark reads the
width alone and keeps its own glyph, exactly as it keeps its own glyph for
`FileRowGit::glyph`.

kvim draws `FILE_SIDEBAR_SELECTION_MARK` only while its sidebar reports
`RegionFocus::Focused`. An unfocused sidebar leaves the column blank, and the
selection band over the whole row still reports the selected row. A host that
reproduces the look of kvim shows its own mark under the same rule, or its
mark and kvim's disagree about when a row carries one. [`windows.md`](windows.md)
owns the mark rule.

kvim publishes no width for the Git mark at the right edge of a row in this
release. A host that draws no Git mark of its own reserves no cell for one, so
the gap costs it nothing. A host that wants kvim's own Git mark already reads
`FileRowGit::glyph` for the glyph; the width of that one cell stays with
`draw_file_row` until a host asks kvim to align a Git mark of its own beside
it.

#### One Row Painter

`kvim_tui::draw_file_row` paints one `FileRow` into one `kvim_ui::SidebarCanvas`
exactly as kvim's own file tree paints it. The host asked for the look of kvim
and not only for its facts, so kvim publishes the painter rather than a second
description that a host would have to reproduce.

The call takes five arguments and reads nothing else:

- the canvas, which `SidebarState::render` hands to the row callback of the
  host and which clips every draw at the edges of the row;
- the row, which carries every fact that the painter draws;
- one `Theme`, the palette that the host already holds for its own surfaces;
- one `kvim_settings::FileTreeIcons`, which decides whether a row takes an icon
  glyph or the expansion marker that needs no patched font;
- one `kvim_tui::RegionFocus`, which reports whether the sidebar of the host
  holds the input focus.

`RegionFocus` names one region, and a region is one editor window or one
sidebar. kvim uses the same value for both surfaces, so the facade publishes
one focus vocabulary and no second one can disagree with it. The focus is a
property of the sidebar and not of one row, so it reaches the painter as one
argument of the call instead of one field of every `FileRow`. A host reads the
focus of its own surfaces, so it already holds the value.

The painter owns the layout of the row. The first cell holds the selection
mark, the indent guides and the two glyph cells follow it, and the last cell
holds the Git mark. A canvas narrower than the text clips from the right edge.
The final three visible text cells then fade toward the effective row
background, while the Git mark keeps its own style in the last cell. Short
text keeps its normal color. A host that wants a different layout reads the
facts and paints its own cells instead.

The selection mark belongs to `RegionFocus::Focused` alone. A sidebar that
reports `RegionFocus::Unfocused` leaves the mark cell blank, and the selection
band over the whole row still reports the selected row. The mark cell keeps its
width in both states, so no cell of the row moves when the focus moves.
[`windows.md`](windows.md) owns the mark rule and the cursor cell that goes
with it.

kvim's own file tree draws through this one call. `crates/kvim-tui/src/tree.rs`
holds no second row-drawing path, so the look that a host reaches and the look
that the standalone editor shows cannot drift apart. The two indent guide
copies that `windows.md` records are the failure that this rule prevents.

`crates/kvim-tui/examples/embedded_file_sidebar.rs` paints its rows through the
call and prints the resulting cells.

`EmbeddedEditor::file_sidebar` applies one `FileSidebarInput`. `Move` takes one
`kvim_ui::ListMotion`, which stops at the first and the last row and never
wraps. See [List Motion](windows.md#list-motion) for the row space that
`ListMotion::ToRow` names. `Open` opens the selected directory or activates
the selected file.
`Close` closes the selected directory or selects the directory that holds the
selected row. `Activate` activates the selected file or opens and closes the
selected directory.

The reduction returns one `FileSidebarOutcome`. `Activated` carries the
contained path of one activated file, and the sidebar opened no buffer for it. A
host that shows the file calls `EmbeddedEditor::open_file` with that path.
`FileSidebarOutcome::event` converts the activation into
`EditorEvent::FileActivated` for a host that keeps one uniform event stream,
exactly as `InputRequest::event` converts a focus boundary. Nothing is queued,
so no activation waits behind another event. Every reduction latches
`RedrawRequested`.

The tree reads no directory on the host event loop. A row that needs a listing
leaves the editor as one unit of work through `EmbeddedEditor::dispatch`, and
the listing reaches the tree through `EmbeddedEditor::apply`. The host therefore
drives these reads with the one work channel that it already drives for the
editor, and it adds no second channel. A directory reports
`FileRowKind::LoadingDirectory` between the expansion and the listing.

`FileTree` governs collapse for this tree. It withholds the rows of a closed
directory itself, so a closed directory contributes no published row.
[`windows.md`](windows.md) records that decision.

`crates/kvim-tui/examples/embedded_file_sidebar.rs` is one complete host of one
such sidebar.

The host supplies a `ratatui::Rect` and `ratatui::Buffer` for rendering. The
editor accepts one explicit rectangle first, because the layout, the viewports,
and the cursor all follow that rectangle. It writes only inside that rectangle.
It validates that the rectangle holds cells, matches the accepted rectangle, and
fits the buffer before changing any cell. Invalid geometry returns a typed error
and leaves the buffer unchanged.

Rendering returns an optional cursor position and cursor-shape request. The
host decides whether to apply either request. The editor names its own cursor
shape and owns no terminal sequence.

### The Prompt Line

A prompt reads one line of text. kvim publishes the verbs of that line already,
and it publishes the line itself, so a host holds both halves and no host
writes a line without a cursor.

`kvim_input::EditedLine` is that line. It holds the text and one cursor
position, and it owns every change of the text, so the two can never disagree.
The position counts characters, because a character is the unit that a reader
inserts and deletes. It never passes the number of characters of the text, so
it always names a character boundary.

`EditedLine::apply` takes one `PromptEdit` and reports one `LineChange`:
`TextChanged` when the text changed, `CursorMoved` when only the cursor moved,
`Unchanged` when the edit changed nothing, and `Deferred` when the line answers
no such edit. The three text edits and the six motions change the line. The two
completion keys, the accept, and the cancel report `Deferred`, because a
candidate list and a prompt belong to the host and never to one line.
`EditedLine::insert`, `EditedLine::delete_backward`,
`EditedLine::delete_word_backward`, and `EditedLine::write` name the same edits
directly, for a host that reads a key table of its own.

`EditedLine::text` and `EditedLine::cursor` answer what a host draws.
`EditedLine::cursor_offset` answers the byte offset of the cursor, so a host
measures the text before it in the cells of its own terminal. The line counts
characters and never cells, so the conversion stays where the host knows the
width of a character.

The host states the bound. `EditedLine::opened` and `EditedLine::opened_at`
take the largest number of characters that the line accepts, and that bound
refuses rather than cuts: an insert above it changes nothing and reports
`Unchanged`, so no reader loses a character in silence. `opened` places the
cursor after the whole seed, and `opened_at` places it where the host names.

The line names no prompt kind, no prefix, and no completion. kvim's own prompt
is that line plus its kind and its candidate list, so the command line, the
search prompt, the picker query, and the four file-tree prompts all edit
through the published rules. `crates/kvim-tui/src/session.rs` holds no second
cursor arithmetic.

[`input-actions.md`](input-actions.md) owns the rules and the bounds.
`crates/kvim-input/examples/edited_line.rs` holds one complete line of a host.

### The Candidate Menu

A prompt line offers candidates for the text that a reader typed. kvim
publishes that menu, so a host writes no second one and no second appearance
can drift.

`LineCompletion` is the model. `LineCompletion::open` takes the typed text, the
candidates of the host, the character bound of its prompt line, and one
`CompletionCycle` that names the direction of the key that opened the menu.
`LineCompletion::cycle` moves the selection and wraps at both ends.
`LineCompletion::selected` returns the candidate that the prompt line shows,
`LineCompletion::candidates` returns every candidate, and
`LineCompletion::selected_row` names the selected one among them.
`LineCompletion::outcome` reports one `CompletionOutcome`: `Missed` while no
candidate answered the line, `Completed` while one candidate answered it alone,
and `Listed` while several candidates need a choice.
`LineCompletion::into_typed` returns the typed text and drops the menu, so a
cancelled menu restores the prompt line exactly.

The typed text and every candidate are the text of one line without the prompt
prefix. The prompt paints its own `:` or its own marker in front of that line.
The model holds line text alone, and the painter draws the model alone, so a
prompt prefix reaches no row of the menu. A host that repeats its prefix inside
a candidate therefore shows the prefix twice, which is the defect that this
split removes.

`LineCompletion` is not a `kvim_ui::Selector`. A selector ranks candidates
against a query through `kvim-fuzzy`, keeps a viewport, and stops at the first
and the last row. A menu receives candidates that a producer already filtered by
the typed prefix, ranks nothing, wraps at both ends, and restores the typed text
on a cancel. The two behaviours differ, so the facade publishes two types
instead of bending either one.

#### One Menu Painter

`kvim_tui::draw_completion_menu` paints one `LineCompletion` exactly as kvim's
own command line paints it. The call takes four arguments and reads nothing
else:

- the `ratatui::Buffer` that the host owns;
- the band that the menu may cover, which is the body band in kvim;
- one `Theme`, the palette that the host already holds for its own surfaces;
- the menu, which carries every fact that the painter draws.

The painter owns the layout. The menu takes the last rows of the band and
starts at the left edge of it, so it sits under the prompt line that it
describes and covers no band below. It keeps one cell beside its text on both
sides, and it carries no rail and no border. The selected row takes the popup
selection color of the palette.

The painter draws nothing while the menu holds one candidate alone, because one
candidate needs no choice.

`COMPLETION_ROWS_MAX` bounds the rows and `COMPLETION_COLUMNS_MAX` bounds the
width. A menu with more candidates than rows spends its last row on `...`, and a
candidate wider than the menu loses its start behind a `<`.
`COMPLETION_CANDIDATES_MAX` bounds the candidate list itself, and that bound
refuses rather than cuts: `LineCompletion::open` returns `None` for a longer
list, so a host ranks and shortens its own source instead of reading a menu that
dropped candidates in silence.

kvim's own command line draws through this one call.
`crates/kvim-tui/src/render.rs` holds no second menu-drawing path, so the look
that a host reaches and the look that the standalone editor shows cannot drift
apart.

[`windows.md`](windows.md) owns the placement rule and the bounds.
`crates/kvim-tui/examples/completion_menu.rs` opens one menu over host-owned
candidates, cycles it, draws it, and cancels it.

### The Chrome Band

A statusline, a winbar, and every other one-row band hold parts that a narrow
terminal cannot all show. Which part goes first is a rule, and kvim publishes
it.

`kvim_ui::ChromeBand` holds that rule. A host lists one `BandSegment` for each
part, with the text it already rendered, the edge that the part sits against,
and one `BandRank`. `ChromeBand::placements` answers where every kept part sits.
The lowest rank sheds first, the highest rank survives every shed, and two parts
of one rank shed the later one first. `BAND_SEGMENTS_MAX` bounds the list, and
`ChromeBand::new` refuses a longer one rather than cutting it.
[`windows.md`](windows.md) owns the rule and the bounds.

The band names no subject, no color, and no glyph. A host fills it with its own
parts and paints them with its own palette. A host reaches these values in
`kvim-ui`, the crate that already holds `SidebarCanvas` and the other drawing
values, so the band adds no dependency to a host that draws kvim rows today.

The statusline and the winbar of kvim draw through this same band, so the
precedence that a host keeps is the precedence that the standalone editor shows.
`crates/kvim-tui/src/chrome.rs` and `crates/kvim-tui/src/buffer_view.rs` hold no
shedding rule of their own.

A statusline usually names the mode. `EmbeddedEditor::mode` answers the editing
mode of the editor, and `Mode` renders its own label, so a host builds the mode
segment from that value.

`EmbeddedEditor::input_context` answers a different question. It publishes one
`InputContextSnapshot`, and its `scope` names the owner of the keys. The owner
is `BindingScope::Mode(Mode)` while the editor holds them, and it names a
prompt, the file sidebar, or the picker while one of those reads them. A host
that builds its mode segment from the scope alone therefore loses its mode
label whenever a prompt opens. The standalone editor keeps the mode on its
statusline through a prompt, and a host reaches the same fact through
`EmbeddedEditor::mode`.

`crates/kvim-ui/examples/chrome_band.rs` is one complete host of one band.

## Editor Events

`EditorEvent` includes these facts and requests:

- `ActiveFileChanged`,
- `FileWritten`,
- `WorkspaceChanged`,
- `FileActivated`,
- `RedrawRequested`,
- `FocusBoundary(Direction)`,
- `CloseRequested`.

Review uses separate typed `ReviewEvent` variants. The host decides the effect
of every editor and review event. Kvim assigns no host meaning to a review
comment.

`RedrawRequested` and `ActiveFileChanged` are coalesced latches. Each one
reports current state instead of a history, so a burst consumes no queue slot
and can never saturate the queue. The bounded queue therefore holds the
mandatory facts of the durable operations alone.

Focus-boundary and close outcomes return from the synchronous input reduction
that produced them. One reduction reports one outcome: applied, one host
request, or one typed refusal. A refused operation performs no side effect. A
full event queue returns a typed `Saturated` refusal and drops no event
silently.

## Mandatory Event Lifecycle

A successful durable side effect has one mandatory event:

| Side effect | Mandatory event |
|---|---|
| Save | `FileWritten` |
| Create, delete, rename, copy, or move | `WorkspaceChanged` |
| Review comment submission | `CommentSubmitted` |

The driver reserves one bounded outbox slot before it accepts an operation with
a side effect. If no slot is available, it returns `Saturated` before starting
the side effect. A save can format first, so the save entry point checks the
capacity before that format starts and the write itself holds the reservation.

Every accepted operation follows this state sequence:

`Reserved -> Running -> Committed -> Published`

Cancellation can stop `Reserved` or `Running` work before commit. Once commit
starts, the task masks cancellation. It reports `Committed` and uses its
reserved slot before the driver can finish shutdown. Failure releases the
reservation. The driver never detaches or aborts a task that can be committed.

This sequence guarantees delivery after a side effect succeeds. Shutdown drains
all mandatory events or returns `ShutdownDrain`. It never reports complete while
a mandatory event can remain unpublished.

## Workspace Composition

`WorkspaceComposer<SurfaceId>` lives in `kvim-ui`, because that crate owns
generic split, sidebar, and which-key presentation and depends on `kvim-keymap`
alone. The composer combines:

- one generic split tree,
- generic sidebar regions,
- overlay scope and focus,
- one shared key resolver,
- which-key state.

The composer owns no surface instance, transcript, session, worktree list, host
command, or host-domain value. The host supplies opaque surface identities,
minimum dimensions, sidebar row metrics, input contexts, bindings, and styles.
Every surface enters the composer with the `InputContextSnapshot` that it
publishes, and the host republishes that snapshot after every input.

One reduction routes a key or paste to one host command, surface command,
interrupted command, typed text owner, pending sequence, unsupported input, or
unbound result. The composer does not accept, store, or invoke a surface input
or render callback.

`Composition::Interrupted` names the key that cancelled a pending sequence. A
complete binding of a scope that precedes the scope of that sequence takes the
key, so a host-global escape leaves a focused surface at any moment. See
[`input-actions.md`](input-actions.md). The composer clears the pending key
prefix alone. The named surface still holds its own count, operator, register,
and text object, and every one of them belongs to the cancelled sequence. The
host resets that surface with `EmbeddedEditor::cancel_pending`, exactly as it
resets it for `CompositionEffect::CancelPending`, and then it runs the command
on the named owner.

The host supplies the elapsed time with each reduction, and that time reaches
the which-key overlay alone. `WorkspaceComposer::reduce` therefore takes the
same `Option<Duration>` that `Resolver::dispatch` takes. `None` states that the
host draws no which-key overlay, so pending input arms no timer and a host that
reads no clock holds one composer, and one resolver, inside pure state.

An open overlay is not a pending phase. It owns the keyboard; it does not wait
for the rest of a sequence. The context that an overlay publishes to
`WorkspaceComposer::open_overlay` is therefore idle, even when the overlay reads
one line: the surface keeps its own prompt phase to itself. A context that is
not idle makes the overlay unclosable, because `close_overlay` addresses the
surface that owns input, which is the overlay, and the only reset that empties a
prompt phase is that close.

An open overlay owns input while it stays open, so an overlay key never reaches
the focused surface below it. The focused region and the focused surface stay
unchanged while the overlay is open. This rule covers the focused scope alone.
The resolver still evaluates the host-global scope between the overlay and the
focused scope, so a key that the overlay does not bind can still reach a
host-global binding while the overlay is open. A host that binds keys in the
host-global scope must silence that scope itself while an overlay is open,
with `WorkspaceComposer::set_global_scope(None)`, or a host-global binding
fires while the overlay waits for its answer.

A focus or overlay transition that needs surface state returns one bounded,
addressed `CompositionEffect::CancelPending { surface, transition }`. Focus and
overlay ownership remain unchanged. The host applies the effect to that surface
and returns its reset `InputContextSnapshot`. `EmbeddedEditor::cancel_pending`
is that entry point for one embedded editor, and
`EmbeddedEditor::input_context` publishes the snapshot that follows it.

`resume_transition` validates the transition identity, surface identity, and
snapshot generation. It requires empty count, operator, register, text-object,
and prompt phases before it commits focus or overlay ownership. A snapshot that
carries the generation of the proposal proves that the surface published no new
context, so the composer refuses it. This protocol lets focus cross editor and
review boundaries while the host keeps final focus policy.

A split copies the surface of its source window, so the surface that owns input
does not change and no reset is needed.

`close_focused` commits at once and never returns `CancelPending`. A close needs
no reset handshake, because the surface that would have to reset is the surface
that goes away: its count, operator, register, text-object, and prompt phases
die with the region. An open overlay keeps input ownership and its own state,
because no close removes an overlay. A close ends every waiting proposal, since
the topology that the proposal addressed changed under it; the host proposes
again after the close. A tree that holds one window reports
`CloseOutcome::LastWindow` and changes nothing, so the host decides whether its
workspace ends. [`windows.md`](windows.md) owns the tree behavior of a close.

One layout pass returns sidebar, surface, and overlay placements inside the
supplied rectangle, each one clipped to that rectangle. The which-key hints come
from the same resolver through one view of the shared registry. The host renders
each owned surface. The composer performs no input or output, starts no task,
reads no clock, and owns no terminal lifecycle.

### The Standalone Editor Does Not Use The Composer

Standalone kvim is one whole workspace inside one `Session`, so the composer is
not on its path. The binary adapts `Session`, `EditorDriver`, and
`kvim-terminal`, and `Session` owns its own splits through `Windows`.

Two facts make the composer unusable for the binary today. `EmbeddedEditor`
publishes no key-sequence resolver, and `Resolution::Prompt` and
`Resolution::Confirmation` reach private `Session` methods. A binary that sat
above the embedded facade alone could therefore open neither the command line
nor a confirmation. The composer stays the facade for a host that owns several
surfaces, and the binary stays the adapter for one workspace.

`WorkspaceComposer::close_focused` and `WorkspaceComposer::arm_which_key`
therefore have no in-tree production caller. `crates/kvim-tui/examples/host_workspace.rs`
is their only in-tree driver, and that is the intended shape: the composer is a
library facade for an external host, and its dedicated example is the in-tree
proof that the facade composes. Both methods stay public.

## External Use

An external host can consume syntax, LSP, keymap, UI, or the embedded editor
independently. Syntax highlighting requires no LSP, ratatui, editor, file,
project, or runtime session. LSP is optional for highlighting and editor use.
Cargo features let consumers disable unused languages and grammars.

Public crates support revision-pinned Cargo Git dependencies without a shared
parent workspace. [`architecture.md`](architecture.md) owns package stability,
MSRV, ratatui compatibility, and the exact feature matrix.

A consumer of a private repository needs three more settings. It sets
`net.git-fetch-with-cli = true` in its Cargo configuration, because Cargo's
built-in libgit2 client fails SSH agent authentication with the error
`attempted ssh-agent authentication, but no usernames succeeded: 'git'`. That
setting makes Cargo run the `git` executable itself, so the build environment
must hold `git` and `openssh`. A hermetic build environment without them fails
outright. A developer machine can appear to work, because the user profile
supplies both programs there. The dependency URL must use the
`ssh://git@github.com/OWNER/REPO.git` form. The scp-style address
`git@github.com:OWNER/REPO.git` is not a URL, so Cargo rejects it.

## Public Examples

Every public feature API has one dedicated, hermetic example. Module rustdoc
links directly to its owning example. Continuous integration compiles and runs
every example. One combined example does not replace a feature example.

The required examples are:

- `crates/kvim-path/examples/confine_worktree_paths.rs`
- `crates/kvim-fuzzy/examples/rank_candidates.rs`
- `crates/kvim-input/examples/edited_line.rs`
- `crates/kvim-keymap/examples/dispatch_keys.rs`
- `crates/kvim-syntax/examples/highlight.rs`
- `crates/kvim-lsp/examples/lsp_diagnostics.rs`
- `crates/kvim-ui/examples/chrome_band.rs`
- `crates/kvim-ui/examples/selector.rs`
- `crates/kvim-ui/examples/sidebar.rs`
- `crates/kvim-ui/examples/split_windows.rs`
- `crates/kvim-ui/examples/tab_strip.rs`
- `crates/kvim-ui/examples/which_key.rs`
- `crates/kvim-tui/examples/completion_menu.rs`
- `crates/kvim-tui/examples/embedded_editor.rs`
- `crates/kvim-tui/examples/embedded_file_sidebar.rs`
- `crates/kvim-tui/examples/host_workspace.rs`
- `crates/kvim-tui/examples/worktree_diff_review.rs`

Each example demonstrates one feature and its minimum setup. Supporting public
types use their owning feature example. Internal helpers do not require another
example.

The LSP example starts itself as a deterministic fixture server. A UI example
renders into a test buffer, or prints the state that it drives when the feature
paints no cell. `host_workspace.rs` composes host-owned chat, a real
embedded editor, a real review surface, and sidebar surfaces through one shared
resolver. Editor, composition, and review examples use temporary worktrees.

No example requires a user-installed server, network access, terminal ownership,
or this repository as input.

`crates/kvim/tests/repository_policy.rs` enforces this policy. It checks that
every public feature module names an example file that exists, that no other
example replaces a feature example, and that every example link of the published
documentation resolves. [`architecture.md`](architecture.md) names the complete
set of release gates.
