# Embedding

## Ownership

This document owns host, driver, embedded editor, event lifecycle, workspace
composition, external use, and public example rules.

Kvim supplies bounded library capabilities. A host composes them. Kvim knows no
host session, agent, tool, task, plan, or other host-domain concept.

## Facade Contract

`kvim-embed` is the only supported high-level editor facade. Its default
feature set publishes `MemoryEditor`. `WorktreeEditor` is an existing separate
rendered editor behind the `worktree` Cargo feature. Host-resolved input,
bounded binding publication, addressed cancellation, merged host registries,
independent presentation ownership, semantic command/status/sidebar state, and
standalone review are available through production facade APIs.
`MemoryEditor` remains the default path.
`ReviewSurface` is a separate rendered review surface behind the `review`
feature. Supplied candidates require no editor or service runtime. The
`worktree` feature adds bounded Git capture without constructing an editor.
`MemoryEditor` owns one bounded `TextBuffer`, one modal `EditingState`,
registers, one window, validated `EditorSettings`, and its accepted rectangle.
It applies caller-resolved
`Command` values, literal Insert-mode text, and host-owned elapsed time. Elapsed
time produces no transition because this editor has no timer-driven state. It
renders plain text, line numbers, and the cursor into a caller-supplied ratatui
buffer.

`MemoryEditor::open` validates the settings, applies its realized
`files.max_file_bytes` value as the text byte limit, and validates nonempty
geometry before it creates state. `MemoryEditor::resize` validates new geometry.
`MemoryEditor::render` validates that the complete accepted rectangle fits the
cell buffer before it changes a cell. Drop ends the pure in-memory lifecycle;
there is no explicit close operation because cleanup cannot fail.

The editor creates no worktree and has no service request type. Its default
Cargo dependency closure contains no `kvim-tui`, `kvim-runtime`,
`kvim-language`, `kvim-lsp`, `kvim-workspace`, `kvim-path`, `kvim-terminal`,
Tokio, crossterm, notify, or cap-std. Terminal lifecycle remains host-owned.
`crates/kvim-embed/examples/in_memory_editor.rs` opens supplied text, edits it,
renders it, and drops the editor.

The facade has two rendered editor types. `MemoryEditor` edits supplied bounded
text and renders to a caller-supplied ratatui buffer. It requires no worktree,
filesystem, Git, watcher, process, or language service. `WorktreeEditor` is a
separate type behind the `worktree` Cargo feature. It adds explicit worktree
capabilities.
Do not add a common editor trait until shared behavior requires one.

The default `kvim-embed` feature set is in-memory only. It must not compile
`kvim-tui`, `kvim-runtime`, `kvim-language`, `kvim-lsp`, `kvim-workspace`,
`kvim-path`, `kvim-terminal`, Tokio, crossterm, notify, or cap-std. The
`worktree` feature enables the worktree path and forwards grammar features to
both `kvim-language` and `kvim-tui` only through that path.

The `worktree` constructor owns a private Tokio executor and isolated bounded
worker, result, and process capacity. `WorktreeCapacity` validates those three
dimensions. Hosts call `dispatch`, await `ready`, pass the opaque
`WorktreeCompletion` to `apply`, and consume `shutdown`. A timed-out shutdown
returns `WorktreeDrain`, which owns the executor until mandatory events arrive.
No public signature names Tokio, channels, runtime work payloads,
`EditorDriver`, `Session`, or runtime, language, and workspace package types.
Public asynchronous methods still require host polling. Their internal work
runs on the private executor.

Explicit consuming `shutdown` is required to observe mandatory durable-work
events. Drop cancels internal owners before it shuts down the executor. Drop is
a best-effort fallback and does not promise durable event delivery.

The facade assigns each `WorktreeEditor` an opaque `WorktreeInstanceId`.
Recovery uses a separate facade-owned bounded recovery identity for each
file-backed buffer. A recovery event carries that identity, the addressed
`WorktreeInstanceId`, target metadata, baseline state, and bounded recovery
state. The host resolves it with an addressed `Restore`, `Discard`, or `Defer`
decision. `WorktreeEditor` rejects a wrong instance, recovery identity, buffer,
target, baseline, or revision before it changes visible state or deletes a
record. Restore is one undoable dirty replacement. Discard deletes only the
addressed record and keeps disk text. Defer keeps both values for a later open.
A stale baseline warns and remains available for explicit later disposal. These
filesystem capabilities exist only with the `worktree` feature. `MemoryEditor`
remains filesystem-free and publishes no recovery identity or event.

