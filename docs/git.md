# Git

This document owns the Git dependency of kvim: status, immutable worktree diff
capture, review anchors, and the process policy for every Git read.

## Scope

Kvim reads the repository. It never writes it. It has no stage, unstage, revert,
discard, or comment-persistence operation. Every Git command gives up optional
locks, so a read changes no file inside `.git`.

`git` is the first external command of the first release beside `rg`,
`rust-analyzer`, and the clipboard command. It comes from the host platform.
[`architecture.md`](architecture.md) owns the packaging rule for an external
command. A host without `git` keeps a fully usable editor.

## Execution Policy

One `GitExecutionPolicy` builds every Git command without a shell. It sets the
canonical `WorktreeRoot` as the explicit working directory. It never reads or
changes the process current directory.

The policy disables optional locks, external diff, text conversion, pagers,
prompts, filesystem monitors, and hooks. It also drops every inherited helper
variable that names a program or redirects the read to another repository,
another index, or another configuration file. Command-line configuration
outranks the repository and the host, so neither can start another program
during a Git read.

Diff capture uses plumbing commands or explicit `--no-ext-diff --no-textconv`
reads for exact bytes. Kvim classifies failures through typed outcomes, exit
status, or stable Git codes. It never inspects error text.

## The Status Read

The editor never runs `git` on the terminal event loop. The file-tree sidebar
builds one request, the event loop hands it to the bounded process service, and
the typed snapshot returns through one transition. See
[`responsiveness.md`](responsiveness.md).

One read runs two commands. The `-z` record format always names a path against
the top level of the repository, and that top level can sit above the workspace
root. The first command therefore asks Git where the root sits inside its
repository, and the second collects the records:

```
git rev-parse --show-prefix
git status --porcelain=v2 -z \
    --ignored=traditional --untracked-files=normal -- .
```

Both commands carry the execution policy above.

Each argument of the status command answers one requirement:

| Argument | Reason |
|---|---|
| `--porcelain=v2` | The format is stable for machines and names both halves of one change. |
| `-z` | Records are NUL separated, so a name with a space, a quote, or a line break stays one entry. |
| `--ignored=traditional` | Git names one ignored directory instead of every file below it. |
| `--untracked-files=normal` | Git collapses an untracked directory the same way. The ignored mode above collapses only while this mode does. |
| `-- .` | The pathspec keeps the report inside the workspace root, and the separator keeps a root that starts with a hyphen a path. |

Git runs with the canonical worktree root as its explicit working directory.
Kvim inspects no directory above that root: the reported prefix answers the one
question that the record paths need. The publication subtracts that prefix,
drops every record that does not start with it, and converts the remainder to a
validated `WorktreeRelativePath`. The snapshot therefore names only contained
entries.

Git reports a directory outside a repository through its exit code. No branch of
kvim reads the message text of `git`, of any other command, or of any error.

## Worktree Diff Capture

### The Compared Pair

The caller supplies one `DiffComparison`, which names both states that the
capture compares. Kvim discovers no review base and no pair.

| Comparison | Compares | Section |
|---|---|---|
| `CommitToWorktree` | one commit against the working tree | every change, staged or not |
| `CommitToIndex` | one commit against the index | the staged half |
| `IndexToWorktree` | the index against the working tree | the unstaged half |
| `CommitToCommit` | one commit against another commit | one finished range of work |

Each case maps to one argument set. The staged half adds `--cached`. The
unstaged half names no revision, because a bare diff already compares the index
with the working tree. A commit pair names both commits in order.

An untracked file exists in the working tree alone, so a comparison that ends
elsewhere lists none and skips that read.

A commit pair is reproducible from the repository alone, because neither side
can change. A host that records a range of work therefore records two object
identifiers and reads the same candidate again at any later time.

The caller supplies one full `BaseRevision` Git commit object identifier for
every commit that a comparison names. An unavailable object or an object that is
not a commit returns `BaseUnavailable`. `IndexToWorktree` names no commit and
proves none.

