# Settings

## Ownership

The `settings` module owns the `EditorSettings` structure and every default
value in it. `settings` depends on no other module. Every other module may
depend on `settings`.

`EditorSettings` is the single owner of every adjustable value. No other module
holds an adjustable constant. A module that needs an adjustable value reads it
from the injected `EditorSettings` value.

A fixed safety bound is not an adjustable value. Runtime limits, picker limits,
analysis limits, and protocol limits stay in their owning documents:
[`responsiveness.md`](responsiveness.md), [`files.md`](files.md), and
[`language-services.md`](language-services.md).

The defaults match the reference Neovim setup at `~/.config/nvim`.

## Structure

`EditorSettings` contains one group for each subject below. A group is a typed
structure, not a flat list of primitives.

## Display

| Field | Default |
|---|---|
| Line numbers | Enabled |
| Relative line numbers | Enabled |
| Line wrapping | Disabled |
| Vertical scroll margin | 2 rows |
| Horizontal scroll margin | 4 columns |
| Sign column | Always visible |

[`windows.md`](windows.md) owns the presentation behavior for these fields.

## Indent

| Field | Default |
|---|---|
| Expand tab to spaces | Enabled |
| Tab width | 4 columns |
| Shift width | Follows the tab width |

[`text-model.md`](text-model.md) owns the indent policy.

## Search

| Field | Default |
|---|---|
| Case sensitivity | Smart case |
| Highlight matches | Enabled |

Case sensitivity is one mode with three values: sensitive, insensitive, and
smart case. Smart case ignores the case until the query contains an uppercase
character. One mode replaces the reference pair of ignore-case and smart-case
flags, because that pair can express a state that has no meaning.

## Windows

| Field | Default |
|---|---|
| Horizontal split placement | Below |
| Vertical split placement | Right |
| Adaptive split ratio | 2.5 |
| Resize step | 6 cells |
| Minimum window width | 20 cells |
| Minimum window height | 3 rows |
| File tree width | 40 cells |
| File tree icons | Shown |

[`windows.md`](windows.md) owns the split, focus, and resize behavior. The file
tree width is the width of the right sidebar when it opens. A directional resize
toward the sidebar changes the width of the open sidebar, not this default.

The file tree icons setting is a mode with two values: shown and hidden. An icon
needs a patched font, and the reference configuration installs one, so the
default shows the icons. A terminal without a patched font hides them, and the
tree still aligns. [`files.md`](files.md) owns the icon table.

## Files

| Field | Default |
|---|---|
| Persistent undo file | Enabled |
| Format on save | Enabled |
| Atomic save | Enabled |
| Maximum file size | 4 MiB |

[`files.md`](files.md) owns saving, conflicts, and persistent undo files. Format
on save is the default for each new buffer. The per-buffer toggle does not
change this default. See [`language-services.md`](language-services.md).

## Input

| Field | Default |
|---|---|
| Which-key delay | 500 ms |
| Count maximum | 9,999 |
| Pending keys maximum | 4 keys |

A pending key sequence has no timeout, so this table holds none. The sequence
waits for the next key until `Esc`, `Ctrl-C`, a mismatch, a completed command, or
a mode change ends it.

[`input-actions.md`](input-actions.md) owns modes, commands, and sequence
resolution. The resolver never reads a clock. The event loop supplies elapsed
time and compares it with the which-key delay.

## Language

| Field | Default |
|---|---|
| Check depth | Lints |
| Diagnostics | Enabled |

The check depth is language neutral. It selects the compile check or the
extended lint check of a language. Each language adapter maps the mode onto the
option of its own server, so no setting names one server. The Rust adapter maps
the lint check onto `clippy` and the compile check onto `check`.

[`language-services.md`](language-services.md) owns the language adapter
boundary and the language server session.

## Notifications

| Field | Default |
|---|---|
| Overlay rows | 16 rows |
| Spinner period | 1 s |
| Finished item lifetime | 2 s |

The notification overlay shows the work-done progress of every language server,
and nothing else. The row bound drops the oldest row above it. The spinner period is the time of one complete spinner cycle, and the
overlay divides it by the number of spinner frames. The lifetime is the time
that a finished item stays visible.

The defaults match the reference `fidget.nvim` configuration.
[`language-services.md`](language-services.md) owns the overlay, and
[`responsiveness.md`](responsiveness.md) owns the deadline that drives it.

## Theme

| Field | Default |
|---|---|
| Base color | `#111317` |
| Surface color | `#161a20` |

The palette is tokyonight night with these two darkened overrides. Every other
palette value comes from the tokyonight night palette.
[`windows.md`](windows.md) owns the semantic theme roles.

## Typed Values

Each field carries its mode and its unit in the type system. A field is not a
bare primitive:

- A duration is a duration value, not a number of milliseconds.
- A width, a margin, and a resize step are cell counts, not plain integers.
- The adaptive split ratio is a validated positive ratio.
- The shift width is a mode with a variant that follows the tab width and a
  variant that holds an explicit width. This makes an inconsistent pair of
  widths unrepresentable.
- The sign column, the case sensitivity, the split placements, the file tree
  icons, and the check depth are modes, not boolean flags or strings.

A constructor validates each value and establishes its invariant. An invalid
value cannot exist.

## Configuration Loading

The first release does not parse a configuration file. `EditorSettings` uses its
defaults for every field.

A later slice adds a loader that overrides these fields. The loader parses,
validates, and realizes user values, then constructs the same typed
`EditorSettings` value. Every field in this document must stay overridable by
that loader. Do not add a value that only a loader could set, and do not add a
value outside `EditorSettings`.

## Deferred Decisions

- Slice 7 must confirm the minimum window dimensions against the implemented
  layout.
