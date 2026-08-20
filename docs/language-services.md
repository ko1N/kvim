# Language Services

## Ownership

The `language` module owns the language adapter registry, the Tree-sitter
analysis, the language-server session, the position encoding of that session,
and the external formatter. All language work runs off the terminal event loop
through bounded runtime services. See
[`responsiveness.md`](responsiveness.md).

## Language Adapter Boundary

The adapter boundary is the multi-language extension point of kvim. kvim is
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
| Language servers | The declared servers of the language, in declaration order. One declaration names its stable identifier, the program, its arguments, the protocol language identifier, its formatting role, its workspace root markers, the initialization options, and the optional workspace settings. |
| External formatter | The program that formats a buffer of this language, and its arguments in command order. One argument is a literal text, or the place of the document path. |

The analysis, the highlight walk, the indent query, the comment toggle, and the
renderer read only these values. A new language therefore needs one new adapter
and one more entry in the registry table, and no change anywhere else.

The registry contains one adapter for each of assembly, Bash, C, C++, CSS,
fish, GLSL, Go, HTML, JavaScript, JSON, Lua, Markdown, Nix, Python, Rust, SCSS,
SQL, Terraform, TOML, TSX, TypeScript, XML, YAML, and Zig. Every match is
case-sensitive. Twenty-two of the twenty-five adapters declare one language
server: `asm-lsp` for assembly, `bash-language-server` for Bash, `clangd` for C
and for C++, `vscode-css-language-server` for CSS and for SCSS, `fish-lsp` for
fish, `glsl_analyzer` for GLSL, `gopls` for Go, `vscode-html-language-server`
for HTML, `vscode-json-language-server` for JSON, `lua-language-server` for Lua,
`marksman` for Markdown, `nil` for Nix, `pyright-langserver` for Python,
`rust-analyzer` for Rust, `sqls` for SQL, `tofu-ls` for Terraform, `taplo` for
TOML, `lemminx` for XML, `yaml-language-server` for YAML, and `zls` for Zig.
JavaScript, TypeScript, and TSX each declare two:
`vscode-eslint-language-server` for the lint problems, and
`typescript-language-server` for the type problems. A later release adds an
adapter for another language and for its language server, because the Language
Server Protocol is language independent.

Exactly one adapter owns each extension and each file name. Two owners make
every path of that key an ambiguous failure, which leaves the buffer without
highlighting, without a server, and without a formatter. C therefore owns `c`
and `h`, and C++ owns `cc`, `cpp`, `cxx`, `hh`, `hpp`, and `hxx`. A plain
header carries C far more often than C++, so the plain extension belongs to C.
Assembly owns `s` and `S` as two separate names, because the match is
case-sensitive and the C preprocessor runs over the uppercase name alone. Bash
owns `bash` and `sh`, because one grammar reads the POSIX shell language and the
Bash extensions of it. Python owns `py` and `pyi`, because one grammar reads the
source and the stub of a module.

The web languages split their keys by grammar. HTML owns `htm` and `html`.
JavaScript owns `cjs`, `js`, `jsx`, and `mjs`, because one grammar reads the
module extensions and the JSX extension of the language. TypeScript owns `cts`,
`mts`, and `ts`. TSX owns `tsx` alone, because the grammar crate ships a second
grammar for that dialect, and the plain TypeScript grammar rejects the JSX
syntax. One adapter carries one grammar entry point, so TSX needs an adapter of
its own, although it declares the same servers and the same formatter as
TypeScript.

Bash also owns the file names `.bash_logout`, `.bash_profile`, `.bashrc`, and
`.profile`. Each name is a startup script of an interactive or a login shell,
and each one carries no extension, so the file-name key is the only key that
selects it.

The data and configuration languages split their keys the same way. YAML owns
`yaml` and `yml`, and it also owns the file names `.clang-format` and
`.clang-tidy`, because each one holds YAML and carries no extension. XML owns
`svg`, `xml`, `xsd`, `xsl`, and `xslt`, which are the extensions that `lemminx`
serves, and each one carries an XML document. The XML grammar crate ships a
second grammar for a standalone document type definition, and kvim compiles the
document grammar alone, because no registered extension names such a file. SQL
owns `sql`. Terraform owns `tf` and `tfvars`, and it leaves `hcl` unclaimed,
because a plain HCL file carries another tool that `tofu-ls` does not serve.

