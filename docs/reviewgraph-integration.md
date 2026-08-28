# ReviewGraph Integration

## Ownership

This document owns the relationship between kvim and ReviewGraph. It is the only
document that describes deferred ReviewGraph work.

## Dependency Boundary

Kvim has no ReviewGraph application dependency. It does not import ReviewGraph
sessions, commands, workspaces, or comparison state.

Kvim can publish domain-neutral crates and contracts that ReviewGraph, keel, or
another host consumes. Shared use does not make kvim aware of the consumer. The
public syntax, LSP, keymap, UI, embedded editor, Git diff, and review contracts
remain useful without a ReviewGraph application.

Kvim mutates text. ReviewGraph is read-only. Each application keeps its own
state model and composition policy. A neutral review comment carries a bounded
anchor and body. It carries no application-specific meaning.

Kvim adapts generic ReviewGraph behavior where useful. See
[`architecture.md`](architecture.md) for crate boundaries and
[`embedding.md`](embedding.md) for host ownership.

## Source Attribution

Adapted ReviewGraph code carries a second module document line that records its
origin, for example:

```rust
//! Adapted from ReviewGraph (MIT), src/runtime.rs.
```

Both projects use the MIT license, so later code movement stays possible with
preserved notices.

## Application Integration

### Standalone Review Facade

`ReviewSurface` consumes bounded immutable candidates or bounded worktree
capture. It shares private review state, relocation, snapshots, and painting
with integrated review. The host owns focus, comment persistence, and all
application meaning. Supplied candidates are available with the `review`
feature. Bounded worktree capture is available with the `worktree` feature.

Application integration stays outside kvim:

- A host can compose a ReviewGraph surface with an embedded kvim editor.
- A host can open a working file from an immutable review candidate.
- A host owns comparison state, focus, commands, and comment persistence.
- Kvim owns only its domain-neutral editor, diff, anchor, and presentation
  values.

Do not add a ReviewGraph application dependency or application concept to kvim.
Do not copy ReviewGraph session policy, blame policy, or comparison workflow
into generic kvim modules. Neutral shared contracts are permitted.
