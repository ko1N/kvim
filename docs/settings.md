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

[`windows.md`](windows.md) owns the split, focus, and resize behavior.

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
| Sequence timeout | 750 ms |
| Which-key delay | 200 ms |
| Count maximum | 9,999 |
| Pending keys maximum | 4 keys |

[`input-actions.md`](input-actions.md) owns modes, commands, and sequence
resolution. The resolver never reads a clock. The event loop supplies elapsed
time and compares it with these values.

## Language

| Field | Default |
|---|---|
| rust-analyzer check command | `clippy` |
| Diagnostics | Enabled |

[`language-services.md`](language-services.md) owns the language adapter
boundary and the `rust-analyzer` session.

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
- The sign column, the case sensitivity, the split placements, and the
  rust-analyzer check command are modes, not boolean flags or strings.

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

- The undo history bound is not yet decided. Slice 4 must record it here. See
  [`text-model.md`](text-model.md).
- Slice 4 must confirm the maximum file size against the selected text storage.
- Slice 7 must confirm the minimum window dimensions against the implemented
  layout.