One adapter declares a table of servers, not one server. A language whose tools
split the work therefore runs every declared server together. The order of the
table is the declaration order, and the merge rules below read that order, so
the merged answer never depends on which server answers first. JavaScript,
TypeScript, and TSX are the languages that split the work today. Each one
declares the linter first and the type checker second, so the lint message of
`vscode-eslint-language-server` survives a merge with an identical message of
`typescript-language-server`. The linter names the rule that produced the
report, and the type checker does not.

Twenty of the twenty-five adapters also declare an external formatter: `black`
for Python, `clang-format` for C and for C++, `goimports` for Go, `lua-format`
for Lua, `nixfmt` for Nix, `prettier` for CSS, HTML, JavaScript, JSON,
Markdown, SCSS, TSX, and TypeScript, `shfmt` for Bash, `sql-formatter` for SQL,
`taplo` for TOML, `tofu fmt` for Terraform, `xmlformat` for XML, and `yamlfmt`
for YAML. The reference table names the `xmlformatter` package for XML, and the
command of that package is `xmlformat`, so the declaration names the command.
`prettier` selects its parser from the document path, so the declaration of
each of those eight languages names that path. fish, GLSL, Rust, and Zig
declare none, so `fish-lsp`, `glsl_analyzer`, `rust-analyzer`, and `zls` format
a buffer of their language. Assembly declares neither, because `asm-lsp`
supplies no document formatting, so an assembly buffer shows no format-on-save
state. `pyright-langserver` supplies no document formatting either, so `black`
is the only formatter of a Python buffer. The formatting section below owns the
precedence between the two paths.

TOML, Nix, assembly, Bash, fish, Python, Terraform, and YAML carry `#` as their
line comment, the C family, Go, GLSL, JavaScript, Rust, SCSS, TSX, TypeScript,
and Zig carry `//`, and Lua and SQL carry `--`. Terraform reads `#` and `//` as
a line comment, and `tofu fmt` writes `#`, so the adapter names that token. The
assembly grammar reads `#`, `//`, and `;`, because it serves several assembler
dialects. The adapter writes `#`, because the GNU assembler reads the file on
macOS and on Linux. Lua opens a long comment with `--[[` and closes it with
`]]`. Zig, Bash, fish, and Python define no block comment, so the metadata of
each one carries the line token alone. A triple-quoted Python text is a string
expression, not a comment, so it stays out of that metadata. CSS, HTML, and XML
define a block comment alone, and JSON and Markdown define no comment of their
own, so the comment metadata of those five languages carries no line token. The
first-release toggle reads the line token, so it stays disabled for all five and
reports the reason. That is the same path that a file without an adapter
takes.

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

A grammar crate sometimes ships the patterns of its own dialect alone, because
the upstream query inherits the patterns of a base language. kvim resolves no
query inheritance, so such an adapter joins the texts once and keeps the base
text first. C++ joins the C patterns, SCSS joins the CSS patterns, and
TypeScript and TSX join the JavaScript patterns. JavaScript joins the JSX
patterns of its own crate, because one grammar reads both dialects.

The HCL grammar crate ships no query at all, so Terraform would highlight
nothing. kvim therefore keeps one vendored query file beside the adapter
sources, at `crates/kvim-language/queries/hcl/highlights.scm`. The file carries
the origin, the upstream project, and the Apache 2.0 license of its text in its
own header, and the Terraform module document repeats them. The adapter
includes the file at compile time, so the single binary still needs no parser
file and no query file on the host.

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

The rule fits a language whose block carries an opening and a closing token,
because one node then spans the complete block. A C brace, a Bash `fi`, a fish
`end`, and a Lua `end` each close their node that way. The rule names the node
that spans the whole construct, and never the inner statement list, because two
entries would count one level twice.

Python is the one registered language that closes a block with indentation
alone. Its `block` node starts at the first token of the suite and ends at the
last one, so no node spans the header line and the body together. The Python
adapter therefore names the compound statement that owns each suite, and one
statement supplies the level of its own body. Two limits follow from that model,
and the user corrects each affected line:

- The last line of a suite reports one level too few, because the compound
  statement ends on that line and no token follows it.
- A compound statement whose header spans several lines reports one level too
  many, because the statement already supplies the level of its own body.

A bracketed Python expression carries its own opening and closing character, so
a list, a call, and a parameter list each behave exactly as the equivalent node
of a brace language.

