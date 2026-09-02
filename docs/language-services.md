# Language Services

## Ownership

`kvim-syntax` owns grammar selection, parser ownership, bounded highlighting,
and theme-independent syntax classes. `kvim-lsp` owns language-neutral,
project-scoped protocol and process sessions. This includes validated launch
requests and injectable launch and lifecycle capabilities. `kvim-language`
owns grammar-independent service profiles, the headless diagnostics registry
and project projection, and the optional syntax/editor composition. It also
owns indentation, formatting, hover markup, and editor publication gates.

All parser, filesystem, process, formatter, and LSP work runs off every host
event loop through bounded runtime services. A bounded markup parse is pure and
can run on the visible-state owner. Tree-sitter work cannot. See
[`responsiveness.md`](responsiveness.md).

The `kvim-language` public registry and service construction boundary validates
all public declarations before it creates live state. It rejects duplicate
aliases, duplicate server identifiers, conflicting formatter declarations,
invalid root markers, invalid bounds, and mismatched roots with typed errors.
These checks run in release builds. A failed construction reserves no service,
process, or registry state.

The supported worktree facade keeps service construction private.
`WorktreeCapabilities::language` defaults to `Disabled`.
`ServicePolicy::BuiltIn` constructs services from the editor root and settings
and makes construction mandatory. `ServicePolicy::BestEffortBuiltIn` uses the
same built-in registry but continues with language behavior unavailable when
construction fails. The standalone uses this best-effort policy, so a language
construction failure does not prevent file editing.
No facade signature exposes a registry, session, runtime, or service handle.

No grammar feature is a supported configuration. In that configuration,
`LanguageRegistry::first_release()` returns a valid empty grammar-backed
registry and never panics. Its path lookup returns
`AnalysisError::UnsupportedPath`. Its language-name lookup returns `None`.
Analysis and formatter selection therefore return their existing typed
unsupported or unavailable outcomes. Markup parsing remains available, and a
fenced block stays plain because no grammar adapter can highlight it.
`LanguageServices::new` remains available and constructs an empty editor
service set; a later path request returns `LspError::UnsupportedPath` without
starting a process. A worktree facade can therefore use either built-in service
policy without assuming that Rust or another grammar exists.

The separate `DiagnosticsRegistry::first_release()` is grammar-independent and
contains every first-release language service profile. It supports headless
path selection and diagnostics composition without `kvim-syntax`, Tree-sitter,
`kvim-core`, `kvim-runtime`, markup, editor, TUI, terminal, or rendering
dependencies. `kvim-language` enables `editor-services` by default for
compatibility with existing editor consumers. Use `--no-default-features` for
the grammar-free profiles and headless facade. Every grammar feature implies
`editor-services` and enables its optional syntax path.

LSP is optional for syntax and editor consumers. `kvim-syntax` enables no
grammar by default. It provides one feature for each language and one
`all-grammars` feature. `kvim-language` forwards these features without a
default grammar. The standalone `kvim` binary enables all 25 grammars.

The default Nix development shell supplies every first-release language server
and external formatter declared below. It is the runnable reference for the
exact external tools that a fully featured kvim checkout needs. The pinned Rust
toolchain supplies `rust-analyzer`; nixpkgs supplies the other commands. The
minimal `msrv` shell does not include these optional editor tools.

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

A grammar-independent service profile has a stable language identifier and a
version. It decides whether it supports a path. Both the headless diagnostics
registry and a grammar-backed adapter use this profile. Registry selection is
deterministic. No match means unsupported. Multiple matches are an ambiguous
typed failure.

Selection reads two lookup keys of the same path: the file extension and the
complete file name. Both keys are adapter data, so one selection path serves
both. The file name key serves a file whose extension names its tool instead of
its format, for example the JSON lock file `flake.lock`.

A third lookup key selects an adapter without a path: the name of a language. A
markdown fence names its language in an info string, and that name is no path.
The key is adapter data, as the two path keys are. The registry answers one name
with one adapter, or with nothing. A fence may name any language of the world,
so an unknown name is no failure. It selects nothing, and the fence stays plain.

The name match folds ASCII case, and the two path keys stay case-sensitive. A
path is a filesystem entity, where `s` and `S` name two different assembler
files. A language name is prose that a server writes, so `Rust` and `rust` name
one language. Every adapter declares its names in lower case, and the fold reads
ASCII alone, so one name always reaches at most one adapter.

An adapter declares the name of its language, and the aliases that an author or
a server writes for it. The identifier of the adapter is always one of its
names. Exactly one adapter owns each name, as exactly one adapter owns each
extension.

The registry reads one complete name. It normalizes no info string, because a
CommonMark info string may carry an attribute after the name. The reader of the
fence extracts the name and passes it alone.

A service profile supplies language selection and server data, not behavior.
It is the only source for path selectors, language names, and ordered language
server declarations. A syntax adapter delegates these values to its profile and
adds grammar-backed editor data. No syntax adapter copies a selector or server
table. One `kvim-syntax` catalog entry carries everything that selects and
parses the grammar, and the adapter carries the remaining editor data. The
adapter names its catalog entry, so no grammar lookup table exists twice.

The compiled highlight query belongs to `SyntaxHighlighter`, which owns a
bounded cache and releases it on drop. One adapter exists only while the Cargo
feature of its grammar does, so `kvim-language` registers the languages that the
build enables and no more:

| Item | Owner | Meaning |
|---|---|---|
| Identifier | Service profile | The stable name of the language and the identity used by headless results. |
| File extensions | Service profile | The case-sensitive extensions that the language owns. |
| File names | Service profile | The case-sensitive complete file names that the language owns, for a file whose extension does not name its format. |
| Language names | Service profile | The names that the language answers to, in lower case. The match folds ASCII case, and it needs no path. |
| Grammar | Catalog entry | The Tree-sitter grammar entry point, its highlight query, and its optional injection and local queries. |
| Version | Adapter | The stable name of the analysis implementation. |
| Comment tokens | Adapter | The line-comment token and the block-comment delimiters, each optional. |
| Indent rule | Adapter | The width of one indent level, the node kinds that hold their content one level deeper, the characters that close such a node, and the optional body field that a node does not indent. |
| Language servers | Service profile | The declared servers of the language, in declaration order. One declaration names its stable identifier, fallback diagnostic source, program, ordered arguments, protocol language identifier, formatting role, workspace root markers, initialization options, optional workspace settings, and diagnostics completion policy. |
| External formatter | Adapter | The program that formats a buffer of this language, and its arguments in command order. One argument is a literal text, or the place of the document path. |

The analysis, the highlight walk, the indent query, the comment toggle, and the
renderer read only these values. A new language therefore needs one new adapter
and one more entry in the registry table, and no change anywhere else.

The complete `all-grammars` registry contains one adapter for each of assembly,
Bash, C, C++, CSS, fish, GLSL, Go, HTML, JavaScript, JSON, Lua, Markdown, Nix,
Python, Rust, SCSS, SQL, Terraform, TOML, TSX, TypeScript, XML, YAML, and Zig.
Every match is case-sensitive. Twenty-two of the twenty-five adapters declare
one language
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

The language names follow the same rule. Every adapter names its language, and
it names the aliases that a fence carries beside that name: `assembly`
beside `asm`, `sh` and `shell` beside `bash`, `c++` and `cxx` beside `cpp`,
`golang` beside `go`, `js` and `jsx` beside `javascript`, `md` beside
`markdown`, `py` beside `python`, `rs` beside `rust`, `tf` beside `terraform`,
`ts` beside `typescript`, and `yml` beside `yaml`. `jsx` names the JavaScript
adapter, because one grammar reads the module syntax and the JSX syntax of it.
`tsx` names the TSX adapter, because the plain TypeScript grammar rejects that
syntax. No adapter claims `console`, `text`, or `hcl`. A terminal transcript is
no language of this registry, and a plain text is none either. A plain HCL file
carries another tool. Each of those three names therefore selects nothing, and
the fence stays plain.

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

## Headless Diagnostics Public API

A grammar-free host composes the two public handover contracts as follows:

