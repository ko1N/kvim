# kvim

kvim is a modal terminal editor for Rust projects. It provides Vim-style
editing, a dynamic window tree, a file tree sidebar that marks the repository
state, fuzzy pickers, a workspace file watcher, Tree-sitter highlighting for 25
languages, language-server services, and format-on-save in one executable. kvim
runs on macOS and on Linux. Every grammar is compiled in, so you install no
parser file. This release reads no configuration file, so every setting keeps
the default that this document records.

The 25 languages are assembly, Bash, C, C++, CSS, fish, GLSL, Go, HTML,
JavaScript, JSON, Lua, Markdown, Nix, Python, Rust, SCSS, SQL, Terraform, TOML,
TSX, TypeScript, XML, YAML, and Zig.

## Install

kvim needs Rust 1.85 or newer when you build it with Cargo.

### With Nix

The repository provides a flake. Run the editor without an installation:

```sh
nix run github:ko1N/kvim
```

Install the package into your profile:

```sh
nix profile install github:ko1N/kvim
```

Build the executable from a checkout:

```sh
nix build
```

The package wraps the executable and puts `git`, `rg`, and `rust-analyzer` on
its search path. It supplies no other language server and no formatter, so
install the tools of the other languages yourself. kvim reports a missing tool
once and stays fully usable without it.

To work on kvim itself, enter the development shell:

```sh
nix develop
```

The shell supplies Cargo, Rust, `rustfmt`, Clippy, `nixfmt`, `git`, `rg`, and
`rust-analyzer`. The `rust-toolchain.toml` file at the repository root names
the exact Rust version, and the shell supplies that version.

### With Cargo

```sh
cargo install --git https://github.com/ko1N/kvim.git --locked kvim
```

The command installs the `kvim` executable into the binary directory of Cargo,
usually `~/.cargo/bin`. A Cargo installation does not supply the external
commands. Install them yourself, as the next section describes.

## Use

Open one file, or start with an empty buffer:

```sh
kvim src/main.rs
kvim
```

kvim uses the working directory as the workspace root. The file tree, the
pickers, and the language server all work inside that root.

Space is the leader key. `Space ff` opens the file picker, `Space f/` opens the
search picker, `Space o` lists the loaded buffers, and `gd` goes to a
definition. `Ctrl-e` opens the file tree sidebar at the active file, and a
second `Ctrl-e` closes it. Press the leader key and wait half a second. The
which-key overlay then lists the keys that can follow.

kvim reports its own work on the message line at the bottom of the terminal. The
overlay in the bottom-right corner reports language-server progress, and nothing
else.

Run `kvim --help` for the command forms, and `kvim --version` for the version.

## Diagnostics

Run this command first when a feature seems to be missing:

```sh
kvim --diagnostics
```

The command prints a plain-text report and exits. The report names the version,
the workspace root, the state of `git` and `rg`, every language server and every
formatter that kvim declares, the clipboard commands of this host, and the
resource limits. Each program row names the program, whether your `PATH` holds
it, and the languages that use it. It writes no escape sequence, so you can
redirect it to a file or paste it into a bug report.

Run `:diagnostics` inside the editor for the same report in a buffer.

## External Commands

kvim runs five kinds of external command. Each one is optional. kvim reports a
missing command once and stays fully usable without it.

| Command | Enables | Without it |
|---|---|---|
| `git` | The repository marks of the file tree | The file tree still lists every entry and stays fully usable. It shows no repository state. kvim never writes the repository. |
| `rg` | The search picker on `Space f/` | The search picker returns no result. kvim reports the missing command once. Every other picker still works. |
| A language server | Diagnostics, go-to-definition, hover, and formatting | The buffer stays fully editable. kvim shows no diagnostics and answers no definition or hover request. Tree-sitter highlighting and the comment toggle still work, because they need no server. |
| A formatter | Format-on-save for the languages that name an external program | The save writes the unformatted content. kvim reports the missing program once. |
| A clipboard command | The system clipboard | The editor registers still hold every yank and every paste. Only the exchange with other applications stops. |

Put `git` and `rg` on `PATH`, with the language server and the formatter of each
language that you edit. A Nix installation supplies `git`, `rg`, and
`rust-analyzer`.

