# Files, Buffers, And Workspace

## Ownership

`kvim-workspace` owns generic files, buffers, saving, the file tree, workspace
mutations, pickers, Git capture, and review data. It owns no host worktree list,
workspace shell, focus policy, or command policy. `kvim-core` owns buffer text. See
[`text-model.md`](text-model.md).

All filesystem work and all external processes run off every host event loop
through bounded runtime services. See
[`responsiveness.md`](responsiveness.md).

## Worktree Paths

Every public file operation starts from one explicit `WorktreeRoot`. This value
is an owned, canonical, absolute directory identity. A
`WorktreeRelativePath` is non-empty, relative, and free of parent traversal.
Public APIs expose no unchecked path constructor.

A `cap-std` capability directory owns access below one root. Existing symbolic
links are usable only when their resolved target remains below that root. A new
target resolves through its nearest existing parent below the root. Tree and
picker traversal never crosses the root and never loops through links.

Public APIs never inspect or change the process current directory. LSP and Git
children receive the canonical root as their explicit working directory. Two
instances can use the same relative path under different roots without sharing
file identity or state. [`architecture.md`](architecture.md) records the
`cap-std` security purpose.

## File Operations

One file operation is one request and one result. The visible-state owner builds
the complete request with its validated root, relative path, policy, and buffer
content. The bounded worker service runs the blocking steps. The owner then
applies the typed result as one state transition.

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
create a second buffer for the same file. A buffer records its root identity and
canonical contained target, so two contained spellings of one file reach one
buffer. A path that matches no loaded buffer needs one file read, and the
completed load compares the resolved target again before publication.

Opening a path that holds no file starts an empty buffer. The first save writes
a new file at that path.

A buffer records its dirty state, its loaded line ending, the file end that the
file held, and the file metadata that kvim observed at load time or at the last
successful save. The save writes the recorded file end, so a file without a
final line ending never receives one. See [`text-model.md`](text-model.md).

Unloading a buffer removes it from the buffer list. Every window that shows the
buffer must first move to another buffer. kvim stages that replacement buffer
before the removal, and it creates one empty buffer when the list holds no other
buffer. kvim refuses to unload a buffer that holds unsaved changes.

The buffer list holds at most 128 buffers. The editor always keeps one loaded
buffer, so a window always shows text.

## Saving

kvim saves through a staged atomic replacement where the platform supports it.
The save procedure is:

1. Check the file for an external change.
2. Write the complete buffer content to a temporary file in the target
   directory.
3. Flush the temporary file.
4. Rename the temporary file over the target path.
5. Record the new file metadata with the buffer.
6. Clear the dirty state.

The rename replaces the file in one step, so a reader never observes a partial
file. kvim preserves the existing file permissions and resolves a symlink to its
target before it replaces the file.

A save failure at any step leaves the buffer dirty and usable. The user keeps
every unsaved change and can retry the save. A failed save never discards buffer
content and never leaves the temporary file in place.

An embedded driver reserves event capacity before it accepts a save. A
successful save publishes `FileWritten` through that reservation. If capacity
is unavailable, the save returns `Saturated` before it writes. See
[`embedding.md`](embedding.md).

The temporary file stays in the directory of the target, so the rename never
crosses a filesystem boundary. Its name holds the target name, the process
identifier, and a counter, so two saves never use one temporary file.

The `atomic save` setting selects this procedure. A disabled setting writes the
target file directly. See [`settings.md`](settings.md).

`:q` asks before it closes the last window while the buffer holds unsaved
changes. The answer `y` closes the window and discards them, and every other
answer keeps the buffer and the window. The question names the buffer, and the
answer closes the window only while that named buffer still holds the focus,
because an open that completes while the question waits makes another buffer
active. `:q!` discards the changes without a question. `:wq` saves first and
closes the window after the save succeeds. A failed save keeps the window open.

## External Change Detection

kvim records the file identity when it loads a file and after every successful
save. The file identity holds two values:

- the file size, in bytes,
- the modification time that the platform reports.

One comparison of the recorded identity with the current identity answers both
directions: the save reads it before it overwrites a file, and the reload reads
it before it replaces a buffer. The comparison has three typed results:

