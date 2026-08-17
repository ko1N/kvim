# Files, Buffers, And Workspace

## Ownership

The `workspace` module owns files, buffers, saving, the file tree, workspace
mutations, and pickers. The `core` module owns buffer text. See
[`text-model.md`](text-model.md).

All filesystem work and all external processes run off the terminal event loop
through bounded runtime services. See
[`responsiveness.md`](responsiveness.md).

## File Operations

One file operation is one request and one result. The event loop builds the
complete request, which holds the path, the settings, and the buffer content
that the operation needs. The bounded worker service runs the blocking steps.
The event loop then applies the typed result as one state transition.

The editor runs one file operation at a time. A second command reports that one
operation is already running. This rule keeps a result from reaching a buffer
state that a newer operation already replaced.

A cancelled, timed out, or refused operation changes no buffer. The editor
reports the typed failure and keeps every unsaved change.

## Buffer Identity

A buffer has a stable identity that does not change while the buffer is loaded.
Windows, registers, language sessions, diagnostics, and pickers refer to a
buffer by identity, never by path.

A buffer also tracks its current path. A workspace rename or move updates the
path of every affected loaded buffer as part of the same transition. The
identity stays unchanged.

Opening a path that a loaded buffer already owns reuses that buffer. It does not
create a second buffer for the same file. A buffer records the absolute path
with every symlink resolved, so two spellings of one file reach one buffer. A
path that matches no loaded buffer needs one file read, and the completed load
compares the resolved path again before it publishes a new buffer.

Opening a path that holds no file starts an empty buffer. The first save writes
a new file at that path.

A buffer records its dirty state, its loaded line ending, and the file metadata
that Kvim observed at load time or at the last successful save.

Unloading a buffer removes it from the buffer list. Every window that shows the
buffer must first move to another buffer. Kvim stages that replacement buffer
before the removal, and it creates one empty buffer when the list holds no other
buffer. Kvim refuses to unload a buffer that holds unsaved changes.

The buffer list holds at most 128 buffers. The editor always keeps one loaded
buffer, so a window always shows text.

## Saving

Kvim saves through a staged atomic replacement where the platform supports it.
The save procedure is:

1. Check the file for an external change.
2. Write the complete buffer content to a temporary file in the target
   directory.
3. Flush the temporary file.
4. Rename the temporary file over the target path.
5. Record the new file metadata with the buffer.
6. Clear the dirty state.

The rename replaces the file in one step, so a reader never observes a partial
file. Kvim preserves the existing file permissions and resolves a symlink to its
target before it replaces the file.

A save failure at any step leaves the buffer dirty and usable. The user keeps
every unsaved change and can retry the save. A failed save never discards buffer
content and never leaves the temporary file in place.

The temporary file stays in the directory of the target, so the rename never
crosses a filesystem boundary. Its name holds the target name, the process
identifier, and a counter, so two saves never use one temporary file.

The `atomic save` setting selects this procedure. A disabled setting writes the
target file directly. See [`settings.md`](settings.md).

`:q` refuses to close the last window while the buffer holds unsaved changes.
`:q!` discards them. `:wq` saves first and closes the window after the save
succeeds. A failed save keeps the window open.

## External Change Detection

Kvim records the file identity when it loads a file and after every successful
save. The file identity holds two values:

- the file size, in bytes,
- the modification time that the platform reports.

Kvim compares the recorded identity with the current identity before it
overwrites a file. Kvim reads no file content for this comparison, so the check
stays cheap for a large file.

These cases are conflicts:

- the current identity differs from the recorded identity,
- the path held no file at load time, and a file exists now.

A missing file is not a conflict. The file holds no content to lose, so the save
writes it again.

A conflict is a typed result, not an error message string. Kvim reports the
conflict and does not overwrite the file. The buffer stays dirty and usable. The
user then reloads the file with `:e` and applies the change again.

## Persistent Undo Files

Kvim writes a persistent undo file for each saved buffer, so undo history
survives a restart.

### Location

Undo files live in one directory of the user state:

