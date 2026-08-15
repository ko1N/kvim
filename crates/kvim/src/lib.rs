//! Kvim is a modal terminal editor for Rust projects.
//!
//! The crate root declares the module boundaries of the editor. Each module owns
//! one responsibility and keeps blocking work away from the terminal event loop.

pub mod core;
pub mod editor;
pub mod input;
pub mod language;
pub mod runtime;
pub mod settings;
pub mod terminal;
pub mod tui;
pub mod workspace;
