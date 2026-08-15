# Language Services

## Ownership

The `language` module owns the language adapter registry, the Tree-sitter
analysis, and the language-server session. All language work runs off the
terminal event loop through bounded runtime services. See
[`responsiveness.md`](responsiveness.md).

## Language Adapter Boundary

The adapter boundary is the multi-language extension point of Kvim. Kvim is
language agnostic above that boundary. Rust is the primary target, because it
is the language that the editor is built for, but no code above the boundary
knows that.

Only a language adapter selects a path by language or file extension. Generic
`core`, `editor`, `runtime`, `terminal`, `tui`, and `workspace` code passes a
path and exact buffer content without inspecting the extension. No name, type,
or assumption of one language appears above the boundary.

An adapter has a stable identifier and a version. It decides whether it supports
a path. Registry selection is deterministic. No match means unsupported.
Multiple matches are an ambiguous typed failure.

An adapter supplies data, not behavior:

| Item | Meaning |
|---|---|
| Identifier and version | The stable name of the adapter and of its analysis implementation. |
| File extensions | The case-sensitive extensions that the adapter owns. An adapter that selects files by name overrides the path rule instead. |
| Grammar | The Tree-sitter grammar entry point, its highlight query, and its optional injection and local queries. |
| Comment tokens | The line-comment token and the block-comment delimiters, each optional. |
| Indent rule | The node kinds that hold their content one level deeper, and the characters that close such a node. |

The analysis, the highlight walk, the indent query, the comment toggle, and the
renderer read only these values. A new language therefore needs one new adapter
and one more entry in the registry table, and no change anywhere else.

The first-release registry contains one adapter, for Rust. Only that adapter
recognizes case-sensitive `.rs` paths. Support for further languages is
deferred. A later release may add adapters for other languages and for their
language servers, because the Language Server Protocol is language independent.

A file that no adapter serves stays a normal, fully editable buffer. It renders
plain text, it uses the fallback indent rule of
[`text-model.md`](text-model.md), and its comment toggle changes nothing and
reports the reason. An unsupported path is never a failure of the editor.

## Tree-Sitter Analysis

The adapter parses buffer content with Tree-sitter. It reparses incrementally
after an edit transaction, so a small change does not reparse the complete
buffer.

Parsing and highlighting run only on the bounded worker service. They never run
on the terminal event loop.

Each analysis request carries the buffer version that produced its input. The
publication gate rejects a result whose buffer version is obsolete. An obsolete
result never changes visible state and never enters a cache. See
[`text-model.md`](text-model.md) for buffer versions.

The adapter returns:

- bounded highlight spans,
- comment metadata for the comment-toggle command, and
- the indent level for one line.

A highlight span identifies a line, a byte range inside that line, and a
terminal-independent highlight role. The adapter never returns a terminal color.

Comment metadata carries the tokens of the language as data: one optional line
token and one optional pair of block delimiters. The one comment-toggle code
path reads that data, so a language with different tokens needs no new code.
The first-release toggle uses the line token. A language that has only block
delimiters needs the block toggle, which is deferred. The toggle applies its
change as one edit transaction, so one undo reverses a complete toggle. The
toggle preserves the existing indent of each affected line.

The indent level answers one question: how many indent levels does a new line at
this position take? The analysis reads the syntax tree with the node kinds that
the adapter names. A position inside such a node gains one level over its
enclosing node. A closing delimiter of that node loses one level. The analysis
returns a level count, not a column count, so [`settings.md`](settings.md)
keeps the tab width and the shift width.

The indent query must answer from the current buffer version without blocking the
terminal event loop. When the parse result for that version is not yet available,
the editor uses the fallback rule in [`text-model.md`](text-model.md) instead of
waiting. A late result never rewrites a line that the user already typed.

## Analysis Limits

Analysis enforces explicit limits on buffer bytes, buffer lines, syntax nodes,
traversal depth, and highlight spans. Kvim rejects a complete result that
exceeds a limit. It never publishes a truncated result. The `language` module
names each bound as one constant. The constant and the row below must always
agree.

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Source bytes | `ANALYSIS_SOURCE_BYTES_MAX` | 4 MiB | The maximum file size of [`text-model.md`](text-model.md). Every buffer that Kvim loads is therefore analyzable, and no larger text reaches the parser. |
| Source lines | `ANALYSIS_SOURCE_LINES_MAX` | 100000 lines | A source file of this length already exceeds normal practice. The check runs before the parse, so a generated one-line-per-byte file fails early. |
| Syntax nodes | `ANALYSIS_NODES_MAX` | 1000000 nodes | About one node for each four source bytes at the byte limit. A larger tree means a pathological grammar result, not source that a reader edits. |
| Traversal depth | `ANALYSIS_DEPTH_MAX` | 128 levels | The indent query walks ancestors, and the highlight walk stacks captures. Readable code nests far below 128 levels, so the bound stops only a damaged or hostile file. |
| Highlight spans | `ANALYSIS_HIGHLIGHT_SPANS_MAX` | 100000 spans | One span for each visible token of a large file. The renderer reads the spans of the visible lines only, so a larger list would cost memory without improving the frame. |
| Analysis deadline | `ANALYSIS_DEADLINE` | 2 s | An incremental reparse and a highlight of a bounded file finish far below this value. Two seconds reports a runaway job, and highlighting is optional decoration, so a shorter deadline than the general worker deadline is safe. |

The deadline belongs to the request, and the bounded worker service enforces it.
See [`responsiveness.md`](responsiveness.md) for the worker bounds.

Highlighting is optional decoration. Unsupported, malformed, cancelled, timed
out, or oversized analysis renders plain text. It never changes buffer content,
line mappings, or the cursor position.

## Highlight Roles

Highlight roles are terminal-independent. The `tui` theme owns the role set, and
it maps each role to one style. The language boundary does not know the palette,
and the theme does not know Tree-sitter capture names. The role mapping reads
capture names only, and Tree-sitter highlight queries share one capture
vocabulary across grammars, so the mapping serves every language. See
[`windows.md`](windows.md) for the theme rule.

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
