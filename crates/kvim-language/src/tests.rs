//! Behavior tests for the language registry and the Tree-sitter adapters.

use std::ffi::OsString;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use kvim_core::{EditTransaction, TextBuffer, TextChange};
use kvim_runtime::{ProcessOutput, PublicationGate, RequestSlot, Runtime, RuntimeLimits};
use kvim_settings::FileSettings;

use super::formatter::declaration_is_valid;
use kvim_syntax::NeverCancelled;

use super::server::declarations_are_valid;
use super::{
    ANALYSIS_DEADLINE, ANALYSIS_DEPTH_MAX, ANALYSIS_HIGHLIGHT_SPANS_MAX, ANALYSIS_SOURCE_BYTES_MAX,
    ANALYSIS_SOURCE_LINES_MAX, Analysis, AnalysisError, AnalysisInput, BoundMeasure, BufferSyntax,
    CommentStyle, FORMATTER_ARGS_MAX, FORMATTER_DEADLINE, FORMATTER_OUTPUT_BYTES_MAX,
    FormattedDocument, FormatterArgument, FormatterDeclaration, FormatterFailure, FormatterRequest,
    Grammar, HighlightLimits, IndentRule, IndentScope, JsonAdapter, LANGUAGE_ROOT_MARKERS_MAX,
    LANGUAGE_SERVERS_MAX, LanguageAdapter, LanguageCatalogEntry, LanguageFormatter,
    LanguageRegistry, MarkdownAdapter, NixAdapter, Publication, RustAdapter, ServerFormatting,
    SyntaxHighlighter, SyntaxRole, SyntaxTree,
};

/// The node kind that a test adapter indents.
const TEST_INDENT_SCOPES: [IndentScope; 1] = [IndentScope::whole("block")];

/// The number of columns that one indent level takes in a test adapter.
///
/// The value matches the four-column default of the settings, so no test
/// measures a language convention of its own.
const TEST_INDENT_WIDTH: NonZeroU8 = NonZeroU8::new(4).expect("the literal 4 is not zero");

/// A second adapter that proves the multi-language seam.
///
/// The adapter supplies its own extension, its own comment token, and its own
/// indent rule. It reuses the bundled grammar, because this release adds no
/// second grammar. Nothing above the trait changes for it.
#[derive(Clone, Copy, Debug)]
struct SecondAdapter;

/// The extensions of the second language.
static SECOND_EXTENSIONS: [&str; 1] = ["kv"];

/// The catalog entry of the second language.
static SECOND_CATALOG: LanguageCatalogEntry =
    LanguageCatalogEntry::new("second", &[], &SECOND_EXTENSIONS, &[], second_grammar);

/// Returns the grammar of the second language.
///
/// The language reuses the bundled Rust grammar, because this release adds no
/// second grammar.
fn second_grammar() -> Grammar {
    RustAdapter::new().grammar()
}

impl LanguageAdapter for SecondAdapter {
    fn catalog(&self) -> &'static LanguageCatalogEntry {
        &SECOND_CATALOG
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("#"), None)
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &TEST_INDENT_SCOPES,
            width: TEST_INDENT_WIDTH,
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
        .analyze(
            &input,
            &mut SyntaxHighlighter::new(),
            &CancellationToken::new(),
        )
        .expect("the test source stays inside every bound")
}

