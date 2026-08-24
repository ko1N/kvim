//! One structural check: the composer accepts no host input or render callback.
//!
//! `docs/embedding.md` states that the composer performs no input and no
//! output. A callback parameter would compile and pass every behaviour test,
//! so this check reads the source of the composer instead.
//!
//! The check scans production code only. It drops every line of a
//! `#[cfg(test)]` module and every comment line, so a test fixture and a
//! documentation example never trip it.

use std::fs;
use std::path::{Path, PathBuf};

/// The module that owns the composition model.
const COMPOSER: &str = "src/composer.rs";

/// The markers that name a caller-supplied function value.
///
/// The sidebar takes one render callback, and it lives in another module. The
/// composer must name none of these, so reduction and layout can neither store
/// nor invoke host code.
const CALLBACK_MARKERS: &[&str] = &["Fn(", "FnMut", "FnOnce", "dyn ", "impl Fn"];

/// The markers that name terminal output or a cell buffer.
const OUTPUT_MARKERS: &[&str] = &["Buffer", "render", "draw", "Style"];

/// Returns the directory of this crate.
fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Returns the production lines of the composer module.
fn production_code() -> String {
    let path = crate_root().join(COMPOSER);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
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
    assert!(
        kept.contains("pub struct WorkspaceComposer"),
        "the check must read the composer, not an empty file"
    );
    kept
}

#[test]
fn the_composer_names_no_caller_supplied_function_value() {
    let code = production_code();
    for marker in CALLBACK_MARKERS {
        assert!(
            !code.contains(marker),
            "{COMPOSER} names `{marker}`, so reduction or layout could store or \
             invoke host code. The composer accepts identities and values only."
        );
    }
}

#[test]
fn the_composer_names_no_cell_buffer_and_no_terminal_style() {
    let code = production_code();
    for marker in OUTPUT_MARKERS {
        assert!(
            !code.contains(marker),
            "{COMPOSER} names `{marker}`, so it takes part in rendering. \
             The host renders every published placement itself."
        );
    }
}