```
${XDG_STATE_HOME:-$HOME/.local/state}/kvim/undo/
```

The name of one undo file is the 64-bit FNV-1a hash of the absolute target
path, in lowercase hexadecimal digits, with the `.kvu` extension. A hash keeps
every name inside the name limit of the filesystem, which a long encoded path
would pass. A hash collision cannot corrupt a buffer, because the invalidation
rule below rejects a record of another content.

Kvim writes no undo file when the platform reports neither `XDG_STATE_HOME` nor
`HOME`.

### Format

The undo file is a binary file with a fixed header:

| Bytes | Field |
|---|---|
| 0-7 | the magic value `KVIMUNDO` |
| 8-11 | the format version, an unsigned 32-bit value |
| 12-19 | the length of the saved content, in bytes |
| 20-27 | the FNV-1a 64-bit hash of the saved content |

The body holds one base text and a chain of changes above it. The base text is
the oldest state that the record keeps. Each change replaces one character
range with one text and records the cursor position before the change. Loading
replays the chain, so the restored history uses the same transactions as a live
editing session.

The current format version is 1. Every value is little-endian.

The record holds no redo history above the saved state. Kvim writes the file at
save time, and the saved state is then the newest state.

### Invalidation

Kvim ignores an undo file when any of these is true:

- the magic value is not `KVIMUNDO`,
- the format version is not the current version,
- the recorded content length differs from the loaded file,
- the recorded content hash differs from the loaded file,
- the body is truncated, malformed, or above the bounds below,
- the replay of the chain does not reproduce the loaded file content exactly.

The last rule is the final check. It protects the buffer against a damaged
record that passed the header check.

### Bounds

| Bound | Value | Rationale |
|---|---|---|
| Steps in one undo file | 64 | Each step costs one text comparison at save time, so the bound also bounds the save cost. |
| Replacement text in one undo file | 1 MiB | One undo file keeps the recent changes, not a complete edit history. |
| Undo file size | 8 MiB | The value holds one base text of the maximum file size and the bounded chain above it. |

The remaining undo steps stay in memory for the running session. See
[`text-model.md`](text-model.md) for the memory bounds of the history.

An unreadable, unsupported, or invalidated undo file is not an error. Kvim
starts the buffer with empty undo history and continues. A failed undo file
write is not an error either, because the saved file is already correct.

The `undo file` setting enables this behavior. See
[`settings.md`](settings.md).

## File Tree

The file tree is a fixed-width sidebar on the right side of the terminal. See
[`windows.md`](windows.md) for the sidebar rule.

The tree expands directories lazily. It reads a directory only when the user
expands it, when a reveal needs it, or when a refresh needs it. Directory reads
run off the event loop through the same request pattern as the file operations
above. The tree model itself performs no filesystem work. It reports the next
directory that needs a read, and the event loop applies the completed listing as
one transition.

The tree orders entries deterministically. A directory sorts before a file, and
two entries of one kind sort by name. A symbolic link takes the kind of its
target, so an expanded link to a directory shows that directory.

### Icons

The tree paints one icon before each name. The icon set follows the
`nvim-web-devicons` set of the reference configuration.

The icon table lives in the interface layer as presentation data. It keys on the
file extension and on a well-known file name, such as `Cargo.lock` or
`.gitignore`. A well-known name wins over the extension, and every other file
receives one default icon. A directory carries an open icon or a closed icon,
which follows its expansion state. [`architecture.md`](architecture.md) records
this one narrow exception to the language-adapter rule: an icon never selects a
parser, an indent rule, a comment token, or a language server.

The table covers Rust, Lua, TOML, YAML, JSON, Nix, lock files, Markdown, shell
scripts, Git files, and images. Every glyph occupies one terminal cell.

The theme colors each icon through an icon role, such as code, configuration,
document, script, version control, generated, media, or unknown. A call site
names the role, never a color. See [`windows.md`](windows.md).

