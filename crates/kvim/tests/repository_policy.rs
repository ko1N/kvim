//! Repository policy checks that no single crate can own.
//!
//! Each check reads the sources of the workspace through the manifest directory
//! of this binary. The binary is never a package of another repository, so a
//! path that leaves this crate stays valid.
//!
//! The checks enforce two rules of `docs/embedding.md` and `docs/architecture.md`:
//!
//! - every public feature module names its own dedicated example, and every
//!   example link that a module publishes resolves to a file that exists;
//! - exactly one layer owns the terminal, and the presentation crate owns none
//!   of it.

use std::fs;
use std::path::{Path, PathBuf};

/// The document that owns the complete example list.
const REQUIRED_EXAMPLES_DOCUMENT: &str = "docs/embedding.md";

/// The largest directory depth that the source sweep walks.
///
/// The source tree of one crate is flat today. The bound stops a symbolic link
/// or a future nested module tree from turning the sweep into an endless walk.
const SOURCE_DEPTH_MAX: usize = 8;

/// Every public feature module and the dedicated example that it must name.
///
/// `docs/embedding.md` owns this list. One feature has one example, and one
/// combined example never replaces a feature example.
const FEATURE_EXAMPLES: [(&str, &str); 27] = [
    (
        "crates/kvim-path/src/lib.rs",
        "crates/kvim-path/examples/confine_worktree_paths.rs",
    ),
    (
        "crates/kvim-fuzzy/src/lib.rs",
        "crates/kvim-fuzzy/examples/rank_candidates.rs",
    ),
    (
        "crates/kvim-keymap/src/lib.rs",
        "crates/kvim-keymap/examples/dispatch_keys.rs",
    ),
    (
        "crates/kvim-keymap/src/resolver.rs",
        "crates/kvim-keymap/examples/dispatch_keys.rs",
    ),
    (
        "crates/kvim-syntax/src/lib.rs",
        "crates/kvim-syntax/examples/highlight.rs",
    ),
    (
        "crates/kvim-syntax/src/highlight.rs",
        "crates/kvim-syntax/examples/highlight.rs",
    ),
    (
        "crates/kvim-lsp/src/lib.rs",
        "crates/kvim-lsp/examples/lsp_diagnostics.rs",
    ),
    (
        "crates/kvim-lsp/src/diagnostics.rs",
        "crates/kvim-lsp/examples/lsp_diagnostics.rs",
    ),
    (
        "crates/kvim-language/src/lib.rs",
        "crates/kvim-language/examples/headless_diagnostics.rs",
    ),
    (
        "crates/kvim-language/src/headless.rs",
        "crates/kvim-language/examples/headless_diagnostics.rs",
    ),
    (
        "crates/kvim-ui/src/sidebar.rs",
        "crates/kvim-ui/examples/sidebar.rs",
    ),
    (
        "crates/kvim-ui/src/guides.rs",
        "crates/kvim-ui/examples/sidebar.rs",
    ),
    (
        "crates/kvim-ui/src/list.rs",
        "crates/kvim-ui/examples/sidebar.rs",
    ),
    (
        "crates/kvim-ui/src/selector.rs",
        "crates/kvim-ui/examples/selector.rs",
    ),
    (
        "crates/kvim-ui/src/window.rs",
        "crates/kvim-ui/examples/split_windows.rs",
    ),
    (
        "crates/kvim-ui/src/which_key.rs",
        "crates/kvim-ui/examples/which_key.rs",
    ),
    (
        "crates/kvim-keymap/src/hint.rs",
        "crates/kvim-ui/examples/which_key.rs",
    ),
    (
        "crates/kvim-ui/src/composer.rs",
        "crates/kvim-ui/examples/composer.rs",
    ),
    (
        "crates/kvim-embed/src/lib.rs",
        "crates/kvim-embed/examples/in_memory_editor.rs",
    ),
    (
        "crates/kvim-embed/src/worktree.rs",
        "crates/kvim-embed/examples/worktree_editor.rs",
    ),
    (
        "crates/kvim-embed/src/composition.rs",
        "crates/kvim-embed/examples/merged_leader.rs",
    ),
    (
        "crates/kvim-embed/src/review.rs",
        "crates/kvim-embed/examples/supplied_review.rs",
    ),
    (
        "crates/kvim-tui/src/completion.rs",
        "crates/kvim-tui/examples/completion_menu.rs",
    ),
    (
        "crates/kvim-input/src/edited_line.rs",
        "crates/kvim-input/examples/edited_line.rs",
    ),
    (
        "crates/kvim-ui/src/tabs.rs",
        "crates/kvim-ui/examples/tab_strip.rs",
    ),
    (
        "crates/kvim-ui/src/band.rs",
        "crates/kvim-ui/examples/chrome_band.rs",
    ),
    (
        "crates/kvim-workspace/src/review.rs",
        "crates/kvim-tui/examples/worktree_diff_review.rs",
    ),
];