/// Analyzes one source text and returns the typed failure.
fn analysis_error(source: &str) -> AnalysisError {
    let input = AnalysisInput::new(buffer("").version(), Arc::from(source));
    match rust().analyze(
        &input,
        &mut SyntaxHighlighter::new(),
        &CancellationToken::new(),
    ) {
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
        .analyze(
            &input,
            &mut SyntaxHighlighter::new(),
            &CancellationToken::new(),
        )
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
            &mut SyntaxHighlighter::new(),
            &CancellationToken::new(),
        )
        .expect("the source stays inside every bound");
    let complete = rust()
        .analyze(
            &AnalysisInput::new(text.version(), source),
            &mut SyntaxHighlighter::new(),
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

/// Returns a dense literal list of one element count.
///
/// The Rust query captures the literal and the delimiter of each element, so
/// one element produces two spans. That result is the densest measured density
/// of one span for each source byte, so the source stays far below the byte
/// bound, the line bound, and the node bound.
fn dense_list(elements: usize) -> String {
    format!("static VALUES: [u8; 1] = [{}];\n", "1,".repeat(elements))
}

#[test]
fn a_source_with_too_many_spans_is_rejected_as_a_complete_result() {
    let source = dense_list(ANALYSIS_HIGHLIGHT_SPANS_MAX / 2 + 1024);

    assert!(matches!(
        analysis_error(&source),
        AnalysisError::Bounds {
            measure: BoundMeasure::HighlightSpans,
            limit: ANALYSIS_HIGHLIGHT_SPANS_MAX,
            ..
        }
    ));
}

#[test]
fn a_source_above_the_first_span_bound_keeps_its_highlighting() {
    // The first bound of 100000 spans made three real files above 1.6 MiB
    // render plain text. A denser result now returns a complete analysis.
    let first_bound = 100_000;
    let analysis = analyze(&buffer(&dense_list(first_bound)));

    assert!(
        analysis.highlights().len() > first_bound,
        "the analysis publishes more spans than the first bound allowed"
    );
}

#[test]
fn a_cancelled_request_returns_no_result() {
    let text = buffer("fn main() {}\n");
    let input = AnalysisInput::new(text.version(), Arc::from(text.to_string()));
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert_eq!(
        rust()
            .analyze(&input, &mut SyntaxHighlighter::new(), &cancellation)
            .unwrap_err(),
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
            adapter.analyze(&input, &mut SyntaxHighlighter::new(), &cancellation)
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
        .analyze(
            &input,
            &mut SyntaxHighlighter::new(),
            &CancellationToken::new(),
        )
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
        ("boot.S", "asm"),
        ("boot.s", "asm"),
        ("Cargo.toml", "toml"),
        ("cmd/main.go", "go"),
        ("home/.bashrc", "bash"),
        ("functions/greet.fish", "fish"),
        ("plugin/init.lua", "lua"),
        ("scripts/build.bash", "bash"),
        ("scripts/build.sh", "bash"),
        ("src/api.pyi", "python"),
        ("src/main.py", "python"),
        ("docs/notes.markdown", "markdown"),
        ("flake.nix", "nix"),
        ("include/api.h", "c"),
        ("include/api.hpp", "cpp"),
        ("package.json", "json"),
        ("README.md", "markdown"),
        ("shaders/light.frag", "glsl"),
        ("src/main.c", "c"),
        ("src/main.cpp", "cpp"),
        ("src/main.rs", "rust"),
        ("src/main.zig", "zig"),
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
    for adapter in LanguageRegistry::first_release().adapters() {
        let id = adapter.id();
        assert!(
            declarations_are_valid(adapter.language_servers()),
            "the {id} adapter declares at most {LANGUAGE_SERVERS_MAX} servers, names each \
             identifier once, carries at most one formatting server, and names at most \
             {LANGUAGE_ROOT_MARKERS_MAX} valid root markers for each server",
        );
        assert!(
            adapter
                .language_servers()
                .iter()
                .filter(|declaration| declaration.formatting == ServerFormatting::Enabled)
                .count()
                <= 1,
            "the {id} adapter names at most one server that formats its buffers",
        );
    }
}

#[test]
fn every_registered_adapter_highlights_through_its_catalog_entry() {
    // One highlighter serves every adapter of the registry, so this sweep
    // proves that each grammar of the build still compiles and that the shared
    // cache keeps one compiled query for each language.
    let mut highlighter = SyntaxHighlighter::new();
    let adapters = LanguageRegistry::first_release().adapters();
    for adapter in adapters {
        let entry = adapter.catalog();
        let id = entry.id();
        assert_eq!(
            id,
            adapter.id(),
            "the {id} adapter and its catalog entry answer to one identifier",
        );
        // Every grammar reads an empty fragment, so the sweep compiles the
        // query of each language without depending on its syntax.
        highlighter
            .highlight(entry, "", &HighlightLimits::default(), &NeverCancelled)
            .unwrap_or_else(|failure| {
                panic!("the {id} grammar compiles its highlight query: {failure}")
            });
    }
    assert_eq!(
        highlighter.cached_languages(),
        adapters.len(),
        "the shared cache keeps one compiled query for each registered language",
    );
}

#[test]
fn no_two_adapters_of_the_registry_claim_one_lookup_key() {
    let registry = LanguageRegistry::first_release();

    // Two adapters that claim one key make every path of that key an ambiguous
    // failure, which leaves the buffer without highlighting, without a server,
    // and without a formatter. The probe reads the real selection path, so it
    // covers the extension key and the file name key together.
    for adapter in registry.adapters() {
        let id = adapter.id();
        for extension in adapter.extensions() {
            let path = format!("probe.{extension}");
            assert_eq!(
                registry
                    .adapter(Path::new(&path))
                    .map(LanguageAdapter::id)
                    .ok(),
                Some(id),
                "the {id} adapter owns the .{extension} extension alone",
            );
        }
        for name in adapter.file_names() {
            assert_eq!(
                registry
                    .adapter(Path::new(name))
                    .map(LanguageAdapter::id)
                    .ok(),
                Some(id),
                "the {id} adapter owns the {name} file name alone",
            );
        }
        for name in adapter.language_names() {
            assert_eq!(
                *name,
                name.to_ascii_lowercase(),
                "the {id} adapter declares every language name in lower case, which the folding match assumes",
            );
            assert_eq!(
                registry.adapter_of_language(name).map(LanguageAdapter::id),
                Some(id),
                "the {id} adapter owns the {name} language name alone",
            );
            assert_eq!(
                registry
                    .adapter_of_language(&name.to_ascii_uppercase())
                    .map(LanguageAdapter::id),
                Some(id),
                "the {name} language name reaches the {id} adapter through the case fold",
            );
        }
    }
}

#[test]
fn every_adapter_of_the_registry_answers_to_its_own_identifier() {
    let registry = LanguageRegistry::first_release();

    // A fence names a language, and the identifier of an adapter is the name of
    // that language. An adapter that dropped the name would leave every fence
    // of its language plain.
    for adapter in registry.adapters() {
        let id = adapter.id();
        assert!(
            adapter.language_names().contains(&id),
            "the {id} adapter answers to its own identifier",
        );
    }
}

#[test]
fn a_language_name_selects_the_adapter_of_that_language() {
    let registry = LanguageRegistry::first_release();

    assert_eq!(
        registry
            .adapter_of_language("rust")
            .map(LanguageAdapter::id),
        Some("rust")
    );
    // A declared alias reaches the same adapter as the name of the language.
    assert_eq!(
        registry.adapter_of_language("rs").map(LanguageAdapter::id),
        Some("rust")
    );
    assert_eq!(
        registry.adapter_of_language("c++").map(LanguageAdapter::id),
        Some("cpp")
    );
    // The match folds ASCII case, because the name is server text.
    assert_eq!(
        registry
            .adapter_of_language("Rust")
            .map(LanguageAdapter::id),
        Some("rust")
    );
    // Two grammars read the JSX syntax, so the two names reach two adapters.
    assert_eq!(
        registry.adapter_of_language("jsx").map(LanguageAdapter::id),
        Some("javascript")
    );
    assert_eq!(
        registry.adapter_of_language("tsx").map(LanguageAdapter::id),
        Some("tsx")
    );
}

#[test]
fn an_unknown_language_name_selects_nothing() {
    let registry = LanguageRegistry::first_release();

    // A fence may name any language of the world, so an unknown name is no
    // failure. It selects nothing, and the fence stays plain.
    assert!(registry.adapter_of_language("console").is_none());
    assert!(registry.adapter_of_language("text").is_none());
    assert!(registry.adapter_of_language("klingon").is_none());
    assert!(registry.adapter_of_language("").is_none());
    // The registry reads one complete name. A CommonMark info string may carry
    // an attribute after the name, and the reader of the fence extracts it.
    assert!(registry.adapter_of_language("rust,ignore").is_none());
    assert!(registry.adapter_of_language("rust title=\"x\"").is_none());
    // A long name selects nothing, because no declared name holds its length.
    let hostile = "rust".repeat(4096);
    assert!(registry.adapter_of_language(&hostile).is_none());
}

#[test]
fn a_language_name_selects_no_path() {
    let registry = LanguageRegistry::first_release();

    // The name key carries no path, so a file that is named after a language
    // reaches no adapter, and the two path keys keep their own answers.
    assert_eq!(
        registry.adapter(Path::new("rust")).err(),
        Some(AnalysisError::UnsupportedPath)
    );
    assert_eq!(
        registry.adapter(Path::new("python")).err(),
        Some(AnalysisError::UnsupportedPath)
    );
    assert_eq!(
        registry
            .adapter(Path::new("src/main.rs"))
            .map(LanguageAdapter::id)
            .ok(),
        Some("rust")
    );
    assert_eq!(
        registry
            .adapter(Path::new("flake.lock"))
            .map(LanguageAdapter::id)
            .ok(),
        Some("json")
    );
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

    for (path, token) in [
        ("Cargo.toml", "#"),
        ("build.sh", "#"),
        ("flake.nix", "#"),
        ("greet.fish", "#"),
        ("init.lua", "--"),
        ("main.py", "#"),
        ("main.rs", "//"),
        ("main.ts", "//"),
        ("site.scss", "//"),
    ] {
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

/// One C source that carries a comment, a nested block, and a literal.
const C_SOURCE: &str =
    "// note\nint main(void) {\n    if (1) {\n        int value = 1;\n    }\n}\n";

/// One C++ source that carries a namespace, a class, and a method.
const CPP_SOURCE: &str = "// note\nnamespace app {\nclass Shape {\n  public:\n    int area() {\n        return 1;\n    }\n};\n}\n";

/// One Zig source that carries a comment, a nested block, and a literal.
const ZIG_SOURCE: &str =
    "// note\npub fn main() void {\n    if (true) {\n        var value: u8 = 1;\n    }\n}\n";

/// One Go source that carries a comment, a switch, and one case.
const GO_SOURCE: &str =
    "// note\npackage main\n\nfunc main() {\n\tswitch 1 {\n\tcase 1:\n\t\tprintln(1)\n\t}\n}\n";

/// One assembly source that carries a comment, a label, and one instruction.
const ASM_SOURCE: &str = "# note\n_start:\n    mov $1, %rax\n";

/// One GLSL source that carries a comment, a nested block, and a literal.
const GLSL_SOURCE: &str =
    "// note\nvoid main() {\n    if (true) {\n        float value = 1.0;\n    }\n}\n";

/// One Python source that carries a comment, a nested suite, and a literal.
const PYTHON_SOURCE: &str = "# note\ndef main():\n    if True:\n        value = 1\n    return 0\n";

/// One Python source whose list spans several lines.
const PYTHON_LIST_SOURCE: &str = "values = [\n    1,\n]\n";

/// One Bash source that carries a comment, a nested block, and a literal.
const BASH_SOURCE: &str =
    "# note\nmain() {\n    if [ -n \"$1\" ]; then\n        echo 1\n    fi\n}\n";

/// One fish source that carries a comment, a nested block, and a literal.
const FISH_SOURCE: &str =
    "# note\nfunction main\n    if test -n \"$argv\"\n        echo 1\n    end\nend\n";

/// One Lua source that carries a comment, a nested block, and a table.
const LUA_SOURCE: &str = "-- note\nlocal function main()\n    if true then\n        local value = \
                          1\n    end\n    return { a = 1 }\nend\n";

#[test]
fn c_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("src/main.c", C_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Type));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::Number));
}

#[test]
fn the_c_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("src/main.c", C_SOURCE);
    let byte = |needle: &str| {
        C_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        C_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // A new line at the end of the signature line enters the function block.
    assert_eq!(analysis.indent_level(byte("\n    if")).unwrap().get(), 1);
    // A new line inside the nested block gains the second level.
    assert_eq!(
        analysis.indent_level(byte("\n        int")).unwrap().get(),
        2
    );
    // A line that starts with a closing delimiter loses one level again.
    assert_eq!(analysis.indent_level(byte("    }")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(last("}\n")).unwrap().get(), 0);
}

#[test]
fn cpp_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("src/main.cpp", CPP_SOURCE);

    // The comment pattern belongs to the C query, and the class name pattern
    // belongs to the C++ query, so both roles prove the joined query.
    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Type));
    assert!(roles(&analysis, 5).contains(&SyntaxRole::Number));
}