```rust
let request = kvim_lsp::ServerLaunchRequest::new(program, arguments, root)?;
let transport = kvim_lsp::TransportFactory::process_with(request, launcher);

let project = kvim_language::HeadlessDiagnosticsProject::with_launchers(
    kvim_language::DiagnosticsRegistry::first_release(), root_path, settings,
    project_id, launcher_factory,
)?;
let selection = project.select(&relative_path)?;
let (opened, driver) = project.open(&manager, &relative_path)?;
```

`ServerLaunchRequest::new(OsString, Vec<OsString>, WorkspaceRoot)` validates and
owns the program, ordered arguments, and absolute workspace root.
`ServerLauncher::launch(&mut self, &ServerLaunchRequest)` returns
`LaunchedServer`, which transfers standard input, standard output, standard
error, and `ServerProcessHandle`. Its `wait` operation owns the single reap.
Its `terminate` operation requests forced termination without consuming that
wait. Every lifecycle implementation must request best-effort termination from
`Drop`. Kvim calls the launcher for the first attempt and each bounded restart.
The default `TransportFactory::process(request)` uses
`DefaultServerLauncher`. `TransportFactory::custom` transfers streams only and
leaves the remote lifecycle with the host. Kvim owns graceful `shutdown` and
`exit`, one absolute graceful deadline, forced termination, waiting, reaping,
and bounded standard-error reports. Start, wait, terminate, and stream failures
preserve their safe source errors. Cleanup reports do not replace an earlier
attempt failure. Diagnostics outcomes disclose no raw standard error. Nix
wrapping and executable policy stay with the host.

`DiagnosticsRegistry::first_release()` returns 25 grammar-independent service
profiles. `HeadlessDiagnosticsProject::new(...)` uses the default launcher.
`HeadlessDiagnosticsProject::with_launchers(...)` stores a factory without
calling it. Construction validates the absolute `WorkspaceRoot`, probes root
markers, realizes `LanguageSettings`, and creates no runtime or task.
`select(&WorktreeRelativePath)` returns stable language identity and active or
gated metadata in declaration order. `open(&ProjectManager,
&WorktreeRelativePath)` invokes factories only for active declarations of the
selected profile. It returns `OpenedHeadlessDiagnosticsProject` and
`ProjectDriver<DiagnosticsConversation>`. The host runs the driver, keeps the
hub warm for successive `ChangedFile` requests, then consumes the project
handle with `close().await`. A construction failure publishes no project. A
manager refusal can occur after launcher objects are created, but no process
starts until the host runs the driver.

`LanguageServerId` is stable Kvim metadata. `ServerId` is a neutral identity of
one opened project. Gated declarations have no neutral identity and reserve no
process slot. Only the three built-in eslint declarations currently use
`CompletionPolicy::Pull`. Every other declaration uses `Unsupported`. No
built-in declaration currently uses `VersionedPush`. An `Unsupported` server
therefore completes with an unsupported server outcome. It never completes by
guessing from a quiet period.

## Pre-1.0 Migration

Kvim is before version 1.0. Revision-pinned Git consumers receive source breaks
without a version increase. Pin a tested revision and apply these migrations.

### Process launch

Replace the removed field construction `TransportFactory::Process { program, args, root }`
with validated construction:

```rust
// New default process ownership
let request = ServerLaunchRequest::new(program, args, root)?;
let transport = TransportFactory::process(request);

// New injected process ownership
let request = ServerLaunchRequest::new(program, args, root)?;
let transport = TransportFactory::process_with(request, launcher);
```

An injected launcher must return all three streams and a lifecycle capability.
Kvim owns shutdown, termination, wait, and reap. Use
`TransportFactory::custom` only for stream transports whose remote lifecycle
stays caller-owned.

### Language declarations and features

Add `diagnostics_completion: CompletionPolicy` to every
`LanguageServerDeclaration` literal. Use `Unsupported` unless the server has a
verified exact-revision pull or versioned-push completion rule. The built-in
inventory is conservative: only eslint is `Pull` today.

`kvim-language` now defaults to `editor-services`. Use this default for the
existing editor-service surface. Use `default-features = false` for the
grammar-free `DiagnosticsRegistry` and `HeadlessDiagnosticsProject` surface.
Every `grammar-*` feature implies `editor-services`.

### Syntax selection

`kvim-syntax` no longer owns language aliases or path selectors. It removed
`language_of_path`, `LanguageCatalogEntry::language_names`, `extensions`,
`file_names`, `answers_to`, and `owns_path`. `LanguageCatalogEntry::new` now
takes only `(id, grammar)`. Its `language(id)` lookup is exact and
case-sensitive. `language("rust")` can match, but `language("Rust")` and aliases
cannot. Direct selector consumers now use
`kvim_language::DiagnosticsRegistry`:

```rust
let registry = kvim_language::DiagnosticsRegistry::first_release();
let profile = registry.profile(std::path::Path::new("src/lib.rs"))?;
assert_eq!(profile.id(), "rust");
```

## Syntax Highlighting

`SyntaxHighlighter` owns a bounded parser and query cache. Dropping it releases
that state. Its `highlight` operation accepts source text, a language hint,
explicit bounds, and cancellation. It returns zero-based byte ranges and
theme-independent `SyntaxRole` values.

The result distinguishes unsupported language, malformed syntax, cancellation,
and truncation. A malformed fragment can return useful spans and bounded syntax
errors. Every truncated result reports which bound stopped work.

Highlighting is synchronous bounded processor work. It creates no task and
reads no runtime or clock. The scheduler owns the deadline and cancellation
token. The highlighter checks cancellation during parser and query work. A
direct consumer submits requests to its own bounded worker spawner. An embedded
driver uses the caller-supplied worker spawner. No host event loop calls the
synchronous highlighter directly.

The standalone language adapter reparses buffer content incrementally after an
edit transaction, so a small change does not reparse the complete buffer.

Parsing and highlighting run only on a bounded worker service. They never run
on a host event loop.

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

One analysis request names the text that it reads. It names either one open
buffer or the preview text of one picker row. A buffer request carries the
buffer identity, generation, and version that produced its input. A preview
request carries the preview key of the selected row instead, because preview
text is loose text and no buffer. The publication gate rejects a result with an
obsolete value of either kind. An obsolete result never changes visible state
and never enters a cache. See [`text-model.md`](text-model.md) for the identity
rules and [`files.md`](files.md) for the preview.

The two kinds share one worker slot, one highlighter, and one publication path,
because one analysis runs at a time. A running preview holds that slot until it
answers. A submission cancels the job that held the slot, so a buffer job
started during a preview would cancel that preview, the preview would ask
again, and neither kind would ever finish. The picker covers the buffer while it
is open, so the preview comes first and the buffer job follows it.

A preview highlight is decoration. A failed preview analysis answers no span, so
the preview paints plain text and asks for no further job, and the outcome
reaches the editor log and never the message line. A preview of a file that no
language adapter serves needs no job at all.

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
returns a level count, not a column count. The adapter also declares the width
of one indent level for its language, and [`settings.md`](settings.md) keeps
the override and the fallback width.

The indent query must answer from the current buffer revision without blocking
the terminal event loop. When the parse result for that revision is not yet
available,
the editor uses the fallback rule in [`text-model.md`](text-model.md) instead of
waiting. A late result never rewrites a line that the user already typed.

The rule fits a language whose block carries an opening and a closing token,
because one node then spans the complete block. A C brace, a Bash `fi`, a fish
`end`, and a Lua `end` each close their node that way. The rule names the node
that spans the whole construct, and never the inner statement list, because two
entries would count one level twice.

Every scope names one indent span, which is the part of its node that takes the
level. The rule above describes the whole span: it reaches between the first
and the last byte of the node, and a closing delimiter ends it. The until-body
span reaches from the first byte of the node until the named body field
starts, so the scope excludes a body that indents itself. The Nix
`let_expression` node spans its own body, so its scope excludes that field.
Without the exclusion, the body would take the level of the `let` in addition
to its own level. The undelimited-body span reaches from the end of the
header, which is the sibling before the named field, through the last byte of
the node, and it holds both ends, because no delimiter follows the body. The
span opens a block for one of three reasons: the body holds no statement yet;
the body starts on a later row than the header; or the body reaches a later
row than the header and its first child carries no scope of its own. A YAML
block scalar (`a: |`) needs the third reason, because its value starts on the
header row but its content follows on a later row and carries no scope of its
own. A YAML flow collection (`a: [`) must fail the third reason, because its
value is a scope in the same rule and already supplies its own level. The
Python paragraph and the YAML paragraph below name the scopes that use this
span.

