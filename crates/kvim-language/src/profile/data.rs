//! First-release language service profile data.

use super::*;
use kvim_lsp::CompletionPolicy;
use serde_json::{Value, json};

fn empty_options(_: LanguageSettings) -> Value {
    json!({})
}
fn rust_options(settings: LanguageSettings) -> Value {
    let command = match settings.check_depth {
        CheckDepth::Compile => "check",
        CheckDepth::Lints => "clippy",
    };
    json!({ "check": { "command": command } })
}
fn eslint_settings(_: LanguageSettings) -> Value {
    json!({ "validate": "on", "nodePath": Value::Null, "problems": { "shortenToSingleLine": false }, "rulesCustomizations": [] })
}
fn ts_options(_: LanguageSettings) -> Value {
    json!({ "hostInfo": "kvim" })
}

const ESLINT_MARKERS: [&str; 12] = [
    ".eslintrc",
    ".eslintrc.cjs",
    ".eslintrc.js",
    ".eslintrc.json",
    ".eslintrc.yaml",
    ".eslintrc.yml",
    "eslint.config.cjs",
    "eslint.config.cts",
    "eslint.config.js",
    "eslint.config.mjs",
    "eslint.config.mts",
    "eslint.config.ts",
];

macro_rules! server {
    ($id:literal,$program:literal,$args:expr,$lang:literal,$format:ident,$markers:expr,$options:expr,$settings:expr,$completion:ident) => {
        LanguageServerDeclaration {
            id: $id,
            program: $program,
            args: $args,
            language_id: $lang,
            formatting: ServerFormatting::$format,
            root_markers: $markers,
            initialization_options: $options,
            workspace_settings: $settings,
            diagnostics_completion: CompletionPolicy::$completion,
        }
    };
}
const ASM_S: [LanguageServerDeclaration; 1] = [server!(
    "asm_lsp",
    "asm-lsp",
    &[],
    "asm",
    Disabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const BASH_S: [LanguageServerDeclaration; 1] = [server!(
    "bashls",
    "bash-language-server",
    &["start"],
    "shellscript",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const C_S: [LanguageServerDeclaration; 1] = [server!(
    "clangd",
    "clangd",
    &[],
    "c",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const CPP_S: [LanguageServerDeclaration; 1] = [server!(
    "clangd",
    "clangd",
    &[],
    "cpp",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const CSS_S: [LanguageServerDeclaration; 1] = [server!(
    "cssls",
    "vscode-css-language-server",
    &["--stdio"],
    "css",
    Disabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const FISH_S: [LanguageServerDeclaration; 1] = [server!(
    "fish_lsp",
    "fish-lsp",
    &["start"],
    "fish",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const GLSL_S: [LanguageServerDeclaration; 1] = [server!(
    "glsl_analyzer",
    "glsl_analyzer",
    &[],
    "glsl",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const GO_S: [LanguageServerDeclaration; 1] = [server!(
    "gopls",
    "gopls",
    &[],
    "go",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const HTML_S: [LanguageServerDeclaration; 1] = [server!(
    "html",
    "vscode-html-language-server",
    &["--stdio"],
    "html",
    Disabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const JS_S: [LanguageServerDeclaration; 2] = [
    server!(
        "eslint",
        "vscode-eslint-language-server",
        &["--stdio"],
        "javascript",
        Disabled,
        &ESLINT_MARKERS,
        empty_options,
        Some(eslint_settings),
        Pull
    ),
    server!(
        "ts_ls",
        "typescript-language-server",
        &["--stdio"],
        "javascript",
        Disabled,
        &[],
        ts_options,
        None,
        Unsupported
    ),
];
const JSON_S: [LanguageServerDeclaration; 1] = [server!(
    "jsonls",
    "vscode-json-language-server",
    &["--stdio"],
    "json",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const LUA_S: [LanguageServerDeclaration; 1] = [server!(
    "lua_ls",
    "lua-language-server",
    &[],
    "lua",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const MARKDOWN_S: [LanguageServerDeclaration; 1] = [server!(
    "marksman",
    "marksman",
    &["server"],
    "markdown",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const NIX_S: [LanguageServerDeclaration; 1] = [server!(
    "nil_ls",
    "nil",
    &[],
    "nix",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const PYTHON_S: [LanguageServerDeclaration; 1] = [server!(
    "pyright",
    "pyright-langserver",
    &["--stdio"],
    "python",
    Disabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const RUST_S: [LanguageServerDeclaration; 1] = [server!(
    "rust_analyzer",
    "rust-analyzer",
    &[],
    "rust",
    Enabled,
    &[],
    rust_options,
    None,
    Pull
)];
const SCSS_S: [LanguageServerDeclaration; 1] = [server!(
    "cssls",
    "vscode-css-language-server",
    &["--stdio"],
    "scss",
    Disabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const SQL_S: [LanguageServerDeclaration; 1] = [server!(
    "sqls",
    "sqls",
    &[],
    "sql",
    Disabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const TERRAFORM_S: [LanguageServerDeclaration; 1] = [server!(
    "tofu_ls",
    "tofu-ls",
    &["serve"],
    "terraform",
    Disabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const TOML_S: [LanguageServerDeclaration; 1] = [server!(
    "taplo",
    "taplo",
    &["lsp", "stdio"],
    "toml",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const TSX_S: [LanguageServerDeclaration; 2] = [
    server!(
        "eslint",
        "vscode-eslint-language-server",
        &["--stdio"],
        "typescriptreact",
        Disabled,
        &ESLINT_MARKERS,
        empty_options,
        Some(eslint_settings),
        Pull
    ),
    server!(
        "ts_ls",
        "typescript-language-server",
        &["--stdio"],
        "typescriptreact",
        Disabled,
        &[],
        ts_options,
        None,
        Unsupported
    ),
];
const TYPESCRIPT_S: [LanguageServerDeclaration; 2] = [
    server!(
        "eslint",
        "vscode-eslint-language-server",
        &["--stdio"],
        "typescript",
        Disabled,
        &ESLINT_MARKERS,
        empty_options,
        Some(eslint_settings),
        Pull
    ),
    server!(
        "ts_ls",
        "typescript-language-server",
        &["--stdio"],
        "typescript",
        Disabled,
        &[],
        ts_options,
        None,
        Unsupported
    ),
];
const XML_S: [LanguageServerDeclaration; 1] = [server!(
    "lemminx",
    "lemminx",
    &[],
    "xml",
    Disabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const YAML_S: [LanguageServerDeclaration; 1] = [server!(
    "yamlls",
    "yaml-language-server",
    &["--stdio"],
    "yaml",
    Disabled,
    &[],
    empty_options,
    None,
    Unsupported
)];
const ZIG_S: [LanguageServerDeclaration; 1] = [server!(
    "zls",
    "zls",
    &[],
    "zig",
    Enabled,
    &[],
    empty_options,
    None,
    Unsupported
)];

macro_rules! language_selectors {
    ($name:ident, $id:literal, [$($language:literal),*], [$($extension:literal),*], [$($file:literal),*]) => {
        const $name: (&str, &[&str], &[&str], &[&str]) = (
            $id,
            &[$($language),*],
            &[$($extension),*],
            &[$($file),*],
        );
    };
}

include!("../language_selectors.in");

macro_rules! built_in_profile {
    ($profile:ident, $selector:ident, $servers:ident) => {
        pub static $profile: LanguageServiceProfile = LanguageServiceProfile::new(
            $selector.0,
            "1",
            $selector.1,
            $selector.2,
            $selector.3,
            &$servers,
        );
    };
}

built_in_profile!(ASM_PROFILE, ASM, ASM_S);
built_in_profile!(BASH_PROFILE, BASH, BASH_S);
built_in_profile!(C_PROFILE, C, C_S);
built_in_profile!(CPP_PROFILE, CPP, CPP_S);
built_in_profile!(CSS_PROFILE, CSS, CSS_S);
built_in_profile!(FISH_PROFILE, FISH, FISH_S);
built_in_profile!(GLSL_PROFILE, GLSL, GLSL_S);
built_in_profile!(GO_PROFILE, GO, GO_S);
built_in_profile!(HTML_PROFILE, HTML, HTML_S);
built_in_profile!(JAVASCRIPT_PROFILE, JAVASCRIPT, JS_S);
built_in_profile!(JSON_PROFILE, JSON, JSON_S);
built_in_profile!(LUA_PROFILE, LUA, LUA_S);
built_in_profile!(MARKDOWN_PROFILE, MARKDOWN, MARKDOWN_S);
built_in_profile!(NIX_PROFILE, NIX, NIX_S);
built_in_profile!(PYTHON_PROFILE, PYTHON, PYTHON_S);
built_in_profile!(RUST_PROFILE, RUST, RUST_S);
built_in_profile!(SCSS_PROFILE, SCSS, SCSS_S);
built_in_profile!(SQL_PROFILE, SQL, SQL_S);
built_in_profile!(TERRAFORM_PROFILE, TERRAFORM, TERRAFORM_S);
built_in_profile!(TOML_PROFILE, TOML, TOML_S);
built_in_profile!(TSX_PROFILE, TSX, TSX_S);
built_in_profile!(TYPESCRIPT_PROFILE, TYPESCRIPT, TYPESCRIPT_S);
built_in_profile!(XML_PROFILE, XML, XML_S);
built_in_profile!(YAML_PROFILE, YAML, YAML_S);
built_in_profile!(ZIG_PROFILE, ZIG, ZIG_S);

pub(super) static FIRST_RELEASE: [LanguageServiceProfile; FIRST_RELEASE_LANGUAGE_PROFILES] = [
    ASM_PROFILE,
    BASH_PROFILE,
    C_PROFILE,
    CPP_PROFILE,
    CSS_PROFILE,
    FISH_PROFILE,
    GLSL_PROFILE,
    GO_PROFILE,
    HTML_PROFILE,
    JAVASCRIPT_PROFILE,
    JSON_PROFILE,
    LUA_PROFILE,
    MARKDOWN_PROFILE,
    NIX_PROFILE,
    PYTHON_PROFILE,
    RUST_PROFILE,
    SCSS_PROFILE,
    SQL_PROFILE,
    TERRAFORM_PROFILE,
    TOML_PROFILE,
    TSX_PROFILE,
    TYPESCRIPT_PROFILE,
    XML_PROFILE,
    YAML_PROFILE,
    ZIG_PROFILE,
];
