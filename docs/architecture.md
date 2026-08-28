# Architecture

## Purpose

This document owns the workspace shape, the crate boundaries, the dependency
direction, state ownership, and the dependency ledger for kvim.

kvim is an embeddable editor platform and a standalone terminal modal editor.
The `kvim` executable consumes the same public library APIs as an external host.
macOS and Linux use one editor model. Platform branches stay in terminal,
process, filesystem, clipboard, and packaging boundaries.

## Workspace

The repository uses one Cargo workspace. Every charter below is one library
crate under `crates/`, plus the `kvim` binary crate that produces the
executable.

One crate for each charter makes the dependency direction a compile error
instead of a review rule. A reverse dependency no longer builds, so the layering
below cannot drift.

Build time is the second reason. One 41,000-line crate is the smallest unit that
`cargo` can recompile, so an edit anywhere recompiled all of it. After the split,
an edit recompiles its own crate and the crates above it only. The measured gain
on a warm incremental cache is small, because this codebase already compiles in
seconds: a one-item edit cost about 0.7 s to check and about 1.3 s to build
before the split. The enforced boundary, not the clock, is the reason to keep
the split.

Keep the crate set below stable. Add a crate only when a new charter appears.

## Crates

| Crate | Charter |
|---|---|
| `kvim-core` | Deterministic text model: rope buffer, validated coordinates, edit transactions, undo and redo. Performs no input or output. |
| `kvim-editor` | Modal editing state: cursors, selections, text objects, motions, operators, registers, search, dot-repeat, and the viewport of each window. |
| `kvim-keymap` | Terminal-neutral keys, generic bindings, the shared resolver with its one pending sequence, published input contexts, dispatch ownership, and which-key hints. |
| `kvim-path` | Canonical worktree roots, safe relative paths, and descriptor-relative capability access. |
| `kvim-fuzzy` | The deterministic fuzzy score of one candidate against one query, and the one rule that ranks a candidate list from those scores. Names no path, no buffer, and no editor concept. Depends on no other crate. |
| `kvim-syntax` | Grammar selection, parser ownership, bounded highlighting, and stable theme-independent syntax classes. |
| `kvim-lsp` | Project-scoped processes, protocol state, synchronization, diagnostics, deadlines, cancellation, and shutdown. |
| `kvim-ui` | Generic ratatui split with its adaptive orientation rule, the one scroll and motion rule of every bounded list, the tree sidebar with its indent guide rule, the domain-neutral selector over `kvim-fuzzy`, which-key presentation, and the host-workspace composer over `kvim-keymap`. |
| `kvim-input` | Kvim commands, modes, prompts, the semantic reducer for counts, operators, registers, and text objects, and the standalone binding preset. Builds on `kvim-keymap`. |
| `kvim-language` | Syntax and LSP adapters, indentation, formatting, hover markup, and editor publication gates. The standalone registry holds 25 adapters. [`language-services.md`](language-services.md) owns the table. |
| `kvim-clipboard` | The system clipboard boundary. Runs the platform clipboard command through the bounded process service. Holds no register value. |
| `kvim-runtime` | Bounded background work: process and worker services, the filesystem watch service, cancellation, deadlines, request identity, and publication gates. |
| `kvim-settings` | The `EditorSettings` structure and its defaults. Depends on no other crate. |
| `kvim-terminal` | Terminal lifecycle and conversion from crossterm events into terminal-neutral `kvim-keymap` values. |
| `kvim-tui` | Internal presentation implementation. It owns no terminal and no event loop. Its hidden adapter seam is not a supported host contract. |
| `kvim-workspace` | Files, buffers, tree state, Git capture, review data, workspace mutations, and pickers built on the domain-neutral selector of `kvim-ui`. It owns no host worktree list or focus policy. |
| `kvim-embed` | The only supported high-level editor facade. It publishes the existing rendered `MemoryEditor` and optional `WorktreeEditor`. Planned host-composition additions and a standalone `ReviewSurface` will extend this facade. It owns facade lifecycle, outcomes, and bounded execution capacity. |
| `kvim` | Raw mode, the alternate screen, standard input and output, terminal events, signals, panic restoration, cursor application, runtime startup, redraw scheduling, shutdown order, and the standalone application loop. |

Crates communicate through narrow contracts. Generic terminal, runtime, window,
and file code must not contain language-specific path rules. Only a language
adapter selects a path, by file extension or by file name. A lock file that
carries the format of another language, for example `flake.lock`, reaches its
adapter through the file-name key.

One narrow exception exists: the file tree selects an icon by file extension and
by well-known file name, and the which-key overlay selects an icon by command
group. An icon is presentation data, so one table serves both and lives in
`kvim-tui` beside the theme. An icon must never select a parser, an indent rule,
a comment token, or a language server, and no icon value may reach the language
adapters. [`files.md`](files.md) owns the icon table.

The color palette follows the same rule. A color is presentation data, so the
complete palette lives in `crates/kvim-tui/src/theme.rs` beside the semantic
roles, and `EditorSettings` holds no color. This keeps recoloring the editor to
one file and one rebuild. A test fails when a color reaches any other module of
the crate. [`windows.md`](windows.md) owns the palette.

`kvim-language` and `kvim-workspace` each publish one test seam behind a
`test-support` feature: the mock language server and the temporary-directory
helper. The editor tests of `kvim-tui` drive both, so one mock server and one
directory helper serve every layer. A normal build enables neither feature.

## Tests And The Ambient Environment

A test states every path and every value that it needs. It never passes or
fails by a property of the host that runs it. Three rules hold:

- The temporary-directory helper returns a canonical path. A host that reaches
  its temporary directory through a symbolic link, as macOS does with `/tmp`,
  must not make two spellings of one path look different. A test that needs two
  spellings of one file builds the second spelling itself, for example with a
  parent step, so the two paths differ on every host.
- A test asserts a message that holds a path against the message of the
  session, never against the painted message line. The message line paints one
  terminal row and drops every character behind it, so the length of the
  ambient temporary directory would decide the result.
- A test that needs the editor state directory receives the directory as a
  value. It never reads `HOME` or `XDG_STATE_HOME`, and it never returns early
  when the host reports no state directory.

The build sandbox of `nix flake check` holds the same test suite, but it holds
another environment. Do not use it to find the cause of a test that fails there
and passes on a developer machine, because each run rebuilds every crate.
Reproduce the environment locally instead, and run the compiled test binary
under `./target` with the changed value:

| Difference | Local reproduction |
|---|---|
| The temporary directory is canonical. | `TMPDIR=/private/tmp/<name>` |
| The temporary directory path is long. | `TMPDIR=/private/tmp/<40 characters or more>` |
| The home directory is absent or unwritable. | `HOME=/homeless-shelter` |
| The state directory is absent. | Run without `XDG_STATE_HOME`. |
| The working directory is another directory. | Run the binary from another directory. |

