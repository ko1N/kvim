# Files, Buffers, And Workspace

## Ownership

The `workspace` module owns files, buffers, saving, the file tree, workspace
mutations, and pickers. The `core` module owns buffer text. See
[`text-model.md`](text-model.md).

All filesystem work and all external processes run off the terminal event loop
through bounded runtime services. See
[`responsiveness.md`](responsiveness.md).

## Buffer Identity

A buffer has a stable identity that does not change while the buffer is loaded.
Windows, registers, language sessions, diagnostics, and pickers refer to a
buffer by identity, never by path.

A buffer also tracks its current path. A workspace rename or move updates the
path of every affected loaded buffer as part of the same transition. The
identity stays unchanged.

Opening a path that a loaded buffer already owns reuses that buffer. It does not
create a second buffer for the same file.

A buffer records its dirty state, its loaded line ending, and the file metadata
that Kvim observed at load time or at the last successful save.

Unloading a buffer removes it from the buffer list. Every window that shows the
buffer must first move to another buffer.

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

## External Change Detection

Kvim records file metadata when it loads a file and after every successful save.
Before it overwrites a file, Kvim compares the current metadata with the
recorded metadata.

A difference is a typed conflict, not an error message string. Kvim reports the
conflict and does not overwrite the file. The buffer stays dirty and usable. The
user then chooses to reload the file or to force the save.

## Persistent Undo Files

Kvim writes a persistent undo file for each saved buffer, so undo history
survives a restart. The persistent undo file format is not yet decided.

Slice 9 must define and record here:

- the location of the undo files,
- the version field in the file header,
- the invalidation rule that rejects an undo file whose buffer content no longer
  matches.

An unreadable, unsupported, or invalidated undo file is not an error. Kvim
starts the buffer with empty undo history and continues.

The `undo file` setting enables this behavior. See
[`settings.md`](settings.md).

## File Tree

The file tree is a fixed-width sidebar on the right side of the terminal. See
[`windows.md`](windows.md) for the sidebar rule.

The tree expands directories lazily. It reads a directory only when the user
expands it or when a refresh needs it. Directory reads run off the event loop.

The tree orders entries deterministically. It supports navigation, filtering,
refresh, and reveal of the active file. Reveal expands every parent directory of
the active file and selects the file entry.

## Workspace Mutations

A workspace mutation renames, copies, cuts, pastes, or moves files and
directories. Kvim validates the complete mutation before it changes anything on
disk. Validation checks:

- that the source exists and is a supported kind,
- that the destination does not collide with an existing entry,
- that the destination stays inside the workspace,
- which loaded buffers the mutation affects,
- whether an affected buffer is dirty.

Kvim builds one staged transition that describes the filesystem operation and
every affected buffer path. It applies the filesystem operation first. It then
applies the buffer path updates as one visible state change. A validation
failure or a filesystem failure leaves both the workspace and the buffers
unchanged.

After completion, Kvim refreshes only the affected workspace state. It does not
rebuild the complete tree.

## File-Operation Clipboard

The file-operation clipboard holds copied or cut workspace entries. It is
distinct from the text registers that `editor` owns and distinct from the system
clipboard. A file operation never reads a text register. A text paste never
reads the file-operation clipboard.

A cut entry stays in place until a paste completes. A cancelled paste leaves the
source unchanged.

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
