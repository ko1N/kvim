# Input And Actions

## Ownership

`kvim-keymap` owns terminal-neutral key values, generic bindings, the registry,
one bounded sequence resolver, dispatch ownership, and which-key hints.
`kvim-input` owns kvim commands, modes, prompts, counts, semantic pending state,
and the standalone binding preset. `kvim-terminal` converts crossterm events into
terminal-neutral keys. The editor, workspace, and TUI consume resolved commands.

State and view code must never compare crossterm events. Such an event exists
only inside `kvim-terminal`. Every other input boundary uses a
`kvim-keymap` value.

## Editor Modes

kvim has five modes:

- Normal: motions, operators, and commands act on the buffer.
- Insert: printable keys insert text through edit transactions.
- Visual: a characterwise selection follows the cursor.
- Visual Line: a linewise selection follows the cursor.
- Visual Block: a rectangular selection follows the cursor.

The mode is one typed value. A mode change resets pending input. Each Visual
mode keeps its own selection anchor. `Esc` and `Ctrl-C` both return to Normal
mode from every other mode, because the reference configuration maps `<C-c>` to
`<Esc>` in each of them.

The three Visual modes switch between each other. The key that enters a Visual
mode from Normal mode also switches into it from another Visual mode, and the
key of the active Visual mode returns to Normal mode:

| Key | Visual | Visual Line | Visual Block |
|---|---|---|---|
| `v` | Normal | Visual | Visual |
| `V` | Visual Line | Normal | Visual Line |
| `Ctrl-V` | Visual Block | Visual Block | Normal |

A switch between two Visual modes keeps the selection anchor. Only the shape of
the selection changes, so `V` in Visual mode selects the complete lines that the
existing anchor and cursor cover.

The command line is an input context, not a mode. The file-tree sidebar and the
confirmation are input contexts too. See the sections below.

## Binding Scopes

The mapping registry is generic over command identity, scope identity, and
surface identity. One binding names command ownership as host or surface. The
resolver evaluates the active overlay, host-global, and focused-surface scopes
in that order. A scope declares whether printable input falls back to typed text
and which owner receives it.

The editor context reports when `d`, `c`, or `y` waits for its target. Shared
dispatch still resolves motions, linewise operator commands, text-object starts,
and cancellation from the registry. The editor semantic reducer keeps the
operator through count and motion input. It does not resolve another key
sequence.

The sidebar scope holds no leader sequence. Count digits are resolved surface
commands, so its reducer can apply `5j` or `12G`. The sidebar owns focused
surface commands while it holds focus. Host-global and overlay scopes retain
their higher precedence.

The picker scope holds no count, because its typed-text fallback owns a digit.
The picker reads that query through one prompt, and its own table answers before
the query takes the key. The table holds the chords below alone, so every
printable key still reaches the query.

## Command Line

`:` opens the command line from Normal mode. The command line reads one line of
text and runs one command. kvim does not implement the Ex grammar. It accepts
this fixed set only:

| Command | Effect |
|---|---|
| `:w[rite]` | Save the active buffer |
| `:q[uit]` | Close the focused window, after a confirmation of unsaved changes |
| `:q[uit]!` | Close the focused window and discard unsaved changes |
| `:wq` | Save the active buffer, then close the focused window |
| `:e[dit] <path>` | Open one file in the focused window |
| `:e[dit]` | Read the file of the focused window again, after a confirmation of unsaved changes |
| `:e[dit]!` | Discard the unsaved changes of that buffer and read its file |
| `:l[ogs]` | Open one snapshot of the editor log in a new buffer |
| `:d[iagnostics]` | Open the host report of this machine in a new buffer |
| `:<number>` | Move the cursor to that line |

Each command declares one full name and the shortest abbreviation that names it.
The square brackets hold the optional part of the name, so `:q[uit]` accepts
`:q`, `:qu`, `:qui`, and `:quit`. Every length between the declared minimum and
the full name reaches the same command. The `!` variant follows the name, so
the parser accepts `:quit!` where it accepts `:q!`. This rule names what the
parser reads, never what the completion offers. The completion offers a `!`
variant only while the typed text already holds the `!`. See the Command Line
Completion section below.

The declared minimum names the command, and the shortest unique prefix does not.
`w` starts both `write` and `wq`. The minimum of `write` is one character, so
`:w` saves the buffer and never becomes ambiguous. `wq` declares its two
characters as the minimum and keeps its own name. `wq` has no longer name.

The declared minimum is a promise. A shorter minimum breaks no command line, but
a longer one breaks a command line that already works.

The command line rejects every other input with a concise message. It accepts an
abbreviation of a name in the table above only, so a name that no row declares
stays unknown.

`Esc` cancels the command line and restores the previous mode. An open candidate
list takes that key first, so the cancel of the line follows the cancel of the
list. See the Command Line Completion section below. The command line holds a
bounded query length. `:w` and `:e` use the same save and open path as their
bound keys. See [`files.md`](files.md).

`:q` and `:e` destroy data only while the buffer holds unsaved changes, so each
one asks only then. `:q` asks only while the focused window is the last window,
because another open window keeps the buffer and its changes. The question names
the buffer. The answer `y` performs the action, and every other answer keeps the
buffer and its unsaved changes. `:q!` and `:e!` ask nothing, and `:wq` asks
nothing, because the save keeps every change. See the Confirmation section below
and [`files.md`](files.md).

