//! One structural check: only `kvim-keymap` matches keys against a table.
//!
//! `docs/input-actions.md` states that the shared registry is the only source
//! of dispatch, conflicts, help, and which-key. A second table would compile
//! and pass every behaviour test, so this check reads the sources of
//! `kvim-input`, `kvim-tui`, and the embedded editor instead.
//!
//! The check scans production code only. It drops every `*_tests.rs` file,
//! every line of a `#[cfg(test)]` module, and every comment line, so a test
//! fixture and a documentation example never trip it.

use std::fs;
use std::path::{Path, PathBuf};

/// The crates that must hold no key table of their own.
///
/// `kvim-editor` is the embedded editor state. `kvim-tui` owns the standalone
/// session and the embedded presentation. `kvim-input` owns the kvim preset
/// over the shared registry.
const SCANNED_CRATES: &[&str] = &["kvim-input", "kvim-tui", "kvim-editor"];

/// The one file that builds the kvim binding table.
///
/// It names every key of the preset, so it is the only production file that may
/// mention a key code.
const BINDING_TABLE: &str = "kvim-input/src/registry.rs";

/// The one file that adapts the shared resolver to the standalone editor.
///
/// It reads the chord and the code of one key to recognize the two cancel keys.
/// That predicate maps no key to a command, so it is no second table.
const SHARED_ADAPTER: &str = "kvim-input/src/resolver.rs";

/// One production source file with its workspace-relative name.
struct Source {
    name: String,
    code: String,
}

/// Returns the workspace root of this repository.
fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("the crate lives two directories below the workspace root")
        .to_path_buf()
}

/// Collects every production source file of the scanned crates.
fn production_sources() -> Vec<Source> {
    let root = workspace_root();
    let mut sources = Vec::new();
    for crate_name in SCANNED_CRATES {
        let directory = root.join("crates").join(crate_name).join("src");
        assert!(
            directory.is_dir(),
            "{} must hold the sources that this check reads",
            directory.display()
        );
        collect(&directory, &format!("{crate_name}/src"), &mut sources);
    }
    assert!(
        sources.len() > 10,
        "the check read {} files, so it found no sources to scan",
        sources.len()
    );
    sources
}

/// Reads every Rust file of one directory tree.
fn collect(directory: &Path, prefix: &str, sources: &mut Vec<Source>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("the directory entry must be readable").path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_dir() {
            collect(&path, &format!("{prefix}/{name}"), sources);
            continue;
        }
        // A `*_tests.rs` file holds test code only, and the harness compiles it
        // behind `#[cfg(test)]`.
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
        sources.push(Source {
            name: format!("{prefix}/{name}"),
            code: production_code(&text),
        });
    }
}

/// Returns the production lines of one file.
///
/// Everything from the first `#[cfg(test)]` line belongs to a test module, and
/// every comment line is prose.
fn production_code(text: &str) -> String {
    let mut kept = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            break;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

#[test]
fn only_the_kvim_preset_names_a_key_code() {
    for source in production_sources() {
        if source.name == BINDING_TABLE || source.name == SHARED_ADAPTER {
            continue;
        }
        assert!(
            !source.code.contains("KeyCode::"),
            "{} names a key code, so it holds a second binding table. \
             Every key belongs to the shared registry in {BINDING_TABLE}.",
            source.name
        );
    }
}

#[test]
fn no_module_matches_the_chord_or_the_code_of_a_key() {
    for source in production_sources() {
        if source.name == SHARED_ADAPTER {
            continue;
        }
        for marker in [".chord()", ".code()"] {
            assert!(
                !source.code.contains(marker),
                "{} reads `{marker}` of a key, so it classifies keys itself. \
                 The shared resolver of `kvim-keymap` owns that decision.",
                source.name
            );
        }
    }
}

#[test]
fn no_module_owns_a_second_pending_sequence() {
    for source in production_sources() {
        for marker in ["Vec<Key>", "[Key;"] {
            assert!(
                !source.code.contains(marker),
                "{} owns a key buffer, so it holds a second pending sequence. \
                 The shared resolver of `kvim-keymap` owns the only one.",
                source.name
            );
        }
    }
}

#[test]
fn exactly_one_adapter_declares_a_resolver() {
    let mut declared = Vec::new();
    for source in production_sources() {
        for line in source.code.lines() {
            let trimmed = line.trim_start();
            if (trimmed.starts_with("pub struct ") || trimmed.starts_with("struct "))
                && trimmed.contains("Resolver")
            {
                declared.push(source.name.clone());
            }
        }
    }
    assert_eq!(
        declared,
        vec![SHARED_ADAPTER.to_owned()],
        "only the standalone adapter may declare a resolver type, \
         because `kvim-keymap` owns the shared resolver"
    );
}