| Result | Recorded state | Current state |
|---|---|---|
| unchanged | a file, or no file | the same file, or still no file |
| changed | a file | another file, or the same file with another identity |
| changed | no file | a file |
| missing | a file | no file |

kvim reads no file content for this comparison, so the check stays cheap for a
large file.

A save refuses a changed file. The refusal is a typed conflict, not an error
message string. kvim reports the conflict and does not overwrite the file. The
buffer stays dirty and usable.

A save writes a missing file again, because that file holds no content to lose.

## Reload

kvim keeps every open buffer current with its file. The workspace watch above
publishes one coalesced burst for each window of change. Every burst starts one
bounded check of every loaded buffer against its file, because a content change
names no path and a removed entry names only its directory. The check is one
file operation on the bounded worker service, like an open and a save, so the
terminal event loop reads no file.

The check reads a file only when it must:

- a buffer without unsaved changes whose file changed reloads,
- a buffer with unsaved changes is compared and never read,
- a file that still holds the recorded identity is not read at all.

A buffer that no window shows reloads with every other buffer, so a background
buffer is current when the user reaches it.

**A background check never reloads a buffer that holds an unsaved change.** That
buffer is marked instead, and the editor reports the external change once. The
mark is one of two states, and the winbar shows `[!]` for both:

- *changed*: the file changed, and the buffer keeps its unsaved changes,
- *missing*: the file is gone from its path.

A deleted file and a renamed file reach the same missing state, because the path
of the buffer names no file in either case. The buffer keeps its text and stays
fully editable, because that text is then the only copy that kvim can write. A
save writes the file again. A save and a successful reload both clear the mark.

A reload takes the same path as an ordinary open: the same size limit, the same
UTF-8 rule, and the same restore of the persistent undo file. A file that grew
past the size limit therefore reloads no buffer. kvim reports that, marks the
buffer as changed, and keeps its text.

Every window that shows a reloaded buffer keeps its own cursor and its own first
visible row. Both clamp to the new text, so a file that became shorter leaves no
cursor and no viewport beyond its end.

A reload replaces the whole buffer, so the reloaded buffer restarts its undo
history and counts its versions from the start, as a fresh open does. The
persistent undo file is not part of the reload: kvim writes it at save time, and
its content check rejects a record that no longer describes the file. Everything
that one buffer version guards restarts with the buffer: the accepted analysis,
the reuse tree, the published diagnostics, and the matches of the active search.

A reload publishes only while the buffer still holds the version that the check
compared. A buffer that the user changed while the check ran rejects the
outcome, so no obsolete text reaches a buffer.

The language server receives the reloaded text as one document synchronization
that carries the reloaded buffer version, and kvim drops every queued change of
that buffer, so no obsolete version reaches the server. See
[`language-services.md`](language-services.md).

A background check reports nothing when it finds nothing that the editor cannot
follow, and a refused or failed background check reports nothing at all, because
the user never asked for it. The next burst checks again.

`:e` without a path is the manual form of the same operation for the buffer of
the focused window. It reads the file whatever its identity holds. `:e` asks
before it replaces a buffer that holds an unsaved change, because the file then
replaces the only copy of that work. The answer `y` reads the file and discards
the change, and every other answer keeps the buffer. The question names the
buffer, and the answer reads the file of that named buffer, never of the focused
window, because an open that completes while the question waits makes another
buffer active. `:e!` discards the change without a question. `:e <path>` keeps
its own meaning and opens another file.

## Persistent Undo Files

kvim writes a persistent undo file for each saved buffer, so undo history
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

kvim writes no undo file when the platform reports neither `XDG_STATE_HOME` nor
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

The record holds no redo history above the saved state. kvim writes the file at
save time, and the saved state is then the newest state.

### Invalidation

kvim ignores an undo file when any of these is true:

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

An unreadable, unsupported, or invalidated undo file is not an error. kvim
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

### Row Layout

Every row of the sidebar holds the same five parts, from the left edge:

1. One mark cell. The selected row paints its mark there, and every other row
   keeps the cell blank, so one mark never moves a name.
2. The indent guides. One level costs two cells. The first level draws no guide,
   because the header row above it is no sibling of the first entries.
3. Two glyph cells. They hold the icon of the entry, or the expansion marker of
   a directory while the tree hides its icons.