Python is the one registered language that closes a block with indentation
alone. Its `block` node starts at the first token of the suite and ends at the
last one, so no node spans the header line and the body together. The Python
adapter therefore names the compound statement that owns each suite, and that
scope carries the undelimited-body indent span: it indents from the end of the
header, which is the `:` token before the suite, through the end of the node.
The indent walk starts at the character before the position, so a position at
the end of a suite still reaches the node that owns it and gains its level. A
one-line suite, such as `if a: x = 1`, opens no indented block, so the scope
then holds no position.

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
a suite. The `block_mapping_pair` node names its `value` field, and the YAML
adapter gives that node the undelimited-body indent span, so the entry that
owns each nested collection reaches the last line of its collection and keeps
that collection's level. A `block_sequence_item` node names no field, so it
keeps the whole span, and the entry that holds the sequence supplies the level
of every item. A `flow_mapping` node and a `flow_sequence` node carry their own
brackets and their own scope, so both supply their own level, and the entry
that holds one adds no second level. A block scalar (`a: |`) carries no scope
of its own, so it takes its level from the entry that holds it.

SQL names its parenthesized constructs alone: the column list of a table, a
call with its arguments, a value list, a parenthesized predicate, and a nested
query. Each one carries its own opening and closing character, so each one is
exact. A select list carries no delimiter of its own, so it takes no level, and
the user indents a continuation of that list.

## Analysis Limits

Analysis enforces explicit limits on buffer bytes, buffer lines, syntax nodes,
traversal depth, parser work, captures, highlight spans, and syntax errors. The
public syntax facade reports truncation and can return its bounded useful
prefix. The standalone editor can choose plain text instead of publishing a
truncated buffer analysis. The owning syntax configuration validates every
limit against its fixed safety cap.

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Source bytes | `ANALYSIS_SOURCE_BYTES_MAX` | 4 MiB | The maximum file size of [`text-model.md`](text-model.md), so no larger text reaches the parser. A buffer that kvim loads must also hold the line bound of the row below, and two real files exceed that bound. |
| Source lines | `ANALYSIS_SOURCE_LINES_MAX` | 100000 lines | A source file of this length already exceeds normal practice. The check runs before the parse, so a generated one-line-per-byte file fails early. |
| Syntax nodes | `ANALYSIS_NODES_MAX` | 1000000 nodes | The densest measured source produces one node for each 5.6 bytes, so the byte limit produces about 750000 nodes. A larger tree means a pathological grammar result, not source that a reader edits. |
| Traversal depth | `ANALYSIS_DEPTH_MAX` | 128 levels | The indent query walks ancestors, and the highlight walk stacks captures. The bound measures syntax-tree depth, not source indentation, and a generated header reaches far more levels than a reader expects. |
| Highlight spans | `ANALYSIS_HIGHLIGHT_SPANS_MAX` | 750000 spans | The densest measured real source produces one span for each 5.8 bytes, so the byte limit produces about 727000 spans. One span holds 16 bytes, so the bound retains 12 MB for one buffer, and the syntax tree of that source costs more. |
| Analysis deadline | `ANALYSIS_DEADLINE` | 2 s | An incremental reparse and a highlight of a bounded file finish far below this value. Two seconds reports a runaway job, and highlighting is optional decoration, so a shorter deadline than the general worker deadline is safe. |

The deadline belongs to the request, and the bounded worker service enforces it.
See [`responsiveness.md`](responsiveness.md) for the worker bounds.

The bounds above were sized for five small grammars. A measurement over large
real files of the heavy grammars confirms two of them, and a second measurement
raised the highlight-span bound.

The node bound and the depth bound hold. The densest measured file is a 7.97
MiB C++ header with one node for each 5.6 bytes, so the source-byte limit
produces about 750000 nodes. That same header reaches 119 levels of tree depth,
and a 574 KiB TypeScript declaration file reaches 91 levels. Both stay below
128 levels, but the margin is small, and a deeper real file would lose its
indent answer for one line.

The line bound rejects two real files that the byte bound admits. `data.rs` of
`encoding_rs` at 2.5 MiB and `parser.c` of `tree-sitter-c` at 3.87 MiB each hold
more than 100000 lines. The check runs before the parse, so neither file reaches
the parser, and each one renders plain text. A generated table of one value for
each line is the shape that reaches this bound. The bound keeps its present
value, because a change of it is behavior and not reconciliation.

The first highlight-span bound of 100000 spans was too small. Three real files
above 1.6 MiB exceeded it and rendered plain text. The measurement below counts
the spans of large real files of nine grammars, and it sets the present bound.

| File | Adapter | Bytes | Highlight spans | Bytes for each span |
|---|---|---|---|---|
| `vulkan_funcs.hpp` | cpp | 2300965 | 155076 | 14.84 |
| `lib.dom.d.ts` | typescript | 1874901 | 116081 | 16.15 |
| `vulkan_raii.hpp` | cpp | 1773393 | 102991 | 17.22 |
| `vulkan_handles.hpp` | cpp | 1668256 | 96319 | 17.32 |
| `parser.c` of tree-sitter-go | c | 1572685 | 147659 | 10.65 |
| `tables.rs` of unicode-width | rust | 1484709 | 257191 | 5.77 |
| `typeEvaluator.ts` of pyright | typescript | 1244484 | 130405 | 9.54 |
| `vulkan.hpp` | cpp | 1103199 | 64379 | 17.14 |
| `pnpm-lock.yaml` | yaml | 993669 | 74977 | 13.25 |

Three results decide the value of 750000 spans.

- A generated Rust table is the densest real source. It produces one span for
  each 5.77 bytes, so a source of that density produces about 727000 spans at
  the 4 MiB byte limit. The bound therefore holds every measured real file at
  the largest size that kvim loads.
- One `HighlightSpan` holds 16 bytes, so the bound retains 12 MB for one
  buffer. The syntax tree of the same source costs more, and the session
  already retains that tree beside the spans.
- Every span comes from one captured node, and a line break splits one range
  into at most one more span for each line, so the node bound already caps the
  count. The densest source of any kind produces one span for each byte, and it
  reaches 980011 spans before the node bound rejects it.

The renderer reads the spans of the visible lines only. It finds the first span
of one line with a binary search, so a longer list costs no frame time.

Highlighting is optional decoration. Unsupported, malformed, cancelled, timed
out, or oversized analysis renders plain text. It never changes buffer content,
line mappings, or the cursor position. Every such outcome reaches the editor log
under the `JOB` source, so a user reads why one file carries no highlighting.

## Highlight Roles

Highlight roles are terminal-independent, so `kvim-syntax` owns the role set.
A role names what a range of source is, never how it looks. `kvim-tui` maps each
role to one style and keeps every color.

The language boundary therefore does not know the palette, and the theme does
not know Tree-sitter capture names. The role mapping reads capture names only,
and Tree-sitter highlight queries share one capture vocabulary across grammars,
so the mapping serves every language. See [`windows.md`](windows.md) for the
theme rule.

The public role set is non-exhaustive and keeps the current vocabulary. A
grammar whose query uses a name of the shared vocabulary that the mapping does
not yet cover extends the capture mapping before it extends the role set. The `text`
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

`kvim-lsp` is a general Language Server Protocol (LSP) client. One manager owns
several independent projects. The caller supplies each `ProjectId`, canonical
root, bounded server declaration list, language selectors, initialization data,
completion policies, manager limits, and deadlines.

Every handle, request, and event carries project identity. One project owns its
processes, document versions, diagnostics, cancellation, and shutdown. Two
projects can use one root, and projects on different roots remain independent.
Closing one project consumes its handle and cannot cancel another project.

