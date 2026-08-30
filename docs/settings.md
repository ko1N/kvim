# Settings

## Ownership

`kvim-settings` owns the standalone `EditorSettings` structure and every
standalone default in it. It depends on no other kvim crate. Only the crate
dependency table in [`architecture.md`](architecture.md) decides which other
crates may depend on it.

`EditorSettings` is the single owner of adjustable standalone editor behavior.
A standalone module reads such a value from the injected `EditorSettings`.
Public library limits and policies use validated feature-specific configuration
types. A syntax-only, LSP-only, keymap, UI, or embedded consumer does not need to
construct `EditorSettings`.

A fixed safety cap is not an adjustable value. Runtime, picker, syntax, and
protocol caps stay in their owning documents. Public request and instance limits
can be lower validated values inside those caps. See
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
| Vertical scroll margin | 2 rows |
| Horizontal scroll margin | 4 columns |
| Sign column | Always visible |

[`windows.md`](windows.md) owns the presentation behavior for these fields.

## Diff

| Field | Default |
|---|---|
| View | Side by side |
| Smallest column width | 24 cells |

[`diff-view.md`](diff-view.md) owns the presentation behavior for these fields.
A window that cannot hold two columns of the smallest width draws inline
whatever the view field asks for.

## Indent

| Field | Default |
|---|---|
| Expand tab to spaces | Enabled |
| Tab width | 4 columns |
| Shift width | Follows the tab width |
| Indent width override | Follows the language |

A language adapter declares the width of one indent level for its language,
exactly as it declares its comment token. See
[`language-services.md`](language-services.md). `EditorSettings` resolves one
indent width for the active buffer, in this order:

1. The indent width override wins, when the user sets an explicit width.
2. Otherwise, the width that the language adapter declares wins.
3. Otherwise, the shift width applies, for a buffer that no adapter serves. The
   shift width follows the tab width by default, so the default resolution of
   such a buffer is the tab width.

The resolved width is one indent level. The automatic indent, the tab key, the
Visual `<` and `>` commands, and the formatting request of a language server
all step by it.

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
| Unsaved-edit recovery | Enabled |
| Maximum recovery record text | 4 MiB |
| Format on save | Enabled |
| Atomic save | Enabled |
| Maximum file size | 4 MiB |

The maximum file size and maximum recovery record text remain raw in
`FileSettings` so settings overrides can replace them before realization.
`EditorSettings::realize` validates both values. The recovery value must be
nonzero, must not exceed 4 MiB, and must not exceed the realized file-size
limit. A composition boundary gives each file-backed buffer both realized
limits. Recovery records therefore cannot retain more text than the buffer can
hold. The recovery setting applies only to file-backed `WorktreeEditor` buffers.
It has no effect on `MemoryEditor`.

[`files.md`](files.md) owns saving, conflicts, persistent undo files, and
unsaved-edit recovery. Format
on save is the default for each new buffer. The per-buffer toggle does not
change this default. The setting names no formatter: the language adapter
decides whether an external program or a language server formats the buffer.
See [`language-services.md`](language-services.md).

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

Kvim language adapters translate `EditorSettings` into neutral syntax and LSP
declarations. The public `kvim-syntax` and `kvim-lsp` APIs do not accept
`EditorSettings` and do not name a standalone setting.

## Mouse

| Field | Default |
|---|---|
| Wheel scroll distance | 3 rows |
| Sidebar double-click interval | 500 ms |

`MouseSettings::scroll_rows` and `MouseSettings::double_click_interval` are raw
until `EditorSettings::realize` runs. The scroll distance must be from 1 through
100 rows. The double-click interval must be greater than zero and at most two
seconds. These bounds limit pointer work and keep activation deterministic.

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

`EditorSettings` holds no color. The complete palette lives in
`crates/kvim-tui/src/theme.rs`, beside the semantic roles that use it, so a user
who recolors the editor edits one file and rebuilds.

This is a deliberate exception to the rule that every adjustable value belongs
to `EditorSettings`. A color is presentation data, not behavior, and splitting
the palette between two crates made the simple act of recoloring the editor
touch two files. The same reasoning already places the icon table of the file
tree in `kvim-tui`. [`windows.md`](windows.md) owns the palette, the semantic
roles, and the instructions for recoloring.

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
- The mouse scroll distance is a row count. Realization validates its nonzero
  bounded raw value before editor state exists.

A constructor validates each value and establishes its invariant. Public fields
do not bypass this boundary. Realization rejects zero resolver and window bounds,
file limits outside their supported range, a recovery limit above the file
limit, malformed linewise values, oversized
registers and edited-line seeds, and runtime capacities above their published
maximum. These checks run in release builds and return typed errors for invalid
consumer input. Debug assertions protect only invariants that a validated
boundary already established.

Rendering uses horizontal scrolling. The first release has no line-wrapping
setting because no wrapping architecture exists.

## Configuration Loading

The first release does not parse a configuration file. `EditorSettings` uses its
defaults for every field.

A later standalone release can add a loader that overrides these fields. The
loader parses, validates, and realizes user values. It then constructs the same
typed `EditorSettings` value. Every field in this document must stay overridable
by that loader. Do not add a value that only a loader could set. Do not add
standalone adjustable behavior outside `EditorSettings`.