#[test]
fn the_cpp_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("src/main.cpp", CPP_SOURCE);
    let byte = |needle: &str| {
        CPP_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };

    // A new line at the end of the namespace line enters its declaration list.
    assert_eq!(analysis.indent_level(byte("\nclass")).unwrap().get(), 1);
    // The method body sits inside the namespace, the class, and the method.
    assert_eq!(
        analysis
            .indent_level(byte("\n        return"))
            .unwrap()
            .get(),
        3
    );
    // A line that starts with a closing delimiter loses one level again.
    assert_eq!(analysis.indent_level(byte("    }")).unwrap().get(), 2);
    assert_eq!(analysis.indent_level(byte("};")).unwrap().get(), 1);
}

#[test]
fn zig_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("src/main.zig", ZIG_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::Number));
}

#[test]
fn the_zig_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("src/main.zig", ZIG_SOURCE);
    let byte = |needle: &str| {
        ZIG_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        ZIG_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // A new line at the end of the signature line enters the function block.
    assert_eq!(analysis.indent_level(byte("\n    if")).unwrap().get(), 1);
    // A new line inside the nested block gains the second level.
    assert_eq!(
        analysis.indent_level(byte("\n        var")).unwrap().get(),
        2
    );
    // A line that starts with a closing delimiter loses one level again.
    assert_eq!(analysis.indent_level(byte("    }")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(last("}\n")).unwrap().get(), 0);
}

#[test]
fn go_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("cmd/main.go", GO_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 6).contains(&SyntaxRole::Number));
}