The public project driver returns a future. The host starts and supervises it.
The library creates no runtime and detaches no task.

`ProjectManager::open` starts nothing. It reserves the budget of one project and
returns one `ProjectHandle` and one `ProjectDriver`. The manager refuses a second
project of one identity, and it refuses a project that passes the shared budget
for projects, processes, open documents, or queue capacity. A refused project
reserves nothing. The reservation returns to the manager when the handle drops,
so a closed project, a cancelled project, and a forgotten project all release it.

The handle reads the results of its own project and owns its cancellation.
`ProjectHandle::close` consumes the handle, cancels that project alone, and waits
`LSP_PROJECT_CLOSE_DEADLINE` for its driver to end. Dropping the handle instead
requests best-effort cancellation: it starts the shutdown and waits for nothing.
Every server of a dropped or cancelled project still ends through its own process
drop, so no untracked child survives.

`ServerSupervisor` owns the bounded restart loop of one server. It starts the
process, runs the handshake, hands the live streams to one `ServerConversation`,
runs `shutdown` and `exit`, ends the process, and starts at most
`LSP_RESTARTS_MAX` further attempts. It records `Started`, `Unavailable`,
`Failed`, `Restarted`, and `Stopped` as neutral `ProjectEvent` values, and the
process reporter records `Reported` without waiting. Every event carries project
identity and server identity, so a host translates one project's records into its
own outcome vocabulary and never mixes two projects.

Inside one project, kvim runs one persistent session for each selected server.
The session speaks JSON-RPC and knows no server product. Rust-analyzer is adapter
data, not a special case inside the client.

The service profile declares each server as data: the identifier, fallback
diagnostic source, program, ordered arguments, protocol language identifier,
formatting role, workspace root markers, initialization options, optional
workspace settings, and one explicit `CompletionPolicy`. The policy is `Pull`,
`VersionedPush`, or `Unsupported`. Each built-in declaration uses a verified
policy or conservatively uses `Unsupported`. Kvim never infers completion from
an executable name or negotiated prose. It never guesses versionless push
completion from a quiet period. The session sends what the declaration names.
Adding a language server therefore means adding one declaration to one service
profile. No code above the language boundary changes, and no assumption of one
server appears there.

One session identity contains project identity, server declaration identity,
and declaration order. Every request correlation key contains project, server,
and request identity. Two projects or servers can use the same request number
without collision.

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
- complete buffer-revision checks for every request and published result.

The session runs as one tracked background task. The host or editor driver sends
bounded requests through one queue and reads typed results from another queue.
No event loop reads, writes, or waits for a server. A full request queue returns
a typed saturated result at once, and the caller keeps its previous state.

That work splits over two crates at one seam. `kvim-lsp` owns the neutral half:
validated launch requests, injectable launch and lifecycle capabilities, the
child process, the bounded transport, the standard-error recorder, the
`initialize` handshake with its deadline and cancellation, the negotiated
capabilities, and the `shutdown` and `exit` sequence. A launch request owns a
validated program, ordered arguments, and `WorkspaceRoot`. A launcher receives
that request for the first attempt and for every restart. It returns standard
input, standard output, mandatory standard error, and one owning lifecycle
capability. Callers never receive a mutable Tokio `Command` and cannot override
Kvim's pipes, protocol, restart, deadline, standard-error, or completion rules.
Nix command composition remains host policy outside Kvim.

The lifecycle capability reports typed start, wait, termination, and input or
output failures with safe source errors. One `ServerProcess` owns the lifecycle
and all three streams. Graceful close sends `shutdown` and `exit`, then waits
within one absolute graceful-shutdown deadline. It does not restart that
budget for each step. If the server does not exit, Kvim performs bounded forced
termination and reaping. Dropping the lifecycle must initiate best-effort
termination. Async cleanup alone is not cancellation-safe.

If an attempt fails and cleanup also fails, the attempt failure remains the
primary failure. Kvim reports the cleanup failure separately through its typed,
nonblocking bounded event or report path. A missing executable is typed
unavailable and is not restarted. Other restartable failures use the existing
bounded restart policy. No classification reads human error text.

A launched process always has Kvim-owned lifecycle. A stream-only custom
transport remains available for an embedded server or socket. Its remote
lifecycle stays caller-owned, so Kvim does not claim that shutdown reaps a
remote process.

`kvim-lsp` also owns the bounded restart loop, because that loop names no editor
state. `kvim-language` owns the editor composition and the grammar-independent
headless composition over that seam. The editor path owns open documents,
buffer revisions, pending requests, diagnostic pulls, hover markup, and editor
outcome translation. The headless path owns service-profile selection, marker
gates, realized settings, and stable projection into neutral LSP declarations.
No editor type crosses into `kvim-lsp`.

The handshake offers the UTF-8 position encoding first and the UTF-16 position
encoding second. The Position Encoding section owns the negotiation and the
conversion. kvim also answers every unsolicited server request, so an
unimplemented request cannot stall the server.

Workspace containment rejects a path outside the workspace root with a typed
result. The session decodes a `file` URI and rejects another scheme, a malformed
escape, and a traversal component. A definition target outside the root is
rejected and never offered. kvim validates every server-supplied range against
the exact source bytes before it uses that range.

The session sends `didOpen`, `didChange`, and `didClose` for the buffers that
it queries. It sends `didChange` only after an edit transaction completes. The
Document Synchronization section owns what one `didChange` carries.

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
the editor fully usable with no diagnostics. Kvim reports the state once. A
missing executable is an unavailable start outcome and is never restarted.

A reload replaces the whole text of one buffer and advances its generation while
its edit version restarts at zero. kvim therefore synchronizes a reload as one
fresh document open that carries the reloaded text and complete buffer revision.
It drops every queued change of that buffer. No obsolete revision reaches the
server, and the server copy replaces the old copy in one step. See
[`files.md`](files.md).

A crashed server restarts a bounded number of times. The new server holds no
document, so kvim reports the restart and opens its buffers again. The session
does not retry a failed request. Cancellation owns child termination. Shutdown
follows the order in [`responsiveness.md`](responsiveness.md).

### The Markup Of One Hover Answer

kvim names `markdown` before `plaintext` in the hover capability of its
`initialize` request. The protocol reads that order as the preference of the
client, so a server that honors the order sends the markup that the float
renders.

The protocol writes the answer of a hover request in four shapes. The session
reads every shape and carries one text and one markup kind to the editor. The
kind names plain text or markdown, and nothing above the session guesses it. An
answer that names markdown carries its document as well, and the section below
owns that value.

| Shape | Kind | Reason |
|---|---|---|
| `MarkupContent`, which names `plaintext` or `markdown` | The named kind | The server declares the markup of its own text. |
| A bare string | Markdown | The protocol defines the deprecated `MarkedString` string as markdown. |
| An object with a `language` and a `value` | Markdown | The protocol defines that deprecated pair as one fenced markdown code block. |
| An array of the shapes above | One kind for the array | The array holds parts of one answer, and the float shows one text. |

The session joins the parts of an array on separate lines and answers the one
kind that covers the joined text. A part of plain text decides the whole
answer. Markdown that a reader shows unchanged keeps every character, and plain
text that a parser reads as markdown loses the characters that mark up a
document, so the join takes the kind that loses nothing. The current protocol
holds only `MarkedString` parts in an array, and every such part is markdown, so
a mixed array reaches kvim from a server that the protocol does not describe.

An object that names no kind and no language is no shape of the protocol, and an
unknown kind name names no shape either. Both take plain text for the same
reason.

The session writes the fence of a deprecated pair itself, because the protocol
defines that pair as one code block and the text alone is no code block. The
opening fence holds more backticks than the longest run of backticks inside the
value, so a value that holds a fence of its own never closes the block early.

The editor merges the answers of several servers after this reading. Each answer
keeps its own kind, because each server declares the markup of its own text.

### The Document Of One Markdown Answer

A server answer that names markdown carries the source of a document, not the
text of one. A reader that shows that source shows literal asterisks, literal
backticks, and an unrendered fence. `kvim-language` therefore reads one markdown
text into one document value, and `kvim-tui` paints it.

