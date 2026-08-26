# Windows And Presentation

## Ownership

`kvim-ui` owns generic split topology, sidebar state, the domain-neutral
selector, deterministic geometry, domain-neutral ratatui presentation, and the
workspace composer that joins them.
`kvim-tui` owns editor and review presentation adapters, the standalone theme,
and the editor log. One host owner owns visible state for each composed
interface. See [`architecture.md`](architecture.md) and
[`embedding.md`](embedding.md).

`WorkspaceComposer<SurfaceId>` is the one composition model. It holds the split
tree, the sidebar regions, the overlay ownership, the focus, one shared
resolver, and the which-key state of that resolver. It computes no rectangle of
its own: one layout pass reads the window layout and publishes the clipped
placement of every visible surface and of the open overlay.
[`embedding.md`](embedding.md) owns the transition protocol that moves focus and
overlay ownership.

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

A close of the focused region takes the focused sidebar first: that sidebar
hides and keeps its surface, so showing it again restores it unchanged. A close
that reaches the last leaf window reports `CloseOutcome::LastWindow` and changes
nothing, so the caller above the tree decides what happens next.

`WorkspaceComposer::close_focused` adds the composition rules to that tree
operation. It commits at once, because the surface that would have to reset its
semantic phases is the surface that goes away. A surface that no region shows
any longer leaves the composer with its context, the shared resolver drops its
pending key prefix, and every waiting focus or overlay proposal ends.
[`embedding.md`](embedding.md) owns the transition protocol that this close
bypasses.

## Window View

A window owns the cursor, the selection anchor, the viewport, and the jump
list. The generic tree holds none of them: the standalone adapter in `kvim-tui`
owns one view for each window identity and discards that view when the window
closes. Only the buffer text is shared. Two windows that show one buffer
therefore move and scroll independently: a scroll in one window moves no other
window, and a move in one window moves no other cursor.

