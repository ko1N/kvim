# Text Model

## Ownership

The `core` module owns the text model: coordinates, edit transactions, undo, and
redo. It performs no input or output. It depends on no other module except
`settings`.

The `editor` module builds transactions from motions and operators. The `tui`
module renders text and measures terminal cells. Neither module changes text
outside a transaction.

## Text Storage

Kvim stores buffer text in a `ropey` 1.6 rope. The rope converts between byte
offsets, character positions, and line indexes natively, so the five coordinate
types below convert without a local index. It keeps insertion and deletion cost
away from the buffer length, so one keystroke stays cheap in a large file.

The `core` module is the only module that uses the rope. It keeps the rope
private. Other modules receive validated coordinates and owned line text. The
[`architecture.md`](architecture.md) dependency ledger records the version
reason, the cost, and the later move to the 2.0 line.

## Coordinates

Kvim keeps five text positions as distinct types or validated boundaries:

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

## Edit Transactions

An edit transaction is the only way text changes. A transaction contains the
complete set of insertions, deletions, and replacements for one user-visible
change. Typing, paste, comment toggling, indent changes, and formatter edits all
apply as transactions.

A transaction is deterministic. Equal buffer content and equal transaction input
produce equal buffer content and equal cursor result.

Build a transaction against the current buffer version. Validate every range in
the transaction before any text changes. Apply the complete transaction as one
state change. A rejected transaction leaves the buffer unchanged.

Each successful transaction increases the buffer version. Background analysis,
formatting, and language-server results carry the buffer version that produced
them. See [`responsiveness.md`](responsiveness.md) for the version check.

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

Kvim loads regular UTF-8 files. It rejects other encodings with a typed result
and a clear message. It does not guess an encoding and it does not transcode.

Every buffer operation preserves UTF-8 boundaries. A transaction that would
split a character is invalid. A byte offset that falls inside a character is a
typed error, never a panic. Every transaction range uses character positions, so
an applied transaction cannot split a character.

A combining mark keeps its own character position. `core` validates character
boundaries only, because a grapheme cluster boundary needs a segmentation table
that `core` does not hold. The `editor` module owns grapheme-aware cursor
movement when a later slice needs it.

Kvim detects the line ending of the loaded file. It records that line ending
with the buffer. It writes the same line ending on save. A file with mixed line
endings uses the first detected line ending for new lines and keeps existing
lines unchanged. The buffer model treats a line ending as a line terminator, not
as text inside the line.

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

Kvim rejects an oversized file before it publishes a buffer. Rejection happens
before parsing, highlighting, or rendering. The maximum file size belongs to
`EditorSettings`.

The default maximum file size is 4 MiB. ReviewGraph uses the same bound for
analysis sources. The rope holds a 4 MiB buffer without trouble, so the bound
stays at 4 MiB. `core` rejects a larger text with a typed error before it builds
a buffer.

## Indent Policy

The default indent policy uses four-space soft tabs. Kvim inserts spaces for the
tab key. The tab width is four. The shift width follows the tab width, so one
indent level and one tab render at equal width.

The Visual `<` and `>` commands change the selection by one shift width. The
comment toggle preserves the existing indent of each affected line.

## Automatic Indent

A new line receives an automatic indent. The `Enter` key in Insert mode and the
`o` and `O` commands all use the same rule.

The language adapter owns the rule. The syntax-tree rule derives the indent from
the parse result: a new line inside a block gains one level, and a closing
delimiter loses one level. The adapter names the node kinds and the delimiters
of its language, so the rule holds for every language. See
[`language-services.md`](language-services.md).

Kvim uses a fallback rule when no adapter serves the buffer, or when the syntax
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

All indent values belong to `EditorSettings`. See [`settings.md`](settings.md).
No other module holds an indent constant.

## Backward Delete

The `Backspace` key in Insert mode deletes the character before the cursor. At
column zero it removes the complete line ending before the cursor line, so the
two lines join. At the start of the buffer it changes nothing.

The delete is one edit transaction, so one undo reverses it. The delete writes
no register.

## Deferred Decisions

- The `editor` module owns the automatic indent rule and the shift commands.
  `core` supplies the indent measurement, the indent rendering, and the shift
  step. See Slices 6 and 12.