HTML, XML, and TSX close a markup element with a tag, not with a character. An
`element` node and a `jsx_element` node each span the opening tag, the content,
and the closing tag, so each one supplies the level of its own content. A
closing tag opens with the same `<` character as an opening tag, so no closing
delimiter separates the two, and the adapter names none. A line that holds a
closing tag therefore reports one indent level too many, and the user corrects
that line.

YAML closes a block collection with indentation alone, exactly as Python closes
a suite. The YAML adapter therefore names the entry that owns each nested
collection, `block_mapping_pair` and `block_sequence_item`, and one entry
supplies the level of its own block. The same two limits follow, and the user
corrects each affected line: the last line of a block reports one level too
few, and an entry whose value spans several lines reports one level too many. A
`flow_mapping` node and a `flow_sequence` node carry their own brackets, so
both behave exactly as the equivalent node of a brace language.

SQL names its parenthesized constructs alone: the column list of a table, a
call with its arguments, a value list, a parenthesized predicate, and a nested
query. Each one carries its own opening and closing character, so each one is
exact. A select list carries no delimiter of its own, so it takes no level, and
the user indents a continuation of that list.

## Analysis Limits

Analysis enforces explicit limits on buffer bytes, buffer lines, syntax nodes,
traversal depth, and highlight spans. kvim rejects a complete result that
exceeds a limit. It never publishes a truncated result. The `language` module
names each bound as one constant. The constant and the row below must always
agree.

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Source bytes | `ANALYSIS_SOURCE_BYTES_MAX` | 4 MiB | The maximum file size of [`text-model.md`](text-model.md). Every buffer that kvim loads is therefore analyzable, and no larger text reaches the parser. |
| Source lines | `ANALYSIS_SOURCE_LINES_MAX` | 100000 lines | A source file of this length already exceeds normal practice. The check runs before the parse, so a generated one-line-per-byte file fails early. |
| Syntax nodes | `ANALYSIS_NODES_MAX` | 1000000 nodes | The densest measured source produces one node for each 5.6 bytes, so the byte limit produces about 750000 nodes. A larger tree means a pathological grammar result, not source that a reader edits. |
| Traversal depth | `ANALYSIS_DEPTH_MAX` | 128 levels | The indent query walks ancestors, and the highlight walk stacks captures. The bound measures syntax-tree depth, not source indentation, and a generated header reaches far more levels than a reader expects. |
| Highlight spans | `ANALYSIS_HIGHLIGHT_SPANS_MAX` | 100000 spans | The renderer reads the spans of the visible lines only, so a larger list would cost memory without improving the frame. A real file above 1.6 MiB of a heavy grammar passes the bound and renders plain text, which the section below records. |
| Analysis deadline | `ANALYSIS_DEADLINE` | 2 s | An incremental reparse and a highlight of a bounded file finish far below this value. Two seconds reports a runaway job, and highlighting is optional decoration, so a shorter deadline than the general worker deadline is safe. |

The deadline belongs to the request, and the bounded worker service enforces it.
See [`responsiveness.md`](responsiveness.md) for the worker bounds.

The bounds above were sized for five small grammars. A measurement over large
real files of the heavy grammars confirms two of them and corrects one.

The node bound and the depth bound hold. The densest measured file is a 7.97
MiB C++ header with one node for each 5.6 bytes, so the source-byte limit
produces about 750000 nodes. That same header reaches 119 levels of tree depth,
and a 574 KiB TypeScript declaration file reaches 91 levels. Both stay below
128 levels, but the margin is small, and a deeper real file would lose its
indent answer for one line.

The highlight-span bound is the one bound that a real file exceeds. A 1.7 MiB
C++ header and a 1.8 MiB TypeScript declaration file each produce more than
100000 spans. Each one therefore renders plain text. A 1.6 MiB C++ header
produces 96319 spans and still highlights. The behavior is the documented
oversized-analysis path below, and every such buffer stays fully editable. A
later release measures a larger bound against the renderer memory.

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
takes the constant role. The older words of the same vocabulary are mapped that
way too: a conditional and a repeat take the keyword role, a field takes the
property role, a method takes the function role, and a parameter takes the
parameter role. The `tag` word of the markup grammars is mapped that way as
well: a tag names the kind of an element, exactly as a type name names the kind
of a value, so it takes the type role. An HTML tag name, a CSS tag selector, an
XML tag name, and a JSX element name therefore share one role. The `markup`
family is the newer name of the same `text` family, so each of its names takes
the role of the older word that carries the same meaning: a heading takes the
type role, and a link and a raw text take the string role. The remaining older
words follow the same rule: a floating-point literal takes the number role, a
storage class takes the keyword role, and a member of a value takes the
property role.