`WorktreeCompletion` carries that identity privately. `WorktreeEditor::apply`
returns `Result<WorktreeUpdate, WorktreeApplyError>`. A wrong editor returns
`WorktreeApplyErrorKind::WrongInstance` before it advances elapsed time,
releases or consumes a reservation, changes visible state, applies a service
result, or publishes an event. The error retains the completion, and
`WorktreeApplyError::into_completion` returns it for routing to its owner.
Facade signatures expose no `kvim-tui` identity, completion, result, or error
type.

The legacy `EditorDriver` validates its `Session` at `dispatch`, `apply`,
`shutdown`, and drain completion. These host-routable methods return
`DriverError::WrongInstance` in every build profile. `WorktreeEditor` owns its
session and driver together, so its internal dispatch and shutdown pairings are
infallible after construction. Its public `apply` returns `EditorApplyError`
for a completion from another instance.

`WorktreeCapabilities` defaults Git, watcher, language, and clipboard policies
to `Disabled`. The facade starts none of those services by default.
`ServicePolicy::BuiltIn` selects the supported production implementation and
makes startup mandatory. A requested language service or watcher that cannot
construct fails `open` with a facade-owned `WorktreeOpenErrorKind` and keeps its
private source chain. `ServicePolicy::BestEffortBuiltIn` selects the same
implementation but degrades a language construction failure to unavailable
language behavior. A watcher startup failure reports that no watcher runs, then
the editor remains usable and the refresh command reads the workspace by hand.
File open, edit, render, and save remain available in both cases. The
standalone uses best-effort built-in language and watcher services. It uses
required built-in Git and clipboard policies. Git has no opening operation, and
clipboard command availability is a runtime process outcome, so both policies
remain active after the editor opens. The facade
reports runtime service outcomes through its ordinary editor events.
Filesystem file open, edit, render, and save remain core worktree behavior.

The host owns terminal lifecycle, terminal input, signals, raw mode, alternate
screen, panic restoration, cursor application, and final redraw scheduling.
The facade owns no such terminal operation.

`kvim-tui` is the internal presentation implementation behind the optional
worktree facade. Its `#[doc(hidden)]` adapter modules are non-contract seams for
`kvim-embed` only. New hosts use `kvim-embed`.

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
| KV-A13 | `architecture.md` | A supported setting has production behavior; the stale wrapping setting was removed before release. |
| KV-A14 | `architecture.md` | A supported public path has production behavior; stale paths are removed before release. |

## Host Composition Contract

`WorktreeBindingMode::FacadeResolved` keeps physical resolution inside kvim and
uses `BindingProfile::Standalone`. `WorktreeBindingMode::HostResolved` selects
`BindingProfile::Embedded`, publishes its bounded `BindingManifest`, and makes
the host the only owner of physical key and paste arbitration. In that mode,
`WorktreeEditor::input` rejects key, paste, and unsupported raw input before
mutation. Resolver-independent resize input remains accepted.

The direct `command`, `literal`, and `paste` methods are semantic APIs. They do
not run or claim a physical binding. Hosts can use them for menus, command
palettes, and already-arbitrated text. A physical resolver result instead uses
`semantic_dispatch`, which validates the addressed instance, context
generation, active focus scope, and active picker overlay before mutation.

The host reads `binding_context` after each transition. It contains the current
semantic phases, active scope, optional picker overlay, generation, and the
host-supplied reserved escape key. Static `Pending` belongs only to the host
resolver. `TextObjectPending` updates kvim's semantic text-object phase.
Complete, text fallback, unbound, and interrupted decisions retain their typed
meanings at the facade boundary.