4. The name of the entry, and the suffix of the row state behind it.
5. One Git mark cell at the right edge. Every row reserves it, and a row without
   a recorded Git state keeps it blank, so one mark never moves a name and never
   covers one. [`git.md`](git.md) owns the marks and their states.

An indent guide is one box-drawing character of one terminal cell. A level that
holds a further entry below the row draws a trunk, and the last child of a level
closes it with an elbow. The theme colors the guides through their own role, so
they never read as one name.

The header row names the workspace root. It shortens the home directory of the
user to `~`, as the reference shell does, and it carries an open-directory
glyph in the same cells that an entry row uses. The sidebar reads the home
directory once, when it opens, so the render path performs no ambient read.

The selected row paints one band over the complete width of the sidebar, so the
reader finds it at any indent depth, and it marks the left edge of that band.
The sidebar leaves every row below the last entry empty. It shows no
end-of-buffer marker, because that marker belongs to a buffer window.
[`windows.md`](windows.md) owns the marker rule.

### Row States

One row takes exactly one state, which decides its color and its suffix:

| State | Appearance |
| --- | --- |
| Directory | The name takes the title color. |
| File | The name takes the normal text color. |
| Generated | The name dims, because the entry holds machine output or the Git ignore rules name it. |
| Held | The name dims and gains the suffix of the pending file operation. |
| Omitted | The row dims and turns italic. It counts entries, it names none. |
| Incomplete | The row warns and turns italic, because a read kept entries out. |

The generated names are a small fixed list beside the icon table: `.direnv`,
`.git`, `__pycache__`, `node_modules`, and `target`. The list is presentation
data, like the icon table, and it selects no parser and no language server. The
workspace watch below ignores exactly these names, so one list answers both
questions.

An entry that the Git ignore rules name takes the same state, so the two rules
extend each other and never disagree about one row. The fixed list stays the
answer for a workspace that is no repository and for a host without `git`. The
Git mark separates the two cases. [`git.md`](git.md) owns that decision.

The name of a changed file takes the color of its Git state. A directory keeps
the title color, because its state rolls up from the entries below it and names
no change of the directory itself.

A held entry carries the mode of the file-operation clipboard, never two
separate flags, so the row can never report a cut and a copy at one time. The
suffix is ` (cut)` for a move and ` (copied)` for a copy. The tree reads the one
file-operation clipboard for this state and keeps no second copy of it.

An omitted row and an incomplete row must not read alike. A count of hidden
entries reports a choice of the reader, so it stays quiet. A truncated or a
failed read keeps entries out that the reader expects, so it warns.

The tree search marks every matched name over the state of its row, so a match
inside a dimmed or held entry still reads as one match.

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
scripts, Git files, images, and the license file of a repository. Every glyph
occupies one terminal cell.

The theme colors each icon through an icon role, such as code, configuration,
document, script, version control, generated, media, or unknown. A call site
names the role, never a color. See [`windows.md`](windows.md).

The same table also holds one icon for each which-key command group: search,
code, window, buffer, file tree, and one default icon for every other command.
`input` names the group of a command, and this table names the glyph and the
role, so no interface value reaches the input layer. See
[`input-actions.md`](input-actions.md).

An icon needs a patched font. The `file tree icons` setting turns the icons off
for a terminal without one. See [`settings.md`](settings.md). One setting turns
every glyph of the tree and of the which-key overlay off together. The glyph
cells of the tree keep their width in both states, so the names stay aligned.
Without the icons a directory shows the expansion marker in those cells, so the
state of a directory stays visible without a patched font. Every which-key
column loses the same cells instead, so the columns of the overlay stay
aligned.

### Visibility

The tree hides an entry whose name starts with a full stop, and the names
`.DS_Store` and `thumbs.db`. One command shows every entry again.

Every directory counts the entries that this policy keeps out of its own rows,
and closes its entries with one row that names the count, for example
`(5 hidden items)`. The count reports the existing decision. It never changes
which entries the policy hides, and a directory that hides none carries no such
row. The row names no entry, so the selection never rests on it. The policy that
shows every entry needs no count, so the rows lose it again.

### Tree Search

The tree search behaves like the buffer search. It removes no row. It marks
every name that holds the query, compared without case, and one key pair moves
the selection between the marks in row order. The move wraps at the first and
the last match, as `n` and `N` wrap in a buffer window.

