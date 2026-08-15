# Input And Actions

## Ownership

The `input` module owns editor modes, semantic commands, the mapping registry,
the bounded sequence resolver, and which-key generation. The `terminal` module
decodes raw terminal events into normalized keys. The `editor`, `workspace`, and
`tui` modules consume semantic commands.

State and view code must never compare raw keys. A raw key exists only inside
`terminal` and inside the mapping registry.

## Editor Modes

Kvim has five modes:

- Normal: motions, operators, and commands act on the buffer.
- Insert: printable keys insert text through edit transactions.
- Visual: a characterwise selection follows the cursor.
- Visual Line: a linewise selection follows the cursor.
- Visual Block: a rectangular selection follows the cursor.

The mode is one typed value. A mode change resets pending input. Each Visual
mode keeps its own selection anchor. `Esc` returns to Normal mode from every
other mode.

The command line is an input context, not a mode. See the section below.

## Command Line

`:` opens the command line from Normal mode. The command line reads one line of
text and runs one command. Kvim does not implement the Ex grammar. It accepts
this fixed set only:

| Command | Effect |
|---|---|
| `:w` | Save the active buffer |
| `:q` | Close the focused window |
| `:q!` | Close the focused window and discard unsaved changes |
| `:wq` | Save the active buffer, then close the focused window |
| `:e <path>` | Open one file in the focused window |
| `:<number>` | Move the cursor to that line |

The command line rejects every other input with a concise message. It does not
guess a command from a prefix.

`Esc` cancels the command line and restores the previous mode. The command line
holds a bounded query length. `:w` and `:e` use the same save and open path as
their bound keys. See [`files.md`](files.md).

## Semantic Commands

A semantic command describes intent, such as move the cursor, delete a range,
paste a register, save the buffer, focus a window, resize a window, open a
picker, toggle a comment, or go to a definition. A command carries typed
arguments, such as a count or a motion, never a key value.

Each command has a stable identifier and a short label. The which-key overlay
and any help output derive their text from these labels.

## Mapping Registry

The mapping registry maps a key sequence to a semantic command. The registry is
keyed by mode. One key sequence can appear in several modes with different
commands, because only one mode is active.

The registry validates itself at construction. It rejects a duplicate sequence
inside one mode. It rejects a sequence that is also a strict prefix of another
sequence in the same mode when both would resolve, because such a pair makes
resolution ambiguous. It rejects an empty sequence and a sequence longer than
the pending-key maximum.

The first release ships one hardcoded registry. It does not parse a
configuration file. See [`settings.md`](settings.md) for that rule.

## Counts And Sequences

The resolver accepts an optional decimal count before a command sequence. The
count maximum is 9,999. A count above the maximum is a mismatch and resets
pending input.

A count belongs to Normal mode and the three Visual modes. Insert mode holds no
count, because a digit is buffer text there. The `input` module owns this rule,
so no other module filters digit keys.

The resolver classifies each pending sequence as a complete match, a valid
prefix, or no match. A pending sequence holds at most four keys.

A pending sequence has a deadline. The deadline is 750 ms by default. The
resolver is clock-independent: it never reads a clock. The event loop supplies
the elapsed time with each resolution request and reports deadline expiry. This
keeps resolution deterministic and testable.

The count maximum, the pending-key maximum, and the sequence timeout belong to
`EditorSettings`. See [`settings.md`](settings.md).

## Leader And Which-Key

Space is the leader key in Normal, Visual, and Visual Line modes. Space starts a
pending sequence. It does not insert a space in these modes. In Insert mode
Space inserts a space.

The which-key overlay lists the commands that the current pending sequence can
still reach. It is generated from the command labels in the active registry for
the active mode. It is never a separate hand-written list.

The overlay appears after the which-key delay of 200 ms. The delay belongs to
`EditorSettings`. The event loop supplies that elapsed time, using the same rule
as the sequence deadline. A completed or reset sequence hides the overlay.