#[test]
fn the_go_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("cmd/main.go", GO_SOURCE);
    let byte = |needle: &str| {
        GO_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        GO_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // A new line at the end of the signature line enters the function block.
    assert_eq!(analysis.indent_level(byte("\n\tswitch")).unwrap().get(), 1);
    // A case label keeps the level of its switch, because the switch itself
    // holds no indent scope.
    assert_eq!(analysis.indent_level(byte("\n\tcase")).unwrap().get(), 1);
    // The statements of one case take one more level.
    assert_eq!(
        analysis.indent_level(byte("\n\t\tprintln")).unwrap().get(),
        2
    );
    assert_eq!(analysis.indent_level(last("}\n")).unwrap().get(), 0);
}

#[test]
fn asm_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("boot.s", ASM_SOURCE);

    // The grammar marks a comment twice, so this row also proves that a
    // decoration capture never takes the place of a role.
    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Statement));
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Function));
}

#[test]
fn the_asm_indent_level_stays_flat() {
    let analysis = analyze_path("boot.s", ASM_SOURCE);
    let byte = |needle: &str| {
        ASM_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };

    // Assembly nests through no bracketed node, so every position of the source
    // takes the same level and the user owns the layout.
    assert_eq!(analysis.indent_level(byte("\n_start")).unwrap().get(), 0);
    assert_eq!(analysis.indent_level(byte("\n    mov")).unwrap().get(), 0);
    assert_eq!(analysis.indent_level(byte("    mov")).unwrap().get(), 0);
}

#[test]
fn glsl_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("shaders/light.frag", GLSL_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Type));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::Number));
}

#[test]
fn the_glsl_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("shaders/light.frag", GLSL_SOURCE);
    let byte = |needle: &str| {
        GLSL_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        GLSL_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // A new line at the end of the signature line enters the function block.
    assert_eq!(analysis.indent_level(byte("\n    if")).unwrap().get(), 1);
    // A new line inside the nested block gains the second level.
    assert_eq!(
        analysis
            .indent_level(byte("\n        float"))
            .unwrap()
            .get(),
        2
    );
    // A line that starts with a closing delimiter loses one level again.
    assert_eq!(analysis.indent_level(byte("    }")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(last("}\n")).unwrap().get(), 0);
}

#[test]
fn python_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("src/main.py", PYTHON_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Function));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::Number));
}

#[test]
fn the_python_indent_level_follows_the_compound_statements() {
    let analysis = analyze_path("src/main.py", PYTHON_SOURCE);
    let byte = |needle: &str| {
        PYTHON_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };

    // A line that opens a suite gains one level, because the compound statement
    // that owns the suite already encloses the end of its own header line.
    assert_eq!(analysis.indent_level(byte("\n    if")).unwrap().get(), 1);
    assert_eq!(
        analysis
            .indent_level(byte("\n        value"))
            .unwrap()
            .get(),
        2
    );
    // Python closes a suite with no delimiter, so the compound statement ends at
    // the last line of its suite. Both rows below therefore report one level too
    // few, which is the documented limit of the Python indent rule.
    assert_eq!(
        analysis.indent_level(byte("\n    return")).unwrap().get(),
        1
    );
    assert_eq!(
        analysis
            .indent_level(PYTHON_SOURCE.len() - 1)
            .unwrap()
            .get(),
        0
    );
}

#[test]
fn a_bracketed_python_expression_indents_like_a_brace_language() {
    let analysis = analyze_path("src/main.py", PYTHON_LIST_SOURCE);
    let byte = |needle: &str| {
        PYTHON_LIST_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };

    // A bracket carries its own opening and closing character, so the list
    // behaves exactly as the equivalent node of a brace language.
    assert_eq!(analysis.indent_level(byte("\n    1,")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(byte("]")).unwrap().get(), 0);
}

#[test]
fn bash_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("scripts/build.sh", BASH_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 2).contains(&SyntaxRole::String));
    // The Bash query names every command word a function and captures no
    // numeric literal, so the argument of `echo` carries the function role.
    assert!(roles(&analysis, 3).contains(&SyntaxRole::Function));
}

#[test]
fn the_bash_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("scripts/build.sh", BASH_SOURCE);
    let byte = |needle: &str| {
        BASH_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        BASH_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // Every compound statement of the shell carries its own terminator, so each
    // one spans its complete body and every row below is exact.
    assert_eq!(analysis.indent_level(byte("\n    if")).unwrap().get(), 1);
    assert_eq!(
        analysis.indent_level(byte("\n        echo")).unwrap().get(),
        2
    );
    assert_eq!(analysis.indent_level(byte("\n    fi")).unwrap().get(), 2);
    assert_eq!(analysis.indent_level(byte("\n}")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(last("}")).unwrap().get(), 0);
}

#[test]
fn fish_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("functions/main.fish", FISH_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::Number));
}

#[test]
fn the_fish_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("functions/main.fish", FISH_SOURCE);
    let byte = |needle: &str| {
        FISH_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        FISH_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // Every compound statement of fish ends with the `end` keyword, so each one
    // spans its complete body and every row below is exact.
    assert_eq!(analysis.indent_level(byte("\n    if")).unwrap().get(), 1);
    assert_eq!(
        analysis.indent_level(byte("\n        echo")).unwrap().get(),
        2
    );
    assert_eq!(analysis.indent_level(byte("\n    end")).unwrap().get(), 2);
    assert_eq!(analysis.indent_level(byte("\nend")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(last("end") + 3).unwrap().get(), 0);
}

#[test]
fn lua_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("plugin/init.lua", LUA_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::Number));
    // The query names a table field with the older word of the shared
    // vocabulary, so this row also proves the extended role mapping.
    assert!(roles(&analysis, 5).contains(&SyntaxRole::Property));
}