A match may sit below a directory that is closed, or inside a directory that
the tree never listed. The search therefore reads the directories of every
loaded listing through the bounded worker service, never on the event loop, and
it opens the directories above each match.

The tree records the owner of every open directory before it opens anything, so
the end of a search is one commit instead of a rollback: every directory that
the search opened closes, and every directory that the user opened stays open.
A directory that the user opens while the search runs therefore survives the
end of that search. `Esc` and `Ctrl-C` both end the search, and an empty query
ends it as well.

kvim reads no Git ignore rules for the tree in the first release. The dimmed
generated names above are a fixed presentation list, not a Git ignore rule.

### Sidebar Focus And Operations

The sidebar owns every key while it holds the focus.
[`input-actions.md`](input-actions.md) owns the key table.

`l` expands the selected directory, and `h` collapses an expanded directory. On
a file, and on a closed directory, `h` selects the directory that holds the
entry. `l` on a file opens that file in the editor window, as `Enter` does, so
one key always moves the reader deeper into the tree. An operation that
needs text, such as a rename, an add, or a search, reads one line through the
prompt of the message line. The tree opens no second input mechanism.

The tree runs one workspace operation at a time, as the file operations do. The
event loop takes the next directory read after every completed operation, so the
reads of one reveal or one refresh reach the worker in order. A cancelled,
timed out, or refused operation changes no workspace state and no buffer.

kvim applies one completed mutation as one transition. The transition updates
the path of every affected buffer, refreshes only the changed directories, and
reveals the new entry.

### Reveal And Refresh

Reveal expands every parent directory of one path and selects that entry. It
loads only the directories on that path.

A refresh also asks for the repository state again, as a save and a workspace
mutation do. [`git.md`](git.md) owns the refresh triggers.

Refresh reads one expanded directory again. The reconciliation keeps every
expanded directory and the selection while their entries still exist. It drops
the state of an entry that disappeared. A selection whose entry disappeared
moves to the closest visible parent.

Collapsing a directory drops its loaded entries, so the loaded state stays
inside the bound below and a later expansion reads current entries.

### Workspace Watch

A watcher observes the workspace root, so the tree follows a change that another
program made without a refresh command.

The `runtime` module owns the watcher, because it is the one portable boundary
for the `notify` dependency. See [`architecture.md`](architecture.md). The
watcher converts every platform event into one typed value, and no `notify` type
crosses that boundary. The typed kinds are `Created`, `Removed`, `Renamed`,
`Modified`, and `Unknown`, which a platform reports when it names no kind.

The watcher runs its platform callback and one coalescing task beside the event
loop. The loop reads one published burst as it reads a language event, and it
performs no filesystem work of its own.

The start of the watcher places no watch and reads no directory. The coalescing
task performs the registration, on a blocking thread, so the editor draws its
first frame before the first watch exists and a large workspace delays no frame.
[`responsiveness.md`](responsiveness.md) owns the start-up ordering.

No watch covers the window between the first frame and the completed
registration. The coalescing task therefore publishes one burst of `Dropped` as
it opens its stream. The sidebar then reads every expanded directory again, and
the editor checks every loaded buffer against its file, so a change inside the
window reaches the editor through those reads. The burst follows the
registration, so every change after it reaches a watch and no change falls
between the two.

One logical change writes many platform events, and one compiler run writes
thousands. The watcher therefore collects events for `WATCH_COALESCE_WINDOW` and
publishes one burst for that window. kvim coalesces itself instead of adding a
debouncer dependency, because the burst is a small pure accumulation over typed
values and it tests without a filesystem.

The burst names the directories whose listing may have changed. `Created`,
`Removed`, `Renamed`, and `Unknown` name the directory of the path. `Modified`
names no directory, because the entries of that directory stay the same. A burst
records a content change instead, and the sidebar asks for the repository state
again, as a save and a mutation already do. [`git.md`](git.md) owns the refresh
triggers.

The tree reads only the directories that the burst named. Every read takes the
ordinary refresh path, so the expansion, the selection, and the first visible row
all survive.

Every burst also starts one bounded check of the open buffers against their
files. The Reload section above owns that rule.

