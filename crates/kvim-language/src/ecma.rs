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

/// Returns the workspace settings of `vscode-eslint-language-server`.
///
/// The server reads its behavior from the workspace configuration, not from the
/// initialization options. It answers an empty report until this data reaches
/// it, so the declaration names the four members that one lint run needs. A
/// probe of the installed server measured each member, and the server fails or
/// reports nothing when one of them is absent:
///
/// - `validate` selects the lint run. Every other value returns an empty
///   report.
/// - `nodePath` selects the directory that holds the eslint library. The server
///   reads the member without a default, so an absent member ends the request
///   with a type failure. The null value keeps the search of the server.
/// - `problems.shortenToSingleLine` selects a one-line message. The server
///   reads the member without a default, and Kvim wraps a long message in its
///   own float.
/// - `rulesCustomizations` overrides the severity of a rule. The server walks
///   the list without a default, and Kvim changes no severity.
///
/// The server needs no member from the language-neutral settings, so the
/// function reads nothing from `settings`. See `docs/language-services.md`.
pub(super) fn eslint_workspace_settings(_settings: LanguageSettings) -> Value {
    json!({
        "validate": "on",
        "nodePath": Value::Null,
        "problems": { "shortenToSingleLine": false },
        "rulesCustomizations": [],
    })
}

/// Returns the initialization options of `typescript-language-server`.
///
/// The server records the name of its client and needs no option from the
/// language-neutral settings, so the function reads nothing from `settings`.
pub(super) fn ts_ls_options(_settings: LanguageSettings) -> Value {
    json!({ "hostInfo": "kvim" })
}