Every test must pass under each value. Run `nix flake check` to confirm the
result, not to find the cause.

## Dependency Direction

The dependency direction is one-way, and Cargo enforces it:

| Layer | Crate | Depends on kvim crates |
|---|---|---|
| 0 | `kvim-keymap` | none |
| 0 | `kvim-fuzzy` | none |
| 0 | `kvim-path` | none |
| 0 | `kvim-settings` | none |
| 0 | `kvim-syntax` | none |
| 1 | `kvim-core` | None |
| 1 | `kvim-runtime` | `kvim-path` |
| 1 | `kvim-terminal` | `kvim-keymap` |
| 2 | `kvim-clipboard` | `kvim-runtime` |
| 2 | `kvim-input` | `kvim-keymap`, `kvim-settings` |
| 2 | `kvim-lsp` | `kvim-path` |
| 2 | `kvim-ui` | `kvim-keymap`, `kvim-fuzzy` |
| 3 | `kvim-editor` | `kvim-core`, `kvim-input`, `kvim-settings` |
| 3 | `kvim-language` | `kvim-core`, `kvim-lsp`, `kvim-runtime`, `kvim-settings`, `kvim-syntax` |
| 3 | `kvim-workspace` | `kvim-core`, `kvim-path`, `kvim-runtime`, `kvim-settings`, `kvim-ui` (the `review` partition uses only `kvim-path`) |
| 4 | `kvim-tui` | `kvim-clipboard`, `kvim-core`, `kvim-editor`, `kvim-fuzzy`, `kvim-input`, `kvim-language`, `kvim-path`, `kvim-runtime`, `kvim-settings`, `kvim-terminal`, `kvim-ui`, `kvim-workspace` (the `review` partition uses only neutral input, path, settings, UI, and workspace review partitions) |
| 5 | `kvim-embed` | `kvim-core`, `kvim-editor`, `kvim-input`, `kvim-keymap`, `kvim-language`, `kvim-lsp`, `kvim-path`, `kvim-runtime`, `kvim-settings`, `kvim-tui`, `kvim-ui`, `kvim-workspace` |
| 6 | `kvim` | `kvim-embed`, `kvim-path`, `kvim-settings`, `kvim-terminal` |

External dependencies do not change the layer number. The default
`kvim-embed` path uses ratatui directly for its small plain-text memory view.
The workspace disables ratatui default features because the default enables a
crossterm backend. Rendering into a caller-owned `Buffer` needs no backend.
`kvim-ui` and `kvim-tui` compile against this backend-neutral API. The
standalone binary enables ratatui's `crossterm` feature at its composition
root. This keeps terminal lifecycle and crossterm ownership outside the memory
facade. The facade reuses `kvim-editor` viewport and modal state. Its
`worktree` feature privately adapts `kvim-tui` and owns its Tokio executor.
Grammar features imply `worktree` and forward to the matching `kvim-tui`
language feature. The default dependency closure remains unchanged. `kvim-ui`
ratatui geometry and rendering. No syntax-only consumer compiles LSP, ratatui,
or the editor.

`kvim-tui` keeps its dependency on `kvim-terminal`. The edge carries the
`TerminalEvent` value alone, because `Session`, the standalone presentation
adapter, applies one normalized event as one pure transition. `kvim-tui` owns no
terminal lifecycle code: no raw mode, no alternate screen, no event stream, no
signal handler, no panic hook, and no write to standard output. A structural
test in `kvim-tui` proves that no module of that crate names such an owner, and
a structural test in `kvim` proves that its terminal loop holds every one of
them.

The alternative was to move `TerminalEvent` down into `kvim-keymap` and leave
the crossterm conversion in `kvim-terminal`. That move is refused, because the
value carries `Resize`, which is a terminal fact and not a key fact. A keymap
crate that named it would own two charters. The accepted cost is that an
external host of the embedded facade also compiles `kvim-terminal` and
crossterm, although `WorktreeEditor` names no terminal type.

`kvim-ui` depends on `kvim-keymap` because `WorkspaceComposer` holds one shared
`Resolver` and reads one published `InputContextSnapshot` for each surface. The
split tree, the sidebar, and the which-key widget still name no keymap type, so
a consumer of those parts alone compiles no dispatch code that it does not use.

`kvim-ui` depends on `kvim-fuzzy` because `Selector<R>` ranks its candidates
through `kvim_fuzzy::rank`. `kvim-fuzzy` is layer 0 and depends on no other
kvim crate, so the edge adds no cycle and no other module of `kvim-ui`
compiles ranking code that it does not use.

`kvim-workspace` depends on `kvim-ui` because `Picker` holds one
`Selector<usize>` for its query, its ranking, its match list, and its
selection. `kvim-ui` is layer 2 and `kvim-workspace` is layer 3, so the edge
adds no cycle. The workspace crate does not re-export or wrap `kvim-fuzzy`.
Consumers that score or rank their own candidates use the supported
`kvim-fuzzy` package directly. Command-line completion in `kvim-tui` also calls
`kvim_fuzzy::rank` directly over borrowed workspace candidates.

`kvim-runtime` depends on `kvim-path` only for the portable filesystem watcher.
The watcher uses one caller-supplied worktree capability for registration reads
and event validation. The ambient path required by `notify` stays inside that
portable boundary.

`kvim-clipboard` has one consumer. `kvim-tui` mirrors the unnamed register into
the system clipboard, and it reads the selected commands for the host report.
Both reach the platform through the same selection, so no module above the
crate names a clipboard command. A host grants one `ClipboardAccess` policy
instead, and the binary grants `ClipboardAccess::System`. See
[`clipboard.md`](clipboard.md).

The binary is the standalone composition root. An external host is another
composition root and constructs only the public capabilities that it uses.

Do not add a reverse dependency. Move a shared type to the lowest charter that
owns its meaning. A reverse dependency is a Cargo cycle, so it fails the build.

## External Consumption

The supported external packages are `kvim-path`, `kvim-fuzzy`, `kvim-core`,
`kvim-settings`, `kvim-keymap`, `kvim-input`, `kvim-editor`, `kvim-syntax`,
`kvim-lsp`, `kvim-ui`, and `kvim-embed`. `kvim-embed` is the only supported
high-level editor facade. `kvim-tui` is an internal presentation
implementation. Its hidden adapter module is not a compatibility contract and
exists only for `kvim-embed`.

`kvim-embed` defaults to an empty feature set and compiles only the in-memory
editor. Its default dependency closure contains no `kvim-tui`, `kvim-runtime`,
`kvim-language`, `kvim-lsp`, `kvim-workspace`, `kvim-path`, `kvim-terminal`,
Tokio, crossterm, notify, or cap-std. The in-memory editor reuses the lower
modal state and viewport. Its small plain-text painter remains local because
the full worktree renderer is structurally tied to worktree and language state.
This avoids a reverse dependency and avoids duplicating that full renderer.