An interrupted decision changes no editor state. It returns an instance- and
generation-bound `CancelPendingProposal`. `cancel_pending` rejects a wrong
instance, stale generation, or idle context before mutation. A successful call
closes prompts and confirmations, clears counts, operators, registers, and text
objects, applies operator cancellation effects, advances the generation, and
only then returns an idle `CancelPendingResume`. The host changes focus only
after that resume succeeds.

The host-global scope and merged host registry are facade features in
host-resolved mode. `WorktreeBindingModel` merges bounded host-global,
host-leader, focused-context, and kvim manifest bindings into one generic
`kvim-keymap::Registry`. It projects each binding into a deterministic effective
scope before registry validation. Duplicate sequences and unreachable prefixes
therefore fail with typed errors, independent of registration order.

The host-global scope receives first refusal through the existing
`DispatchContext` order. Chat focus includes host and chat groups but no editor
group. Editor focus includes host leader and focused contributions only in
Normal, Visual, and sidebar contexts. Insert, picker, prompt, confirmation,
register-selection, and operator-pending contexts retain kvim input ownership.
Review focus includes host and review groups. The model publishes bounded owner
and semantic group labels for each command. Hosts use the existing `Resolver`
or `WorkspaceComposer` for dispatch, pending continuations, interruption hints,
and one which-key model; no facade resolver runs in parallel.

Composition rejects duplicate and strict-prefix conflicts by default. An
explicit bounded override identifies the effective scope, physical sequence,
and addressed host or editor command that wins. Composition rejects stale,
nonexistent, ambiguous, uncovered, or contradictory overrides. It never uses
registration order. Host and editor identities remain distinct when their
string identifiers are equal.

`WorktreeBindingModel::editor_context` validates the current
`WorktreeBindingContext`, then projects it with its picker overlay. The
host-supplied reserved escape key must resolve to one complete host-global
command. Missing, pending, editor-owned, or ambiguous escape bindings fail with
a typed error. The picker scope therefore precedes the focused prompt scope.
Ordinary editor contexts publish no overlay.

A host may ultimately compose input routing for editor, review, chat, and other
surfaces. Kvim also supports the existing facade-resolved path in which kvim
owns resolution and which-key presentation.

`BindingProfile::Standalone` preserves the current preset.
`BindingProfile::Embedded` disables review-entry bindings and host-navigation
conflicts by semantic command identity, while keeping semantic commands
available. Binding overrides reject duplicate sequences and unreachable
prefixes. `ReviewBindingProfile` configures standalone review independently from
editor bindings.

`WorktreePresentation` independently selects command-line, statusline,
which-key, and file-sidebar ownership. `standalone()` and the default keep all
four surfaces embedded. `integrated_host()` assigns all four to the host.
Builder methods support every mixed combination, and construction fixes the
choices for the editor lifetime.

The effective resolver still owns which-key presentation. Facade-resolved
input requires embedded which-key. Host-resolved input requires host-owned
which-key. `WorktreeEditorBuilder::open` returns
`WorktreeOpenErrorKind::Presentation` before worktree or live editor state
exists for either inconsistent combination.

A host-owned command line additionally requires `WorktreeCommandSurface`.
Opening without that marker returns `WorktreeOpenErrorKind::CommandSurface`
before root validation or live state construction. A host-owned statusline
requires no callback or capability.

Kvim realizes presentation before it constructs the private session.
Host-owned command and status rows have zero height and become body rows.
Host-owned file-sidebar presentation prevents creation of the private sidebar
region. Kvim therefore writes no blank placeholder cells for host-owned
surfaces. Embedded ownership keeps the existing chrome, sidebar, cursor,
viewport, and which-key behavior.

`WorktreeEditor::file_sidebar_snapshot` returns `None` for embedded ownership.
When the host owns the sidebar, it publishes at most
`FILE_SIDEBAR_ROWS_MAX` facade-owned rows. Each row carries a stable semantic
identity derived from its validated contained path or its parent and notice
kind. Labels, root labels, depth, paths, and the snapshot row list have
published bounds. Rows also carry kind and expansion/loading state, selection,
Git state, symbolic-link state, and semantic icon role. Notice rows carry no
path. Snapshot reads copy loaded state only and perform no filesystem or Git
work.

