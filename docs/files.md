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

### Visibility

The tree hides an entry whose name starts with a full stop, and the names
`.DS_Store` and `thumbs.db`. One command shows every entry again. A filter query
narrows the visible rows to the names that hold the query, compared in
lowercase. A directory stays visible while its own name matches or while it
holds one matching descendant.

Kvim reads no Git ignore rules for the tree in the first release.

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
search. A picker owns a prompt, a filtered result list, a stable selection, and
an asynchronous preview.

The prompt sits at the top. The result list ascends from the prompt. A result
path shows the filename first, then its directory. A wide layout gives the
preview approximately 75 percent of the picker area.

File search and ripgrep search respect Git ignore rules by default. Ripgrep
search streams results from a bounded process.

A newer query or a newer selection makes the older search and the older preview
obsolete. The publication gate rejects the obsolete result. See
[`responsiveness.md`](responsiveness.md).

Pickers enforce explicit limits on:

- retained results,
- scanned or received bytes,
- concurrent processes,
- preview bytes and preview lines.

Kvim stops at the first limit and reports a typed truncated state. The concrete
limit values are not yet decided. Slice 11 must record them here before
implementation enforces them.

## Supported Files

Kvim loads regular UTF-8 files. It rejects a directory target, a device file, a
binary file, an unsupported encoding, and an oversized file with a typed result
and a clear message. See [`text-model.md`](text-model.md) for the size limit and
the encoding policy.
