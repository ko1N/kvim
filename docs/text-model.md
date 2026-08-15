# Text Model

## Ownership

The `core` module owns the text model: coordinates, edit transactions, undo, and
redo. It performs no input or output. It depends on no other module except
`settings`.

The `editor` module builds transactions from motions and operators. The `tui`
module renders text and measures terminal cells. Neither module changes text
outside a transaction.

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
position. Undo history is bounded. The bound is not yet decided. Slice 4 must
record it in [`settings.md`](settings.md) before implementation enforces it.

Dot-repeat replays the last repeatable transaction description, not the recorded
transaction result. The `editor` module owns that description.

[`files.md`](files.md) owns persistent undo files.

## UTF-8 And Line Endings

Kvim loads regular UTF-8 files. It rejects other encodings with a typed result
and a clear message. It does not guess an encoding and it does not transcode.

Every buffer operation preserves UTF-8 boundaries. A transaction that would
split a character is invalid.

Kvim detects the line ending of the loaded file. It records that line ending
with the buffer. It writes the same line ending on save. A file with mixed line
endings uses the first detected line ending for new lines and keeps existing
lines unchanged. The buffer model treats a line ending as a line terminator, not
as text inside the line.

## Size Limits

Kvim rejects an oversized file before it publishes a buffer. Rejection happens
before parsing, highlighting, or rendering. The maximum file size belongs to
`EditorSettings`.

The default maximum file size is 4 MiB. ReviewGraph uses the same bound for
analysis sources. Slice 4 must confirm the value against the selected text
storage before implementation enforces it.

## Indent Policy

The default indent policy uses four-space soft tabs. Kvim inserts spaces for the
tab key. The tab width is four. The shift width follows the tab width, so one
indent level and one tab render at equal width.

The Visual `<` and `>` commands change the selection by one shift width. The
comment toggle preserves the existing indent of each affected line.

All indent values belong to `EditorSettings`. See [`settings.md`](settings.md).
No other module holds an indent constant.

## Deferred Decisions

- The text-storage dependency is not yet chosen. Slice 4 evaluates a maintained
  rope or piece-table crate for correctness and resource cost, records the
  choice here and in the [`architecture.md`](architecture.md) dependency ledger,
  and only then uses it.
- The undo history bound is not yet decided. Slice 4 owns it.
