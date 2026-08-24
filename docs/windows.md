# Windows And Presentation

## Ownership

`kvim-ui` owns generic split topology, sidebar state, deterministic geometry,
and domain-neutral ratatui presentation. `kvim-tui` owns editor and review
presentation adapters, the standalone theme, and the editor log. One host owner
owns visible state for each composed interface. See
[`architecture.md`](architecture.md) and [`embedding.md`](embedding.md).

`WindowTree<SurfaceId>` contains opaque surface identities, split structure,
validated ratios, focus, limits, and minimum dimensions. Host surface values,
buffer text, and terminal colors stay outside the tree.

## Window Tree

The window tree is a binary tree with two node kinds:

- A leaf holds one opaque surface identity. The standalone adapter associates
  that identity with one editor window and its buffer view.
- A split node has an orientation and two children. A horizontal split node
  stacks its children top and bottom. A vertical split node places its children
  left and right.

A window identity stays stable while the window exists. Splitting, resizing,
closing a sibling, and resizing the terminal never change an existing identity.
Focus, buffer association, and the view of the window follow the identity.

Closing a window replaces its parent split node with the remaining sibling. The
tree always has at least one leaf window. The closed window discards its view
with it.

## Window View

A window owns the cursor, the selection anchor, and the viewport. The generic
tree holds none of them: the standalone adapter in `kvim-tui` owns one view for
each window identity and discards that view when the window closes. Only the
buffer text is shared. Two windows that show one buffer therefore move and
scroll independently: a scroll in one window moves no other window, and a move
in one window moves no other cursor.

The mode is global. Vim holds one mode, not one mode for each window, so the
editor keeps the mode, the operator-pending state, and the repeat description
outside the window.

Moving the focus changes no cursor. Each window resumes exactly where it was.
Showing another buffer in a window restarts the cursor and the anchor of that
window at the first line, because both describe the previous text.

