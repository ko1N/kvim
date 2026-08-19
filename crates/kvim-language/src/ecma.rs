//! The shared server declarations of the ECMAScript family adapters.
//!
//! JavaScript, TypeScript, and TSX run the same two servers, so the data that
//! all three declarations repeat lives here once. Each adapter still writes its
//! own table, because a declaration also carries the protocol language
//! identifier of its language. See `docs/language-services.md`.

use serde_json::{Value, json};

use kvim_settings::LanguageSettings;

/// The workspace root markers of the `eslint` declaration.
///
/// `eslint` reports a failure for every buffer of a workspace that holds no
/// eslint configuration, so the declaration names every file that carries such
/// a configuration. The table repeats the names of the reference
/// `nvim-lspconfig` configuration, and it holds twelve of the
/// `LANGUAGE_ROOT_MARKERS_MAX` markers of one declaration.
///
/// The reference walks from the buffer to a parent directory. Kvim reads the
/// workspace root alone, so the table names each file directly.
pub(super) const ESLINT_ROOT_MARKERS: [&str; 12] = [
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

/// Returns the initialization options of `vscode-eslint-language-server`.
///
/// The server needs no option from the language-neutral settings, so the
/// function returns the empty object and reads nothing from `settings`.
pub(super) fn eslint_options(_settings: LanguageSettings) -> Value {
    json!({})
}

/// Returns the initialization options of `typescript-language-server`.
///
/// The server records the name of its client and needs no option from the
/// language-neutral settings, so the function reads nothing from `settings`.
pub(super) fn ts_ls_options(_settings: LanguageSettings) -> Value {
    json!({ "hostInfo": "kvim" })
}