An icon needs a patched font. The `file tree icons` setting turns the icons off
for a terminal without one. See [`settings.md`](settings.md). Every entry row
reserves the same width for its icon, and a hidden icon reserves none, so the
names stay aligned in both states.

### Visibility

The tree hides an entry whose name starts with a full stop, and the names
`.DS_Store` and `thumbs.db`. One command shows every entry again. A filter query
narrows the visible rows to the names that hold the query, compared in
lowercase. A directory stays visible while its own name matches or while it
holds one matching descendant.

Kvim reads no Git ignore rules for the tree in the first release.

### Sidebar Focus And Operations

The sidebar owns every key while it holds the focus.
[`input-actions.md`](input-actions.md) owns the key table.

`l` expands the selected directory, and `h` collapses an expanded directory. On
a file, and on a closed directory, `h` selects the directory that holds the
entry. `l` on a file opens that file in the editor window, as `Enter` does, so
one key always moves the reader deeper into the tree. An operation that
needs text, such as a rename, an add, or a filter, reads one line through the
prompt of the message line. The tree opens no second input mechanism.

The tree runs one workspace operation at a time, as the file operations do. The
event loop takes the next directory read after every completed operation, so the
reads of one reveal or one refresh reach the worker in order. A cancelled,
timed out, or refused operation changes no workspace state and no buffer.

Kvim applies one completed mutation as one transition. The transition updates
the path of every affected buffer, refreshes only the changed directories, and
reveals the new entry.

### Reveal And Refresh

Reveal expands every parent directory of one path and selects that entry. It
loads only the directories on that path.

Refresh reads one expanded directory again. The reconciliation keeps every
expanded directory and the selection while their entries still exist. It drops
the state of an entry that disappeared. A selection whose entry disappeared
moves to the closest visible parent.

Collapsing a directory drops its loaded entries, so the loaded state stays
inside the bound below and a later expansion reads current entries.

### Tree Bounds

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Entries of one directory | `TREE_DIRECTORY_ENTRIES_MAX` | 512 | One sidebar shows far fewer rows. The bound keeps one large directory from filling the loaded state. |
| Names of one directory read | `TREE_DIRECTORY_SCAN_MAX` | 4096 | The read stops here, so a very large directory costs bounded time and memory. |
| Entries of the complete tree | `TREE_ENTRIES_MAX` | 8192 | The value holds 16 full directories, which is more than one navigation session expands. |
| Depth below the root | `TREE_DEPTH_MAX` | 16 | A Rust repository nests far less. The bound also stops a symbolic link that points at one of its own parents. |
| Waiting directory reads | `TREE_PENDING_READS_MAX` | 64 | One reveal or refresh queues few reads. The bound keeps the queue from growing while the worker is busy. |
| Characters of one filter query | `TREE_FILTER_CHARS_MAX` | 64 | A filter query is a short name fragment. |

A directory above the entry bound shows its first entries in the order above and
reports the truncation as one visible row. The tree never shows a partial
directory without that report. An unreadable directory reports the same way.

## Workspace Mutations

A workspace mutation creates, deletes, renames, copies, or moves files and
directories. Kvim validates the complete mutation before it changes anything on
disk. Validation checks:

- that the source exists and is a supported kind,
- that the destination does not collide with an existing entry,
- that two sources of one mutation do not claim one destination name,
- that the destination stays inside the workspace and holds no parent-directory
  component,
- that a directory does not receive one of its own parents,
- which loaded buffers the mutation affects,
- whether an affected buffer is dirty.

Kvim builds one staged transition that describes the filesystem operation and
every affected buffer path. It applies the filesystem operation first. It then
applies the buffer path updates as one visible state change. A validation
failure or a filesystem failure leaves both the workspace and the buffers
unchanged.

A buffer of a moved or renamed entry follows that entry and keeps its identity.
A buffer of a removed entry stays loaded, so the user keeps the content. Kvim
refuses to remove an entry whose buffer holds unsaved changes.

### Staged Application

A copy or a move writes every entry under a temporary name beside its
destination first. The commit then renames the temporary names, which is one
cheap step inside one directory. A failure at any step undoes every staged step,
so a paste of several paths never leaves half a result. The temporary name holds
the target name, the process identifier, and a counter, as the save procedure
above does.

