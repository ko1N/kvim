//! Behavior tests for the language registry and the Tree-sitter adapters.

use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use kvim_core::{EditTransaction, TextBuffer, TextChange};
use kvim_runtime::{PublicationGate, RequestSlot, Runtime, RuntimeLimits};
use kvim_settings::FileSettings;

use super::server::declarations_are_valid;
use super::{
    ANALYSIS_DEADLINE, ANALYSIS_DEPTH_MAX, ANALYSIS_SOURCE_BYTES_MAX, ANALYSIS_SOURCE_LINES_MAX,
    Analysis, AnalysisError, AnalysisInput, BoundMeasure, BufferSyntax, CommentStyle, Grammar,
    IndentRule, LANGUAGE_SERVERS_MAX, LanguageAdapter, LanguageRegistry, Publication, RustAdapter,
    SyntaxRole, SyntaxTree,
};

/// A second adapter that proves the multi-language seam.
///
/// The adapter supplies its own extension, its own comment token, and its own
/// indent rule. It reuses the bundled grammar, because this release adds no
/// second grammar. Nothing above the trait changes for it.
#[derive(Clone, Copy, Debug)]
struct SecondAdapter;

impl LanguageAdapter for SecondAdapter {
    fn id(&self) -> &'static str {
        "second"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["kv"]
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("#"), None)
    }

    fn grammar(&self) -> Grammar {
        RustAdapter::new().grammar()
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &["block"],
            closing_delimiters: &['}'],
        }
    }
}

static FIRST_RUST: RustAdapter = RustAdapter::new();
static SECOND_RUST: RustAdapter = RustAdapter::new();
static SECOND: SecondAdapter = SecondAdapter;

/// One registry that serves two languages.
static TWO_LANGUAGES: [&dyn LanguageAdapter; 2] = [&FIRST_RUST, &SECOND];

/// One registry whose two adapters claim the same paths.
static AMBIGUOUS: [&dyn LanguageAdapter; 2] = [&FIRST_RUST, &SECOND_RUST];

/// Returns the adapter that the registry selects for a Rust path.
fn rust() -> &'static dyn LanguageAdapter {
    LanguageRegistry::first_release()
        .adapter(Path::new("src/main.rs"))
        .expect("the Rust adapter owns a .rs path")
}

fn buffer(text: &str) -> TextBuffer {
    TextBuffer::from_text(text, &FileSettings::default()).expect("the test text is small")
}

/// Analyzes one buffer without a previous tree.
fn analyze(buffer: &TextBuffer) -> Analysis {
    let input = AnalysisInput::new(buffer.version(), Arc::from(buffer.to_string()));
    rust()
        .analyze(&input, &CancellationToken::new())
        .expect("the test source stays inside every bound")
}

/// Analyzes one source text and returns the typed failure.
fn analysis_error(source: &str) -> AnalysisError {
    let input = AnalysisInput::new(buffer("").version(), Arc::from(source));
    match rust().analyze(&input, &CancellationToken::new()) {
        Err(error) => error,
        Ok(_) => panic!("the test source must pass one bound"),
    }
}

/// Analyzes one source with the adapter that the registry selects for a path.
fn analyze_path(path: &str, source: &str) -> Analysis {
    let text = buffer(source);
    let input = AnalysisInput::new(text.version(), Arc::from(text.to_string()));
    LanguageRegistry::first_release()
        .adapter(Path::new(path))
        .expect("the registry serves the path")
        .analyze(&input, &CancellationToken::new())
        .expect("the test source stays inside every bound")
}

/// Returns the roles of one line, in ascending byte order.
fn roles(analysis: &Analysis, line: u32) -> Vec<SyntaxRole> {
    analysis
        .highlights()
        .iter()
        .filter(|span| span.line == line)
        .map(|span| span.role)
        .collect()
}

/// Inserts text at one character position and returns the moved tree.
fn insert(buffer: &mut TextBuffer, tree: &SyntaxTree, at: usize, text: &str) -> SyntaxTree {
    let at = buffer.char_position(at).expect("the position exists");
    let transaction = EditTransaction::single(at, TextChange::insert(at, text));
    let moved = tree.clone().edited(buffer, &transaction);
    buffer.apply(transaction).expect("the position fits");
    moved
}

#[test]
fn only_an_adapter_selects_a_path() {
    let registry = LanguageRegistry::first_release();

    assert_eq!(
        registry.adapter(Path::new("src/main.rs")).unwrap().id(),
        "rust"
    );
    // Only a registered language reaches an adapter, and the extension match
    // is case-sensitive.
    for path in ["src/main.RS", "notes.txt", "rs"] {
        assert_eq!(
            registry.adapter(Path::new(path)).err(),
            Some(AnalysisError::UnsupportedPath),
            "{path} belongs to no adapter"
        );
    }
}