`kvim-language` owns the value for the reason that it owns the highlight roles.
A role is terminal-independent, and the palette lives in `kvim-tui` alone. The
parse also measures no terminal cell, because `unicode-width` runs in
`kvim-tui` only. The document therefore holds text, roles, and structure, and
the renderer holds every glyph, every width, and every color. The fence of a
code block names a language as well, and only a language adapter may select a
path by language, so the highlighter of a fence stays inside this crate. The
language name key of the registry answers that name.

The parse is pure. It reads no clock, no environment, and no file, so one text
always produces one document.

The session names the document of each markdown answer where that answer
arrives, beside the text that the server wrote. An answer of plain text carries
no document at all, because a markdown parse of a plain text removes the
characters that mark up a document. The float reads the document of an answer
only while every answer of that float names markdown.

#### The Blocks Of One Document

A document is a sequence of blocks. Each block names the containers around it
and the content inside it.

| Block body | Holds | Reason |
|---|---|---|
| Prose | One styled text | One paragraph or one list item wraps at the width that the renderer gives it. |
| Heading | One level and one styled text | The level names the rank of the heading. The renderer chooses the marker and the style of that rank. |
| Code | One info string, the source lines, and the highlight spans of those lines | A code line must not wrap, because a wrap moves the rest of the line under its own indentation. The info string names the language of the fence, and the spans name the syntax roles of its code. |
| Rule | Nothing | A thematic break separates two parts of an answer. The renderer draws it, because a glyph and a width are presentation. |

One styled text is one string and a sequence of runs that partition it. A run
names one byte range and one role, so a wrapped line is a slice of that string
and a role survives a wrap point on both sides of it.

Each block also carries the containers around it, outermost first. A quote
container rails every row of the block. A list container indents every row, and
it names the marker of the item when the block opens that item. The marker of an
ordered item counts up from the start value of its list, as CommonMark defines.
A source that jumps numbers therefore reads in order. The renderer builds the
prefix from the containers, because a marker and the blanks that replace it must
occupy one terminal width, and only the renderer measures a terminal cell.

A block reports whether one blank row stands above it. The blocks of one list
follow one another without a blank row, so a list reads as one list and not as
one paragraph for each item.

#### The Markup Roles

A markup role names what one stretch of text is, never how it looks. `kvim-tui`
maps each role to one style and keeps every color, exactly as it maps a
highlight role. See [`windows.md`](windows.md).

| Role | Meaning |
|---|---|
| Text | The body text of one block |
| Heading | The text of one heading |
| Emphasis | Text between one pair of emphasis markers |
| Strong | Text between one pair of strong markers |
| InlineCode | One code span inside a text |
| Link | The text of one link. The destination is markup, so it never paints. |
| Quote | The text inside one block quote |

A code block carries no role, because its body already names it as code. The
float paints the range of each highlight span of a code line in the syntax role
of that span, and every other part of that line in the code span role. A fence
that carries no span therefore paints in one color, exactly as every fence
painted before the highlight.

#### What The Parse Keeps As Text

The parse enables no extension of the CommonMark grammar. A table, a footnote,
and a task list marker therefore arrive as the text that the server wrote, and
no character disappears. A table also needs the width of every column, and only
the renderer measures a width, so a table has no place in this value.

An image has no place on a terminal screen, so its alternative text takes the
plain text role. kvim renders no markup, so an HTML block and an inline tag
arrive as their own text as well.

A fence that opens and never closes is already a code block, because CommonMark
closes it at the end of the text. A hover answer arrives complete, so the parse
needs no further rule for an incomplete document.

#### The Highlight Of One Fence

A fence names its language, so the code of a fence carries the syntax roles that
the same code carries in a buffer. One text therefore reads the same in a hover
answer and in an open file.

The document holds the spans of each fence beside its lines. One span addresses
the line by its index inside the fence and the range by its bytes inside that
line, exactly as one span of a buffer does, so the renderer paints a fence
through the mapping that already paints a buffer. `kvim-tui` keeps every color.

The highlight reads the language name of the info string and selects its catalog
entry. It uses the owned `SyntaxHighlighter` that also serves the language
composition instance, so a fence needs no second parser cache. The reader of the
fence extracts the name because no code above the adapter boundary may match a
language name.

A fence that names no language, a fence that names a language that no adapter
declares, and a fence that passes one bound all keep every line and carry no
span. None of these is a failure. A server may write any info string, and a
fence without a span reads as plain code, which is the state that every fence
had before this work.

A code line that is wider than the float loses its end, and that clip measures
terminal cells. The renderer therefore keeps the spans of the part that it
paints. It drops a span that starts behind the cut, and it shortens a span that
crosses it. A clip never splits a wide character, so every kept span still
addresses a character boundary of the painted text.

The highlight is Tree-sitter work, so the terminal event loop must never run it.
Two shapes serve that rule. The editor can highlight the answer where it
arrives, off the loop, and let the float paint a finished value. The editor can
also ask for the highlight as a bounded worker job, paint the fence plain until
the job answers, and repaint once.

kvim highlights the answer where it arrives. The measurement decides it: one
real rust-analyzer hover answer of two fences costs about 68 microseconds of
highlight work in a release build, and one fence at the source bound costs about
1.4 milliseconds. The answer already travels the language-server task, which is
off the loop, so that task absorbs the work and the reader sees one finished
float. A worker job would add one queue, one identity, one deadline, and one
repaint for work that costs less than a tenth of one frame.

The first fence of one language compiles the highlight query of its grammar,
which costs about 13 milliseconds. That cost falls once for each grammar of the
build, the analysis of a buffer shares the compiled result, and it also falls
off the loop.

The markdown parse of one hover answer therefore moves down beside the analysis,
into `kvim-language`. `kvim-tui` is the layer above, and it renders on the
terminal event loop, so a document that it parses itself could carry no span.
[`architecture.md`](architecture.md) owns the layer table, and the direction
stays one-way: the answer arrives in `kvim-language`, the document is complete
when it leaves, and `kvim-tui` paints it.

#### The Join Of Several Answers

Each answer of one hover carries its own document, because the highlight of a
fence runs where that answer arrives. The editor therefore joins documents and
never joins the texts of two markdown answers.

The join appends the blocks of each later document to the blocks of the first
one. One blank row stands above the first block of each later document, so a
reader sees where one answer ends and the next one starts. The join reports
itself as clipped as soon as one joined document reports itself as clipped.

The bounds of one document hold over the join as well. The join tests the two
counts before each block, exactly as the parse tests them before each event. It
therefore appends no block after the count reached `MARKUP_BLOCKS_MAX` or
`MARKUP_PIECES_MAX`, and it reports the join as clipped as soon as one of these
bounds stops it. The answers of several servers therefore cost the memory of one
document.

The join names no role and reads no grammar. It moves finished blocks, so the
terminal event loop may run it.

#### The Bounds Of One Document

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Markup source | `MARKUP_SOURCE_BYTES_MAX` | 16 KiB | The value of `LSP_HOVER_BYTES_MAX`, which bounds the largest markup that the editor reads today. One constant holds both, so the two cannot drift. A longer text stops at the last character boundary below the bound. |
| Blocks of one document | `MARKUP_BLOCKS_MAX` | 256 blocks | One block needs at least one character of its own, so a source of the bound above holds far more blocks than a float shows. The float shows at most `FLOAT_ROWS_MAX` rows, so 256 blocks exceed one float many times. |
| Pieces of one document | `MARKUP_PIECES_MAX` | 2,048 pieces | One piece is one stretch of text that the parse appends in one role, and one line of a code block counts as one piece. The parse tests the count before each event, so the bound stops it between two events. The lines of one code block join the count when that block closes, so they stop the parse after it and never inside it. `MARKUP_SOURCE_BYTES_MAX` bounds the lines of one such block. |
| Containers of one block | `MARKUP_NESTING_DEPTH_MAX` | 8 containers | A quote inside a quote inside a list indents the text of a block, and a server can nest without a limit. A container below this depth adds no further prefix, so the text still reaches the screen and the prefix cannot consume the whole width. |
| Source of one highlighted fence | `MARKUP_FENCE_SOURCE_BYTES_MAX` | 4 KiB | The float shows at most `FLOAT_ROWS_MAX` rows of `FLOAT_COLUMNS_MAX` cells, which is about 1536 bytes of source, so this bound holds every fence that a reader can see more than twice over. One fence of this size costs about 1.4 milliseconds of highlight work in a release build. |
| Spans of one fence | `MARKUP_FENCE_SPANS_MAX` | 2,048 spans | A dense measured fence of 4061 bytes produces 918 spans, which is one span for each 4.4 bytes. The bound holds one span for each two bytes of the source bound above, so no real fence reaches it. One span holds 16 bytes, so the bound retains 32 KiB for one fence. |
| Highlighted fences of one document | `MARKUP_FENCES_MAX` | 16 fences | `MARKUP_SOURCE_BYTES_MAX` already bounds the text of every fence together, and this bound holds the setup cost of one highlight, which the source bound does not hold. That cost measures about 1.4 microseconds for a fence that holds no line. A fence occupies at least one row of the float, and one blank row stands above it, so a float of 16 rows shows at most 8 fences. |

