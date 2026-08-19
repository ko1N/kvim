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

Only a language adapter selects a path by language, by file extension, or by
file name. Generic `core`, `editor`, `runtime`, `terminal`, `tui`, and
`workspace` code passes a path and exact buffer content without inspecting
either key. No name, type, or assumption of one language appears above the
boundary.

An adapter has a stable identifier and a version. It decides whether it supports
a path. Registry selection is deterministic. No match means unsupported.
Multiple matches are an ambiguous typed failure.

Selection reads two lookup keys of the same path: the file extension and the
complete file name. Both keys are adapter data, so one selection path serves
both. The file name key serves a file whose extension names its tool instead of
its format, for example the JSON lock file `flake.lock`.

An adapter supplies data, not behavior:

| Item | Meaning |
|---|---|
| Identifier and version | The stable name of the adapter and of its analysis implementation. |
| File extensions | The case-sensitive extensions that the adapter owns. |
| File names | The case-sensitive complete file names that the adapter owns, for a file whose extension does not name its format. |
| Grammar | The Tree-sitter grammar entry point, its highlight query, and its optional injection and local queries. |
| Comment tokens | The line-comment token and the block-comment delimiters, each optional. |
| Indent rule | The node kinds that hold their content one level deeper, and the characters that close such a node. |
| Language servers | The declared servers of the language, in declaration order. One declaration names its stable identifier, the program, its arguments, the protocol language identifier, its formatting role, its workspace root markers, and the initialization options. |
| External formatter | The program that formats a buffer of this language, and its arguments in command order. One argument is a literal text, or the place of the document path. |

The analysis, the highlight walk, the indent query, the comment toggle, and the
renderer read only these values. A new language therefore needs one new adapter
and one more entry in the registry table, and no change anywhere else.

The registry contains one adapter for each of JSON, Markdown, Nix, Rust, and
TOML. Every match is case-sensitive. Each of the five adapters declares one
language server: `vscode-json-language-server` for JSON, `marksman` for
Markdown, `nil` for Nix, `rust-analyzer` for Rust, and `taplo` for TOML. A
later release adds an adapter for another language and for its language
server, because the Language Server Protocol is language independent.

One adapter declares a table of servers, not one server. A language whose tools
split the work therefore runs every declared server together. The order of the
table is the declaration order, and the merge rules below read that order, so
the merged answer never depends on which server answers first.

Four of the five adapters also declare an external formatter: `nixfmt` for Nix,
`prettier` for JSON and for Markdown, and `taplo` for TOML. Rust declares none,
so `rust-analyzer` formats a Rust buffer. The formatting section below owns the
precedence between the two paths.

TOML and Nix carry `#` as their line comment. JSON and Markdown define no
comment of their own, so their comment metadata carries no token, and the
comment toggle stays disabled and reports the reason. That is the same path
that a file without an adapter takes.

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

Highlight roles are terminal-independent, so `kvim-language` owns the role set.
A role names what a range of source is, never how it looks. `kvim-tui` maps each
role to one style and keeps every color.

The language boundary therefore does not know the palette, and the theme does
not know Tree-sitter capture names. The role mapping reads capture names only,
and Tree-sitter highlight queries share one capture vocabulary across grammars,
so the mapping serves every language. See [`windows.md`](windows.md) for the
theme rule.

The role set is fixed, because `kvim-tui` maps every role to one style. A
grammar whose query uses a name of the shared vocabulary that the mapping does
not yet cover therefore extends the mapping, never the role set. The `text`
family of the prose grammars is mapped that way: a title takes the type role, a
literal and a uniform resource identifier take the string role, and a reference
takes the constant role.

## The Language Server Session

Kvim is a general Language Server Protocol (LSP) client. It runs one persistent
session for each server that an adapter of the workspace declares. The session
speaks the protocol over JSON-RPC and knows no server product. rust-analyzer is
the first configuration of that client, not a special case inside it.

The adapter declares each server as data: the identifier, the program, its
arguments, the protocol language identifier, the formatting role, the workspace
root markers, and the initialization options. The session sends what the
declaration names. Adding a language server therefore means adding one
declaration to one adapter. No code above the adapter boundary changes, and no
name, type, or assumption of one server appears there.