The watcher ignores the generated directory names of the row-state list above:
`.direnv`, `.git`, `__pycache__`, `node_modules`, and `target`. The file tree and
the watcher read one list, so a hidden row and an unwatched directory can never
name two different sets. A workspace whose own root carries such a name still
reports every change inside it, because the comparison starts below the root.

The list limits the registration first. The watcher walks the workspace once,
after the first frame, and adds one watch for each directory that stays. It
reads no directory below an ignored name. Every watch covers one directory
alone, so the platform adds no watch of its own inside an ignored subtree. One
recursive watch over the root would read that subtree instead. Linux then adds
one kernel watch for each directory below the root, so a repository with a large
build output directory costs tens of thousands of system calls. Such a
repository also reaches the watch limit of the host. The registration then
covers a part of the workspace, and the Watch Coverage section below owns that
report.

The registration adds every directory in one batch and applies the batch once.
A platform that keeps one event stream, as macOS does, rebuilds that stream for
each applied change. One measured tree of 1112 directories costs 105 ms as one
batch, and 71 s as one call for each directory.

The filter of the same list also runs inside the platform callback, before every
queue, so an ignored subtree costs no queue space and no later work. One list
answers the registration and the callback, so the two rules can never disagree
about one directory.

The walk is bounded. A directory that the process cannot read loses its subtree,
and a platform that also refuses the watch of that directory loses its entries.
Its parent still reports every change of the directory itself, so one refused
directory never stops the watch. A symbolic link adds no watch, because the
entry type names the link itself, so no link watches one tree twice and no link
builds a cycle. The directory bound, the depth bound, and the bound of one
directory read all keep the directories that the walk reached and report the
part that carries no watch. A directory above the bound of one read loses the
directories after that count, and it reports that loss as the same gap. That
bound counts every entry, so it also stops a read of many plain files. The walk
reads no entry type after the bound, so it reports a possible gap and not a
certain one. A silent loss costs the user more than one extra manual refresh.

A watch covers one directory alone, so a directory that appears after the walk
carries no watch of its own. The watcher closes that gap on the next burst. The
coalescing task owns the registration, so it adds every later watch beside the
event loop and the event loop performs no watch call.

The task reads every directory that the burst names, and it walks each
directory below them that the registration misses. It stops at a directory that
the registration already holds, because that directory reports its own new
entries through its own watch. One burst therefore costs one directory read for
each directory that it names, and one full walk of each new subtree.

The task adds the new watches in one batch, and it adds them before it publishes
the burst. The consumer then reads the named directory while the new watch
already runs, so no change escapes both the read and the watch. The task also
holds the set of watched directories, so a burst that names an unchanged
directory adds no path, opens no batch, and rebuilds no event stream.

The later registration obeys the same bounds as the walk at start. A batch adds
nothing above the directory bound, and a walk reaches nothing below the depth
bound. A tree above either bound keeps the watches that it already holds. The
set also records a directory that the platform refused, so one refused directory
costs one attempt instead of one attempt for each burst. A directory that
disappeared between the burst and the batch produces no entry, because its read
fails and its parent still reports its removal.

Every queue is bounded. A full queue, a burst above the directory bound, and a
failed platform read all drop events. A burst that lost events reports
`Dropped`, and the sidebar then reads every expanded directory again instead of
trusting an incomplete set. A drop therefore never leaves the tree stale, and no
queue ever grows without a limit.

A host that refuses the watch leaves the editor fully usable. The editor names
that state once for each session and the refresh command reads the workspace by
hand. That report shares one flag with the coverage report below, so one session
shows one watch report.

The registration runs after the first frame, so a host that refuses the root
refuses it there. The coalescing task then ends and closes its published stream.
The event loop reads that end as the report that no watcher observes the
workspace, and it names the state once, as a refused start already does.

### Watch Coverage

One registration can cover a part of the workspace. The platform refuses one
watch at the watch limit of the host. A bound of the walk stops at a very large
tree, at a very deep tree, and at a very large directory. Both causes can leave
a directory of the workspace without a watch, and both keep every other feature
of the editor.

Every burst carries that coverage. The value holds the number of refused
directories and the cause of that refusal. It also holds whether a bound of the
editor stopped the walk. A registration that covers the whole workspace names no
gap, so a complete watch reports nothing.

