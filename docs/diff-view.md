# The Diff View

This document owns the presentation of one captured diff: the screen rows of one
hunk, the two views that draw them, the changes panel, and the read state of one
review.

[`git.md`](git.md) owns the capture, the compared pair, the anchors, and every
Git command. This document owns what a reader sees.

## Ownership

`kvim-workspace` owns the pure values: the aligned rows of one hunk and the read
state of one review. `kvim-tui` owns the drawing and the keys.

Kvim owns the view and the neutral values alone. It learns nothing about a
ticket, a session, an agent, or a review thread. A host composes those above the
values that this document names.

## The Screen Rows Of One Hunk

A hunk publishes one sequence of lines, where a removed line and the added line
that replaces it follow each other. A two-column view needs them beside each
other, so one pure function pairs the published lines into rows.

- A context line stands on both sides of one row.
- A run of removed lines pairs one for one with the run of added lines that
  follows it, because a replacement reads best beside the text that it replaced.
- A surplus on either side takes its own row, and the empty side draws as a gap.

The row count never passes the published line count of the longer side. A
truncated hunk therefore aligns exactly what it published and invents no row.

Both views read the same rows. The two-column view draws each side in its own
column. The inline view draws one column and marks the origin of each line.
Neither view can disagree with the other about what one hunk holds, because
neither owns a row model of its own.

## Read State

A review marks each hunk that the reader finished. The mark answers one question
that decides whether a large change is reviewable at all: what changed since I
last looked.

The mark is one anchor over the whole hunk, not a line number. A reload
therefore carries every mark through the relocation of [`git.md`](git.md):

| Relocation | The mark |
|---|---|
| `Exact` | stays; the content and the place are unchanged |
| `Relocated` | stays; the content is unchanged and the lines above it moved |
| `Missing` | clears; the content changed or left the candidate |
| `Ambiguous` | clears; the review guesses no place |

The mark therefore follows content instead of position. A hunk that the author
did not touch stays read after a reload, even when an edit far above it moved
every line number. A rewritten hunk becomes unread, because the reader must see
it again.

The relocation search is bounded. An exhausted bound answers `Ambiguous`, so a
very large file clears a mark that a longer search would have kept. That is the
safe direction: the review shows a hunk again rather than hiding one.

The unread walk reaches the next and the previous unread hunk. A walk that
reaches the border without finding one restores the cursor, so a failed walk
moves nothing.

The read state lives in the review, so it lasts as long as the view. A host that
needs it to outlive the view owns that persistence.

## The Two Views

`EditorSettings` holds the diff policy in `DiffSettings`. `view` names the view
that a review opens with, and `SideBySide` is the default, because a replacement
reads best beside the text that it replaced.

The two-column view gives each side one column of equal width with one gap
between them, and one line-number column inside each. A text that passes its
column clips; it wraps onto no further row, because a wrapped diff row would
break the pairing that the column layout exists for.

`side_column_cells_min` names the smallest useful width of one column. A window
that cannot hold two such columns draws inline whatever the setting asks for,
because two columns of a handful of cells each show nothing.

The inline view draws one column and marks the origin of every line: a space for
an unchanged line, `-` for a removed line, and `+` for an added one. It writes a
removal before the addition that replaced it, which is the order that every
unified diff writes.

## Text That Is Not Text

A capture publishes exact bytes, and not every byte sequence is text. A line
that holds none draws its state instead of guessed characters, so a reader never
mistakes a repaired byte for the content of the file.

## The Changes Panel

The panel lists the changed files of the captured candidates, never of the live
status. The panel and the diff therefore always agree, because both read one
value. A status read that happens between them cannot make them disagree.

Each section holds its own capture: the staged half compares the commit against
the index, and the unstaged half compares the index against the working tree.
The panel never merges them. A section without a change publishes no heading, so
a workspace with nothing staged shows one section instead of an empty one.

One row names its file, the marker of its change kind, its added and removed
line counts, and the number of hunks that stay unread. A file reads as complete
when no hunk stays unread and no bound truncated it. A truncated file never
reads as complete, because the candidate holds content that the reader cannot
reach.

A heading takes no selection. Selecting a file row moves the review cursor to
the first hunk of that file.

## Entering, Leaving, And Moving

`<leader>gg` opens the review, which is the sequence that the reference Git
interface uses, so the key that a reader already knows opens this one.

The review draws over the window tree. It changes no window, no viewport, and no
buffer, so leaving it restores the layout by drawing that tree again. Nothing is
saved, because nothing is replaced.

The review surface survives a close. A reader who jumps into a file and opens the
review again keeps every read mark and the cursor, and the new captures reload
into the surface instead of replacing it.

| Key | What |
|---|---|
| `q`, `Esc`, `Ctrl-C` | Leave the review |
| `s` | Switch the two views |
| `j`, `k` | The next and the previous hunk |
| `n`, `N` | The next and the previous unread hunk |
| `]`, `[` | The first hunk of the next and the previous changed file |
| `m` | Mark the hunk at the cursor as read |
| `Enter` | Open the file of the hunk at its first line |

The review owns every key while it stays open, so a buffer key reaches no
buffer. A key that the review does not bind reaches nothing.

`Enter` leaves the review, records a jump, and opens the file, so `Ctrl-O`
returns to where the reader stood before. The review cursor names one hunk, so
the jump reaches the first line of that hunk. A hunk that publishes no new line,
such as a complete removal, names its old side instead.

## The Captures Of One Review

The review captures both halves through the bounded process service, so it opens
at once and fills as the captures resolve. The session runs no `git` command
itself.

A repository without a commit publishes no staged half, which is a normal state
and not a failure: `HEAD` names no commit to compare the index against. A
refused capture reaches the message line once and leaves a usable editor.