One session identity is the pair of the adapter identifier and the declaration
identifier. The identity also carries the position of the declaration in the
table of its adapter, so every merge reads the servers in declaration order.
The identifier is unique inside one adapter. Two adapters may declare the same
program, which starts one session for each adapter.

Every result of a session carries that identity. One server that is missing,
that fails, or that stops therefore disables only its own session. Every other
server of the same language keeps serving the buffer.

The session owns:

- bounded JSON-RPC framing, with separate header and body limits,
- cumulative session budgets for input bytes, output bytes, requests, and
  messages, all enforced by one bounds helper,
- the child-process lifecycle, including start, `initialize`, `shutdown`,
  `exit`, and a bounded restart after a failure,
- workspace containment for every path and every `file` URI,
- protocol limits for open documents, in-flight requests, and every received
  list,
- explicit deadlines for the handshake, for every request, and for shutdown,
- buffer-version checks for every request and for every published result.

The session runs as one background task. The terminal event loop sends bounded
requests through one queue and reads typed results from another queue. It never
reads, writes, or waits for a server. A full request queue returns a typed
saturated result at once, and the caller keeps its previous visible state.

The handshake declares the UTF-8 position encoding. Kvim rejects a server that
does not confirm that encoding, because one protocol column must be one UTF-8
byte offset inside its line. Kvim also answers every unsolicited server request,
so an unimplemented request cannot stall the server.

Workspace containment rejects a path outside the workspace root with a typed
result. The session decodes a `file` URI and rejects another scheme, a malformed
escape, and a traversal component. A definition target outside the root is
rejected and never offered. Kvim validates every server-supplied range against
the exact source bytes before it uses that range.

The session sends `didOpen`, incremental `didChange`, and `didClose` for the
buffers that it queries. It derives the changes of one `didChange` from one
applied edit transaction, and it sends them in descending order, because the
protocol applies them one after the other. It sends `didChange` only after an
edit transaction completes.

The session supports diagnostics, definition, hover, and document formatting in
the first release. It does not support completion, code actions, or symbol
rename.

The Rust adapter declares `rust-analyzer` and maps the language-neutral check
depth of `EditorSettings` onto the `check.command` option of that server. The
default check depth runs `clippy`. That mapping function is the one place in
Kvim that names a setting of one concrete server. See
[`settings.md`](settings.md).

A language without a server declaration, a language whose servers the workspace
does not use, and a language whose declared executable is not installed leave
the editor fully usable with no diagnostics. Kvim reports the state once and
starts no further server for that language. A missing server is never an error
path that degrades editing.

A reload replaces the whole text of one buffer, and the reloaded buffer counts
its versions from the start. Kvim therefore synchronizes a reload as one fresh
document open that carries the reloaded text and the reloaded buffer version,
and it drops every queued incremental change of that buffer. No obsolete
version reaches the server, and the server copy replaces the old copy in one
step. See [`files.md`](files.md).

A crashed server restarts a bounded number of times. The new server holds no
document, so Kvim reports the restart and opens its buffers again. The session
does not retry a failed request. Cancellation owns child termination. Shutdown
follows the order in [`responsiveness.md`](responsiveness.md).

## Workspace Root Markers

One language server serves a workspace only when the workspace uses its tool. A
linter that needs a project configuration reports a failure for every buffer of
a workspace that holds no such configuration. That report is noise, because the
workspace never asked for the tool.

Each declaration therefore names its workspace root markers: the file names and
the directory names that prove that the workspace uses this server. A marker
matches a file of the workspace root. It also matches a directory of that root,
because a project proves a tool with both shapes.

The lookup reads the workspace root alone. It never walks to a parent
directory. Kvim resolves one workspace root for the complete editor session.
Workspace containment rejects every path outside that root. A parent directory
is therefore outside the workspace, and it decides nothing.

An empty marker table names no marker, so its server always starts. Every
adapter of the registry declares an empty table today, so every present server
keeps its behavior.