A removal renames every entry to a temporary name beside itself, which is the
visible removal, and then removes the temporary names. A failed rename restores
every renamed entry. A failed removal after the commit leaves one hidden
temporary entry, which the default visibility rule keeps out of the tree.

Kvim moves an entry with one rename. It performs no copy across a filesystem
boundary in the first release, and it reports the refusal of the platform
instead.

### Mutation Bounds

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Paths of one mutation | `MUTATION_PATHS_MAX` | 128 | One paste holds the entries of one directory selection. |
| Entries of one recursive copy | `COPY_ENTRIES_MAX` | 4096 | The bound stops a copy of a build directory or of a looping link. |
| Depth of one recursive copy | `COPY_DEPTH_MAX` | 32 | A copied source directory nests far less. |

After completion, Kvim refreshes only the affected workspace state. It does not
rebuild the complete tree.

## File-Operation Clipboard

The file-operation clipboard holds copied or cut workspace entries. It is
distinct from the text registers that `editor` owns and distinct from the system
clipboard. A file operation never reads a text register. A text paste never
reads the file-operation clipboard.

A cut entry stays in place until a paste completes, because the clipboard
records the intent only and the paste builds the move. A cancelled paste leaves
the source unchanged. Kvim clears the clipboard after a move paste completes, so
one cut never moves the same entry twice.

The clipboard holds at most `FILE_CLIPBOARD_PATHS_MAX` entries, which is the
value of `MUTATION_PATHS_MAX` above, so every held entry fits into one paste.

## Pickers

One bounded picker framework serves file search, ripgrep search, and buffer
search. A picker owns a prompt, a bounded candidate list, a fuzzy filter, a
stable selection, keyboard navigation, and an asynchronous preview. The three
pickers differ only in the source of their candidates and in what one accepted
row opens:

| Keys | Source | An accepted row |
|---|---|---|
| `Space ff` | The workspace walk | Opens the file |
| `Space f/` | The ripgrep search | Opens the file at the matched line |
| `Space o`, `Space fb` | The loaded buffer list | Shows the buffer |

The picker covers the complete terminal and keeps no padding on either axis. The
prompt sits at the top. The result list ascends from the prompt, so the best
match sits next to it. A result shows the filename first, then its directory. A
wide layout gives the preview 75 percent of the width, on the right. No region
carries a divider glyph: one blank row and one blank column separate them. See
[`windows.md`](windows.md).

A terminal that cannot hold a readable preview column and a readable result
column shows the results alone, over the complete width.

[`input-actions.md`](input-actions.md) owns the keys of the picker.

### Ranking

The fuzzy match is a subsequence match without case. Each matched character
scores by its position: a character that follows the previous match scores most,
a character at the start of one word scores next, and every other character
scores least. The characters between the first and the last match cost one point
each, so a dense match ranks above a spread match.

The match runs against the filename first, and a match there receives the weight
`FUZZY_NAME_WEIGHT`, because the row shows the filename first. Only a query that
the filename does not hold reaches the complete path.

The order of the result list is total, so two equal queries always produce one
order:

1. the higher score first,
2. then the shorter row,
3. then the earlier candidate of the source.

The source order is deterministic: the walk returns its directories in the tree
order, `rg` returns its matches in its own output order, and the buffer list
ascends by buffer identity. An empty query keeps that source order.

The search picker sends its query to `rg`, so its rows already answer the query.
It applies no second filter over the filenames.

The selection follows its candidate across one refiltering while the query still
keeps that candidate. A selection that the query drops returns to the best row.

### Git Ignore Rules

The file picker walks the workspace through the same bounded directory reader as
the file tree. It drops every file that the Git ignore rules name, and it always
drops the `.git` directory.

