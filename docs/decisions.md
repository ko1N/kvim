# Decision Ledger

This file records each decision that the user made, with its reason and history.
Entry IDs are stable. Never renumber or delete an entry.

Statuses: `Accepted`, `Rejected`, `Superseded`, `Deferred follow-up`.

## Input

**D-001 - Close keys close the focused window**
- Status: Accepted
- Decision: `Space q`, `Ctrl-Q`, and `Space wq` close only the focused window, as
  `:q` and `:wq` do. They do not remove the buffer from the other windows.
  `Space x` still unloads the buffer. The user's Neovim configuration is the
  reference, and kvim matches its buffer and window keys closely.
- Alternatives considered: Keep `Space q` and `Ctrl-Q` as a buffer close.
- Rationale: "when i have 2 splits open and i press space+q i expect the split to
  close, not the buffer to unload in both splits". "we should match the bindings
  of my nvim config closely".
- Consequences/constraints: In the file-tree sidebar, the same keys hide the
  sidebar, because the sidebar owns `CloseWindow`. `Command::CloseBuffer` has
  no key.
- Owning architecture section: [Files And Buffers](input-actions.md#files-and-buffers)
- Amendment (2026-11-19, user): `Space x` copies `:bd`: it unloads the buffer,
  closes the other windows that show it, and the focused window shows another
  buffer. User: "can we copy these bindings exactly (also space x)?"

**D-002 - A wheel event does not cancel a drag selection**
- Status: Accepted
- Decision: A mouse wheel event scrolls the window and keeps the active drag, in
  the terminal editor and in the embedded editor. Later drag events keep
  extending the selection.
- Alternatives considered: Cancel the drag on a wheel event (the previous
  behavior).
- Rationale: The user reported it as a bug: "when i drag the mouse to select text
  and i scroll the selection hangs and wont select anything anymore."
- Consequences/constraints: Key, paste, resize, release, a lost target, and an
  overlay change still cancel the drag.
- Owning architecture section: [Pointer input](input-actions.md)