[`windows.md`](windows.md) owns overlay placement and styling.

## Reset Rules

Pending input resets after:

- a completed command,
- a sequence mismatch,
- sequence timeout,
- a mode change,
- editor shutdown.

A reset clears the pending keys, the pending count, and the which-key overlay.
A reset never changes buffer text and never cancels a running background
request.

Input processing stays on the terminal event loop. It must remain responsive
while background work runs. See [`responsiveness.md`](responsiveness.md).

## First-Release Bindings

This table is the complete first-release binding set. Slices 5 and 6 name the
motion, operator, register, paste, undo, redo, search, and dot-repeat subset.
Where the plan names a command without naming its key, this table uses the
standard Vim key that the ReviewGraph Vim preset already documents in
`../reviewgraph/docs/input-actions.md`. Slice 3 must confirm each such key when
it builds the registry.

### Modes

| Keys | Command | Modes |
|---|---|---|
| `i` | Insert before the cursor | Normal |
| `I` | Insert at the first non-blank character | Normal |
| `a` | Insert after the cursor | Normal |
| `A` | Insert at the end of the line | Normal |
| `o` | Open a line below and insert | Normal |
| `O` | Open a line above and insert | Normal |
| `v` | Enter Visual mode | Normal |
| `V` | Enter Visual Line mode | Normal |
| `Ctrl-V` | Enter Visual Block mode | Normal |
| `:` | Open the command line | Normal |
| `Esc` | Return to Normal mode | Insert, Visual, Visual Line, Visual Block |

Every motion row below also applies in Visual Block mode. The tables name the
three Visual modes separately only where their behavior differs.

### Motions

| Keys | Command | Modes |
|---|---|---|
| `h` | Move left | Normal, Visual, Visual Line |
| `j` | Move down | Normal, Visual, Visual Line |
| `k` | Move up | Normal, Visual, Visual Line |
| `l` | Move right | Normal, Visual, Visual Line |
| `w` | Move to the next word start | Normal, Visual, Visual Line |
| `b` | Move to the previous word start | Normal, Visual, Visual Line |
| `e` | Move to the next word end | Normal, Visual, Visual Line |
| `0` | Move to the first column | Normal, Visual, Visual Line |
| `^` | Move to the first non-blank character | Normal, Visual, Visual Line |
| `$` | Move to the end of the line | Normal, Visual, Visual Line |
| `gg` | Move to the first line | Normal, Visual, Visual Line |
| `G` | Move to the last line, or to the count line | Normal, Visual, Visual Line |
| `Ctrl-D` | Move down one half page | Normal, Visual, Visual Line |
| `Ctrl-U` | Move up one half page | Normal, Visual, Visual Line |
| `Ctrl-F` | Move down one full page | Normal, Visual, Visual Line |
| `Ctrl-B` | Move up one full page | Normal, Visual, Visual Line |
| `zz` | Center the cursor line in the window | Normal, Visual, Visual Line |
| `zt` | Align the cursor line to the window top | Normal, Visual, Visual Line |
| `zb` | Align the cursor line to the window bottom | Normal, Visual, Visual Line |

A decimal count before a motion repeats it.

### Operators, Registers, And Repeat

| Keys | Command | Modes |
|---|---|---|
| `d` | Delete over a motion | Normal |
| `c` | Change over a motion | Normal |
| `y` | Yank over a motion | Normal |
| `d` | Delete the selection | Visual, Visual Line, Visual Block |
| `c` | Change the selection | Visual, Visual Line, Visual Block |
| `y` | Yank the selection | Visual, Visual Line, Visual Block |
| `I` | Insert before every selected line, at the block left edge | Visual Block |
| `A` | Insert after every selected line, at the block right edge | Visual Block |
| `dd` | Delete the current line | Normal |
| `cc` | Change the current line | Normal |
| `yy` | Yank the current line | Normal |
| `D` | Delete to the end of the line | Normal |
| `C` | Change to the end of the line | Normal |
| `Y` | Yank the current line | Normal |
| `p` | Paste after the cursor | Normal, Visual, Visual Line |
| `P` | Paste before the cursor | Normal, Visual, Visual Line |
| `u` | Undo one transaction | Normal |
| `Ctrl-R` | Redo one transaction | Normal |
| `.` | Repeat the last repeatable change | Normal |

