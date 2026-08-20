# Git

This document owns the Git dependency of kvim: what the editor reads, how it
reads it, and how the file tree shows the result.

## Scope

kvim reads the repository. It never writes it. The first release holds no
stage, no unstage, no revert, no discard, and no hunk operation. Every function
of `kvim-workspace/src/git.rs` is a read, and the one command that the editor
runs gives up every optional lock, so a status read changes no file inside
`.git`.

`git` is the first external command of the first release beside `rg`,
`rust-analyzer`, and the clipboard command. It comes from the host platform.
[`architecture.md`](architecture.md) owns the packaging rule for an external
command. A host without `git` keeps a fully usable editor.

## The Status Read

The editor never runs `git` on the terminal event loop. The file-tree sidebar
builds one request, the event loop hands it to the bounded process service, and
the typed snapshot returns through one transition. See
[`responsiveness.md`](responsiveness.md).

The command is:

```
git --no-optional-locks status --porcelain=v2 -z \
    --ignored=traditional --untracked-files=normal -- .
```

Each argument answers one requirement:

| Argument | Reason |
|---|---|
| `--no-optional-locks` | The read never refreshes the index cache, so it writes nothing. |
| `--porcelain=v2` | The format is stable for machines and names both halves of one change. |
| `-z` | Records are NUL separated, so a name with a space, a quote, or a line break stays one entry. |
| `--ignored=traditional` | Git names one ignored directory instead of every file below it. |
| `--untracked-files=normal` | Git collapses an untracked directory the same way. The ignored mode above collapses only while this mode does. |
| `-- .` | The pathspec keeps the report inside the workspace root, and the separator keeps a root that starts with a hyphen a path. |

Git reports every path against the top level of the repository, not against the
working directory. The request therefore locates that top level with a bounded
search for a `.git` entry above the workspace root, and it performs that search
inside the process service, never on the event loop. A workspace root below the
top level of a repository is a normal case, and its marks resolve correctly.

Git reports a directory outside a repository through its exit code. No branch of
kvim reads the message text of `git`, of any other command, or of any error.

## The Recorded State

`GitStatus` names the state of one entry:

| State | Meaning | Mark |
|---|---|---|
| `Staged` | The index holds a change that the last commit does not hold. | `■` |
| `Modified` | The working tree holds a change that the index does not hold. | `●` |
| `StagedAndModified` | The index and the working tree each hold a change. | `◆` |
| `Untracked` | The repository tracks no entry of this path. | `□` |
| `Ignored` | The Git ignore rules name the entry. | `☑` |
| `Conflicted` | The entry holds an unresolved merge conflict. | `▲` |

Git records the staged half and the worktree half of one change separately, and
both halves can hold a change at one time. The combined state is therefore one
variant, never two flags, so no value of the editor can report a change of a
half that Git did not report.

The variants rise by severity, so a comparison ranks two states the way a reader
ranks them. The marks are presentation data beside the icon table, so
`kvim-tui` owns the glyphs and the theme owns every color.
[`files.md`](files.md) owns the icon table and the row layout.

## The Roll-Up

A directory carries the state of the entries below it, so a collapsed directory
still reports a change. The roll-up is one pure function over the parsed
records:

- Every reported entry merges its state into each directory above it, up to the
  workspace root.
- A conflict wins over every other state.
- A staged half and a worktree half combine into `StagedAndModified`, so a
  directory that holds staged work and unstaged work reports both.
- An ignored entry never reaches the directories above it. An ordinary
  repository ignores its build directory, and that directory must not make the
  whole workspace read as ignored.

A collapsed directory record, which Git closes with a separator, covers the
directory itself and every entry below it. An entry inside an ignored or an
untracked directory therefore reports the state of that directory, even though
Git named no record for it.

## Ignored Entries

`--ignored=matching` lists every ignored file. One Rust workspace holds tens of
thousands of files under `target`, so that mode would cost a very large listing
for one directory that the reader already knows about.

kvim uses `--ignored=traditional` instead. Git then names the ignored
*directory* once, and kvim inherits that state down the subtree. The cost of one
ignored build directory is one record. An ignored file inside a directory that
is not ignored still receives its own record, and that set is small.