The parse stops at the first bound that it reaches, and the document then
reports that it is clipped. The rest of the source does not reach the value,
because the float shows at most `FLOAT_ROWS_MAX` rows and already reports that
it hides content.

The three fence bounds degrade one fence and never the document. A fence above
one of them keeps every line and carries no span, so it reads as plain code and
the document reports no clip for it. A fence that reaches the span bound carries
no span at all, because kvim publishes no partial result.

### The Document Synchronization

Each server chooses what one change notification carries. The session reads the
`textDocumentSync` capability of the `initialize` result and sends the mode that
the server asked for. The mode belongs to one server attempt, so a restart reads
the capability again.

The capability carries one number, or one object that names that number in its
`change` member.

| Capability | Mode | What one `didChange` carries |
|---|---|---|
| The number 1, or the object member `change` 1 | Full | The complete text of the document, and no range. |
| The number 2, or the object member `change` 2 | Incremental | One range and one replacement for each change. |
| The number 0, or the object member `change` 0 | None | Nothing. The session sends no `didChange`. |
| An object without a `change` member | None | Nothing. |
| No capability | None | Nothing. |
| Another number, or another type | None | Nothing. |

The protocol defines the mode `None` for a result that omits the capability, and
for an object that omits its `change` member. kvim follows both definitions,
because a server that reads a change notification declares the mode of that
notification. The protocol reserves no number above 2, so kvim sends nothing for
one. A wrong number must never send a change that the server reads as another
shape.

kvim sends `didOpen` and `didClose` in every mode, because each request of the
session names one open document. A server that asks for no synchronization
therefore holds the text of the open. The session keeps the recorded buffer
version on that text, so every later request of that document reports a stale
version. No answer of that server then describes text that the buffer does not
hold.

An incremental session derives the changes of one `didChange` from one applied
edit transaction, and it sends them in descending order, because the protocol
applies them one after the other.

A full session sends one change that carries the complete text and no range. It
builds that text from the mirror of the document, which holds the text that the
server still holds. It sends the text first, and it moves the mirror after the
notification reached the server. The Position Encoding section owns the mirror.

A probe of all 22 declared servers found four that ask for a full
synchronization: `bash-language-server`, `marksman`, `sqls`, and `taplo`. The
other eighteen ask for an incremental synchronization. Eleven declared servers
name the mode in the object form, and eleven name it as one number, so kvim must
read both forms. `marksman` names a full synchronization in the object form.

No declared server of this build omits the capability, so the `None` mode serves
no server today. kvim implements the mode because the protocol defines it, and
because a later release can declare a server that uses it.

One full change carries the text of the document, exactly as one `didOpen` does,
so `LSP_MESSAGE_BYTES_MAX` bounds both by the same rule. A full session spends
the cumulative `LSP_INPUT_BYTES_MAX` budget in proportion to the size of the
document, because each change carries that text again.

### The Refused Synchronization

The dispatch of one synchronization can fail before the request reaches a
server. The typed state of that failure decides whether a server copy drifted.

A full request queue answers `Saturated`. That session runs, it holds a copy of
each open document, and it drops the request. kvim therefore opens that document
again. The fresh open carries the complete text of the current buffer version,
and it supersedes every queued change of that buffer. The dispatch of that pass
stops, because the same session refuses every further request of the pass. The
next pass sends the open to a queue that drained.

Every other refusal names a state where no session holds a copy:

- No adapter serves the path.
- The adapter declares no language server.
- The workspace uses no declared server of the path.
- The declared executable is not installed.
- The server process did not start.
- Every session of the path stopped.

kvim opens no document again for those states, because no copy can drift. A
stopped server that restarts opens every document again on its own.

A refused close names no buffer, so kvim opens no document again. That server
keeps one document that the editor no longer holds at that path, and the next
open of that path replaces the copy.

The synchronization mode never refuses a change. A server that asks for no
synchronization accepts the change and sends nothing, so no copy of that server
drifts. The editor reads no mode, so a refused request of such a server still
opens the document again. That open carries the text that the server already
holds, so the repair changes nothing.

kvim reports the repair on the message line, and the log records that report.
The reader therefore knows that the editor repaired the copy, and not only that
one request failed. See [`windows.md`](windows.md).

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

Each recorded line reaches the host through a typed, nonblocking bounded report
sink. The report path can expose a bounded process report, but raw standard
error never enters `DiagnosticsOutcome`, `ChangedFileReport`, or
`ServerOutcome`. The `language` module holds no editor log, and it depends on no
module above it. The editor can record a report. See [`windows.md`](windows.md).

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

### Changed-File Diagnostics

The public changed-file operation reads no Git state and requests no full build.
The caller supplies one project, one validated worktree-relative path, exact
document text, one document revision, one language, result limits, cancellation,
and a wait policy.

The operation dispatches to every configured server that declares that language,
in declaration order. Each server has its own pull or versioned-push completion
policy and one result slot. One refusal, unsupported policy, failure, timeout, or
cancellation remains visible beside the outcomes of other servers.

A request reaches `Ready` only when every selected server reaches a terminal
ready, unavailable, unsupported, failed, or cancelled state. A ready result is
accepted only for the requested path, exact text, and document revision.

`WaitPolicy::Immediate` can return `Starting` and then ends that request. It
does not publish a later result.

`WaitPolicy::Until(Deadline)` owns the overall deadline. It keeps the exact
request alive through process startup and diagnostic completion. It returns
diagnostics, a terminal availability outcome, `Superseded`, or timeout for that
revision without polling or resubmission.

A newer request for the same document cannot receive an older result. The
configured policy either waits behind the active revision or explicitly
supersedes it. Kvim never guesses that versionless push diagnostics completed
after a quiet period. A server without a safe completion policy returns an
ordinary unsupported outcome.

Kvim applies each server's bounds before aggregate bounds. The result reports
both forms of truncation and every per-server outcome. It merges exact duplicate
diagnostics once. It sorts the remaining diagnostics by severity, path, range,
source, message, and server declaration order. Diagnostics above the aggregate
bound retain stricter severities first.

Bounds cover projects, processes, servers, open documents, exact text,
diagnostics, related information, ranges, message bytes, queues, protocol
traffic, and output. Every wait has one cancellation owner and an explicit
deadline. Typed errors keep protocol, process, invalid-response, timeout,
cancellation, and availability causes distinct. No classification inspects
error text.

One diagnostics hub owns the request side of one project. It creates one
conversation for each declared server, and the caller hands those conversations
to the project declaration. The project driver keeps every server warm, so a
later changed-file request reuses one running session. The host owns the async
runtime and runs the driver. The library creates no runtime and detaches no
task.

The hub holds one active request. A conversation reads that request as soon as
its server answers the handshake, so a request that a caller sent before the
process started still reaches that server. A server that is not installed never
serves an attempt, so the supervisor also shows every recorded step to its
conversation. The liveness of that server then answers the request with an
ordinary unavailable or cancelled outcome.

One request closes the document that the attempt holds and opens the requested
revision again. One open notification carries the complete text, so the sequence
serves every synchronization mode and it triggers a fresh analysis of a push
server.

