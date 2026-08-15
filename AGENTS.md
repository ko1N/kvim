# Repository Guidance

## Repo Facts

- This repository is the standalone Kvim terminal editor.
- The project uses one Cargo workspace with one member crate, `crates/kvim`, that produces the `kvim` executable.
- Keep `core`, `editor`, `input`, `language`, `runtime`, `settings`, `terminal`, `tui`, and `workspace` as modules until two concrete consumers justify separate crates.
- A crate-root module named `core` shadows the `core` standard crate. Never write `use core::` in this crate. Write `std::` or `crate::core::` instead.
- macOS and Linux are both first-class platforms. Verify every release on both.
- Binding architecture documents live under `docs/`.
- Plans live under `plans/` and remain local workflow artifacts. Do not commit them.
- The project provides a Nix development shell. Run project commands through `nix develop -c <command>`.
- Commit subjects use imperative sentence case without a prefix.

## Architecture Rules

- Keep the terminal event loop free from filesystem, process, Git, language server, formatting, and Tree-sitter work.
- Keep visible editor state under one event-loop owner.
- Bound queues, buffers, files, searches, previews, protocol messages, retries, processes, worker jobs, and caches.
- Give every background request an explicit identity, cancellation owner, deadline, and buffer version.
- Reject obsolete picker, preview, analysis, formatting, and language server results.
- Represent text changes as deterministic edit transactions and apply them as undoable units.
- Stage fallible file and workspace mutations before changing live editor state.
- Keep byte offsets, character positions, source columns, and terminal-cell columns as distinct types or validated boundaries.
- Keep terminal, process, filesystem, clipboard, and language server behavior behind portable boundaries.
- Keep all adjustable behavior in the `EditorSettings` structure in `settings`.
- Select a language path only through a language adapter.
- Record architecture decisions in the owning document under `docs/` before implementation depends on them.
- Record the origin of adapted ReviewGraph code in a module document line.