`WorktreeEditor::file_sidebar_command` accepts bounded movement, expansion,
collapse, refresh, activation, and focus-boundary commands. Directory and Git
work continues through `dispatch`, `ready`, and `apply`, including existing
request identity and obsolete-result rejection. Activation queues the selected
contained file in the editor. Embedded sidebar ownership refuses this host
command path and keeps existing rendering and input behavior.

The host owns sidebar placement, width, visibility, and focus order. It can draw
its own tree and kvim's snapshot as separate regions. Kvim accepts no host rows
and performs no merge. `crates/kvim-embed/examples/host_sidebar.rs` demonstrates
this two-tree composition.

The status snapshot is now a supported publication, not a planned addition.
`WorktreeEditor::status` returns a cheap borrowed `EditorStatusSnapshot` for one
`WorktreeInstanceId`. It includes the editing mode, active contained path,
modified state, one-based logical cursor position, access, bounded diagnostic
counts, and formatter availability with format-on-save state. It contains no
preformatted labels or private implementation values. The host polls it after
an applied transition, typically after `WorktreeUpdate::Redraw`; no separate
status event exists.

Embedded statusline rendering and facade publication derive mode, cursor, and
formatter state from the same semantic session facts. A host-owned statusline
still reserves zero rows.

The addressed command catalog is now a supported publication.
`WorktreeEditor::command_catalog` returns one bounded `EditorCommandCatalog`
for the current instance and input generation. Descriptors contain stable typed
identities, canonical and qualified names, aliases, descriptions, argument
schemas, current availability, and completion capability. Host and editor
commands remain separately addressed when their unqualified names collide.
Kvim parses and validates the selected editor command line.
`execute_addressed_command` rejects wrong-instance, stale-generation,
unavailable, and identity-mismatch requests before mutation. Worktree path
completion remains asynchronous work for the command-line lifecycle.

Host-owned command-line opening and completion sessions are supported.
`Command::OpenCommandLine` returns `WorktreeInputRequest::OpenCommandLine` with
one facade-owned `EditorCommandSessionId`; kvim opens no internal prompt.
`EditorCommandCatalog::complete_names` performs bounded pure name and alias
completion. `request_command_completion` sends contained-path work through the
normal `dispatch`, `ready`, and `apply` lifecycle. Each answer carries editor,
session, and host request identity. A newer request cancels the previous slot,
and the publication gate rejects obsolete work. `close_command_session`
cancels the active slot. The host owns line text, cursor, candidate selection,
and history. It must close the session before a prompt focus change, using the
same cancel-before-focus ordering as other pending input.

### Editor Sidebar Publication

Host-owned sidebar rows publish bounded identity, path, depth, kind, loading,
selection, Git, symlink, and icon-role facts. Kvim publishes its tree separately
and never accepts or merges host tree rows.

### Standalone Review

`ReviewSurface` is an additional standalone surface behind the `review`
feature. It does not replace integrated review in `WorktreeEditor`.
`ReviewSurface::from_candidates` accepts bounded immutable candidates and
performs no input or output. It uses facade-owned commands, events, errors, and
snapshots. Snapshot restoration relocates durable anchors without guessing.
The host owns focus policy, file opening, comment persistence, and comment
meaning. `ReviewConfig` selects `ReviewBindingProfile::Standalone` or
`ReviewBindingProfile::HostResolved` independently from editor bindings. It can
also supply bounded semantic review overrides. `ReviewSurface::bindings`
publishes the realized review-only manifest for host arbitration. The facade
accepts `ReviewInput` after arbitration. It does not resolve raw keys for
`ReviewSurface`. The standalone profile keeps the traditional review table. The host-resolved profile leaves `Tab` and
`Shift-Tab` unclaimed and publishes `]s` and `[s` for section navigation.

`ReviewSurface::for_worktree` adds bounded Git capture behind the `worktree`
feature. The surface privately owns capture dispatch, readiness, application,
cancellation, and consuming shutdown. One `dispatch` call submits every queued
half or follow-up step. The host then alternates `ready`, `apply`, and `dispatch`
until it receives `CaptureFinished` or `CaptureFailed`. Shutdown accepts a wait
timeout. Dropping the surface cancels capture and stops its private executor as
a best-effort safety net. The surface publishes staged and unstaged candidates
as one pair. Both paths share private review state, relocation, and painting
with integrated review.