`kvim-input` publishes `Command`, the semantic reducer, and the binding preset,
so its action list is public. A consumer that resolves keys itself reads the
resolved command, count, and register name from it. `kvim-editor` publishes the
modal editing state over one `TextBuffer`, so a consumer can put a real Vim
buffer behind its own text field without the terminal session.

`kvim-input` also publishes the edit vocabulary of one prompt line.
`PromptEdit` names every edit that an open prompt takes, and
`PromptEdit::of_command` turns a resolved command into that edit, so a host
that draws its own prompt line reaches the same vocabulary that kvim uses. The
vocabulary holds the two text edits, the two completion edits, `Accept`,
`Cancel`, and six cursor motions: one character each way, one word each way,
and each end of the line. The preset binds the motions in the prompt scope
alone, over the arrow keys, `Home`, `End`, and the readline chords `Ctrl-B`,
`Ctrl-F`, `Ctrl-A`, and `Ctrl-E`. A host that binds one of those chords in its
own global scope wins that key, because the resolver walks a global scope
before a focused one. Such a host keeps the plain key of the same motion.
`kvim-input` publishes the line that those edits edit as well. `EditedLine`
holds one text and one cursor counted in characters, and it owns every edit
that applies at that cursor, so a host holds the verbs and the noun.
`EditedLine::apply` reports one `LineChange`, and the caller-owned edits report
`Deferred`, because a candidate list and a prompt belong to the host. The line
names no prompt kind and no completion: the prompt of kvim is that line plus
its kind and its candidate list, exactly as `kvim_workspace::Picker` is one
`kvim_ui::Selector` plus the file vocabulary.
[`input-actions.md`](input-actions.md) owns the prompt line and its keys.

`kvim-fuzzy` holds the ranking rule alone: `score_candidate` scores one
candidate against one query, and `rank` turns those scores into one ordered
list of source indexes. A host that ranks a list of its own values takes
either without the file, buffer, and picker charter of `kvim-workspace`,
which is not a supported package. `kvim-ui` publishes `Selector<R>` over that
same rule, so a host that also wants the bounded query, candidate, match, and
selection mechanics takes `kvim-ui` instead of calling `kvim-fuzzy` directly.

`kvim-ui` also publishes the adaptive split rule. `WindowTree::adaptive_orientation`
takes the sense of the command, `AdaptiveSplit`, and a caller-supplied ratio, and
answers the orientation that the command selects. A host that binds an adaptive
split key reaches the same rule that kvim uses, without depending on
`kvim-settings`. [`windows.md`](windows.md) owns the rule.

`kvim-ui` also publishes the tree mechanics of one sidebar. `SidebarRow<R>`
carries a depth, a collapsed flag, and a section index, through `with_depth`,
`with_collapsed`, and `with_section`. `SidebarState<R>` hides a collapsed
subtree and a collapsed section from every motion, from the placements, and
from the line count, and `set_sections` holds the collapsed flag of each
section. `sidebar_guides` draws the indent guides of one row, over the
`SIDEBAR_GUIDE_TRUNK`, `SIDEBAR_GUIDE_ELBOW`, and `SIDEBAR_GUIDE_BLANK`
constants. `SIDEBAR_ROW_DEPTH_MAX` and `SIDEBAR_SECTIONS_MAX` bound the depth
and the section count. A host supplies the row identities and the meaning. It
writes no indent rule, no collapse rule, and no motion rule of its own.
[`windows.md`](windows.md) owns these rules.

`kvim-ui` also publishes the one scroll rule of every bounded list.
`ListWindow::reconciled` is that rule, as a pure function that stores nothing.
`ListViewport` is the stateful shell over it: it owns a height, a scroll margin,
and the last answer, and it keeps a selected item inside the window without
scrolling past the end of the list. `ListItem` is the measure of one item, and
`ListItem::single` builds a list of one line for each item, so a uniform list is
the simple case of the same rule. `ListPlacement` names the visible part of one
item and carries no host identity. `LIST_VIEWPORT_LINES_MAX` bounds the total
line count. `SidebarState<R>` and `Selector<R>` each hold one viewport, so a
host paints a bounded sidebar or a bounded picker without writing an offset rule
of its own. Both also publish `window_for_height`, which answers one window
through a shared reference for a height that the caller supplies at draw time,
so a host whose frame builder holds its state by shared reference reads the
window without a mutable borrow. [`windows.md`](windows.md) owns the rule.

`ListMotion` is the one motion type that both lists answer. It replaces
`SidebarMotion`, which is gone from the public surface with no alias, and a
host renames its import. `ListMotion::ToRow` names a row of the row space of
the list that receives it, and the two lists keep different row spaces, so a
host reads that section before it reuses one value across both.
`Selector::apply_motion` answers every variant, so a host picker reaches the
last row and jumps to a row exactly as a host sidebar does.

`ListMotion::Parent` climbs to the nearest earlier row of a smaller depth that a
reader can select, so a host tree binds one key and writes no depth scan.
`parent_row` publishes that climb as a pure function over one iterator of
`ParentScanRow`, which names the depth, the section, and the selectable state of
one row. One iterator refuses a short or misordered argument that three parallel
slices would accept, and neither caller allocates. `SidebarState<R>` answers the
motion, `kvim_workspace::FileTree::select_parent` calls the same rule, and
`Selector<R>` keeps its row, because a match list carries no depth.
[`windows.md`](windows.md) owns the rule.

`Selector<R>` publishes that window beside its ranking. `set_height_rows`,
`set_scroll_margin`, `first_line`, `total_lines`, and `placements` read or
write the one viewport, and one `SelectorPlacement` names the row position and
the matched candidate. `candidates_len` returns the number of held candidates,
so a host separates a list with no candidate from a query that keeps nothing.
The two cases show different text.

`WhichKeyOverlay` pages a list that outgrows its frame. `at_page` consumes and
returns the overlay, so a page and the rows it pages stay one value, and
`render` answers one `WhichKeyPlacement` that names the drawn rows, the size of
the complete list, the drawn page, and the number of pages. One page is one
full frame of columns, so the pages reach every row exactly once.
`WhichKeyOverlayRow` is the drawn row. It replaces `kvim_ui::WhichKeyHint`,
which is gone with no alias, because `kvim_keymap::WhichKeyHint` keeps that
name for what a key reaches. The row carries two independent facts:
`WhichKeyOverlayRow::icon` marks the table that holds the key, and
`WhichKeyOverlayRow::key_style` marks whether the key continues the pending
sequence or abandons it. Both fields are public, so a struct literal outside
this repository names the new one.
`WhichKeyOverlay::placement_for` answers that same `WhichKeyPlacement` from the
hints and the body rectangle, without a buffer and without a mutable borrow, so
a host that writes "page 1 of 2" into a title reads the count before it draws.
Both entry points call one private geometry, so one capacity rule remains.
[`input-actions.md`](input-actions.md) owns these rules.