The burst that opens the stream carries the coverage of the first registration.
The report therefore reaches the editor with the first published value, before
the user changes one file. Every later batch carries its own coverage with the
burst of the same window, so a directory that appears after the start reports
its own gap as well.

A directory that another program removed between the walk and the watch call is
no gap. That directory holds no entry to watch, and its parent still reports the
removal.

The report names the cause, because the two causes need two different actions.
The user raises the watch limit of the host. The bounds of the editor hold no
setting of the host, so the refresh command reads the workspace by hand instead.

The `runtime` module names the setting that holds the limit, because it is the
one portable boundary of the watcher. Linux names that setting
`fs.inotify.max_user_watches`. A platform that publishes no such name reports
the refusal and its count alone.

The editor shows one watch report for each session. The missing watcher and the
partial registration share one flag, so the first report of a session wins. A
session that reported a partial registration shows no later missing-watcher note.
A session that reported the missing watcher shows no later coverage note. The
editor accepts that trade for a quiet message line, because the two reports name
the same loss and offer the same manual refresh.

### Watch Bounds

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Window of one burst | `WATCH_COALESCE_WINDOW` | 200 ms | One save writes a temporary file and renames it, so a shorter window would read one directory twice. A reader still sees a new file at once. |
| Waiting platform events | `WATCH_EVENT_QUEUE_MAX` | 1024 | One burst of an ordinary change writes far fewer events. The bound absorbs a burst without holding the platform callback. |
| Waiting bursts | `WATCH_BATCH_QUEUE_MAX` | 16 | The event loop reads one burst per window, so a queue of 16 covers a loop that is briefly busy. |
| Directories of one burst | `WATCH_BATCH_DIRECTORIES_MAX` | 64 | The value matches `TREE_PENDING_READS_MAX`, so one burst never names more reads than the tree queues. |
| Events of one burst | `WATCH_BURST_EVENTS_MAX` | 4096 | The bound ends one window even while a program writes without pause, so the consumer always receives its burst. |
| Directories of one registration | `WATCH_DIRECTORIES_MAX` | 4096 | Linux counts one inotify watch against `fs.inotify.max_user_watches`, which is a limit of the user and not of the process. Every program of that user shares one budget, and hosts set it to 8192 on an older system and 524288 on a current one. The bound keeps the editor a considerate user of that budget. A larger value takes watches from the other programs of the same user. A normal workspace stays far below the bound. After the walk skips the ignored names, this repository registers 31 directories of 2345. One repository of 6828 directories registers 43. The bound is therefore a backstop for a very large monorepo, not a routine limit. A workspace above the bound keeps the watches that the walk placed, and the burst reports the gap. |
| Depth of one registration | `WATCH_DEPTH_MAX` | 16 | The value matches `WALK_DEPTH_MAX` of the picker walk. A source tree below 16 levels holds no edited file. The bound also stops a generated path that nests far below any source tree. A deeper tree carries no watch below the bound, and the burst reports the gap. |
| Entries of one registration read | `WATCH_DIRECTORY_SCAN_MAX` | 4096 | The value matches `TREE_DIRECTORY_SCAN_MAX`, so one very large directory costs the registration bounded time. A directory above the bound loses the entries after that count, and the burst reports the gap. The bound counts every entry, so the report names a possible gap and not a certain one. |

### Tree Bounds

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Entries of one directory | `TREE_DIRECTORY_ENTRIES_MAX` | 512 | One sidebar shows far fewer rows. The bound keeps one large directory from filling the loaded state. |
| Names of one directory read | `TREE_DIRECTORY_SCAN_MAX` | 4096 | The read stops here, so a very large directory costs bounded time and memory. |
| Entries of the complete tree | `TREE_ENTRIES_MAX` | 8192 | The value holds 16 full directories, which is more than one navigation session expands. |
| Depth below the root | `TREE_DEPTH_MAX` | 16 | A Rust repository nests far less. The bound also stops a symbolic link that points at one of its own parents. |
| Waiting directory reads | `TREE_PENDING_READS_MAX` | 64 | One reveal or refresh queues few reads. The bound keeps the queue from growing while the worker is busy. |
| Characters of one search query | `TREE_SEARCH_CHARS_MAX` | 64 | A search query is a short name fragment. |
| Directories of one search read | `TREE_SEARCH_READS_MAX` | 64 | One search reveals the matches around the reader instead of walking a complete workspace. |
| Matches of one search | `TREE_SEARCH_MATCHES_MAX` | 256 | The reader refines the query instead of stepping through more marks. |

