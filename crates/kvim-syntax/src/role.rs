//! The theme-independent syntax vocabulary.
//! Adapted from ReviewGraph (MIT), src/analysis.rs.
//!
//! A role names what a range of source is, never how it looks, so this module
//! needs no palette, no terminal, and no editor. The consumer maps each role to
//! one style of its own.

/// One terminal-independent syntax role.
///
/// The vocabulary is non-exhaustive, because a later release can name a further
/// kind of source without breaking a consumer that already matches on it. Match
/// with a wildcard arm and paint an unknown role as plain text.
///
/// # Examples
///
/// ```
/// use kvim_syntax::SyntaxRole;
///
/// let role = SyntaxRole::Keyword;
/// let style = match role {
///     SyntaxRole::Comment => "dim",
///     SyntaxRole::Keyword => "bold",
///     _ => "plain",
/// };
/// assert_eq!(style, "bold");
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum SyntaxRole {
    /// An attribute, such as a Rust derive attribute.
    Attribute,
    /// A boolean literal.
    Boolean,
    /// A bracket, brace, or parenthesis.
    Bracket,
    /// A comment.
    Comment,
    /// A named constant.
    Constant,
    /// A constructor, such as an enum variant.
    Constructor,
    /// A delimiter, such as a comma or a semicolon.
    Delimiter,
    /// A function name.
    Function,
    /// A language keyword.
    Keyword,
    /// A macro name.
    Macro,
    /// A numeric literal.
    Number,
    /// An operator.
    Operator,
    /// A function parameter.
    Parameter,
    /// A preprocessor directive.
    Preprocessor,
    /// A structure field or a property.
    Property,
    /// A statement keyword.
    Statement,
    /// A string literal.
    String,
    /// A type name.
    Type,
    /// A variable name.
    Variable,
}

/// One bounded highlight range inside one source line.
///
/// The range holds byte offsets inside the line, never terminal cells and never
/// a color. The consumer maps [`SyntaxRole`] to one style of its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighlightSpan {
    /// The zero-based line index.
    pub line: u32,
    /// The first byte of the range inside the line.
    pub start_byte: u32,
    /// The byte after the range inside the line.
    pub end_byte: u32,
    /// The terminal-independent role of the range.
    pub role: SyntaxRole,
}

/// Maps one capture name of a highlight query to one syntax role.
///
/// Tree-sitter highlight queries share one capture vocabulary across grammars,
/// so the mapping stays language-neutral. The first component of a dotted name
/// carries the meaning. A constant that starts with a digit is a numeric
/// literal, which several queries capture as a constant.
pub(crate) fn highlight_role(name: &str, bytes: &[u8]) -> Option<SyntaxRole> {
    let mut parts = name.split('.');
    let prefix = parts.next()?;
    match prefix {
        "attribute" => Some(SyntaxRole::Attribute),
        "boolean" => Some(SyntaxRole::Boolean),
        // A character literal is a string of one character, so it takes the
        // string role.
        "character" => Some(SyntaxRole::String),
        "comment" => Some(SyntaxRole::Comment),
        // The Lua query names the branch keywords and the loop keywords with
        // the older words of the same shared vocabulary.
        "conditional" | "repeat" => Some(SyntaxRole::Keyword),
        "constant" if bytes.first().is_some_and(u8::is_ascii_digit) => Some(SyntaxRole::Number),
        "constant" => Some(SyntaxRole::Constant),
        "constructor" => Some(SyntaxRole::Constructor),
        // The C family names a comma and a semicolon `delimiter`, while the
        // other grammars name the same characters `punctuation.delimiter`.
        "delimiter" => Some(SyntaxRole::Delimiter),
        "escape" | "string" => Some(SyntaxRole::String),
        // A table field of the Lua query is the property of its table.
        "field" => Some(SyntaxRole::Property),
        // The SQL query names a floating-point literal with the older word of
        // the same shared vocabulary.
        "float" => Some(SyntaxRole::Number),
        "function" if name.split('.').any(|part| part == "macro") => Some(SyntaxRole::Macro),
        "function" => Some(SyntaxRole::Function),
        "keyword" => Some(SyntaxRole::Keyword),
        "label" => Some(SyntaxRole::Statement),
        // The `markup` family is the newer name of the `text` family below, so
        // each name takes the role of the older word that carries the same
        // meaning. A bare `markup` marks the plain text of a document, which
        // no role names, so that capture stays off and its text stays plain.
        "markup" => match parts.next() {
            Some("heading") => Some(SyntaxRole::Type),
            Some("link" | "raw") => Some(SyntaxRole::String),
            _ => None,
        },
        // A method is a function of one value, so it takes the function role.
        "method" => Some(SyntaxRole::Function),
        // A module name names a namespace of declarations, so it takes the type
        // role of that namespace.
        "module" => Some(SyntaxRole::Type),
        "number" => Some(SyntaxRole::Number),
        "operator" => Some(SyntaxRole::Operator),
        // The Lua query names a function parameter with the older word of the
        // same shared vocabulary.
        "parameter" => Some(SyntaxRole::Parameter),
        "preproc" => Some(SyntaxRole::Preprocessor),
        "property" => Some(SyntaxRole::Property),
        "punctuation" => match parts.next() {
            Some("bracket") => Some(SyntaxRole::Bracket),
            Some("delimiter") => Some(SyntaxRole::Delimiter),
            _ => Some(SyntaxRole::Operator),
        },
        // A storage class names how a database keeps an object, so the SQL
        // query marks a keyword with the older word of the same shared
        // vocabulary.
        "storageclass" => Some(SyntaxRole::Keyword),
        // A tag names the kind of an element, exactly as a type name names the
        // kind of a value, so the markup grammars take the type role. The
        // deeper names of the same family, for example the erroneous end tag of
        // the HTML query, keep that role.
        "tag" => Some(SyntaxRole::Type),
        // The `text` family belongs to the prose grammars of the same shared
        // vocabulary. Each name maps onto the role that carries the same
        // meaning, because the role set names source meaning and stays fixed.
        "text" => match parts.next() {
            Some("literal" | "uri") => Some(SyntaxRole::String),
            Some("reference") => Some(SyntaxRole::Constant),
            Some("title") => Some(SyntaxRole::Type),
            _ => None,
        },
        "type" => Some(SyntaxRole::Type),
        "variable" => match parts.next() {
            // A member of a value is the property of that value, so the newer
            // word takes the role of `field` above.
            Some("member") => Some(SyntaxRole::Property),
            Some("parameter") => Some(SyntaxRole::Parameter),
            _ => Some(SyntaxRole::Variable),
        },
        _ => None,
    }
}
