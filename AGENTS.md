# Repository Guidance

## Repo Facts

- This repository is the standalone kvim terminal editor.
- The project uses one Cargo workspace. Every module charter is one library crate under `crates/`: `kvim-clipboard`, `kvim-core`, `kvim-editor`, `kvim-input`, `kvim-keymap`, `kvim-language`, `kvim-lsp`, `kvim-path`, `kvim-runtime`, `kvim-settings`, `kvim-syntax`, `kvim-terminal`, `kvim-tui`, `kvim-ui`, and `kvim-workspace`. The `crates/kvim` binary crate produces the `kvim` executable.
- The supported external packages are `kvim-path`, `kvim-syntax`, `kvim-lsp`, `kvim-keymap`, `kvim-ui`, and the embedded facade in `kvim-tui`. `docs/architecture.md` owns the public feature matrix and the stability policy.
- `fixtures/consumer` compiles the public crates as an outside repository would. The workspace `exclude` key keeps it out of the workspace.
- `kvim-syntax`, `kvim-language`, and `kvim-tui` enable no grammar by default. Test them with `--features all-grammars` or `--all-features`.
- The crate boundaries make the one-way dependency direction a compile error. `docs/architecture.md` owns the layer table.
- Declare every dependency version in `[workspace.dependencies]` only. A member references it with `workspace = true`, grouped under the comment headers of that table.
- macOS and Linux are both first-class platforms. Verify every release on both.
- Binding architecture documents live under `docs/`.
- Plans live under `plans/` and remain local workflow artifacts. Do not commit them.
- The default Nix development shell uses Rust 1.97.1 from `rust-toolchain.toml` for normal development and releases. Run project commands through `nix develop -c <command>`.
- The workspace minimum supported Rust version (MSRV) is Rust 1.94.1 from `[workspace.package].rust-version`. The `msrv` Nix shell and CI enforce it separately.
- Commit subjects use imperative sentence case without a prefix.

## Verification Economy

- During multi-slice plan execution, run `cargo check` only for the directly affected crate or highest affected consumer.
- Run only tests added or changed by the current slice. Select them with exact test or module filters.
- Do not run a complete package or workspace test suite for each slice.
- Defer doctests, all examples, Clippy, release builds, MSRV rebuilds, `cargo doc`, and Nix build checks to the final closeout slice.
- A toolchain or verification-infrastructure slice can run its one required establishment check.
- Broaden verification before closeout only when a narrower command cannot prove the changed boundary. State the reason.
- If shared `sccache` stalls, retry the same narrow command without `sccache`. Do not replace it with a broad command.
- The final closeout slice runs every deferred test and slow compilation path once.

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
- Keep all adjustable behavior in the `EditorSettings` structure in `kvim-settings`.
- Select a language path only through a language adapter.
- Record architecture decisions in the owning document under `docs/` before implementation depends on them.
- Record the origin of adapted ReviewGraph code in a module document line.