#[test]
fn the_lua_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("plugin/init.lua", LUA_SOURCE);
    let byte = |needle: &str| {
        LUA_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        LUA_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // Every compound statement of Lua ends with the `end` keyword, so each one
    // spans its complete body and every row below is exact.
    assert_eq!(analysis.indent_level(byte("\n    if")).unwrap().get(), 1);
    assert_eq!(
        analysis
            .indent_level(byte("\n        local"))
            .unwrap()
            .get(),
        2
    );
    assert_eq!(analysis.indent_level(byte("\n    end")).unwrap().get(), 2);
    assert_eq!(
        analysis.indent_level(byte("\n    return")).unwrap().get(),
        1
    );
    assert_eq!(analysis.indent_level(byte("\nend")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(last("end") + 3).unwrap().get(), 0);
}

/// One HTML document that carries a comment, a nested element, and an attribute.
const HTML_SOURCE: &str = "<!-- note -->\n<html>\n    <body>\n        <p class=\"a\">text</p>\n    \
                           </body>\n</html>\n";

/// One CSS stylesheet that carries a comment, a nested block, and a literal.
const CSS_SOURCE: &str =
    "/* note */\ndiv .a {\n    top: 0;\n    margin: calc(\n        1px\n    );\n}\n";

/// One SCSS stylesheet that carries a comment, a nested rule, and a literal.
const SCSS_SOURCE: &str =
    "// note\n@mixin m($a) {\n    top: $a;\n    .b {\n        left: 0;\n    }\n}\n";

/// One JavaScript source that carries a comment, nested blocks, and a literal.
const JAVASCRIPT_SOURCE: &str = "// note\nfunction main(a) {\n    if (a) {\n        const o = \
                                 {\n            x: [\n                1,\n            ],\n        \
                                 };\n    }\n}\n";

/// One JavaScript source that carries the JSX dialect.
const JSX_SOURCE: &str = "const app = <div className=\"a\">text</div>;\n";

/// One TypeScript source that carries a comment, an interface, and a function.
const TYPESCRIPT_SOURCE: &str = "// note\ninterface Shape {\n    area: number;\n}\n\nfunction \
                                 main(value: Shape): number {\n    return value.area;\n}\n";

/// One TSX source that carries a comment and a nested JSX element.
const TSX_SOURCE: &str = "// note\nconst app = (\n    <div className=\"a\">\n        \
                          <span>text</span>\n    </div>\n);\n";

#[test]
fn html_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("public/index.html", HTML_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    // The markup grammars name an element with the `tag` word of the shared
    // vocabulary, so this row also proves the extended role mapping.
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Type));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::Attribute));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::String));
}

#[test]
fn the_html_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("public/index.html", HTML_SOURCE);
    let byte = |needle: &str| {
        HTML_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };

    // An element spans its start tag, its content, and its end tag, so a new
    // line inside it gains one level for each enclosing element.
    assert_eq!(
        analysis.indent_level(byte("\n    <body>")).unwrap().get(),
        1
    );
    assert_eq!(
        analysis.indent_level(byte("\n        <p")).unwrap().get(),
        2
    );
    // An end tag opens with the same character as a start tag, so no closing
    // delimiter separates the two. Both rows below therefore report one level
    // too many, which is the documented limit of the HTML indent rule.
    assert_eq!(
        analysis.indent_level(byte("\n    </body>")).unwrap().get(),
        2
    );
    assert_eq!(analysis.indent_level(byte("\n</html>")).unwrap().get(), 1);
}

#[test]
fn css_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("assets/site.css", CSS_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    // A tag selector names an element, so it carries the same role as an HTML
    // tag name.
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Type));
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Property));
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Number));
}

#[test]
fn the_css_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("assets/site.css", CSS_SOURCE);
    let byte = |needle: &str| {
        CSS_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        CSS_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // Every scope of CSS carries its own opening and closing character, so
    // every row below is exact.
    assert_eq!(analysis.indent_level(byte("\n    top")).unwrap().get(), 1);
    assert_eq!(
        analysis.indent_level(byte("\n        1px")).unwrap().get(),
        2
    );
    assert_eq!(analysis.indent_level(byte("    );")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(last("}")).unwrap().get(), 0);
}

#[test]
fn scss_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("styles/site.scss", SCSS_SOURCE);

    // The SCSS query marks a comment twice, and the second name carries no
    // role, so this row also proves that the highlighter keeps the first name.
    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    // The crate ships the SCSS patterns alone, so a property name and a literal
    // prove that the adapter joined the CSS patterns to them.
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Property));
    assert!(roles(&analysis, 4).contains(&SyntaxRole::Number));
}

#[test]
fn the_scss_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("styles/site.scss", SCSS_SOURCE);
    let byte = |needle: &str| {
        SCSS_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        SCSS_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // Every scope of SCSS carries its own opening and closing character, so
    // every row below is exact.
    assert_eq!(analysis.indent_level(byte("\n    top")).unwrap().get(), 1);
    assert_eq!(
        analysis.indent_level(byte("\n        left")).unwrap().get(),
        2
    );
    assert_eq!(analysis.indent_level(byte("    }")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(last("}")).unwrap().get(), 0);
}

#[test]
fn javascript_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("src/main.js", JAVASCRIPT_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Function));
    assert!(roles(&analysis, 5).contains(&SyntaxRole::Number));
}

#[test]
fn the_javascript_adapter_reads_the_jsx_dialect() {
    // One grammar reads both dialects, so the `jsx` extension needs no adapter
    // of its own. The adapter joins the JSX patterns to the JavaScript
    // patterns, so a JSX element highlights in both files.
    for path in ["src/main.js", "src/app.jsx"] {
        let analysis = analyze_path(path, JSX_SOURCE);
        assert!(
            roles(&analysis, 0).contains(&SyntaxRole::Type),
            "{path} highlights the JSX element name"
        );
        assert!(
            roles(&analysis, 0).contains(&SyntaxRole::Attribute),
            "{path} highlights the JSX attribute name"
        );
    }
}