## Worktree Implementation Contract

The sections below define worktree behavior published by `kvim-embed`.
`kvim-tui` implements presentation privately and does not define another
supported high-level integration.

## Host Responsibilities

The host owns:

- the set of worktrees, sessions, and visible surfaces,
- workspace state and focus policy outside each editor,
- terminal lifecycle and terminal events,
- polling facade readiness and applying returned completions,
- surface composition and final event effects,
- cursor application and redraw scheduling.

Each `WorktreeEditor` owns a private Tokio executor and its bounded worker,
process, result, and event capacity. The host selects those bounds with
`WorktreeCapacity`. The host can enable built-in watcher, language, Git, and
clipboard capabilities through `WorktreeCapabilities`. These services are
optional. The standalone `kvim` binary enables the production policies that it
uses. See [`clipboard.md`](clipboard.md).

The standalone binary is one such host. It is the only layer that owns raw
mode, the alternate screen, standard input, standard output, the terminal event
stream, termination signals, panic restoration, cursor application, redraw
scheduling, and shutdown order. `kvim-tui` owns none of these.

The host keeps every event loop free from filesystem, process, Git, LSP,
formatter, and Tree-sitter work. `WorktreeEditor::dispatch` submits queued work
to the facade-owned bounded executor without blocking the host loop.

## Facade Execution Responsibilities

The facade creates one private runtime during `WorktreeEditorBuilder::open` and
starts no detached task. It tracks every submitted task until completion or
cancellation.

Closing one editor cannot cancel or drain another editor's services. Every
completion carries its instance identity. Several editors can use one root or
different roots without sharing request or cancellation namespaces.

Shutdown consumes the editor. It rejects new work, cancels pre-commit work,
closes optional services, and waits for tracked work until the supplied
deadline. If the deadline expires while mandatory delivery remains, shutdown
returns a bounded, must-use `WorktreeDrain`. The drain owns the private runtime,
remaining tasks, reservations, and event delivery until `complete` returns.

## Embedded Editor

`WorktreeEditor` owns visible editor state for one explicit worktree root.
`WorktreeAccess::ViewOnly` rejects text and filesystem mutation.
`WorktreeAccess::ReadWrite` permits normal editing and bounded workspace writes.

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

The facade reserves one mandatory event slot before each save or workspace
mutation. It also reserves capacity before it accepts a recovery decision that
deletes a record. A recovery checkpoint itself uses the dedicated bounded
committing worker lane and does not delay ordinary open or save work. The
facade publishes its recovery event only after off-loop validation identifies a
candidate record. A `Committed` result consumes that slot and applies the staged visible
transition. An `Unchanged` result releases it and reports the causal failure.

An `Indeterminate` result consumes the slot with
`SaveReconciliationRequired` or `WorkspaceReconciliationRequired`. It preserves
dirty or stale visible state and starts bounded reconciliation through the
existing reload and tree-refresh requests. The facade never applies staged
mutation path updates from an indeterminate result.

`WorktreeEditor` is the public facade of one instance. `WorktreeEditor::builder`
takes the validated root and the first rectangle, because both bound what the
editor can reach. Every other setting has a default. `open` returns a typed
geometry error for a rectangle without cells, and it builds the model and the
driver of one instance together. `WorktreeEditor::shutdown` consumes the editor
and returns `WorktreeShutdown`. `Finished` holds every remaining
`WorktreeEvent`. `Draining` holds one `WorktreeDrain`, which owns mandatory
events from committed work and keeps the private runtime alive until
`WorktreeDrain::complete` returns them.
`crates/kvim-embed/examples/worktree_editor.rs` is one complete host of one such
editor.

The editor offers two explicit input paths. `WorktreeEditor::input` accepts
normalized terminal-neutral input and resolves kvim's built-in key tables
inside the facade. This path preserves prompt, confirmation, count, operator,
register, and text-object state.