#[test]
fn the_rust_adapter_reports_its_comment_token() {
    assert_eq!(RustAdapter::new().comment().line_token(), Some("//"));
}

#[test]
fn rust_source_produces_terminal_independent_roles() {
    let analysis = analyze(&buffer("// note\nfn main() {\n    let value = 1;\n}\n"));

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Function));
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Number));
    // Every span stays inside the line that carries it.
    for span in analysis.highlights() {
        assert!(span.start_byte < span.end_byte, "{span:?} is empty");
    }
}

#[test]
fn an_incremental_reparse_reuses_the_unchanged_part_of_the_tree() {
    let mut text = buffer("fn first() {}\nfn second() {\n}\n");
    let first = analyze(&text);
    let unchanged = first
        .tree()
        .0
        .root_node()
        .child(0)
        .expect("the source holds two items")
        .id();

    // The change belongs to the second item, so the first item stays valid.
    let moved = insert(&mut text, first.tree(), 28, "    let value = 1;\n");
    let source: Arc<str> = Arc::from(text.to_string());
    let incremental = rust()
        .analyze(
            &AnalysisInput::new(text.version(), Arc::clone(&source)).reusing(moved),
            &CancellationToken::new(),
        )
        .expect("the source stays inside every bound");
    let complete = rust()
        .analyze(
            &AnalysisInput::new(text.version(), source),
            &CancellationToken::new(),
        )
        .expect("the source stays inside every bound");

    assert_eq!(
        incremental.tree().0.root_node().child(0).unwrap().id(),
        unchanged,
        "the reparse kept the node of the unchanged first item"
    );
    assert_ne!(
        complete.tree().0.root_node().child(0).unwrap().id(),
        unchanged,
        "a complete reparse builds every node again"
    );
    assert_eq!(incremental.highlights(), complete.highlights());
}

#[test]
fn malformed_syntax_still_produces_spans() {
    let analysis = analyze(&buffer("fn main( {\n    let = ;\n}\n"));

    assert!(
        analysis.highlights().iter().any(|span| span.line == 0),
        "a broken signature keeps its keyword span"
    );
    assert!(analysis.highlights().iter().any(|span| span.line == 1));
}

#[test]
fn a_span_of_a_multibyte_line_keeps_character_boundaries() {
    let text = "let name = \"日本語\";\nlet wide = '漢';\n";
    let analysis = analyze(&buffer(text));
    let lines: Vec<&str> = text.lines().collect();

    let mut found = 0;
    for span in analysis.highlights() {
        let line = lines[span.line as usize];
        let start = span.start_byte as usize;
        let end = span.end_byte as usize;
        assert!(end <= line.len(), "{span:?} leaves its line");
        assert!(
            line.is_char_boundary(start) && line.is_char_boundary(end),
            "{span:?} splits a character"
        );
        if span.role == SyntaxRole::String {
            found += 1;
        }
    }
    assert!(found > 0, "the multi-byte string keeps its role");
}

