# ReviewGraph Integration

## Ownership

This document owns the relationship between Kvim and ReviewGraph. It is the only
document that describes deferred ReviewGraph work.

## First Release

The first release has no ReviewGraph dependency. Kvim does not depend on the
`reviewgraph` crate and does not share a crate with it.

Two reasons support this decision:

- Kvim mutates text. ReviewGraph is read-only. The two applications own
  different state models, so a shared crate would carry both models.
- Neither interface is stable enough to extract shared crates. An early
  extraction would freeze interfaces that both projects still change.

Kvim therefore adapts generic ReviewGraph behavior instead of importing it. See
[`architecture.md`](architecture.md) for the module boundaries that receive the
adapted behavior.

## Source Attribution

Adapted ReviewGraph code carries a second module document line that records its
origin, for example:

```rust
//! Adapted from ReviewGraph (MIT), src/runtime.rs.
```

Both projects use the MIT license, so later code movement stays possible with
preserved notices.

## Deferred Work

The following work stays outside the first release:

- Integrate ReviewGraph as an editor review workspace that publishes complete
  immutable comparison candidates.
- Let a ReviewGraph action open a working file in Kvim, without making
  ReviewGraph comparison state mutable.
- Merge the projects only after Kvim and ReviewGraph prove stable matching
  interfaces.
- Extract shared terminal interface, runtime, language, and Git crates during
  that later merge.

Do not add a ReviewGraph dependency, a shared crate, or a ReviewGraph
application concept to Kvim before that merge. Do not copy ReviewGraph concepts
such as commits, blame, or comparison previews into generic Kvim modules.
