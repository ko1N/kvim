//! The bundled grammar catalog.
//!
//! Each language sits behind its own Cargo feature, so a consumer compiles only
//! the grammars that it names. The default build bundles none.
//!
//! One entry owns everything that selects and parses one language: the language
//! names, the file extensions, the complete file names, and the Tree-sitter
//! grammar with its queries. Nothing here names an editor, a server, or a
//! terminal.
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
///     .find(|entry| entry.language_names().contains(&"rust"))
///     .expect("the feature bundles Rust");
/// assert!(rust.extensions().contains(&"rs"));
/// # }
/// ```
pub fn bundled() -> &'static [&'static LanguageCatalogEntry] {
    static REGISTRY: OnceLock<Vec<&'static LanguageCatalogEntry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let entries: Vec<&'static LanguageCatalogEntry> = [
            #[cfg(feature = "grammar-asm")]
            &ASM,
            #[cfg(feature = "grammar-bash")]
            &BASH,
            #[cfg(feature = "grammar-c")]
            &C,
            #[cfg(feature = "grammar-cpp")]
            &CPP,
            #[cfg(feature = "grammar-css")]
            &CSS,
            #[cfg(feature = "grammar-fish")]
            &FISH,
            #[cfg(feature = "grammar-glsl")]
            &GLSL,
            #[cfg(feature = "grammar-go")]
            &GO,
            #[cfg(feature = "grammar-html")]
            &HTML,
            #[cfg(feature = "grammar-javascript")]
            &JAVASCRIPT,
            #[cfg(feature = "grammar-json")]
            &JSON,
            #[cfg(feature = "grammar-lua")]
            &LUA,
            #[cfg(feature = "grammar-markdown")]
            &MARKDOWN,
            #[cfg(feature = "grammar-nix")]
            &NIX,
            #[cfg(feature = "grammar-python")]
            &PYTHON,
            #[cfg(feature = "grammar-rust")]
            &RUST,
            #[cfg(feature = "grammar-scss")]
            &SCSS,
            #[cfg(feature = "grammar-sql")]
            &SQL,
            #[cfg(feature = "grammar-terraform")]
            &TERRAFORM,
            #[cfg(feature = "grammar-toml")]
            &TOML,
            #[cfg(feature = "grammar-tsx")]
            &TSX,
            #[cfg(feature = "grammar-typescript")]
            &TYPESCRIPT,
            #[cfg(feature = "grammar-xml")]
            &XML,
            #[cfg(feature = "grammar-yaml")]
            &YAML,
            #[cfg(feature = "grammar-zig")]
            &ZIG,
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
static ASM: LanguageCatalogEntry =
    LanguageCatalogEntry::new("asm", &["asm", "assembly"], &["S", "asm", "s"], &[], || {
        Grammar {
            language: || tree_sitter_asm::LANGUAGE.into(),
            highlights_query: tree_sitter_asm::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    });

#[cfg(feature = "grammar-bash")]
static BASH: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "bash",
    &["bash", "sh", "shell"],
    &["bash", "sh"],
    &[".bash_logout", ".bash_profile", ".bashrc", ".profile"],
    || Grammar {
        language: || tree_sitter_bash::LANGUAGE.into(),
        // The crate names the query in the singular.
        highlights_query: tree_sitter_bash::HIGHLIGHT_QUERY,
        injections_query: "",
        locals_query: "",
    },
);

#[cfg(feature = "grammar-c")]
static C: LanguageCatalogEntry =
    LanguageCatalogEntry::new("c", &["c"], &["c", "h"], &[], || Grammar {
        language: || tree_sitter_c::LANGUAGE.into(),
        // The crate names the query in the singular.
        highlights_query: tree_sitter_c::HIGHLIGHT_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-cpp")]
static CPP: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "cpp",
    &["c++", "cpp", "cxx"],
    &["cc", "cpp", "cxx", "hh", "hpp", "hxx"],
    &[],
    || Grammar {
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
    },
);

#[cfg(feature = "grammar-css")]
static CSS: LanguageCatalogEntry =
    LanguageCatalogEntry::new("css", &["css"], &["css"], &[], || Grammar {
        language: || tree_sitter_css::LANGUAGE.into(),
        highlights_query: tree_sitter_css::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-fish")]
static FISH: LanguageCatalogEntry =
    LanguageCatalogEntry::new("fish", &["fish"], &["fish"], &[], || Grammar {
        language: tree_sitter_fish::language,
        highlights_query: tree_sitter_fish::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-glsl")]
static GLSL: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "glsl",
    &["glsl"],
    &["comp", "frag", "geom", "glsl", "tesc", "tese", "vert"],
    &[],
    || Grammar {
        // The crate holds one grammar and names it after its language.
        language: || tree_sitter_glsl::LANGUAGE_GLSL.into(),
        highlights_query: tree_sitter_glsl::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    },
);

#[cfg(feature = "grammar-go")]
static GO: LanguageCatalogEntry =
    LanguageCatalogEntry::new("go", &["go", "golang"], &["go"], &[], || Grammar {
        language: || tree_sitter_go::LANGUAGE.into(),
        highlights_query: tree_sitter_go::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-html")]
static HTML: LanguageCatalogEntry =
    LanguageCatalogEntry::new("html", &["html"], &["htm", "html"], &[], || Grammar {
        language: || tree_sitter_html::LANGUAGE.into(),
        highlights_query: tree_sitter_html::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-javascript")]
static JAVASCRIPT: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "javascript",
    &["javascript", "js", "jsx"],
    &["cjs", "js", "jsx", "mjs"],
    &[],
    || Grammar {
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
    },
);

#[cfg(feature = "grammar-json")]
static JSON: LanguageCatalogEntry =
    LanguageCatalogEntry::new("json", &["json"], &["json"], &["flake.lock"], || Grammar {
        language: || tree_sitter_json::LANGUAGE.into(),
        highlights_query: tree_sitter_json::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-lua")]
static LUA: LanguageCatalogEntry =
    LanguageCatalogEntry::new("lua", &["lua"], &["lua"], &[], || Grammar {
        language: || tree_sitter_lua::LANGUAGE.into(),
        highlights_query: tree_sitter_lua::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-markdown")]
static MARKDOWN: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "markdown",
    &["markdown", "md"],
    &["markdown", "md"],
    &[],
    || Grammar {
        // The parser splits Markdown into a block grammar and an inline
        // grammar. This entry compiles the block grammar.
        language: || tree_sitter_md::LANGUAGE.into(),
        highlights_query: tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
        injections_query: "",
        locals_query: "",
    },
);

#[cfg(feature = "grammar-nix")]
static NIX: LanguageCatalogEntry =
    LanguageCatalogEntry::new("nix", &["nix"], &["nix"], &[], || Grammar {
        language: || tree_sitter_nix::LANGUAGE.into(),
        highlights_query: tree_sitter_nix::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-python")]
static PYTHON: LanguageCatalogEntry =
    LanguageCatalogEntry::new("python", &["py", "python"], &["py", "pyi"], &[], || {
        Grammar {
            language: || tree_sitter_python::LANGUAGE.into(),
            highlights_query: tree_sitter_python::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    });

#[cfg(feature = "grammar-rust")]
static RUST: LanguageCatalogEntry =
    LanguageCatalogEntry::new("rust", &["rs", "rust"], &["rs"], &[], || Grammar {
        language: || tree_sitter_rust::LANGUAGE.into(),
        highlights_query: tree_sitter_rust::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-scss")]
static SCSS: LanguageCatalogEntry =
    LanguageCatalogEntry::new("scss", &["scss"], &["scss"], &[], || Grammar {
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
static SQL: LanguageCatalogEntry =
    LanguageCatalogEntry::new("sql", &["sql"], &["sql"], &[], || Grammar {
        language: || tree_sitter_sequel::LANGUAGE.into(),
        highlights_query: tree_sitter_sequel::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-terraform")]
static TERRAFORM: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "terraform",
    &["terraform", "tf"],
    &["tf", "tfvars"],
    &[],
    || Grammar {
        language: || tree_sitter_hcl::LANGUAGE.into(),
        highlights_query: include_str!("../queries/hcl/highlights.scm"),
        injections_query: "",
        locals_query: "",
    },
);

#[cfg(feature = "grammar-toml")]
static TOML: LanguageCatalogEntry =
    LanguageCatalogEntry::new("toml", &["toml"], &["toml"], &[], || Grammar {
        language: || tree_sitter_toml_ng::LANGUAGE.into(),
        highlights_query: tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });

#[cfg(feature = "grammar-tsx")]
static TSX: LanguageCatalogEntry =
    LanguageCatalogEntry::new("tsx", &["tsx"], &["tsx"], &[], || Grammar {
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
static TYPESCRIPT: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "typescript",
    &["ts", "typescript"],
    &["cts", "mts", "ts"],
    &[],
    || Grammar {
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
    },
);

#[cfg(feature = "grammar-xml")]
static XML: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "xml",
    &["xml"],
    &["svg", "xml", "xsd", "xsl", "xslt"],
    &[],
    || Grammar {
        language: || tree_sitter_xml::LANGUAGE_XML.into(),
        highlights_query: tree_sitter_xml::XML_HIGHLIGHT_QUERY,
        injections_query: "",
        locals_query: "",
    },
);

#[cfg(feature = "grammar-yaml")]
static YAML: LanguageCatalogEntry = LanguageCatalogEntry::new(
    "yaml",
    &["yaml", "yml"],
    &["yaml", "yml"],
    &[".clang-format", ".clang-tidy"],
    || Grammar {
        language: || tree_sitter_yaml::LANGUAGE.into(),
        highlights_query: tree_sitter_yaml::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    },
);

#[cfg(feature = "grammar-zig")]
static ZIG: LanguageCatalogEntry =
    LanguageCatalogEntry::new("zig", &["zig"], &["zig"], &[], || Grammar {
        language: || tree_sitter_zig::LANGUAGE.into(),
        highlights_query: tree_sitter_zig::HIGHLIGHTS_QUERY,
        injections_query: "",
        locals_query: "",
    });