`BaseRevision` accepts the two published object formats: 40 hexadecimal
characters for SHA-1 and 64 for SHA-256. It accepts either letter case and keeps
the lowercase form that Git writes. It accepts no abbreviation, because an
abbreviated identifier can name more than one object, and a review base must
stay one object for the life of the review.

`DiffTarget` selects the complete worktree or one validated
`WorktreeRelativePath`. One-path selection matches either side of a rename and
returns the complete rename pair.

A `CommitToWorktree` capture includes commits after the base, staged changes,
unstaged changes, and untracked content. A clean worktree with commits after the
base remains reviewable.

### The Old Side

One candidate records the state that it compared against as a `DiffOldSide`. A
commit names itself. The index is no commit, so the unstaged half records the
index digest that the capture read.

A `ReviewAnchor` carries the same value. An anchor of the unstaged half
therefore states that its old lines came from the index, instead of naming a
commit that never held them. The identity stays true whenever the index and
`HEAD` disagree, which is whenever work is staged.

The candidate records exact source bytes, file kinds, modes, old and new sides,
line mappings, truncation, and one `DiffRevision`. Added, deleted, modified,
renamed, binary, symbolic-link, submodule, and unsupported sides remain distinct.
Truncated data stays visibly truncated. Omitted content cannot receive a review
comment.

`DiffRevision` is a BLAKE3 digest of:

- the old side, with one tag for a commit and one for the index,
- current `HEAD`,
- index authority,
- sorted paths,
- file kinds and modes,
- exact published side bytes.

[`architecture.md`](architecture.md) records the BLAKE3 dependency.

## Capture Consistency

### Standalone Review Surface Capture

`ReviewSurface::for_worktree` uses this bounded capture boundary and publishes
no partial staged or unstaged pair. `ReviewSurface::from_candidates` bypasses
Git and accepts immutable candidates only. Both remain neutral: comments
contain bounded anchors and bodies, and the host owns persistence. No Git
operation gains host session, task, agent, or thread meaning.

Capture reads one authority fingerprint before and after collection. The
fingerprint covers the base commit, current `HEAD`, the index, status records,
and each selected worktree file identity and content digest.

The collected candidate derives the same authority projection from its paths,
modes, and side-byte digests. The initial fingerprint, candidate projection, and
final fingerprint must match before publication. This three-way comparison also
rejects an A-to-B-to-A change during capture.

A changed fingerprint retries within an explicit attempt bound. Exhaustion
returns `ChangedDuringCapture`. Kvim never publishes a mixed candidate. Capture
also has explicit source-byte, process-output, file, hunk, line, deadline, and
cancellation bounds. Every limit returns a typed outcome and reports
truncation where partial display is safe.

## The Capture Read

One capture runs the same command sequence three times. The first pass reads the
authority, the second pass collects the candidate that the capture may publish,
and the third pass reads the authority again. Every pass builds one complete
candidate and derives one projection from it, so the three values compare
directly and the content digest of one selected worktree file is the digest of
the exact side bytes that the candidate publishes. The collection therefore sits
between two independent reads, which is the only arrangement that rejects a
change that returns to its first value.

The final authority of a refused attempt becomes the initial authority of the
next one, because it names the state that the retry starts from.

One pass runs six commands, all under the execution policy above and all with a
fixed locale, so every marker of the patch format stays the same on every host:

| Command | Answer |
|---|---|
| `rev-parse --verify --quiet HEAD` | The commit of `HEAD`. Git separates an unborn branch from every other refusal by its exit code, so the capture reads no message text. |
| `ls-files --stage -z` | The index authority. The staged listing names every stage of every entry, so an unresolved merge changes it exactly as a staged change does. |
| `status --porcelain=v2 -z --ignored=no --untracked-files=all` | The status records of the fingerprint. |
| `ls-files --others --exclude-standard -z` | Every untracked file. The listing writes paths against the working directory, so the capture needs no repository prefix. |
| `diff --raw -z --relative <base>` | The paths, modes, and kinds of every changed file. NUL separation keeps every name one field. |
| `diff --patch --unified=3 --relative <base>` | The exact lines of every changed file. |