A directory above the entry bound shows its first entries in the order above and
reports the truncation as one visible row. The tree never shows a partial
directory without that report. An unreadable directory reports the same way.

## Workspace Mutations

A workspace mutation creates, deletes, renames, copies, or moves files and
directories. kvim validates the complete mutation before it changes anything on
disk. Validation checks:

- that the source exists and is a supported kind,
- that the destination holds no entry, or that the user approved the overwrite
  of exactly that entry,
- that the source and the destination name two entries,
- that two sources of one mutation do not claim one destination name,
- that the destination stays inside the workspace and holds no parent-directory
  component,
- that a directory does not receive one of its own parents,
- which loaded buffers the mutation affects,
- whether an affected buffer is dirty.

A mutation plan contains capability-relative operations only. Kvim revalidates
every source, parent, and destination immediately before commit. It never
replaces a destination that changed without the exact prior approval.

kvim builds one staged transition that describes the filesystem operation and
every affected buffer path. It applies the filesystem operation first. It then
applies the buffer path updates as one visible state change. A validation
failure or a filesystem failure leaves both the workspace and the buffers
unchanged.

An embedded driver reserves event capacity before it accepts a mutation. A
successful create, delete, rename, copy, or move publishes one bounded
`WorkspaceChanged` fact. Saturation refuses the mutation before it starts.

A buffer of a moved or renamed entry follows that entry and keeps its identity.
A buffer of a removed entry stays loaded, so the user keeps the content. kvim
refuses to remove an entry whose buffer holds unsaved changes.

A removal destroys data, so kvim asks the user before it removes an entry. The
question names the entry, or the number of entries. A cancelled question removes
nothing and changes no workspace state. kvim stages the removal when the answer
arrives, not when the question opens, so an entry that disappeared meanwhile
reaches no worker. [`input-actions.md`](input-actions.md) owns the keys of the
question.

### Overwrite

An overwrite destroys the entry that holds the destination, so kvim asks the
user before it replaces one. A rename or a transfer onto a free destination
asks nothing.

A taken destination refuses the mutation first, exactly as it did before this
capability. The refusal carries every taken destination and its kind back to the
editor, and the editor then asks one question. The question names the entry, or
the number of entries. The answer `y` replaces the named destinations. Every
other answer leaves every source and every destination unchanged.

The existence check runs on the bounded worker service with the rest of the
staging, so the terminal event loop reads no filesystem state. The question
therefore follows the result of the operation that the user asked for, not a
key alone. [`input-actions.md`](input-actions.md) owns that rule.

The answer carries the destinations that the question named, with the kind that
the first staging observed. kvim stages the operation again and replaces exactly
those destinations. The world can change while the question waits, so the second
staging decides again:

- a destination that became free needs no overwrite and takes the free path,
- a destination that changed its kind refuses the mutation, so no answer
  destroys a directory that took the place of a file,
- a destination that the question did not name refuses the mutation, as every
  taken destination does.

kvim refuses to overwrite a destination whose buffer holds unsaved changes, as
it refuses to remove such an entry. That refusal reaches the user before the
question, so the user never approves a mutation that the second staging rejects.
A buffer of an overwritten file without unsaved changes reloads through the
workspace watch above.

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

An overwrite adds one step to the commit. The commit renames the entry that
holds an approved destination to a temporary name beside itself, which parks it,
and it then gives that destination to the staged entry. A failure of any later
step puts every parked entry back under its own name, so a failed overwrite
leaves the destination unchanged. The commit removes the parked entries only
after every destination holds its new entry. An overwrite therefore never
destroys the destination before the replacement is complete.

kvim moves an entry with one rename. It performs no copy across a filesystem
boundary in the first release, and it reports the refusal of the platform
instead.

### Mutation Bounds

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Paths of one mutation | `MUTATION_PATHS_MAX` | 128 | One paste holds the entries of one directory selection. |
| Entries of one recursive copy | `COPY_ENTRIES_MAX` | 4096 | The bound stops a copy of a build directory or of a looping link. |
| Depth of one recursive copy | `COPY_DEPTH_MAX` | 32 | A copied source directory nests far less. |