#[test]
fn the_indent_level_follows_the_syntax_tree() {
    let source = "fn main() {\n    if true {\n        let value = 1;\n    }\n}\n";
    let text = buffer(source);
    let analysis = analyze(&text);
    let byte = |needle: &str| source.find(needle).expect("the test source holds the text");

    // A new line at the end of the signature line enters the function block.
    assert_eq!(analysis.indent_level(byte("\n    if")).unwrap().get(), 1);
    // A new line inside the nested block gains the second level.
    assert_eq!(
        analysis.indent_level(byte("\n        let")).unwrap().get(),
        2
    );
    // A new line before a closing delimiter loses one level again.
    assert_eq!(analysis.indent_level(byte("    }")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(byte("}\n") + 4).unwrap().get(), 0);
    // A position outside every block takes no indent.
    assert_eq!(analysis.indent_level(0).unwrap().get(), 0);
}

#[test]
fn an_indent_query_outside_the_source_is_rejected() {
    let analysis = analyze(&buffer("fn main() {}\n"));

    assert_eq!(
        analysis.indent_level(1024).unwrap_err(),
        AnalysisError::MalformedOutput
    );
}

#[test]
fn a_deeper_tree_than_the_depth_bound_is_rejected() {
    let nesting = ANALYSIS_DEPTH_MAX + 8;
    let source = format!(
        "fn main() {}{}{}\n",
        "{".repeat(nesting),
        "}".repeat(nesting),
        "}"
    );
    let analysis = analyze(&buffer(&source));

    let deepest = source.rfind('{').expect("the source holds braces") + 1;
    assert!(matches!(
        analysis.indent_level(deepest),
        Err(AnalysisError::Bounds {
            measure: BoundMeasure::Depth,
            ..
        })
    ));
}

#[test]
fn an_oversized_source_is_rejected_before_the_parse() {
    let source = "a".repeat(ANALYSIS_SOURCE_BYTES_MAX + 1);

    assert!(matches!(
        analysis_error(&source),
        AnalysisError::Bounds {
            measure: BoundMeasure::Bytes,
            limit: ANALYSIS_SOURCE_BYTES_MAX,
            ..
        }
    ));
}

#[test]
fn a_source_with_too_many_lines_is_rejected_before_the_parse() {
    let source = "\n".repeat(ANALYSIS_SOURCE_LINES_MAX + 1);

    assert!(matches!(
        analysis_error(&source),
        AnalysisError::Bounds {
            measure: BoundMeasure::Lines,
            limit: ANALYSIS_SOURCE_LINES_MAX,
            ..
        }
    ));
}

#[test]
fn a_source_with_too_many_spans_is_rejected_as_a_complete_result() {
    // Each line produces several spans, so the repetition passes the span bound
    // long before it reaches the byte bound or the line bound.
    let source = "let alpha = beta(1, gamma);\n".repeat(20_000);

    assert!(matches!(
        analysis_error(&source),
        AnalysisError::Bounds {
            measure: BoundMeasure::HighlightSpans,
            ..
        }
    ));
}

#[test]
fn a_cancelled_request_returns_no_result() {
    let text = buffer("fn main() {}\n");
    let input = AnalysisInput::new(text.version(), Arc::from(text.to_string()));
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert_eq!(
        rust().analyze(&input, &cancellation).unwrap_err(),
        AnalysisError::Cancelled
    );
}

#[test]
fn an_obsolete_result_never_enters_the_cache() {
    let mut text = buffer("fn main() {}\n");
    let stale = analyze(&text);
    let at = text.char_position(0).expect("the position exists");
    text.apply(EditTransaction::single(
        at,
        TextChange::insert(at, "// note\n"),
    ))
    .expect("the position fits");
    let mut syntax = BufferSyntax::new();

    assert_eq!(
        syntax.accept(text.version(), stale),
        Publication::Rejected,
        "a result for an older buffer version is obsolete"
    );
    assert!(syntax.analysis().is_none());
    assert!(syntax.highlights().is_empty());
    assert!(syntax.indent_level(text.version(), 0).is_none());

    let current = analyze(&text);
    assert_eq!(
        syntax.accept(text.version(), current),
        Publication::Accepted
    );
    assert!(!syntax.highlights().is_empty());
}

#[test]
fn an_indent_query_answers_only_for_the_current_buffer_version() {
    let mut text = buffer("fn main() {\n}\n");
    let mut syntax = BufferSyntax::new();
    let analysis = analyze(&text);
    assert_eq!(
        syntax.accept(text.version(), analysis),
        Publication::Accepted
    );
    assert_eq!(syntax.indent_level(text.version(), 11).unwrap().get(), 1);

    let at = text.char_position(11).expect("the position exists");
    text.apply(EditTransaction::single(
        at,
        TextChange::insert(at, "\n    "),
    ))
    .expect("the position fits");

    // The accepted result belongs to the previous version, so the editor uses
    // the fallback rule instead of waiting for the next parse.
    assert!(syntax.indent_level(text.version(), 11).is_none());
    // Decoration keeps the previous spans while the next analysis runs.
    assert!(!syntax.highlights().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_worker_service_runs_one_analysis_off_the_event_loop() {
    let text = buffer("fn main() {\n    let value = 1;\n}\n");
    let input = AnalysisInput::new(text.version(), Arc::from(text.to_string()));
    let adapter = rust();
    let limits = RuntimeLimits::new(4, 1, 1).expect("the literal limits are not zero");
    let (runtime, mut events) = Runtime::with_limits(limits);
    let gate = PublicationGate::default();
    let request = gate.begin(RequestSlot::new(1), &runtime.cancellation_root());

    runtime
        .submit_worker(request, ANALYSIS_DEADLINE, move |cancellation| {
            adapter.analyze(&input, &cancellation)
        })
        .expect("the worker service holds one free permit");
    let event = events
        .recv()
        .await
        .expect("the request produces one result");

    let analysis = event
        .result
        .expect("the worker finished inside the deadline")
        .expect("the source stays inside every bound");
    assert!(!analysis.highlights().is_empty());
    assert_eq!(analysis.version(), text.version());
    runtime.shutdown().await;
}

#[test]
fn a_second_adapter_adds_a_language_without_a_change_above_the_trait() {
    let registry = LanguageRegistry::new(&TWO_LANGUAGES);

    let second = registry
        .adapter(Path::new("notes.kv"))
        .expect("the second adapter owns the extension");
    assert_eq!(second.id(), "second");
    assert_eq!(second.comment().line_token(), Some("#"));
    assert_eq!(second.comment().block(), None);
    assert_eq!(
        registry.adapter(Path::new("main.rs")).unwrap().id(),
        "rust",
        "the Rust adapter keeps its own paths"
    );

    // The analysis reads adapter data only, so the second language reaches the
    // same parse, the same bounds, and the same roles.
    let text = buffer("fn main() {\n}\n");
    let input = AnalysisInput::new(text.version(), Arc::from(text.to_string()));
    let analysis = second
        .analyze(&input, &CancellationToken::new())
        .expect("the source stays inside every bound");
    assert!(!analysis.highlights().is_empty());
    assert_eq!(analysis.indent_level(11).unwrap().get(), 1);
}

#[test]
fn two_adapters_that_claim_one_path_are_an_ambiguous_failure() {
    let registry = LanguageRegistry::new(&AMBIGUOUS);

    assert_eq!(
        registry.adapter(Path::new("main.rs")).err(),
        Some(AnalysisError::AmbiguousPath)
    );
}

#[test]
fn every_registered_extension_selects_its_adapter() {
    let registry = LanguageRegistry::first_release();

    for (path, id) in [
        ("Cargo.toml", "toml"),
        ("docs/notes.markdown", "markdown"),
        ("flake.nix", "nix"),
        ("package.json", "json"),
        ("README.md", "markdown"),
        ("src/main.rs", "rust"),
    ] {
        assert_eq!(
            registry
                .adapter(Path::new(path))
                .map(LanguageAdapter::id)
                .ok(),
            Some(id),
            "{path} belongs to the {id} adapter"
        );
    }
}

#[test]
fn every_registered_adapter_declares_a_valid_server_table() {
    let registry = LanguageRegistry::first_release();

    for path in [
        "Cargo.toml",
        "flake.nix",
        "package.json",
        "README.md",
        "src/main.rs",
    ] {
        let adapter = registry
            .adapter(Path::new(path))
            .expect("the path belongs to one adapter");
        assert!(
            declarations_are_valid(adapter.language_servers()),
            "{path} declares at most {LANGUAGE_SERVERS_MAX} servers, names each identifier \
             once, and carries at most one formatting server",
        );
        assert_eq!(
            adapter.formatter().is_some(),
            !adapter.language_servers().is_empty(),
            "{path} formats through the one server that carries the formatting role",
        );
    }
}

#[test]
fn a_file_name_selects_an_adapter_as_an_extension_does() {
    let registry = LanguageRegistry::first_release();

    // A lock file in the JSON format carries the extension of its tool, not
    // the extension of its format.
    assert_eq!(
        registry.adapter(Path::new("flake.lock")).unwrap().id(),
        "json"
    );
    assert_eq!(
        registry
            .adapter(Path::new("nested/flake.lock"))
            .unwrap()
            .id(),
        "json",
        "the name rule reads the file name, never the directory"
    );
    // The name is exact, so another lock file reaches no adapter.
    assert_eq!(
        registry.adapter(Path::new("Cargo.lock")).err(),
        Some(AnalysisError::UnsupportedPath)
    );
}

#[test]
fn every_registered_language_produces_terminal_independent_roles() {
    let toml = analyze_path("Cargo.toml", "# note\n[package]\nname = \"kvim\"\n");
    assert_eq!(roles(&toml, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&toml, 2).contains(&SyntaxRole::String));

    let nix = analyze_path("flake.nix", "# note\n{ value = 1; }\n");
    assert_eq!(roles(&nix, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&nix, 1).contains(&SyntaxRole::Number));

    let json = analyze_path("flake.lock", "{\n  \"nodes\": 1\n}\n");
    assert!(roles(&json, 1).contains(&SyntaxRole::String));
    assert!(roles(&json, 1).contains(&SyntaxRole::Number));

    let markdown = analyze_path("README.md", "# Title\n\ntext\n");
    assert!(roles(&markdown, 0).contains(&SyntaxRole::Type));
}

#[test]
fn each_language_reports_its_own_comment_token() {
    let registry = LanguageRegistry::first_release();

    for (path, token) in [("Cargo.toml", "#"), ("flake.nix", "#"), ("main.rs", "//")] {
        assert_eq!(
            registry
                .adapter(Path::new(path))
                .unwrap()
                .comment()
                .line_token(),
            Some(token),
            "{path} carries its own line comment"
        );
    }
}

#[test]
fn a_language_without_a_comment_keeps_the_toggle_disabled() {
    let registry = LanguageRegistry::first_release();

    // JSON and Markdown define no comment, so the toggle finds no token and
    // reports the same reason that a file without an adapter reports.
    for path in ["flake.lock", "package.json", "README.md"] {
        let comment = registry.adapter(Path::new(path)).unwrap().comment();
        assert_eq!(comment.line_token(), None, "{path} has no line comment");
        assert_eq!(comment.block(), None, "{path} has no block comment");
    }
}