A host that owns a resolver can instead call `WorktreeEditor::command` with the
resolved command, count, and register. `None` names the unnamed register. The
facade validates a supplied register before it changes editor state. The host
must forward the register carried by its resolution result. A host that drops
that name sends the operation to the unnamed register. See
[`clipboard.md`](clipboard.md).

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

### File Sidebar Support

Embedded sidebar ownership renders the built-in file tree as part of the
complete editor surface. Host-owned sidebar ownership removes that region and
publishes `FileSidebarSnapshot` with bounded semantic rows.
`WorktreeEditor::file_sidebar_command` accepts semantic movement, expansion,
collapse, refresh, activation, and focus-boundary commands. Directory and Git
work still uses facade dispatch, readiness, and application. The host owns
placement and painting. Kvim does not accept host rows or merge host and editor
trees. Generic bounded sidebar components remain available in `kvim-ui`.

The internal row painter reserves the first cell for the selection mark and the
last cell for the Git mark. It draws the selection mark only while the sidebar
has focus. An unfocused sidebar keeps that cell blank and retains the selected
row band, so focus changes move no row content.

When a label reaches the reserved Git cell, the final three visible text cells
fade toward the effective row background. The Git mark retains its own style in
the last cell. Short labels retain their normal color. These rules apply to the
built-in complete surface and do not publish file-row payloads or a custom row
painter through `WorktreeEditor`. [`windows.md`](windows.md) owns the selection
mark rule.

`kvim-tui` still contains component-level completion models and painters. These
are lower-level presentation APIs, not payloads of the supported worktree
facade.

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

A statusline usually names the mode. `WorktreeEditor::mode` answers the editing
mode of the editor, and `Mode` renders its own label, so a host builds the mode
segment from that value.

`WorktreeEditor::input_context` answers a different question. It publishes one
`InputContextSnapshot`, and its `scope` names the owner of the keys. The owner
is `BindingScope::Mode(Mode)` while the editor holds them, and it names a
prompt, the file sidebar, or the picker while one of those reads them. A host
that builds its mode segment from the scope alone therefore loses its mode
label whenever a prompt opens. The standalone editor keeps the mode on its
statusline through a prompt, and a host reaches the same fact through
`WorktreeEditor::mode`.

`crates/kvim-ui/examples/chrome_band.rs` is one complete host of one band.

## Editor Events

`WorktreeEvent` includes these facts and requests:

- `ActiveFileChanged`,
- `FileWritten`,
- `WorkspaceChanged`,
- `SaveReconciliationRequired`,
- `WorkspaceReconciliationRequired`,
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

Cancellation can stop `Reserved` or `Running` work before commit. For the
current runtime API, starting a committing blocking closure is its commit point.
Once that closure starts, the task reports its actual result and uses its
reserved slot before the driver can finish shutdown. Failure before commit
releases the reservation. The driver never detaches or aborts a task that can
be committed.

This sequence guarantees delivery after a side effect succeeds. Shutdown drains
all mandatory events or returns `WorktreeDrain`. It never reports complete while
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
complete binding of a preceding scope takes the key, so a host-global escape
can leave a focused surface. The composer clears only its resolver prefix. The
host resets semantic pending state before it runs the interrupting command.
A host-resolved `WorktreeEditor` supports this reset through
`semantic_dispatch` and `cancel_pending`; its facade-owned proposal and resume
also validate editor identity and context generation.

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
overlay ownership remain unchanged. A compatible host applies the effect
through its own surface reset contract and returns the reset
`InputContextSnapshot`. `resume_transition` then validates the transition,
surface, and snapshot before it commits focus or overlay ownership.

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

### The Standalone Editor Uses The Worktree Facade

Standalone kvim hosts one `WorktreeEditor`. It explicitly selects
`WorktreeBindingMode::FacadeResolved` and `WorktreePresentation::standalone()`.
The binary owns raw terminal input, the crossterm backend, signals, cursor
application, redraw scheduling, and terminal restoration. The facade owns
visible state, standalone key resolution, embedded command-line, statusline,
sidebar, and which-key presentation, background execution, completion routing,
and consuming shutdown.