A query may also mark one node with a name that carries no role at all, for
example the spell-check marker of another editor. The highlighter reads the last
capture of one node, so such a marker would take the place of the role and leave
the node plain. kvim therefore turns off every capture that carries no role
before it highlights, and the role of the node survives.

## The Language Server Session

kvim is a general Language Server Protocol (LSP) client. It runs one persistent
session for each server that an adapter of the workspace declares. The session
speaks the protocol over JSON-RPC and knows no server product. rust-analyzer is
the first configuration of that client, not a special case inside it.

The adapter declares each server as data: the identifier, the program, its
arguments, the protocol language identifier, the formatting role, the workspace
root markers, the initialization options, and the optional workspace settings.
The session sends what the declaration names. Adding a language server
therefore means adding one declaration to one adapter. No code above the
adapter boundary changes, and no name, type, or assumption of one server
appears there.

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
- the standard error of the child, which one background task drains and records
  inside its bounds,
- workspace containment for every path and every `file` URI,
- protocol limits for open documents, in-flight requests, and every received
  list,
- explicit deadlines for the handshake, for every request, and for shutdown,
- buffer-version checks for every request and for every published result.

The session runs as one background task. The terminal event loop sends bounded
requests through one queue and reads typed results from another queue. It never
reads, writes, or waits for a server. A full request queue returns a typed
saturated result at once, and the caller keeps its previous visible state.

The handshake offers the UTF-8 position encoding first and the UTF-16 position
encoding second. The section below owns the negotiation and the conversion. kvim
also answers every unsolicited server request, so an unimplemented request
cannot stall the server.

Workspace containment rejects a path outside the workspace root with a typed
result. The session decodes a `file` URI and rejects another scheme, a malformed
escape, and a traversal component. A definition target outside the root is
rejected and never offered. kvim validates every server-supplied range against
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
default check depth runs `clippy`. A declaration function of this kind is the
only place in kvim that names a setting of one concrete server. See
[`settings.md`](settings.md).

A language without a server declaration, a language whose servers the workspace
does not use, and a language whose declared executable is not installed leave
the editor fully usable with no diagnostics. kvim reports the state once and
starts no further server for that language. A missing server is never an error
path that degrades editing.

A reload replaces the whole text of one buffer, and the reloaded buffer counts
its versions from the start. kvim therefore synchronizes a reload as one fresh
document open that carries the reloaded text and the reloaded buffer version,
and it drops every queued incremental change of that buffer. No obsolete
version reaches the server, and the server copy replaces the old copy in one
step. See [`files.md`](files.md).

A crashed server restarts a bounded number of times. The new server holds no
document, so kvim reports the restart and opens its buffers again. The session
does not retry a failed request. Cancellation owns child termination. Shutdown
follows the order in [`responsiveness.md`](responsiveness.md).

### The Standard Error Of One Server

A server writes its own log to its standard error. That text names the cause of
a failure that the protocol never reports. A server that cannot start writes the
cause there and exits, so the editor must read that stream.

The session starts every server with a pipe on its standard error, and one
background task reads that pipe. The task is not optional. A child that writes
to a pipe that nobody reads blocks when the pipe fills. Several servers write to
their standard error while they run correctly.

The task separates draining from recording. It drains the stream until the
stream ends, so the pipe never fills and the server never blocks. It records at
most `LSP_STDERR_BYTES_MAX` bytes of one attempt. It records one further line
that names the bound, and it then drains the rest of that attempt without
recording it.

The task splits the stream into lines and clips one line at
`LSP_STDERR_LINE_BYTES_MAX` bytes. It drops an empty line, and it replaces every
byte sequence that is not valid UTF-8.

Each recorded line reaches the editor as one typed result of the session, on the
event path that carries every other result. The `language` module holds no
editor log, and it depends on no module above it. The editor records the line.
See [`windows.md`](windows.md).