`kvim-ui` also publishes the shedding rule of every one-row band. `ChromeBand`
holds a list of `BandSegment`, and each segment carries the text that the caller
already rendered, one `BandSide` for the edge that it sits against, and one
`BandRank`. `ChromeBand::placements` answers one `BandPlacement` for each kept
segment. A band that cannot hold every segment sheds the lowest rank first, the
highest rank survives every shed, and two segments of one rank shed the later
one first. `BAND_SEGMENTS_MAX` bounds the list, and `ChromeBand::new` answers
`BandError` for a longer one instead of cutting it. The band names no subject,
no color, and no glyph, so a host fills it with its own parts and keeps kvim's
precedence. The statusline and the winbar of kvim draw through the same band,
so no second shedding rule exists. [`windows.md`](windows.md) owns the rule.

`kvim-keymap` publishes three which-key lists and one registry helper.
`kvim-embed::WorktreeBindingModel` composes bounded host and kvim contributions
into that generic registry. It projects editor host-leader contributions only
into Normal, Visual, and sidebar scopes. Literal and internal pending contexts
retain kvim ownership. Explicit bounded overrides select one addressed host or
editor winner for duplicate and strict-prefix conflicts. The model rejects
stale, ambiguous, contradictory, or uncovered overrides without using
registration order. It validates the facade context by requiring its reserved
escape key to complete one host-global command. It then projects the picker
overlay intact. It publishes bounded owner and semantic group labels while
leaving resolution and hint generation in `kvim-keymap`.
`Resolver::which_key` returns one `WhichKeyView`, and `WhichKeyView::hints`
reports the hints of every scope that extends the pending prefix. Each hint
names its own scope. `WhichKeyView::interruptions` reports the complete one-key
bindings of every scope that precedes the scope owning that prefix, so an
overlay names the keys that abandon the sequence beside the keys that continue
it. `Resolver::idle_which_key` lists the top-level bindings of every scope of
one context, with no pending prefix, so a host-global escape stays
discoverable.
`Registry::all_bindings` yields every `(scope, KeySequence, BoundCommand)`
triple of one registry, so a host takes kvim's preset without walking the
scopes itself. [`input-actions.md`](input-actions.md) owns the resolution
order, and [`embedding.md`](embedding.md) owns the host recipe.

`kvim-keymap` also publishes `Dispatch::Interrupted`, and `kvim-ui` carries it
to a host as `Composition::Interrupted`. A complete binding of a preceding
scope cancels a pending key sequence and runs, so a host-global escape leaves a
focused surface at any moment. The outcome names the owner and the command. The
resolver drops its key prefix alone, so the host resets the named surface
before it runs the command. Every consumer matches the enum, so a host that
ignores the outcome does not compile.
[`input-actions.md`](input-actions.md) owns the rule, and
[`embedding.md`](embedding.md) owns the host contract.

`kvim-keymap` also publishes `UnboundInput` on `InputContextSnapshot`, beside
`TextFallback`. A scope that waits for one answer it does not bind declares
`UnboundInput::Cancels`, so input that no binding, no extension, no
interruption, and no text fallback took ends that scope. The default is
`UnboundInput::Ignored`, so a present host keeps its behavior. The resolver
answers a cancelling scope with `Dispatch::Cancelled`, and `kvim-ui` carries it
to a host as `Composition::Cancelled`, which names the surface and no command.
kvim's own `BindingScope::RegisterSelection` declares the rule instead of
holding a special case outside the registry.
[`input-actions.md`](input-actions.md) owns the rule, and
[`embedding.md`](embedding.md) owns the host contract.

The internal presentation implementation renders one built-in file sidebar.
The supported `WorktreeEditor` facade exposes the complete rendered surface,
but it does not expose file rows, a root label, direct sidebar input, or a
custom row painter. Custom file-sidebar integration is deferred. Generic
sidebar state and rendering remain public component APIs of `kvim-ui`.

The built-in sidebar keeps its selection mark only while the sidebar has focus.
When a long label reaches the fixed right-edge Git mark, its final three visible
text cells fade toward the row background. This behavior remains internal to the
complete rendered surface. [`embedding.md`](embedding.md) owns the behavior,
and [`windows.md`](windows.md) owns the mark rule.

The lower-level candidate-menu model and painter remain component APIs of
`kvim-tui`. They are not payloads of the supported high-level facade.

The embedded facade also publishes the candidate menu of one prompt line.
`LineCompletion` is the model, `CompletionCycle` names the direction of the key
that opened it, `CompletionOutcome` reports whether one candidate answered the
line alone, and `draw_completion_menu` paints the model. `COMPLETION_ROWS_MAX`,
`COMPLETION_COLUMNS_MAX`, and `COMPLETION_CANDIDATES_MAX` bound the rows, the
width, and the candidate list, and the last one refuses a longer list rather
than cutting it. The model holds the line without its prompt prefix, so no row
repeats a prefix that the prompt itself paints. kvim's own command line draws
through the same call, so no second appearance exists. The menu is no
`Selector`: a selector ranks against a query and stops at both ends, while a
menu wraps at both ends and restores the typed text on a cancel, so the facade
publishes two types. [`windows.md`](windows.md) owns the placement rule.

`WorktreeEditor::mode` answers the editing mode of one editor. A host that names
the mode in a band of its own reads this value. `WorktreeEditor::input_context`
answers the scope that owns the keys instead, so it names a prompt, a sidebar,
or a picker while one of those reads them. The two answer different questions,
and a host that read the scope alone would lose its mode label whenever a prompt
opened. [`embedding.md`](embedding.md) owns the host contract.

`kvim-core` and `kvim-settings` are the vocabulary of those signatures.
`TextBuffer`, `EditTransaction`, the coordinate types, `FileSettings`, and
`EditorSettings` all appear in a public parameter or return value, so a consumer
cannot use the editor or the embedded facade without naming both packages.

Each public crate supports a revision-pinned Cargo Git dependency from another
repository. It requires no shared parent workspace. Every normal dependency of
a public crate is available at the same Git revision or from crates.io. A public
crate must not depend on an unpublished path outside this repository or on a
test-support feature.

The public and workspace minimum supported Rust version (MSRV) is Rust 1.94.1.
`[workspace.package].rust-version` records this minimum. The development and
release toolchain remains Rust 1.97.1 in `rust-toolchain.toml`. Continuous
integration compiles and tests the library workspace separately with Rust
1.94.1. The `msrv` development shell of `flake.nix` supplies that toolchain,
so a developer can run the same check locally.

Public ratatui signatures use workspace ratatui 0.29 types. External consumer
checks use the same compatible release. Public feature crates remain at version
`0.1`.

kvim is before its first release. A consumer pins one revision of the Git
repository, so a version number signals nothing to it yet. A breaking facade
change therefore needs updated rustdoc and an updated dedicated example, and it
needs no version increase and no migration note. Both obligations begin at the
first release.

