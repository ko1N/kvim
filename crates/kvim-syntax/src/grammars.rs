//! The bundled grammar catalog.
//!
//! Each language sits behind its own Cargo feature, so a consumer compiles only
//! the grammars that it names. The default build bundles none.
//!
//! One entry owns the stable grammar identity and the Tree-sitter grammar with
//! its queries. Language aliases and path selectors belong to `kvim-language`.
//! Nothing here names an editor, a server, or a terminal.
//!
//! The Terraform highlight query is adapted from nvim-treesitter (Apache-2.0),
//! runtime/queries/hcl/highlights.scm. The `tree-sitter-hcl` crate ships no
//! query file, so `queries/hcl/highlights.scm` of this crate vendors that text
//! and names its origin and its license.

use std::sync::OnceLock;

#[cfg(any(
    feature = "grammar-asm",
    feature = "grammar-bash",
    feature = "grammar-c",
    feature = "grammar-cpp",
    feature = "grammar-css",
    feature = "grammar-fish",
    feature = "grammar-glsl",
    feature = "grammar-go",
    feature = "grammar-html",
    feature = "grammar-javascript",
    feature = "grammar-json",
    feature = "grammar-lua",
    feature = "grammar-markdown",
    feature = "grammar-nix",
    feature = "grammar-python",
    feature = "grammar-rust",
    feature = "grammar-scss",
    feature = "grammar-sql",
    feature = "grammar-terraform",
    feature = "grammar-toml",
    feature = "grammar-tsx",
    feature = "grammar-typescript",
    feature = "grammar-xml",
    feature = "grammar-yaml",
    feature = "grammar-zig",
))]
use crate::catalog::Grammar;
use crate::catalog::LanguageCatalogEntry;

/// The largest number of languages that one build bundles.
///
/// The bound is the size of the complete catalog, so a build that enables every
/// feature still fits, and a future entry that passes it fails the sweep test
/// instead of growing the registry without notice.
pub const BUNDLED_LANGUAGES_MAX: usize = 32;

/// Returns the catalog entries that the enabled features bundle.
///
/// The list is empty in a build without a grammar feature, which is the
/// default. Selection then returns [`crate::HighlightFailure::UnsupportedLanguage`]
/// for every language, and a consumer that needs a language enables its feature.
///
/// # Examples
///
/// ```
/// // A build without a grammar feature bundles nothing at all.
/// assert!(kvim_syntax::bundled().len() <= kvim_syntax::BUNDLED_LANGUAGES_MAX);
///
/// # #[cfg(feature = "grammar-rust")] {
/// let rust = kvim_syntax::bundled()
///     .iter()
///     .find(|entry| entry.id() == "rust")
///     .expect("the feature bundles Rust");
/// assert_eq!(rust.id(), "rust");
/// # }
/// ```
pub fn bundled() -> &'static [&'static LanguageCatalogEntry] {
    static REGISTRY: OnceLock<Vec<&'static LanguageCatalogEntry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let entries: Vec<&'static LanguageCatalogEntry> = [
            #[cfg(feature = "grammar-asm")]
            &ASM_GRAMMAR,
            #[cfg(feature = "grammar-bash")]
            &BASH_GRAMMAR,
            #[cfg(feature = "grammar-c")]
            &C_GRAMMAR,
            #[cfg(feature = "grammar-cpp")]
            &CPP_GRAMMAR,
            #[cfg(feature = "grammar-css")]
            &CSS_GRAMMAR,
            #[cfg(feature = "grammar-fish")]
            &FISH_GRAMMAR,
            #[cfg(feature = "grammar-glsl")]
            &GLSL_GRAMMAR,
            #[cfg(feature = "grammar-go")]
            &GO_GRAMMAR,
            #[cfg(feature = "grammar-html")]
            &HTML_GRAMMAR,
            #[cfg(feature = "grammar-javascript")]
            &JAVASCRIPT_GRAMMAR,
            #[cfg(feature = "grammar-json")]
            &JSON_GRAMMAR,
            #[cfg(feature = "grammar-lua")]
            &LUA_GRAMMAR,
            #[cfg(feature = "grammar-markdown")]
            &MARKDOWN_GRAMMAR,
            #[cfg(feature = "grammar-nix")]
            &NIX_GRAMMAR,
            #[cfg(feature = "grammar-python")]
            &PYTHON_GRAMMAR,
            #[cfg(feature = "grammar-rust")]
            &RUST_GRAMMAR,
            #[cfg(feature = "grammar-scss")]
            &SCSS_GRAMMAR,
            #[cfg(feature = "grammar-sql")]
            &SQL_GRAMMAR,
            #[cfg(feature = "grammar-terraform")]
            &TERRAFORM_GRAMMAR,
            #[cfg(feature = "grammar-toml")]
            &TOML_GRAMMAR,
            #[cfg(feature = "grammar-tsx")]
            &TSX_GRAMMAR,
            #[cfg(feature = "grammar-typescript")]
            &TYPESCRIPT_GRAMMAR,
            #[cfg(feature = "grammar-xml")]
            &XML_GRAMMAR,
            #[cfg(feature = "grammar-yaml")]
            &YAML_GRAMMAR,
            #[cfg(feature = "grammar-zig")]
            &ZIG_GRAMMAR,
        ]
        .to_vec();
        debug_assert!(
            entries.len() <= BUNDLED_LANGUAGES_MAX,
            "the catalog holds at most BUNDLED_LANGUAGES_MAX languages",
        );
        entries
    })
}