/// Every required external facade consumer and its accepted kvim dependencies.
const FACADE_CONSUMERS: [(&str, &[&str]); 8] = [
    (
        "kvim-embed-memory",
        &["kvim-embed", "kvim-input", "kvim-settings"],
    ),
    (
        "kvim-embed-worktree",
        &["kvim-embed", "kvim-input", "kvim-path"],
    ),
    (
        "kvim-embed-host-composition",
        &["kvim-embed", "kvim-input", "kvim-keymap"],
    ),
    (
        "kvim-embed-mixed-presentation",
        &["kvim-embed", "kvim-input"],
    ),
    (
        "kvim-embed-unified-host",
        &["kvim-embed", "kvim-input", "kvim-keymap"],
    ),
    ("kvim-embed-host-sidebar", &["kvim-embed"]),
    ("kvim-embed-review-supplied", &["kvim-embed", "kvim-path"]),
    ("kvim-embed-review-worktree", &["kvim-embed"]),
];

/// Returns the complete set of example files that this repository publishes.
///
/// A new example belongs to one public feature. An example outside this list is
/// a kitchen-sink example, which `docs/embedding.md` refuses.
///
/// That document owns the list, so this check reads it instead of holding a
/// second copy. Two copies of one fact are free to drift, which is the failure
/// that this file exists to prevent.
///
/// The reader takes the bullet list under the heading below and stops at the
/// first line that is no bullet, so a later paragraph adds no entry.
fn required_examples() -> Vec<String> {
    const HEADING: &str = "The required examples are:";

    let document = fs::read_to_string(repository_root().join(REQUIRED_EXAMPLES_DOCUMENT))
        .expect("the embedding document is readable text");
    let (_, listed) = document
        .split_once(HEADING)
        .unwrap_or_else(|| panic!("{REQUIRED_EXAMPLES_DOCUMENT} names its example list"));

    let mut examples: Vec<String> = listed
        .lines()
        .map(str::trim)
        .skip_while(|line| line.is_empty())
        .take_while(|line| line.starts_with("- "))
        .map(|line| line.trim_start_matches("- ").trim_matches('`').to_owned())
        .collect();
    assert!(
        !examples.is_empty(),
        "{REQUIRED_EXAMPLES_DOCUMENT} lists at least one example under its heading"
    );
    examples.sort();
    examples
}