`:l[ogs]` opens one snapshot of the editor log in a new buffer. That buffer is an
ordinary scratch buffer. The user reads it with the normal motions, searches it,
yanks from it, edits it, and closes it. An edit of the buffer changes no log
entry, and a later report changes no open snapshot. A second `:logs` opens a
second buffer that holds the log of that later moment. The command reads editor
state only, so it performs no filesystem work. [`windows.md`](windows.md) owns
the log, its bounds, and the shape of one entry.

`:d[iagnostics]` opens the host report of this machine in a new buffer. The
report names every external program that kvim runs and whether the host holds
it. `kvim --diagnostics` prints the same report before the editor starts, and
one builder serves both. That buffer is an ordinary scratch buffer, as the log
buffer is.

The report reads the executable search path, which is filesystem work, so the
command starts one bounded background probe and the event loop reads no path.
The message line reports that the probe runs, and the editor stays fully usable
while it runs. The buffer opens when the probe answers. A second
`:d[iagnostics]` starts no second probe while one runs, and a failed probe
opens no buffer and reports the outcome. [`architecture.md`](architecture.md)
owns the report and its bounds.

### Command Line Completion

The command line completes a command name and the path argument of `:e[dit]`. It
reads these keys:

| Keys | Effect |
|---|---|
| `Tab` | Write the next candidate into the line |
| `Shift-Tab` | Write the previous candidate into the line |
| `Esc` | Close the candidate list and restore the typed text |
| `Ctrl-C` | Close the candidate list and restore the typed text |

The completion writes the selected candidate into the line, so the line always
shows the command that `Enter` runs. The candidate list holds a bounded number
of candidates, and one candidate is one command name or one path.
[`files.md`](files.md) names that bound.

The candidates stay anchored to the text that the user typed, so one cycle never
narrows them. A forward cycle past the last candidate wraps to the first one. A
backward cycle past the first candidate wraps to the last one. A `Shift-Tab` that
opens the completion selects the last candidate.

A text that matches more than one name opens a candidate list above the message
line. A text that matches one name writes that name and opens no list. A text
that matches no name changes nothing and reports nothing. See
[`windows.md`](windows.md).

The completion offers the full name of a command and never an intermediate
abbreviation, so one cycle shows the whole name and the list stays short. It
matches the full name by its prefix, so `q` offers `quit`, `e` offers `edit`,
and `w` offers `wq` and `write`. Neither `wq` nor `write` is a `!` variant, so
the prefix `w` offers both names.

The completion offers no `!` variant that the user did not type. A `!` variant
discards unsaved changes and asks nothing, so `Tab` on `q` must never reach
`quit!`. A `!` at the end of the typed text is a deliberate choice. The
completion then serves that text, so `q!` and `qu!` both offer `quit!`. The
completion offers a name that the parser accepts only, so a command without a
`!` variant never reaches the list with one. `w!` therefore offers no candidate.
A line number is no name, so a line of digits offers no candidate.

`Esc` closes an open candidate list and restores the text that the user typed. A
second `Esc` then cancels the command line. One typed character and one
`Backspace` both close the list and keep the line as it is shown, so the next
completion reads the new line. A key that the command line does not read changes
nothing and leaves the list open.

`crates/kvim-input/src/command_line.rs` holds the name table beside the parser,
so one new command needs one new row and no new completion code.

#### The Path Argument

A blank after the command name ends the name and opens its argument, so a line
with a blank completes a path instead of a name. Only `:e[dit]` takes a path
today, so every other line with a blank offers no candidate. `:e!` reloads the
buffer and takes no path either. The parser owns this rule beside the name
table, so the parser and the completion can never disagree.

The candidates are the workspace files that the file picker offers. The
completion ranks them with the scorer of the picker, so one fuzzy rule serves
both. `:e src/ma` therefore offers the same files in the same order as the
picker query `src/ma`. See [`files.md`](files.md).

A candidate keeps the command name that the user typed, so `:e src/ma` completes
to `:e src/main.rs` and `:edit src/ma` completes to `:edit src/main.rs`.

The workspace walk that finds the files runs on the bounded worker service. The
line asks for that walk when it first holds a path argument, so a line without
one asks for nothing. One open command line then starts exactly one walk, and
every later character of that line starts no second walk. The command line
therefore never waits for the filesystem, and the user keeps typing while the
walk runs. `Tab` before the result arrives offers no candidate, changes nothing,
and reports nothing, exactly as a text that names no command. A cancelled or
timed out walk leaves the command line in that same state. The next `Tab` after
the result arrives offers the files.

Every candidate comes from a walk that starts at the workspace root, so no
candidate reaches outside that root.

## Confirmation

An action that destroys data asks the user first. The question sits on the
message line, in the form `<question>? [y/N]:`. It opens no window and no
overlay. The confirmation is an input context, not a mode.

The confirmation reads a typed answer. One keypress therefore never performs the
action:

| Keys | Effect |
|---|---|
| A printable key | Add the character to the answer |
| `Backspace` | Remove the character before the answer cursor |
| `Ctrl-W` | Remove the word before the answer cursor |
| `Enter` | Read the answer and close the question |
| `Esc` | Cancel the action |
| `Ctrl-C` | Cancel the action |
| Every other key | Change nothing |

The answer `y` and the answer `yes` perform the action. The confirmation
compares the letters without case, so `Y`, `Yes`, and `YES` perform it as well.
Every other answer cancels the action. The capital `N` of the question names the
default, so an empty answer cancels the action too.

The confirmation completes nothing, so `Tab` and `Shift-Tab` add no character
and offer no candidate. `Backspace` on an empty answer keeps the question open,
because only `Enter`, `Esc`, and `Ctrl-C` close it.