From that release, a breaking facade change requires a workspace minor-version
increase, a migration note, updated rustdoc, and an updated dedicated example. A
patch release must not intentionally break a documented public facade.

A published enum states which of two kinds it holds, and the attribute follows
from the kind. An enum whose variant demands a decision from a host stays
exhaustive, so a new variant stops the build until the host answers it. An enum
that holds a growing vocabulary carries `#[non_exhaustive]`, because a host
binds the members it wants and ignoring a new member is the correct answer.
`WorktreeEvent` and `Dispatch` are of the first kind. `kvim_input::Command` is of
the second: it names every editor command, a host binds the ones it publishes,
and a new command breaks nothing. `kvim_syntax::SyntaxRole` and
`kvim_syntax::LimitKind` are of the second kind for the same reason, so the
three carry `#[non_exhaustive]` and are no exception to reconcile.
`kvim_input::PromptEdit` is of the first kind, although it lives beside
`kvim_input::Command`. A host that draws its own prompt line must answer every
edit, and a host that absorbed a cursor motion into a wildcard arm would drop
that key with no compile error. The six cursor motions of the prompt line
arrived as such a break: a host adds one match arm for each and moves the
cursor of its own line by characters. kvim is before its first release, so that
break takes the obligations of this section for one: updated rustdoc and an
updated dedicated example, and no version increase and no migration note.
Neither `kvim-tui` nor `kvim-ui` adds a `#[non_exhaustive]` attribute to a
published enum, because every enum of the facade is of the first kind. A new
variant is therefore a breaking change, by design. The compile error is how a
host learns that a new behavior exists. The compile error forces the host to
decide how it handles that behavior. `Dispatch::Interrupted` proves the rule.
Plan 029 published it as a new variant, not a flag. Every consumer must handle
the new variant or fail to build. A host that ignored the interruption would
run a command with a stale count, or leave an operator armed. A new variant
costs a host one added match arm. That cost falls due at the version bump that
a breaking facade change already requires. The rejected alternative is
`#[non_exhaustive]`. That attribute would let a host absorb a new variant into
a wildcard arm. A wildcard arm ships the wrong behavior with no compile error
to catch it.

A published enum that names an input also derives `Ord` and `PartialOrd`. A
host can use that value inside its own command vocabulary without building a
parallel type. This rule applies to supported component APIs such as
`PromptEdit`, `ConfirmEdit`, `ListMotion`, `CompletionCycle`, and
`AdaptiveSplit`. An enum that names an outcome or a fact needs no order.

The rule covers the payload of a variant as well as the variant list. A changed
payload type is a breaking facade change because every host must decide how to
handle the new value. Supported facade payloads must come from supported crates;
private presentation and workspace types must not leak through `kvim-embed`.

Every supported surface in this section carries rustdoc, one owning document,
and the dedicated example of its feature. The supported set includes generic
`kvim-ui` composition and sidebar components, prompt editing, candidate menus,
the editing mode of `WorktreeEditor`, and the rendered worktree facade. It does
not include private worktree file-row payloads or a multi-surface composer
adapter for `WorktreeEditor`.
`crates/kvim/tests/repository_policy.rs` proves that last link, so the same
rule governs all of them.

Continuous integration checks minimal features, one representative grammar,
default features, and all valid grammar combinations. Independent external
consumers prove each supported package in isolation. The exact matrix is:

| Crate | Default | Required CI combinations |
|---|---|---|
| `kvim-path` | no optional production features | default |
| `kvim-keymap` | no optional production features | default |
| `kvim-lsp` | no optional production features | default |
| `kvim-ui` | no optional production features | default |
| `kvim-syntax` | no grammar | no grammar, `grammar-rust`, `all-grammars` |
| `kvim-embed` | in-memory only | default, no-default, `worktree`, `grammar-rust`, `all-grammars`; planned host-composition and review feature combinations remain documentation targets until implementation |
| `kvim-tui` | internal only | no grammar, `grammar-rust`, `all-grammars` |

`kvim-language` forwards the same grammar features without a default grammar.
Its no-grammar registry is valid and empty. Path lookup is typed unsupported,
language-name lookup returns none, fenced markup stays plain, and service
construction starts no language process. No lower or facade layer may assume
that Rust or another adapter exists. The `kvim` binary enables `all-grammars`.
Private `test-support` features are not external combinations. Record an
architectural reason before excluding any future combination.

## Enforced Policy

Continuous integration turns each rule above into a release gate. It runs every
gate on macOS and on Linux.

| Gate | Command | It proves |
|---|---|---|
| Feature examples | `scripts/run-required-examples.sh` | The script reads the authoritative list in [`embedding.md`](embedding.md), then runs every dedicated example. |
| Example policy | `cargo test -p kvim --test repository_policy` | Every public feature module names an example file that exists, no extra example replaces a feature example, and every documented example link resolves. |
| Rustdoc links | `cargo doc --workspace --no-deps --all-features` under `RUSTDOCFLAGS=-D warnings` | Every intra-doc link of the published documentation resolves. |
| Dependency edges | `scripts/check-dependency-edges.sh` | Every direct and transitive kvim edge appears in the layer table above, each isolation charter reaches none of the external crates that it refuses, and every dependency of a supported package is reachable from the same revision or from crates.io. |
| Syntax isolation | `cargo check -p kvim-syntax --no-default-features [--features …]` | The syntax package builds with no grammar, with one grammar, and with every grammar. |
| External consumers | `scripts/check-external-consumer.sh` | Independent workspaces compile each supported package through revision-pinned Git dependencies. They cover memory and worktree lifecycles and the feature matrix with development and minimum supported Rust version toolchains. |

The external-consumer script uses the checked-out repository's `origin` and the
selected Git revision by default. It does not print the repository URL. Pass
`--repository-url` to select a different repository explicitly. Continuous
integration uses `--checked-out-repository`, a `file://` clone of the full-depth
checkout. This preserves outside-workspace, revision-pinned Git behavior and
makes pull-request merge commits available on macOS and Linux without remote
authentication. Run it with `--local-source` to include uncommitted worktree
files without remote authentication. Local mode copies the worktree into a
temporary Git repository because Cargo Git dependencies cannot read uncommitted
files.

Each directory under `fixtures/consumer/` is an independent workspace. One
fixture imports one supported package. The two `kvim-embed` fixtures may also
import documented supported companion packages and ratatui. No facade fixture
imports `kvim-tui`, `kvim-runtime`, `kvim-language`, or `kvim-workspace`.

The dependency gate reads the layer table of this document, so the policy and
architecture cannot disagree. A new charter row changes both at once.

## Planned Host And Review Surface

