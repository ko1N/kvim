# The Diff View

This document owns the presentation of one captured diff: the screen rows of one
hunk, the two views that draw them, the changes panel, and the read state of one
review.

[`git.md`](git.md) owns the capture, the compared pair, the anchors, and every
Git command. This document owns what a reader sees.

## Ownership

`kvim-workspace` owns the pure diff values and anchor relocation.
`kvim-tui` owns one private review model and one painter. The model owns both
captured sections, cursor and selection state, read marks, panel state, focus,
viewport state, and the selected diff view. The integrated editor adapts this
model without adding review state. The same painter accepts the private model
for integrated review and future standalone composition.

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

One row names its file alone, exactly as the file tree names one entry. The
directory rows above it carry the rest of the path, the mark column carries the
repository state, and the header of the diff carries the counts. A truncated
file names its bound, because the panel is the one place that can state it.

A file that the reader finished dims. It reads as complete when no hunk stays
unread and no bound truncated it, so a truncated file never dims: the candidate
holds content that the reader cannot reach.

The first row of the review carries the section strip over the diff and the
workspace header over the panel beside it. The strip therefore stops at the edge
of the panel, and the path of the workspace stays on that first line.

The panel carries the header of the file tree, so both sidebars name the
workspace the same way, and it draws the same selection mark, indent guides, and
repository marks. The staged half draws the mark and the color of a staged
entry, and the unstaged half those of a changed one.

### The Header Of The Diff

One line above the diff names the file that the body draws and what changed in
it. An added line count reads green and a removed one reads red, which is the
vocabulary that every diff uses.

A heading takes no selection. Selecting a file row moves the review cursor to
the first hunk of that file.

## Entering, Leaving, And Moving

The current integrated review opens with `<leader>gg` in standalone kvim. The
review owns its keys and preserves the current window, viewport, and buffer
while it draws over the window tree. The review surface survives a close, so
read marks and the cursor remain available when the review opens again.

A planned standalone `ReviewSurface` will be independent from editor bindings.
It will support `from_candidates` without I/O and `for_worktree` with bounded Git
capture. Integrated and standalone review will share private state, relocation,
and painting. Snapshots will preserve bounded review position and read state,
while comment persistence and host meaning remain outside kvim.


The review draws over the window tree. It changes no window, no viewport, and no
buffer, so leaving it restores the layout by drawing that tree again. Nothing is
saved, because nothing is replaced.

The review surface survives a close. A reader who jumps into a file and opens the
review again keeps every read mark and the cursor, and the new captures reload
into the surface instead of replacing it.

### The Sections

The review shows one section at a time, and a strip of tabs at the top names
them: the unstaged half first, because that is the half a reader works on, and
the staged half beside it. `Tab` walks to the next section and `Shift-Tab` to
the previous one, and the walk cycles, so a reader needs no mapping for each
section. A section that publishes no change opens no tab.

One tab carries the mark of its repository state, its name, and the number of
files that it holds. The mark takes the color that the rows below it take for
the same state, so the strip and the panel name one state the same way. The
active tab takes the background of the body below it, so it connects to its own
content, and every other tab dims on the bar above them. A band lighter than the
bar reads washed out, because the bar and the text then sit at the same weight.

`kvim_ui::TabStrip` holds the strip. It is a domain-neutral value: it carries
opaque host identities and bounded labels, owns no surface, and draws every cell
through a host callback. A host that shows a chat, an editor, and a review uses
the same value for those.

### The Two Regions

The review holds two regions: the changes panel and the diff body. One of them
owns the keys, and `Ctrl-H` and `Ctrl-L` move between them, which are the keys
that already move between windows.

Both regions move like an ordinary buffer. The review binds the motions that the
buffer and the file-tree sidebar already publish, so it holds no motion
vocabulary of its own and a reader needs to learn none.

The panel sits at the right edge, where kvim keeps its sidebar, and the diff
fills the rest. `Ctrl-L` therefore reaches the panel and `Ctrl-H` returns to the
diff.