The report keeps the related information that names the changed document. kvim
holds the exact text of that document only, so a range of another document has
no text to validate against and that entry leaves the report.

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

A full session mirrors the text in both encodings, because it builds the
complete text of every change from that mirror. A UTF-8 session of such a server
therefore holds the mirror and still converts no column.

A change that the mirror cannot apply proves that the session and the server
hold different text. The session drops that document, so no later answer of it
carries a converted column. The editor opens the document again on the next
resynchronization.

The mirror records the start of each line, so one conversion costs the length of
its line and never a walk over the document. One list of diagnostics therefore
stays linear in the text that it marks. Each open document stays below the
maximum file size of [`text-model.md`](text-model.md), and
`LSP_OPEN_DOCUMENTS_MAX` bounds the documents, so the mirrors of one session
stay bounded. The same two bounds hold for the mirror of a full session.

The conversion covers both directions.

| Direction | Values |
|---|---|
| Received | The range of a diagnostic, of a definition target, and of a formatting edit. |
| Sent | The range of every incremental `didChange` change, and the position of a definition or a hover request. |

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

One backward step of the jump list opens a document through this same path. The
two targets keep different position rules. A server position stays exact,
because the answer describes the document that the server read. A recorded
position of the jump list clamps into the loaded document, because the file can
shrink after the editor records it. See [`windows.md`](windows.md).

## Workspace Root Markers

One language server serves a workspace only when the workspace uses its tool. A
linter that needs a project configuration reports a failure for every buffer of
a workspace that holds no such configuration. That report is noise, because the
workspace never asked for the tool.

Each declaration therefore names its workspace root markers: the file names and
the directory names that prove that the workspace uses this server. A marker
matches a file of the workspace root. It also matches a directory of that root,
because a project proves a tool with both shapes.

The lookup reads one project's canonical root alone. It never walks to a parent
directory. Workspace containment rejects every path outside that root. A parent
directory is therefore outside the project, and it decides nothing.

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

The headless project probes all bounded distinct markers once during project
creation. The probe runs off the host event loop and asks for one path for each
distinct marker. Its cost follows service-profile data, not workspace size.
Each realized declaration keeps a typed visible gate outcome: matching file,
matching directory, absent marker, or no marker requirement. Every later gate
decision reads those outcomes alone. The root does not change while the project
runs, so one probe answers for every document of that project.

If the root probe cannot read a marker path, it records no match for that
marker, as the existing gate does. Every server gated only by unreadable or
absent markers then stays off. Its visible gate outcome names the unsatisfied
marker requirement. Every server without a marker still starts.

A gated server starts no child process, enters no session map, and reserves no
process capacity. It therefore does not count against `LSP_SESSIONS_MAX` or the
manager process budget. The realized metadata retains the declaration and its
gate outcome, even though the project declaration and diagnostics hub omit that
server.

A gated server is a normal state, not a process failure. The editor stays fully
usable, Kvim reports the state once, and no request starts that server again.
The state stays distinct from a server that is not installed. A gated server
was never meant to run in this workspace. A server that is not installed was
meant to run and could not. A gated formatting server keeps the format-on-save
state of its buffer, as a server that is not installed does.

### Headless Project Projection

A headless host selects one language with a validated `WorktreeRelativePath`
through the same service-profile selectors that editor adapters use. The typed
selection reports one profile, unsupported, or ambiguous. It preserves the
profile's stable language identity and every applicable server declaration in
source order. Several declarations for one language remain several servers.

Project realization validates the workspace root as `WorkspaceRoot`. For each
declaration it publishes stable language and server identity, fallback
diagnostic source, program, ordered arguments, protocol language identifier,
root markers, typed gate outcome, realized initialization options, realized
workspace settings, and declared `CompletionPolicy`. The neutral server IDs
used by `DiagnosticsHub` and the project declaration map back to the same
published identities. Source fallback and merge order therefore remain stable
across requests and restarts.

Realization does not start a runtime, detach a task, or execute a driver. The
host owns the runtime, runs the project driver, and can cache the owning project
value. One cached project keeps warm server sessions across changed-file
requests. Marker probing occurs only during realization, not for each request.

## Merging The Answers Of Several Servers

One buffer reaches every running server of its adapter. Each server answers on
its own, so the editor merges the answers before it changes visible state. The
rules below read the declaration order, never the arrival order, so one buffer
always shows the same result.

| Answer | Rule |
|---|---|
| Diagnostics | The editor keeps the newest set of each server and merges every set. Two diagnostics describe the same problem when their range and their message text are both identical, and the merge keeps the diagnostic of the earlier declaration. The merged list ascends by position. |
| Hover | The editor joins the non-empty answers in declaration order. One blank row separates two answers. Every answer of markdown joins as its document, and one answer of plain text makes the whole float plain text. |
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