The limit of this choice: kvim learns nothing about an ignored file below an
ignored directory that Git did not name, and it does not need to, because the
inherited state already answers every row.

## Ignored Entries And The Generated Names

The file tree already dims a fixed list of names as generated content:
`.direnv`, `.git`, `__pycache__`, `node_modules`, and `target`. That list is
presentation data and explicitly not a Git rule. See [`files.md`](files.md).

The two rules **extend** each other and never disagree: an entry that Git
ignores takes the same row state as a generated name, so both dim in exactly one
way. The fixed list stays the answer for a workspace that is no repository, for
a host without `git`, and for the time before the first status read answers. An
ignored entry additionally carries the `☑` mark, which the fixed list never
adds, so the reader can still tell which rule spoke.

## Refresh

kvim reads the repository state after a save, after a workspace mutation, after
a workspace-watch burst, and on the tree refresh command. It uses no timer,
because the renderer draws only after a visible state change and runs no
unconditional frame loop.

One read runs at a time. The sidebar holds one queued request, so a newer
trigger replaces the request that it supersedes. The publication gate cancels
the read that a newer one replaces and rejects the result of an obsolete one.
The sidebar rejects a snapshot a second time from its visible state, by the
workspace root that the snapshot names.

## Failure

Every failure is a normal state. The tree keeps every row, every key, and the
marks of the last successful read.

| Failure | Behavior |
|---|---|
| The host holds no `git` command. | The editor names it once for each session and shows no mark. |
| The directory is inside no repository. | The editor shows no mark and reports nothing. |
| The read was cancelled, passed its deadline, or passed its output bound. | The editor keeps the marks of the last successful read and reports nothing. |
| The submission was refused. | The same as above. The next trigger asks again. |

## Bounds

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Captured output | `GIT_STATUS_OUTPUT_BYTES_MAX` | 1 MiB | The collapsed ignored listing keeps one status far below this value. |
| Records of one snapshot | `GIT_STATUS_ENTRIES_MAX` | 4096 | A larger set of changes leaves the remaining entries unmarked, and the marks are decoration. |
| Deadline of one read | `GIT_STATUS_DEADLINE` | 5 s | One status read of a large repository finishes far below this value. |
| Levels of one path walk | `GIT_PATH_DEPTH_MAX` | 64 | The search for the repository, the roll-up, and the inherited lookup each stop here, so no malformed path costs unbounded time. |

The parser is pure and defensive. It drops a record that names no known type, a
record with too few fields, a record whose path holds a root component or a
parent step, a record whose path leaves the workspace root, and the last record
when the output bound stopped inside it. A malformed record is never a panic.

## Tests

Two kinds of test cover this boundary, and each proves what the other cannot.

Most tests drive the pure parser with recorded `git status --porcelain=v2 -z`
bytes. They are deterministic, they need no external command, and they cover
every record type, every malformed shape, and every bound.

A small set runs the real command end to end through the bounded process
service. Only a real invocation proves that the flags are right: a recorded
output cannot show that `--ignored=traditional` still names one directory
instead of every file below it, or that `--porcelain=v2` still writes the format
that the parser reads. Three facts need one real read each: every state of one
repository, the top-level discovery below a repository, and a directory that is
no repository.

`TempRepository` in the `test-support` module of `kvim-workspace` builds each
such repository. The development shell and the build sandbox both provide `git`,
so these tests run everywhere the test suite runs.

A test must never pass or fail by the configuration of the host, which
[`architecture.md`](architecture.md) binds. Three rules hold:

- Every setup command states its own author, because the build sandbox names
  none, and neutralizes the system file and the global file through the
  environment of the child.
- Every repository sets its own empty `core.excludesFile`. The status read runs
  through the process service and inherits the settings of the editor, not those
  of the setup commands, so only a value inside the repository can keep a global
  ignore file of the developer out of the result. The local value wins over
  every other one.
- Every repository names its initial branch, so a Git release that changes its
  own default changes no result.