The walk reads the `.gitignore` file of every visited directory. It supports this
pattern subset: a comment line, an empty line, a negation with `!`, a
directory-only pattern with a trailing `/`, an anchored pattern with a leading
`/` or with an inner `/`, and the globs `?`, `*`, and `**`. The innermost ignore
file decides first, and the last matching pattern of one file wins.

The walk reads no global ignore file, no `.git/info/exclude`, and no Git
configuration, because it starts no Git process.

### Previews

The file picker and the search picker show the region around the selected line.
The preview of a loaded buffer needs no file read at all.

A newer query or a newer selection makes the older search and the older preview
obsolete. The publication gate rejects the obsolete result, and the picker
rejects it a second time from its visible state. See
[`responsiveness.md`](responsiveness.md).

A missing `rg` command is a normal state, not an error. Kvim reports it once and
stays fully usable without the search picker.

### Picker Bounds

Kvim stops at the first limit and reports the truncated state above the result
list.

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Candidates of one picker | `PICKER_CANDIDATES_MAX` | 4096 | One reader never inspects more rows, and the bound keeps one keystroke inside the latency budget. |
| Characters of one query | `PICKER_QUERY_CHARS_MAX` | 128 | A query is a short name fragment or one search pattern. |
| Characters of one matched line | `PICKER_MATCH_CHARS_MAX` | 160 | One result row shows the start of the matched line, not the complete line. |
| Files of one walk | `WALK_FILES_MAX` | 4096 | The value is `PICKER_CANDIDATES_MAX`, so the walk collects no file that the picker drops. |
| Directories of one walk | `WALK_DIRECTORIES_MAX` | 4096 | One repository holds far fewer directories, and the bound stops a looping symbolic link. |
| Depth of one walk | `WALK_DEPTH_MAX` | 16 | A Rust repository nests far less, and the bound stops a link that points at one of its own parents. |
| Bytes of one ignore file | `IGNORE_FILE_BYTES_MAX` | 64 KiB | An ignore file is a short list of names. |
| Patterns of one ignore file | `IGNORE_PATTERNS_MAX` | 512 | Each pattern costs one comparison for each visited entry. |
| Matches of one search | `RIPGREP_MATCHES_MAX` | 1024 | A reader refines the query instead of reading more rows. |
| Matches of one file | `RIPGREP_FILE_MATCHES_MAX` | 32 | One file must not fill the complete result list. |
| Columns of one matched line | `RIPGREP_COLUMNS_MAX` | 160 | The value is `PICKER_MATCH_CHARS_MAX`, so `rg` sends no line that the row drops. |
| Output of one search | `RIPGREP_OUTPUT_BYTES_MAX` | 1 MiB | The bounded match list needs far less, and the process service stops a flood early. |
| Bytes of one preview | `PREVIEW_BYTES_MAX` | 128 KiB | The preview shows one screen of text, so it reads the start of the file only. |
| Lines of one preview | `PREVIEW_LINES_MAX` | 200 | The value holds more rows than any terminal shows. |
| Characters of one preview line | `PREVIEW_LINE_CHARS_MAX` | 400 | The value holds more cells than any terminal row shows. |
| Lines above the matched line | `PREVIEW_CONTEXT_LINES` | 8 | The reader sees the lines that lead to the match. |
| Deadline of one walk | `PICKER_WALK_DEADLINE` | 5 s | A bounded walk of one repository finishes far below this value. |
| Deadline of one search | `RIPGREP_DEADLINE` | 5 s | A cold search of a large repository needs seconds, and the value reports a stuck command. |
| Deadline of one preview | `PICKER_PREVIEW_DEADLINE` | 2 s | One bounded file read finishes far below this value. |

The search runs through the process service of
[`responsiveness.md`](responsiveness.md), so `PROCESS_CONCURRENCY_LIMIT` bounds
the running commands. One picker holds one candidate slot and one preview slot,
so it runs at most one search and one preview at a time.

## Supported Files

Kvim loads regular UTF-8 files. It rejects a directory target, a device file, a
binary file, an unsupported encoding, and an oversized file with a typed result
and a clear message. See [`text-model.md`](text-model.md) for the size limit and
the encoding policy.