/// Returns the root directory of this repository.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Collects every Rust source below one directory.
fn rust_sources(directory: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    assert!(
        depth < SOURCE_DEPTH_MAX,
        "the source tree of one crate stays below {SOURCE_DEPTH_MAX} directories"
    );
    let entries = fs::read_dir(directory).expect("every source directory is readable");
    for entry in entries {
        let path = entry.expect("the directory lists its entries").path();
        if path.is_dir() {
            rust_sources(&path, depth + 1, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// Collects the `src` directory of every crate of this workspace.
fn crate_sources() -> Vec<PathBuf> {
    let root = repository_root();
    let mut found = Vec::new();
    let entries = fs::read_dir(root.join("crates")).expect("the workspace holds its crates");
    for entry in entries {
        let source = entry
            .expect("the crate directory lists its entries")
            .path()
            .join("src");
        if source.is_dir() {
            rust_sources(&source, 0, &mut found);
        }
    }
    assert!(
        found.len() > 1,
        "the sweep read the sources of this workspace"
    );
    found
}

/// Returns the documentation lines of one source.
fn documentation(source: &str) -> impl Iterator<Item = &str> {
    source
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("//!") || line.starts_with("///"))
}

#[test]
fn facade_consumers_are_independent_and_use_only_supported_dependencies() {
    let root = repository_root();
    for (fixture, accepted) in FACADE_CONSUMERS {
        let manifest_path = root
            .join("fixtures/consumer")
            .join(fixture)
            .join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path)
            .unwrap_or_else(|_| panic!("{} is readable text", manifest_path.display()));
        assert!(
            manifest.lines().any(|line| line.trim() == "[workspace]"),
            "{fixture} is an independent outside-workspace package"
        );
        for line in manifest.lines().map(str::trim) {
            let Some((name, specification)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if !name.starts_with("kvim-") {
                continue;
            }
            assert!(
                accepted.contains(&name),
                "{fixture} imports unsupported {name}"
            );
            assert!(
                specification.contains("default-features = false"),
                "{fixture} disables default features for {name}"
            );
            assert!(
                specification.contains("git = ") && specification.contains("rev = "),
                "{fixture} revision-pins {name} as a Git dependency"
            );
        }
    }
}

#[test]
fn every_public_feature_names_its_dedicated_example() {
    let root = repository_root();
    for (module, example) in FEATURE_EXAMPLES {
        assert!(
            root.join(example).is_file(),
            "{example} is the dedicated example of {module}, but the file is absent"
        );
        let source = fs::read_to_string(root.join(module))
            .unwrap_or_else(|_| panic!("{module} is readable text"));
        let documented: String = documentation(&source).collect::<Vec<_>>().join("\n");

        // A module of the owning crate may name the example through the path
        // that Cargo uses for it, which omits the crate directory.
        let owner = module.split('/').nth(1).expect("a module names its crate");
        let relative = example
            .strip_prefix(&format!("crates/{owner}/"))
            .unwrap_or(example);
        assert!(
            documented.contains(example) || documented.contains(relative),
            "{module} publishes a public feature, so its documentation must name {example}"
        );
    }
}

#[test]
fn no_example_replaces_a_feature_example() {
    let root = repository_root();
    let mut published = Vec::new();
    let entries = fs::read_dir(root.join("crates")).expect("the workspace holds its crates");
    for entry in entries {
        let directory = entry
            .expect("the crate directory lists its entries")
            .path()
            .join("examples");
        if !directory.is_dir() {
            continue;
        }
        let mut found = Vec::new();
        rust_sources(&directory, 0, &mut found);
        for path in found {
            let relative = path
                .strip_prefix(&root)
                .expect("every example sits below the repository root")
                .to_string_lossy()
                .replace('\\', "/");
            published.push(relative);
        }
    }
    published.sort();

    assert_eq!(
        published,
        required_examples(),
        "{REQUIRED_EXAMPLES_DOCUMENT} names the complete set of examples"
    );
}

#[test]
fn every_documented_example_link_resolves() {
    let root = repository_root();
    let mut checked = 0;
    for module in crate_sources() {
        let owner = module
            .parent()
            .and_then(Path::parent)
            .and_then(|path| path.file_name())
            .expect("every module sits inside one crate")
            .to_string_lossy()
            .into_owned();
        let source = fs::read_to_string(&module).expect("every module is readable text");
        for line in documentation(&source) {
            for link in example_links(line, &owner) {
                assert!(
                    root.join(&link).is_file(),
                    "{} names {link}, which is no example file",
                    module.display()
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked >= required_examples().len(),
        "the sweep found the example links of this workspace"
    );
}

/// Returns true when the text is one plain example file stem.
fn is_file_stem(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '_' || value == '-')
}

/// Returns every example that one documentation line names.
///
/// The line can name the file directly, with or without the crate directory, or
/// it can name the Cargo command that runs it.
fn example_links(line: &str, owner: &str) -> Vec<String> {
    const DIRECTORY: &str = "examples/";
    const OPTION: &str = "--example ";

    let mut found = Vec::new();
    let mut consumed = 0;
    while let Some(start) = line[consumed..].find(DIRECTORY) {
        let before = &line[..consumed + start];
        let tail = &line[consumed + start + DIRECTORY.len()..];
        consumed += start + DIRECTORY.len();
        let Some(end) = tail.find(".rs") else {
            continue;
        };
        let stem = &tail[..end];
        if !is_file_stem(stem) {
            continue;
        }
        let package = before.rsplit_once("crates/").map_or_else(
            || owner.to_owned(),
            |(_, name)| name.trim_end_matches('/').to_owned(),
        );
        found.push(format!("crates/{package}/examples/{stem}.rs"));
    }

    let package = line
        .split_once("-p ")
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .unwrap_or(owner);
    let mut consumed = 0;
    while let Some(start) = line[consumed..].find(OPTION) {
        let tail = &line[consumed + start + OPTION.len()..];
        consumed += start + OPTION.len();
        let stem = tail.split_whitespace().next().unwrap_or_default();
        if !is_file_stem(stem) {
            continue;
        }
        found.push(format!("crates/{package}/examples/{stem}.rs"));
    }

    found
}

/// Returns the source directory of the presentation crate.
fn presentation_directory() -> PathBuf {
    repository_root()
        .join("crates")
        .join("kvim-tui")
        .join("src")
}

/// Returns the source of the terminal event loop.
///
/// The checks below name the very strings that they look for. They live in this
/// integration test, outside the crate source, so a check never finds its own
/// list.
fn loop_body() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("editor.rs");
    let source = fs::read_to_string(path).expect("the terminal event loop is readable text");
    assert!(
        !source.is_empty(),
        "the terminal event loop holds its own source"
    );
    source
}

#[test]
fn the_terminal_event_loop_runs_no_syntax_work() {
    // `kvim-tui` owns the inert `AnalysisRequest` value and hands it to the
    // bounded worker service through the driver. The loop therefore names no
    // syntax value at all. See `docs/responsiveness.md`.
    let body = loop_body();
    for name in ["analyze(", "SyntaxHighlighter", "AnalysisRequest"] {
        assert!(
            !body.contains(name),
            "the terminal event loop names {name}, so syntax work can reach it"
        );
    }
}

#[test]
fn this_binary_is_the_only_terminal_owner() {
    // The acceptance of the standalone editor is that exactly one layer owns
    // raw mode, the alternate screen, standard output, the event stream, the
    // signals, and the panic restoration. This check proves both halves: the
    // loop of this binary holds every owner, and the presentation crate holds
    // none of them. See `docs/architecture.md`.
    const TERMINAL_OWNERS: [&str; 5] = [
        "TerminalSession::enter",
        "TerminationSource::from_process",
        "EventSource::from_terminal",
        "CrosstermBackend::new",
        "set_cursor_shape",
    ];
    let body = loop_body();
    for owner in TERMINAL_OWNERS {
        assert!(
            body.contains(owner),
            "the terminal event loop of this binary must own {owner}"
        );
    }

    let mut found = Vec::new();
    rust_sources(&presentation_directory(), 0, &mut found);
    for path in &found {
        let source = fs::read_to_string(path).expect("every module is readable text");
        for owner in TERMINAL_OWNERS {
            assert!(
                !source.contains(owner),
                "{} names {owner}, but this binary owns the terminal",
                path.display()
            );
        }
    }
    assert!(found.len() > 1, "the check read the presentation modules");
}
