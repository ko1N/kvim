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