The language services read the workspace root once, when the editor creates
them and before the terminal event loop runs. The probe asks the filesystem for
one path for each distinct marker of the registry. Its cost therefore follows
the adapter data, and never the size of the workspace. The answer is the set of
markers that the root holds. Every later gate decision reads that set alone, so
no gate performs a filesystem lookup on the terminal event loop. The workspace
root does not change while the editor runs, so one probe answers for every
buffer of the session.

A root that the process cannot read records no marker. Every gated server then
stays off, and every server without a marker still starts.

A gated server starts no child process, so it never enters the session map and
never counts against `LSP_SESSIONS_MAX`. That bound counts the child processes
of one editor, and a gated server owns none.

A gated server is a normal state, not a failure. The editor stays fully usable,
Kvim reports the state once, and no request starts that server again. The state
stays distinct from a server that is not installed. A gated server was never
meant to run in this workspace. A server that is not installed was meant to run
and could not. A gated formatting server keeps the format-on-save state of its
buffer, as a server that is not installed does.

## Merging The Answers Of Several Servers

One buffer reaches every running server of its adapter. Each server answers on
its own, so the editor merges the answers before it changes visible state. The
rules below read the declaration order, never the arrival order, so one buffer
always shows the same result.

| Answer | Rule |
|---|---|
| Diagnostics | The editor keeps the newest set of each server and merges every set. Two diagnostics describe the same problem when their range and their message text are both identical, and the merge keeps the diagnostic of the earlier declaration. The merged list ascends by position. |
| Hover | The editor joins the non-empty answers in declaration order. One blank row separates two answers. |
| Definition | The editor takes the first non-empty answer in declaration order. |
| Formatting | Exactly one declaration of one adapter carries the formatting role. Only that server receives a formatting request, and only while its adapter declares no external formatter. |

The editor always records the producer of a diagnostic. It keeps the `source`
field of the protocol when the server sends one, and it names the declaration
identifier otherwise. The record is data, and it never depends on what the
screen shows.

The float shows that name before the message only while the merged diagnostics
of the buffer carry more than one producer name. One name alone needs no
prefix, because the prefix would then repeat on every row without telling the
reader anything.

The count reads the names, not the servers. One server reports under more than
one name when it separates its own tools: rust-analyzer names `rustc` for a
compiler diagnostic and `clippy` for a lint of the same buffer. The reader needs
both names, although one server sent them. A name that is empty names nothing,
so it never turns the other names on.

The rule reads the complete buffer, not the cursor position. Every diagnostic of
one buffer therefore names its producer, or none of them does, and one
diagnostic keeps its name while the cursor moves.

The editor waits for every server that accepted one question before it merges.
A server that fails, that times out, or that stops answers nothing, and the
merge continues with the remaining answers. A question therefore never waits for
a server that no longer runs.

## Protocol Limits And Deadlines

