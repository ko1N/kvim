//! Highlights one fragment of Rust and prints the role of each range.
//!
//! The example is one complete consumer of this crate. It uses no editor, no
//! language server, no terminal library, and no asynchronous runtime.
//!
//! Run it with the grammar that it needs:
//!
//! ```text
//! cargo run -p kvim-syntax --example highlight \
//!     --no-default-features --features grammar-rust
//! ```

use kvim_syntax::{HighlightLimits, NeverCancelled, SyntaxHighlighter, Truncation};

/// The fragment that the example highlights.
const FRAGMENT: &str = "\
fn greet(name: &str) -> String {
    // One comment.
    let count = 2;
    format!(\"{name} said hello {count} times\")
}
";

fn main() {
    let Some(entry) = kvim_syntax::language("rust") else {
        eprintln!("this build bundles no Rust grammar; enable the grammar-rust feature");
        return;
    };

    // One chat fragment is small, so the request names small bounds. The
    // defaults suit an editor buffer instead.
    let limits = HighlightLimits::default()
        .with_source_bytes_max(64 * 1024)
        .with_spans_max(4_000);

    let mut highlighter = SyntaxHighlighter::new();
    // The call is synchronous processor work. A consumer with a scheduler runs
    // it on a bounded worker and passes its own cancellation signal here.
    let highlighted = match highlighter.highlight(entry, FRAGMENT, &limits, &NeverCancelled) {
        Ok(highlighted) => highlighted,
        Err(failure) => {
            eprintln!("the fragment stays plain text: {failure}");
            return;
        }
    };

    println!("language: {}", entry.id());
    println!("spans: {}", highlighted.spans().len());
    for span in highlighted.spans() {
        let line = FRAGMENT.lines().nth(span.line as usize).unwrap_or_default();
        let start = span.start_byte as usize;
        let end = span.end_byte as usize;
        let text = line.get(start..end).unwrap_or_default();
        println!("  line {:>2} {:?} {text:?}", span.line, span.role);
    }

    for error in highlighted.errors() {
        println!("  the grammar could not read line {}", error.line);
    }

    match highlighted.truncation() {
        Truncation::Complete => println!("the report holds every span"),
        Truncation::Truncated { limit } => println!("one bound stopped the walk: {limit:?}"),
    }
}
