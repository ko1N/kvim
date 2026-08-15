# Language Services

## Ownership

The `language` module owns the language adapter registry, Rust Tree-sitter
analysis, and the `rust-analyzer` session. All language work runs off the
terminal event loop through bounded runtime services. See
[`responsiveness.md`](responsiveness.md).

## Language Adapter Boundary

Only a language adapter selects a path by language or file extension. Generic
`core`, `editor`, `runtime`, `terminal`, `tui`, and `workspace` code passes a
path and exact buffer content without inspecting the extension.

An adapter has a stable identifier and a version. It decides whether it supports
a path. Registry selection is deterministic. No match means unsupported.
Multiple matches are an ambiguous typed failure.

The first-release registry contains one Rust adapter. Only that adapter
recognizes case-sensitive `.rs` paths. Kvim supports no other language in the
first release.

## Rust Tree-Sitter Analysis

The Rust adapter parses buffer content with Tree-sitter. It reparses
incrementally after an edit transaction, so a small change does not reparse the
complete buffer.

Parsing and highlighting run only on the bounded worker service. They never run
on the terminal event loop.

Each analysis request carries the buffer version that produced its input. The
publication gate rejects a result whose buffer version is obsolete. An obsolete
result never changes visible state and never enters a cache. See
[`text-model.md`](text-model.md) for buffer versions.

The adapter returns:

- bounded highlight spans, and
- comment metadata for the comment-toggle command.

A highlight span identifies a line, a byte range inside that line, and a
terminal-independent highlight role. The adapter never returns a terminal color.

Comment metadata describes the line comment token and its placement rules for
Rust. The comment toggle uses that metadata and applies its change as one edit
transaction, so one undo reverses a complete toggle. The toggle preserves the
existing indent of each affected line.

Analysis enforces explicit limits on buffer bytes, buffer lines, visited syntax
nodes, traversal depth, and highlight spans. Kvim rejects a complete result that
exceeds a limit. It never publishes a truncated result. The concrete limit
values are not yet decided. Slice 12 must record them here before implementation
enforces them.

Highlighting is optional decoration. Unsupported, malformed, cancelled, timed
out, or oversized analysis renders plain text. It never changes buffer content,
line mappings, or the cursor position.

## Highlight Roles

Highlight roles are terminal-independent. The interface layer maps each role to
a theme role. The language boundary does not know the palette, and the theme
does not know Tree-sitter capture names. See [`windows.md`](windows.md) for the
theme rule.

## The rust-analyzer Session

Kvim runs one persistent `rust-analyzer` session for the workspace. The session
speaks the Language Server Protocol (LSP) over JSON-RPC.

The session owns:

- bounded JSON-RPC framing, with explicit header and message size limits,
- the child-process lifecycle, including start, `initialize`, `shutdown`,
  `exit`, and restart,
- repository containment for every path and every `file` URI,
- protocol limits for opened files, in-flight requests, received messages, and
  protocol bytes,
- explicit deadlines for every request and for the session itself,
- buffer-version checks for every result.

Repository containment rejects a path outside the workspace with a typed result.
The session decodes a `file` URI and rejects malformed escapes and traversal.

The session sends `didOpen`, incremental `didChange`, and `didClose` for the
buffers that it queries. It sends `didChange` only after an edit transaction
completes.

The session supports diagnostics, definition, hover, and document formatting in
the first release. It does not support completion, code actions, or symbol
rename.

Kvim uses `clippy` as the `rust-analyzer` check command. The value belongs to
`EditorSettings`. See [`settings.md`](settings.md).

A missing `rust-analyzer` executable is a normal unavailable state, not a
failure. Kvim reports the state and keeps editing available.

The session does not retry a failed request. Cancellation owns child
termination. Shutdown follows the order in
[`responsiveness.md`](responsiveness.md).

## Diagnostics

Diagnostics are decoration. They never change source text, line mappings, or the
cursor position. A diagnostic carries the buffer version that produced it. Kvim
discards a diagnostic for an obsolete buffer version.

Kvim orders diagnostics by position, so diagnostic navigation is deterministic.
The diagnostic float shows the diagnostics at the cursor position.

[`input-actions.md`](input-actions.md) owns the diagnostic keys.

## Formatting

Kvim requests document formatting from `rust-analyzer`. It applies the accepted
formatter edits as one edit transaction, so one undo reverses a complete format.

Kvim rejects formatter edits whose buffer version is obsolete. It never applies
an edit that was computed against different content.

Formatting has an explicit deadline. A timeout leaves the buffer unchanged and
does not block terminal input.

Format-on-save is enabled for each new buffer. The default belongs to
`EditorSettings`. The toggle is per buffer, so a change affects only the active
buffer and does not change the default for other buffers. Kvim shows the current
format-on-save state for the active buffer.

A format-on-save failure or timeout does not cancel the save. Kvim saves the
unformatted buffer content and reports the typed formatting state.