Each language names its own tools. Rust uses `rust-analyzer`, Python uses
`pyright-langserver` and `black`, C and C++ use `clangd` and `clang-format`, and
the web languages use `vscode-eslint-language-server`,
`typescript-language-server`, and `prettier`. `docs/language-services.md` names
the complete table.

## Clipboard

kvim selects the clipboard command once at startup. It never guesses per
operation.

| Platform | Write | Read |
|---|---|---|
| macOS | `pbcopy` | `pbpaste` |
| Linux, Wayland | `wl-copy` | `wl-paste --no-newline` |
| Linux, X11 | `xclip -selection clipboard` | `xclip -selection clipboard -o` |
| Linux, X11 fallback | `xsel --clipboard --input` | `xsel --clipboard --output` |

On Linux, kvim prefers the Wayland commands when the session is a Wayland
session. It then falls back through the X11 commands in the order above, and it
selects the first command that exists on `PATH`.

A host without any clipboard command is a supported environment, and a remote
terminal is the common case. The editor stays fully usable. A yank still fills
the internal register, and a paste still reads it. kvim reports the missing
command once for each session.

## Limits

Every bound below is fixed in this release.

| Limit | Value | Effect when you reach it |
|---|---|---|
| File size | 4 MiB | kvim refuses to load the file and reports the size. |
| Clipboard transfer | 1 MiB | The value stays in the internal register, and kvim reports that it was too large. |
| Loaded buffers | 128 | kvim opens no further buffer until you unload one. |
| Picker candidates | 4096 | The picker shows the first candidates and reports the truncation above the list. |
| Search matches | 1024 | The search picker shows the first matches and reports the truncation. Refine the query. |
| Undo steps in one undo file | 64 | The undo file keeps the newest steps. The running session keeps its complete history in memory. |
| Command count | 9999 | The input resolver refuses a larger count prefix. |
| Keys of one binding sequence | 4 | No key sequence in the mapping registry is longer. |

kvim also loads UTF-8 files only. It rejects a directory, a device file, a
binary file, and any other encoding with a clear message. It does not guess an
encoding and it does not transcode.

## Failure Behavior And Recovery

kvim treats every failure below as a normal runtime state. None of them loses
your text.

**A clipboard command fails.** The internal register keeps the value, so the
yank succeeded. kvim reports the failure and continues. A paste falls back to
the internal register.

**The language server is absent.** kvim reports the state once and starts no
further server for that language. The buffer stays fully editable, and kvim
shows no diagnostics.

**The language server crashes.** kvim restarts it at most three times in one
session. Each new server holds no document, so kvim reports the restart and
opens the buffers again. After the last restart, the editor continues without
the server.

**The language server is slow.** Every request carries a deadline. kvim reports
the timeout and keeps the buffer usable. A formatting failure or a formatting
timeout never cancels a save: kvim writes the unformatted content and reports
the state.

**A file exceeds the size limit.** kvim refuses to load it and reports the
size. No buffer changes.

**A save conflicts with an external change.** kvim compares the file size and
the modification time before it overwrites a file. When either differs from the
recorded value, kvim reports the conflict and writes nothing. The buffer stays
dirty and usable. Reload the file with `:e`, then apply your change again.

**Another program changes an open file.** kvim watches the workspace. A buffer
without an unsaved change reloads by itself and keeps the cursor. A buffer that
holds an unsaved change never reloads: kvim reports the change once and marks
the buffer `[!]` in its window bar. Reload it with `:e` after you save your
work, or discard your work and reload with `:e!`. A file that disappeared, that
grew past the size limit, or that is no longer text keeps its buffer, so the
text in memory stays the only copy until you save it.

**A save fails.** kvim writes to a temporary file in the target directory and
renames it over the target, so a reader never sees a partial file. A failure at
any step leaves the buffer dirty and leaves no temporary file behind. Retry the
save.

**An undo file is damaged.** kvim checks the magic value, the format version,
the content length, the content hash, and the replayed result. A record that
fails any check is ignored. The buffer starts with empty undo history and stays
correct.

**kvim panics.** A panic hook restores the terminal before the process ends. It
leaves the alternate screen, disables raw mode, shows the cursor, restores the
cursor shape, and pops the keyboard enhancement flags. The hook then prints the
normal panic message. Terminal restoration does not depend on unwinding,
because some platforms abort a panic without running any destructor. Your shell
therefore stays usable, and unsaved buffer content is lost.

## License

kvim is available under the [MIT License](LICENSE).
