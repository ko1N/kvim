//! Deterministic text model: rope buffer, validated coordinates, edit transactions, undo and redo.
//!
//! The module performs no input and no output. It depends on no other module.
//! The module keeps its parts in private submodules and re-exports the public
//! items from this file.
//!
//! Implementation arrives in Slice 4.