The `language` module names each bound as one constant. The constant and the row
below must always agree.

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Servers of one adapter | `LANGUAGE_SERVERS_MAX` | 4 servers | One language splits its work over a type checker, a linter, and few other tools. Four declarations cover that practice and still bound the merge of one buffer. |
| Root markers of one server | `LANGUAGE_ROOT_MARKERS_MAX` | 16 markers | One linter names every file name that can hold its configuration. The reference `eslint` configuration names twelve of them, so sixteen covers that practice and still bounds the probe of one workspace. |
| Sessions of one workspace | `LSP_SESSIONS_MAX` | 16 sessions | One workspace mixes few languages, and a session starts only when the user opens a buffer of its language. Sixteen exceeds normal practice and still bounds the child processes of one editor. |
| Frame header | `LSP_HEADER_BYTES_MAX` | 256 B | One `Content-Length` header and one optional `Content-Type` header fit far below this value, so a header that never ends stops early. |
| Frame body | `LSP_MESSAGE_BYTES_MAX` | 8 MiB | One `didOpen` carries a complete file. [`text-model.md`](text-model.md) bounds one file at 4 MiB, so 8 MiB keeps headroom for JSON escaping. |
| Session input | `LSP_INPUT_BYTES_MAX` | 512 MiB | The cumulative bytes that one session writes. A day of editing stays far below this value, and an unbounded write loop stops. |
| Session output | `LSP_OUTPUT_BYTES_MAX` | 512 MiB | The cumulative bytes that one session reads. The value matches the input budget, so neither direction can grow without limit. |
| Session requests | `LSP_REQUESTS_MAX` | 1,000,000 requests | One keystroke starts at most one request, so this budget covers a long session and still bounds a request loop. |
| Session messages | `LSP_MESSAGES_MAX` | 4,000,000 messages | A server sends progress and diagnostics without a request, so the message budget is larger than the request budget. |
| Open documents | `LSP_OPEN_DOCUMENTS_MAX` | 64 documents | The editor opens one document for each visible or recently used buffer. Sixty-four exceeds normal practice and still bounds the server memory. |
| Pending requests | `LSP_PENDING_REQUESTS_MAX` | 32 requests | A user produces few simultaneous questions. The bound stops a request storm from an automated caller. |
| Request queue | `LSP_REQUEST_QUEUE_CAPACITY` | 64 requests | The queue absorbs one burst of editor requests. A full queue returns a saturated result instead of waiting on the event loop. |
| Result queue | `LSP_EVENT_QUEUE_CAPACITY` | 256 results | The queue matches the runtime result queue of [`responsiveness.md`](responsiveness.md), so one slow frame does not stall a session. |
| Content changes | `LSP_CONTENT_CHANGES_MAX` | 4,096 changes | The transaction bound of [`text-model.md`](text-model.md). Every transaction that the buffer accepts can therefore synchronize. |
| Diagnostics | `LSP_DIAGNOSTICS_MAX` | 1,024 diagnostics | The bound counts the diagnostics that one server publishes for one document. One file with more than a thousand diagnostics is already unreadable. The renderer shows the diagnostics of the visible lines only. |
| Definition locations | `LSP_LOCATIONS_MAX` | 128 locations | One definition query answers with one target, or with few candidates. A larger list means a wrong or hostile answer. |
| Formatting edits | `LSP_FORMAT_EDITS_MAX` | 4,096 edits | The transaction bound of [`text-model.md`](text-model.md), so every accepted formatter answer becomes exactly one undoable transaction. |
| Progress string | `LSP_PROGRESS_CHARS_MAX` | 128 characters | One progress token, title, or message names one operation. A longer string cannot fit on the overlay row, so the session clips it and drops a token above it. |
| Hover text | `LSP_HOVER_BYTES_MAX` | 16 KiB | One hover float shows a signature and a short description. A larger text cannot fit on a terminal screen. |
| Restarts | `LSP_RESTARTS_MAX` | 3 restarts | A server that fails four times in one session is broken. Further restarts would loop instead of reporting the state. |
| Handshake deadline | `LSP_INITIALIZE_DEADLINE` | 30 s | A cold server indexes a workspace before it answers `initialize`. Thirty seconds reports a stuck server without failing a normal cold start. |
| Request deadline | `LSP_REQUEST_DEADLINE` | 5 s | A definition or a hover answer is interactive. Five seconds reports a stuck request while the buffer stays editable. |
| Formatting deadline | `LSP_FORMAT_DEADLINE` | 10 s | A formatter runs a complete pass over the document, so it needs more time than a position query. The value matches the process deadline of [`responsiveness.md`](responsiveness.md). |
| Shutdown deadline | `LSP_SHUTDOWN_DEADLINE` | 250 ms | Editor exit must stay immediate. A server that does not answer `shutdown` in 250 ms is killed instead. |

A received list that passes its bound produces a typed failure. Kvim publishes
no partial result. Nested lists of one answer share one element budget, so a
server cannot allocate without limit by splitting many elements over many short
lists.

Every bound above applies to one session. The merged diagnostics of one buffer
therefore hold at most `LANGUAGE_SERVERS_MAX` times `LSP_DIAGNOSTICS_MAX`
entries, because only the servers of one adapter describe one buffer. The merge
removes the duplicates, so the visible list is normally far shorter.

A language-server session owns a long-lived child process that no bounded
process service starts. `LSP_SESSIONS_MAX` therefore bounds those children on
its own, and the `PROCESS_CONCURRENCY_LIMIT` of
[`responsiveness.md`](responsiveness.md) keeps its whole capacity for the short
external commands of the editor.