#[test]
fn the_javascript_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("src/main.js", JAVASCRIPT_SOURCE);
    let byte = |needle: &str| {
        JAVASCRIPT_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        JAVASCRIPT_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // Every scope of JavaScript carries its own opening and closing character,
    // so every row below is exact.
    assert_eq!(analysis.indent_level(byte("\n    if")).unwrap().get(), 1);
    assert_eq!(
        analysis
            .indent_level(byte("\n        const"))
            .unwrap()
            .get(),
        2
    );
    assert_eq!(
        analysis
            .indent_level(byte("\n                1,"))
            .unwrap()
            .get(),
        4
    );
    assert_eq!(
        analysis.indent_level(byte("            ],")).unwrap().get(),
        3
    );
    assert_eq!(analysis.indent_level(last("}")).unwrap().get(), 0);
}

#[test]
fn typescript_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("src/main.ts", TYPESCRIPT_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Type));
    // The crate ships the type patterns alone, so a JavaScript keyword proves
    // that the adapter joined the JavaScript patterns to them.
    assert!(roles(&analysis, 5).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 5).contains(&SyntaxRole::Function));
}

#[test]
fn the_typescript_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("src/main.ts", TYPESCRIPT_SOURCE);
    let byte = |needle: &str| {
        TYPESCRIPT_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };

    // Every scope of TypeScript carries its own opening and closing character,
    // so every row below is exact.
    assert_eq!(analysis.indent_level(byte("\n    area")).unwrap().get(), 1);
    assert_eq!(
        analysis.indent_level(byte("}\n\nfunction")).unwrap().get(),
        0
    );
    assert_eq!(
        analysis.indent_level(byte("\n    return")).unwrap().get(),
        1
    );
}

#[test]
fn tsx_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("src/app.tsx", TSX_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Type));
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Attribute));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::Type));
}

#[test]
fn the_tsx_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("src/app.tsx", TSX_SOURCE);
    let byte = |needle: &str| {
        TSX_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };

    // A JSX element spans its opening element, its content, and its closing
    // element, so a new line inside it gains one level.
    assert_eq!(analysis.indent_level(byte("\n    <div")).unwrap().get(), 1);
    assert_eq!(
        analysis
            .indent_level(byte("\n        <span"))
            .unwrap()
            .get(),
        2
    );
    // A closing element opens with the same character as an opening element, so
    // the row below reports one level too many. That is the documented limit of
    // the TSX indent rule.
    assert_eq!(
        analysis.indent_level(byte("\n    </div>")).unwrap().get(),
        2
    );
    assert_eq!(analysis.indent_level(byte(");")).unwrap().get(), 0);
}

/// One YAML document that carries a comment, nested blocks, and a literal.
const YAML_SOURCE: &str = "# note\nroot:\n  a: 1\n  b: true\nlist:\n  - one\n  - name: x\n    \
                           id: 2\n";

/// One XML document that carries a comment, an attribute, and a nested element.
const XML_SOURCE: &str = "<!-- note -->\n<root attr=\"v\">\n    <child>text</child>\n</root>\n";

/// One Terraform configuration that carries a comment, a block, and an object.
const TERRAFORM_SOURCE: &str = "# note\nresource \"aws_s3_bucket\" \"b\" {\n    bucket = \
                                \"name\"\n    tags = {\n        env = \"dev\"\n    }\n}\n";

/// One SQL file that carries a comment, a column list, and a nested query.
const SQL_SOURCE: &str = "-- note\ncreate table users (\n    id int primary key,\n    name \
                          varchar(10)\n);\n\nselect id\nfrom users\nwhere id in (\n    select \
                          id\n    from admins\n);\n";

#[test]
fn yaml_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("deploy/values.yaml", YAML_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Property));
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Number));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::Boolean));
}

#[test]
fn the_yaml_indent_level_follows_the_block_entries() {
    let analysis = analyze_path("deploy/values.yaml", YAML_SOURCE);
    let byte = |needle: &str| {
        YAML_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };

    // An entry that owns a nested block supplies the level of that block, so
    // the first line of the block is exact.
    assert_eq!(analysis.indent_level(byte("\n  a: 1")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(byte("\n  b: true")).unwrap().get(), 1);
    assert_eq!(
        analysis.indent_level(byte("\n  - name: x")).unwrap().get(),
        1
    );
    assert_eq!(analysis.indent_level(byte("\n    id: 2")).unwrap().get(), 2);
}

#[test]
fn xml_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("build/pom.xml", XML_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    // A tag names the kind of an element, so it carries the type role that
    // every markup grammar of the registry shares.
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Type));
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Property));
    assert!(roles(&analysis, 1).contains(&SyntaxRole::String));
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Type));
}

#[test]
fn the_xml_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("build/pom.xml", XML_SOURCE);
    let byte = |needle: &str| {
        XML_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };

    // An element spans its start tag, its content, and its end tag, so a new
    // line inside it gains one level.
    assert_eq!(
        analysis.indent_level(byte("\n    <child>")).unwrap().get(),
        1
    );
    // An end tag opens with the same character as a start tag, so the row below
    // reports one level too many. That is the documented limit of the XML
    // indent rule.
    assert_eq!(analysis.indent_level(byte("\n</root>")).unwrap().get(), 1);
}

#[test]
fn terraform_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("infra/main.tf", TERRAFORM_SOURCE);

    // The grammar crate ships no query, so every row below also proves that the
    // vendored query compiled against this grammar and matched its nodes.
    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 1).contains(&SyntaxRole::String));
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Property));
    assert!(roles(&analysis, 4).contains(&SyntaxRole::Property));
}

