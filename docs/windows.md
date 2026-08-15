# Windows And Presentation

## Ownership

The `tui` module owns the window tree, layout, focus, resize, rendering, and the
theme. It is the sole owner of visible editor state. See
[`architecture.md`](architecture.md).

The window tree contains no buffer text and no terminal colors. It contains
window identities, split structure, and validated dimensions.

## Window Tree

The window tree is a binary tree with two node kinds:

- A leaf window shows one buffer. It has a stable window identity, a viewport
  offset, and a cursor position.
- A split node has an orientation and two children. A horizontal split node
  stacks its children top and bottom. A vertical split node places its children
  left and right.

A window identity stays stable while the window exists. Splitting, resizing,
closing a sibling, and resizing the terminal never change an existing identity.
Focus, buffer association, and viewport state follow the identity.

Closing a window replaces its parent split node with the remaining sibling. The
tree always has at least one leaf window.

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

The new window shows the same buffer as the source window. The new window
receives focus.

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

A resize that would push any window below its minimum dimensions leaves the
layout unchanged.

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
rows. It keeps a winbar row, one text row, and a statusline row visible. Both
values belong to `EditorSettings`. See [`settings.md`](settings.md).

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

## Theme

The theme maps semantic roles to terminal styles. Call sites request a role,
such as normal text, selection, search match, line number, active line number,
window title, status text, or a syntax role. Call sites never name a raw color.
Only the theme holds color values.

The palette is tokyonight night with a darkened base color `#111317` and a
surface color `#161a20`. Both values belong to `EditorSettings`. See
[`settings.md`](settings.md). Every other palette value comes from the
tokyonight night palette. This document does not restate those values.

Window titles use a color that is distinct from every syntax role, so a title
never reads as code. The focused window title is emphasized. Other window titles
use the dimmed text color on the same surface band.

Syntax roles are terminal-independent at the language boundary. The interface
layer maps them to theme roles. See
[`language-services.md`](language-services.md).

## Buffer Presentation

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