The viewport of every window reconciles against the cursor of that window and
the buffer of that window. See the scroll margin in
[Buffer Presentation](#buffer-presentation).

The viewport covers the text rows of the window rectangle, never the complete
rectangle. The winbar row belongs to the rectangle and shows no buffer line, so
the standalone adapter removes it. The gutter width depends on the buffer, which
the adapter never holds, so the session narrows the viewport width after every
layout change. The viewport therefore always reports the cells that the renderer
paints with buffer text.

## Layout

One layout calculation converts the window tree and a caller-supplied rectangle
into the exact placement of each surface and sidebar. Rendering, scrolling,
focus, resize, and tests all use these rectangles. No other code computes a
rectangle.

Layout is deterministic. Equal tree, equal ratios, and equal terminal size
produce equal rectangles.

Layout returns complete or explicitly constrained output. It never silently
hides a surface. A constrained layout keeps the focused surface visible and
names every constraint. Leaf count, tree depth, ratio precision, minimum
dimensions, and identity allocation are bounded and validated.

## Split Creation

A new horizontal split opens the new window below the current window. A new
vertical split opens the new window to the right of the current window. Both
defaults belong to `EditorSettings`. See [`settings.md`](settings.md).

An explicit split succeeds only when both new subtrees fit the supplied area.
Recursive child minima decide that fit. A refused split leaves topology and
focus unchanged.

The new window shows the same buffer as the source window, and it copies the
cursor, the selection anchor, and the viewport of that window, so it opens at
the same place. Both windows then move independently. The new window receives
focus.

## Adaptive Split

The adaptive split command selects the orientation from the current window
rectangle. It selects a vertical split when the width exceeds the height
multiplied by the adaptive ratio of 2.5. Otherwise it selects a horizontal
split.

One rule comes before the ratio: when the terminal holds exactly one editor
window, the adaptive split always selects a vertical split. A full-width terminal
would otherwise divide into two short windows. The reference configuration uses
the same exception.

The inverse adaptive split command mirrors that decision. It selects a
horizontal split when the width exceeds the height multiplied by the same ratio.
Otherwise it selects a vertical split.

The adaptive ratio belongs to `EditorSettings`.

## Focus And Resize

Directional focus moves from the focused window to the nearest window in the
requested direction. The move uses the layout rectangles, not the tree order. If
no window exists in that direction, focus stays unchanged.

Directional resize moves one shared edge by six cells. The resize step belongs to
`EditorSettings`. The command names the direction that the edge moves, not a size
change. The focused window therefore grows or shrinks according to which side
holds a neighbor:

- A neighbor on the right, for a horizontal command, or below, for a vertical
  command: move that shared edge in the named direction. `Ctrl-Alt-H` moves the
  right edge left, so the focused window shrinks. `Ctrl-Alt-L` moves the right
  edge right, so the focused window grows.
- No neighbor there, but a neighbor on the left or above: move that shared edge
  in the named direction instead. `Ctrl-Alt-H` then moves the left edge left, so
  the focused window grows.
- No neighbor on either side: leave the layout unchanged.

The far edge always wins when both edges exist. The reference configuration uses
this same rule. One key then always moves the layout in one direction, whichever
side of a split the focused window is on.

Neighbor detection compares layout rectangles, not tree order. Two windows are
neighbors when one edge meets the other edge and the perpendicular ranges
overlap. When several windows qualify, the window with the largest overlap wins.

A resize moves absolute cells, not a ratio. The editor computes the current
layout in cells, then moves the one border that the rules above select by the
resize step:

- The panes on the other side of that border give up the cells. The pane at the
  border gives first.
- A pane that would fall below its minimum dimensions keeps its minimum and
  passes the remaining cells to the next pane along the same direction.
- The cascade repeats until every cell is placed.
- Every other pane keeps its exact cell size. A resize therefore never
  rearranges a pane that shares no border with the moved one.

A split node stores the share of its first child as a weight. The editor derives
those weights again from the resulting cell sizes, so the layout calculation
reproduces the same rectangles. The weight stays the storage format, and the
cell stays the unit of the operation.

A resize that reaches no arrangement that keeps every minimum leaves the layout
unchanged.

A sidebar keeps a fixed width, but a directional resize whose neighbor is a
sidebar changes the sidebar width instead of refusing the command. The inner
edge of a sidebar is one border of the layout, so it follows the same absolute
rule: the pane that touches the sidebar absorbs the cells, a pane that reaches
its minimum passes the rest to the next pane along, and every other pane keeps
its exact width. A sidebar that holds the focus answers the resize keys itself,
so the file tree resizes without leaving it first.

[`input-actions.md`](input-actions.md) owns the keys for focus and resize.

### Reflow And Explicit Resize

One representation carries two rules, because the two operations want opposite
behavior. Keep them apart.

- A reflow redistributes proportionally. A terminal resize and a new split
  change the extent that a subtree divides, and every pane inside that subtree
  gives or takes its share. The weight expresses that directly, so the layout
  fills the new terminal without a stale absolute size.
- An explicit resize command is absolute. It resolves the affected subtree to
  cells, moves exactly one border by the resize step, and holds every pane that
  does not share that border at its exact cell count.

The explicit resize derives the weights again from the resulting cell sizes. The
weight denominator is above every terminal extent, so one weight reproduces one
cell count exactly, and a repeated command therefore never drifts by one cell.

A proportional explicit resize would move every border of the layout at once,
which is the behavior that the absolute rule replaces. Do not collapse the two
rules into one.

## Minimum Dimensions And Terminal Resize

Every window has a minimum width and a minimum height. The layout calculation
enforces the minimum before it publishes rectangles.

A host-area resize recomputes the layout from the same tree. It does not change
tree structure or surface identities. If the area becomes too small, layout
returns an explicitly constrained result and keeps the focused surface visible.

The default minimum window width is 20 cells. It keeps a line number column, a
sign column, and readable text visible. The default minimum window height is 3
rows. It keeps a winbar row and readable text visible. Both values belong to
`EditorSettings`. See [`settings.md`](settings.md).

The implemented layout confirms both values. A split node divides its rectangle
only while that rectangle holds two children at the minimum, so the layout
publishes 20 cells and 3 rows as the smallest window. A rectangle that is too
small keeps the subtree that holds the focused window instead.

## Sidebars

`SidebarState<RowId>` owns selection and viewport state only. Rows, actions,
styles, labels, and semantic meaning are borrowed host inputs. Each row supplies
a bounded, variable height in terminal rows.

Selection and scrolling count terminal rows, not only item indexes. Layout
publishes clipped visible row placements. A host callback renders each placement
inside its clipped rectangle, so one row can use several lines, arbitrary cells,
styled spans, markers, and host semantic state.

Sidebar row, height, line, cell, label, action, and output counts are bounded.
A full component event queue returns `Saturated` and drops no event silently.

A sidebar is a fixed-width region at the edge of the terminal. The right-side
file tree is a sidebar. A sidebar is not an ordinary editor window:

- A sidebar has no place in the window tree.
- A sidebar has a fixed width, not a ratio.
- A sidebar does not participate in adaptive splits.
- A sidebar cannot hold focus while it is hidden.

Hiding a sidebar that holds focus moves focus to the previously focused editor
window. [`files.md`](files.md) owns file-tree behavior.

The file tree opens at 40 cells. The width belongs to `EditorSettings`. See
[`settings.md`](settings.md).

A sidebar owns its own keys while it holds the focus. The mapping registry keeps
one binding scope for it, so a tree key never reaches an editor window. That
scope holds the directional focus keys and the directional resize keys as well,
so the focused file tree resizes like an editor window. See
[`input-actions.md`](input-actions.md).

The sidebar keeps one identity for the complete session. Closing it hides the
region and keeps that identity, so a later reveal shows the same sidebar. One
title row above the rows names the workspace root, and it carries the focused or
the unfocused title color. The terminal draws its own cursor on the selected row
while the sidebar holds the focus, so one frame still reports one cursor cell.

## Chrome

The terminal holds three bands. The window tree receives the body band only.

- The body band holds the window tree and every overlay.
- The statusline band holds one statusline for the whole terminal. It shows the
  active mode at the left. At the right it shows the format-on-save state of the
  focused buffer, and then the cursor position. The statusline shows that state
  only while a formatter can format the focused buffer, so it never promises a
  format that no save performs. See
  [`language-services.md`](language-services.md). A band that cannot hold every
  part drops the format-on-save state first, then the cursor position. The mode
  always survives, because the mode decides what the next key does.
- The message line is the last row. It shows the last message, and the command
  line and the search prompt share it. An ordinary message takes the normal text
  color, so only a warning and a failure stand out. A confirmation shows its
  question on the same row, over the prompt and over the message, because it
  owns the keys. The user types the answer after the hint of the question, so
  the line draws the cursor after that answer. Every message that this line
  shows also reaches the editor log, so a replaced message stays readable. See
  [`input-actions.md`](input-actions.md) and [The Editor Log](#the-editor-log).

The command line can open a candidate list of the completion. The list is an
overlay, so it takes the last rows of the body band and covers neither the
message line nor the statusline. See
[Command-Line Candidate List](#command-line-candidate-list).

One winbar row sits above the text of every window. It shows, from the left, one
blank, the path of the buffer, and a marker for a modified buffer. It shows the
scroll position at the right edge.

The path is relative to the explicit worktree root that the file tree shows. An
outside path is rejected before a buffer opens. A buffer that holds no file
shows its short name. A path that is too long for the row loses its start, and a
`<` marks the cut, so the file name always stays visible. The cut counts terminal
cells, so it never splits a wide character or overflows the row.

The scroll position reports where the visible rows sit inside the buffer, in
three cells: `ALL` while the complete buffer fits, `TOP` while the first line is
visible, `BOT` while the last line is visible, and otherwise the share of the
buffer above the first visible line, in percent. kvim follows the Vim
convention, so the three named outcomes take precedence over a number.

A winbar that cannot hold every part drops them in a fixed order: the scroll
position first, then the changed marker. The path always survives, because it
names the file.

A terminal that cannot hold every band drops the bands in a deterministic
order: the body first, then the statusline. The message line survives longest,
because it reports why the terminal is too small.

No region carries a divider glyph. The surface color of the winbar band and of
the statusline band separates the regions, and the title color separates the
focused window from the others. kvim keeps the borderless ReviewGraph
presentation. See [`reviewgraph-integration.md`](reviewgraph-integration.md).

The which-key overlay covers the bottom of the body band. It lists the next keys
that may follow the pending key sequence, one key for each row, and it shows a
bounded number of rows. [`input-actions.md`](input-actions.md) owns the rows and
the delay.

The language float covers a part of one window instead of the body band. It sits
beside the cursor cell of the focused window, so it never reaches outside that
window rectangle, and a split places it inside its own window. It uses the same
cursor cell that the frame reports for the terminal cursor, so the float and the
cursor never disagree. [`language-services.md`](language-services.md) owns the
placement rule and the bounds.

The overlays paint in a fixed order over the window tree: the notification
overlay first, then the language float, then the command-line candidate list,
then the which-key overlay, and the picker last over the complete terminal.

### Command-Line Candidate List

The candidate list of the command-line completion takes the last rows of the
body band. The statusline and the message line stay below it. The command line
that the list describes therefore always stays visible. The list appears only
while more than one candidate matches, because one candidate needs no choice.
[`input-actions.md`](input-actions.md) owns the keys that cycle it.

The list shows at most eight rows, and never more rows than the body band
holds. A list that holds more candidates than rows replaces its last row with
`...`, so no candidate disappears without a note. The language float reports a
lost row the same way. See [`language-services.md`](language-services.md). The
shown candidates always hold the selected one, so a cycle moves the shown
candidates instead of hiding the selection.

The list is at most 48 cells wide, and never wider than the body band. A
candidate that is wider than the list loses its start, and a `<` marks the cut.
The winbar cuts a long path the same way, and for the same reason: the file name
at the end of a path names the file that the user looks for. Every row of one
path list also starts with the same command name, so the cut hides no text that
separates two rows. The clip counts terminal cells, so it never splits a wide
character.

The text of a row starts one cell inside the list, so a candidate stands above
the text of the command line, which follows the `:` prefix. The selected row
carries the selection color of a popup list, which the picker uses for its own
selected row. The list is decoration: it changes no buffer text, no cursor
position, and no line mapping.

The list and the notification overlay both reach the last rows of the body
band. The list draws over the notification overlay. The user cycles the list
with a key and reads it now. The overlay reports background work that no key
waits for. See [`language-services.md`](language-services.md).

## The Editor Log

The message line shows one message. A second message replaces the first one, so
the message line alone loses a report that the user still needs. The editor
therefore keeps a bounded log beside the message line, and every report that
reaches the message line also reaches the log.

The `tui` module owns the log, because it owns the message line and every other
visible editor state. The log is a history. It never replaces the message line,
and the message line reports exactly what it reports without it.

### The Shape Of One Entry

One entry holds five values:

- the elapsed time since the editor started,
- the severity of the report, which is `Error`, `Warning`, or `Info`,
- the source that made the report,
- the one-line text of the report,
- the number of times that the report repeated.

The elapsed time is the time that the event loop already passes into every
entry point. The session reads no clock, so no entry holds a wall-clock time and
no entry needs one. See [`responsiveness.md`](responsiveness.md). A reader needs
the order of the reports and the distance between two of them, and the elapsed
time gives both.

The log keeps at most 256 entries. A new entry above that count removes the
oldest entry, so the newest reports always survive. The number holds every
report of a normal session, and it still holds several groups of reports from a
component that fails and starts again, which is the case that a reader opens the
log for. A smaller number loses the first report of such a group, which is the
report that usually names the cause.

The text of one entry keeps at most as many characters as the message line
keeps. One long report therefore never fills the log. A message-line entry also
loses no character that the message line showed.

That bound counts the text of the entry alone. It counts no other field. The
time, the severity, the source, and the count each add characters to the
rendered row, so one row can be longer than the text bound. Every field of a
row is bounded, so the row stays bounded as well.

The log replaces every control character of the text with one blank. One entry
is therefore one row of the log buffer, and a search reaches every entry.

One entry renders as four fields with one blank between them, and a count after
the last field:

```
00:12.345 ERROR MESSAGE the file does not exist
00:13.001 INFO  MESSAGE "main.rs" 42L, 900B
00:13.400 INFO  SERVER  rust/rust-analyzer started
00:14.902 INFO  JOB     analysis rejected: the buffer changed (x84)
```

The first field is the elapsed time as minutes, seconds, and milliseconds. The
minutes field grows past two digits after 100 minutes. The second field is the
severity, and the third field is the source. Both are uppercase and padded to a
fixed width, so the entries align and a search for `ERROR` or for `MESSAGE`
reaches one severity or one source without reaching ordinary report text. The
fourth field is the text of the report. A count above one follows the text.

### The Sources Of One Entry

The log holds the reports of three sources, and every source uses the one entry
shape above. A later source adds one label. It adds no second store, no second
entry shape, and no second rule.

`MESSAGE` names every report that reached the message line. Without the log a
user reads no report that a second report replaced.

`SERVER` names the states of one language server: its start, its restart, its
stop, its failure, and a program that the host does not hold. It also names the
text that the server wrote to its standard error, and the moment when that text
passed the bound of the editor. Every `SERVER` entry names the adapter and the
server, so a reader knows which server made the report. Without the log a user
reads no cause for a server that cannot start.

The text of a server is not a failure by itself, because a healthy server writes
notes while it runs. The log therefore records that text at the `Info` severity,
and the lifecycle entry beside it carries the severity of the state. The
`language` module bounds the text of one server before it reaches the log, so
one server that writes without limit costs bounded memory. See
[`language-services.md`](language-services.md).

`JOB` names one background job that ended without a report on the message line.
An analysis that passed a bound, a job that the worker service refused, and a
result that a newer buffer version displaced are all such outcomes. Without the
log a user reads no cause for a file that lost its highlighting.

Every `JOB` entry names the job first and the outcome second. It names no
buffer and no path, so every repeat of one outcome carries the same text and
collapses into one entry. [`responsiveness.md`](responsiveness.md) owns the
list of the recorded outcomes and the reason for each one.

### One Entry For One Repeated Report

A background job repeats one outcome as often as the user types. A log that
adds one entry for each repeat therefore loses every earlier report inside one
paragraph of typing. That log looks complete and is not, which is worse than no
log. The log therefore collapses a repeated report into one entry with a count.

The log compares a new report with its newest entry alone. Two reports are the
same report when the source, the severity, and the one-line text are all equal.
The log then raises the count of that entry, and it adds no entry. The entry
keeps the time of its first report, so the entry names when the group started.

Any other report ends the group. A later repeat of an earlier report starts a
new entry, so the log merges no two groups that another report separates. The
count stops at the largest value that its field holds. A group that repeats
without limit therefore costs one entry and one bounded number.

A row shows a count above one as `(xN)` after the text of the report. A row of
a single report shows no count. The rule holds for every source, so the log
keeps one mechanism and one entry shape.

### Opening The Log

`:l[ogs]` opens one snapshot of the log as a new buffer, newest entry last. The
snapshot is a value, so the buffer never changes while it is open and an edit of
that buffer changes no entry. A log that holds no entry opens an empty buffer,
because the editor reported nothing. See [`input-actions.md`](input-actions.md).

## Theme

Public `kvim-ui` widgets accept explicit ratatui styles and semantic roles. They
do not depend on the standalone `Theme` or on host-domain state. Which-key
presentation consumes hints from the active shared resolver.

Every render validates that its rectangle fits the supplied
`ratatui::Buffer`. Invalid geometry returns a typed error before any cell
changes. Rendering performs no input or output and writes only inside its area.

The theme maps semantic roles to terminal styles. Call sites request a role,
such as normal text, selection, search match, line number, active line number,
window title, status text, or a syntax role. Call sites never name a raw color.
Only the theme holds color values.

The palette is tokyonight night with a darkened base color `#111317` and a
surface color `#161a20`. Every other palette value comes from the tokyonight
night palette. This document does not restate those values.

### Recoloring The Editor

`crates/kvim-tui/src/theme.rs` is the one file in the workspace that holds a
color value. To recolor the editor, edit the palette constants at the top of
that file and rebuild with `nix develop -c cargo build --release`.

A constant names a color of the palette, for example `BASE`, `TEXT`, or
`WARNING`. A role names what the editor marks with it, for example
`ThemeRole::SearchMatch`. Change a constant to recolor every role that uses it,
and change a role arm in `Theme::style` to move one part of the interface onto
another color. Neither change reaches a call site, because a call site names a
role and never a color.

The test `only_the_theme_module_names_a_color` reads the sources of the crate
and fails when a color reaches any other module. It exempts `theme.rs`, and it
exempts `render_tests.rs` and `picker_tests.rs`, which name colors to assert
them. It reads code lines only, so a color inside a comment is not a failure.

The first release reads no configuration file, so the palette is compiled in.
[`settings.md`](settings.md) records that decision.

### Interface Roles

The `kvim-tui` theme owns this role set. A new call site selects one of these
roles. A new role belongs here first, and its color stays in code.

| Role | Meaning |
|---|---|
| Text | Buffer text on the editor background |
| NonText | A glyph that stands for absent text |
| EndOfBuffer | The marker on the rows below the last buffer line |
| Cursor | The cell that marks the cursor of the prompt line |
| Selection | A cell inside the Visual selection |
| SearchMatch | A cell inside one search match |
| CurrentSearchMatch | A cell inside the match that holds the cursor |
| MatchingBracket | A cell of the bracket pair that the cursor stands on |
| LineNumber | A line number that is not the cursor line |
| CursorLineNumber | The absolute number of the cursor line |
| SignColumn | The sign column beside the line numbers |
| Surface | The background band of a floating surface or a popup |
| Statusline | The statusline text |
| StatuslineMuted | The quiet part of the statusline, such as the format-on-save state |
| Winbar | The winbar band above one window |
| Title | The title of a focused window or of an overlay |
| TitleMuted | The title of an unfocused window |
| PopupSelection | The selected row of a popup list |
| Icon(role) | One file-tree icon |
| Markup(role) | One markup role of a server answer |
| MarkupStructure | One glyph that the float draws for a markup document |
| Error, Warning, Info, Hint | One message severity |
| Syntax(role) | One syntax role of a language adapter |

The selection and the prompt cursor carry no color of their own. They decorate
the style below them, so a later syntax color survives both. A file-tree icon
carries a foreground color only, so a selected row keeps its background behind
the glyph. The matching bracket carries a foreground color and the bold
modifier, so the selection band and the search band stay visible under it.

### Icon Roles

An icon role names what a file-tree entry is, never its color. The role set is:
Directory, Code, Configuration, Document, Script, VersionControl, Generated,
Media, and Unknown. Every role maps to a color of the palette above, so the
icons add no new color. [`files.md`](files.md) owns the icon table and the
setting that hides the icons.

A window title uses the title color, which the reference palette shares with
the function syntax role. The surface band and the bold modifier keep a title
distinct from code, so a title never reads as a function name. The focused
window title is emphasized. Other window titles use the dimmed text color on
the same surface band.

### Syntax Roles

Syntax roles are terminal-independent, so `kvim-syntax` owns the non-exhaustive
role set. The standalone theme maps each current role to one style. See
[`language-services.md`](language-services.md).

The role set is: Attribute, Boolean, Bracket, Comment, Constant, Constructor,
Delimiter, Function, Keyword, Macro, Number, Operator, Parameter, Preprocessor,
Property, Statement, String, Type, and Variable. The comment role and the
keyword role also carry the italic modifier of the reference configuration.

Add a new syntax role in `kvim-syntax`, then add its standalone style here. The theme
holds the color; it never defines the role.

### Markup Roles

A markup role names one stretch of text of a server answer, so `kvim-language`
owns that role set as well. The role set is: Text, Heading, Emphasis, Strong,
InlineCode, Link, and Quote. [`language-services.md`](language-services.md) owns
the roles and the document that carries them.

The heading role carries the bold modifier, the strong role carries it as well,
the emphasis role carries the italic modifier, and the link role carries the
underline. The quote role carries the italic modifier and the quiet text color.
The code span role takes the color of a string literal, because both hold source
text. Every markup style holds a foreground color alone, so the surface band of
the float stays behind the text.

A code block of a document carries the highlight spans of its fence beside its
lines. The float paints the range of each span in the syntax role of that span,
through the mapping above that already paints a buffer. One code text therefore
takes one color in a hover answer and in an open file, and the float adds no
color of its own. The parts of a code line that no span names take the code span
role, so a fence without a span paints in one color.

The float draws two glyphs that no role of the document names: the thematic
break and the marker of a list item. Both take the structure role, which is
quiet, because they separate text instead of holding it.

## Buffer Presentation

Every window paints the buffer of its own leaf. Two windows therefore show two
different files after `:e` in a split. The focused window paints the Visual
selection, because the mode is global and belongs to the focused window. An
unfocused window paints none.

The terminal draws the cursor itself, because a cell grid cannot draw half a
cell. One frame reports one cursor cell: the cell of the focused window, behind
the gutter and after the horizontal scroll. An unfocused window reports no cell,
so it shows no cursor. It still holds its own cursor position, so its relative
line numbers count from that line.

The cursor shape follows the mode. Insert mode requests a steady vertical bar,
and every other mode requests a steady block. The editor writes the shape only
after a mode change, never for each frame, and it restores the previous cursor
state when it exits. The shape is decoration: a terminal that ignores the
sequence still shows its own cursor.

The search highlight belongs to the active buffer, so a window that shows
another buffer paints no match.

One buffer cell collects its style in a fixed order. The text style sits at the
bottom, the syntax role sits over it, then the diagnostic underline, then the
bracket pair, then the Visual selection, and the search match sits on top. A
selected bracket therefore still reads as selected, and a searched bracket still
reads as a match.

The focused window marks the bracket pair that its cursor stands on. The pair
comes from the search that the `%` motion uses, so the highlight always marks
the bracket that a jump reaches. See [`input-actions.md`](input-actions.md). The
character under the cursor decides whether a pair exists, so a bracket that only
follows the cursor on the same line marks nothing. Normal mode paints the pair;
every other mode paints none. A bracket outside the visible lines paints no
cell.

The Visual selection covers exactly the selected characters. It never paints a
cell behind the last character of a line, in any of the three selection shapes.
A selected line without a character therefore shows no highlighted cell, and a
rectangular selection stops at the last character of a shorter line.

kvim shows absolute and relative line numbers together. The cursor line shows
its absolute number. Every other line shows its distance from the cursor line.
Both settings belong to `EditorSettings`.

The vertical scroll margin is two rows. Movement starts scrolling before the
cursor reaches the top or bottom edge of the window. A small window reduces the
margin so the cursor line always stays visible. An explicit alignment command
overrides the margin for that command.

The horizontal scroll margin is four columns and follows the same rule. Both
margins belong to `EditorSettings`. See [`settings.md`](settings.md).

Line wrapping is disabled by default. A long line scrolls horizontally inside
its window. Rendering clips at the window edge deterministically.

Rendering uses terminal-cell widths, not byte counts or character counts. See
[`text-model.md`](text-model.md) for the coordinate rule.

kvim renders only after a visible state change. See
[`responsiveness.md`](responsiveness.md).

## Sign Column

The gutter of a buffer window holds the sign column and then the number column.
The sign column is one cell wide and sits left of the numbers. The sign rule
belongs to `EditorSettings`. The default rule reserves the column at all times,
so an arriving or a leaving diagnostic never moves the buffer text sideways. See
[`settings.md`](settings.md).

One row shows one sign at most:

- A row after the last buffer line shows `~`. The marker takes the color of a
  glyph that stands for absent text, so the reader sees which rows hold no text.
  A window without a reserved sign column still marks that cell, because no
  number and no character claims it.
- A row with a buffer line shows the sign of the strictest diagnostic severity
  that marks the line: `E` for an error, `H` for a warning, `I` for information,
  and `H` for a hint. Each sign takes the color of its severity.
- Every other row leaves the cell empty.

A diagnostic names a buffer line, and a row after the last buffer line holds no
buffer line, so the two can never compete for the cell. The row decides which
value applies, and a diagnostic that reaches past the last line marks no row
after it.

The sidebar is not a buffer window. It shows no end-of-buffer marker and no
diagnostic sign. See [`files.md`](files.md).