Visual paste replaces the selection and preserves the source register.

### Search

| Keys | Command | Modes |
|---|---|---|
| `/` | Open the search prompt | Normal |
| `n` | Move to the next match | Normal |
| `N` | Move to the previous match | Normal |

Search uses smart-case matching. See [`settings.md`](settings.md).

### Visual Selection

| Keys | Command | Modes |
|---|---|---|
| `J` | Move the selection down one line and keep it | Visual, Visual Line, Visual Block |
| `K` | Move the selection up one line and keep it | Visual, Visual Line, Visual Block |
| `<` | Shift the selection left one shift width and keep it | Visual, Visual Line, Visual Block |
| `>` | Shift the selection right one shift width and keep it | Visual, Visual Line, Visual Block |

A block operator applies to each selected line inside the block columns. A line
that is shorter than the block left edge receives no change. Block insert applies
one edit transaction, so one undo reverses the whole block.

### Files And Buffers

| Keys | Command | Modes |
|---|---|---|
| `Ctrl-S` | Save the active buffer | Every mode |
| `Ctrl-E` | Reveal the active file in the file tree | Normal |
| `Space o` | Open the buffer picker | Normal |
| `Space fb` | Open the buffer picker | Normal |
| `Space x` | Unload the active buffer | Normal |
| `Space ff` | Open the file search picker | Normal |
| `Space f/` | Open the ripgrep search picker | Normal |

`Ctrl-S` saves without forcing an unrelated mode transition.

### Windows

| Keys | Command | Modes |
|---|---|---|
| `Ctrl-H` | Focus the window to the left | Normal |
| `Ctrl-J` | Focus the window below | Normal |
| `Ctrl-K` | Focus the window above | Normal |
| `Ctrl-L` | Focus the window to the right | Normal |
| `Ctrl-Alt-H` | Resize the window six cells to the left | Normal |
| `Ctrl-Alt-J` | Resize the window six cells downward | Normal |
| `Ctrl-Alt-K` | Resize the window six cells upward | Normal |
| `Ctrl-Alt-L` | Resize the window six cells to the right | Normal |
| `Space Enter` | Split the window with the adaptive rule | Normal |
| `Ctrl-Enter` | Split the window with the adaptive rule | Normal |
| `Space \` | Split the window with the inverse adaptive rule | Normal |
| `Ctrl-\` | Split the window with the inverse adaptive rule | Normal |
| `Space q` | Close the focused window | Normal |
| `Ctrl-Q` | Close the focused window | Every mode |

The terminal requests enhanced keyboard reporting so `Ctrl-Alt`, `Ctrl-Enter`,
and `Ctrl-\` chords stay distinct. A terminal that cannot report one chord leaves
the leader form usable. See [`windows.md`](windows.md) for the resize model and
the adaptive split rule.

### Language Services

| Keys | Command | Modes |
|---|---|---|
| `Space /` | Toggle the comment on the current line | Normal |
| `Space /` | Toggle the comment on the selected lines | Visual, Visual Line |
| `gd` | Go to the definition | Normal |
| `Space k` | Show hover information | Normal |
| `Space e` | Show the diagnostic float | Normal |
| `]d` | Move to the next diagnostic | Normal |
| `[d` | Move to the previous diagnostic | Normal |
| `Space cf` | Toggle format-on-save for the active buffer | Normal |

See [`language-services.md`](language-services.md) for the behavior behind these
commands.
