# Architecture

## Purpose

This document owns the workspace shape, the module boundaries, the dependency
direction, state ownership, and the dependency ledger for Kvim.

Kvim is a standalone terminal modal editor for Rust. It builds one executable
named `kvim`. Kvim mutates text. macOS and Linux use one editor model. Platform
branches stay in terminal, process, filesystem, clipboard, and packaging
boundaries.

## Workspace

The repository uses one Cargo workspace. The workspace has one member crate at
`crates/kvim`. That crate produces the `kvim` executable.

Keep the modules below inside the one member crate. Extract a crate only when
two concrete consumers justify the extraction. A second consumer inside the same
executable is not a second consumer.

## Modules

| Module | Charter | Arrives |
|---|---|---|
| `core` | Deterministic text model: rope buffer, validated coordinates, edit transactions, undo and redo. Performs no input or output. Depends on no other module. | Slice 4 |
| `editor` | Modal editing state: cursors, motions, operators, registers, search, and dot-repeat. | Slices 5–6 |
| `input` | Editor modes, semantic commands, the mapping registry, the bounded sequence resolver, and which-key generation. | Slice 3 |
| `language` | The language adapter registry, Rust Tree-sitter analysis, and the rust-analyzer session. | Slices 12–13 |
| `clipboard` | The system clipboard boundary. Runs the platform clipboard command through the bounded process service. Holds no register value. | Slice 6 |
| `runtime` | Bounded background work: process and worker services, cancellation, deadlines, request identity, and publication gates. | Slice 2 |
| `settings` | The `EditorSettings` structure and its defaults. Depends on no other module. | Slice 1 |
| `terminal` | Terminal lifecycle, raw mode, the alternate screen, enhanced keyboard reporting, and normalized terminal events. | Slice 2 |
| `tui` | The window tree, layout, rendering, the theme, and the event loop. Sole owner of visible editor state. | Slices 7–8 |
| `workspace` | Files, buffers, atomic save, the file tree, workspace mutations, and pickers. | Slices 9–11 |

Modules communicate through narrow contracts. Generic terminal, runtime,
window, and file code must not contain language-specific path rules. Only a
language adapter selects a path by language or file extension.

## Dependency Direction

The dependency direction is one-way:

- Every module may depend on `settings`.
- `core` depends on no other module.
- `editor` depends on `core`.
- `tui` depends on `editor`.
- The binary is the composition root. It constructs dependencies and starts the
  editor.

Do not add a reverse dependency. Move a shared type down to `core` or
`settings` instead.

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

### Slice 1

- `thiserror`
  - Replaces: hand-written error types, `Display` implementations, and manual
    source chains.
  - May run: in every module, including `core`.
  - Cost: one derive macro at compile time. No runtime cost.

### Slices 2 And Later

These dependencies must not run inside `core`. They stay at the imperative
boundary.

- `crossterm`
  - Replaces: local raw mode, alternate screen, resize handling, enhanced
    keyboard reporting, and key decoding.
  - May run: in `terminal` only.
  - Cost: compile time and platform-specific transitive code.
- `ratatui`
  - Replaces: a local widget set, cell buffer, and layout implementation.
  - May run: in `tui` only.
  - Cost: compile time. Rendering cost stays bounded by the terminal buffer and
    the visible window content.
- `unicode-width`
  - Replaces: local terminal-cell width tables.
  - May run: in `terminal` and `tui` only. `core` defines the terminal-column
    coordinate type, but it does not measure cell width.
  - Cost: small. Work stays bounded to visible or otherwise bounded text.
- `futures-util`
  - Replaces: a local polling loop over terminal events.
  - May run: in `terminal`, `tui`, and `runtime`.
  - Cost: one small stream extension API.
- `tokio`
  - Replaces: local thread pools, channels, deadlines, and child-process
    handling.
  - May run: in `runtime` and the composition root. Other modules receive
    runtime services as injected values.
  - Cost: compile time, supply-chain size, and a worker thread pool.
- `tokio-util`
  - Replaces: local cancellation flags and shared shutdown state.
  - May run: in `runtime` only.
  - Cost: small. It adds owned cancellation tokens.
