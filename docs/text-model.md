# Text Model

## Ownership

The `core` module owns the text model: coordinates, edit transactions, undo, and
redo. It performs no input or output. Its `BufferBytesMax` value owns the
persistent byte-limit invariant. Settings convert their configured primitive
into this value at composition boundaries.

The `editor` module builds transactions from motions and operators. The `tui`
module renders text and measures terminal cells. Neither module changes text
outside a transaction.

## Text Storage

kvim stores buffer text in a `ropey` 1.6 rope. The rope converts between byte
offsets, character positions, and line indexes natively, so the five coordinate
types below convert without a local index. It keeps insertion and deletion cost
away from the buffer length, so one keystroke stays cheap in a large file.

The `core` module is the only module that uses the rope. It keeps the rope
private. Other modules receive validated coordinates and owned line text. The
[`architecture.md`](architecture.md) dependency ledger records the version
reason, the cost, and the later move to the 2.0 line.

## Coordinates

kvim keeps five text positions as distinct types or validated boundaries:

- Byte offset: a position in the UTF-8 byte sequence of the buffer.
- Character position: a count of Unicode scalar values.
- Line index: a zero-based line number in the buffer.
- Source column: a position inside one line, measured in the source text.
- Terminal-cell column: a position on the rendered screen row.

These five values differ for the same visible position. A multi-byte character
makes the byte offset larger than the character position. A wide character makes
the terminal-cell column larger than the source column. A tab expands to several
cells but occupies one source position.

Conflating these values corrupts text or cursor placement. A byte offset used as
a character position can split a UTF-8 sequence and produce invalid text. A
terminal-cell column used as a source column can place the cursor inside a
character or past the end of a line.

Each coordinate type validates its own invariant when it is constructed. A byte
offset must fall on a UTF-8 character boundary. A line index must exist in the
buffer. A source column must exist in its line. Conversion between coordinate
types is an explicit operation, never an implicit cast.

`core` defines the terminal-cell column type, but `core` does not measure cell
width. The terminal boundary measures width with `unicode-width` and constructs
the validated value. See [`architecture.md`](architecture.md) for that rule.

A language server measures a column in its own position encoding, which counts
UTF-8 bytes or UTF-16 code units. That column is a protocol value, not a
text-model coordinate. The language-server session converts every protocol
column into a source column at its own boundary, so no protocol column reaches
`core`. See [`language-services.md`](language-services.md).

## Edit Transactions

An edit transaction is the only way text changes. A transaction contains the
complete set of insertions, deletions, and replacements for one user-visible
change. Typing, paste, comment toggling, indent changes, and formatter edits all
apply as transactions.

A transaction is deterministic. Equal buffer content and equal transaction input
produce equal buffer content and equal cursor result.

Build a transaction against the current buffer generation and version. Validate
every range and the resulting byte length before any text changes. Apply the
complete transaction as one state change. A rejected transaction leaves text,
generation, version, dirty state, and history unchanged.

Each successful transaction increases the buffer version. A full reload or
replacement retains `BufferId`, increases a monotonic buffer generation, and
starts a new version sequence. Every text-derived background request carries
buffer identity, generation, and version. A publication gate rejects a result
that differs in any value. See [`responsiveness.md`](responsiveness.md) for the
publication check.

## Undo And Redo

Undo and redo operate on transactions, not on individual keystrokes. One undo
step reverses one transaction. One redo step reapplies one reversed transaction.

A transaction records the cursor position before the change and after the
change. Undo restores the recorded position, so the cursor returns to the place
where the change started.

A new transaction after an undo discards the redo entries above the current
position.

Undo history is bounded twice. One buffer keeps at most 1000 applied
transactions, which matches the reference Neovim `undolevels` value. One buffer
also keeps at most 16 MiB of replaced and inserted text, which is four times the
maximum file size. The buffer drops the oldest entries until both bounds hold.
The bounds belong to `core`, not to `EditorSettings`, because they protect the
text model against unbounded memory instead of adjusting editor behavior.

A buffer is dirty while its history position differs from the position of the
last save. Undo back to that position makes the buffer clean again. A buffer
whose saved position falls out of the bounded history stays dirty until the next
save.

Dot-repeat replays the last repeatable transaction description, not the recorded
transaction result. The `editor` module owns that description.

[`files.md`](files.md) owns persistent undo files.

## UTF-8 And Line Endings

kvim loads regular UTF-8 files. It rejects other encodings with a typed result
and a clear message. It does not guess an encoding and it does not transcode.

Every buffer operation preserves UTF-8 boundaries. A transaction that would
split a character is invalid. A byte offset that falls inside a character is a
typed error, never a panic. Every transaction range uses character positions, so
an applied transaction cannot split a character.

A combining mark keeps its own character position. `core` validates character
boundaries only, because a grapheme cluster boundary needs a segmentation table
that `core` does not hold. The `editor` module owns that table.

Every cursor column stands on a grapheme cluster boundary of its line. The
`Cursor` constructors hold the rule, and every motion, every clamp, and every
operator range reaches one of them, so no cursor stands between a letter and its
combining mark. One horizontal step passes one whole cluster, and one backward
delete removes one whole cluster.

