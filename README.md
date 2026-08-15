# Kvim

Kvim is a modal terminal editor for Rust projects. It provides Vim-style editing, a dynamic window tree, a file tree, fuzzy pickers, Rust Tree-sitter highlighting, and rust-analyzer services in one executable.

The first release is in progress. The current executable parses its command line and reports that interactive editing arrives later.

## Install

Kvim targets macOS and Linux. Install Rust 1.85 or newer, then run:

```sh
cargo install --git https://github.com/ko1N/kvim.git --locked kvim
```

The command installs the `kvim` executable into Cargo's binary directory, usually `~/.cargo/bin`.

Kvim runs `rg` and `rust-analyzer` as external commands. Put both on `PATH`. The Nix package wraps the executable and supplies them.

## Use

Open one file, or start with an empty buffer:

```sh
kvim src/main.rs
kvim
```

Run `kvim --help` for all command forms, and `kvim --version` for the version.

## License

Kvim is available under the [MIT License](LICENSE).