The session reports its own lifecycle on that same path. It reports the start of
one server after the handshake, and it already reports a failure, a restart, and
a stop. The editor records each of those reports with the identity of the
server, so a reader knows which server changed its state and why.

A full result queue drops one output line instead of waiting for space. The
capture is a report, never a failure path, and the drain must never stop.

### The Two Diagnostic Models

The protocol carries diagnostics in two models, and kvim serves both.

| Model | Message | Direction |
|---|---|---|
| Push | `textDocument/publishDiagnostics` | The server sends one set without a request. |
| Pull | `textDocument/diagnostic` | The client asks, and the server answers one report. |

The handshake selects the model of one session. The session reads the
`diagnosticProvider` capability of the `initialize` result. A result that names
that capability selects the pull model, and every other result keeps the push
model. The model belongs to one server attempt, so a restart reads the
capability again.

The capability decides whether the session asks. Both models publish one
diagnostic set through the same path, so nothing above the session knows which
model produced a set. A pull session still accepts a published notification,
because the protocol lets one server send both.

kvim declares no `textDocument.diagnostic` client capability in this release. A
server that reads that capability therefore keeps the push model, and no
declared server of the registry changes its model.
`vscode-eslint-language-server` advertises `diagnosticProvider` without the
capability, so the pull path serves it. A later release declares the capability
after every declared server is measured against the pull path.

A pull session asks for one document at three moments:

- after the document opens,
- after a change of that document settles, which is
  `LSP_DIAGNOSTIC_PULL_DELAY` after the last accepted `didChange`, and
- after the server asks for a refresh.

One document holds at most one pull at a time. A later trigger waits for the
answer of the running pull, so a fast typist starts no request storm. A pull
also waits while the session already holds `LSP_PENDING_REQUESTS_MAX` requests,
so a workspace that opens many documents delays a pull instead of failing one.

Every rule of a request applies to a pull. The request carries the buffer
version of the document, `LSP_DIAGNOSTIC_DEADLINE` bounds it, and the session
rejects an answer whose buffer version moved. `LSP_DIAGNOSTICS_MAX` bounds the
returned items exactly as it bounds a published set.

The session asks with the document, with the provider identifier of the
capability, and with the result identifier of the previous report of that
document. The answer is one of two reports.

| Report | Content | Result |
|---|---|---|
| `full` | A result identifier and the complete items | The session publishes the items and records the identifier. |
| `unchanged` | A result identifier alone | The session publishes nothing and keeps the recorded identifier. |

An `unchanged` report means that the previous set still describes the document,
so the editor keeps the set that it already holds for that server. The result
identifier therefore saves one transfer of an unchanged document.
`LSP_RESULT_ID_BYTES_MAX` bounds one identifier, because the session holds one
identifier for each open document. The same bound measures the provider
identifier of the capability, which every request repeats.

A report may also carry a `relatedDocuments` member, which describes other
documents. The session ignores that member and never parses it. kvim pulls each
open document on its own, so a related report would repeat a set that the
session already asks for, and an ignored member allocates nothing.

A pull that fails, that times out, or that answers for an obsolete buffer
version publishes nothing. Diagnostics are decoration, so a lost pull leaves the
previous set and never reports a request failure to the editor. A transport
failure still ends the session attempt, as every other fatal failure does.

The server asks kvim to pull again with `workspace/diagnostic/refresh`. The
session accepts that request and schedules one pull for each open document of
that session. kvim answers every unsolicited server request, so no request of a
server can stall it.

### The Settings Channel

Some servers read their behavior from the workspace configuration of the client
instead of the initialization options. Such a server asks with
`workspace/configuration`, and it expects a `workspace/didChangeConfiguration`
notification when the configuration changes.

The settings are adapter data. One declaration names a pure function of the
language-neutral settings, exactly as it names the initialization options, so no
code above the adapter boundary names a setting of one concrete server.

A declaration that names settings changes three things for its own session:

- The `initialize` request declares the `workspace.configuration` client
  capability.
- The session sends one `workspace/didChangeConfiguration` notification with the
  declared settings after the handshake.
- The session answers the `workspace/configuration` request of the server.

The answer holds one value for each requested item. An item that names a section
receives the member of that name, and an item that names no section, or that
names the empty section, receives the complete settings object. A section that
the object does not hold receives the null value.
`LSP_CONFIGURATION_ITEMS_MAX` bounds the item list of one request.