#[test]
fn the_terraform_indent_level_follows_the_syntax_tree() {
    let analysis = analyze_path("infra/main.tf", TERRAFORM_SOURCE);
    let byte = |needle: &str| {
        TERRAFORM_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        TERRAFORM_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // Every scope of Terraform carries its own opening and closing character,
    // so every row below is exact.
    assert_eq!(
        analysis.indent_level(byte("\n    bucket")).unwrap().get(),
        1
    );
    assert_eq!(
        analysis.indent_level(byte("\n        env")).unwrap().get(),
        2
    );
    assert_eq!(analysis.indent_level(byte("    }")).unwrap().get(), 1);
    assert_eq!(analysis.indent_level(last("}")).unwrap().get(), 0);
}

#[test]
fn sql_source_produces_terminal_independent_roles() {
    let analysis = analyze_path("migrations/001_users.sql", SQL_SOURCE);

    assert_eq!(roles(&analysis, 0), vec![SyntaxRole::Comment]);
    assert!(roles(&analysis, 1).contains(&SyntaxRole::Keyword));
    assert!(roles(&analysis, 2).contains(&SyntaxRole::Type));
    assert!(roles(&analysis, 3).contains(&SyntaxRole::String));
    // A selected column is a field of its relation, so it takes the property
    // role of the shared vocabulary.
    assert!(roles(&analysis, 6).contains(&SyntaxRole::Property));
}

#[test]
fn the_sql_indent_level_follows_the_parenthesized_scopes() {
    let analysis = analyze_path("migrations/001_users.sql", SQL_SOURCE);
    let byte = |needle: &str| {
        SQL_SOURCE
            .find(needle)
            .expect("the test source holds the text")
    };
    let last = |needle: &str| {
        SQL_SOURCE
            .rfind(needle)
            .expect("the test source holds the text")
    };

    // Every scope of SQL carries its own opening and closing character, so
    // every row below is exact.
    assert_eq!(
        analysis.indent_level(byte("\n    id int")).unwrap().get(),
        1
    );
    assert_eq!(analysis.indent_level(byte("\n    name")).unwrap().get(), 1);
    assert_eq!(
        analysis.indent_level(byte("\n    select")).unwrap().get(),
        1
    );
    assert_eq!(
        analysis
            .indent_level(byte("\n    from admins"))
            .unwrap()
            .get(),
        1
    );
    assert_eq!(analysis.indent_level(last(")")).unwrap().get(), 0);
    // A select list carries no delimiter, so it takes no level of its own and
    // the user indents its continuation.
    assert_eq!(
        analysis.indent_level(byte("\nfrom users")).unwrap().get(),
        0
    );
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

/// A deterministic formatter for the process tests.
///
/// `tr` is a POSIX command, and it maps every lowercase letter onto its
/// uppercase letter. The tests therefore prove that the buffer reaches the
/// standard input of the program and that the formatted document returns on the
/// standard output, without a formatter of the host system.
const UPPERCASE_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "tr",
    args: &[
        FormatterArgument::Literal("a-z"),
        FormatterArgument::Literal("A-Z"),
    ],
};

/// A program that no host holds, which proves the missing-formatter state.
const MISSING_FORMATTER: FormatterDeclaration = FormatterDeclaration {
    program: "kvim-formatter-that-no-host-holds",
    args: &[],
};

/// Returns one captured process result.
fn process_output(status_code: Option<i32>, stdout: &[u8]) -> ProcessOutput {
    ProcessOutput {
        status_code,
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
}

/// Returns one run of the Nix formatter over the given buffer text.
fn nix_run(content: &str) -> (TextBuffer, FormatterRequest) {
    let text = buffer(content);
    let declaration = NixAdapter::new()
        .external_formatter()
        .expect("the Nix adapter declares a formatter");
    let request = FormatterRequest::new(
        declaration,
        PathBuf::from("/workspace/flake.nix"),
        text.version(),
        text.to_string(),
    );
    (text, request)
}

/// Runs one formatter through the bounded process service, exactly as the
/// terminal event loop does.
///
/// The call runs a real program, so it proves the arguments and the standard
/// input of [`FormatterRequest::command`]. A recorded output can never prove
/// them.
async fn run_formatter(
    request: FormatterRequest,
) -> Result<Option<FormattedDocument>, FormatterFailure> {
    let limits = RuntimeLimits::new(1, 1, 1).expect("every capacity is nonzero");
    let (runtime, mut events) = Runtime::<ProcessOutput>::with_limits(limits);
    let handle =
        PublicationGate::default().begin(RequestSlot::new(1), &runtime.cancellation_root());
    runtime
        .submit_process(handle, request.command(), |output| output)
        .expect("the isolated runtime holds one free permit");
    let event = events
        .recv()
        .await
        .expect("every accepted request produces one result");
    runtime.shutdown().await;
    match event.result {
        Ok(output) => request.publish(&output),
        Err(error) => Err(FormatterFailure::of(&error)),
    }
}

#[test]
fn an_external_formatter_takes_precedence_over_a_formatting_server() {
    // The Nix adapter declares both, so the precedence rule decides.
    let nix = NixAdapter::new();
    assert_eq!(
        nix.language_servers()[0].formatting,
        ServerFormatting::Enabled,
        "the declared server keeps the fallback role"
    );
    assert!(
        matches!(
            nix.formatter(),
            Some(LanguageFormatter::External(declaration)) if declaration.program == "nixfmt"
        ),
        "the declared program formats a Nix buffer"
    );

    // Rust declares no program, so its server formats the buffer.
    assert!(
        matches!(
            RustAdapter::new().formatter(),
            Some(LanguageFormatter::Server(declaration)) if declaration.id == "rust_analyzer"
        ),
        "a language without a program keeps its server formatter"
    );

    // An adapter that declares neither has no formatter at all.
    assert!(SecondAdapter.formatter().is_none());
}

#[test]
fn every_registered_adapter_declares_the_formatter_of_its_language() {
    let registry = LanguageRegistry::first_release();

    for (path, program, formats) in [
        ("boot.s", None, false),
        ("Cargo.toml", Some("taplo"), true),
        ("cmd/main.go", Some("goimports"), true),
        ("flake.nix", Some("nixfmt"), true),
        ("include/api.h", Some("clang-format"), true),
        ("package.json", Some("prettier"), true),
        ("README.md", Some("prettier"), true),
        ("shaders/light.frag", None, true),
        ("src/main.cpp", Some("clang-format"), true),
        ("src/main.rs", None, true),
        ("src/main.zig", None, true),
    ] {
        let adapter = registry
            .adapter(Path::new(path))
            .expect("the path belongs to one adapter");
        let declaration = adapter.external_formatter();
        assert_eq!(
            declaration.map(|declaration| declaration.program),
            program,
            "{path} declares its own external formatter",
        );
        assert!(
            declaration.is_none_or(declaration_is_valid),
            "{path} names a program and at most {FORMATTER_ARGS_MAX} arguments",
        );
        assert_eq!(
            adapter.formatter().is_some(),
            formats,
            "{path} formats through one of the two paths, or through neither",
        );
    }
}

#[test]
fn the_json_formatter_names_its_parser_for_a_path_whose_extension_does_not() {
    // `prettier` selects its parser from the extension of the document path.
    // The JSON adapter also owns a lock file whose extension names no format,
    // so the command must name the parser itself. A command without that name
    // reaches `prettier`, and `prettier` then refuses the document.
    let text = buffer("{\"a\":1}\n");
    let declaration = JsonAdapter::new()
        .external_formatter()
        .expect("the JSON adapter declares a formatter");

    let request = FormatterRequest::new(
        declaration,
        PathBuf::from("/workspace/flake.lock"),
        text.version(),
        text.to_string(),
    );
    let command = request.command();

    assert_eq!(command.program, "prettier");
    assert_eq!(
        command.args,
        vec![
            OsString::from("--parser"),
            OsString::from("json"),
            OsString::from("--stdin-filepath"),
            OsString::from("/workspace/flake.lock"),
        ],
        "the command names the parser, and it keeps the path that finds the configuration"
    );
}

#[test]
fn the_formatter_command_substitutes_the_document_path() {
    let text = buffer("#  Title\n");
    let declaration = MarkdownAdapter::new()
        .external_formatter()
        .expect("the Markdown adapter declares a formatter");

    let request = FormatterRequest::new(
        declaration,
        PathBuf::from("/workspace/notes.md"),
        text.version(),
        text.to_string(),
    );
    let command = request.command();

    assert_eq!(command.program, "prettier");
    assert_eq!(
        command.args,
        vec![
            OsString::from("--stdin-filepath"),
            OsString::from("/workspace/notes.md"),
        ],
        "prettier selects its parser from the document path"
    );
    assert_eq!(command.stdin, b"#  Title\n", "the buffer reaches stdin");
    assert_eq!(command.output_bytes_max, FORMATTER_OUTPUT_BYTES_MAX);
    assert_eq!(command.deadline, FORMATTER_DEADLINE);
}

#[test]
fn a_formatter_answer_that_kvim_cannot_use_changes_nothing() {
    let (_text, request) = nix_run("{  }\n");

    assert_eq!(
        request.publish(&process_output(Some(1), b"{ }\n")),
        Err(FormatterFailure::Unavailable),
        "a non-zero exit code reports a refusal"
    );
    assert_eq!(
        request.publish(&process_output(Some(0), &[0xff, 0xfe])),
        Err(FormatterFailure::Unavailable),
        "bytes that are not UTF-8 name no document"
    );
    assert_eq!(
        request.publish(&process_output(Some(0), b"")),
        Err(FormatterFailure::Unavailable),
        "a program that writes nothing formatted nothing"
    );
    assert_eq!(
        request.publish(&process_output(Some(0), b"{  }\n")),
        Ok(None),
        "a buffer that already matches its formatter records no undo step"
    );
}

#[test]
fn a_formatted_document_replaces_the_buffer_as_one_transaction() {
    let (mut text, request) = nix_run("{  }\n");
    let document = request
        .publish(&process_output(Some(0), b"{ }\n"))
        .expect("the program reported success")
        .expect("the program changed the document");
    let cursor = text.char_position(0).expect("the position exists");

    let transaction = document
        .transaction(&text, cursor)
        .expect("the buffer still holds the version of the request");

    assert_eq!(
        transaction.changes().len(),
        1,
        "one undo reverses a complete format"
    );
    text.apply(transaction).expect("the range fits the buffer");
    assert_eq!(text.to_string(), "{ }\n");
}

#[test]
fn a_formatted_document_of_an_obsolete_buffer_version_is_rejected() {
    let (mut text, request) = nix_run("{  }\n");
    let document = request
        .publish(&process_output(Some(0), b"{ }\n"))
        .expect("the program reported success")
        .expect("the program changed the document");

    // The user types while the formatter runs, so the answer describes text
    // that the buffer no longer holds.
    let at = text.char_position(0).expect("the position exists");
    text.apply(EditTransaction::single(at, TextChange::insert(at, "#")))
        .expect("the position fits");
    let cursor = text.char_position(0).expect("the position exists");

    assert_eq!(
        document.transaction(&text, cursor),
        Err(FormatterFailure::Obsolete)
    );
    assert_eq!(
        text.to_string(),
        "#{  }\n",
        "the buffer keeps what the user typed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bounded_process_service_formats_one_buffer_off_the_event_loop() {
    let text = buffer("let value = 1;\n");
    let request = FormatterRequest::new(
        &UPPERCASE_FORMATTER,
        PathBuf::from("/workspace/main.kv"),
        text.version(),
        text.to_string(),
    );

    let document = run_formatter(request)
        .await
        .expect("the program ran and reported success")
        .expect("the program changed the document");

    assert_eq!(document.text(), "LET VALUE = 1;\n");
    assert_eq!(document.version(), text.version());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_formatter_that_the_host_does_not_hold_reports_that_it_is_not_installed() {
    let text = buffer("let value = 1;\n");
    let request = FormatterRequest::new(
        &MISSING_FORMATTER,
        PathBuf::from("/workspace/main.kv"),
        text.version(),
        text.to_string(),
    );

    assert_eq!(
        run_formatter(request).await,
        Err(FormatterFailure::NotInstalled)
    );
}
