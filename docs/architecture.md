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
| `kvim-ui` | Generic ratatui split, sidebar, the domain-neutral selector over `kvim-fuzzy`, which-key presentation, and the host-workspace composer over `kvim-keymap`. |
| `kvim-input` | Kvim commands, modes, prompts, the semantic reducer for counts, operators, registers, and text objects, and the standalone binding preset. Builds on `kvim-keymap`. |
| `kvim-language` | Syntax and LSP adapters, indentation, formatting, hover markup, and editor publication gates. The standalone registry holds 25 adapters. [`language-services.md`](language-services.md) owns the table. |
| `kvim-clipboard` | The system clipboard boundary. Runs the platform clipboard command through the bounded process service. Holds no register value. |
| `kvim-runtime` | Bounded background work: process and worker services, the filesystem watch service, cancellation, deadlines, request identity, and publication gates. |
| `kvim-settings` | The `EditorSettings` structure and its defaults. Depends on no other crate. |
| `kvim-terminal` | Terminal lifecycle and conversion from crossterm events into terminal-neutral `kvim-keymap` values. |
| `kvim-tui` | The embedded editor and review presentation models and standalone presentation adapters. It owns visible state for one supplied editor instance. It owns no terminal and no event loop. |
| `kvim-workspace` | Files, buffers, tree state, Git capture, review data, workspace mutations, and pickers built on the domain-neutral selector of `kvim-ui`. It owns no host worktree list or focus policy. |
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
| 1 | `kvim-core` | `kvim-settings` |
| 1 | `kvim-runtime` | `kvim-path` |
| 1 | `kvim-terminal` | `kvim-keymap` |
| 2 | `kvim-clipboard` | `kvim-runtime` |
| 2 | `kvim-input` | `kvim-keymap`, `kvim-settings` |
| 2 | `kvim-lsp` | `kvim-path` |
| 2 | `kvim-ui` | `kvim-keymap`, `kvim-fuzzy` |
| 3 | `kvim-editor` | `kvim-core`, `kvim-input`, `kvim-settings` |
| 3 | `kvim-language` | `kvim-core`, `kvim-lsp`, `kvim-runtime`, `kvim-settings`, `kvim-syntax` |
| 3 | `kvim-workspace` | `kvim-core`, `kvim-fuzzy`, `kvim-path`, `kvim-runtime`, `kvim-settings`, `kvim-ui` |
| 4 | `kvim-tui` | every library above, including `kvim-terminal` for the normalized event value alone |
| 5 | `kvim` | `kvim-language`, `kvim-path`, `kvim-runtime`, `kvim-settings`, `kvim-terminal`, `kvim-tui` |

External dependencies do not change the layer number. `kvim-ui` owns ratatui
geometry and rendering. No syntax-only consumer compiles LSP, ratatui, or the
editor.

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
value carries `Resize` and `Focus`, which are terminal facts and not key facts.
A keymap crate that named them would own two charters. The accepted cost is that
an external host of the embedded facade also compiles `kvim-terminal` and
crossterm, although `EmbeddedEditor` names no terminal type.

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
adds no cycle. `kvim-workspace` keeps its direct dependency on `kvim-fuzzy`
too, because it re-exports `score_candidate`, `FUZZY_NAME_WEIGHT`, and
`FUZZY_TEXT_CHARS_MAX` for a consumer that scores a candidate of its own
without the file, buffer, and picker vocabulary of `kvim-workspace`, and
because its own public `rank_candidates` function clips its query and then
calls `kvim_fuzzy::rank` over borrowed candidates. `kvim_fuzzy::rank` is the
one ranking rule that `Picker`, the command-line completion, and `Selector<R>`
all share.

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
`kvim-lsp`, `kvim-ui`, and the embedded facade in `kvim-tui`.

`kvim-input` publishes `Command`, the semantic reducer, and the binding preset,
so its action list is public. A consumer that resolves keys itself reads the
resolved command, count, and register name from it. `kvim-editor` publishes the
modal editing state over one `TextBuffer`, so a consumer can put a real Vim
buffer behind its own text field without the terminal session.