A declaration that names no settings keeps the present behavior. It declares no
`workspace.configuration` capability, it sends no notification, and it reports
the `workspace/configuration` request as an unknown method. Such a server runs
with its own defaults.

The `eslint` declaration of JavaScript, of TypeScript, and of TSX is the one
declaration that names settings today. It names the four members that the server
needs to lint one document, and a probe of the installed server measured each
one:

| Member | Value | Reason |
|---|---|---|
| `validate` | `"on"` | The server returns an empty report for every other value. |
| `nodePath` | null | The server reads this member without a default, and an absent member ends the request with a type failure. |
| `problems.shortenToSingleLine` | false | The server reads this member without a default, and kvim wraps a long message in its own float. |
| `rulesCustomizations` | The empty list | The server walks this list without a default, and kvim changes no rule severity. |

`sqls` reports a problem only after a database connection reaches it through a
configuration. That declaration names no settings, so an SQL buffer stays fully
editable and shows no diagnostic in this release.

## The Position Encoding

kvim measures every column in UTF-8 bytes. The protocol measures a column in
UTF-16 code units unless the server confirms another encoding. The session
negotiates the encoding, records the answer, and converts every column at its
own boundary. No code above the session reads a protocol column.

The client offers `utf-8` first and `utf-16` second, so a server that supports
UTF-8 still selects UTF-8. The session then reads the `positionEncoding` field
of the `initialize` result.

| Answer | Result |
|---|---|
| `utf-8` | The session converts nothing. One protocol column is one byte column. |
| `utf-16` | The session converts every column in both directions. |
| No field | The protocol defines UTF-16, so the session converts as the row above. |
| Another value | The session rejects the server and reports the state once. |

Most installed servers name no encoding. `clangd`, `rust-analyzer`, and `zls`
confirm UTF-8. `gopls`, `nil`, `taplo`, `marksman`, and the servers of JSON,
Python, TypeScript, Bash, fish, Lua, SQL, Terraform, XML, and YAML all omit the
field. A gate that demands UTF-8 therefore refuses almost every declared
server.

A UTF-16 column indexes the line that its position names, so the conversion
needs the exact text of that line. A UTF-16 session mirrors the text of every
document that it holds open. The mirror holds the exact text that the session
sent to the server, and each `didChange` updates it before the next conversion
reads it. A UTF-8 session mirrors no text and pays no conversion cost.

A change that the mirror cannot apply proves that the session and the server
hold different text. The session drops that document, so no later answer of it
carries a converted column. The editor opens the document again on the next
resynchronization.

The mirror records the start of each line, so one conversion costs the length of
its line and never a walk over the document. One list of diagnostics therefore
stays linear in the text that it marks. Each open document stays below the
maximum file size of [`text-model.md`](text-model.md), and
`LSP_OPEN_DOCUMENTS_MAX` bounds the documents, so the mirrors of one session
stay bounded.

The conversion covers both directions.

| Direction | Values |
|---|---|
| Received | The range of a diagnostic, of a definition target, and of a formatting edit. |
| Sent | The range of every `didChange` change, and the position of a definition or a hover request. |

kvim reads no range of a hover answer, so that answer carries no column to
convert.

Two rules bound a column that its line does not hold. A column above the end of
its line becomes the end of that line, which is the rule that the protocol
defines. A column inside a character is a typed failure, because an edit at such
a column would split that character and corrupt the buffer. A line index that
the document does not hold is the same typed failure. kvim publishes no partial
result, so one rejected position rejects the complete answer that carries it.

A definition target can name a document that the session does not hold open. No
mirrored text then holds the line that the column indexes, so the target keeps
the line and the column that the server sent. The line stays exact, because no
encoding changes a line index, and the column stays exact for a line of ASCII
text. The target moves the cursor and changes no buffer content, so a line with
text above the Basic Multilingual Plane can place the cursor a few columns away.

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
directory. kvim resolves one workspace root for the complete editor session.
Workspace containment rejects every path outside that root. A parent directory
is therefore outside the workspace, and it decides nothing.

