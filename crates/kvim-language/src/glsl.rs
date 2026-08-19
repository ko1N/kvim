//! The GLSL language adapter.
//!
//! The adapter supplies data only, exactly as the other adapters do: the paths
//! that it owns, the Tree-sitter grammar with its highlight query, the comment
//! metadata, the indent rule, and the language servers. See
//! `docs/language-services.md`.

use serde_json::{Value, json};
use tree_sitter::Language;

use kvim_settings::LanguageSettings;

use super::{
    BlockComment, CommentStyle, Grammar, IndentRule, LanguageAdapter, LanguageServerDeclaration,
    ServerFormatting,
};

/// The file extensions that the GLSL adapter owns.
///
/// The table names the shader stage extensions of the reference configuration
/// beside the generic extension. Each extension names one stage of the
/// rendering pipeline.
const GLSL_EXTENSIONS: [&str; 7] = ["comp", "frag", "geom", "glsl", "tesc", "tese", "vert"];

/// The node kinds whose content takes one more indent level in GLSL.
///
/// The GLSL grammar extends the C grammar, so the node kinds are the node kinds
/// of C. The enumerator list stays out of the table, because the shading
/// language defines no enumeration.
const GLSL_INDENT_SCOPES: [&str; 6] = [
    "argument_list",
    "compound_statement",
    "field_declaration_list",
    "initializer_list",
    "parameter_list",
    "parenthesized_expression",
];

/// The characters that close a GLSL indent scope.
const GLSL_CLOSING_DELIMITERS: [char; 3] = [')', ']', '}'];

/// Returns the GLSL grammar of the bundled parser.
fn glsl_language() -> Language {
    // The crate holds one grammar and names it after its language.
    tree_sitter_glsl::LANGUAGE_GLSL.into()
}

/// Returns the initialization options of `glsl_analyzer`.
///
/// `glsl_analyzer` needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
fn glsl_analyzer_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// The language servers that the GLSL adapter declares, in declaration order.
const GLSL_SERVERS: [LanguageServerDeclaration; 1] = [LanguageServerDeclaration {
    id: "glsl_analyzer",
    program: "glsl_analyzer",
    args: &[],
    language_id: "glsl",
    // GLSL declares no external formatter, so this server formats a GLSL
    // buffer.
    formatting: ServerFormatting::Enabled,
    // The server needs no project file, so no marker gates its start.
    root_markers: &[],
    initialization_options: glsl_analyzer_options,
    workspace_settings: None,
}];

/// The language adapter for GLSL shader paths.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use kvim_language::{GlslAdapter, LanguageAdapter};
///
/// let adapter = GlslAdapter::new();
/// assert!(adapter.supports_path(Path::new("shaders/light.frag")));
/// assert!(adapter.supports_path(Path::new("shaders/light.vert")));
/// assert_eq!(adapter.comment().line_token(), Some("//"));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GlslAdapter;

impl GlslAdapter {
    /// Creates the adapter that the registry holds.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for GlslAdapter {
    fn id(&self) -> &'static str {
        "glsl"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &GLSL_EXTENSIONS
    }

    fn comment(&self) -> CommentStyle {
        CommentStyle::new(Some("//"), Some(BlockComment::new("/*", "*/")))
    }

    fn grammar(&self) -> Grammar {
        Grammar {
            name: "glsl",
            language: glsl_language,
            highlights_query: tree_sitter_glsl::HIGHLIGHTS_QUERY,
            injections_query: "",
            locals_query: "",
        }
    }

    fn indent_rule(&self) -> IndentRule {
        IndentRule {
            scopes: &GLSL_INDENT_SCOPES,
            closing_delimiters: &GLSL_CLOSING_DELIMITERS,
        }
    }

    fn language_servers(&self) -> &'static [LanguageServerDeclaration] {
        &GLSL_SERVERS
    }
}