`WorktreeEditor::input` accepts normalized terminal-neutral input when
`WorktreeBindingMode::FacadeResolved` is selected. This keeps raw terminal
ownership in the binary and preserves prompt and confirmation resolution inside
the facade. A host-resolved editor instead uses `semantic_dispatch` after its
own physical arbitration. The binary does not access `Session`,
`EditorDriver`, runtime handles, or completion payloads.

The composer remains available as a lower-level component for hosts that own
opaque surfaces and provide compatible context-reset behavior. It is not a
multi-surface `WorktreeEditor` facade. Its dedicated component example proves
the lower-level contract.

## External Use

An external host can consume syntax, LSP, keymap, UI, or the embedded editor
independently. Syntax highlighting requires no LSP, ratatui, editor, file,
project, or runtime session. LSP is optional for highlighting and editor use.
Cargo features let consumers disable unused languages and grammars.

Public crates support revision-pinned Cargo Git dependencies without a shared
parent workspace. Each supported package has an independent fixture under
`fixtures/consumer/`. Eight facade fixtures drive complete memory and worktree
editor lifecycles. They also prove host-resolved composition, mixed ownership,
unified command/status/which-key, host-owned sidebar, supplied review, and
worktree-captured review. The external-consumer script derives remote mode from
the checked-out repository's `origin` and never
prints that URL. Pass `--repository-url` to select another repository. Run
`scripts/check-external-consumer.sh --local-source` to verify uncommitted local
changes without remote authentication. [`architecture.md`](architecture.md)
owns package stability, MSRV, ratatui compatibility, and the exact feature
matrix.

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
links directly to its owning example. Continuous integration runs
`scripts/run-required-examples.sh` on macOS and Linux. The script reads the
list below, so this document is the single source for required examples. One
combined example does not replace a feature example.

The required examples are:

- `crates/kvim-path/examples/confine_worktree_paths.rs`
- `crates/kvim-fuzzy/examples/rank_candidates.rs`
- `crates/kvim-input/examples/edited_line.rs`
- `crates/kvim-keymap/examples/dispatch_keys.rs`
- `crates/kvim-syntax/examples/highlight.rs`
- `crates/kvim-lsp/examples/lsp_diagnostics.rs`
- `crates/kvim-lsp/examples/custom_lsp_transport.rs`
- `crates/kvim-embed/examples/host_owned_chrome.rs`
- `crates/kvim-embed/examples/host_sidebar.rs`
- `crates/kvim-embed/examples/in_memory_editor.rs`
- `crates/kvim-embed/examples/merged_leader.rs`
- `crates/kvim-embed/examples/supplied_review.rs`
- `crates/kvim-embed/examples/unified_command_line.rs`
- `crates/kvim-embed/examples/worktree_editor.rs`
- `crates/kvim-embed/examples/worktree_review.rs`
- `crates/kvim-ui/examples/composer.rs`
- `crates/kvim-ui/examples/selector.rs`
- `crates/kvim-ui/examples/sidebar.rs`
- `crates/kvim-ui/examples/split_windows.rs`
- `crates/kvim-ui/examples/tab_strip.rs`
- `crates/kvim-ui/examples/chrome_band.rs`
- `crates/kvim-ui/examples/which_key.rs`
- `crates/kvim-tui/examples/completion_menu.rs`
- `crates/kvim-tui/examples/worktree_diff_review.rs`

Each example demonstrates one feature and its minimum setup. Supporting public
types use their owning feature example. Internal helpers do not require another
example.

The LSP example starts itself as a deterministic fixture server. A UI example
renders into a test buffer, or prints the state that it drives when the feature
paints no cell. The in-memory editor example uses no temporary worktree.
Worktree editor, composition, chrome, sidebar, and review examples use temporary
worktrees.

No example requires a user-installed server, network access, terminal ownership,
or this repository as input.

`crates/kvim/tests/repository_policy.rs` enforces this policy. It checks that
every public feature module names an example file that exists, that no other
example replaces a feature example, and that every example link of the published
documentation resolves. [`architecture.md`](architecture.md) names the complete
set of release gates.