The answer holds `CONFIRM_ANSWER_CHARS_MAX` characters, which is 32. A longer
answer is no accepted word, so the bound rejects the further characters and
changes nothing else. The row shows the answer after the hint and draws the
cursor after the answer. A cancelled action changes nothing and leaves no trace
on the message line.

The confirmation owns every key while it is open, so a key that it does not read
reaches no other owner. It owns the keys only while it is open, so no key
reaches it before the question appears or after the answer closes it. It holds
no count and no key sequence, so it never takes the key of a pending operator or
of a pending count. Opening it resets pending input, exactly as opening a prompt
does.

Three owners can be open at the same time, and they own the keys in one order.
The confirmation owns them first, because it draws over the prompt. An open
prompt owns them next. The scope of the focus owns them last. Each owner returns
the keys to the next owner that is still open. A question that opened over a
prompt therefore returns the keys to that prompt, which keeps its text. A
question with no open prompt returns them to the scope, so a question of the
file-tree sidebar returns the keys to the sidebar.

The confirmation reads text, and it stays beside the prompt model. A question
can open over an open prompt, and that prompt keeps its own text, so the two
lines must exist at the same time. The confirmation also draws a dynamic
question instead of a static prefix, and it reads its own small key table. One
open confirmation therefore holds its question, its answer, and its action in
one value, and `Enter` reaches the confirmation alone.

At most one confirmation is open. A second request while one waits is refused,
and the open question keeps the line.

Every confirmation follows an action of the user. A key opens most questions.
The overwrite question opens when the worker reports that the destination holds
an entry, because the terminal event loop reads no filesystem state. That report
belongs to the operation that the user asked for, and the user waits for it.

No unsolicited background event opens a question, so a watcher burst and a
language server reply never take the key of a user who types.

## Semantic Commands

A semantic command describes intent, such as move the cursor, delete a range,
paste a register, save the buffer, focus a window, resize a window, open a
picker, toggle a comment, or go to a definition. A command carries typed
arguments, such as a count or a motion, never a key value.

Each command has a stable identifier and a short label. The which-key overlay
and any help output derive their text from these labels.

## Mapping Registry

The mapping registry maps a bounded key sequence to a generic command identity,
scope, surface, ownership, label, and metadata. One sequence can appear in
several scopes with different commands.

The registry validates itself at construction. It rejects a duplicate sequence
inside one scope and ownership layer. It rejects a sequence that is also a
strict prefix of another sequence in the same effective scope when both would
resolve. It rejects an empty sequence and a sequence above the pending-key
maximum.

Registry snapshots are immutable and shareable. The registry is the only source
for dispatch, conflict checks, help, and which-key. Kvim exports its default
mode bindings as contributions to the shared registry and keeps no private
mapping table. The standalone executable still loads no configuration file. See
[`settings.md`](settings.md).

## Shared Dispatch

One resolver belongs to one composed interface instance. Pending static sequence
state belongs to that resolver, not to an editor. The resolver accepts bounded
key and paste input, elapsed time, active scopes, and one
`InputContextSnapshot` for each focused surface.

Resolution returns one of these typed outcomes:

- host command,
- surface command,
- typed text for one owner,
- pending sequence,
- unsupported input,
- unbound input.

Unsupported modified terminal input never degrades into an unmodified binding.
Paste follows the focused scope's typed-text owner and remains bounded. Focused
unbound and unsupported outcomes remain addressed surface inputs while that
surface reports semantic pending state.

The resolver evaluates the overlay scope, the host-global scope, and the focused
scope in that order. It walks this order for every key of a sequence, not only
the first. The first scope that completes a key wins. When no scope completes
it, the first scope that can extend it owns that key. The owning scope can
therefore change from one key to the next.

Before this evaluation order spanned every key, a host-global binding that
shared kvim's leader could silently swallow kvim's own leader bindings. The
host-global scope precedes the focused scope in evaluation order. It always
armed the prefix first. Every further key then resolved in that scope alone.
The resolver now re-evaluates the scope order at every key. kvim's own leader
bindings therefore resolve correctly beside a host-global binding under the
same prefix.

