# Clipboard

## Ownership

The `clipboard` module owns the system clipboard boundary. It runs the platform
clipboard command through the bounded process service. It holds no register
value.

The `editor` module owns the registers. A register value stays inside the editor
even when the clipboard command fails. See [`responsiveness.md`](responsiveness.md)
for the process bounds and the deadline rule.

## The Provider Boundary

The clipboard is an input and output boundary. One trait declares the two
operations, read and write. Each platform supplies its own implementation behind
that trait.

The editor depends on the trait only. It never names a platform, a command, or a
selection. This keeps one rule: a new clipboard implementation needs no editor
change.

kvim supplies these implementations:

- one macOS implementation,
- one Linux implementation, which selects the Wayland or X11 commands,
- one implementation that performs no operation, for a system with no clipboard
  command,
- one memory implementation, for tests.

The composition root selects the implementation once, at startup, and injects
it. The platform branch stays inside the `clipboard` module. The editor and the
register logic contain no platform condition.

## The Host Grants The Access

A host names the clipboard policy, never the implementation. `ClipboardAccess`
holds the two policies:

- `ClipboardAccess::None` reaches no clipboard command at all. Every yank and
  every put stays inside the registers of that editor. This is the default, so
  no test and no host reaches the platform clipboard by accident.
- `ClipboardAccess::System` reaches the clipboard command of this platform.

`WorktreeEditorBuilder::capabilities` accepts the
policy. The selection runs once, inside that call, because it reads the target
platform and the executable search path. The `SessionClipboard` value stays
private, so no host names a clipboard command, a selection, or an executor.

The standalone `kvim` binary grants `ClipboardAccess::System`, which keeps the
behavior that the editor always had. An embedded editor keeps the default until
its host grants more.

`Session` keeps the granted policy in one `clipboard_access` field, and
`Session::clipboard_access` returns it. The field is not a second copy of a
readable fact: `SessionClipboard` holds a boxed platform boundary and can report
no policy, so this field is the only record of what the host granted. The
accessor mirrors `Session::access` for the editor access policy, so a host reads
back both grants through the same shape. Both stay.

A later implementation, for example an OSC 52 clipboard for a remote terminal,
joins the same trait and needs no change outside this module.

## The Unnamed Register

The unnamed register mirrors the system clipboard. The reference Neovim
configuration sets `clipboard=unnamedplus`, so a yank reaches other applications
and an external copy reaches the editor.

A yank, a delete, and a change write the unnamed register and then write the
system clipboard. A paste reads the system clipboard.

## The Named Registers

An operation that names a register with `"` reads and writes that register
alone. The system clipboard never sees a named register, so `"ayy` reaches no
other application and no external copy replaces the stored value.

The `editor` module holds the named registers beside the unnamed one, and its
revision counts the unnamed writes alone. The mirror reads that revision, so a
named write starts no clipboard work. A paste that names a register reads it
directly and starts no clipboard read either, so it changes the buffer on the
first step.

`"` names the unnamed register, so `""yy` behaves exactly like `yy` and still
reaches the system clipboard. `_` is the black-hole register: a write to it
discards the value, and a read from it holds nothing. An upper-case name appends
to its lower-case register, which is the rule that Vim follows.

## The Event Loop

The terminal event loop must never wait for a clipboard command. See
[`responsiveness.md`](responsiveness.md). Every clipboard operation therefore
runs in two steps.

1. The session runs the operation. An implementation that needs no external
   command finishes at once. An implementation that needs one produces a command
   instead of a value.
2. The event loop hands that command to the bounded process service. The output
   returns to the session, which repeats the same operation over that output and
   finishes it.

A yank finishes on the first step from the view of the user, because the unnamed
register already holds the value. A paste waits for the second step, because it
needs the clipboard text. A paste that waits changes nothing, so the buffer stays
exactly as it was until the read resolves.

The session runs one clipboard operation at a time, and one publication slot
holds it. A newer operation cancels the command of the operation that it
replaces and resolves that operation from internal state, so a waiting paste can
never wait for a result that no longer arrives.

## Linewise Values Across The Boundary

A register value is characterwise, linewise, or blockwise. A system clipboard
carries text only. It carries no shape.

kvim keeps the shape with its own register value. On a paste, kvim compares the
clipboard text with the text it wrote last. Equal text means the clipboard still
holds the kvim value, so the paste uses the recorded shape. Different text means
another application wrote the clipboard, so the paste treats the text as
characterwise, unless the text ends with a line ending, which makes it linewise.

This rule keeps `yy` followed by `p` linewise, and it keeps an external copy
usable.

## Platform Commands

kvim selects the command at startup and records the selection. It never guesses
per operation.

| Platform | Write | Read |
|---|---|---|
| macOS | `pbcopy` | `pbpaste` |
| Linux, Wayland | `wl-copy` | `wl-paste --no-newline` |
| Linux, X11 | `xclip -selection clipboard` | `xclip -selection clipboard -o` |
| Linux, X11 fallback | `xsel --clipboard --input` | `xsel --clipboard --output` |

On Linux, kvim prefers the Wayland commands when the session is a Wayland
session. It falls back through the X11 commands in the order above. It selects
the first command that exists on `PATH`.

## Failure Behavior

A missing command, a failed command, a timeout, and a cancelled command are all
expected runtime states. None of them may lose editor data.

- A failed clipboard write keeps the internal register value. The yank succeeded.
  kvim reports the clipboard failure once and continues.
- A failed clipboard read falls back to the internal register value.
- kvim reports a missing clipboard command once for each session, not once for
  each operation.

kvim reports only a proven failure. Each failure states whether the transfer
provably did not happen, or whether kvim never learned the outcome.

| Outcome | Report |
|---|---|
| The command reported a non-zero status, or a signal ended it | kvim reports the failure |
| The command did not start | kvim reports the failure |
| The bounded process service refused the command | kvim reports the failure |
| The clipboard holds bytes that are not text | kvim reports the bytes |
| The command passed its deadline | kvim reports nothing |
| A newer operation or the shutdown cancelled the command | kvim reports nothing |

The deadline row is not a convenience. The Linux write commands own the
selection through a background process, and that process inherits the captured
output streams of the command. Those streams therefore stay open for as long as
the selection lives, so the bounded process service sees the deadline instead of
the exit status of a write that succeeded. A deadline on a write is the normal
end of a successful `wl-copy` or `xclip` write, so it must never reach the
message line. A write that truly fails exits with a status before it owns any
selection, which keeps the report for the case that needs it.

kvim keeps the register value on every one of these paths, so a silent outcome
still loses nothing.

The editor stays fully usable without any clipboard command. A remote terminal
without a clipboard tool is a supported environment.

## Bounds

The clipboard command runs through the bounded process service with the process
deadline from [`responsiveness.md`](responsiveness.md). kvim bounds the transfer
at 1 MiB. A larger register value stays internal, and kvim reports that the
value was too large for the system clipboard.