An empty marker table names no marker, so its server always starts. Only the
`eslint` declaration of JavaScript, of TypeScript, and of TSX names markers
today, and it names the twelve file names that can hold an eslint
configuration. Every other declaration of the registry names none. `clangd`,
`glsl_analyzer`, `gopls`, `zls`, and `asm-lsp` each serve a single file as well
as a complete project. `typescript-language-server`,
`vscode-html-language-server`, `vscode-css-language-server`, `lemminx`, `sqls`,
`tofu-ls`, and `yaml-language-server` do the same. The lookup also reads the
workspace root alone. A marker would therefore stop such a server in an
ordinary subdirectory layout.

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
kvim reports the state once, and no request starts that server again. The state
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
| Diagnostics | `LSP_DIAGNOSTICS_MAX` | 1,024 diagnostics | The bound counts the diagnostics that one server publishes for one document, and the items that one pulled report carries. One file with more than a thousand diagnostics is already unreadable. The renderer shows the diagnostics of the visible lines only. |
| Result identifier | `LSP_RESULT_ID_BYTES_MAX` | 256 B | The bound measures the result identifier of each open document of a pull session, and the provider identifier of that session. A server writes a counter or a hash there, so 256 bytes covers that practice and bounds what the session keeps. |
| Configuration items | `LSP_CONFIGURATION_ITEMS_MAX` | 64 items | One `workspace/configuration` request asks for the sections of few documents. The value matches `LSP_OPEN_DOCUMENTS_MAX`, so a server may ask for every open document at once and no more. |
| Definition locations | `LSP_LOCATIONS_MAX` | 128 locations | One definition query answers with one target, or with few candidates. A larger list means a wrong or hostile answer. |
| Formatting edits | `LSP_FORMAT_EDITS_MAX` | 4,096 edits | The transaction bound of [`text-model.md`](text-model.md), so every accepted formatter answer becomes exactly one undoable transaction. |
| Progress string | `LSP_PROGRESS_CHARS_MAX` | 128 characters | One progress token, title, or message names one operation. A longer string cannot fit on the overlay row, so the session clips it and drops a token above it. |
| Hover text | `LSP_HOVER_BYTES_MAX` | 16 KiB | One hover float shows a signature and a short description. A larger text cannot fit on a terminal screen. |
| Recorded standard error | `LSP_STDERR_BYTES_MAX` | 64 KiB | The bytes of the standard error of one server attempt that the editor records. A server that fails names its cause in its first lines, so this value holds that cause and bounds a server that writes without limit. The task drains every further byte and records none. |
| Standard error line | `LSP_STDERR_LINE_BYTES_MAX` | 1 KiB | One line of a server log names one state. The editor log clips an entry further, so this value bounds a stream that carries no line break. |
| Restarts | `LSP_RESTARTS_MAX` | 3 restarts | A server that fails four times in one session is broken. Further restarts would loop instead of reporting the state. |
| Handshake deadline | `LSP_INITIALIZE_DEADLINE` | 30 s | A cold server indexes a workspace before it answers `initialize`. Thirty seconds reports a stuck server without failing a normal cold start. |
| Request deadline | `LSP_REQUEST_DEADLINE` | 5 s | A definition or a hover answer is interactive. Five seconds reports a stuck request while the buffer stays editable. |
| Formatting deadline | `LSP_FORMAT_DEADLINE` | 10 s | A formatter runs a complete pass over the document, so it needs more time than a position query. The value matches the process deadline of [`responsiveness.md`](responsiveness.md). |
| Diagnostic pull deadline | `LSP_DIAGNOSTIC_DEADLINE` | 10 s | A pull analyses the complete document, and a cold linter loads its configuration first, so it needs the time of a formatter and not the time of a position query. |
| Diagnostic pull delay | `LSP_DIAGNOSTIC_PULL_DELAY` | 300 ms | The delay after which a change settles and the session pulls again. A typist produces keystrokes far below this interval, so one burst of edits starts one pull. A reader still sees a new report shortly after the last keystroke. |
| Shutdown deadline | `LSP_SHUTDOWN_DEADLINE` | 250 ms | Editor exit must stay immediate. A server that does not answer `shutdown` in 250 ms is killed instead. |

A received list that passes its bound produces a typed failure. kvim publishes
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

kvim declares the `window.workDoneProgress` client capability, so a server may
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
surface, and kvim does not: a second surface for those reports repeats what the
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
cursor position. A diagnostic carries the buffer version that produced it. kvim
discards a diagnostic for an obsolete buffer version.

kvim orders diagnostics by position, so diagnostic navigation is deterministic.
The diagnostic float shows every diagnostic of the cursor position, not the
first one alone. One blank row separates two diagnostics, so a reader sees where
one message ends and the next one starts.