## Work-Done Progress

Kvim declares the `window.workDoneProgress` client capability, so a server may
report the state of a long operation. The session accepts the
`window/workDoneProgress/create` request and parses the `$/progress`
notification. It publishes one typed report through the same event path that
every other result uses. No code above the session parses protocol text.

Every report carries the generation of the session attempt that produced it. A
restart raises the generation, because the new server assigns its own tokens.
The editor drops every report below the generation that it already accepted, so
a report of a server that no longer runs never changes visible state.

Only a `begin` report creates an item. A `report` or an `end` for a token that
no `begin` created addresses no item, so it changes nothing.

The same notification also carries the partial results of a request, whose value
holds no work-done stage. Progress is decoration, so the session drops every
report that it cannot read and never reports a failure for one.

## Notification Overlay

One overlay shows the work-done progress of the language servers. It sits in the
bottom right corner of the body band, above the statusline. It is decoration: it
moves no cursor, it changes no buffer text, and it paints over the buffer.

The overlay holds one group for each language server. A group shows its items
and then its own title row, which names the server and carries the spinner while
one item of the group runs. An item shows its state, its message, and the
completion that the server reported. A finished item shows the done icon and
leaves after its lifetime.

The overlay carries language server progress alone. Every other report of the
editor, such as a completed save, a clipboard notice, or a failed file
operation, stays on the message line and the statusline. The reference
`fidget.nvim` configuration also routes the editor notifications onto its
surface, and Kvim does not: a second surface for those reports repeats what the
message line already shows. An editor with no server activity therefore paints
no overlay and reports no deadline for one.

The overlay paints text alone, as the reference does. It carries no background
and no border, so the buffer text and the end-of-buffer markers stay visible
between and around its rows. Every theme role of the overlay owns a foreground
color only, because a background would hide the buffer behind it. The overlay
reaches the corner and keeps one cell between its text and the right edge, which
places the text where the reference places it. It keeps no row above or below
its text.

[`settings.md`](settings.md) owns the row bound, the spinner period, and the
lifetime of a finished item. [`responsiveness.md`](responsiveness.md) owns the
deadline path that advances the spinner, because the renderer draws only after a
visible state change and runs no frame loop.

## Diagnostics

Diagnostics are decoration. They never change source text, line mappings, or the
cursor position. A diagnostic carries the buffer version that produced it. Kvim
discards a diagnostic for an obsolete buffer version.

Kvim orders diagnostics by position, so diagnostic navigation is deterministic.
The diagnostic float shows every diagnostic of the cursor position, not the
first one alone. One blank row separates two diagnostics, so a reader sees where
one message ends and the next one starts.

Kvim holds the newest set of each server of one buffer, and it merges the sets
into the ordered list that the float and the navigation read. A new set of one
server replaces the previous set of that server alone. The section above owns
the merge rule and the producer name.

[`input-actions.md`](input-actions.md) owns the diagnostic keys.

## The language float

The hover answer and the diagnostics of the cursor position share one float, so
both follow one placement rule.

The float sits beside the cursor cell of the window that asked, not at the
bottom of the terminal. It stands one row below the cursor line, and it flips
above that line when the rows below cannot hold it. It never covers the cursor
line itself, because that line holds the text that the float describes. When
neither side holds the complete float, it takes the larger side and clips its
height there, so it stays anchored to the cursor.

The float belongs to one window, so it never reaches outside that window
rectangle. In a split it sits inside its own window. It starts at the cursor
column and moves left until its right edge sits inside the window.

The float bounds both dimensions. A long message wraps at `FLOAT_COLUMNS_MAX`
terminal cells, or at the window width when the window is narrower. The wrap
counts terminal cells, so it never splits a wide character. The float shows at
most `FLOAT_ROWS_MAX` rows and replaces the last row with `...` as soon as it
holds more, so no row disappears without a note.

The float is decoration. It changes no buffer text, no line mapping, and no
cursor position, and the next key closes it. See [`windows.md`](windows.md) for
the overlay layering.