Each window owns one jump list, so two windows walk independent histories. A
closed window discards its list with its view. A split copies the jump list of
the source window, so the new window returns to the same recorded positions,
and both lists grow apart from that moment. This matches the rule that a new
window opens at the same place as its source. See
[Split Creation](#split-creation).

The list holds at most `JUMPS_MAX` positions, which is 100, and a push past
that bound drops the oldest entry. A recorded position clamps when the buffer
shrank under it. The editor adjusts no recorded position while the user types,
which keeps that work off the edit path.

The jump list lives in `kvim-tui` and not in `kvim-editor`, because one entry
names a `BufferId`, and `kvim-workspace` owns that type. The layer table in
[`architecture.md`](architecture.md) gives `kvim-editor` a dependency on
`kvim-core`, `kvim-input`, and `kvim-settings` only, so `kvim-editor` cannot
name a `BufferId`.

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
cursor, the selection anchor, the viewport, and the jump list of that window,
so it opens at the same place and returns to the same recorded positions. Both
windows then move independently, and both jump lists grow apart from that
moment. The new window receives focus.

## Adaptive Split

`WindowTree::adaptive_orientation` owns the adaptive split rule. It selects the
orientation from the current window rectangle and a caller-supplied ratio. It
selects a vertical split when the width exceeds the height multiplied by the
ratio. Otherwise it selects a horizontal split. One rule now serves kvim and
every host that composes a `WindowTree`: the host supplies the ratio, and it
reaches the rule through `kvim-ui` alone, with no dependency on `kvim-tui` or
`EditorSettings`.

`AdaptiveSplit` names the sense of the command: `Normal` or `Inverse`. It lives
beside `WindowTree` in `kvim-ui`. `kvim-tui` re-exports it, so no present
consumer of `kvim_tui::AdaptiveSplit` breaks.

One rule comes before the ratio: when the tree holds exactly one window, the
adaptive split always selects a vertical split. A full-width host area would
otherwise divide into two short windows. The reference configuration uses the
same exception.

The inverse adaptive split command mirrors that decision. It selects a
horizontal split when the width exceeds the height multiplied by the same ratio.
Otherwise it selects a vertical split.

`kvim-ui` depends on no other kvim crate above `kvim-keymap` and `kvim-fuzzy`,
so `WindowTree::adaptive_orientation` takes the ratio as a plain `f32`, not as
`kvim_settings::SplitRatio`. A ratio that is not finite, zero, or negative
falls back to the neutral ratio 1.0, so the rule always answers one defined
orientation. Without that fallback, a comparison against a value such as `NaN`
would silently answer `false` on every comparison and always select
`Horizontal`.

`Windows::adaptive_orientation`, in `kvim-tui`, reads `EditorSettings::adaptive_split_ratio`
and calls `WindowTree::adaptive_orientation` with the validated ratio. It keeps
its present signature, so the standalone editor and every present caller stay
unchanged. The adaptive ratio belongs to `EditorSettings`.

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

## List Viewport

`ListWindow::reconciled` owns the one scroll rule of every bounded list of
`kvim-ui`, and it is the only copy of that rule. It is a pure function: it
takes the measure of every item, the position of the selected item, a viewport
height, a scroll margin, and the first visible line of the previous answer, and
it returns one `ListWindow`. It stores nothing.

`ListViewport` is the stateful shell over that rule. It holds a viewport height
in terminal rows, a scroll margin, and the last answer. It holds no item: the
caller owns every item value, and `reconcile` takes the measure of each item and
the position of the selected one. `reconcile` calls `ListWindow::reconciled`
with the stored height, the stored margin, and the offset that the previous
answer left behind, then keeps the result. The stored rule and the pure rule can
therefore never disagree, because one calls the other.

One `ListItem` is the measure of one item: the terminal lines it occupies, and
whether it is visible. A list of one line for each item builds every item with
`ListItem::single`, so a uniform list is the simple case of the same rule, not a
second rule. The viewport publishes one entry point for both cases, because two
entry points would let the two rules drift apart.

The viewport takes the visibility of each item instead of a list of visible
items alone. A collapsed sidebar subtree and a collapsed sidebar section both
hide items, and the caller indexes its own list by position. One index space
therefore serves the selection, the placements, and the caller's own list, and
no mapping table stands between them. A hidden item contributes no line to the
total, to the scroll margin, or to the placements.

`reconcile` moves the window until it shows the selection, then publishes one
`ListPlacement` for each item that the window shows. A placement names the
position of the item, its first visible line, how many lines the window shows,
and its offset from the top of the window. It carries no host identity, so one
placement shape serves every list. A caller that names its rows wraps the
placement with its own identity, the way `SidebarPlacement<R>` does.

The margin stops at half the window, so a short window still shows the selected
item. The window stops at the last line of the list, so it never scrolls past
the items to satisfy a margin that no item can fill. An item that is taller than
the window shows its first line. The reconciled offset always shows the selected
item, and a debug assertion holds that invariant.

`LIST_VIEWPORT_LINES_MAX` bounds the total line count at 1048576. The bound
keeps every sum of the offset rule inside `u32`. Each caller bounds its own list
first, and every present bound stays well below it.

Call `reconcile` after every change of the items, the selection, the height, or
the margin. The placements describe the state of the last reconciliation.

### The Window Of A Shared List

The geometry of a bounded list is known at draw time, from the rectangle of the
frame. `set_height_rows` takes `&mut self`, so a host that reaches the window
only through the stored viewport must mutate before it reads. A host whose frame
builder holds its state by shared reference cannot do that without a mutable
borrow across the whole frame.

`Selector::window_for_height` and `SidebarState::window_for_height` therefore
answer one window through `&self`, for a height and a margin that the caller
supplies at draw time. Each one calls `ListWindow::reconciled` and names every
returned placement with its own placement type. Neither host writes arithmetic
of its own.

The two names divide the two questions. `placements` and `first_line` answer the
window that the list stored, at the height that the list stored.
`window_for_height` answers the window of the height that the caller names now,
and stores nothing.

Both answers start from the same offset: the first visible line that the list
stored. That choice keeps two properties. The answer for the stored height and
the stored margin repeats the stored window exactly, because the offset rule is
idempotent. And the answer stays sticky, so the window keeps its offset while
the selection stays inside the margin.

The cost is explicit. A host that never calls `set_height_rows` leaves the stored
offset at zero, and then every answer is the smallest offset that satisfies the
margin. That window always shows the selection, but it steps between the top and
the bottom of the area instead of scrolling with the selection. A host that wants
the scrolling answer calls `set_height_rows` once, when it learns the height, and
then reads `window_for_height` at that same height for every frame.

`ListWindow::reconciled` allocates one vector of placements for each call. The
vector holds at most one placement for each terminal row of the window, so the
allocation stays bounded by the height of the terminal. `ListViewport::reconcile`
takes the same allocation instead of reusing its previous buffer. One call runs
for each input event or frame, so the cost is one small allocation per event.

### List Motion

`ListMotion` is the one motion type that `SidebarState` and `Selector` both
answer. It replaces `SidebarMotion`, which is gone from the public surface.
The rename is a clean break: no alias remains, because both crates stay
below version 1.0 and the one embedding host upgrades deliberately. A host
that read `SidebarMotion` renames its import to `ListMotion`. The four
variants and their sidebar behavior stay the same.

`ListMotion` holds four variants. `Down(usize)` and `Up(usize)` move by a row
count and stop at both edges. `ToRow(usize)` moves to a named row. `LastRow`
moves to the last row. No variant wraps.

`ToRow` names a row inside the row space of the list that receives the
motion, and the two present lists keep different row spaces. `SidebarState`
indexes its complete flat row list, hidden rows included. A hidden target
resolves like an inert row: to the nearest selectable row in the direction
of travel, then to the nearest one behind it. `Selector` indexes `matches`
instead, the row space that `selected_row` and `SelectorPlacement::index`
also use, so `ToRow` never resolves to a row that the current query drops.
The same index value can therefore name two different rows, or no row at
all, across the two lists. A host that reuses one `ToRow` value across both
lists reads the row space of each list first.

`Selector::apply_motion` answers `ListMotion` the same way
`SidebarState::reduce` answers a `SidebarInput::Move`. `select_next` and
`select_previous` stay as public methods, now as thin wrappers over a
single-row `ListMotion::Down` and `ListMotion::Up`, so a present host keeps
its call sites unchanged.

## Sidebars

`SidebarState<RowId>` owns selection and viewport state only. Rows, actions,
styles, labels, and semantic meaning are borrowed host inputs. Each row supplies
a bounded, variable height in terminal rows.

The sidebar holds one `ListViewport` and writes no offset rule of its own. It
hands the viewport one `ListItem` for each row, with the height of the row and
whether a collapsed ancestor or a collapsed section hides it, and it names each
returned `ListPlacement` with the host identity of its row. `set_height_rows`,
`set_scroll_margin`, `first_line`, `total_lines`, and `height_rows` all read or
write that one viewport, and `window_for_height` answers a window of any height
through a shared reference. See [List Viewport](#list-viewport).

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

### Tree Rows

`SidebarRow<R>` carries a depth and a collapsed flag. The depth is the
distance of the row below the root of its tree. The root row of a tree holds
depth 0. `SIDEBAR_ROW_DEPTH_MAX` bounds the depth at 16, so a row that names a
deeper tree makes `set_rows` return an error instead of an unbounded guide
string.

The row list stays flat. A row's parent is the nearest earlier row of a
strictly smaller depth. `SidebarState` computes which rows are visible once,
when `set_rows` runs, and stores the result. Every later read uses the stored
result. A row is hidden when an ancestor, transitively, carries the collapsed
flag. A collapsed row stays visible itself; only the rows below it are
hidden.

A hidden row composes with the inert-row rule instead of replacing it. A row
takes the selection only when it is visible and its kind is `Selectable`.

`Down` and `Up` count visible rows only, so a move over a collapsed parent
lands on the next visible row, at or above the depth of the collapsed parent.
`ToRow` and `LastRow` address visible rows only. A hidden target resolves
like an inert row does: to the nearest selectable row in the direction of
travel, then to the nearest one behind it. Neither motion wraps.

`rows()` still returns the complete flat list, hidden rows included, because
the host indexes into that list and needs those indexes to stay stable. Use
`placements()` for the visible rows alone.

A hidden row contributes no line to the total line count, to the scroll
margin, or to `placements()`.

### Tree Guides

`sidebar_guides` draws the indent guides of one tree row: a trunk for a level
that holds a further row below, an elbow for the last row of a level, and a
blank guide otherwise. The rule starts at depth 1, so a top-level row of a
tree carries no guide of its own.

A host calls `sidebar_guides` for a visible row only, the same row it draws.
A collapsed subtree then changes no guide, and the scan needs no visibility
test. Every row that the scan reads holds the depth of the closing level or a
deeper one, and every row that a collapse hides sits deeper than the ancestor
that hides it. A hidden row therefore never holds the depth that closes the
level. The scan allocates nothing beyond the returned string, so a host that
calls `sidebar_guides` once for every drawn row, every frame, pays for that
string alone.

The file tree and the changes panel of the diff view drew this exact rule
twice before `kvim-ui` published it. The two copies read alike but were not
identical, because the file tree draws one header row above the workspace
root. Its top-level rows sit below that header and carry no sibling that
could ever close a guide, so the file tree prepends one blank guide of its
own before the shared result. The changes panel has no such header, so its
top-level rows take the shared result unchanged. `sidebar_guides` never adds
the leading blank itself, because that blank is a fact of the file tree's own
header, not a fact of the shared rule.

### File Tree Collapse Ownership

`FileTree`, in `kvim-workspace`, and `SidebarState`, in `kvim-ui`, can each
hide a collapsed subtree. `FileTree` owns the expansion set. `SidebarState`
hides a subtree whose row carries the collapsed flag. Two owners of one
truth can disagree, so the file tree names one owner: `FileTree` alone.

`FileTree` withholds the rows of a collapsed directory from `tree.rows()`
itself, because it also drives the directory reads. It cannot emit a row for
a directory that it has never read. `crates/kvim-tui/src/tree.rs` therefore
builds every `SidebarRow<usize>` from a row list that already excludes the
hidden rows, and it never calls `SidebarRow::with_collapsed`. The collapsed
flag of every file tree row stays `false`, the default that
`SidebarRow::single` sets, so `SidebarState`'s own hiding rule finds nothing
to hide and stays inert for this host. The file tree therefore keeps one
owner of the truth, and the sidebar's collapsed flag is not a forgotten
wire; a later reader who adds `with_collapsed` to the file tree would hide a
row twice, not once.

The changes panel of the diff view carries no expansion set of its own. Its
directory rows always show every file below them, so its rows never carry
the collapsed flag either, and the question of an owner does not yet arise
for that host.

### Sidebar Sections

A section is a second axis over the same flat row list, not a nested
container. `SidebarRow<R>` carries a section index alongside its depth, set
through `with_section`. The row list stays ordered by section: every row of
section 0 precedes every row of section 1. A depth still counts from the
root of a row's own tree, inside its own section, so a further section
simply restarts the tree at depth 0. No row of one section is the ancestor of
a row of the next one, so a section boundary closes every open subtree of the
section above it. A host that starts a section below depth 0 therefore still
hides no row of it behind a collapsed row of the previous section.

`SidebarState<R>` holds the collapsed flag of every section as one bounded,
ordered list, set through `set_sections`. `SIDEBAR_SECTIONS_MAX` bounds the
list at 64, and a longer list makes `set_sections` return
`SidebarError::Sections` instead of an unbounded state, exactly as
`SIDEBAR_ROWS_MAX` bounds `set_rows`. A refused list leaves the previous
sections and the previous selection in place. A section index that no
declared section covers counts as not collapsed, so a sidebar that never
calls `set_sections` hides no row through this axis, and every present
consumer keeps its current behavior without a change.

`sidebar_visibility` is the one function that decides row visibility. It
folds the section rule into the same pass as the depth-collapse rule: a row
is hidden when a collapsed ancestor hides it, exactly as before, or when its
own section is collapsed. A row inside a collapsed section is hidden
regardless of its own subtree state, because the two rules combine with a
logical and. `set_rows` and `set_sections` both change one input of that
one function and share the same recovery afterward: the visibility list, the
total line count, and the selection all recompute, and a selection that a
new collapse hides moves to the nearest visible, selectable row, the same
rule that a collapsed tree row already follows.

A section carries no header row of its own. `kvim-ui` publishes no drawn
text for a section, the same way it publishes none for a tree row, so a host
that wants a visible heading draws an ordinary row for it, at depth 0 in
that section, and gives that row its own meaning. The changes panel of the
diff view is the first consumer: `ChangesRow::Directory` and `ChangesRow::File`
each carry a `ChangeSection`, ready to become a `with_section` index for a
host that wants to show the staged and the unstaged files together. That
conversion is a further change. `kvim-ui` publishes the mechanism alone.

## Selector

`Selector<R>` owns a bounded query, a bounded candidate list, a ranked match
list, and a selection, generic over one opaque host identity `R`. It names no
path, no buffer, and no file. A host offers each candidate as a name and a
container string, and the selector ranks the two through `kvim-fuzzy`, the
same way that `kvim_workspace::Picker` ranks a file candidate against its name
and its directory. A tied score breaks by the combined character width of the
name and the container, then by the earlier candidate of the source list.

The candidate list stops at `SELECTOR_CANDIDATES_MAX`, which is 4096, and a
longer list reports the truncation. The query stops at
`SELECTOR_QUERY_CHARS_MAX`, which is 128 characters. Both bounds match the
bounds that `PICKER_CANDIDATES_MAX` and `PICKER_QUERY_CHARS_MAX` state in
`kvim-workspace`.

The selection follows its candidate across one refiltering while the query
still matches it, so a further character never moves the reader to another
row. A query that drops the selected candidate falls back to the best
remaining match. A move past the first or the last matched row stays on that
row, because a wrap would move the reader past the best match without a key
that says so.

`Selector` holds one `ListViewport` over its matched rows, the same window
that `SidebarState` holds. See [List Viewport](#list-viewport). Every matched
row holds one line, so `set_height_rows` hands the window one `ListItem::single`
for each row of `matches`. `height_rows`, `set_height_rows`, `scroll_margin`,
`set_scroll_margin`, `first_line`, and `total_lines` all read or write that one
window. `placements` returns one `SelectorPlacement` for each row the window
shows, so a host paints a bounded selector without computing an offset of its
own.

`SelectorPlacement::index` names the position of the row inside `matches`, the
row space that `selected_row` also answers. It names no position inside the
candidate list. `SelectorPlacement::candidate_index` names that position
instead, so a host passes it to `Selector::candidate` and reaches the matched
candidate directly, with no further lookup through `matches`.

`candidates_len` returns the number of candidates that the selector holds, not
the number of matched rows. A host reads this count to tell two empty cases
apart. Zero names a list with no candidate at all. A positive count beside an
empty `matches` names a query that keeps nothing.

`apply_motion` answers every `ListMotion` over the row space of `matches`, so
a host picker reaches the last row, jumps to a row, and moves by a count,
exactly as a host sidebar does. See [List Motion](#list-motion).

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
presentation consumes hints from the active shared resolver. A hint list that
outgrows the frame holds one page for each frame of columns, and the render
reports the page it drew as one `WhichKeyPlacement`. See
[`input-actions.md`](input-actions.md).

Every render validates that its rectangle fits the supplied
`ratatui::Buffer`. Invalid geometry returns a typed error before any cell
changes. Rendering performs no input or output and writes only inside its area.

`WhichKeyOverlay::render` therefore returns
`Result<WhichKeyPlacement, WhichKeyError>` and reports `WhichKeyError::Area`,
and `SidebarState::render` reports
`SidebarError::Area`, for a rectangle that names one cell outside the buffer.
An empty rectangle names no cell, so every buffer accepts it. The check runs
before the first write, because `ratatui::Buffer` panics on a cell outside its
own rectangle, and a host that keeps a stale rectangle must read a typed error
instead of a stopped process. One crate-private `fits` function in
`crates/kvim-ui/src/layout.rs` owns the check, so both widgets and every later
widget answer the same question once.

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