The supplied-review boundary uses explicit pure-review feature partitions in
`kvim-workspace` and `kvim-tui`. The workspace partition owns diff values,
alignment, anchors, relocation, and review state. The presentation partition
owns the single review model and painter. The normal editor build enables both
partitions and its integrated review adapts the same model and painter.
`kvim-embed --no-default-features --features review` disables both crates'
service partitions. Its normal dependency closure therefore contains no
`kvim-runtime`, `kvim-language`, `kvim-lsp`, `kvim-terminal`, Tokio, notify, or
Tree-sitter package. These partitions are private implementation boundaries;
`kvim-embed` remains the only supported high-level facade.

The `review` feature adds `ReviewSurface::from_candidates`. This constructor
accepts bounded immutable facade values and performs no input or output. It
reuses `blake3`, already present in the workspace, to derive deterministic
private candidate authority from the bounded host identity. It reuses the
private review model, anchor relocation, and painter used by integrated review.
It does not construct an editor or call filesystem, Git, process, watcher,
clipboard, language, or runtime APIs.

The supplied-review dependency boundary is enforced with `cargo tree` over the
exact review-only feature selection. Constructor-level source inspection is not
dependency isolation.

`ReviewSurface::for_worktree` performs bounded Git capture behind the
`worktree` feature. It privately owns its executor, request publication gates,
and paired staged and unstaged results. It publishes neither half until both
resolve. Hosts own focus, comment persistence, and host-domain meaning for both
review modes.

## State Ownership

One host event-loop owner owns the visible state of each editor instance. The
standalone terminal loop is one such host. Background work returns typed results
through bounded channels and cannot mutate visible state.

Build a complete candidate before publication. Validate the candidate. Publish
the candidate with one state transition. A failed, cancelled, or obsolete
candidate leaves the previous visible state usable.

[`responsiveness.md`](responsiveness.md) owns the bounds, request identity,
publication gates, latency budgets, and shutdown order for this rule.

## Dependency Ledger

Each entry records what local code the dependency replaces, where the
dependency may run, and its cost. Record every new dependency here, or in the
more specific owning document, before implementation uses it.

### Every Crate

- `thiserror`
  - Replaces: hand-written error types, `Display` implementations, and manual
    source chains.
  - May run: in every crate that reports a typed failure, including
    `kvim-core`. Only `kvim-settings` needs no error type.
  - Cost: one derive macro at compile time. No runtime cost.

### The Imperative Boundary

These dependencies must not run inside `kvim-core`. They stay at the
imperative boundary.

- `crossterm`
  - Replaces: local raw mode, alternate screen, resize handling, enhanced
    keyboard reporting, and key decoding.
  - May run: in `kvim-terminal` only.
  - Cost: compile time and platform-specific transitive code.
- `ratatui`
  - Replaces: a local widget set, cell buffer, and layout implementation.
  - May run: in `kvim-ui`, `kvim-tui`, `kvim-embed`, and the standalone
    composition root. `kvim-embed` renders into a caller-owned cell buffer. The
    standalone root owns the terminal backend and draw call.
  - Cost: compile time. Rendering cost stays bounded by the terminal buffer and
    the visible window content.
- `unicode-width`
  - Replaces: local terminal-cell width tables.
  - May run: in `kvim-ui`, `kvim-tui`, and `kvim-embed`, which lay out cells.
    `kvim-core` defines the terminal-column coordinate type, but it does not
    measure cell width, and `kvim-terminal` normalizes events rather than laying
    out cells.
  - Cost: small. Work stays bounded to visible or otherwise bounded text.
- `futures-util`
  - Replaces: a local polling loop over terminal events, and a local join over
    the shutdown futures of several language servers.
  - May run: in `kvim-terminal`, which owns the event stream, and in `kvim-lsp`,
    which awaits every server of one project together.
  - Cost: one small stream extension API and one bounded join.
- `tokio`
  - Replaces: local thread pools, channels, deadlines, and child-process
    handling.
  - May run: in `kvim-runtime`, `kvim-language`, `kvim-lsp`, `kvim-terminal`,
    `kvim-tui`, `kvim-embed`, and the standalone composition root. Lower public
    drivers return futures and create no runtime. `WorktreeEditor` privately
    owns one runtime and bounded spawners.
  - Cost: compile time, supply-chain size, and a worker thread pool.
- `tokio-util`
  - Replaces: local cancellation flags and shared shutdown state.
  - May run: in `kvim-runtime`, and in crates that own cancellable requests:
    `kvim-language`, `kvim-lsp`, `kvim-tui`, and `kvim-workspace`. A token
    crosses the boundary with its request. `kvim-syntax` names no token. It
    reads one `CancellationSignal` trait instead, so a consumer without an
    asynchronous runtime still cancels a highlight walk.
  - Cost: small. It adds owned cancellation tokens.
- `notify`
  - Replaces: local inotify and FSEvents code for external change hints.
  - May run: behind the portable watch service of `kvim-runtime` only. An
    editor driver reads typed bursts from that service. No host event loop
    touches the platform API.
  - Cost: platform-specific transitive code and one callback thread. Watch
    roots and callback delivery stay bounded.

### Filesystem Confinement

- `cap-std`
  - Replaces: ambient path access and local symbolic-link confinement logic.
  - May run: in `kvim-path` and behind `kvim-workspace` file boundaries. Public
    operations use a capability directory rooted at one canonical worktree.
  - Cost: compile time and descriptor-relative system calls. The security
    benefit is that file access cannot escape its supplied worktree root through
    path traversal or symbolic-link replacement.

### Review Identity

- `blake3`
  - Replaces: a local content-digest implementation for immutable diff
    revisions, side bytes, selected review lines, and capture fingerprints.
  - May run: in bounded Git capture and pure review-anchor construction inside
    `kvim-workspace`.
  - Cost: compile time and one bounded hashing pass over captured bytes. A
    mature cryptographic digest prevents ambiguous review identities and avoids
    designing a security-sensitive hash locally.

### The Text Model

This dependency runs inside `kvim-core`, because the text storage is the text
model. `kvim-core` also uses `thiserror`. Buffer construction uses the core-owned
`BufferBytesMax`. Composition resolves user and language indent settings into a
core-owned `IndentPolicy`, so `kvim-core` does not depend on `kvim-settings`.

- `ropey` 1.6
  - Replaces: a local rope or piece table, a local line index, and local
    conversions between byte offsets, character positions, and line indexes.
  - May run: in `kvim-core` only. `kvim-core` keeps the rope private and exposes
    validated coordinates, edit transactions, and owned line text. No other
    crate sees a rope type.
  - Cost: compile time and one chunked tree over the buffer text. Memory stays
    bounded by the maximum file size and by the undo history bound in
    [`text-model.md`](text-model.md).
  - Version reason: the 1.6 line is the stable line, and the Helix editor ships
    on it. It converts between bytes, characters, and lines natively, which the
    five coordinate types need. The 2.0 line is still a beta, so it is not a safe
    base for undo and incremental parsing. The `crop` crate is newer and
    maintained, but it drops character indexing on purpose.
  - Future consideration: move to `ropey` 2.0 after that line reaches a stable
    release. Confirm the character-index API before the move.