kvim holds the newest set of each server of one buffer, and it merges the sets
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

kvim formats one buffer through one formatter. An adapter declares an external
formatter, a formatting server, or neither of them.

An external formatter takes precedence. kvim runs the declared program when the
adapter names one. It sends a document-formatting request only when the adapter
names no program. `ServerFormatting::Enabled` therefore names the fallback
formatter of its adapter: the server that formats while the adapter declares no
external program. Exactly one declaration of one adapter carries that role, so
two servers of one language never format the same buffer.

The rule follows the reference `conform.nvim` configuration. That configuration
formats with its own table of programs, and it keeps the language server as the
fallback.

kvim applies the accepted answer of either formatter as one edit transaction, so
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
that this argument carries. `clang-format` reads the same kind of path in the
argument that follows its `--assume-filename` flag, and it selects its style and
its language from that path.

A formatter that reads the format from the extension of that path needs one more
argument as soon as its adapter owns a file name. A file name key exists exactly
because the extension of that file names no format, so the program infers no
format from it and refuses the document. The declaration of such an adapter
therefore names the format itself, and it keeps the document path beside that
name for the configuration of the project. The JSON adapter shows the rule: it
owns `flake.lock`, so it names `--parser json` beside `--stdin-filepath`.

kvim writes the exact buffer text to the standard input of the program, and it
reads the formatted document from the standard output. The program runs on the
bounded process service of [`responsiveness.md`](responsiveness.md), so it never
runs on the terminal event loop. Every buffer that kvim loads stays below
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

kvim also rejects a formatted document above the maximum file size of
[`text-model.md`](text-model.md), because that document would build a buffer
that kvim refuses to load.

kvim rejects the answer of a program that reports a non-zero exit code, that
writes no text although the buffer holds text, or that writes bytes that are not
UTF-8. No branch reads the message text of the standard error.

### Format On Save

Format-on-save is enabled for each new buffer. The default belongs to
`EditorSettings`. The toggle is per buffer, so a change affects only the active
buffer and does not change the default for other buffers. kvim reports the
current state on the message line after each toggle. The statusline also shows
the state of the focused buffer, so a window focus change reports the state of
the buffer that the new window shows. See [`windows.md`](windows.md).

A buffer formats only while its language adapter declares a formatter. A buffer
without a file name, a path that no adapter owns, and an adapter that declares
neither an external formatter nor a formatting server therefore have no
formatter. kvim shows no format-on-save state for such a buffer, and the toggle
reports the missing formatter instead of changing a state that no save can act
on. The per-buffer state itself stays unchanged, so a buffer keeps the state
that the user chose if a later release declares a formatter for its language.

The rule reads adapter data alone. An installed, missing, gated, or stopped
server is a runtime state that the reports of the sections above own, and a
formatter program that the host does not hold is the same kind of state. A
buffer whose adapter declares a formatter therefore keeps its format-on-save
state while that formatter is absent.

A format-on-save failure or timeout does not cancel the save. kvim saves the
unformatted buffer content.

The save report names that state. The save writes the message line after the
format, so a format that wrote its own message would lose it to the save report,
and the user would read only that the file was written. The format therefore
hands its state to the save, and the save names the state beside its own result.

The rule gives the user three readable outcomes:

| Outcome | Report | Level |
|---|---|---|
| The save wrote formatted content, or no format ran | `"<path>" <lines>L, <bytes>B written` | Info |
| The formatter is not installed | `the formatter is not installed, so the file holds unformatted content; ` before the same report | Info |
| The formatter produced no usable document | `the formatter failed, so the file holds unformatted content; ` before the same report | Warning |

The reason leads the report. The message line clips at the terminal width, and
the path is the one unbounded part of the report. A narrow terminal therefore
clips the path and never the state of the written file.

A buffer whose adapter declares no formatter belongs to the first row. The
statusline shows no format-on-save state for such a buffer, so its save report
promises no format that kvim never runs.

The note qualifies a message that every save writes. It adds no message and
repeats none, so a formatter that fails on every save fills no message line.
This differs from the once-only reports of the sections above, which each add
one message of their own. A formatter program that the host does not hold is a
normal state, so its report keeps the level of an ordinary save.

An obsolete answer stays silent. The user typed while the formatter ran, so the
save writes exactly the content that the user typed and reports its own result
alone.