## Formatting

Kvim formats one buffer through one formatter. An adapter declares an external
formatter, a formatting server, or neither of them.

An external formatter takes precedence. Kvim runs the declared program when the
adapter names one. It sends a document-formatting request only when the adapter
names no program. `ServerFormatting::Enabled` therefore names the fallback
formatter of its adapter: the server that formats while the adapter declares no
external program. Exactly one declaration of one adapter carries that role, so
two servers of one language never format the same buffer.

The rule follows the reference `conform.nvim` configuration. That configuration
formats with its own table of programs, and it keeps the language server as the
fallback.

Kvim applies the accepted answer of either formatter as one edit transaction, so
one undo reverses a complete format. It rejects an answer whose buffer version
is obsolete, and it never applies a change that was computed against different
content.

Formatting has an explicit deadline. A timeout leaves the buffer unchanged and
does not block terminal input.

### The External Formatter

An external formatter is adapter data: the program, and its arguments in command
order. One argument is a literal text, or the place of the document path. A
formatter that reads its rules from the file name needs that place. `prettier`
takes the document on standard input, and it selects its parser from the path
that this argument carries.

Kvim writes the exact buffer text to the standard input of the program, and it
reads the formatted document from the standard output. The program runs on the
bounded process service of [`responsiveness.md`](responsiveness.md), so it never
runs on the terminal event loop. Every buffer that Kvim loads stays below
`PROCESS_INPUT_BYTES_MAX`, so no buffer is too large for that service.

The answer replaces the complete document as one edit transaction. A formatted
document that equals the buffer content changes nothing, so a buffer that
already matches its formatter records no undo step.

The `language` module names each bound below as one constant. The constant and
the row must always agree.

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Arguments of one formatter | `FORMATTER_ARGS_MAX` | 8 arguments | One formatter names a subcommand, a standard-input flag, and the document path. Eight covers that practice and still bounds the command of one buffer. |
| Captured output | `FORMATTER_OUTPUT_BYTES_MAX` | 8 MiB | The limit counts standard output and standard error together. [`text-model.md`](text-model.md) bounds one file at 4 MiB, so 8 MiB holds the formatted document beside the warnings of the program. |
| Deadline of one run | `FORMATTER_DEADLINE` | 10 s | A cold formatter reads its configuration before it formats. The value matches `LSP_FORMAT_DEADLINE` and the process deadline of [`responsiveness.md`](responsiveness.md). |

Kvim also rejects a formatted document above the maximum file size of
[`text-model.md`](text-model.md), because that document would build a buffer
that Kvim refuses to load.

Kvim rejects the answer of a program that reports a non-zero exit code, that
writes no text although the buffer holds text, or that writes bytes that are not
UTF-8. No branch reads the message text of the standard error.

### Format On Save

Format-on-save is enabled for each new buffer. The default belongs to
`EditorSettings`. The toggle is per buffer, so a change affects only the active
buffer and does not change the default for other buffers. Kvim reports the
current state on the message line after each toggle. The statusline also shows
the state of the focused buffer, so a window focus change reports the state of
the buffer that the new window shows. See [`windows.md`](windows.md).

A buffer formats only while its language adapter declares a formatter. A buffer
without a file name, a path that no adapter owns, and an adapter that declares
neither an external formatter nor a formatting server therefore have no
formatter. Kvim shows no format-on-save state for such a buffer, and the toggle
reports the missing formatter instead of changing a state that no save can act
on. The per-buffer state itself stays unchanged, so a buffer keeps the state
that the user chose if a later release declares a formatter for its language.

The rule reads adapter data alone. An installed, missing, gated, or stopped
server is a runtime state that the reports of the sections above own, and a
formatter program that the host does not hold is the same kind of state. A
buffer whose adapter declares a formatter therefore keeps its format-on-save
state while that formatter is absent.

A format-on-save failure or timeout does not cancel the save. Kvim saves the
unformatted buffer content and reports the typed formatting state. A formatter
program that the host does not hold is a normal state. Kvim reports it once, as
it reports a language server that is not installed, and every later save writes
the unformatted content without a further report.