### Grapheme Clusters

This dependency runs inside `kvim-editor`, because the cursor rule of
[`text-model.md`](text-model.md) needs a segmentation table that `kvim-core`
does not hold.

- `unicode-segmentation` 1.12
  - Replaces: a local Unicode grapheme cluster break table and the boundary
    walk over it.
  - May run: in `kvim-editor` only, over one line at a time. `kvim-core` reports
    whether a line holds ASCII characters alone, and an ASCII line needs no
    segmentation, so the walk runs on other text alone.
  - Cost: compile time and the break table. One walk reads one line, which the
    file settings bound, so a step and a delete stay bounded.
  - Version reason: the 1.x line implements the current Unicode text
    segmentation annex and carries no further dependency.

### The Worker Service

These dependencies run only on the bounded worker service.

- `tree-sitter`
  - Replaces: a local Rust parser and incremental reparse logic.
  - May run: in `kvim-syntax`, when a direct consumer or editor driver submits
    synchronous highlight work through its bounded worker spawner, and in the
    analysis module of `kvim-language`, which owns the parse that the indent
    query and the next incremental reparse also read.
  - Cost: compile time, native code, and bounded parse memory for each buffer.
- `tree-sitter-highlight`
  - Replaces: local highlight-query execution and capture mapping.
  - May run: in `kvim-syntax` through the same bounded worker path.
  - Cost: small addition over `tree-sitter`.
- `tree-sitter-rust`
  - Replaces: a local Rust grammar and local highlight queries.
  - May run: in `kvim-syntax` when its Cargo grammar feature is enabled.
  - Cost: generated C code and compile time.
- The other 23 grammar crates: `tree-sitter-asm`, `tree-sitter-bash`,
  `tree-sitter-c`, `tree-sitter-cpp`, `tree-sitter-css`, `tree-sitter-fish`,
  `tree-sitter-glsl`, `tree-sitter-go`, `tree-sitter-hcl`, `tree-sitter-html`,
  `tree-sitter-javascript`, `tree-sitter-json`, `tree-sitter-lua`,
  `tree-sitter-md`, `tree-sitter-nix`, `tree-sitter-python`, `tree-sitter-scss`,
  `tree-sitter-sequel`, `tree-sitter-toml-ng`, `tree-sitter-typescript`,
  `tree-sitter-xml`, `tree-sitter-yaml`, and `tree-sitter-zig`
  - Replaces: a local grammar and local highlight queries for each registered
    language. [`language-services.md`](language-services.md) owns the language
    table.
  - May run: in `kvim-syntax` when its Cargo grammar feature is enabled. Each
    crate is catalog data. No crate name reaches code above that boundary.
  - Cost: generated C code and compile time for each grammar. Standalone kvim
    enables every grammar, so a user installs no parser file. One host and
    one toolchain measured the whole set: the release executable grew from
    5,402,048 bytes to 19,872,288 bytes, and the cold release build grew from
    22.2 s to 29.5 s. The user accepted that cost, so the complete language
    table works in the standalone build. Public consumers enable only the
    grammar features that they use.
  - Version reason: every one of these crates carries its parser through
    `tree-sitter-language`, not through the `tree-sitter` runtime, so all of
    them link against the single pinned `tree-sitter` version. `tree-sitter-md`
    keeps its `tree-sitter` dependency behind an optional feature that kvim
    leaves off, which is what keeps a second runtime version out of the build.
    `tree-sitter-toml-ng` replaces the unmaintained `tree-sitter-toml` crate,
    which still requires the 0.20 runtime line.
  - Irregular shapes: `tree-sitter-fish` and `tree-sitter-scss` supply the older
    `language()` accessor instead of a `LANGUAGE` constant.
    `tree-sitter-typescript` and `tree-sitter-xml` each hold more than one
    grammar. `tree-sitter-hcl` ships no highlight query, so
    [`language-services.md`](language-services.md) records the vendored query
    and its license.

### The Language-Server Task

These dependencies run only in the bounded language-server task.

- `serde`
  - Replaces: hand-written JSON-RPC envelope parsing.
  - May run: in bounded language-server tasks inside `kvim-lsp`, and in
    `kvim-language`, which builds the initialization data and reads the answers
    that its adapters declare.
  - Cost: derive macros and compile time.
- `serde_json`
  - Replaces: a local JSON parser and serializer.
  - May run: in bounded language-server tasks inside `kvim-lsp`, and in
    `kvim-language`, for the same declarations and answers.
  - Cost: compile time. Allocation stays inside the bounded task.

### The Markup Of One Answer

- `pulldown-cmark` 0.13, with no default feature
  - Replaces: a local CommonMark reader. A local reader would have to answer
    the edge cases of the grammar itself: an emphasis run, a link reference, a
    lazy continuation line, a list item that continues, and a fence that never
    closes.
  - May run: in the markup module of `kvim-language` only. No type of the crate
    leaves that module, so the dependency stays at one boundary. The parse is
    pure and bounded, so the terminal event loop may run it. The parse of one
    hover answer still runs off that loop, because the code of a fence takes
    the Tree-sitter highlight of its language, and only a crate below
    `kvim-tui` may hold a grammar and select by a language name. The document
    is complete when it leaves `kvim-language`, and `kvim-tui` paints it. See
    [`language-services.md`](language-services.md).
  - Cost: compile time, and one further crate in the build, `unicase`. Every
    other dependency of the crate already stands in the lock file. One parse
    reads at most `MARKUP_SOURCE_BYTES_MAX` bytes, so it costs one pass over a
    bounded text and no more.
  - Version reason: the 0.13 line is the current release line of the crate. The
    default features carry an HTML renderer and an option parser, and the
    module walks the event stream instead, so both stay off.

## Release Profile

The Cargo release profile keeps `panic = "unwind"`. Terminal restoration must
not depend on unwinding.

A panic hook is the primary restoration path on every platform. The terminal
session installs the hook when it enters the terminal and removes it after a
successful restore, so the hook exists exactly while the terminal holds the
setup steps. The hook leaves the alternate screen, disables raw mode, shows the
cursor, restores the cursor shape, and pops the keyboard enhancement flags. It
then calls the hook that it replaced, so the normal panic message still reaches
the user. The hook writes the terminal steps only. It allocates nothing and
locks no editor state, because a panic can leave both unusable, and it ignores
every write failure, because no report path remains.

`Drop` is the secondary path. It restores the same steps where unwinding works.
Correctness never depends on it.

The measured reason: on macOS 26.5.1 a panic cannot unwind. The process prints
the panic message, reports `fatal runtime error: failed to initiate panic, error
5`, and aborts, so no destructor runs. A standalone measurement confirmed both
halves: a `Drop` guard did not run, and a hook of `std::panic::set_hook` did run
before the abort.

