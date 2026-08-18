# Windows And Presentation

## Ownership

The `tui` module owns the window tree, layout, focus, resize, rendering, and the
theme. It is the sole owner of visible editor state. See
[`architecture.md`](architecture.md).

The window tree contains no buffer text and no terminal colors. It contains
window identities, split structure, and validated dimensions.

## Window Tree

The window tree is a binary tree with two node kinds:

- A leaf window shows one buffer. It has a stable window identity and one view
  into that buffer.
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

A window owns the cursor, the selection anchor, and the viewport. Only the
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
the window tree removes it. The gutter width depends on the buffer, which the
window tree never holds, so the session narrows the viewport width after every
layout change. The viewport therefore always reports the cells that the renderer
paints with buffer text.

## Layout

One layout calculation converts the window tree and the terminal size into the
exact rectangle for every window and sidebar. Rendering, scrolling, focus,
resize, and tests all use these rectangles. No other code computes a rectangle.

Layout is deterministic. Equal tree, equal ratios, and equal terminal size
produce equal rectangles.

## Split Creation

A new horizontal split opens the new window below the current window. A new
vertical split opens the new window to the right of the current window. Both
defaults belong to `EditorSettings`. See [`settings.md`](settings.md).

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
sidebar changes the sidebar width instead of refusing the command.

[`input-actions.md`](input-actions.md) owns the keys for focus and resize.

## Minimum Dimensions And Terminal Resize

Every window has a minimum width and a minimum height. The layout calculation
enforces the minimum before it publishes rectangles.

A terminal resize recomputes the layout from the same tree. It does not change
the tree structure and it does not change window identities. If the terminal
becomes too small for the current tree, the layout hides windows in a
deterministic order and keeps the focused window visible.

The default minimum window width is 20 cells. It keeps a line number column, a
sign column, and readable text visible. The default minimum window height is 3
rows. It keeps a winbar row and readable text visible. Both values belong to
`EditorSettings`. See [`settings.md`](settings.md).

The implemented layout confirms both values. A split node divides its rectangle
only while that rectangle holds two children at the minimum, so the layout
publishes 20 cells and 3 rows as the smallest window. A rectangle that is too
small keeps the subtree that holds the focused window instead.

## Sidebars

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
one binding scope for it, so a tree key never reaches an editor window. See
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
  active mode and the cursor position.
- The message line is the last row. It shows the last message, and the command
  line and the search prompt share it.

One winbar row sits above the text of every window. It shows, from the left, one
blank, the path of the buffer, and a marker for a modified buffer. It shows the
scroll position at the right edge.

The path is relative to the directory that Kvim started in, which is the
workspace root that the file tree shows. A file outside that root keeps its
complete path, because no relative path reaches it. A buffer that holds no file
shows its short name. A path that is too long for the row loses its start, and a
`<` marks the cut, so the file name always stays visible. The cut counts
terminal cells, so it never splits a wide character and never overflows the row.

The scroll position reports where the visible rows sit inside the buffer, in
three cells: `ALL` while the complete buffer fits, `TOP` while the first line is
visible, `BOT` while the last line is visible, and otherwise the share of the
buffer above the first visible line, in percent. Kvim follows the Vim
convention, so the three named outcomes take precedence over a number.

A winbar that cannot hold every part drops them in a fixed order: the scroll
position first, then the changed marker. The path always survives, because it
names the file.

A terminal that cannot hold every band drops the bands in a deterministic
order: the body first, then the statusline. The message line survives longest,
because it reports why the terminal is too small.

No region carries a divider glyph. The surface color of the winbar band and of
the statusline band separates the regions, and the title color separates the
focused window from the others. Kvim keeps the borderless ReviewGraph
presentation. See [`reviewgraph-integration.md`](reviewgraph-integration.md).

The which-key overlay covers the bottom of the body band. It lists the next keys
that may follow the pending key sequence, one key for each row, and it shows a
bounded number of rows. [`input-actions.md`](input-actions.md) owns the rows and
the delay.

## Theme

The theme maps semantic roles to terminal styles. Call sites request a role,
such as normal text, selection, search match, line number, active line number,
window title, status text, or a syntax role. Call sites never name a raw color.
Only the theme holds color values.

The palette is tokyonight night with a darkened base color `#111317` and a
surface color `#161a20`. Both values belong to `EditorSettings`. See
[`settings.md`](settings.md). Every other palette value comes from the
tokyonight night palette. This document does not restate those values.

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
| StatuslineMuted | The statusline text of an unfocused region |
| Winbar | The winbar band above one window |
| Title | The title of a focused window or of an overlay |
| TitleMuted | The title of an unfocused window |
| PopupSelection | The selected row of a popup list |
| Icon(role) | One file-tree icon |
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

Syntax roles are terminal-independent at the language boundary, so
`kvim-language` owns the role set. The theme maps each role to one style. See
[`language-services.md`](language-services.md).

The role set is: Attribute, Boolean, Bracket, Comment, Constant, Constructor,
Delimiter, Function, Keyword, Macro, Number, Operator, Parameter, Preprocessor,
Property, Statement, String, Type, and Variable. The comment role and the
keyword role also carry the italic modifier of the reference configuration.

Add a new syntax role in `kvim-language`, then add its style here. The theme
holds the color; it never defines the role.

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

Kvim shows absolute and relative line numbers together. The cursor line shows
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

Kvim renders only after a visible state change. See
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
