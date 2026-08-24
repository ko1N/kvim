# Embedding

## Ownership

This document owns host, driver, embedded editor, event lifecycle, workspace
composition, external use, and public example rules.

Kvim supplies bounded library capabilities. A host composes them. Kvim knows no
host session, agent, tool, task, plan, or other host-domain concept.

## Host Responsibilities

The host owns:

- the set of worktrees, sessions, and visible surfaces,
- workspace state, focus policy, and commands,
- terminal lifecycle and terminal events,
- asynchronous runtime startup and task supervision,
- surface composition and final event effects,
- cursor application and redraw scheduling.

The host constructs the asynchronous runtime and supervises every returned
driver future. It supplies a bounded worker and process spawner. Capacity is
isolated for one instance unless the host explicitly supplies a shared capacity
pool.

The host can supply clipboard, watcher, and LSP handles. These services are
optional. The standalone `kvim` binary constructs its implementations.

The host keeps every event loop free from filesystem, process, Git, LSP,
formatter, and Tree-sitter work. It submits synchronous syntax work through its
bounded worker spawner.

## Driver Responsibilities

`EditorDriver` owns the external services of one editor instance. It owns
request identities, result routing, publication gates, service handles, tracked
tasks, and shutdown state.

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

The host supplies a `ratatui::Rect` and `ratatui::Buffer` for rendering. The
editor accepts one explicit rectangle first, because the layout, the viewports,
and the cursor all follow that rectangle. It writes only inside that rectangle.
It validates that the rectangle holds cells, matches the accepted rectangle, and
fits the buffer before changing any cell. Invalid geometry returns a typed error
and leaves the buffer unchanged.

Rendering returns an optional cursor position and cursor-shape request. The
host decides whether to apply either request. The editor names its own cursor
shape and owns no terminal sequence.

## Editor Events

`EditorEvent` includes these facts and requests:

- `ActiveFileChanged`,
- `FileWritten`,
- `WorkspaceChanged`,
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

`WorkspaceComposer<SurfaceId>` combines:

- one generic split tree,
- generic sidebars,
- overlay scope and focus,
- one shared key resolver,
- which-key state.

The composer owns no surface instance, transcript, session, worktree list, host
command, or host-domain value. The host supplies opaque surface identities,
minimum dimensions, sidebar row metrics, input contexts, bindings, and styles.

One reduction routes a key or paste to one host command, surface command, typed
text owner, pending sequence, unsupported input, or unbound result. The composer
does not accept, store, or invoke a surface input or render callback.

A focus or overlay transition that needs surface state returns one bounded,
addressed `CompositionEffect::CancelPending { surface, transition }`. Focus and
overlay ownership remain unchanged. The host applies the effect to that surface
and returns its reset `InputContextSnapshot`.

`resume_transition` validates the transition identity, surface identity, and
snapshot generation. It requires empty count, operator, register, text-object,
and prompt phases before it commits focus or overlay ownership. This protocol
lets focus cross editor and review boundaries while the host keeps final focus
policy.

One layout pass returns sidebar, surface, overlay, and which-key placements
inside the supplied rectangle. The host renders each owned surface. The composer
performs no input or output, starts no task, reads no clock, and owns no terminal
lifecycle.

## External Use

An external host can consume syntax, LSP, keymap, UI, or the embedded editor
independently. Syntax highlighting requires no LSP, ratatui, editor, file,
project, or runtime session. LSP is optional for highlighting and editor use.
Cargo features let consumers disable unused languages and grammars.

Public crates support revision-pinned Cargo Git dependencies without a shared
parent workspace. [`architecture.md`](architecture.md) owns package stability,
MSRV, ratatui compatibility, and the exact feature matrix.

## Public Examples

Every public feature API has one dedicated, hermetic example. Module rustdoc
links directly to its owning example. Continuous integration compiles and runs
every example. One combined example does not replace a feature example.

The required examples are:

- `crates/kvim-syntax/examples/highlight.rs`
- `crates/kvim-lsp/examples/lsp_diagnostics.rs`
- `crates/kvim-ui/examples/sidebar.rs`
- `crates/kvim-ui/examples/split_windows.rs`
- `crates/kvim-ui/examples/which_key.rs`
- `crates/kvim-tui/examples/embedded_editor.rs`
- `crates/kvim-tui/examples/host_workspace.rs`
- `crates/kvim-tui/examples/worktree_diff_review.rs`

Each example demonstrates one feature and its minimum setup. Supporting public
types use their owning feature example. Internal helpers do not require another
example.

The LSP example starts itself as a deterministic fixture server. UI examples
render into test buffers. `host_workspace.rs` composes host-owned chat, a real
embedded editor, a real review surface, and sidebar surfaces through one shared
resolver. Editor, composition, and review examples use temporary worktrees.

No example requires a user-installed server, network access, terminal ownership,
or this repository as input.