The behavior belongs to the operating system, not to one toolchain. The same
measurement aborts identically on the nixpkgs Rust 1.97.1 toolchain, on the
nixpkgs Rust 1.95.0 toolchain, and on the official upstream 1.97.1 toolchain
from fenix. No toolchain pin avoids it.

The `KVIM_PANIC_PROBE` environment variable makes the running executable panic
after its first frame. It verifies the restoration path in a pseudo-terminal.
Any value enables it. The composition root reads whether the variable exists,
and it never reads and never reports the value. The variable is a diagnostic,
not an editor feature.

The profile uses portable settings only. It does not use target-specific or
unsafe optimization flags.

## Nix And Packaging

The Nix flake pins `nixpkgs` through `flake.lock`. Development, package, check,
and application outputs support Linux and Darwin systems.

The `rust-toolchain.toml` file at the repository root names the exact Rust
version. It is the single source of truth for that version. The flake reads it
through the `rust-overlay` input, so the toolchain never drifts with `nixpkgs`.
The `legacyPackages` attribute set carries no overlay, so every output imports
`nixpkgs` with the overlay applied instead.

The development shell supplies Cargo, Rust, rustfmt, Clippy, and
`rust-analyzer` from that one pinned toolchain. It also supplies nixfmt, `git`,
and ripgrep from `nixpkgs`.

The package output builds with the pinned toolchain too. It builds a Rust
platform from the toolchain with `makeRustPlatform`, because the
`rustPlatform` attribute set of `nixpkgs` builds with the Rust of `nixpkgs`.

Continuous integration keeps its own toolchain. It verifies the minimum
supported version from `Cargo.toml`, not the pinned development version. The
workflow sets `RUSTUP_TOOLCHAIN`, because rustup reads `rust-toolchain.toml`
and that file would otherwise override the version of the job.

The package output builds the `kvim` executable from `Cargo.lock`. The package
version comes from `Cargo.toml`. Package metadata declares the MIT license.

kvim calls external commands for the read-only Git status, ripgrep search, the
language servers, the external formatters, and the system clipboard. The
package output wraps the executable and supplies `git`, ripgrep, and
`rust-analyzer`. The wrapper takes `rust-analyzer` from the pinned toolchain,
so the server version matches the compiler that the flake pins. It takes that
one program from the toolchain, and no other program of it. The complete
toolchain would put its Cargo and its Rust in front of the commands that the
user chose for the edited project. The wrapper supplies no other language
server and no formatter. The
registry declares 22 server programs and 12 formatter programs, and one
workspace uses few of them, so each of those programs comes from the host
`PATH`. The package check also needs `git` inside the build sandbox, because
the Git status tests run one real repository. The clipboard command comes from
the host platform, because it differs between macOS and each Linux display
server. A direct Cargo installation requires all of these commands on the
caller's `PATH`. kvim reports a missing command as a typed unavailable state
and stays usable.

Continuous integration verifies macOS and Linux together. Windows verification
stays outside the first release.

## Host Report

kvim answers one question about the host: does this machine hold the programs
that the editor runs? Two entry points ask it, and one builder answers both.
`kvim --diagnostics` prints the report and exits, before the editor starts.
`:diagnostics` opens the same report in a buffer, while a user edits. One
builder serves both, so the flag and the command can never disagree about what
the host holds.

The report names the version of the executable, the resolved workspace root and
whether the language services attach to it, the search command of the picker,
the Git command of the file tree, every language-server program and every
formatter program of the registry, the clipboard commands of this host, and the
resource limits. One program row names the program, whether the search path
holds it, and the languages that declare it. Each heading of a program section
counts the declared, the found, and the missing programs of that section, so a
reader finds an incomplete host in one line. The report carries no escape
sequence, so a redirected output and an editor buffer both stay readable.

`crates/kvim-tui/src/diagnostics.rs` owns the internal report builder because it
also serves the `:diagnostics` buffer. `kvim-embed` wraps that builder with
`WorktreeHostReportRequest` and `WorktreeHostWorkspace`.
`WorktreeHostReportRequest::built_in` selects the built-in language registry
without exposing its implementation type. The standalone binary uses those
facade-owned types and has no direct `kvim-tui` or `kvim-language` dependency.

The probe reads the executable search path once for each distinct program. One
adapter declares at most `LANGUAGE_SERVERS_MAX` servers and at most one
formatter. The registry of this build holds 25 adapters, so it names at most
125 programs. The picker and the file tree add one program each, so one report
of this registry probes at most 127 programs. `HOST_PROGRAMS_MAX` holds 128
lookups and covers every one of them. This build declares 22 server programs
and 12 formatter programs. A registry that passes the bound fails the report
loudly, because the adapter table and the bound then drifted apart.

A search-path lookup is filesystem work, so the terminal event loop must never
run it. `kvim --diagnostics` runs before that loop exists and probes directly.
`:diagnostics` submits one bounded worker job instead. The message line reports
that the probe runs, the editor stays fully usable, and the buffer opens when
the job answers. A second `:diagnostics` starts no second probe while one runs.
A cancelled, saturated, timed out, or failed probe opens no buffer and reports
the outcome on the message line, because the user asked for the report and must
learn that it failed. See [`responsiveness.md`](responsiveness.md).

## Binding Documents

- [`text-model.md`](text-model.md) owns text coordinates, edit transactions,
  undo, encoding, size limits, and the indent policy.
- [`input-actions.md`](input-actions.md) owns editor modes, semantic commands,
  shared key dispatch, input snapshots, and the standalone bindings.
- [`responsiveness.md`](responsiveness.md) owns background work, bounds,
  publication gates, latency budgets, and shutdown.
- [`windows.md`](windows.md) owns the window tree, layout, focus, resize,
  generic sidebars and their tree mechanics, the domain-neutral selector,
  rendering, the standalone theme, and the editor log.
- [`files.md`](files.md) owns buffers, saving, external-change conflicts,
  confined worktree paths, persistent undo files, workspace mutations, and
  picker limits.
- [`language-services.md`](language-services.md) owns the language adapter
  boundary, independent syntax, project-scoped LSP, position encoding, and the
  formatter.
- [`git.md`](git.md) owns read-only Git status and diff capture, review anchors,
  recorded entry states, and safe Git execution.
- [`diff-view.md`](diff-view.md) owns the presentation of one captured diff: the
  screen rows of one hunk, the two views, the changes panel, and the read state
  of one review.
- [`clipboard.md`](clipboard.md) owns the system clipboard boundary, the
  register shape rule, and the platform commands.
- [`settings.md`](settings.md) owns the `EditorSettings` structure and every
  default value.
- [`reviewgraph-integration.md`](reviewgraph-integration.md) owns the deferred
  ReviewGraph relationship and source attribution.
- [`embedding.md`](embedding.md) owns host, facade lifecycle, facade outcomes,
  and public examples. `kvim-embed` owns the supported high-level contract.
  `kvim-tui` owns only its hidden non-contract implementation seam.