The rule bounds its own cost. `core` reports whether one line holds ASCII
characters only, and an ASCII line needs no segmentation, because every ASCII
character is its own cluster. A line that holds other characters is segmented,
and the file settings bound one buffer, so they bound that work.

kvim detects the line ending of the loaded file. It records that line ending
with the buffer. It writes the same line ending on save. A file with mixed line
endings uses the first detected line ending for new lines and keeps existing
lines unchanged. A paste follows the same rule, so the buffer line ending
applies to every line that the pasted text opens. The buffer model treats a
line ending as a line terminator, not as text inside the line.

## Lines And The File End

A line ending terminates its line. It does not separate that line from an empty
line behind it. A file that ends with one line ending therefore holds as many
lines as it holds line endings, which is the count that the reference editor
shows. `"one\ntwo\n"` and `"one\ntwo"` both hold two lines.

The buffer text always ends with a line ending. The last line then carries a
terminator like every other line, so `o`, `Enter`, and a paste open a new line
behind it. `core` terminates the last line when the loaded text ends without one.

`core` records the file end that the loaded file held. The save writes that file
end, so a file that ended without a line ending receives none, and a file that
ended with one keeps exactly one. A save that changes nothing writes the bytes
that the file held. `files.md` owns the save.

An empty file holds one empty line. The buffer text is one line ending, and the
save writes an empty file again.

A buffer always holds at least one line, so a delete of every line keeps one
empty line. Every planned edit keeps the terminator of the last line, because a
buffer text without it would lose the last line of the file.

## Size Limits

`TextBuffer` owns a validated byte limit. The limit is a persistent buffer
invariant, not broad editor configuration. Construction, transactions, undo,
redo, reload transfer, and snapshots preserve it. A staged change above the
limit returns a typed error before it changes text, generation, version, dirty
state, or history.

The limit applies to logical file-content bytes. When a file has no final line
ending, the internal rope adds one synthetic terminator for editing. That
terminator does not count against `BufferBytesMax`. The internal rope therefore
holds at most the logical limit plus the selected terminator width: one byte for
line feed or two bytes for carriage return and line feed.

Each transaction calculates the exact resulting logical UTF-8 byte size from
the current logical size, removed ranges, and replacement strings. It uses
checked arithmetic and does not allocate a duplicate buffer for this check.
Undo and redo can only restore previously valid states and assert that those
states remain within the persistent limit.

kvim rejects an oversized file before it publishes a buffer. Rejection happens
before parsing, highlighting, or rendering. `EditorSettings` supplies the
initial configured limit at the public construction boundary.

The default maximum file size is 4 MiB. ReviewGraph uses the same bound for
analysis sources. The rope holds a 4 MiB buffer without trouble, so the bound
stays at 4 MiB. `core` rejects a larger text with a typed error before it builds
a buffer.

## Indent Policy

The default indent policy uses soft tabs. kvim inserts spaces for the tab key.
The tab width is four, and it measures how wide one existing tab character
draws.

One indent level is a separate width. `EditorSettings` resolves it against the
language of the buffer, so a Nix buffer steps by two columns and a Rust buffer
steps by four. See [`settings.md`](settings.md) for the resolution order.

The tab key inserts one indent level, and the Visual `<` and `>` commands change
the selection by one indent level. The comment toggle preserves the existing
indent of each affected line.

`kvim-core` supplies the indent measurement, the indent rendering, and the shift
step. `kvim-editor` owns the automatic indent rule and the shift commands.

## Automatic Indent

A new line receives an automatic indent. The `Enter` key in Insert mode and the
`o` and `O` commands all use the same rule.

The language adapter owns the rule. The syntax-tree rule derives the indent from
the parse result: a new line inside a block gains one level, and a closing
delimiter loses one level. The adapter names the node kinds and the delimiters
of its language, so the rule holds for every language. See
[`language-services.md`](language-services.md).

kvim uses a fallback rule when no adapter serves the buffer, or when the syntax
tree for the current buffer version is not yet available. The fallback copies the
indent of the previous non-empty line. The fallback never blocks the terminal
event loop while it waits for a parse result.

A Visual selection move uses the same rule. The moved block takes the automatic
indent of a new line at the end of the line that it lands behind. A block that
moves into a scope gains one level, and a block that leaves a scope loses one
level. The block keeps its internal relative indent, and an empty line inside
the block stays empty.

The automatic indent is part of the same edit transaction as the new line, or as
the moved block. One undo reverses both.

The language adapter declares the width of one indent level for its language,
exactly as it already declares its comment token and its indent scopes.
`EditorSettings` keeps an override that wins for every language, and keeps the
fallback width for a buffer that no adapter serves. See
[`settings.md`](settings.md) for the resolution order.

## Backward Delete

The `Backspace` key in Insert mode deletes the character before the cursor. At
column zero it removes the complete line ending before the cursor line, so the
two lines join. At the start of the buffer it changes nothing.

The delete is one edit transaction, so one undo reverses it. The delete writes
no register.
