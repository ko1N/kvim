# Architecture

## Purpose

This document owns the workspace shape, the crate boundaries, the dependency
direction, state ownership, and the dependency ledger for kvim.

kvim is a standalone terminal modal editor for Rust. It builds one executable
named `kvim`. kvim mutates text. macOS and Linux use one editor model. Platform
branches stay in terminal, process, filesystem, clipboard, and packaging
boundaries.

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
| `kvim-input` | Editor modes, semantic commands, the mapping registry, the bounded sequence resolver, and which-key generation. |
| `kvim-language` | The language adapter registry, language-neutral Tree-sitter analysis, the syntax role set, the language-server session, the markup document of one server answer with the highlighted code of its fences, and the external formatter. The registry holds 25 adapters. Every adapter declares at least one language server, and 20 of them also declare an external formatter. [`language-services.md`](language-services.md) owns the table. |
| `kvim-clipboard` | The system clipboard boundary. Runs the platform clipboard command through the bounded process service. Holds no register value. |
| `kvim-runtime` | Bounded background work: process and worker services, the filesystem watch service, cancellation, deadlines, request identity, and publication gates. |
| `kvim-settings` | The `EditorSettings` structure and its defaults. Depends on no other crate. |
| `kvim-terminal` | Terminal lifecycle, raw mode, the alternate screen, enhanced keyboard reporting, normalized terminal events, and the process termination signals. |
| `kvim-tui` | The window tree, layout, rendering, the theme, and the event loop. Sole owner of visible editor state. Also builds the host report that the `--diagnostics` flag and the `:diagnostics` command show. |
| `kvim-workspace` | Files, buffers, atomic save, the file tree, the read-only Git status, workspace mutations, and pickers. |
| `kvim` | The binary and the composition root. Parses the command line, builds the runtime, prints the host report, and starts the editor. |

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

| Layer | Crate | Depends on |
|---|---|---|
| 0 | `kvim-settings` | — |
| 1 | `kvim-core` | `kvim-settings` |
| 1 | `kvim-runtime` | — |
| 1 | `kvim-terminal` | — |
| 2 | `kvim-input` | `kvim-settings`, `kvim-terminal` |
| 2 | `kvim-clipboard` | `kvim-runtime` |
| 3 | `kvim-editor` | `kvim-core`, `kvim-input`, `kvim-settings` |
| 3 | `kvim-language` | `kvim-core`, `kvim-runtime`, `kvim-settings` |
| 3 | `kvim-workspace` | `kvim-core`, `kvim-runtime`, `kvim-settings` |
| 4 | `kvim-tui` | every crate above |
| 5 | `kvim` | `kvim-language`, `kvim-settings`, `kvim-tui` |

`kvim-runtime` and `kvim-terminal` need no setting today. Add
`kvim-settings` to either one when a setting reaches it.

`kvim-clipboard` has one consumer. `kvim-tui` mirrors the unnamed register into
the system clipboard, and it reads the selected commands for the host report.
Both reach the platform through the same selection, so no module above the
crate names a clipboard command. See [`clipboard.md`](clipboard.md).

The binary is the composition root. It constructs dependencies and starts the
editor.

Do not add a reverse dependency. Move a shared type down to `kvim-core` or
`kvim-settings` instead. A reverse dependency is a Cargo cycle, so it fails the
build rather than a review.

## State Ownership

The terminal event loop is the sole owner of visible editor state. Background
work returns typed results through bounded channels. Background work cannot
mutate visible state.

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
  - May run: in `kvim-tui` only.
  - Cost: compile time. Rendering cost stays bounded by the terminal buffer and
    the visible window content.
- `unicode-width`
  - Replaces: local terminal-cell width tables.
  - May run: in `kvim-tui` only. `kvim-core` defines the terminal-column
    coordinate type, but it does not measure cell width, and `kvim-terminal`
    normalizes events rather than laying out cells.
  - Cost: small. Work stays bounded to visible or otherwise bounded text.
- `futures-util`
  - Replaces: a local polling loop over terminal events.
  - May run: in `kvim-terminal` only, which owns the event stream.
  - Cost: one small stream extension API.
- `tokio`
  - Replaces: local thread pools, channels, deadlines, and child-process
    handling.
  - May run: in `kvim-runtime`, the composition root, and the crates that own
    one bounded task of their own: `kvim-language`, `kvim-terminal`, and
    `kvim-tui`. Every other crate receives runtime services as injected values.
  - Cost: compile time, supply-chain size, and a worker thread pool.
- `tokio-util`
  - Replaces: local cancellation flags and shared shutdown state.
  - May run: in `kvim-runtime`, and in every crate that owns a cancellable
    request: `kvim-language`, `kvim-tui`, and `kvim-workspace`. A cancellation
    token crosses the service boundary with the request it belongs to.
  - Cost: small. It adds owned cancellation tokens.
- `notify`
  - Replaces: local inotify and FSEvents code for external change hints.
  - May run: behind the portable watch service of `kvim-runtime` only. The
    event loop of `kvim-tui` reads bursts from that service and never touches
    the platform API.
  - Cost: platform-specific transitive code and one callback thread. Watch
    roots and callback delivery stay bounded.

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

### The Worker Service

These dependencies run only on the bounded worker service.

- `tree-sitter`
  - Replaces: a local Rust parser and incremental reparse logic.
  - May run: on the bounded worker service, inside `kvim-language`.
  - Cost: compile time, native code, and bounded parse memory for each buffer.
- `tree-sitter-highlight`
  - Replaces: local highlight-query execution and capture mapping.
  - May run: on the bounded worker service, inside `kvim-language`.
  - Cost: small addition over `tree-sitter`.
- `tree-sitter-rust`
  - Replaces: a local Rust grammar and local highlight queries.
  - May run: on the bounded worker service, inside `kvim-language`.
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
  - May run: on the bounded worker service, inside `kvim-language`. Each crate
    is adapter data. No crate name reaches code above the adapter boundary.
  - Cost: generated C code and compile time for each grammar. kvim links every
    grammar into the executable, so a user installs no parser file. One host and
    one toolchain measured the whole set: the release executable grew from
    5,402,048 bytes to 19,872,288 bytes, and the cold release build grew from
    22.2 s to 29.5 s. The user accepted that cost, so the complete language
    table works in a normal build. A later release can move each language behind
    a Cargo feature.
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
  - May run: in the bounded language-server task, inside `kvim-language`.
  - Cost: derive macros and compile time.
- `serde_json`
  - Replaces: a local JSON parser and serializer.
  - May run: in the bounded language-server task, inside `kvim-language`.
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
  the mapping registry, sequence resolution, and the first-release bindings.
- [`responsiveness.md`](responsiveness.md) owns background work, bounds,
  publication gates, latency budgets, and shutdown.
- [`windows.md`](windows.md) owns the window tree, layout, focus, resize,
  sidebars, the theme, and the editor log.
- [`files.md`](files.md) owns buffers, saving, external-change conflicts,
  persistent undo files, workspace mutations, and picker limits.
- [`language-services.md`](language-services.md) owns the language adapter
  boundary, Tree-sitter analysis, the language-server session, the position
  encoding, and the formatter.
- [`git.md`](git.md) owns the read-only Git status boundary, the recorded entry
  states, the directory roll-up, and the ignored-entry strategy.
- [`clipboard.md`](clipboard.md) owns the system clipboard boundary, the
  register shape rule, and the platform commands.
- [`settings.md`](settings.md) owns the `EditorSettings` structure and every
  default value.
- [`reviewgraph-integration.md`](reviewgraph-integration.md) owns the deferred
  ReviewGraph relationship and source attribution.