/// Joins two highlight queries into one text.
///
/// kvim resolves no query inheritance, so a grammar whose upstream query
/// inherits another one joins the two texts once. The base patterns come first,
/// so a pattern of the deriving language takes precedence for one node.
#[cfg(any(
    feature = "grammar-cpp",
    feature = "grammar-scss",
    feature = "grammar-javascript",
    feature = "grammar-typescript",
    feature = "grammar-tsx",
))]
fn joined(cell: &'static OnceLock<String>, parts: &[&str]) -> &'static str {
    cell.get_or_init(|| parts.join("\n"))
}

#[cfg(feature = "grammar-asm")]
static ASM_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("asm", || Grammar {
    language: || tree_sitter_asm::LANGUAGE.into(),
    highlights_query: tree_sitter_asm::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-bash")]
static BASH_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("bash", || Grammar {
    language: || tree_sitter_bash::LANGUAGE.into(),
    // The crate names the query in the singular.
    highlights_query: tree_sitter_bash::HIGHLIGHT_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-c")]
static C_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("c", || Grammar {
    language: || tree_sitter_c::LANGUAGE.into(),
    // The crate names the query in the singular.
    highlights_query: tree_sitter_c::HIGHLIGHT_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-cpp")]
static CPP_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("cpp", || Grammar {
    language: || tree_sitter_cpp::LANGUAGE.into(),
    highlights_query: {
        static QUERY: OnceLock<String> = OnceLock::new();
        // The crates name both queries in the singular.
        joined(
            &QUERY,
            &[
                tree_sitter_c::HIGHLIGHT_QUERY,
                tree_sitter_cpp::HIGHLIGHT_QUERY,
            ],
        )
    },
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-css")]
static CSS_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("css", || Grammar {
    language: || tree_sitter_css::LANGUAGE.into(),
    highlights_query: tree_sitter_css::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-fish")]
static FISH_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("fish", || Grammar {
    language: tree_sitter_fish::language,
    highlights_query: tree_sitter_fish::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-glsl")]
static GLSL_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("glsl", || Grammar {
    // The crate holds one grammar and names it after its language.
    language: || tree_sitter_glsl::LANGUAGE_GLSL.into(),
    highlights_query: tree_sitter_glsl::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-go")]
static GO_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("go", || Grammar {
    language: || tree_sitter_go::LANGUAGE.into(),
    highlights_query: tree_sitter_go::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-html")]
static HTML_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("html", || Grammar {
    language: || tree_sitter_html::LANGUAGE.into(),
    highlights_query: tree_sitter_html::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-javascript")]
static JAVASCRIPT_GRAMMAR: LanguageCatalogEntry =
    LanguageCatalogEntry::new("javascript", || Grammar {
        language: || tree_sitter_javascript::LANGUAGE.into(),
        highlights_query: {
            // The crate ships the JSX patterns in a second text, because
            // another editor selects them by file type. One grammar reads both
            // dialects, so the entry joins the two texts once.
            static QUERY: OnceLock<String> = OnceLock::new();
            joined(
                &QUERY,
                &[
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                ],
            )
        },
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-json")]
static JSON_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("json", || Grammar {
    language: || tree_sitter_json::LANGUAGE.into(),
    highlights_query: tree_sitter_json::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-lua")]
static LUA_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("lua", || Grammar {
    language: || tree_sitter_lua::LANGUAGE.into(),
    highlights_query: tree_sitter_lua::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-markdown")]
static MARKDOWN_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("markdown", || Grammar {
    // The parser splits Markdown into a block grammar and an inline
    // grammar. This entry compiles the block grammar.
    language: || tree_sitter_md::LANGUAGE.into(),
    highlights_query: tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-nix")]
static NIX_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("nix", || Grammar {
    language: || tree_sitter_nix::LANGUAGE.into(),
    highlights_query: tree_sitter_nix::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-python")]
static PYTHON_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("python", || Grammar {
    language: || tree_sitter_python::LANGUAGE.into(),
    highlights_query: tree_sitter_python::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-rust")]
static RUST_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("rust", || Grammar {
    language: || tree_sitter_rust::LANGUAGE.into(),
    highlights_query: tree_sitter_rust::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-scss")]
static SCSS_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("scss", || Grammar {
    language: tree_sitter_scss::language,
    highlights_query: {
        // The crate ships the SCSS patterns alone, because the upstream
        // query inherits the CSS patterns. The SCSS grammar is a superset
        // of the CSS grammar, so every CSS pattern names a node kind that
        // the SCSS grammar holds.
        static QUERY: OnceLock<String> = OnceLock::new();
        joined(
            &QUERY,
            &[
                tree_sitter_css::HIGHLIGHTS_QUERY,
                tree_sitter_scss::HIGHLIGHTS_QUERY,
            ],
        )
    },
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-sql")]
static SQL_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("sql", || Grammar {
    language: || tree_sitter_sequel::LANGUAGE.into(),
    highlights_query: tree_sitter_sequel::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-terraform")]
static TERRAFORM_GRAMMAR: LanguageCatalogEntry =
    LanguageCatalogEntry::new("terraform", || Grammar {
        language: || tree_sitter_hcl::LANGUAGE.into(),
        highlights_query: include_str!("../queries/hcl/highlights.scm"),
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-toml")]
static TOML_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("toml", || Grammar {
    language: || tree_sitter_toml_ng::LANGUAGE.into(),
    highlights_query: tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-tsx")]
static TSX_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("tsx", || Grammar {
    language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
    highlights_query: {
        // TSX reads three dialects at once, and the two crates ship one
        // text for each of them. The JavaScript patterns come first, the
        // JSX patterns follow, and the type patterns come last.
        static QUERY: OnceLock<String> = OnceLock::new();
        joined(
            &QUERY,
            &[
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
            ],
        )
    },
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-typescript")]
static TYPESCRIPT_GRAMMAR: LanguageCatalogEntry =
    LanguageCatalogEntry::new("typescript", || Grammar {
        language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        highlights_query: {
            // The crate ships the type patterns alone, because the upstream
            // query inherits the JavaScript patterns.
            static QUERY: OnceLock<String> = OnceLock::new();
            joined(
                &QUERY,
                &[
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                ],
            )
        },
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-xml")]
static XML_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("xml", || Grammar {
    language: || tree_sitter_xml::LANGUAGE_XML.into(),
    highlights_query: tree_sitter_xml::XML_HIGHLIGHT_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-yaml")]
static YAML_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("yaml", || Grammar {
    language: || tree_sitter_yaml::LANGUAGE.into(),
    highlights_query: tree_sitter_yaml::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});

#[cfg(feature = "grammar-zig")]
static ZIG_GRAMMAR: LanguageCatalogEntry = LanguageCatalogEntry::new("zig", || Grammar {
    language: || tree_sitter_zig::LANGUAGE.into(),
    highlights_query: tree_sitter_zig::HIGHLIGHTS_QUERY,
    injections_query: "",
    locals_query: "",
});