The `lsp` module names each protocol and project bound as one constant, and the
`language` module names each editor bound as one constant. The constant and the
row below must always agree.

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Projects of one manager | `LSP_PROJECTS_MAX` | 8 projects | A host edits few worktrees at once, and every project owns child processes of its own. Eight exceeds normal practice and still bounds the processes, the queues, and the documents that one manager can reach. |
| Processes of one manager | `LSP_MANAGER_PROCESSES_MAX` | 64 processes | The server processes of every open project together. The value is smaller than eight projects of sixteen sessions, because no host runs every language of every project at once, and one project must not spend the budget of every other project. |
| Documents of one manager | `LSP_MANAGER_DOCUMENTS_MAX` | 256 documents | The open documents of every project together. The value is four times `LSP_OPEN_DOCUMENTS_MAX`, so four projects may each reserve their complete document budget and a fifth project must ask for less. |
| Queue capacity of one manager | `LSP_MANAGER_QUEUE_CAPACITY_MAX` | 2,048 results | The result queue slots of every project together. The value is `LSP_PROJECTS_MAX` times `LSP_EVENT_QUEUE_CAPACITY`, so every project may reserve the complete result queue and no further project can. |
| Registered language adapters | `LANGUAGE_ADAPTERS_MAX` | 64 adapters | Registry construction rejects a larger table before it performs pairwise identity and alias validation. |
| Servers of one adapter | `LANGUAGE_SERVERS_MAX` | 4 servers | One language splits its work over a type checker, a linter, and few other tools. Four declarations cover that practice and still bound the merge of one buffer. |
| Root markers of one server | `LANGUAGE_ROOT_MARKERS_MAX` | 16 markers | One linter names every file name that can hold its configuration. The reference `eslint` configuration names twelve of them, so sixteen covers that practice and still bounds the probe of one workspace. |
| Sessions of one project | `LSP_SESSIONS_MAX` | 16 sessions | One project mixes few languages, and a session starts only when a caller opens a document of its language. Sixteen exceeds normal practice and still bounds one project's child processes. |
| Frame header | `LSP_HEADER_BYTES_MAX` | 256 B | One `Content-Length` header and one optional `Content-Type` header fit far below this value, so a header that never ends stops early. |
| Frame body | `LSP_MESSAGE_BYTES_MAX` | 8 MiB | One `didOpen` carries a complete file, and one full `didChange` carries the same text. [`text-model.md`](text-model.md) bounds one file at 4 MiB, so 8 MiB keeps headroom for JSON escaping. |
| Session input | `LSP_INPUT_BYTES_MAX` | 512 MiB | The cumulative bytes that one session writes. A day of editing stays far below this value for an incremental server, and an unbounded write loop stops. A full server receives the text of the document in every change, so its session spends this budget in proportion to that size. |
| Session output | `LSP_OUTPUT_BYTES_MAX` | 512 MiB | The cumulative bytes that one session reads. The value matches the input budget, so neither direction can grow without limit. |
| Session requests | `LSP_REQUESTS_MAX` | 1,000,000 requests | One keystroke starts at most one request, so this budget covers a long session and still bounds a request loop. |
| Session messages | `LSP_MESSAGES_MAX` | 4,000,000 messages | A server sends progress and diagnostics without a request, so the message budget is larger than the request budget. |
| Open documents | `LSP_OPEN_DOCUMENTS_MAX` | 64 documents | The editor opens one document for each visible or recently used buffer. Sixty-four exceeds normal practice and still bounds the server memory. |
| Pending requests | `LSP_PENDING_REQUESTS_MAX` | 32 requests | A user produces few simultaneous questions. The bound stops a request storm from an automated caller. |
| Request queue | `LSP_REQUEST_QUEUE_CAPACITY` | 64 requests | The queue absorbs one burst of editor requests. A full queue returns a saturated result instead of waiting on the event loop. |
| Result queue | `LSP_EVENT_QUEUE_CAPACITY` | 256 results | The queue matches the runtime result queue of [`responsiveness.md`](responsiveness.md), so one slow frame does not stall a session. |
| Content changes | `LSP_CONTENT_CHANGES_MAX` | 4,096 changes | The transaction bound of [`text-model.md`](text-model.md). Every transaction that the buffer accepts can therefore synchronize. |
| Diagnostics | `LSP_DIAGNOSTICS_MAX` | 1,024 diagnostics | The bound counts the diagnostics that one server publishes for one document, and the items that one pulled report carries. One file with more than a thousand diagnostics is already unreadable. The renderer shows the diagnostics of the visible lines only. |
| Merged diagnostics | `LSP_MERGED_DIAGNOSTICS_MAX` | 4,096 diagnostics | The diagnostics of one merged changed-file report. The value is `LANGUAGE_SERVERS_MAX` times `LSP_DIAGNOSTICS_MAX`, so four servers may each contribute a full result and the merge still holds one bounded list. |
| Related information | `LSP_RELATED_INFORMATION_MAX` | 64 entries | The entries that one diagnostic keeps. One diagnostic names few other places of the same document, so a longer list means a wrong or hostile answer. |
| Diagnostic message | `LSP_DIAGNOSTIC_MESSAGE_BYTES_MAX` | 8 KiB | The bytes of one diagnostic message and of one related information message. One message names one problem, and the bound applies to every entry of the merge. |
| Changed document text | `LSP_DOCUMENT_BYTES_MAX` | 4 MiB | The exact text that one changed-file request supplies. The value matches the file bound of [`text-model.md`](text-model.md), so every buffer that the editor holds also reaches a language server. |
| Request traffic | `LSP_REQUEST_BYTES_MAX` | 16 MiB | The protocol bytes that one server spends on one changed-file request. The bound counts the parameters and the results of every message of that server, so a server that never completes cannot allocate without limit before its deadline passes. |
| Languages of one server | `LSP_SERVER_LANGUAGES_MAX` | 16 languages | The languages that one server declares, which select it for a changed-file request. One server serves one language family, and sixteen covers a server that reads several dialects. |
| Language identifier | `LSP_LANGUAGE_BYTES_MAX` | 64 B | The bytes of one language identifier of the protocol, such as `rust` or `typescriptreact`. |
| Diagnostic source name | `LSP_SERVER_SOURCE_BYTES_MAX` | 64 B | The declared name that every diagnostic of one server carries when the server sends no `source` field. A short name keeps the merged report readable. |
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
| Project close deadline | `LSP_PROJECT_CLOSE_DEADLINE` | 2 s | Every server of one project ends inside its own shutdown deadline, and the servers of one project end together, so two seconds covers the scheduling of a full project. A driver that never ends cannot hold the caller past this value. |
| Shutdown deadline | `LSP_SHUTDOWN_DEADLINE` | 250 ms | Editor exit must stay immediate. A server that does not answer `shutdown` in 250 ms is killed instead. |

A received list that passes its bound produces a typed failure. kvim publishes
no partial result. Nested lists of one answer share one element budget, so a
server cannot allocate without limit by splitting many elements over many short
lists.

Session bounds apply to one session. Manager and project configuration also
bound projects, aggregate processes, aggregate documents, and aggregate queue
capacity. The merged diagnostics of one buffer
therefore hold at most `LANGUAGE_SERVERS_MAX` times `LSP_DIAGNOSTICS_MAX`
entries, because only the servers of one adapter describe one buffer. The merge
removes the duplicates, so the visible list is normally far shorter.

A language-server session uses `TransportFactory::process(request)` with the
Tokio launcher by default. A host injects an owned process launcher with
`TransportFactory::process_with(request, launcher)`. A host that owns a remote
server or socket uses `TransportFactory::custom`. The custom factory returns
fresh `Transport` streams for the initial attempt and every restart. Kvim does
not own or reap a remote process behind those streams. These streams implement
Tokio `AsyncRead` and `AsyncWrite`; the protocol reader must own a partial frame
across cancellation. The existing envelope queue and protocol byte limits bound
data after transport.

A language-server session owns a long-lived child process through the supplied
process spawner. `LSP_SESSIONS_MAX` bounds those children per project, and the
manager's process limit bounds them in aggregate. `PROCESS_CONCURRENCY_LIMIT` of
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
cursor position. A diagnostic carries the complete buffer revision that produced
it. kvim discards a diagnostic from an obsolete generation or edit version.

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

A focused sidebar leaves every window unfocused, so no window reports a cursor
cell. The float then takes the bottom of the body band, because no visible
cursor can anchor it. See [`windows.md`](windows.md).

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

### The Rows Of One Markdown Answer

The float holds one plain text or one markup document. A hover answer becomes a
document only while every server answer of it names markdown, and the float then
joins the document of each answer. One answer of plain text therefore decides
the whole float, because a markdown parse of a plain text removes the characters
that mark up a document. A diagnostic message is plain text as well, so it
reaches the float unchanged.

The float parses nothing. `kvim-language` names every block, every role, and
every highlight span before the answer reaches this layer, so the terminal event
loop paints a finished value.

The float renders the document at the width that it has, because only the
renderer measures a terminal cell. One block produces one or more rows, and one
blank row stands above a block that reports one. The prose of a block wraps at
the width that the prefix of the block leaves.

Each row starts with the prefix of the containers of its block.

| Container | The first row of the block | Every later row |
|---|---|---|
| Quote | The rail `│` and one blank | The same rail, because the quote holds every row |
| List item | The marker of the item, right-aligned in the field of the document | Blanks of the field width |
| List continuation | Blanks of the field width | Blanks of the field width |

Every list container of one document occupies the same field, so a row that
continues an item stands under the text of that item. The widest marker of the
document decides that field, and `FLOAT_LIST_FIELD_CELLS_MAX` bounds it. A
marker that is wider than the field loses its end, because a list that keeps one
left edge reads better than one marker that keeps every digit. An unordered item
takes the marker `•`, and an ordered item takes its number and a full stop.

The body of a block decides the rest of the row.

| Body | Rows | Reason |
|---|---|---|
| Prose | The wrapped text, in the roles of its pieces | One paragraph wraps at the width that it has. |
| Heading | The text in the heading role, indented by one cell for each rank below the first | No marker of the source reaches the screen, so the style and the indentation carry the rank. |
| Code | One row for each source line. Each highlight span takes its own syntax role, and the rest of the line takes the code role. | A code line must not wrap, so a line that is wider than the float loses its end, and the spans of the part that survives stay aligned to it. |
| Rule | One row of `─` | The break is as wide as the widest other row of the float, so a short answer keeps a narrow float. |

A document that reports itself as clipped ends with the `...` note of the float,
exactly as a float that holds more rows than it shows. The reader therefore sees
one note for every part of an answer that the float hides.

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

A formatting request carries the indent of the language that it formats. One
session serves one language, so `LanguageServices` resolves that width from the
adapter once and the session holds no unresolved indent value. A server of a
two-column language therefore receives the tab size two. See
[`settings.md`](settings.md) for the resolution order.

kvim applies the accepted answer of either formatter as one edit transaction, so
one undo reverses a complete format. It rejects an answer whose buffer revision
is obsolete, including an equal edit version from an older generation. It never
applies a change that was computed against different content.

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
on. A save of such a buffer also starts no format request, because no formatter
can answer one.

The absent request and the remembered state are two different things. The save
sends no request, and it changes no state. The per-buffer state itself stays
unchanged, so a buffer keeps the state that the user chose if a later release
declares a formatter for its language.

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