The which-key hints of a pending prefix span the same scope order. Each hint
names its scope. Every hinted key resolves to some scope's binding. Two scopes
can hint the same key with different commands. The earlier scope in the order
wins that collision. See [Leader And Which-Key](#leader-and-which-key).

A text fallback takes the first key of a sequence only. A key that breaks a
started sequence types no text.

The scopes that declare a text fallback are Insert mode, the prompt, the
confirmation, and the register selection. Each one names the focused surface as
its text owner. The prompt scope and the confirmation scope hold their own small
binding tables for the keys that type no character, so `Enter`, `Esc`,
`Ctrl-C`, `Backspace`, `Tab`, and `Shift-Tab` are ordinary bindings. Insert mode
binds `Enter`, `Backspace`, and `Tab` for the same reason.

After every command, text, paste, unbound, unsupported, or cancellation input,
the editor returns an `InputContextSnapshot`. It contains mode, operator phase,
count phase, register phase, text-object phase, prompt phase, text fallback, and
a generation.

Every context-state change increments the generation. A generation change
clears a pending static prefix. Focus, mode, or overlay-stack changes also clear
the resolver's pending sequence.

The text-object phase mirrors the resolver's own pending prefix, so it publishes
no new generation. A new generation would clear the very prefix that produced
the phase. Typed text changes no phase, so it keeps the generation as well.

Closing an overlay reveals the previous scope from the same immutable registry.
It does not rebuild the registry.

Before focus, mode ownership, or overlay ownership changes, the workspace
composer can return `CompositionEffect::CancelPending { surface, transition }`.
The host applies it to the addressed surface and returns the reset snapshot.
Only then can the composer commit the transition. See
[`embedding.md`](embedding.md).

## Counts And Sequences

Count digits are surface commands. The registry binds `1` through `9` in every
scope that accepts a count, and the editor semantic reducer composes them with
operators and motions after shared dispatch. `0` carries no count command,
because it is the first-column motion until a count is already open. The reducer
reads that motion as the zero digit while a count is open, so `10j` moves ten
lines and `0` alone still reaches the first column.

A register selection is a surface command as well. `"` reaches it in Normal mode
and in the three Visual modes, and the register-selection scope then converts
the next printable key into the register name through its text fallback.

The count maximum is 9,999. Count composition uses checked arithmetic. A count
above the maximum is invalid and resets semantic pending state.

Count digits, register selection, operator start, and text-object start are
grammar-prefix transitions. They do not complete a semantic operation. A
selected register applies to one completed semantic operation.

A count belongs to Normal mode, the three Visual modes, the operator-pending
scope, and the file-tree sidebar. Insert mode holds no count, because a digit is
buffer text there. The `input` module owns this rule, so no other module filters
digit keys.

The semantic reducer keeps operator state through count and motion commands, so
`d2w` deletes two words. A count before the operator multiplies with the count
before the motion, so `2d3w` deletes six words.

The resolver classifies each pending sequence as a complete match, a valid
prefix, a cancel, or no match. A pending sequence holds at most four keys.

A pending sequence has no deadline. It waits for the next key for as long as the
user needs. The registry already rejects a sequence that is both a complete
match and a strict prefix of a longer sequence in the same mode, so no ambiguity
remains that a timer could resolve. A deadline would only abandon the sequence
and hide the which-key overlay while the user still reads it.

`Esc` and `Ctrl-C` cancel pending input. The cancel works in every mode and at
every depth of a pending sequence, and it clears the pending keys, the pending
count, and the which-key overlay. It changes no buffer text and no mode. With no
pending input the same key reaches the registry, so it still returns to Normal
mode.

The resolver is clock-independent: it never reads a clock. The event loop
supplies the elapsed time with each resolution request. That time serves the
which-key delay only, which keeps resolution deterministic and testable.

The count maximum and the pending-key maximum belong to `EditorSettings`. See
[`settings.md`](settings.md).

## Leader And Which-Key

Space is the leader key in Normal, Visual, and Visual Line modes. Space starts a
pending sequence. It does not insert a space in these modes. In Insert mode
Space inserts a space.

The which-key overlay lists one level at a time. It shows the distinct next keys
that may follow the current pending sequence, never a complete sequence. At the
prefix `Space` it therefore shows `c`, not `c f`. Pressing `c` then shows only
the keys that follow `c`.

A next key that reaches exactly one command shows the label of that command. A
next key that reaches several commands shows a group marker with the number of
distinct commands behind it, in the form `+3 commands`. Two sequences that reach
the same command therefore count once. which-key.nvim marks a group the same
way, with a `+` prefix. The rows follow the key order of the registry, so the
overlay is deterministic.

The hints of a pending prefix come from every scope that extends it. The scope
order is the overlay scope, the host-global scope, and the focused scope,
without repetition. Each hint names its scope. A host can group or style a
host-global hint apart from a focused-surface hint. Every hinted key resolves
to some scope's binding when pressed. Two scopes can hint the same key with
different commands. The earlier scope in the order wins that collision. This
list shows an extension of the pending prefix only. A complete one-key binding
in another scope, such as a host-global escape, does not extend this prefix.
It does not appear here.

`Resolver::idle_which_key` publishes a second list for that gap. A host calls
it with no pending prefix. The list answers before the reader presses any key.

The list holds one entry for each distinct first key of every scope of the
context. The entries follow scope order. Each entry names its scope.

The list does not fold a key across two scopes. A key bound in two scopes
yields two entries. The earlier scope in the order answers when the reader
presses that key. A key that only starts a longer sequence still gets one
entry, because the reader can still press it.

The idle list ignores the which-key delay. It also ignores the overlay state
of the pending view. A reader can ask what to press at any time, not only
after a wait.

The list names a host-global escape, such as the key that returns focus to an
embedding host. A complete one-key binding never extends any prefix. Only the
idle list shows it.

The overlay is generated from the active shared resolver, its pending sequence,
and the same registry used for dispatch. It is never a separate hand-written
list. `keymap` folds the extensions of the pending prefix into these hints,
`ui` owns the bounded widget, its column layout, and its clipping, and the
interface layer supplies the final texts, the icons, and the palette colors.

The overlay lays its rows out in columns that fill the width of the body band.
Every column keeps the width of the widest row, so the keys and the labels of
all columns align. The overlay fills one column from top to bottom before it
starts the next one, and it takes only as many columns as it can fill, so no
column stays empty. A terminal that is narrower than one column shows one
column, which clips at the body edge.

The overlay bounds its height twice. It covers at most half of the body band, so
the buffer text around the cursor never disappears behind it, and one column
holds at most ten rows even in a tall terminal. A body band that cannot hold the
title row and one mapping over its own half shows no overlay. A prefix that
reaches more mappings than the bounded columns hold loses the last ones, and the
title row names how many rows the overlay dropped, for example `+2 more`. The
reader reaches those mappings by typing the next key.

Every row carries one icon, which the group of its command selects. The group is
a property of the command, so `input` owns it beside the identifier and the
label, and the interface layer owns every glyph and every color. The groups are
search, code, window, buffer, and file tree. Every other command falls to one
default group with one default icon. A key that reaches commands of several
groups also carries the default icon, because one icon cannot name two groups.
The one file-tree icon setting turns these glyphs off together with the tree
glyphs. Without them every column loses the same cells, so the columns stay
aligned. See [`files.md`](files.md).

The overlay appears after the which-key delay of 500 ms, so a fast key
combination never flashes it. The event loop supplies the elapsed time.

The delay governs the first appearance only. A visible overlay stays visible for
the rest of the pending sequence: each further key replaces its rows at once,
with no hiding and no second delay. The overlay hides only when a command
completes, when `Esc` or `Ctrl-C` cancels, or when pending input resets for
another reason, such as a mode change or a mismatch. A pending sequence keeps
the overlay visible until the user acts, because the sequence has no deadline.

The `input` module owns this rule. The pending state records that the overlay
became visible and keeps that record while the sequence continues.

[`windows.md`](windows.md) owns overlay placement and styling.

## Bracketed Paste And Unsupported Input

The terminal session enables bracketed paste, so one paste arrives as one
event instead of a run of key presses. A terminal reports a carriage return as
the line separator inside that event. `PasteText` converts every `\r\n` and
every remaining `\r` of the block to `\n` before any bound applies. The stored
block never holds a carriage return. The editor applies that block as one edit
transaction and one undo unit, and it never runs a binding, an auto-indent
rule, or an abbreviation over pasted text. `PasteText` carries the bound, so no
paste exceeds `PASTE_BYTES_MAX` after normalization. An open question and an
open command line take the block first, exactly as they take a typed
character. Normal mode owns no text fallback, so a paste there changes no
buffer.

A terminal that ignores the bracketed-paste request sends the key run instead,
which the editor still accepts.

The terminal reports `TerminalEvent::Unsupported` for input that no binding
accepts: a key that carries an unsupported modifier, and a paste block above
the bound. The editor never removes the modifier and never inserts part of the
block. It resets its pending grammar instead, so a rejected chord can never run
the binding of its unmodified key. A waiting operator lives in the editor as
well, so the reset reports `ReturnToNormal` for it, exactly as a cancel does.

An event that names no input at all, such as a mouse event or an empty paste
block, never reaches the editor. `EventSource` skips it, so moving the mouse
ends no pending sequence.

## Reset Rules

Pending input resets after:

- a completed semantic operation,
- a sequence mismatch,
- a cancel with `Esc` or `Ctrl-C`,
- input that no binding accepts,
- a mode change,
- a focus change,
- an overlay ownership change,
- editor shutdown.

Elapsed time is not in this list. A pending sequence has no deadline, so it
survives until one of the events above ends it.

A static-sequence reset clears pending keys and which-key state. A semantic reset
clears count, operator, register, text-object, and prompt phases as applicable.
Count resets after semantic completion, cancellation, invalid or unbound input,
focus change, mode change, or overlay ownership change. Operator state resets
after semantic completion, cancellation, invalid input, mode change, or focus
change. A selected register resets after one semantic operation, invalid
selection, cancellation, mode change, or focus change.

A reset never changes buffer text and never cancels a running background
request.

Input resolution and semantic reduction are pure. The visible-state owner runs
them on its host event loop, which remains responsive while background work
runs. See [`responsiveness.md`](responsiveness.md).

## First-Release Bindings

This table is the complete first-release binding set, and the registry in
`crates/kvim-input/src/registry.rs` implements it. Every key follows the
standard Vim key for that command.

### Modes

| Keys | Command | Modes |
|---|---|---|
| `i` | Insert before the cursor | Normal |
| `I` | Insert at the first non-blank character | Normal |
| `a` | Insert after the cursor | Normal |
| `A` | Insert at the end of the line | Normal |
| `o` | Open a line below and insert | Normal |
| `O` | Open a line above and insert | Normal |
| `v` | Enter Visual mode | Normal, Visual Line, Visual Block |
| `V` | Enter Visual Line mode | Normal, Visual, Visual Block |
| `Ctrl-V` | Enter Visual Block mode | Normal, Visual, Visual Line |
| `v` | Return to Normal mode | Visual |
| `V` | Return to Normal mode | Visual Line |
| `Ctrl-V` | Return to Normal mode | Visual Block |
| `:` | Open the command line | Normal |
| `Esc` | Return to Normal mode | Insert, Visual, Visual Line, Visual Block |
| `Ctrl-C` | Return to Normal mode | Insert, Visual, Visual Line, Visual Block |

`Esc` and `Ctrl-C` first cancel pending input. They reach the rows above only
while no key sequence and no count wait for completion.

Every motion row below also applies in Visual Block mode. The tables name the
three Visual modes separately only where their behavior differs.

### Insert Text Entry

| Keys | Command | Modes |
|---|---|---|
| `Enter` | Insert a line break | Insert |
| `Backspace` | Delete the character before the cursor | Insert |
| `Ctrl-W` | Delete the word before the cursor | Insert |
| `Tab` | Insert one indent step | Insert |

Every printable key reaches the text fallback of the Insert scope, so only
these four keys carry a command there. Vim, readline, and every terminal shell
delete the word before the cursor on `Ctrl-W`, so Insert mode binds it too. An
embedded editor must not leave that chord to a host prefix.

### Motions

| Keys | Command | Modes |
|---|---|---|
| `h` | Move left | Normal, Visual, Visual Line |
| `j` | Move down | Normal, Visual, Visual Line |
| `k` | Move up | Normal, Visual, Visual Line |
| `l` | Move right | Normal, Visual, Visual Line |
| `Left` | Move left | Normal, Insert, Visual, Visual Line, Visual Block |
| `Down` | Move down | Normal, Insert, Visual, Visual Line, Visual Block |
| `Up` | Move up | Normal, Insert, Visual, Visual Line, Visual Block |
| `Right` | Move right | Normal, Insert, Visual, Visual Line, Visual Block |
| `Ctrl-Left` | Move to the previous word start | Normal, Insert, Visual, Visual Line, Visual Block |
| `Ctrl-Right` | Move to the next word start | Normal, Insert, Visual, Visual Line, Visual Block |
| `w` | Move to the next word start | Normal, Visual, Visual Line |
| `b` | Move to the previous word start | Normal, Visual, Visual Line |
| `e` | Move to the next word end | Normal, Visual, Visual Line |
| `0` | Move to the first column | Normal, Visual, Visual Line |
| `Home` | Move to the first column | Normal, Insert, Visual, Visual Line, Visual Block |
| `^` | Move to the first non-blank character | Normal, Visual, Visual Line |
| `_` | Move to the first non-blank character | Normal, Visual, Visual Line |
| `$` | Move to the end of the line | Normal, Visual, Visual Line |
| `End` | Move to the end of the line | Normal, Insert, Visual, Visual Line, Visual Block |
| `g_` | Move to the last non-blank character | Normal, Visual, Visual Line |
| `%` | Move to the matching bracket | Normal, Visual, Visual Line |
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

The arrow keys name the same four motions as `h`, `j`, `k`, and `l`, and the two
word chords name the same motions as `w` and `b`. All six stay available in
Insert mode, where a letter is buffer text, so the user never leaves Insert mode
to move the cursor. Insert mode holds no count, so a digit before an arrow stays
buffer text there. A vertical arrow keeps the preferred display column, exactly
as `j` and `k` do.

Four keys reach the two ends of a line, and two pairs of them name the same
target. `0` and `Home` reach the first column. `^` and `_` reach the first
non-blank character. `$` and `End` reach the last column, and `g_` reaches the
last non-blank character. `$` and `g_` therefore differ only on a line that ends
with blanks. On a line of blanks alone, `^` keeps the last column and `g_` keeps
the first one, as the reference Vim does. `Home` and `End` stay available in
Insert mode, next to the arrow keys.

`%` reads the line of the cursor forward for the first `(`, `)`, `[`, `]`, `{`,
or `}`. It then moves to the partner of that bracket. The pair table of the text
objects names these three pairs, so one table serves both the jump and the
objects. The angle brackets stay out of it, because `<` and `>` are comparison
operators in most languages. The walk counts the nested pairs of the same
delimiter and crosses lines inside the text-object scan bound. Two cases reach
no partner and report no match: a line that holds no bracket at or after the
cursor, and a bracket without a partner inside that bound. The cursor then stays
where it stands.

`%` matches by text alone. kvim holds no comment region and no string region at
the cursor, so a bracket inside a comment or a string literal matches like every
other bracket. A count repeats the jump, as it does for every other motion,
instead of naming a percentage of the file.

An operator takes `g_` and `%` as a characterwise, inclusive target. `dg_`
therefore deletes to the last non-blank character. `d%` deletes both brackets of
the pair and the text between them.

An operator reads `w` by two rules that the plain motion does not follow.

1. An operator ends at the end of the last word that the motion moved over, when
   that word ends at the end of its line. `dw` on the last word of a line
   therefore removes that word and keeps the line, and `d2w` over a line
   boundary stops at the end of the second word.
2. `cw` on a non-blank changes to the end of the word, exactly as `ce` does, so
   the blanks after the word stay. `cw` on a blank follows the plain motion and
   removes the blanks.

The plain `w` motion keeps its own rule, so `w` on the last word of a line still
moves to the last character of that line.

macOS sends the `Option` chord as the `Alt` modifier, so `Option-Left` and
`Option-Right` arrive as `Alt-Left` and `Alt-Right`. The `terminal` module folds
six `Alt` encodings onto another chord while it normalizes the key, so the
registry holds one entry for each target chord and needs no separate `Alt`
binding. Several terminals and terminal multiplexers send `Ctrl-Alt-H`,
`Ctrl-Alt-J`, `Ctrl-Alt-K`, and `Ctrl-Alt-L` as an `Alt` chord over a control
byte, or as `Alt-Backspace` and `Alt-Enter`, and the module folds those
encodings as well. The table below names the reported code that carries the
`Alt` modifier and the chord it folds onto:

| Reported code | Folded key |
|---|---|
| `Left` | `Ctrl-Left` |
| `Right` | `Ctrl-Right` |
| `Backspace` or the control byte `\u{8}` | `Ctrl-Alt-H` |
| `Enter` or the control byte `\n` | `Ctrl-Alt-J` |
| The control byte `\u{b}` | `Ctrl-Alt-K` |
| The control byte `\u{c}` | `Ctrl-Alt-L` |

The enhanced keyboard reporting flags keep a modified arrow distinct from a
plain arrow.

A key that carries a modifier which no binding uses is rejected. The `terminal`
module never removes the modifier, so a modified key never reaches the binding of
the unmodified key. `Alt` without `Ctrl` and without one of the folds above,
`Super`, `Hyper`, and `Meta` are always rejections. `Shift` is part of the
character value, and `Shift-Tab` arrives as its own code, so both keep their
binding. Every other `Shift` combination is a rejection.

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

`"` names the register of the next operation, and the character after it is the
name. `"ayy` yanks the line into the register `a`, and `"ap` pastes it. `"` names
the unnamed register, `_` discards every write, and an upper-case name appends to
its lower-case register. [`clipboard.md`](clipboard.md) records which registers
the system clipboard sees.

### The Leader Key

The leader belongs to the leader in every scope that binds keys of its own. The
file-tree sidebar therefore passes `Space` through to the leader instead of
acting on the selected row, and every leader sequence reaches its command from
the sidebar exactly as it does from a buffer. `l` and `h` open and close an
entry, and `Enter` opens it, so the sidebar needs no toggle key of its own.

A command that the sidebar does not own falls through to the owner that the
session picks next. A modal surface that reads its own answer, such as the
picker or the review, takes every key while it stays open and passes no leader
sequence.

### The Review

`<leader>gg` opens the review of the changes of the worktree. The review owns
every key while it stays open, and it binds no key that changes a buffer.

The review holds two regions and moves both of them with the motions that the
buffer and the sidebar already publish, so it adds no motion command of its own.
`Ctrl-H` and `Ctrl-L` move the keys between the two regions.
[`diff-view.md`](diff-view.md) owns the table of its keys, the rules of its two
views, and its live updates.

### Text Objects

A text object names a range around the cursor without moving it first. An
operator takes it as its target, and a Visual mode takes it as its selection.

| Keys | Command | Scopes |
|---|---|---|
| `iw` | Select the word | Operator Pending, Visual, Visual Line, Visual Block |
| `aw` | Select the word and its blanks | Operator Pending, Visual, Visual Line, Visual Block |
| `iW` | Select the non-blank run | Operator Pending, Visual, Visual Line, Visual Block |
| `aW` | Select the non-blank run and its blanks | Operator Pending, Visual, Visual Line, Visual Block |
| `i(`, `i)` | Select inside the round brackets | Operator Pending, Visual, Visual Line, Visual Block |
| `a(`, `a)` | Select the round brackets | Operator Pending, Visual, Visual Line, Visual Block |
| `i[`, `i]` | Select inside the square brackets | Operator Pending, Visual, Visual Line, Visual Block |
| `a[`, `a]` | Select the square brackets | Operator Pending, Visual, Visual Line, Visual Block |
| `i{`, `i}` | Select inside the curly brackets | Operator Pending, Visual, Visual Line, Visual Block |
| `a{`, `a}` | Select the curly brackets | Operator Pending, Visual, Visual Line, Visual Block |
| `i<`, `i>` | Select inside the angle brackets | Operator Pending, Visual, Visual Line, Visual Block |
| `a<`, `a>` | Select the angle brackets | Operator Pending, Visual, Visual Line, Visual Block |
| `i"` | Select inside the double quotes | Operator Pending, Visual, Visual Line, Visual Block |
| `a"` | Select the double quotes | Operator Pending, Visual, Visual Line, Visual Block |
| `i'` | Select inside the single quotes | Operator Pending, Visual, Visual Line, Visual Block |
| `a'` | Select the single quotes | Operator Pending, Visual, Visual Line, Visual Block |
| ``i` `` | Select inside the backticks | Operator Pending, Visual, Visual Line, Visual Block |
| ``a` `` | Select the backticks | Operator Pending, Visual, Visual Line, Visual Block |

The open and the close delimiter name one object, so `vi(` and `vi)` select the
same text. `i` takes the text between the delimiters, and `a` takes the
delimiters too.

A bracket pair nests, so a count names the pair that holds the previous pair, and
the scan crosses lines inside the buffer. A quote pair never nests: the quotes of
the cursor line pair from its first column, and a count above one names nothing.
A word object stays inside the cursor line, because a line break separates two
runs. `aw` takes the blanks behind the word, or the blanks before it when none
follow.

A pair that does not close, a count without an outer pair, and a line without a
quote pair all leave the buffer unchanged. Every accepted object applies one edit
transaction, so one undo reverses it and `.` replays it.

### Search

| Keys | Command | Modes |
|---|---|---|
| `/` | Open the search prompt | Normal |
| `n` | Move to the next match | Normal |
| `N` | Move to the previous match | Normal |
| `Esc` | End the active search | Normal |
| `Ctrl-C` | End the active search | Normal |

Search uses smart-case matching. See [`settings.md`](settings.md). Ending the
search removes the match highlight. Both end keys reach the file-tree search as
well.

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
| `Ctrl-E` | Close the file tree | File Tree |
| `Space o` | Open the buffer picker | Normal |
| `Space fb` | Open the buffer picker | Normal |
| `Space x` | Unload the active buffer | Normal |
| `Space ff` | Open the file search picker | Normal |
| `Space f/` | Open the ripgrep search picker | Normal |

`Ctrl-S` saves without forcing an unrelated mode transition.

`Ctrl-E` opens the sidebar, expands every parent of the active file, selects
that file, and moves the focus into the sidebar, so the tree keys act at once.
The sidebar then owns `Ctrl-E`, which closes the sidebar and returns the focus
to the editor window. One key therefore opens and closes the tree, as the
reference configuration does. A buffer without a file name opens the sidebar on
the workspace root and reports that it has no path to reveal.

### File Tree

These keys act while the file-tree sidebar holds the focus. They follow the
reference Neo-tree subset, and the navigation keys follow the buffer instead, so
one row list moves like another. See [`files.md`](files.md) for the behavior
behind them.

| Keys | Command |
|---|---|
| `j` | Select the next entry |
| `k` | Select the previous entry |
| `Ctrl-D` | Select half a page down |
| `Ctrl-U` | Select half a page up |
| `Ctrl-F` | Select one page down |
| `Ctrl-B` | Select one page up |
| `gg` | Select the first entry, or the entry of the count |
| `G` | Select the last entry, or the entry of the count |
| `l` | Expand the selected directory, or open the selected file |
| `h` | Collapse the selected directory, or select the parent directory |
| `Backspace` | Select the parent directory |
| `Enter` | Open the selected file, or expand the selected directory |
| `Space` | Expand or collapse the selected directory |
| `R` | Read the workspace directories again |
| `a` | Add one file |
| `A` | Add one directory |
| `d` | Delete the selected entry, after a confirmation |
| `r` | Rename the selected entry |
| `y` | Copy the selected entry |
| `x` | Cut the selected entry |
| `p` | Paste the held entries |
| `H` | Show or hide the hidden entries |
| `/` | Search the visible entries |
| `n` | Select the next match |
| `N` | Select the previous match |
| `Esc` | End the active search and release the held entries |
| `Ctrl-C` | End the active search and release the held entries |
| `q` | Close the file tree |
| `Ctrl-E` | Close the file tree |
| `Ctrl-Q` | Close the file tree |
| `Ctrl-H/J/K/L` | Focus the window in that direction |
| `Ctrl-Alt-H/J/K/L` | Move the inner border of the file tree in that direction |
| `Ctrl-S` | Save the active buffer |

Every navigation key accepts a count, and every move stops at the first and the
last row, so no move wraps. A count before `gg` or `G` names one row instead of
a number of steps, as it names one line in a buffer. The half-page and the
full-page moves read their size from the visible rows of the sidebar, and the
selected row keeps the same scroll margin that a buffer window keeps.

`a`, `A`, `r`, and `/` read one line through the prompt of the message line, not
through a second input mechanism. The prompt returns the keys to the sidebar
when it closes. `Esc` and `Ctrl-C` cancel the prompt.

`d` destroys data, so it asks before it deletes. The question names the entry,
and it names the count of several entries. The answer `y` performs the removal,
and every other answer leaves every entry in place. The answer returns the keys
to the sidebar. See the Confirmation section above and [`files.md`](files.md).

`r` and `p` destroy data only when the destination holds an entry already, so
they ask only then. The question names the destination, and it names the count
of several destinations. The answer `y` replaces them, and every other answer
leaves every source and every destination in place. A rename onto a free path
and a paste into a free name ask nothing.

The tree search keeps every row. It marks each matching name, and `n` and `N`
move the selection between the marks. The search opens a closed directory that
holds a match, and the end of the search closes exactly those directories
again. See [`files.md`](files.md).

`Esc` and `Ctrl-C` cancel the sidebar work of the user. They end the active
search, and they release the entries that `y` or `x` holds, so one key drops a
paste that the user no longer wants. [`files.md`](files.md) owns the rule of the
file-operation clipboard.

`a` and `p` act on the destination directory, which is the selected directory,
or the directory of the selected file. `Enter` on a file opens it in the editor
window that held the focus, and the focus follows the file.

`l` and `h` follow the nvim-tree and neo-tree rule. `l` only ever moves deeper:
it opens a closed directory, it keeps an open directory open, and it opens a
file in the editor window, as `Enter` does. `h` closes an open directory. On a
file, and on a closed directory, it selects the directory that holds the entry
instead. Two presses therefore take a file to its folder and then close that
folder. The workspace root holds no row, so `h` at the top level changes
nothing.

### Pickers

These keys act while one picker is open. They follow the reference Telescope
subset. See [`files.md`](files.md) for the behavior behind them.

| Keys | Command |
|---|---|
| `Down` or `Ctrl-J` | Select the next result |
| `Up` or `Ctrl-K` | Select the previous result |
| `Enter` | Open the selected result |
| `Esc` | Close the picker |
| `Ctrl-C` | Close the picker |
| `Backspace` | Remove the last character of the query |
| `Ctrl-W` | Remove the last word of the query |

Every other printable key extends the query. `Backspace` on the empty query
closes the picker, as it does for every other prompt. `Ctrl-W` on the empty
query changes nothing and never closes the picker, unlike `Backspace`, because
a host that binds `Ctrl-W` elsewhere must not close a picker with it. Both keys
work the same way on the command line, the search prompt, the file-tree
prompt, and the confirmation. A closed picker restores the previous view
exactly, because the picker changes no editor state until the reader accepts
one row.

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

`Space q` and `Ctrl-Q` reach the same path as `:q`, so both ask before the last
window discards unsaved changes.

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

### Jump List

| Keys | Command | Modes |
|---|---|---|
| `Ctrl-O` | Jump back to the previous position | Normal |
| `Tab` | Jump forward to the next position | Normal |

A terminal reports `Ctrl-I` as `Tab`, which is why `Tab` carries the forward
step. `Tab` keeps its own commands in Insert mode and on the prompt line. See
[`windows.md`](windows.md) for the jump list that these two commands walk.

A jump records the position that the cursor held before it moved. These
sources record one jump: a definition jump (`gd`), an accepted picker row of
the file picker, the ripgrep picker, or the buffer picker, an opened file-tree
entry, `:<number>`, an accepted search query, `n`, `N`, `gg`, `G`, and `%`.
Every other motion records nothing. The half-page and the full-page moves
record nothing. A jump motion that serves as the target of a waiting operator
records nothing either, because the motion serves one change. The diagnostic
motions, `]d` and `[d`, record nothing as well.