- `notify`
  - Replaces: local inotify and FSEvents code for external change hints.
  - May run: behind a portable `runtime` service boundary only.
  - Cost: platform-specific transitive code and one callback thread. Watch
    roots and callback delivery stay bounded.

### Slice 4

This dependency runs inside `core`, because the text storage is the text model.
`core` runs no other dependency except `thiserror`.

- `ropey` 1.6
  - Replaces: a local rope or piece table, a local line index, and local
    conversions between byte offsets, character positions, and line indexes.
  - May run: in `core` only. `core` keeps the rope private and exposes validated
    coordinates, edit transactions, and owned line text. No other module sees a
    rope type.
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

### Slice 12

These dependencies run only on the bounded worker service.

- `tree-sitter`
  - Replaces: a local Rust parser and incremental reparse logic.
  - May run: on the bounded worker service, inside the `language` module.
  - Cost: compile time, native code, and bounded parse memory for each buffer.
- `tree-sitter-highlight`
  - Replaces: local highlight-query execution and capture mapping.
  - May run: on the bounded worker service, inside the `language` module.
  - Cost: small addition over `tree-sitter`.
- `tree-sitter-rust`
  - Replaces: a local Rust grammar and local highlight queries.
  - May run: on the bounded worker service, inside the `language` module.
  - Cost: generated C code and compile time.

### Slice 13

These dependencies run only in the bounded language-server task.

- `serde`
  - Replaces: hand-written JSON-RPC envelope parsing.
  - May run: in the bounded language-server task, inside the `language` module.
  - Cost: derive macros and compile time.
- `serde_json`
  - Replaces: a local JSON parser and serializer.
  - May run: in the bounded language-server task, inside the `language` module.
  - Cost: compile time. Allocation stays inside the bounded task.

## Release Profile

The Cargo release profile keeps `panic = "unwind"`. Terminal cleanup runs while
the process unwinds, so the terminal returns to its normal mode after a panic.
An aborting profile would leave the terminal in raw mode with the alternate
screen active.

The profile uses portable settings only. It does not use target-specific or
unsafe optimization flags.

## Nix And Packaging

The Nix flake pins `nixpkgs` through `flake.lock`. Development, package, check,
and application outputs support Linux and Darwin systems. The development shell
supplies Cargo, Rust, rustfmt, Clippy, nixfmt, ripgrep, and `rust-analyzer`.

The package output builds the `kvim` executable from `Cargo.lock`. The package
version comes from `Cargo.toml`. Package metadata declares the MIT license.

Kvim calls external commands for ripgrep search, `rust-analyzer`, and the system
clipboard. The package output wraps the executable and supplies ripgrep and
`rust-analyzer`. The clipboard command comes from the host platform, because it
differs between macOS and each Linux display server. A direct Cargo installation
requires all of these commands on the caller's `PATH`. Kvim reports a missing
command as a typed unavailable state and stays usable.

Continuous integration verifies macOS and Linux together. Windows verification
stays outside the first release.

## Binding Documents

- [`text-model.md`](text-model.md) owns text coordinates, edit transactions,
  undo, encoding, size limits, and the indent policy.
- [`input-actions.md`](input-actions.md) owns editor modes, semantic commands,
  the mapping registry, sequence resolution, and the first-release bindings.
- [`responsiveness.md`](responsiveness.md) owns background work, bounds,
  publication gates, latency budgets, and shutdown.
- [`windows.md`](windows.md) owns the window tree, layout, focus, resize,
  sidebars, and the theme.
- [`files.md`](files.md) owns buffers, saving, external-change conflicts,
  persistent undo files, workspace mutations, and picker limits.
- [`language-services.md`](language-services.md) owns the language adapter
  boundary, Tree-sitter analysis, and the rust-analyzer session.
- [`clipboard.md`](clipboard.md) owns the system clipboard boundary, the
  register shape rule, and the platform commands.
- [`settings.md`](settings.md) owns the `EditorSettings` structure and every
  default value.
- [`reviewgraph-integration.md`](reviewgraph-integration.md) owns the deferred
  ReviewGraph relationship and source attribution.