After completion, kvim refreshes only the affected workspace state. It does not
rebuild the complete tree.

## File-Operation Clipboard

The file-operation clipboard holds copied or cut workspace entries. It is
distinct from the text registers that `editor` owns and distinct from the system
clipboard. A file operation never reads a text register. A text paste never
reads the file-operation clipboard.

A cut entry stays in place until a paste completes, because the clipboard
records the intent only and the paste builds the move. A cancelled paste leaves
the source unchanged. The tree marks the row of every held entry, so the reader
sees the pending operation.

kvim releases the held entries after one completed workspace mutation, so one
cut never moves the same entry twice and no row reports an operation that
already finished. `Esc` and `Ctrl-C` in the sidebar cancel the pending operation
together with the tree search, which releases the held entries as well.

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

The command-line completion of the path argument of `:e[dit]` ranks with the
same rule, so one ranking serves the picker and the command line. See
[The Walk Of The Command Line](#the-walk-of-the-command-line).

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

### The Walk Of The Command Line

The command-line completion of the path argument of `:e[dit]` reads the same
walk and the same ranking. The line asks for the walk when it first holds a path
argument. `:w`, `:q`, and a line number therefore start no walk. The parser owns
the rule that names the command that takes a path, so the parser and the
completion can never disagree about it.

One open command line asks for exactly one walk, and the completion filters the
result while the user types. Every later character of that line asks for no
second walk. A walk for each typed character would repeat the cost of the start,
so the completion never starts one.

The walk runs on the bounded worker service, and the event loop never waits for
it. The completion holds no file until the result arrives, so it offers no path
until then. A cancelled or timed out walk leaves it without a file, which is the
same usable state. See [`input-actions.md`](input-actions.md) and
[`responsiveness.md`](responsiveness.md).

One publication slot holds the walk of the command line. A newer command line
therefore cancels the walk of the line that it replaces, and the gate rejects
the obsolete result. A result that reaches a closed command line fills no list.

The walk starts at the workspace root and reaches no file above it, so no
candidate leaves that root.

### Previews

The file picker and the search picker show the region around the selected line.
The preview of a loaded buffer needs no file read at all.

A newer query or a newer selection makes the older search and the older preview
obsolete. The publication gate rejects the obsolete result, and the picker
rejects it a second time from its visible state. See
[`responsiveness.md`](responsiveness.md).

A missing `rg` command is a normal state, not an error. kvim reports it once and
stays fully usable without the search picker.

### Picker Bounds

kvim stops at the first limit and reports the truncated state above the result
list.

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Candidates of one picker | `PICKER_CANDIDATES_MAX` | 4096 | One reader never inspects more rows, and the bound keeps one keystroke inside the latency budget. |
| Characters of one query | `PICKER_QUERY_CHARS_MAX` | 128 | A query is a short name fragment or one search pattern. The path argument of the command line uses the same bound, because it reaches the same ranking. |
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
| Candidates of one completion | `COMPLETION_CANDIDATES_MAX` | 64 | One cycle reaches every candidate with a key, so a longer list holds rows that no reader reaches. The completion keeps the best ranked ones. |
| Deadline of one walk | `PICKER_WALK_DEADLINE` | 5 s | A bounded walk of one repository finishes far below this value. The walk of the command line uses the same value, because it is the same walk. |
| Deadline of one search | `RIPGREP_DEADLINE` | 5 s | A cold search of a large repository needs seconds, and the value reports a stuck command. |
| Deadline of one preview | `PICKER_PREVIEW_DEADLINE` | 2 s | One bounded file read finishes far below this value. |

The search runs through the process service of
[`responsiveness.md`](responsiveness.md), so `PROCESS_CONCURRENCY_LIMIT` bounds
the running commands. One picker holds one candidate slot and one preview slot,
so it runs at most one search and one preview at a time.

## Supported Files

kvim loads regular UTF-8 files. It rejects a directory target, a device file, a
binary file, an unsupported encoding, and an oversized file with a typed result
and a clear message. See [`text-model.md`](text-model.md) for the size limit and
the encoding policy.
