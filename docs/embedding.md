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

One reduction routes a key or paste to one host command, surface command, typed
text owner, pending sequence, unsupported input, or unbound result. The composer
does not accept, store, or invoke a surface input or render callback.

The host supplies the elapsed time with each reduction, and that time reaches
the which-key overlay alone. `WorkspaceComposer::reduce` therefore takes the
same `Option<Duration>` that `Resolver::dispatch` takes. `None` states that the
host draws no which-key overlay, so pending input arms no timer and a host that
reads no clock holds one composer, and one resolver, inside pure state.

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
- `crates/kvim-keymap/examples/dispatch_keys.rs`
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

`crates/kvim/tests/repository_policy.rs` enforces this policy. It checks that
every public feature module names an example file that exists, that no other
example replaces a feature example, and that every example link of the published
documentation resolves. [`architecture.md`](architecture.md) names the complete
set of release gates.