The panel draws the changed files with the design of the file tree: it groups
them by directory, draws the same box-drawing indent guides, and carries the
same file and directory icons. One reader reads one shape. A directory row takes
no selection, and a file row names the file alone, because the rows above it
carry the rest of its path.

The panel draws through the same sidebar value that the file tree uses, so a
list longer than the region scrolls with its selection instead of clipping.

`Ctrl-Alt-H` and `Ctrl-Alt-L` resize the panel, exactly as they resize a window.
The review holds one vertical edge, so it resizes on that axis alone, and the
panel stays inside its bounds: no resize hides it and none takes the diff.

The panel moves its selection over the changed files, and the body always shows
the file that the panel names. The cursor reaches a file in either direction, so
a reader who walks down the list and back up returns to every file. The body holds every published row of that file:
one header for each hunk and the aligned rows of that hunk. A motion of one
region never moves the other one.

| Key | What |
|---|---|
| `q`, `Esc`, `Ctrl-C` | Leave the review |
| `Ctrl-H`, `Ctrl-L` | Move the keys to the diff and to the panel |
| `Ctrl-Alt-H`, `Ctrl-Alt-L` | Widen and narrow the changes panel |
| `Tab`, `Shift-Tab` | The next and the previous section |
| `j`, `k` | One row |
| `Ctrl-D`, `Ctrl-U` | One half page |
| `Ctrl-F`, `Ctrl-B` | One full page |
| `gg`, `G` | The first and the last row |
| `s` | Switch the two views |
| `]c`, `[c` | The next and the previous hunk, as Vim walks a diff |
| `]f`, `[f` | The next and the previous changed file |
| `n`, `N` | The next and the previous unread hunk |
| `m` | Mark the hunk at the cursor as read |
| `R` | Capture the changes of the worktree again |
| `Enter` | Open the file of the hunk at its first line |

A count before a motion names the number of steps, as it does everywhere else.

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

Each half holds its own publication slot. One capture takes several commands and
the two halves run at the same time, so one shared slot would cancel the half
that started first and the review would publish one half alone. A newer capture
of the same half still cancels the one that it replaces.

A repository without a commit publishes no staged half, which is a normal state
and not a failure: `HEAD` names no commit to compare the index against. A
refused capture reaches the message line once and leaves a usable editor.

## Live Updates

An open review shows the worktree, so a change of that worktree captures it
again. The file watch that the file tree already uses carries the change, and
the review needs no key and no refresh command.

This is what makes the review usable beside an agent. The agent writes files,
the diff follows, and every read mark and the selection relocate onto the later
candidate, so a hunk that the change did not touch stays read and a rewritten
hunk becomes unread again.

The review queues no capture while captures of its own have not resolved, so a
burst of changes never grows the outbox. The next burst after they resolve
captures the state that the burst left behind.

The strip of sections follows every capture. Staging moves work from one half to
the other, so the tab of a half that empties closes, the tab of a half that
fills opens, and a reader whose section emptied follows the work instead of
watching an empty view.

### The Git Directory

The file watch skips the Git directory, because a repository writes many files
there and a burst of them would name no change of the worktree. An index write
therefore reaches no burst, so `git add` alone updates no open review.

`R` captures the worktree again, exactly as the same key refreshes the file
tree. A later release can watch the index instead, which would need the watch
registration to hold one exception to the ignored names.

## The Cursor Row

The cursor row of the focused region carries the selection band across its whole
width, including the gap between the two columns, so it reads like a Visual-line
selection instead of a mark at one edge. The foreground of each cell keeps the
color of its own change, so an added line still reads as added under the band.

An unfocused panel still marks its selected row, in a quieter role, so a reader
keeps the place while the keys act in the other region.

## One File, One Body

A hunk identity is unique inside its own file alone. The body therefore names
the file that it draws, and every lookup of one hunk reads that file. Nothing
walks over the hunks to find a place, because such a walk crosses into another
file, where the same identity names another hunk.