`kvim-fuzzy` holds the ranking rule alone: `score_candidate` scores one
candidate against one query, and `rank` turns those scores into one ordered
list of source indexes. A host that ranks a list of its own values takes
either without the file, buffer, and picker charter of `kvim-workspace`,
which is not a supported package. `kvim-ui` publishes `Selector<R>` over that
same rule, so a host that also wants the bounded query, candidate, match, and
selection mechanics takes `kvim-ui` instead of calling `kvim-fuzzy` directly.

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
1.94.1. This document records the target policy. The toolchain and manifest
changes belong to the MSRV implementation slice.

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

Continuous integration checks minimal features, each required feature, default
features, and all valid feature combinations. This matrix is exact:

| Crate | Default | Required CI combinations |
|---|---|---|
| `kvim-path` | no optional production features | default |
| `kvim-keymap` | no optional production features | default |
| `kvim-lsp` | no optional production features | default |
| `kvim-ui` | no optional production features | default |
| `kvim-syntax` | no grammar | no grammar, each grammar alone, `all-grammars` |
| `kvim-tui` | no grammar | no grammar, each forwarded grammar alone, `all-grammars` |

`kvim-language` forwards the same grammar features without a default grammar.
The `kvim` binary enables `all-grammars`. Private `test-support` features are
not external combinations. Record an architectural reason before excluding any
future combination.

## Enforced Policy

Continuous integration turns each rule above into a release gate. It runs every
gate on macOS and on Linux.

| Gate | Command | It proves |
|---|---|---|
| Feature examples | `cargo run -p <package> --example <name>` for all eight examples | Every dedicated example still runs and still asserts its own facts. |
| Example policy | `cargo test -p kvim --test repository_policy` | Every public feature module names an example file that exists, no extra example replaces a feature example, and every documented example link resolves. |
| Rustdoc links | `cargo doc --workspace --no-deps --all-features` under `RUSTDOCFLAGS=-D warnings` | Every intra-doc link of the published documentation resolves. |
| Dependency edges | `scripts/check-dependency-edges.sh` | Every direct and transitive kvim edge appears in the layer table above, each isolation charter reaches none of the external crates that it refuses, and every dependency of a supported package is reachable from the same revision or from crates.io. |
| Syntax isolation | `cargo check -p kvim-syntax --no-default-features [--features …]` | The syntax package builds with no grammar, with one grammar, and with every grammar. |
| External consumer | `scripts/check-external-consumer.sh` | `fixtures/consumer` compiles every combination of the matrix above as a revision-pinned Git dependency, without a shared parent workspace, with the development toolchain and with the minimum supported Rust version. |

The dependency gate reads the layer table of this document, so the policy and
the architecture cannot disagree. A new charter row changes both at once.

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
  - May run: in `kvim-ui`, `kvim-tui`, and the standalone composition root,
    which owns the terminal backend, the cell buffer of the process terminal,
    and the draw call.
  - Cost: compile time. Rendering cost stays bounded by the terminal buffer and
    the visible window content.
- `unicode-width`
  - Replaces: local terminal-cell width tables.
  - May run: in `kvim-ui` and `kvim-tui`, which lay out cells. `kvim-core`
    defines the terminal-column coordinate type, but it does not measure cell
    width, and `kvim-terminal` normalizes events rather than laying out cells.
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
    `kvim-tui`, and the standalone composition root. Public drivers return
    futures and create no runtime. Every task starts through a caller-supplied
    bounded spawner.
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
model. `kvim-core` runs no other dependency except `thiserror`.

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

`crates/kvim-tui/src/diagnostics.rs` owns the builder. `kvim-tui` is the lowest
crate that sees the language registry, the clipboard selection, the workspace
limits, and the buffer that the command opens. The binary calls the same
builder and prints what it returns.

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
  generic sidebars and rendering, the standalone theme, and the editor log.
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
- [`embedding.md`](embedding.md) owns host, driver, embedded editor, event
  lifecycle, composition, external use, and public examples.