Both diff reads carry `--no-ext-diff`, `--no-textconv`, `--find-renames`, and
`--ignore-submodules=none`, so their file order is one order and the capture
joins them by that order. A Git blob holds no kind, so Git publishes a type
change as one removal section and one creation section while the raw listing
names one record. A listing that does not account for every section publishes
nothing, because the capture must never attach the lines of one file to another.

One further rule follows from rename detection: both diff reads always cover the
complete worktree root, even for a one-path target. Rename detection inside one
pathspec would split the pair that a one-path target must publish whole. The
publication filters the parsed records instead, so a one-path capture still
names only its own file and its own rename partner.

Git publishes no patch for an untracked file. The capture reads the exact source
bytes of such a file through the confined worktree root and publishes them as one
run of added lines. The published mode comes from the entry itself, never from
the target of a link.

An unborn `HEAD` is a state of the authority, not a refusal. A caller-supplied
base that the repository still holds stays reviewable against it.

## Review Anchors And Events

A `ReviewAnchor` names:

- the base revision and candidate revision,
- the worktree-relative path,
- the old or new file side,
- hunk identity and line range,
- a digest of the selected lines,
- bounded surrounding context.

Before comment submission, Kvim captures the target authority again. The active
candidate path, mode, side-byte digest, and revision must match that authority.
A changed candidate returns a typed stale-location outcome and emits no event.

A successful submission emits one bounded `ReviewEvent::CommentSubmitted` with
the durable anchor and bounded body. The event assigns no host meaning to the
comment. A full event queue returns `Saturated` before submission and drops no
comment silently.

A pure relocation API compares an anchor with a later candidate. It returns
`Exact`, `Relocated`, `Missing`, or `Ambiguous`. It never guesses among matches.

`ReviewState` holds one candidate, one cursor over its published hunks, one
optional anchor, and the bounded event queue. A reload replaces the complete
candidate and relocates the selection through that API. A missing or an
ambiguous outcome clears the selection, because the review guesses no place.

The search compares the selected-line digest of every window of the anchored
side, and then the recorded context outward from the selection. A later
candidate that publishes a shorter context still matches, and a disagreement
inside the shared part never does. The search itself is bounded. An exhausted
bound returns `Ambiguous`, because the part that the search did not compare can
still hold another match.

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
| Attempts of one capture | `DIFF_CAPTURE_ATTEMPTS_MAX` | 3 | A repository that changes through three attempts is under active work, and a fourth read would cost more without a better chance. |
| Deadline of one capture command | `DIFF_CAPTURE_DEADLINE` | 15 s | One diff of a large repository finishes far below this value. |
| Output of one capture command | `DIFF_PROCESS_OUTPUT_BYTES_MAX` | 8 MiB | The bound covers one process. It is separate from the source bound below. |
| Output of one short answer | `DIFF_ANSWER_OUTPUT_BYTES_MAX` | 8 KiB | One object identifier or one type name stays far below this value. |
| Source bytes of one untracked file | `DIFF_SOURCE_BYTES_MAX` | 1 MiB | A larger file publishes no line and reports truncation, so the reader always sees that content is missing. |
| Binary detection window | `DIFF_BINARY_SCAN_BYTES` | 8000 | Git inspects the same window, so a tracked file and an untracked file take the same answer. |
| Bytes of one comment | `REVIEW_COMMENT_BYTES_MAX` | 8 KiB | One review comment is prose about one selection, so a larger body names another concern. |
| Context lines of one anchor | `REVIEW_CONTEXT_LINES_MAX` | 8 | The context proves the identity of a moved selection. A larger window costs more bytes for every stored anchor and relocates no better. |
| Queued review events | `REVIEW_EVENTS_MAX` | 64 | The host drains the queue after every reduction, so this many comments between two drains is far above the rate of one reader. |

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
repository, explicit worktree-root execution, and a directory that is no
repository.

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
