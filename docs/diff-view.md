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
